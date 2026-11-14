// font: one font face from a TrueType, OpenType/CFF, CFF2, or collection file,
// read through skrifa. A `Face` holds the face's names, style and line
// metrics, and answers its character map, glyph metrics, outlines in font
// units (y up), and color glyphs, all at its variation location.
//
// A malformed or truncated file answers an error or an empty glyph, never a
// panic, so a broken system font cannot take the interface down.

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::font::glyph::bitmap::{BitmapSource, Origin, Pixels};
use skrifa::font::glyph::outline::{DrawMemory, DrawOptions, PathSink};
use skrifa::font::{Blob, Font as Model, Variation};
use skrifa::raw::tables::cmap::{CmapSubtable, PlatformId};
use skrifa::raw::types::NameId;
use skrifa::raw::{FontRef, TableProvider};
use skrifa::{GlyphId, Tag};

use crate::errors;
use crate::text::color_glyph;

/// One path command stream in font units, y up: `MOVE x y`, `LINE x y`,
/// `QUAD cx cy x y`, `CUBIC c1x c1y c2x c2y x y`, `CLOSE`.
pub const MOVE: f64 = 0.0;
pub const LINE: f64 = 1.0;
pub const QUAD: f64 = 2.0;
pub const CUBIC: f64 = 3.0;
pub const CLOSE: f64 = 4.0;

/// A glyph outline: path commands in font units and the advance width.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Outline {
    pub path: Vec<f64>,
    pub advance: i64,
    /// Left side bearing in font units.
    pub lsb: i64,
}

/// A color glyph as straight-alpha RGBA pixels, with its placement in the
/// pixels of the size it was drawn for.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorGlyph {
    pub rgba: Vec<u8>,
    /// Pixels per em of the drawing: a bitmap strike's size, or the size asked
    /// for when the glyph is painted from `COLR`.
    pub ppem: f64,
    pub width: i64,
    pub height: i64,
    /// From the pen to the image's left edge, and from the baseline up to its top.
    pub bearing_x: i64,
    pub bearing_y: i64,
}

/// A coverage bitmap glyph from a bitmap-only font (`EBDT`), placed like a
/// `ColorGlyph`; one byte of coverage per pixel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaskGlyph {
    pub coverage: Vec<u8>,
    pub ppem: f64,
    pub width: i64,
    pub height: i64,
    pub bearing_x: i64,
    pub bearing_y: i64,
}

/// A font file's bytes: a file built into the program, read where it lies, one
/// loaded at run time, or an installed file mapped read-only; shared by every
/// face it holds.
#[derive(Clone)]
pub enum FontBytes {
    Static(&'static [u8]),
    Shared(Arc<Vec<u8>>),
    Mapped(Arc<memmap2::Mmap>),
}

impl FontBytes {
    /// Bytes loaded at run time, taken without a copy.
    pub fn owned(data: Vec<u8>) -> Self {
        Self::Shared(Arc::new(data))
    }

    /// The heap the bytes occupy; a built-in or mapped file occupies none.
    pub(crate) fn heap_bytes(&self) -> usize {
        match self {
            Self::Static(_) | Self::Mapped(_) => 0,
            Self::Shared(data) => data.len(),
        }
    }

    // The same bytes as skrifa's blob, shared rather than copied.
    fn blob(&self) -> Blob {
        match self {
            Self::Static(data) => Blob::from(*data),
            Self::Shared(data) => {
                Blob::from(Arc::clone(data) as Arc<dyn AsRef<[u8]> + Send + Sync>)
            }
            Self::Mapped(data) => {
                Blob::from(Arc::clone(data) as Arc<dyn AsRef<[u8]> + Send + Sync>)
            }
        }
    }
}

impl std::ops::Deref for FontBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Static(data) => data,
            Self::Shared(data) => data,
            Self::Mapped(data) => data,
        }
    }
}

impl PartialEq for FontBytes {
    // The same file compares equal by address, without touching its pages.
    fn eq(&self, other: &Self) -> bool {
        let (a, b): (&[u8], &[u8]) = (self, other);
        std::ptr::eq(a, b) || a == b
    }
}

impl std::fmt::Debug for FontBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FontBytes({} bytes)", self.len())
    }
}

