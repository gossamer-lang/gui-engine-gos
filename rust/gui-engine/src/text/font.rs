// font: OpenType parsing. A `Face` reads one font from a TrueType,
// OpenType/CFF, or collection file: its metrics, its character map, and
// each glyph's outline as path commands in font units, y up.
//
// Every read is bounds-checked: a malformed or truncated file answers
// an error or an empty glyph, never a panic, so a broken system font
// cannot take the interface down.

use crate::errors;
use crate::strext::StrExt;
use std::collections::HashMap;

use crate::text::variation;

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

/// The CFF data a Type 2 charstring interpreter needs.
#[derive(Debug, Clone, PartialEq, Default)]
struct Cff {
    /// Absolute offset of the CharStrings INDEX.
    charstrings: i64,
    global_subrs: i64,
    /// Local subroutine INDEX per font dict (one entry for non-CID fonts).
    local_subrs: Vec<i64>,
    /// FDSelect offset for CID-keyed fonts, or -1.
    fd_select: i64,
    nominal_width: Vec<f64>,
}

/// One font face read from a file. Glyph ids are the font's own;
/// `glyph_for` maps characters to them.
/// A color bitmap glyph (CBDT): PNG data and its metrics in the pixels of
/// the strike it was drawn for.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorGlyph {
    pub png: Vec<u8>,
    /// The strike's pixels per em.
    pub ppem: i64,
    pub width: i64,
    pub height: i64,
    /// From the pen to the image's left edge, and from the baseline up to its top.
    pub bearing_x: i64,
    pub bearing_y: i64,
}

/// A font file's bytes: a file built into the program, read where it lies, or
/// one loaded at run time, shared by every face it holds.
#[derive(Clone)]
pub enum FontBytes {
    Static(&'static [u8]),
    Shared(std::sync::Arc<Vec<u8>>),
}

impl FontBytes {
    /// Bytes loaded at run time, taken without a copy.
    pub fn owned(data: Vec<u8>) -> Self {
        Self::Shared(std::sync::Arc::new(data))
    }

    /// The heap the bytes occupy; a built-in file occupies none.
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Static(_) => 0,
            Self::Shared(data) => data.len(),
        }
    }
}

impl std::ops::Deref for FontBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Static(data) => data,
            Self::Shared(data) => data,
        }
    }
}

impl PartialEq for FontBytes {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl std::fmt::Debug for FontBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FontBytes({} bytes)", self.len())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub family: String,
    pub subfamily: String,
    pub bold: bool,
    pub italic: bool,
    pub monospace: bool,
    pub units_per_em: i64,
    pub ascent: i64,
    pub descent: i64,
    pub line_gap: i64,
    pub num_glyphs: i64,
    data: FontBytes,
    /// Offset of this face's table directory (non-zero inside a collection).
    base: i64,
    tables: HashMap<String, (i64, i64)>,
    long_loca: bool,
    num_hmetrics: i64,
    cmap: i64,
    cmap_format: i64,
    cff: Option<Cff>,
    /// Offsets of the color bitmap location and data tables, or -1.
    cblc: i64,
    cbdt: i64,
    /// Normalized variation coordinates, one per `fvar` axis; empty for the
    /// default instance.
    pub coords: Vec<f64>,
    /// The face's index in the font registry, or -1 when it is not registered.
    pub slot: i64,
}

impl Face {
    /// The address and length of the file this face shares with the other
    /// faces loaded from it, when it was loaded at run time.
    pub(crate) fn shared_file(&self) -> Option<(usize, usize)> {
        match &self.data {
            FontBytes::Static(_) => None,
            FontBytes::Shared(data) => Some((std::sync::Arc::as_ptr(data) as usize, data.len())),
        }
    }

    pub(crate) fn owned_bytes(&self) -> usize {
        size_of::<Self>()
            + self.data.heap_bytes()
            + self.family.capacity()
            + self.subfamily.capacity()
            + self.tables.capacity() * (size_of::<(String, (i64, i64))>() + 32)
            + self.tables.keys().map(String::capacity).sum::<usize>()
            + self.coords.capacity() * size_of::<f64>()
            + self.cff.as_ref().map_or(0, |cff| {
                cff.local_subrs.capacity() * 8 + cff.nominal_width.capacity() * 8
            })
    }
}

impl Default for Face {
    fn default() -> Face {
        Face {
            family: String::new(),
            subfamily: String::new(),
            bold: false,
            italic: false,
            monospace: false,
            units_per_em: 0,
            ascent: 0,
            descent: 0,
            line_gap: 0,
            num_glyphs: 0,
            data: FontBytes::Static(&[]),
            base: 0,
            tables: HashMap::new(),
            long_loca: false,
            num_hmetrics: 0,
            cmap: 0,
            cmap_format: 0,
            cff: None,
            cblc: -1,
            cbdt: -1,
            coords: vec![],
            slot: -1,
        }
    }
}

impl Face {
    /// Parses the face at `index` of a font file (index 0 for a single font).
    fn directory(bytes: FontBytes, index: i64) -> Result<Face, errors::Error> {
        let data: &[u8] = &bytes;
        if data.len() < 12 {
            return Err(errors::new("font data is too short"));
        }
        let mut base = 0;
        if tag(data, 0) == "ttcf" {
            let count = be_u32(data, 8);
            if index < 0 || index >= count {
                return Err(errors::new(format!(
                    "font collection has no face {}",
                    index
                )));
            }
            base = be_u32(data, 12 + index * 4);
        }
        let num_tables = be_u16(data, base + 4);
        let mut tables = HashMap::new();
        let mut i = 0;
        while i < num_tables {
            let rec = base + 12 + i * 16;
            if rec + 16 > data.len() as i64 {
                return Err(errors::new("font table directory is truncated"));
            }
            let offset = be_u32(data, rec + 8);
            let length = be_u32(data, rec + 12);
            if offset + length > data.len() as i64 {
                return Err(errors::new(format!(
                    "font table {} lies outside the file",
                    tag(data, rec)
                )));
            }
            tables.insert(tag(data, rec).to_string(), (offset, length));
            i += 1;
        }
        Ok(Face {
            data: bytes.clone(),
            base,
            tables,
            ..Face::default()
        })
    }
    /// Reads only family/style metadata, without loading glyph outlines.
    pub fn metadata(
        data: &[u8],
        index: i64,
    ) -> Result<(String, String, bool, bool), errors::Error> {
        let mut face = Face::directory(FontBytes::owned(data.to_vec()), index)?;
        face.read_head()?;
        face.read_names();
        Ok((face.family, face.subfamily, face.bold, face.italic))
    }
    pub fn parse(data: &[u8], index: i64) -> Result<Face, errors::Error> {
        Self::parse_bytes(FontBytes::owned(data.to_vec()), index)
    }

