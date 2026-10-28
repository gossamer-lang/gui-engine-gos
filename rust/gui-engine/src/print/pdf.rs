//! PDF 1.7 output of printed pages. Text is real text: shaped glyphs of an
//! embeddable face are shown through an embedded Type 0 font with a
//! ToUnicode map, so the PDF can be searched and copied. Faces that cannot be
//! embedded (collection members, variable instances) are drawn as outlines.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use super::Page;
use crate::render::display::DrawOp;
use crate::style::color::Color;
use crate::text::font::{CLOSE, CUBIC, LINE, MOVE, QUAD};
use crate::text::fonts::FaceId;
use crate::text::layout::{self, LayoutOptions};

const OUTPUT_LIMIT: usize = 1024 * 1024 * 1024;

fn num(value: f64) -> String {
    if !value.is_finite() {
        return "0".into();
    }
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.to_string()
    }
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(bytes, 6)
}

/// A PDF text string: UTF-16BE with its byte order mark, in hex.
fn text_string(text: &str) -> String {
    let mut out = String::from("<FEFF");
    for unit in text.encode_utf16() {
        let _ = write!(out, "{unit:04X}");
    }
    out.push('>');
    out
}

fn name(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(60)
        .collect();
    if clean.is_empty() {
        "Font".into()
    } else {
        clean
    }
}

struct Font {
    object: usize,
    resource: String,
    /// Glyph id to the text it stands for.
    glyphs: BTreeMap<i64, String>,
}

struct Writer {
    out: Vec<u8>,
    offsets: Vec<usize>,
    fonts: HashMap<FaceId, Font>,
    images: HashMap<u64, (usize, String)>,
    states: BTreeMap<u8, String>,
}

impl Writer {
    fn reserve(&mut self) -> usize {
        self.offsets.push(0);
        self.offsets.len()
    }

    fn object(&mut self, id: usize, body: &str) {
        self.offsets[id - 1] = self.out.len();
        self.out
            .extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    fn stream(&mut self, id: usize, dict: &str, data: &[u8]) {
        let compressed = deflate(data);
        self.offsets[id - 1] = self.out.len();
        self.out.extend_from_slice(
            format!(
                "{id} 0 obj\n<< {dict} /Filter /FlateDecode /Length {} >>\nstream\n",
                compressed.len()
            )
            .as_bytes(),
        );
        self.out.extend_from_slice(&compressed);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
    }

    fn state(&mut self, alpha: u8) -> String {
        let count = self.states.len();
        self.states
            .entry(alpha)
            .or_insert_with(|| format!("GS{count}"))
            .clone()
    }

    fn font(&mut self, face: FaceId) -> String {
        if let Some(font) = self.fonts.get(&face) {
            return font.resource.clone();
        }
        let object = self.reserve();
        let resource = format!("F{}", self.fonts.len());
        self.fonts.insert(
            face,
            Font {
                object,
                resource: resource.clone(),
                glyphs: BTreeMap::new(),
            },
        );
        resource
    }

    fn image(&mut self, rgba: &[u8], width: i64, height: i64) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (rgba, width, height).hash(&mut hasher);
        let key = hasher.finish();
        if let Some((_, resource)) = self.images.get(&key) {
            return resource.clone();
        }
        let id = self.reserve();
        let resource = format!("Im{}", self.images.len());
        let pixels = (width * height) as usize;
        let mut rgb = Vec::with_capacity(pixels * 3);
        let mut alpha = Vec::with_capacity(pixels);
        for px in rgba.as_chunks::<4>().0 {
            rgb.extend_from_slice(&px[..3]);
            alpha.push(px[3]);
        }
        let mask = if alpha.iter().any(|a| *a != 255) {
            let mask = self.reserve();
            self.stream(
                mask,
                &format!(
                    "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
                     /ColorSpace /DeviceGray /BitsPerComponent 8"
                ),
                &alpha,
            );
            format!(" /SMask {mask} 0 R")
        } else {
            String::new()
        };
        self.stream(
            id,
            &format!(
                "/Type /XObject /Subtype /Image /Width {width} /Height {height} \
                 /ColorSpace /DeviceRGB /BitsPerComponent 8{mask}"
            ),
            &rgb,
        );
        self.images.insert(key, (id, resource.clone()));
        resource
    }
}