/// One font face read from a file. Glyph ids are the font's own;
/// `glyph_for` maps characters to them.
#[derive(Clone)]
pub struct Face {
    pub family: String,
    /// The family its name ID 1 gives where the typographic family (ID 16)
    /// differs, as fontconfig and older settings name a face ("Lilex Light"
    /// of family "Lilex"); empty otherwise.
    pub legacy_family: String,
    pub subfamily: String,
    pub bold: bool,
    pub italic: bool,
    /// The weight on the CSS scale (400 regular, 700 bold), from `OS/2`.
    pub weight: i64,
    pub monospace: bool,
    pub units_per_em: i64,
    pub ascent: i64,
    pub descent: i64,
    pub line_gap: i64,
    pub num_glyphs: i64,
    data: FontBytes,
    /// The face's index in its file (non-zero inside a collection).
    index: u32,
    /// The face as skrifa reads it, at `coords`; `None` only for the empty
    /// default face.
    model: Option<Model>,
    /// The glyph ids of the ASCII characters, read from `cmap` once.
    ascii: [u16; 128],
    /// Normalized variation coordinates, one per `fvar` axis; empty for the
    /// default instance.
    pub coords: Vec<f64>,
    /// The face's index in the font registry, or -1 when it is not registered.
    pub slot: i64,
}

impl PartialEq for Face {
    // The model is read from `data` at `index` and `coords`, which compare here.
    fn eq(&self, other: &Self) -> bool {
        self.family == other.family
            && self.legacy_family == other.legacy_family
            && self.subfamily == other.subfamily
            && self.bold == other.bold
            && self.italic == other.italic
            && self.weight == other.weight
            && self.monospace == other.monospace
            && self.units_per_em == other.units_per_em
            && (self.ascent, self.descent, self.line_gap)
                == (other.ascent, other.descent, other.line_gap)
            && self.num_glyphs == other.num_glyphs
            && self.data == other.data
            && self.index == other.index
            && self.coords == other.coords
            && self.slot == other.slot
    }
}

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Face")
            .field("family", &self.family)
            .field("subfamily", &self.subfamily)
            .field("weight", &self.weight)
            .field("index", &self.index)
            .field("coords", &self.coords)
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}

impl Default for Face {
    fn default() -> Face {
        Face {
            family: String::new(),
            legacy_family: String::new(),
            subfamily: String::new(),
            bold: false,
            italic: false,
            weight: 400,
            monospace: false,
            units_per_em: 0,
            ascent: 0,
            descent: 0,
            line_gap: 0,
            num_glyphs: 0,
            data: FontBytes::Static(&[]),
            index: 0,
            model: None,
            ascii: [0; 128],
            coords: vec![],
            slot: -1,
        }
    }
}

impl Face {
    /// The address and length of the file this face shares with the other
    /// faces loaded from it, when it was loaded at run time.
    pub(crate) fn shared_file(&self) -> Option<(usize, usize)> {
        match &self.data {
            FontBytes::Static(_) | FontBytes::Mapped(_) => None,
            FontBytes::Shared(data) => Some((Arc::as_ptr(data) as usize, data.len())),
        }
    }

    pub(crate) fn owned_bytes(&self) -> usize {
        size_of::<Self>()
            + self.data.heap_bytes()
            + self.family.capacity()
            + self.legacy_family.capacity()
            + self.subfamily.capacity()
            + self.coords.capacity() * size_of::<f64>()
    }

    /// Reads the names, style and line metrics of the face at `index`.
    fn describe(bytes: FontBytes, index: i64) -> Result<Face, errors::Error> {
        let index = u32::try_from(index)
            .map_err(|_| errors::new(format!("font collection has no face {index}")))?;
        let mut face = Face {
            data: bytes,
            index,
            ..Face::default()
        };
        let data = face.data.clone();
        let font = FontRef::from_index(&data, index)
            .map_err(|e| errors::new(format!("font cannot be read: {e}")))?;
        let head = font
            .head()
            .map_err(|_| errors::new("font has no head table"))?;
        face.units_per_em = i64::from(head.units_per_em()).max(16);
        let style = head.mac_style().bits();
        face.bold = style & 1 != 0;
        face.italic = style & 2 != 0;
        face.weight = if face.bold { 700 } else { 400 };
        if let Ok(maxp) = font.maxp() {
            face.num_glyphs = i64::from(maxp.num_glyphs());
        }
        if let Ok(hhea) = font.hhea() {
            face.ascent = i64::from(hhea.ascender().to_i16());
            face.descent = i64::from(hhea.descender().to_i16());
            face.line_gap = i64::from(hhea.line_gap().to_i16());
        }
        if let Ok(os2) = font.os2() {
            let weight = i64::from(os2.us_weight_class());
            if (1..=1000).contains(&weight) {
                face.weight = weight
            };
            if weight >= 600 {
                face.bold = true
            };
            if os2.fs_selection().bits() & 1 != 0 {
                face.italic = true
            }
        }
        if let Ok(post) = font.post() {
            face.monospace = post.is_fixed_pitch() != 0
        }
        if face.ascent == 0 && face.descent == 0 {
            face.ascent = face.units_per_em * 4 / 5;
            face.descent = -(face.units_per_em / 5);
        }
        face.read_names(&font);
        Ok(face)
    }