    /// Parses the face at `index` of `bytes`, which it shares rather than copies.
    pub fn parse_bytes(bytes: FontBytes, index: i64) -> Result<Face, errors::Error> {
        let mut face = Face::directory(bytes, index)?;
        face.read_head()?;
        face.read_metrics()?;
        face.read_cmap()?;
        face.read_names();
        if let Some((cblc, _)) = face.tables.get("CBLC")
            && let Some((cbdt, _)) = face.tables.get("CBDT")
        {
            face.cblc = *cblc;
            face.cbdt = *cbdt;
        }
        if face.tables.contains_key("CFF ") {
            face.cff = Some(face.read_cff()?);
        } else if (!face.tables.contains_key("glyf") || !face.tables.contains_key("loca"))
            && face.cblc < 0
        {
            return Err(errors::new("font has neither outlines nor color bitmaps"));
        }
        Ok(face)
    }

    /// Reads the names, style, metrics and character map of a single-face
    /// font file whose outline tables may be absent, as a catalog summary.
    pub fn summary(data: &[u8]) -> Result<Face, errors::Error> {
        let mut face = Face::directory(FontBytes::owned(data.to_vec()), 0)?;
        face.read_head()?;
        face.read_metrics()?;
        face.read_cmap()?;
        face.read_names();
        Ok(face)
    }

    /// This face at the user axis values given by tag (`wght`, `wdth`, ...);
    /// axes not named keep their defaults.
    pub fn instance(&self, values: &HashMap<String, f64>) -> Face {
        let mut out = self.clone();
        out.coords = variation::normalize(self, values);
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

    /// The table's absolute offset and length, if the face has it.
    pub fn table(&self, name: &str) -> Option<(i64, i64)> {
        self.tables.get(name).copied()
    }

    /// The raw file bytes, for the layout-table readers.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// The font file a document can embed: a whole single-face file at its
    /// default instance, with whether its outlines are CFF.
    pub fn program(&self) -> Option<(&[u8], bool)> {
        if self.base != 0 || !self.coords.is_empty() {
            return None;
        }
        if self.tables.contains_key("glyf") {
            Some((&self.data, false))
        } else if self.tables.contains_key("CFF ") {
            Some((&self.data, true))
        } else {
            None
        }
    }

    fn read_head(&mut self) -> Result<(), errors::Error> {
        let Some((head, _)) = self.tables.get("head") else {
            return Err(errors::new("font has no head table"));
        };
        self.units_per_em = be_u16(&self.data, head + 18).max(16);
        self.long_loca = be_i16(&self.data, head + 50) != 0;
        let style = be_u16(&self.data, head + 44);
        self.bold = style & 1 != 0;
        self.italic = style & 2 != 0;
        if let Some((maxp, _)) = self.tables.get("maxp") {
            self.num_glyphs = be_u16(&self.data, maxp + 4);
        }
        Ok(())
    }

    fn read_metrics(&mut self) -> Result<(), errors::Error> {
        let Some((hhea, _)) = self.tables.get("hhea") else {
            return Err(errors::new("font has no hhea table"));
        };
        self.ascent = be_i16(&self.data, hhea + 4);
        self.descent = be_i16(&self.data, hhea + 6);
        self.line_gap = be_i16(&self.data, hhea + 8);
        self.num_hmetrics = be_u16(&self.data, hhea + 34);
        if let Some((os2, len)) = self.tables.get("OS/2")
            && *len >= 64
        {
            let weight = be_u16(&self.data, os2 + 4);
            if weight >= 600 {
                self.bold = true
            };
            let selection = be_u16(&self.data, os2 + 62);
            if selection & 1 != 0 {
                self.italic = true
            }
        }
        if let Some((post, len)) = self.tables.get("post")
            && *len >= 16
        {
            self.monospace = be_u32(&self.data, post + 12) != 0
        }
        if self.ascent == 0 && self.descent == 0 {
            self.ascent = self.units_per_em * 4 / 5;
            self.descent = -(self.units_per_em / 5);
        }
        Ok(())
    }

    // Prefers a full-repertoire Unicode subtable (format 12) over the BMP one.
    fn read_cmap(&mut self) -> Result<(), errors::Error> {
        let Some((cmap, _)) = self.tables.get("cmap") else {
            return Err(errors::new("font has no cmap table"));
        };
        let count = be_u16(&self.data, cmap + 2);
        let mut best = -1;
        let mut best_rank = -1;
        let mut i = 0;
        while i < count {
            let rec = cmap + 4 + i * 8;
            let platform = be_u16(&self.data, rec);
            let encoding = be_u16(&self.data, rec + 2);
            let offset = cmap + be_u32(&self.data, rec + 4);
            let kind = be_u16(&self.data, offset);
            let rank = if kind == 12 && (platform == 0 || (platform == 3 && encoding == 10)) {
                3
            } else if kind == 4 && (platform == 0 || (platform == 3 && encoding == 1)) {
                2
            } else if kind == 4 || kind == 12 {
                1
            } else {
                -1
            };
            if rank > best_rank {
                best_rank = rank;
                best = offset;
            }
            i += 1;
        }
        if best < 0 {
            return Err(errors::new("font has no Unicode character map"));
        }
        self.cmap = best;
        self.cmap_format = be_u16(&self.data, best);
        Ok(())
    }

    fn read_names(&mut self) {
        let Some((name, len)) = self.tables.get("name") else {
            return;
        };
        let count = be_u16(&self.data, name + 2);
        let strings = name + be_u16(&self.data, name + 4);
        let mut family = String::new();
        let mut typographic = String::new();
        let mut subfamily = String::new();
        let mut i = 0;
        while i < count {
            let rec = name + 6 + i * 12;
            if rec + 12 > name + len {
                break;
            }
            let platform = be_u16(&self.data, rec);
            let encoding = be_u16(&self.data, rec + 2);
            let language = be_u16(&self.data, rec + 4);
            let id = be_u16(&self.data, rec + 6);
            let length = be_u16(&self.data, rec + 8);
            let offset = strings + be_u16(&self.data, rec + 10);
            let text = if platform == 3
                && (encoding == 1 || encoding == 10)
                && (language == 0x409 || language == 0)
            {
                utf16be(&self.data, offset, length)
            } else if platform == 1 && encoding == 0 && language == 0 {
                latin1(&self.data, offset, length)
            } else {
                String::new()
            };
            if !text.is_empty() {
                match id {
                    1 if (family.is_empty() || platform == 3) => family = text,
                    2 if (subfamily.is_empty() || platform == 3) => subfamily = text,
                    16 => typographic = text,
                    _ => (),
                }
            }
            i += 1;
        }
        self.family = if typographic.is_empty() {
            family
        } else {
            typographic
        };
        self.subfamily = subfamily;
    }

    /// The glyph id for `c`, or 0 (the missing glyph).
    pub fn glyph_for(&self, c: char) -> i64 {
        let code = c as i64;
        let d = &self.data;
        let t = self.cmap;
        if self.cmap_format == 12 {
            let groups = be_u32(d, t + 12);
            let mut lo = 0;
            let mut hi = groups - 1;
            while lo <= hi {
                let mid = (lo + hi) / 2;
                let g = t + 16 + mid * 12;
                let start = be_u32(d, g);
                let end = be_u32(d, g + 4);
                if code < start {
                    hi = mid - 1;
                } else if code > end {
                    lo = mid + 1;
                } else {
                    return be_u32(d, g + 8) + code - start;
                }
            }
            return 0;
        }
        if code > 0xffff {
            return 0;
        }
        let segs = be_u16(d, t + 6) / 2;
        let ends = t + 14;
        let starts = ends + segs * 2 + 2;
        let deltas = starts + segs * 2;
        let range_offsets = deltas + segs * 2;
        let mut lo = 0;
        let mut hi = segs - 1;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let end = be_u16(d, ends + mid * 2);
            let start = be_u16(d, starts + mid * 2);
            if code > end {
                lo = mid + 1;
            } else if code < start {
                hi = mid - 1;
            } else {
                let delta = be_u16(d, deltas + mid * 2);
                let ro = be_u16(d, range_offsets + mid * 2);
                if ro == 0 {
                    return (code + delta) % 65_536;
                }
                let at = range_offsets + mid * 2 + ro + (code - start) * 2;
                let g = be_u16(d, at);
                return if g == 0 { 0 } else { (g + delta) % 65_536 };
            }
        }
        0
    }