struct Painter<'a> {
    writer: &'a mut Writer,
    content: String,
    /// The product of enclosing opacities, 0..=1.
    opacity: Vec<f64>,
}

impl Painter<'_> {
    fn paint(&mut self, color: Color, stroke: bool, body: &str) {
        let alpha =
            (f64::from(color.a) * self.opacity.last().copied().unwrap_or(1.0)).round() as u8;
        if alpha == 0 {
            return;
        }
        let (r, g, b) = (
            f64::from(color.r) / 255.0,
            f64::from(color.g) / 255.0,
            f64::from(color.b) / 255.0,
        );
        let operator = if stroke { "RG" } else { "rg" };
        self.content.push_str("q ");
        if alpha != 255 {
            let state = self.writer.state(alpha);
            let _ = write!(self.content, "/{state} gs ");
        }
        let _ = writeln!(
            self.content,
            "{} {} {} {operator} {body} Q",
            num(r),
            num(g),
            num(b)
        );
    }

    fn rect_path(x: f64, y: f64, w: f64, h: f64, radius: f64) -> String {
        let r = radius.clamp(0.0, w.min(h) / 2.0);
        if r <= 0.0 {
            return format!("{} {} {} {} re", num(x), num(y), num(w), num(h));
        }
        // Each quarter circle is a cubic with handles 0.5523 of the radius.
        let k = r * 0.552_284_75;
        let (x1, y1) = (x + w, y + h);
        format!(
            "{} {} m {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c h",
            num(x + r),
            num(y),
            num(x1 - r),
            num(y),
            num(x1 - r + k),
            num(y),
            num(x1),
            num(y + r - k),
            num(x1),
            num(y + r),
            num(x1),
            num(y1 - r),
            num(x1),
            num(y1 - r + k),
            num(x1 - r + k),
            num(y1),
            num(x1 - r),
            num(y1),
            num(x + r),
            num(y1),
            num(x + r - k),
            num(y1),
            num(x),
            num(y1 - r + k),
            num(x),
            num(y1 - r),
            num(x),
            num(y + r),
            num(x),
            num(y + r - k),
            num(x + r - k),
            num(y),
            num(x + r),
            num(y),
        )
    }

    fn path(cmds: &[f64], map: impl Fn(f64, f64) -> (f64, f64)) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < cmds.len() {
            let op = cmds[i];
            let arity = match op {
                MOVE | LINE => 2,
                QUAD => 4,
                CUBIC => 6,
                _ => 0,
            };
            if arity > 0 && i + arity >= cmds.len() {
                break;
            }
            let point = |k: usize| map(cmds[i + 1 + 2 * k], cmds[i + 2 + 2 * k]);
            match op {
                MOVE => {
                    let (x, y) = point(0);
                    let _ = write!(out, "{} {} m ", num(x), num(y));
                }
                LINE => {
                    let (x, y) = point(0);
                    let _ = write!(out, "{} {} l ", num(x), num(y));
                }
                QUAD => {
                    // A quadratic is a cubic with its control point raised.
                    let (cx, cy) = point(0);
                    let (x, y) = point(1);
                    let _ = write!(
                        out,
                        "{} {} {} {} {} {} v ",
                        num(cx),
                        num(cy),
                        num(cx),
                        num(cy),
                        num(x),
                        num(y)
                    );
                }
                CUBIC => {
                    let (ax, ay) = point(0);
                    let (bx, by) = point(1);
                    let (x, y) = point(2);
                    let _ = write!(
                        out,
                        "{} {} {} {} {} {} c ",
                        num(ax),
                        num(ay),
                        num(bx),
                        num(by),
                        num(x),
                        num(y)
                    );
                }
                CLOSE => out.push_str("h "),
                _ => {}
            }
            i += 1 + arity;
        }
        out
    }

    fn text(&mut self, x: f64, y: f64, text: &str, style: &layout::TextStyle, color: Color) {
        let laid = layout::layout(text, style, LayoutOptions::single_line());
        for line in &laid.lines {
            let baseline = y + line.baseline;
            let mut shown = String::new();
            for glyph in &line.glyphs {
                let gx = x + line.left + glyph.x;
                let gy = baseline + glyph.y;
                let face = crate::text::fonts::face(glyph.face);
                if face.program().is_some() {
                    let resource = self.writer.font(glyph.face);
                    let source = laid
                        .text
                        .get(glyph.start as usize..glyph.end as usize)
                        .unwrap_or("")
                        .to_string();
                    if let Some(font) = self.writer.fonts.get_mut(&glyph.face) {
                        font.glyphs.entry(glyph.gid).or_insert(source);
                    }
                    let _ = writeln!(
                        shown,
                        "/{resource} {} Tf 1 0 0 -1 {} {} Tm <{:04X}> Tj",
                        num(style.size),
                        num(gx),
                        num(gy),
                        glyph.gid
                    );
                } else {
                    let outline = face.outline(glyph.gid);
                    let scale = style.size / face.units_per_em.max(1) as f64;
                    let path =
                        Self::path(&outline.path, |ux, uy| (gx + ux * scale, gy - uy * scale));
                    if !path.is_empty() {
                        self.paint(color, false, &format!("{path}f"));
                    }
                }
            }
            if !shown.is_empty() {
                self.paint(color, false, &format!("BT\n{shown}ET"));
            }
        }
    }

    fn ops(&mut self, ops: &[DrawOp]) {
        for op in ops {
            match op {
                DrawOp::FillRect { rect, color } => {
                    let path = Self::rect_path(
                        rect.x as f64,
                        rect.y as f64,
                        rect.width as f64,
                        rect.height as f64,
                        0.0,
                    );
                    self.paint(*color, false, &format!("{path} f"));
                }
                DrawOp::RoundRect {
                    rect,
                    radius,
                    color,
                } => {
                    let path = Self::rect_path(
                        rect.x as f64,
                        rect.y as f64,
                        rect.width as f64,
                        rect.height as f64,
                        *radius as f64,
                    );
                    self.paint(*color, false, &format!("{path} f"));
                }
                DrawOp::Stroke {
                    rect,
                    radius,
                    width,
                    color,
                } => {
                    let half = *width as f64 / 2.0;
                    let path = Self::rect_path(
                        rect.x as f64 + half,
                        rect.y as f64 + half,
                        rect.width as f64 - 2.0 * half,
                        rect.height as f64 - 2.0 * half,
                        *radius as f64,
                    );
                    self.paint(*color, true, &format!("{} w {path} S", num(*width as f64)));
                }
                DrawOp::Line {
                    x0,
                    y0,
                    x1,
                    y1,
                    width,
                    color,
                } => {
                    self.paint(
                        *color,
                        true,
                        &format!("{} w 1 J {x0} {y0} m {x1} {y1} l S", num(*width)),
                    );
                }
                DrawOp::Circle { cx, cy, r, color } => {
                    let (x, y, d) = ((cx - r) as f64, (cy - r) as f64, (2 * r) as f64);
                    let path = Self::rect_path(x, y, d, d, *r as f64);
                    self.paint(*color, false, &format!("{path} f"));
                }
                DrawOp::Fill { path, color } => {
                    let body = Self::path(&path.cmds, |x, y| (x, y));
                    self.paint(*color, false, &format!("{body}f"));
                }
                DrawOp::StrokePath { path, width, color } => {
                    let body = Self::path(&path.cmds, |x, y| (x, y));
                    self.paint(*color, true, &format!("{} w 1 J 1 j {body}S", num(*width)));
                }
                DrawOp::Text {
                    x,
                    y,
                    text,
                    style,
                    color,
                } => {
                    self.text(*x as f64, *y as f64, text, style, *color);
                }
                DrawOp::Image {
                    rect,
                    rgba,
                    width,
                    height,
                } => {
                    if *width <= 0 || *height <= 0 || rgba.len() != (width * height * 4) as usize {
                        continue;
                    }
                    let resource = self.writer.image(rgba, *width, *height);
                    let alpha = (255.0 * self.opacity.last().copied().unwrap_or(1.0)).round() as u8;
                    self.content.push_str("q ");
                    if alpha != 255 {
                        let state = self.writer.state(alpha);
                        let _ = write!(self.content, "/{state} gs ");
                    }
                    let _ = writeln!(
                        self.content,
                        "{} 0 0 {} {} {} cm /{resource} Do Q",
                        rect.width,
                        -rect.height,
                        rect.x,
                        rect.y + rect.height
                    );
                }
                DrawOp::Clip(rect) => {
                    let _ = writeln!(
                        self.content,
                        "q {} {} {} {} re W n",
                        rect.x, rect.y, rect.width, rect.height
                    );
                }
                DrawOp::Unclip => self.content.push_str("Q\n"),
                DrawOp::Opacity(alpha) => {
                    let outer = self.opacity.last().copied().unwrap_or(1.0);
                    self.opacity
                        .push(outer * (*alpha).clamp(0, 255) as f64 / 255.0);
                }
                DrawOp::EndOpacity => {
                    if self.opacity.len() > 1 {
                        self.opacity.pop();
                    }
                }
                DrawOp::Layer { ops, transform } => {
                    let _ = writeln!(
                        self.content,
                        "q {} 0 0 {} {} {} cm",
                        num(transform.scale_x),
                        num(transform.scale_y),
                        num(transform.x),
                        num(transform.y)
                    );
                    self.ops(ops);
                    self.content.push_str("Q\n");
                }
            }
        }
    }
}