    /// Reads only family/style metadata, without loading glyph outlines.
    pub fn metadata(
        data: &[u8],
        index: i64,
    ) -> Result<(String, String, bool, bool), errors::Error> {
        let face = Face::describe(FontBytes::owned(data.to_vec()), index)?;
        Ok((face.family, face.subfamily, face.bold, face.italic))
    }

    pub fn parse(data: &[u8], index: i64) -> Result<Face, errors::Error> {
        Self::parse_bytes(FontBytes::owned(data.to_vec()), index)
    }

    /// Parses the face at `index` of `bytes`, which it shares rather than copies.
    pub fn parse_bytes(bytes: FontBytes, index: i64) -> Result<Face, errors::Error> {
        let mut face = Face::with_model(bytes, index)?;
        let Some(model) = &face.model else {
            return Err(errors::new("font cannot be read"));
        };
        let has_outlines = !model.outline_glyphs().is_empty();
        let has_bitmaps = !model.bitmap_strikes().is_empty();
        if !has_outlines && !has_bitmaps {
            return Err(errors::new("font has neither outlines nor bitmaps"));
        }
        // Text is mostly ASCII, so its glyphs skip the cmap search.
        let charmap = model.charmap();
        for (code, glyph) in (0u8..128).zip(face.ascii.iter_mut()) {
            *glyph = charmap
                .map_unicode(u32::from(code))
                .and_then(|g| u16::try_from(g.to_u32()).ok())
                .unwrap_or(0);
        }
        face.num_glyphs = i64::from(model.num_glyphs());
        Ok(face)
    }

    /// Reads the names, style, metrics and character map of a single-face
    /// font file whose outline tables may be absent, as a catalog summary.
    pub fn summary(data: &[u8]) -> Result<Face, errors::Error> {
        Face::with_model(FontBytes::owned(data.to_vec()), 0)
    }

    fn with_model(bytes: FontBytes, index: i64) -> Result<Face, errors::Error> {
        let mut face = Face::describe(bytes, index)?;
        let model = Model::new(face.data.blob(), face.index)
            .ok_or_else(|| errors::new("font cannot be read"))?;
        if !model.charmap().has_unicode() {
            return Err(errors::new("font has no Unicode character map"));
        }
        face.model = Some(model);
        Ok(face)
    }

    /// This face at the user axis values given by tag (`wght`, `wdth`, ...);
    /// axes not named keep their defaults.
    pub fn instance(&self, values: &HashMap<String, f64>) -> Face {
        let mut out = self.clone();
        let Some(model) = &self.model else {
            return out;
        };
        let variations = values.iter().filter_map(|(tag, value)| {
            let tag = Tag::new_checked(tag.as_bytes()).ok()?;
            Some(Variation::new(tag, *value as f32))
        });
        let varied = model.instance_builder().variations(variations).build();
        out.coords = varied
            .normalized_coords()
            .iter()
            .map(|c| f64::from(c.to_f32()))
            .collect();
        if out.coords.iter().all(|c| *c == 0.0) {
            out.coords.clear();
        }
        if let Some(line) = varied.metrics().hhea_line {
            out.ascent = line.ascender.to_f64().round() as i64;
            out.descent = line.descender.to_f64().round() as i64;
            out.line_gap = line.line_gap.to_f64().round() as i64;
        }
        if let Some(&weight) = values.get("wght") {
            out.weight = weight.round() as i64;
        }
        out.model = Some(varied);
        out
    }

    /// Number of faces in a font file: the collection size, or 1.
    pub fn count(data: &[u8]) -> i64 {
        if data.len() >= 12 && tag(data, 0) == "ttcf" {
            be_u32(data, 8)
        } else {
            1
        }
    }

    /// Whether the face has the table named `name`.
    pub fn has_table(&self, name: &str) -> bool {
        let Ok(tag) = Tag::new_checked(name.as_bytes()) else {
            return false;
        };
        FontRef::from_index(&self.data, self.index).is_ok_and(|f| f.table_data(tag).is_some())
    }

    /// The raw file bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// The face as skrifa reads it, for shaping; `None` for the empty face.
    pub(crate) fn model(&self) -> Option<&Model> {
        self.model.as_ref()
    }

    /// The font file a document can embed: a whole single-face file at its
    /// default instance, with whether its outlines are CFF.
    pub fn program(&self) -> Option<(&[u8], bool)> {
        if Face::count(&self.data) != 1 || !self.coords.is_empty() {
            return None;
        }
        if self.has_table("glyf") {
            Some((&self.data, false))
        } else if self.has_table("CFF ") {
            Some((&self.data, true))
        } else {
            None
        }
    }