    /// The scalars `glyph_for` maps to a real glyph, as inclusive
    /// (first, last) pairs flattened into one sorted, merged list.
    pub fn coverage(&self) -> Vec<i64> {
        let d = &self.data;
        let t = self.cmap;
        let mut out: Vec<i64> = vec![];
        if self.cmap_format == 12 {
            let groups = be_u32(d, t + 12).min(((d.len() as i64 - t - 16).max(0)) / 12);
            let mut previous = -1;
            for i in 0..groups {
                let g = t + 16 + i * 12;
                let start = be_u32(d, g);
                let end = be_u32(d, g + 4).min(0x10ffff);
                if start <= previous || start > end {
                    continue;
                }
                previous = end;
                let first = if be_u32(d, g + 8) == 0 {
                    start + 1
                } else {
                    start
                };
                add_range(&mut out, first, end)
            }
            return out;
        }
        let segs = be_u16(d, t + 6) / 2;
        let ends = t + 14;
        let starts = ends + segs * 2 + 2;
        let deltas = starts + segs * 2;
        let range_offsets = deltas + segs * 2;
        let mut previous = -1;
        for i in 0..segs {
            let start = be_u16(d, starts + i * 2);
            let end = be_u16(d, ends + i * 2);
            if start <= previous || start > end {
                continue;
            }
            previous = end;
            let delta = be_u16(d, deltas + i * 2);
            let ro = be_u16(d, range_offsets + i * 2);
            if ro == 0 {
                // Glyph ids are taken modulo 65536: exactly one code of the
                // segment's residue class lands on glyph 0.
                let zero = (65_536 - delta) % 65_536;
                if zero >= start && zero <= end {
                    add_range(&mut out, start, zero - 1);
                    add_range(&mut out, zero + 1, end)
                } else {
                    add_range(&mut out, start, end)
                }
                continue;
            }
            let mut code = start;
            while code <= end {
                let raw = be_u16(d, range_offsets + i * 2 + ro + (code - start) * 2);
                if raw != 0 && (raw + delta) % 65_536 != 0 {
                    add_range(&mut out, code, code)
                };
                code += 1;
            }
        }
        out
    }

    /// Whether the face draws glyphs as color bitmaps (CBDT) rather than
    /// outlines.
    pub fn has_color_bitmaps(&self) -> bool {
        self.cblc >= 0
    }

    /// The color bitmap of glyph `g` from the strike nearest `ppem` (the
    /// smallest at least as large, else the largest), if the face has one.
    pub fn color_bitmap(&self, g: i64, ppem: f64) -> Option<ColorGlyph> {
        if self.cblc < 0 {
            return None;
        };
        let d = &self.data;
        let sizes = be_u32(d, self.cblc + 4);
        let mut best = -1;
        let mut best_ppem = 0;
        for i in 0..sizes {
            let rec = self.cblc + 8 + i * 48;
            let first = be_u16(d, rec + 40);
            let last = be_u16(d, rec + 42);
            if g < first || g > last {
                continue;
            };
            let strike = be_u8(d, rec + 45);
            let large = strike as f64 >= ppem;
            let best_large = best_ppem as f64 >= ppem;
            let better = best < 0
                || large && !best_large
                || large && strike < best_ppem
                || !best_large && strike > best_ppem;
            if better {
                best = rec;
                best_ppem = strike;
            }
        }
        if best < 0 {
            return None;
        };
        let array = self.cblc + be_u32(d, best);
        let count = be_u32(d, best + 8);
        for i in 0..count {
            let entry = array + i * 8;
            let first = be_u16(d, entry);
            let last = be_u16(d, entry + 2);
            if g < first || g > last {
                continue;
            };
            let sub = array + be_u32(d, entry + 4);
            return self.bitmap_in_subtable(sub, g, first, best_ppem);
        }
        None
    }

    // Reads glyph `g` through one CBLC index subtable that starts at glyph `first`.
    fn bitmap_in_subtable(&self, sub: i64, g: i64, first: i64, ppem: i64) -> Option<ColorGlyph> {
        let d = &self.data;
        let index_format = be_u16(d, sub);
        let image_format = be_u16(d, sub + 2);
        let data = self.cbdt + be_u32(d, sub + 4);
        // Big metrics shared by every glyph of formats 2 and 5.
        let mut shared = -1;
        let at = match index_format {
            1 => data + be_u32(d, sub + 8 + (g - first) * 4),
            3 => data + be_u16(d, sub + 8 + (g - first) * 2),
            2 => {
                shared = sub + 12;
                data + be_u32(d, sub + 8) * (g - first)
            }
            4 => {
                let n = be_u32(d, sub + 8);
                let mut found = -1;
                for k in 0..n {
                    if be_u16(d, sub + 12 + k * 4) == g {
                        found = data + be_u16(d, sub + 12 + k * 4 + 2)
                    }
                }
                found
            }
            5 => {
                shared = sub + 12;
                let n = be_u32(d, sub + 20);
                let mut found = -1;
                for k in 0..n {
                    if be_u16(d, sub + 24 + k * 2) == g {
                        found = data + be_u32(d, sub + 8) * k
                    }
                }
                found
            }
            _ => -1,
        };
        if at < 0 {
            return None;
        };
        let (metrics, png_at) = match image_format {
            17 => (at, at + 5),
            18 => (at, at + 8),
            19 => (shared, at),
            _ => return None,
        };
        if metrics < 0 {
            return None;
        };
        let length = be_u32(d, png_at);
        let start = png_at + 4;
        if start + length > d.len() as i64 {
            return None;
        };
        Some(ColorGlyph {
            png: d[start as usize..(start + length) as usize].to_vec(),
            ppem,
            height: be_u8(d, metrics),
            width: be_u8(d, metrics + 1),
            bearing_x: be_i8(d, metrics + 2),
            bearing_y: be_i8(d, metrics + 3),
        })
    }