/// The PDF holding `pages`, titled `title`; stops when `cancelled` answers
/// true.
pub fn write(pages: &[Page], title: &str, cancelled: &dyn Fn() -> bool) -> Result<Vec<u8>, String> {
    if pages.is_empty() {
        return Err("a PDF needs at least one page".into());
    }
    let mut writer = Writer {
        out: b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec(),
        offsets: Vec::new(),
        fonts: HashMap::new(),
        images: HashMap::new(),
        states: BTreeMap::new(),
    };
    let catalog = writer.reserve();
    let tree = writer.reserve();
    let resources = writer.reserve();
    let info = writer.reserve();
    let mut kids = Vec::new();
    for page in pages {
        if cancelled() {
            return Err("PDF output cancelled".into());
        }
        let mut painter = Painter {
            writer: &mut writer,
            content: format!("1 0 0 -1 0 {} cm\n", num(page.height)),
            opacity: vec![1.0],
        };
        painter.ops(&page.ops);
        let content = painter.content;
        let stream = writer.reserve();
        writer.stream(stream, "", content.as_bytes());
        let id = writer.reserve();
        writer.object(
            id,
            &format!(
                "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 {} {}] /Resources {resources} 0 R /Contents {stream} 0 R >>",
                num(page.width),
                num(page.height)
            ),
        );
        kids.push(format!("{id} 0 R"));
        if writer.out.len() > OUTPUT_LIMIT {
            return Err("the PDF exceeds its byte budget".into());
        }
    }
    write_fonts(&mut writer);
    let fonts: String = writer
        .fonts
        .values()
        .map(|f| format!("/{} {} 0 R ", f.resource, f.object))
        .collect();
    let images: String = writer
        .images
        .values()
        .map(|(id, resource)| format!("/{resource} {id} 0 R "))
        .collect();
    let states: String = writer
        .states
        .iter()
        .map(|(alpha, name)| {
            let a = num(f64::from(*alpha) / 255.0);
            format!("/{name} << /Type /ExtGState /ca {a} /CA {a} >> ")
        })
        .collect();
    writer.object(
        resources,
        &format!("<< /Font << {fonts}>> /XObject << {images}>> /ExtGState << {states}>> >>"),
    );
    writer.object(
        tree,
        &format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            kids.len()
        ),
    );
    writer.object(catalog, &format!("<< /Type /Catalog /Pages {tree} 0 R >>"));
    writer.object(
        info,
        &format!(
            "<< /Title {} /Producer {} >>",
            text_string(title),
            text_string("gui-engine")
        ),
    );
    let start = writer.out.len();
    let mut xref = format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        writer.offsets.len() + 1
    );
    for offset in &writer.offsets {
        let _ = writeln!(xref, "{offset:010} 00000 n ");
    }
    let _ = write!(
        xref,
        "trailer\n<< /Size {} /Root {catalog} 0 R /Info {info} 0 R >>\nstartxref\n{start}\n%%EOF\n",
        writer.offsets.len() + 1
    );
    writer.out.extend_from_slice(xref.as_bytes());
    Ok(writer.out)
}