    // Family and style names, preferring the typographic (16, 17) names, from
    // Windows Unicode English or Macintosh Roman records.
    fn read_names(&mut self, font: &FontRef) {
        let Ok(name) = font.name() else {
            return;
        };
        let mut family = String::new();
        let mut typographic = String::new();
        let mut subfamily = String::new();
        let mut typographic_subfamily = String::new();
        for record in name.name_record() {
            let platform = record.platform_id();
            let encoding = record.encoding_id();
            let language = record.language_id();
            let readable = (platform == 3
                && (encoding == 1 || encoding == 10)
                && (language == 0x409 || language == 0))
                || (platform == 1 && encoding == 0 && language == 0);
            if !readable {
                continue;
            }
            let Ok(string) = record.string(name.string_data()) else {
                continue;
            };
            let text: String = string.chars().collect();
            if text.is_empty() {
                continue;
            }
            match record.name_id() {
                NameId::FAMILY_NAME if family.is_empty() || platform == 3 => family = text,
                NameId::SUBFAMILY_NAME if subfamily.is_empty() || platform == 3 => subfamily = text,
                NameId::TYPOGRAPHIC_FAMILY_NAME => typographic = text,
                NameId::TYPOGRAPHIC_SUBFAMILY_NAME => typographic_subfamily = text,
                _ => (),
            }
        }
        (self.family, self.legacy_family) = if typographic.is_empty() || typographic == family {
            (family, String::new())
        } else {
            (typographic, family)
        };
        self.subfamily = if typographic_subfamily.is_empty() {
            subfamily
        } else {
            typographic_subfamily
        };
        // Some families mark their slanted faces only in the style's name.
        let style = self.subfamily.to_lowercase();
        if style.contains("italic") || style.contains("oblique") {
            self.italic = true;
        }
    }

    pub fn glyph_for(&self, c: char) -> i64 {
        match self.ascii.get(c as usize) {
            Some(&glyph) => i64::from(glyph),
            None => self.lookup_glyph(c),
        }
    }

    fn lookup_glyph(&self, c: char) -> i64 {
        self.model
            .as_ref()
            .and_then(|m| m.charmap().map_unicode(c))
            .map_or(0, |g| i64::from(g.to_u32()))
    }

    /// The scalars the face maps to a glyph, as inclusive (first, last) pairs
    /// flattened into one sorted list.
    pub fn coverage(&self) -> Vec<i64> {
        let mut out: Vec<i64> = vec![];
        let Some(model) = &self.model else {
            return out;
        };
        match self.unicode_subtable() {
            Some(CmapSubtable::Format12(table)) => {
                let mut previous = -1;
                for group in table.groups() {
                    let start = i64::from(group.start_char_code());
                    let end = i64::from(group.end_char_code()).min(0x10ffff);
                    if start <= previous || start > end {
                        continue;
                    }
                    previous = end;
                    let first = if group.start_glyph_id() == 0 {
                        start + 1
                    } else {
                        start
                    };
                    add_range(&mut out, first, end)
                }
            }
            Some(CmapSubtable::Format4(table)) => {
                for (code, glyph) in table.iter() {
                    if glyph.to_u32() != 0 {
                        add_range(&mut out, i64::from(code), i64::from(code))
                    }
                }
            }
            _ => {
                let mut mapped = model
                    .charmap()
                    .unicodes()
                    .filter(|(_, g)| g.to_u32() != 0)
                    .map(|(code, _)| i64::from(code))
                    .collect::<Vec<_>>();
                mapped.sort_unstable();
                mapped.dedup();
                for code in mapped {
                    add_range(&mut out, code, code)
                }
            }
        }
        out
    }