    /// Whether the face has a real glyph for `c`.
    pub fn covers(&self, c: char) -> bool {
        self.glyph_for(c) != 0
    }

    /// Horizontal advance and left side bearing of glyph `g`, font units.
    pub fn h_metrics(&self, g: i64) -> (i64, i64) {
        let (advance, lsb) = self.default_metrics(g);
        if self.coords.is_empty() || !self.tables.contains_key("gvar") {
            return (advance, lsb);
        }
        let delta = if self.tables.contains_key("HVAR") {
            variation::advance_delta(self, g)
        } else {
            self.phantom_advance_delta(g)
        };
        (advance + (delta + 0.5).floor() as i64, lsb)
    }

    // The advance change the phantom points carry, for fonts without HVAR.
    fn phantom_advance_delta(&self, g: i64) -> f64 {
        let (start, end) = self.glyph_range(g);
        let mut xs = vec![];
        let mut ys = vec![];
        let mut ends = vec![];
        if end - start >= 10 && be_i16(&self.data, start) >= 0 {
            if let Some(points) = decode_simple(&self.data, start, end, be_i16(&self.data, start)) {
                xs = points.xs;
                ys = points.ys;
                ends = points.ends;
            }
        } else if end - start >= 10 {
            let count = components(&self.data, start, end).len();
            xs = vec![0.0; count];
            ys = vec![0.0; count];
        }
        let (dx, _) = variation::glyph_deltas(self, g, &xs, &ys, &ends);
        let n = xs.len();
        dx[n + 1] - dx[n]
    }

    fn default_metrics(&self, g: i64) -> (i64, i64) {
        let Some((hmtx, len)) = self.tables.get("hmtx") else {
            return (self.units_per_em / 2, 0);
        };
        let n = self.num_hmetrics.max(1);
        if g < n {
            let at = hmtx + g * 4;
            if at + 4 > hmtx + len {
                return (0, 0);
            };
            return (be_u16(&self.data, at), be_i16(&self.data, at + 2));
        }
        let advance = be_u16(&self.data, hmtx + (n - 1) * 4);
        let at = hmtx + n * 4 + (g - n) * 2;
        let lsb = if at + 2 <= hmtx + len {
            be_i16(&self.data, at)
        } else {
            0
        };
        (advance, lsb)
    }

    /// Glyph `g`'s outline in font units.
    pub fn outline(&self, g: i64) -> Outline {
        let (advance, lsb) = self.h_metrics(g);
        if g < 0 || g >= self.num_glyphs.max(1) {
            return Outline {
                path: vec![],
                advance,
                lsb,
            };
        }
        let mut path = vec![];
        match self.cff {
            Some(ref cff) => {
                let _ = charstring_outline(&self.data, cff, g, &mut path);
            }
            None => self.glyf_outline(g, 0, &mut path, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
        }
        Outline { path, advance, lsb }
    }

    fn glyph_range(&self, g: i64) -> (i64, i64) {
        let Some((loca, len)) = self.tables.get("loca") else {
            return (0, 0);
        };
        let Some((glyf, glen)) = self.tables.get("glyf") else {
            return (0, 0);
        };
        let (start, end) = if self.long_loca {
            if loca + g * 4 + 8 > loca + len {
                return (0, 0);
            };
            (
                be_u32(&self.data, loca + g * 4),
                be_u32(&self.data, loca + g * 4 + 4),
            )
        } else {
            if loca + g * 2 + 4 > loca + len {
                return (0, 0);
            };
            (
                be_u16(&self.data, loca + g * 2) * 2,
                be_u16(&self.data, loca + g * 2 + 2) * 2,
            )
        };
        if end <= start || end > *glen {
            return (0, 0);
        }
        (glyf + start, glyf + end)
    }

    // Appends glyph `g` transformed by the matrix (a b c d) and offset (e, f).
    fn glyf_outline(
        &self,
        g: i64,
        depth: i64,
        path: &mut Vec<f64>,
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        e: f64,
        f: f64,
    ) {
        if depth > 8 {
            return;
        }
        let (start, end) = self.glyph_range(g);
        if end - start < 10 {
            return;
        }
        let data = &self.data;
        let contours = be_i16(data, start);
        if contours >= 0 {
            let Some(mut points) = decode_simple(data, start, end, contours) else {
                return;
            };
            if !self.coords.is_empty() {
                let (dx, dy) =
                    variation::glyph_deltas(self, g, &points.xs, &points.ys, &points.ends);
                for k in 0..points.xs.len() {
                    points.xs[k] += dx[k];
                    points.ys[k] += dy[k];
                }
            }
            let mut first = 0;
            for last in points.ends {
                if last < first || last >= points.xs.len() as i64 {
                    return;
                }
                contour(
                    path,
                    &points.xs,
                    &points.ys,
                    &points.flags,
                    first,
                    last,
                    a,
                    b,
                    c,
                    d,
                    e,
                    f,
                );
                first = last + 1;
            }
            return;
        }
        let mut parts = components(data, start, end);
        if !self.coords.is_empty() {
            let zeros = vec![0.0; parts.len()];
            let (dx, dy) = variation::glyph_deltas(self, g, &zeros, &zeros, &[]);
            for k in 0..parts.len() {
                // Only offsets given as x and y values vary; point-matched ones do not.
                if parts[k].flags & 2 != 0 {
                    parts[k].dx += dx[k];
                    parts[k].dy += dy[k];
                }
            }
        }
        for part in parts {
            // Compose the component's transform with the parent's.
            let na = a * part.a + c * part.b;
            let nb = b * part.a + d * part.b;
            let nc = a * part.c + c * part.d;
            let nd = b * part.c + d * part.d;
            let ne = a * part.dx + c * part.dy + e;
            let nf = b * part.dx + d * part.dy + f;
            self.glyf_outline(part.glyph, depth + 1, path, na, nb, nc, nd, ne, nf)
        }
    }

    fn read_cff(&self) -> Result<Cff, errors::Error> {
        let Some((cff, len)) = self.tables.get("CFF ") else {
            return Err(errors::new("font has no CFF table"));
        };
        let d = &self.data;
        let header = be_u8(d, cff + 2);
        let names = cff + header;
        let top_dicts = index_end(d, names);
        let strings = index_end(d, top_dicts);
        let globals = index_end(d, strings);
        let (top_start, top_end) = index_item(d, top_dicts, 0);
        if top_end <= top_start || top_end > cff + len {
            return Err(errors::new("CFF top dictionary is missing"));
        }
        let top = dict(d, top_start, top_end);
        let charstrings = cff + dict_int(&top.clone(), 17, 0);
        let mut local_subrs = vec![];
        let mut nominal = vec![];
        let mut fd_select = -1;
        if top.contains_key(&1236) {
            // CID-keyed: each font dict carries its own private dict.
            fd_select = cff + dict_int(&top.clone(), 1237, 0);
            let fd_array = cff + dict_int(&top, 1236, 0);
            let count = be_u16(d, fd_array);
            let mut i = 0;
            while i < count {
                let (s, e) = index_item(d, fd_array, i);
                let fd = dict(d, s, e);
                let (subrs, width) = private_dict(d, *cff, &fd);
                local_subrs.push(subrs);
                nominal.push(width);
                i += 1;
            }
        } else {
            let (subrs, width) = private_dict(d, *cff, &top);
            local_subrs.push(subrs);
            nominal.push(width)
        }
        Ok(Cff {
            charstrings,
            global_subrs: globals,
            local_subrs,
            fd_select,
            nominal_width: nominal,
        })
    }
}

// ---- simple TrueType glyphs ----

// A simple glyph's points in font units, their flags and contour ends.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Points {
    xs: Vec<f64>,
    ys: Vec<f64>,
    flags: Vec<i64>,
    ends: Vec<i64>,
}