fn write_fonts(writer: &mut Writer) {
    let faces: Vec<FaceId> = writer.fonts.keys().copied().collect();
    for id in faces {
        let face = crate::text::fonts::face(id);
        let Some((program, cff)) = face.program() else {
            continue;
        };
        let glyphs = writer.fonts[&id].glyphs.clone();
        let object = writer.fonts[&id].object;
        let base = format!("{}{}", name(&face.family), name(&face.subfamily));
        let upm = face.units_per_em.max(1) as f64;
        let scale = |v: i64| (v as f64 * 1000.0 / upm).round() as i64;
        let file = writer.reserve();
        let dict = if cff {
            "/Subtype /OpenType".to_string()
        } else {
            format!("/Length1 {}", program.len())
        };
        writer.stream(file, &dict, program);
        let descriptor = writer.reserve();
        writer.object(
            descriptor,
            &format!(
                "<< /Type /FontDescriptor /FontName /{base} /Flags 32 /FontBBox [0 {} 1000 {}] \
                 /ItalicAngle {} /Ascent {} /Descent {} /CapHeight {} /StemV 80 /{} {file} 0 R >>",
                scale(-face.descent.abs()),
                scale(face.ascent),
                if face.italic { -12 } else { 0 },
                scale(face.ascent),
                scale(-face.descent.abs()),
                scale(face.ascent),
                if cff { "FontFile3" } else { "FontFile2" },
            ),
        );
        let widths: String = glyphs
            .keys()
            .map(|gid| format!("{gid} [{}] ", scale(face.h_metrics(*gid).0)))
            .collect();
        let descendant = writer.reserve();
        let subtype = if cff { "CIDFontType0" } else { "CIDFontType2" };
        let map = if cff { "" } else { " /CIDToGIDMap /Identity" };
        writer.object(
            descendant,
            &format!(
                "<< /Type /Font /Subtype /{subtype} /BaseFont /{base} \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor {descriptor} 0 R /DW 1000 /W [{widths}]{map} >>"
            ),
        );
        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
             /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
             /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
             1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        let mapped: Vec<(&i64, &String)> =
            glyphs.iter().filter(|(_, text)| !text.is_empty()).collect();
        for chunk in mapped.chunks(100) {
            let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
            for (gid, text) in chunk {
                let units: String = text.encode_utf16().map(|u| format!("{u:04X}")).collect();
                let _ = writeln!(cmap, "<{gid:04X}> <{units}>");
            }
            cmap.push_str("endbfchar\n");
        }
        cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
        let unicode = writer.reserve();
        writer.stream(unicode, "", cmap.as_bytes());
        writer.object(
            object,
            &format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /{base} /Encoding /Identity-H \
                 /DescendantFonts [{descendant} 0 R] /ToUnicode {unicode} 0 R >>"
            ),
        );
    }
}