    // The face's full-repertoire Unicode subtable (format 12) where it has
    // one, else its BMP one (format 4); other formats are left to skrifa.
    fn unicode_subtable(&self) -> Option<CmapSubtable<'_>> {
        let font = FontRef::from_index(&self.data, self.index).ok()?;
        let cmap = font.cmap().ok()?;
        let mut best = None;
        let mut best_rank = 0;
        for record in cmap.encoding_records() {
            let Ok(subtable) = record.subtable(cmap.offset_data()) else {
                continue;
            };
            let unicode = record.platform_id() == PlatformId::Unicode;
            let windows = |encoding| {
                record.platform_id() == PlatformId::Windows && record.encoding_id() == encoding
            };
            let rank = match subtable {
                CmapSubtable::Format12(_) if unicode || windows(10) => 4,
                CmapSubtable::Format4(_) if unicode || windows(1) => 3,
                CmapSubtable::Format12(_) => 2,
                CmapSubtable::Format4(_) => 1,
                _ => 0,
            };
            if rank > best_rank {
                best_rank = rank;
                best = Some(subtable);
            }
        }
        best
    }

    pub fn covers(&self, c: char) -> bool {
        self.glyph_for(c) != 0
    }

    /// Horizontal advance and left side bearing of glyph `g`, font units, at
    /// the face's variation location.
    pub fn h_metrics(&self, g: i64) -> (i64, i64) {
        let Some(model) = &self.model else {
            return (0, 0);
        };
        let Ok(id) = u32::try_from(g) else {
            return (0, 0);
        };
        let gid = GlyphId::new(id);
        let advance = model.glyph_metrics().h_advance(gid).round() as i64;
        let lsb = FontRef::from_index(&self.data, self.index)
            .ok()
            .and_then(|f| f.hmtx().ok())
            .and_then(|hmtx| hmtx.side_bearing(gid))
            .map_or(0, i64::from);
        (advance, lsb)
    }

    /// Glyph `g`'s outline in font units.
    pub fn outline(&self, g: i64) -> Outline {
        let (advance, lsb) = self.h_metrics(g);
        let mut path = PathCommands::default();
        if let Some(model) = &self.model
            && let Ok(id) = u32::try_from(g)
            && let Some(glyph) = model.outline_glyphs().get(GlyphId::new(id))
        {
            let _ = glyph.draw(&DrawOptions::new(), DrawMemory::Scratch, &mut path);
        }
        Outline {
            path: path.0,
            advance,
            lsb,
        }
    }

    /// Whether the face draws glyphs in color (`COLR`, `CBDT`, or `sbix`)
    /// rather than as outlines in the text's color.
    pub fn has_color_glyphs(&self) -> bool {
        let Some(model) = &self.model else {
            return false;
        };
        !model.color_glyphs().is_empty()
            || matches!(
                model.bitmap_strikes().source(),
                Some(BitmapSource::Cbdt | BitmapSource::Sbix)
            )
    }

    /// Glyph `g` in color at `ppem` pixels per em: painted from `COLR` with
    /// `foreground` for the palette's text color, or the nearest color bitmap
    /// strike. `None` when the face has no color form of `g`.
    pub fn color_glyph(&self, g: i64, ppem: f64, foreground: [u8; 4]) -> Option<ColorGlyph> {
        let model = self.model.as_ref()?;
        let gid = GlyphId::new(u32::try_from(g).ok()?);
        if let Some(glyph) = model.color_glyphs().get(gid) {
            return color_glyph::paint(model, &glyph, ppem, foreground);
        }
        let bitmap = model
            .bitmap_strikes()
            .select_bitmap(gid, Some(ppem as f32))?;
        if bitmap.source() == BitmapSource::Ebdt {
            return None;
        }
        let (rgba, width, height) = match bitmap.pixels() {
            Pixels::Png(data) => {
                let image = crate::render::png::decode(data)?;
                (image.rgba, image.width, image.height)
            }
            Pixels::Bgra(pixels) => (
                pixels
                    .data()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|&[b, g, r, a]| unpremultiply([r, g, b, a]))
                    .collect(),
                i64::from(pixels.width()),
                i64::from(pixels.height()),
            ),
            Pixels::Mask(_) => return None,
        };
        let (bearing_x, bearing_y) = self.bitmap_bearings(&bitmap);
        Some(ColorGlyph {
            rgba,
            ppem: f64::from(bitmap.ppem()),
            width,
            height,
            bearing_x,
            bearing_y,
        })
    }

    /// Glyph `g` from the face's coverage bitmaps (`EBDT`) nearest `ppem`, for
    /// a bitmap font; `None` when `g` has an outline of its own or no bitmap.
    pub fn mask_glyph(&self, g: i64, ppem: f64) -> Option<MaskGlyph> {
        let model = self.model.as_ref()?;
        let gid = GlyphId::new(u32::try_from(g).ok()?);
        if let Some(outline) = model.outline_glyphs().get(gid) {
            let mut path = PathCommands::default();
            let _ = outline.draw(&DrawOptions::new(), DrawMemory::Scratch, &mut path);
            if !path.0.is_empty() {
                return None;
            }
        }
        let bitmap = model
            .bitmap_strikes()
            .select_bitmap(gid, Some(ppem as f32))?;
        let Pixels::Mask(mask) = bitmap.pixels() else {
            return None;
        };
        let (width, height) = (mask.width(), mask.height());
        let coverage = mask.pixels().collect();
        let (bearing_x, bearing_y) = self.bitmap_bearings(&bitmap);
        Some(MaskGlyph {
            coverage,
            ppem: f64::from(bitmap.ppem()),
            width: i64::from(width),
            height: i64::from(height),
            bearing_x,
            bearing_y,
        })
    }

    // A bitmap's image corner in strike pixels: right of the pen and up from
    // the baseline. The outer bearings are font units; the inner ones pixels.
    fn bitmap_bearings(&self, bitmap: &skrifa::font::glyph::bitmap::BitmapGlyph) -> (i64, i64) {
        let scale = f64::from(bitmap.ppem()) / self.units_per_em.max(1) as f64;
        let inner_top = match bitmap.origin() {
            Origin::TopLeft => bitmap.inner_y_bearing(),
            Origin::BottomLeft => bitmap.inner_y_bearing() + bitmap.height() as i32,
        };
        (
            (f64::from(bitmap.x_bearing()) * scale).round() as i64
                + i64::from(bitmap.inner_x_bearing()),
            (f64::from(bitmap.y_bearing()) * scale).round() as i64 + i64::from(inner_top),
        )
    }
}