// Decodes a simple glyph's points; None when its data is truncated.
fn decode_simple(data: &[u8], start: i64, end: i64, contours: i64) -> Option<Points> {
    let mut ends = vec![];
    let mut at = start + 10;
    let mut i = 0;
    while i < contours {
        ends.push(be_u16(data, at));
        at += 2;
        i += 1;
    }
    if contours == 0 {
        return Some(Points {
            xs: vec![],
            ys: vec![],
            flags: vec![],
            ends: vec![],
        });
    }
    let count = ends[(contours - 1) as usize] + 1;
    let instructions = be_u16(data, at);
    at += 2 + instructions;
    let mut flags = vec![];
    while flags.len() < count as usize {
        if at >= end {
            return None;
        };
        let flag = be_u8(data, at);
        at += 1;
        flags.push(flag);
        if flag & 8 != 0 {
            let repeat = be_u8(data, at);
            at += 1;
            let mut r = 0;
            while r < repeat && flags.len() < count as usize {
                flags.push(flag);
                r += 1;
            }
        }
    }
    let mut xs = vec![0.0; count as usize];
    let mut x = 0;
    let mut k = 0;
    while k < count {
        let flag = flags[k as usize];
        if flag & 2 != 0 {
            let v = be_u8(data, at);
            at += 1;
            x += if flag & 16 != 0 { v } else { -v };
        } else if flag & 16 == 0 {
            x += be_i16(data, at);
            at += 2;
        }
        xs[k as usize] = x as f64;
        k += 1;
    }
    let mut ys = vec![0.0; count as usize];
    let mut y = 0;
    k = 0;
    while k < count {
        let flag = flags[k as usize];
        if flag & 4 != 0 {
            let v = be_u8(data, at);
            at += 1;
            y += if flag & 32 != 0 { v } else { -v };
        } else if flag & 32 == 0 {
            y += be_i16(data, at);
            at += 2;
        }
        ys[k as usize] = y as f64;
        k += 1;
    }
    Some(Points {
        xs,
        ys,
        flags,
        ends,
    })
}

// One component of a composite glyph: the glyph, its flags, its offset and
// its 2x2 transform.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Component {
    glyph: i64,
    flags: i64,
    dx: f64,
    dy: f64,
    a: f64,
    b: f64,
    c: f64,
    d: f64,
}

// The components of the composite glyph in `start..end`.
fn components(data: &[u8], start: i64, end: i64) -> Vec<Component> {
    let mut out = vec![];
    let mut at = start + 10;
    loop {
        if at + 4 > end {
            return out;
        };
        let flags = be_u16(data, at);
        let glyph = be_u16(data, at + 2);
        at += 4;
        let mut dx = 0.0;
        let mut dy = 0.0;
        if flags & 1 != 0 {
            if flags & 2 != 0 {
                dx = be_i16(data, at) as f64;
                dy = be_i16(data, at + 2) as f64;
            }
            at += 4;
        } else {
            if flags & 2 != 0 {
                dx = be_i8(data, at) as f64;
                dy = be_i8(data, at + 1) as f64;
            }
            at += 2;
        }
        let mut ma = 1.0;
        let mut mb = 0.0;
        let mut mc = 0.0;
        let mut md = 1.0;
        if flags & 8 != 0 {
            ma = f2dot14(data, at);
            md = ma;
            at += 2;
        } else if flags & 0x40 != 0 {
            ma = f2dot14(data, at);
            md = f2dot14(data, at + 2);
            at += 4;
        } else if flags & 0x80 != 0 {
            ma = f2dot14(data, at);
            mb = f2dot14(data, at + 2);
            mc = f2dot14(data, at + 4);
            md = f2dot14(data, at + 6);
            at += 8;
        }
        out.push(Component {
            glyph,
            flags,
            dx,
            dy,
            a: ma,
            b: mb,
            c: mc,
            d: md,
        });
        if flags & 0x20 == 0 {
            return out;
        }
    }
}