// Premultiplied BGRA reordered to RGBA, back to straight alpha.
fn unpremultiply([r, g, b, a]: [u8; 4]) -> [u8; 4] {
    if a == 0 {
        return [0; 4];
    }
    let undo = |c: u8| ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8;
    [undo(r), undo(g), undo(b), a]
}

/// Collects an outline as the engine's path commands.
#[derive(Default)]
pub(crate) struct PathCommands(pub(crate) Vec<f64>);

impl PathSink for PathCommands {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.extend([MOVE, f64::from(x), f64::from(y)]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.extend([LINE, f64::from(x), f64::from(y)]);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.extend([
            QUAD,
            f64::from(cx0),
            f64::from(cy0),
            f64::from(x),
            f64::from(y),
        ]);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.extend([
            CUBIC,
            f64::from(cx0),
            f64::from(cy0),
            f64::from(cx1),
            f64::from(cy1),
            f64::from(x),
            f64::from(y),
        ]);
    }
    fn close(&mut self) {
        self.0.push(CLOSE);
    }
}

// Appends `first..=last` to sorted `ranges`, merging it with the last pair
// when they touch.
fn add_range(ranges: &mut Vec<i64>, first: i64, last: i64) {
    if first > last {
        return;
    };
    let n = ranges.len();
    if n > 0 && ranges[n - 1] + 1 >= first {
        ranges[n - 1] = ranges[n - 1].max(last)
    } else {
        ranges.push(first);
        ranges.push(last)
    }
}

pub(crate) fn be_u8(d: &[u8], at: i64) -> i64 {
    if at < 0 || at >= d.len() as i64 {
        0
    } else {
        d[at as usize] as i64
    }
}

pub(crate) fn be_u16(d: &[u8], at: i64) -> i64 {
    be_u8(d, at) * 256 + be_u8(d, at + 1)
}

pub(crate) fn be_u32(d: &[u8], at: i64) -> i64 {
    be_u16(d, at) * 65_536 + be_u16(d, at + 2)
}

pub(crate) fn tag(d: &[u8], at: i64) -> String {
    (0..4)
        .map(|i| char::from_u32(be_u8(d, at + i) as u32).unwrap_or('?'))
        .collect()
}

// Big-endian 16-bit words.
#[cfg(test)]
fn test_words(values: &[i64]) -> Vec<u8> {
    let mut out = vec![];
    for v in values {
        out.push(((v >> 8) & 0xff) as u8);
        out.push((v & 0xff) as u8)
    }
    out
}

// A font file holding `tables`, each (tag, bytes).
#[cfg(test)]
fn test_sfnt(tables: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = test_words(&[1, 0, tables.len() as i64, 0, 0, 0]);
    let mut at = 12 + tables.len() * 16;
    let mut bodies = vec![];
    for (name, body) in tables {
        for b in name.bytes() {
            out.push(b)
        }
        for b in test_words(&[
            0,
            0,
            (at as i64) >> 16,
            (at as i64) & 0xffff,
            (body.len() as i64) >> 16,
            (body.len() as i64) & 0xffff,
        ]) {
            out.push(b)
        }
        at += body.len();
        bodies.push(body)
    }
    for body in bodies {
        for b in body {
            out.push(*b)
        }
    }
    out
}

/// A variable face: glyph 1 (for `A`) is a 100-unit square whose right edge
/// a `wght` tuple (axis 100..400..900) moves 50 units at full weight. With
/// `explicit`, the tuple names only point 2 and the others are inferred.
#[cfg(test)]
pub fn variable_test_face(explicit: bool) -> Vec<u8> {
    let mut head = vec![0u8; 54];
    head[18] = 0x03;
    head[19] = 0xE8;
    let mut hhea = vec![0u8; 36];
    hhea[4] = 0x03;
    hhea[35] = 2;
    let mut cmap = test_words(&[0, 1, 3, 1, 0, 12]);
    for b in test_words(&[
        4,
        32,
        0,
        4,
        4,
        1,
        0,
        0x41,
        0xFFFF,
        0,
        0x41,
        0xFFFF,
        1 - 0x41,
        1,
        0,
        0,
    ]) {
        cmap.push(b)
    }
    let mut glyf = test_words(&[1, 0, 0, 100, 100, 3, 0]);
    for b in [1u8, 1u8, 1u8, 1u8] {
        glyf.push(b)
    }
    for b in test_words(&[0, 0, 100, 0, 0, 100, 0, -100]) {
        glyf.push(b)
    }
    let mut fvar = test_words(&[1, 0, 16, 2, 1, 20, 0, 8]);
    for b in "wght".bytes() {
        fvar.push(b)
    }
    for b in test_words(&[0x64, 0, 0x190, 0, 0x384, 0, 0, 256]) {
        fvar.push(b)
    }
    // One tuple with its peak at full weight; the data names every point
    // (the right edge, 2 and 3, by +50) or only point 2.
    let data: Vec<u8> = if explicit {
        vec![1u8, 0u8, 2u8, 0u8, 50u8, 0x80u8]
    } else {
        vec![0u8, 7u8, 0u8, 0u8, 50u8, 50u8, 0u8, 0u8, 0u8, 0u8, 0x87u8]
    };
    // Every point moves through the shared "all points" list the
    // serialized data starts with; the explicit form names its own.
    let mut variation = test_words(&[
        1 | if explicit { 0 } else { 0x8000 },
        10,
        (data.len() as i64),
        0x8000 | (if explicit { 0x2000 } else { 0 }),
        0x4000,
    ]);
    for b in data {
        variation.push(b)
    }
    if variation.len() % 2 == 1 {
        variation.push(0u8)
    };
    let mut gvar = test_words(&[
        1,
        0,
        1,
        0,
        0,
        26,
        2,
        0,
        0,
        26,
        0,
        0,
        (variation.len() / 2) as i64,
    ]);
    for b in variation {
        gvar.push(b)
    }
    test_sfnt(&[
        ("cmap".to_string(), cmap),
        ("fvar".to_string(), fvar),
        ("glyf".to_string(), glyf),
        ("gvar".to_string(), gvar),
        ("head".to_string(), head),
        ("hhea".to_string(), hhea),
        ("hmtx".to_string(), test_words(&[500, 0, 500, 0])),
        ("loca".to_string(), test_words(&[0, 0, 17])),
        ("maxp".to_string(), test_words(&[0, 0x5000, 2])),
    ])
}

/// A face whose glyph 1 (for `A`) is a 2x2 red PNG in a 20 ppem strike.
#[cfg(test)]
pub fn color_test_face() -> Vec<u8> {
    let png = crate::render::png::encode(2, 2, &[255, 0, 0, 255].repeat(4)).unwrap();
    let mut head = vec![0u8; 54];
    head[18] = 0x08;
    head[19] = 0x00;
    let mut cmap = test_words(&[0, 1, 3, 1, 0, 12]);
    for b in test_words(&[
        4,
        32,
        0,
        4,
        4,
        1,
        0,
        0x41,
        0xFFFF,
        0,
        0x41,
        0xFFFF,
        1 - 0x41,
        1,
        0,
        0,
    ]) {
        cmap.push(b)
    }
    let mut cblc = test_words(&[3, 0, 0, 1, 0, 56, 0, 24, 0, 1, 0, 0]);
    for _ in 0..12 {
        for b in test_words(&[0]) {
            cblc.push(b)
        }
    }
    for b in test_words(&[1, 1, 0x1414, 0x2001]) {
        cblc.push(b)
    }
    for b in test_words(&[1, 1, 0, 8, 1, 17, 0, 4, 0, 0, 0, 9 + (png.len() as i64)]) {
        cblc.push(b)
    }
    let mut cbdt = test_words(&[3, 0]);
    for b in [2u8, 2u8, 0u8, 2u8, 3u8] {
        cbdt.push(b)
    }
    for b in test_words(&[0, png.len() as i64]) {
        cbdt.push(b)
    }
    for b in png {
        cbdt.push(b)
    }
    let mut hhea = vec![0u8; 36];
    hhea[4] = 0x06;
    hhea[35] = 1;
    test_sfnt(&[
        ("CBDT".to_string(), cbdt),
        ("CBLC".to_string(), cblc),
        ("cmap".to_string(), cmap),
        ("head".to_string(), head),
        ("hhea".to_string(), hhea),
        ("maxp".to_string(), test_words(&[0, 0x5000, 2])),
    ])
}