// Emits one quadratic contour, inserting the implied on-curve midpoints.
fn contour(
    path: &mut Vec<f64>,
    xs: &[f64],
    ys: &[f64],
    flags: &[i64],
    first: i64,
    last: i64,
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
) {
    let n = last - first + 1;
    if n < 2 {
        return;
    }
    let px = |i: i64| -> f64 { a * xs[i as usize] + c * ys[i as usize] + e };
    let py = |i: i64| -> f64 { b * xs[i as usize] + d * ys[i as usize] + f };
    let on = |i: i64| -> bool { flags[i as usize] & 1 != 0 };
    // Start at an on-curve point, or the midpoint of the first two off points.
    let start_x;
    let start_y;
    let mut origin = -1;
    let mut k = 0;
    while k < n {
        if on(first + k) {
            origin = k;
            break;
        }
        k += 1;
    }
    if origin < 0 {
        start_x = (px(first) + px(first + 1)) / 2.0;
        start_y = (py(first) + py(first + 1)) / 2.0;
        origin = 0;
    } else {
        start_x = px(first + origin);
        start_y = py(first + origin);
    }
    path.push(MOVE);
    path.push(start_x);
    path.push(start_y);
    let mut ctrl = false;
    let mut cx = 0.0;
    let mut cy = 0.0;
    let mut step = 1;
    while step <= n {
        let i = first + (origin + step) % n;
        let x = px(i);
        let y = py(i);
        if on(i) {
            if ctrl {
                path.push(QUAD);
                path.push(cx);
                path.push(cy);
                path.push(x);
                path.push(y);
                ctrl = false;
            } else {
                path.push(LINE);
                path.push(x);
                path.push(y)
            }
        } else {
            if ctrl {
                let mx = (cx + x) / 2.0;
                let my = (cy + y) / 2.0;
                path.push(QUAD);
                path.push(cx);
                path.push(cy);
                path.push(mx);
                path.push(my)
            }
            cx = x;
            cy = y;
            ctrl = true;
        }
        step += 1;
    }
    if ctrl {
        path.push(QUAD);
        path.push(cx);
        path.push(cy);
        path.push(start_x);
        path.push(start_y)
    }
    path.push(CLOSE)
}

// ---- CFF ----

// The end offset of the INDEX at `at` (its first byte past the data).
fn index_end(d: &[u8], at: i64) -> i64 {
    let count = be_u16(d, at);
    if count == 0 {
        return at + 2;
    }
    let size = be_u8(d, at + 2);
    let last = offset_n(d, at + 3 + count * size, size);
    at + 3 + (count + 1) * size + last - 1
}

// The absolute byte range of item `i` of the INDEX at `at`.
fn index_item(d: &[u8], at: i64, i: i64) -> (i64, i64) {
    let count = be_u16(d, at);
    if i < 0 || i >= count {
        return (0, 0);
    }
    let size = be_u8(d, at + 2);
    let data = at + 3 + (count + 1) * size - 1;
    let s = offset_n(d, at + 3 + i * size, size);
    let e = offset_n(d, at + 3 + (i + 1) * size, size);
    (data + s, data + e)
}

fn offset_n(d: &[u8], at: i64, size: i64) -> i64 {
    let mut v = 0;
    let mut k = 0;
    while k < size {
        v = v * 256 + be_u8(d, at + k);
        k += 1;
    }
    v
}

// A CFF DICT as operator -> operands. Two-byte operators are 1200 + b1.
fn dict(d: &[u8], start: i64, end: i64) -> HashMap<i64, Vec<f64>> {
    let mut out = HashMap::new();
    let mut operands = vec![];
    let mut at = start;
    while at < end {
        let b0 = be_u8(d, at);
        if b0 <= 21 {
            let mut op = b0;
            at += 1;
            if b0 == 12 {
                op = 1200 + be_u8(d, at);
                at += 1;
            }
            out.insert(op, operands);
            operands = vec![];
        } else if b0 == 28 {
            operands.push(be_i16(d, at + 1) as f64);
            at += 3;
        } else if b0 == 29 {
            operands.push(be_i32(d, at + 1) as f64);
            at += 5;
        } else if b0 == 30 {
            let (value, next) = real(d, at + 1, end);
            operands.push(value);
            at = next;
        } else if (32..=246).contains(&b0) {
            operands.push((b0 - 139) as f64);
            at += 1;
        } else if (247..=250).contains(&b0) {
            operands.push(((b0 - 247) * 256 + be_u8(d, at + 1) + 108) as f64);
            at += 2;
        } else if (251..=254).contains(&b0) {
            operands.push((-(b0 - 251) * 256 - be_u8(d, at + 1) - 108) as f64);
            at += 2;
        } else {
            at += 1;
        }
    }
    out
}

// A DICT real number: nibble-coded decimal.
fn real(d: &[u8], at: i64, end: i64) -> (f64, i64) {
    let mut text = String::new();
    let mut i = at;
    while i < end {
        let b = be_u8(d, i);
        i += 1;
        let mut done = false;
        for nibble in [b >> 4, b & 15] {
            match nibble {
                10 => text += ".",
                11 => text += "E",
                12 => text += "E-",
                14 => text += "-",
                15 => done = true,
                13 => (),
                _ => {
                    if nibble <= 9 {
                        text += &format!("{}", nibble)
                    }
                }
            }
            if done {
                break;
            }
        }
        if done {
            break;
        }
    }
    (text.to_f64().unwrap_or(0.0), i)
}

fn dict_int(dict: &HashMap<i64, Vec<f64>>, op: i64, default: i64) -> i64 {
    match dict.get(&op) {
        Some(v) => {
            if v.is_empty() {
                default
            } else {
                v[v.len() - 1] as i64
            }
        }
        None => default,
    }
}

// The local Subrs INDEX offset (or -1) and nominal width of a private dict.
fn private_dict(d: &[u8], cff: i64, owner: &HashMap<i64, Vec<f64>>) -> (i64, f64) {
    let Some(private) = owner.get(&18) else {
        return (-1, 0.0);
    };
    if private.len() < 2 {
        return (-1, 0.0);
    }
    let size = private[0] as i64;
    let offset = cff + private[1] as i64;
    let p = dict(d, offset, offset + size);
    let subrs = if p.contains_key(&19) {
        offset + dict_int(&p.clone(), 19, 0)
    } else {
        -1
    };
    let width = match p.get(&21) {
        Some(v) => {
            if v.is_empty() {
                0.0
            } else {
                v[0]
            }
        }
        None => 0.0,
    };
    (subrs, width)
}

fn subr_bias(count: i64) -> i64 {
    if count < 1240 {
        107
    } else if count < 33_900 {
        1131
    } else {
        32_768
    }
}

fn fd_index(d: &[u8], cff: &Cff, g: i64) -> i64 {
    if cff.fd_select < 0 {
        return 0;
    }
    let at = cff.fd_select;
    let kind = be_u8(d, at);
    if kind == 0 {
        return be_u8(d, at + 1 + g);
    }
    if kind == 3 {
        let ranges = be_u16(d, at + 1);
        let mut i = 0;
        while i < ranges {
            let r = at + 3 + i * 3;
            let first = be_u16(d, r);
            let next = be_u16(d, r + 3);
            if g >= first && g < next {
                return be_u8(d, r + 2);
            }
            i += 1;
        }
    }
    0
}