#[cfg(test)]
mod tests {
    use super::{
        CUBIC, Face, LINE, MOVE, QUAD, color_test_face, test_sfnt, test_words, variable_test_face,
    };

    // A `name` table of Windows Unicode English records, (id, text) each.
    fn names(records: &[(i64, &str)]) -> Vec<u8> {
        let count = records.len() as i64;
        let mut out = test_words(&[0, count, 6 + 12 * count]);
        let mut strings = vec![];
        for (id, text) in records {
            let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
            out.extend(test_words(&[
                3,
                1,
                0x409,
                *id,
                bytes.len() as i64,
                strings.len() as i64,
            ]));
            strings.extend(bytes);
        }
        out.extend(strings);
        out
    }

    #[test]
    fn a_style_named_italic_marks_the_face_italic_where_its_flags_do_not() {
        let mut head = vec![0u8; 54];
        head[18] = 0x03;
        head[19] = 0xE8;
        let font = test_sfnt(&[
            ("head".to_string(), head),
            (
                "name".to_string(),
                names(&[
                    (1, "Lilex Light Italic"),
                    (2, "Regular"),
                    (16, "Lilex"),
                    (17, "Light Italic"),
                ]),
            ),
        ]);
        let (family, subfamily, _, italic) = Face::metadata(&font, 0).unwrap();
        assert_eq!(family, "Lilex");
        assert_eq!(subfamily, "Light Italic");
        assert!(italic);
    }

    use std::fs;

    // The largest x of glyph 1's outline at `weight`.
    fn right_edge(face: &Face, weight: f64) -> f64 {
        let path = face
            .instance(&crate::hmap! { "wght" => weight })
            .outline(1)
            .path;
        let mut most: f64 = -1.0;
        let mut i = 0;
        while i < path.len() {
            match path[i] {
                MOVE | LINE => {
                    most = most.max(path[i + 1]);
                    i += 3;
                }
                QUAD => i += 5,
                CUBIC => i += 7,
                _ => i += 1,
            }
        }
        most
    }

    #[test]
    fn a_variable_face_moves_points_with_its_coordinates() {
        let face = Face::parse(&variable_test_face(false), 0).unwrap();
        assert_eq!(
            face.instance(&crate::hmap! { "wght" => 650.0 }).coords,
            vec![0.5]
        );
        assert_eq!(
            face.instance(&crate::hmap! { "wght" => 250.0 }).coords,
            vec![-0.5]
        );
        assert_eq!(right_edge(&face, 400.0), 100.0);
        assert_eq!(right_edge(&face, 900.0), 150.0);
        assert_eq!(right_edge(&face, 650.0), 125.0);
        // Below the default the tuple (peak at +1) does not apply.
        assert_eq!(right_edge(&face, 100.0), 100.0)
    }

    #[test]
    fn points_a_variation_leaves_out_are_inferred() {
        // Only point 2 moves; as the contour's one named point, every point
        // follows it.
        let face = Face::parse(&variable_test_face(true), 0).unwrap();
        assert_eq!(right_edge(&face, 900.0), 150.0);
        let left = face
            .instance(&crate::hmap! { "wght" => 900.0 })
            .outline(1)
            .path[1];
        assert_eq!(left, 50.0)
    }

    #[test]
    fn a_color_bitmap_face_answers_its_strike() {
        let face = Face::parse(&color_test_face(), 0).unwrap();
        assert!(face.has_color_glyphs());
        let g = face.glyph_for('A');
        assert_eq!(g, 1);
        let bitmap = face.color_glyph(g, 40.0, [0, 0, 0, 255]).unwrap();
        assert_eq!(
            (
                bitmap.ppem,
                bitmap.width,
                bitmap.height,
                bitmap.bearing_x,
                bitmap.bearing_y
            ),
            (20.0, 2, 2, 0, 2)
        );
        assert_eq!(&bitmap.rgba[..4], &[255, 0, 0, 255]);
        assert!(face.color_glyph(0, 40.0, [0, 0, 0, 255]).is_none());
        let sans = Face::parse(&fs::read("assets/fonts/DejaVuSans.ttf").unwrap(), 0).unwrap();
        assert!(!sans.has_color_glyphs())
    }

    #[test]
    fn an_outline_face_draws_closed_contours_in_font_units() {
        let sans = Face::parse(&fs::read("assets/fonts/DejaVuSans.ttf").unwrap(), 0).unwrap();
        let o = sans.outline(sans.glyph_for('O'));
        assert_eq!(o.path[0], MOVE);
        assert_eq!(o.path.iter().filter(|c| **c == super::CLOSE).count(), 2);
        assert!(o.advance > 0);
        assert_eq!(sans.h_metrics(sans.glyph_for('O')).0, o.advance);
    }
}