/// Mutable state of one Type 2 charstring run.
#[derive(Debug, Clone, PartialEq)]
struct Charstring {
    stack: Vec<f64>,
    x: f64,
    y: f64,
    stems: i64,
    open: bool,
    width_done: bool,
    depth: i64,
    done: bool,
}

fn charstring_outline(
    d: &[u8],
    cff: &Cff,
    g: i64,
    path: &mut Vec<f64>,
) -> Result<(), errors::Error> {
    let (start, end) = index_item(d, cff.charstrings, g);
    if end <= start {
        return Ok(());
    }
    let fd = fd_index(d, cff, g);
    let local = if fd < cff.local_subrs.len() as i64 {
        cff.local_subrs[fd as usize]
    } else {
        -1
    };
    let mut st = Charstring {
        stack: vec![],
        x: 0.0,
        y: 0.0,
        stems: 0,
        open: false,
        width_done: false,
        depth: 0,
        done: false,
    };
    run_charstring(d, cff, local, start, end, &mut st, path)?;
    if st.open {
        path.push(CLOSE)
    }
    Ok(())
}

fn cs_move(st: &mut Charstring, path: &mut Vec<f64>, dx: f64, dy: f64) {
    if st.open {
        path.push(CLOSE)
    }
    st.x += dx;
    st.y += dy;
    path.push(MOVE);
    path.push(st.x);
    path.push(st.y);
    st.open = true;
}

fn cs_line(st: &mut Charstring, path: &mut Vec<f64>, dx: f64, dy: f64) {
    st.x += dx;
    st.y += dy;
    path.push(LINE);
    path.push(st.x);
    path.push(st.y)
}

fn cs_curve(
    st: &mut Charstring,
    path: &mut Vec<f64>,
    dx1: f64,
    dy1: f64,
    dx2: f64,
    dy2: f64,
    dx3: f64,
    dy3: f64,
) {
    let x1 = st.x + dx1;
    let y1 = st.y + dy1;
    let x2 = x1 + dx2;
    let y2 = y1 + dy2;
    st.x = x2 + dx3;
    st.y = y2 + dy3;
    path.push(CUBIC);
    path.push(x1);
    path.push(y1);
    path.push(x2);
    path.push(y2);
    path.push(st.x);
    path.push(st.y)
}

// Drops a leading width operand when an operator finds one too many.
fn take_width(st: &mut Charstring, expected_even: bool) {
    if st.width_done {
        return;
    }
    st.width_done = true;
    let odd = st.stack.len() % 2 == 1;
    if odd == expected_even && !st.stack.is_empty() {
        st.stack.remove(0);
    }
}

fn run_charstring(
    d: &[u8],
    cff: &Cff,
    local: i64,
    start: i64,
    end: i64,
    st: &mut Charstring,
    path: &mut Vec<f64>,
) -> Result<(), errors::Error> {
    st.depth += 1;
    if st.depth > 10 {
        return Err(errors::new("charstring subroutines nest too deeply"));
    }
    let mut at = start;
    while at < end && !st.done {
        let b0 = be_u8(d, at);
        at += 1;
        if b0 >= 32 || b0 == 28 {
            if b0 == 28 {
                st.stack.push(be_i16(d, at) as f64);
                at += 2;
            } else if b0 <= 246 {
                st.stack.push((b0 - 139) as f64)
            } else if b0 <= 250 {
                st.stack
                    .push(((b0 - 247) * 256 + be_u8(d, at) + 108) as f64);
                at += 1;
            } else if b0 <= 254 {
                st.stack
                    .push((-(b0 - 251) * 256 - be_u8(d, at) - 108) as f64);
                at += 1;
            } else {
                st.stack.push(be_i32(d, at) as f64 / 65536.0);
                at += 4;
            }
            continue;
        }
        let s = st.stack.clone();
        match b0 {
            1 | 3 | 18 | 23 => {
                take_width(st, true);
                st.stems += (st.stack.len() / 2) as i64;
                st.stack = vec![];
            }
            19 | 20 => {
                take_width(st, true);
                st.stems += (st.stack.len() / 2) as i64;
                st.stack = vec![];
                at += (st.stems + 7) / 8;
            }
            21 => {
                take_width(st, true);
                let n = st.stack.len();
                if n >= 2 {
                    cs_move(st, path, st.stack[n - 2], st.stack[n - 1])
                };
                st.stack = vec![];
            }
            22 => {
                take_width(st, false);
                let n = st.stack.len();
                if n >= 1 {
                    cs_move(st, path, st.stack[n - 1], 0.0)
                };
                st.stack = vec![];
            }
            4 => {
                take_width(st, false);
                let n = st.stack.len();
                if n >= 1 {
                    cs_move(st, path, 0.0, st.stack[n - 1])
                };
                st.stack = vec![];
            }
            5 => {
                let mut i = 0;
                while i + 1 < s.len() {
                    cs_line(st, path, s[i], s[i + 1]);
                    i += 2;
                }
                st.stack = vec![];
            }
            6 | 7 => {
                let mut horizontal = b0 == 6;
                for v in s {
                    if horizontal {
                        cs_line(st, path, v, 0.0)
                    } else {
                        cs_line(st, path, 0.0, v)
                    };
                    horizontal = !horizontal;
                }
                st.stack = vec![];
            }
            8 => {
                let mut i = 0;
                while i + 5 < s.len() {
                    cs_curve(
                        st,
                        path,
                        s[i],
                        s[i + 1],
                        s[i + 2],
                        s[i + 3],
                        s[i + 4],
                        s[i + 5],
                    );
                    i += 6;
                }
                st.stack = vec![];
            }
            24 => {
                let mut i = 0;
                while i + 5 < s.len() - 2 {
                    cs_curve(
                        st,
                        path,
                        s[i],
                        s[i + 1],
                        s[i + 2],
                        s[i + 3],
                        s[i + 4],
                        s[i + 5],
                    );
                    i += 6;
                }
                if i + 1 < s.len() {
                    cs_line(st, path, s[i], s[i + 1])
                };
                st.stack = vec![];
            }
            25 => {
                let mut i = 0;
                while i + 1 < s.len() - 6 {
                    cs_line(st, path, s[i], s[i + 1]);
                    i += 2;
                }
                if i + 5 < s.len() {
                    cs_curve(
                        st,
                        path,
                        s[i],
                        s[i + 1],
                        s[i + 2],
                        s[i + 3],
                        s[i + 4],
                        s[i + 5],
                    )
                }
                st.stack = vec![];
            }
            26 => {
                let mut i = 0;
                let mut dx1 = 0.0;
                if s.len() % 2 == 1 {
                    dx1 = s[0];
                    i = 1;
                }
                while i + 3 < s.len() {
                    cs_curve(st, path, dx1, s[i], s[i + 1], s[i + 2], 0.0, s[i + 3]);
                    dx1 = 0.0;
                    i += 4;
                }
                st.stack = vec![];
            }
            27 => {
                let mut i = 0;
                let mut dy1 = 0.0;
                if s.len() % 2 == 1 {
                    dy1 = s[0];
                    i = 1;
                }
                while i + 3 < s.len() {
                    cs_curve(st, path, s[i], dy1, s[i + 1], s[i + 2], s[i + 3], 0.0);
                    dy1 = 0.0;
                    i += 4;
                }
                st.stack = vec![];
            }
            30 | 31 => {
                let mut vertical = b0 == 30;
                let mut i = 0;
                while i + 3 < s.len() {
                    let last = i + 4 >= s.len() - 1;
                    let extra = if last && s.len() - i == 5 {
                        s[i + 4]
                    } else {
                        0.0
                    };
                    if vertical {
                        cs_curve(st, path, 0.0, s[i], s[i + 1], s[i + 2], s[i + 3], extra)
                    } else {
                        cs_curve(st, path, s[i], 0.0, s[i + 1], s[i + 2], extra, s[i + 3])
                    }
                    vertical = !vertical;
                    i += 4;
                }
                st.stack = vec![];
            }
            10 | 29 => {
                let Some(index) = st.stack.pop() else {
                    return Ok(());
                };
                let subrs = if b0 == 10 { local } else { cff.global_subrs };
                if subrs < 0 {
                    return Err(errors::new("charstring calls a missing subroutine set"));
                }
                let count = be_u16(d, subrs);
                let n = index as i64 + subr_bias(count);
                let (ss, se) = index_item(d, subrs, n);
                if se > ss {
                    run_charstring(d, cff, local, ss, se, st, path)?
                }
            }
            11 => {
                st.depth -= 1;
                return Ok(());
            }
            14 => {
                take_width(st, true);
                st.done = true;
                st.stack = vec![];
            }
            12 => {
                let b1 = be_u8(d, at);
                at += 1;
                match b1 {
                    35 if s.len() >= 12 => {
                        cs_curve(st, path, s[0], s[1], s[2], s[3], s[4], s[5]);
                        cs_curve(st, path, s[6], s[7], s[8], s[9], s[10], s[11])
                    }
                    34 if s.len() >= 7 => {
                        cs_curve(st, path, s[0], 0.0, s[1], s[2], s[3], 0.0);
                        cs_curve(st, path, s[4], 0.0, s[5], -s[2], s[6], 0.0)
                    }
                    36 if s.len() >= 9 => {
                        cs_curve(st, path, s[0], s[1], s[2], s[3], s[4], 0.0);
                        cs_curve(st, path, s[5], 0.0, s[6], s[7], s[8], -(s[1] + s[3] + s[7]))
                    }
                    37 if s.len() >= 11 => {
                        let sx = s[0] + s[2] + s[4] + s[6] + s[8];
                        let sy = s[1] + s[3] + s[5] + s[7] + s[9];
                        cs_curve(st, path, s[0], s[1], s[2], s[3], s[4], s[5]);
                        if sx.abs() > sy.abs() {
                            cs_curve(st, path, s[6], s[7], s[8], s[9], s[10], -sy)
                        } else {
                            cs_curve(st, path, s[6], s[7], s[8], s[9], -sx, s[10])
                        }
                    }
                    _ => (),
                }
                st.stack = vec![];
            }
            _ => st.stack = vec![],
        }
    }
    st.depth -= 1;
    Ok(())
}

// ---- byte readers (out-of-range reads answer zero) ----

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

pub(crate) fn be_i8(d: &[u8], at: i64) -> i64 {
    let v = be_u8(d, at);
    if v >= 128 { v - 256 } else { v }
}

pub(crate) fn be_u16(d: &[u8], at: i64) -> i64 {
    be_u8(d, at) * 256 + be_u8(d, at + 1)
}

pub(crate) fn be_i16(d: &[u8], at: i64) -> i64 {
    let v = be_u16(d, at);
    if v >= 32_768 { v - 65_536 } else { v }
}

pub(crate) fn be_u32(d: &[u8], at: i64) -> i64 {
    be_u16(d, at) * 65_536 + be_u16(d, at + 2)
}

pub(crate) fn be_i32(d: &[u8], at: i64) -> i64 {
    let v = be_u32(d, at);
    if v >= 2_147_483_648 {
        v - 4_294_967_296
    } else {
        v
    }
}

fn f2dot14(d: &[u8], at: i64) -> f64 {
    be_i16(d, at) as f64 / 16384.0
}

pub(crate) fn tag(d: &[u8], at: i64) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < 4 {
        match char::from_u32(be_u8(d, at + i) as u32) {
            Some(c) => s.push(c),
            None => s.push('?'),
        }
        i += 1;
    }
    s
}

fn utf16be(d: &[u8], at: i64, len: i64) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i + 1 < len {
        let mut unit = be_u16(d, at + i);
        i += 2;
        if (0xd800..0xdc00).contains(&unit) && i + 1 < len {
            let low = be_u16(d, at + i);
            i += 2;
            unit = 0x10000 + (unit - 0xd800) * 1024 + (low - 0xdc00);
        }
        if let Some(c) = char::from_u32(unit as u32) {
            s.push(c)
        }
    }
    s
}

fn latin1(d: &[u8], at: i64, len: i64) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < len {
        if let Some(c) = char::from_u32(be_u8(d, at + i) as u32) {
            s.push(c)
        }
        i += 1;
    }
    s
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
    let mut variation = test_words(&[
        1,
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
    use super::{CUBIC, Face, LINE, MOVE, QUAD, color_test_face, variable_test_face};

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
            crate::text::variation::axes(&face)
                .iter()
                .map(|a| (a.tag.as_str(), a.min, a.default, a.max))
                .collect::<Vec<_>>(),
            vec![("wght", 100.0, 400.0, 900.0)]
        );
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
        assert!(face.has_color_bitmaps());
        let g = face.glyph_for('A');
        assert_eq!(g, 1);
        let bitmap = face.color_bitmap(g, 40.0).unwrap();
        assert_eq!(
            (
                bitmap.ppem,
                bitmap.width,
                bitmap.height,
                bitmap.bearing_x,
                bitmap.bearing_y
            ),
            (20, 2, 2, 0, 2)
        );
        assert!(face.color_bitmap(0, 40.0).is_none());
        let sans = Face::parse(&fs::read("assets/fonts/DejaVuSans.ttf").unwrap(), 0).unwrap();
        assert!(!sans.has_color_bitmaps())
    }
}
