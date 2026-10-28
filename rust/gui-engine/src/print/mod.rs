//! Printing: shared pagination of headings, paragraphs, images, drawings and
//! rich documents into pages of drawing operations in points; previews drawn
//! by the renderer; PDF export; and submission to the system's printers.
//!
//! The same pages feed the preview, the PDF and the printer, so what a
//! preview shows is what prints.

pub mod api;
pub mod document;
pub mod job;
pub mod native;
pub mod pdf;

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;
use crate::render::display::DrawOp;
use crate::render::retained::Transform;
use crate::style::color::Color;
use crate::text::layout::{self, LayoutOptions, TextStyle};

const PAGE_LIMIT: usize = 10_000;
const TEXT_LIMIT: usize = 64 * 1024 * 1024;
const IMAGE_BYTES: usize = 256 * 1024 * 1024;
const PREVIEW_PIXELS: i64 = 4096;

/// A sheet size in points (1/72 inch), portrait.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Paper {
    A3,
    A4,
    A5,
    Letter,
    Legal,
    Tabloid,
    Custom { width: f64, height: f64 },
}

impl Paper {
    /// Portrait width and height in points.
    pub fn points(&self) -> (f64, f64) {
        match self {
            Paper::A3 => (841.89, 1190.55),
            Paper::A4 => (595.28, 841.89),
            Paper::A5 => (419.53, 595.28),
            Paper::Letter => (612.0, 792.0),
            Paper::Legal => (612.0, 1008.0),
            Paper::Tabloid => (792.0, 1224.0),
            Paper::Custom { width, height } => (*width, *height),
        }
    }

    /// The name printers know the size by.
    pub fn media(&self) -> String {
        match self {
            Paper::A3 => "A3".into(),
            Paper::A4 => "A4".into(),
            Paper::A5 => "A5".into(),
            Paper::Letter => "Letter".into(),
            Paper::Legal => "Legal".into(),
            Paper::Tabloid => "Tabloid".into(),
            Paper::Custom { width, height } => format!("Custom.{width:.0}x{height:.0}"),
        }
    }
}

/// Space left blank around the printed area, in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Margins {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// How pages are laid out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageSetup {
    pub paper: Paper,
    pub landscape: bool,
    pub margins: Margins,
    /// The body text size in points; headings and code scale from it.
    pub font_size: f64,
    /// Text centered in the top margin; `{page}` and `{pages}` number it.
    pub header: String,
    /// Text centered in the bottom margin.
    pub footer: String,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            paper: Paper::A4,
            landscape: false,
            margins: Margins {
                top: 54.0,
                right: 54.0,
                bottom: 54.0,
                left: 54.0,
            },
            font_size: 11.0,
            header: String::new(),
            footer: "Page {page} of {pages}".into(),
        }
    }
}

impl PageSetup {
    /// The sheet's width and height in points, as oriented.
    pub fn size(&self) -> (f64, f64) {
        let (w, h) = self.paper.points();
        if self.landscape { (h, w) } else { (w, h) }
    }

    fn check(&self) -> Result<(), String> {
        let (w, h) = self.size();
        let m = &self.margins;
        let fine = |v: f64| v.is_finite() && v >= 0.0;
        if !(72.0..=14_400.0).contains(&w) || !(72.0..=14_400.0).contains(&h) {
            return Err("the paper size is outside 1 to 200 inches".into());
        }
        if ![m.top, m.right, m.bottom, m.left].into_iter().all(fine)
            || m.left + m.right > w - 36.0
            || m.top + m.bottom > h - 36.0
        {
            return Err("the margins leave no printable area".into());
        }
        if !(4.0..=96.0).contains(&self.font_size) {
            return Err("the font size must be 4 to 96 points".into());
        }
        Ok(())
    }
}

/// One piece of a printable document, laid out top to bottom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Item {
    Heading {
        text: String,
        level: i64,
    },
    Paragraph {
        text: String,
    },
    /// Monospace text whose lines are kept as written.
    Code {
        text: String,
    },
    Quote {
        text: String,
    },
    /// A list entry; `marker` is its bullet or number.
    ListItem {
        text: String,
        marker: String,
        depth: i64,
    },
    /// A PNG file, at `width` points wide (0 fits its natural size to the
    /// page).
    Image {
        path: String,
        width: f64,
        alt: String,
    },
    /// Pixels the application already holds.
    Pixels {
        rgba: Vec<u8>,
        width: i64,
        height: i64,
        alt: String,
    },
    /// Drawing operations in a `width` x `height` box, scaled to fit the
    /// printable width: charts, scenes and other pictures.
    Drawing {
        ops: Vec<DrawOp>,
        width: f64,
        height: f64,
    },
    /// A rich document's content, by handle.
    Rich(i64),
    Rule,
    Spacer(f64),
    PageBreak,
}

/// One printed sheet: drawing operations in points from its top-left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub width: f64,
    pub height: f64,
    pub ops: Vec<DrawOp>,
}

struct Flow<'a> {
    setup: &'a PageSetup,
    pages: Vec<Page>,
    ops: Vec<DrawOp>,
    y: f64,
    width: f64,
    height: f64,
    left: f64,
    top: f64,
    bottom: f64,
    image_bytes: usize,
}

impl Flow<'_> {
    fn content_width(&self) -> f64 {
        self.width - self.setup.margins.left - self.setup.margins.right
    }

    fn break_page(&mut self) -> Result<(), String> {
        if self.pages.len() + 1 >= PAGE_LIMIT {
            return Err(format!("the document exceeds {PAGE_LIMIT} pages"));
        }
        self.pages.push(Page {
            width: self.width,
            height: self.height,
            ops: std::mem::take(&mut self.ops),
        });
        self.y = self.top;
        Ok(())
    }

    /// Starts a new page unless `height` more points fit on this one or the
    /// page is still empty.
    fn room(&mut self, height: f64) -> Result<(), String> {
        if self.y + height > self.bottom && self.y > self.top {
            self.break_page()?;
        }
        Ok(())
    }

    fn text(
        &mut self,
        text: &str,
        style: &TextStyle,
        indent: f64,
        after: f64,
    ) -> Result<(), String> {
        let width = (self.content_width() - indent).max(36.0);
        let laid = layout::layout(text, style, LayoutOptions::wrapped(width));
        if let Some(error) = laid.error {
            return Err(error);
        }
        for line in &laid.lines {
            self.room(line.height)?;
            let source = &laid.text[line.start as usize..line.end as usize];
            if !source.trim().is_empty() {
                self.ops.push(DrawOp::Text {
                    x: (self.left + indent + line.left).round() as i64,
                    y: self.y.round() as i64,
                    text: source.to_string(),
                    style: style.clone(),
                    color: Color::black(),
                });
            }
            self.y += line.height;
        }
        self.y += after;
        Ok(())
    }

    fn picture(
        &mut self,
        natural: (f64, f64),
        requested: f64,
        op: impl FnOnce(Rect) -> DrawOp,
    ) -> Result<(), String> {
        let (nw, nh) = natural;
        if nw <= 0.0 || nh <= 0.0 {
            return Ok(());
        }
        let available = self.bottom - self.top;
        let mut w = if requested > 0.0 { requested } else { nw };
        w = w.min(self.content_width());
        let mut h = w * nh / nw;
        if h > available {
            h = available;
            w = h * nw / nh;
        }
        self.room(h)?;
        let rect = Rect::new(
            self.left.round() as i64,
            self.y.round() as i64,
            w.round().max(1.0) as i64,
            h.round().max(1.0) as i64,
        );
        self.ops.push(op(rect));
        self.y += h + self.setup.font_size * 0.6;
        Ok(())
    }

    fn pixels(
        &mut self,
        rgba: Vec<u8>,
        width: i64,
        height: i64,
        requested: f64,
    ) -> Result<(), String> {
        if width <= 0 || height <= 0 || rgba.len() != (width * height * 4) as usize {
            return Err("image pixels do not match their size".into());
        }
        self.image_bytes = self.image_bytes.saturating_add(rgba.len());
        if self.image_bytes > IMAGE_BYTES {
            return Err("the document's images exceed their byte budget".into());
        }
        // Pixels print at 96 per inch unless a width is given.
        let natural = (width as f64 * 0.75, height as f64 * 0.75);
        self.picture(natural, requested, |rect| DrawOp::Image {
            rect,
            rgba,
            width,
            height,
        })
    }

    fn item(&mut self, item: &Item, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
        if cancelled() {
            return Err("printing cancelled".into());
        }
        let size = self.setup.font_size;
        let body = TextStyle::sans(size);
        match item {
            Item::Heading { text, level } => {
                let level = (*level).clamp(1, 6) as f64;
                let mut style = TextStyle::sans(size * (2.0 - 0.18 * (level - 1.0)).max(1.1));
                style.bold = true;
                self.y += size * 0.4;
                // A heading never ends a page alone.
                self.room(style.size * 1.4 + size * 2.4)?;
                self.text(text, &style, 0.0, size * 0.5)
            }
            Item::Paragraph { text } => self.text(text, &body, 0.0, size * 0.6),
            Item::Code { text } => {
                let mono = TextStyle::mono(size * 0.92);
                for line in text.lines() {
                    self.text(if line.is_empty() { " " } else { line }, &mono, size, 0.0)?;
                }
                self.y += size * 0.6;
                Ok(())
            }
            Item::Quote { text } => {
                let mut style = body.clone();
                style.italic = true;
                self.text(text, &style, size * 2.0, size * 0.6)
            }
            Item::ListItem {
                text,
                marker,
                depth,
            } => {
                let indent = size * 1.6 * (*depth).clamp(0, 8) as f64;
                self.room(size * 1.4)?;
                self.ops.push(DrawOp::Text {
                    x: (self.left + indent).round() as i64,
                    y: self.y.round() as i64,
                    text: marker.clone(),
                    style: body.clone(),
                    color: Color::black(),
                });
                self.text(text, &body, indent + size * 1.6, size * 0.3)
            }
            Item::Image { path, width, alt } => {
                let bytes = crate::services::bounded_io::read(
                    std::path::Path::new(path),
                    64 * 1024 * 1024,
                    &|| !cancelled(),
                )
                .map_err(|e| format!("image {path}: {e}"))?;
                match crate::render::png::decode_with_limit(&bytes, IMAGE_BYTES) {
                    Some(image) => self.pixels(image.rgba, image.width, image.height, *width),
                    None => self.text(&format!("[{alt}]"), &body, 0.0, size * 0.6),
                }
            }
            Item::Pixels {
                rgba,
                width,
                height,
                ..
            } => self.pixels(rgba.clone(), *width, *height, 0.0),
            Item::Drawing { ops, width, height } => {
                let ops = ops.clone();
                let (w, h) = (*width, *height);
                self.picture((w, h), 0.0, |rect| DrawOp::Layer {
                    transform: Transform {
                        x: rect.x as f64,
                        y: rect.y as f64,
                        scale_x: rect.width as f64 / w,
                        scale_y: rect.height as f64 / h,
                    },
                    ops,
                })
            }
            Item::Rich(id) => {
                for item in rich_items(*id)? {
                    self.item(&item, cancelled)?;
                }
                Ok(())
            }
            Item::Rule => {
                self.room(size)?;
                let y = (self.y + size * 0.5).round() as i64;
                let right = (self.left + self.content_width()).round() as i64;
                self.ops.push(DrawOp::Line {
                    x0: self.left.round() as i64,
                    y0: y,
                    x1: right,
                    y1: y,
                    width: 0.75,
                    color: Color::rgb(128, 128, 128),
                });
                self.y += size;
                Ok(())
            }
            Item::Spacer(points) => {
                self.y += points.clamp(0.0, self.bottom - self.top);
                Ok(())
            }
            Item::PageBreak => {
                if self.y > self.top {
                    self.break_page()?;
                }
                Ok(())
            }
        }
    }
}

/// A rich document's blocks as printable items.
fn rich_items(id: i64) -> Result<Vec<Item>, String> {
    use crate::rich::Kind;
    let snapshot = crate::rich::registry::snapshot(id)?;
    let mut items = Vec::new();
    fn walk(
        blocks: &[std::sync::Arc<crate::rich::Block>],
        depth: i64,
        number: Option<i64>,
        document: i64,
        items: &mut Vec<Item>,
    ) {
        let mut ordinal = number.unwrap_or(1);
        for block in blocks {
            let text = block.text.to_string();
            match &block.kind {
                Kind::Paragraph => {
                    for span in &block.spans {
                        if let Some(image) = &span.image
                            && let Some(asset) =
                                crate::rich::registry::image(document, &image.source)
                        {
                            items.push(Item::Pixels {
                                rgba: asset.rgba.clone(),
                                width: asset.width,
                                height: asset.height,
                                alt: image.alt.clone(),
                            });
                        }
                    }
                    if !text.trim().is_empty() {
                        items.push(Item::Paragraph { text })
                    }
                }
                Kind::Heading(level) => items.push(Item::Heading {
                    text,
                    level: *level,
                }),
                Kind::Code(_) => items.push(Item::Code { text }),
                Kind::Rule => items.push(Item::Rule),
                Kind::Quote => {
                    if !text.is_empty() {
                        items.push(Item::Quote { text });
                    }
                    walk(&block.children, depth, None, document, items);
                }
                Kind::List(start) => walk(
                    &block.children,
                    depth + 1,
                    start.or(Some(0)).filter(|_| start.is_some()),
                    document,
                    items,
                ),
                Kind::Item => {
                    let marker = match number {
                        Some(_) => {
                            let marker = format!("{ordinal}.");
                            ordinal += 1;
                            marker
                        }
                        None => "\u{2022}".to_string(),
                    };
                    let (first, rest) = block
                        .children
                        .split_first()
                        .map_or((String::new(), &[][..]), |(first, rest)| {
                            (first.text.to_string(), rest)
                        });
                    let text = if text.is_empty() { first } else { text };
                    items.push(Item::ListItem {
                        text,
                        marker,
                        depth: (depth - 1).max(0),
                    });
                    walk(rest, depth, None, document, items);
                }
            }
        }
    }
    walk(&snapshot.content.blocks, 0, None, id, &mut items);
    Ok(items)
}

/// Lays `items` out on pages, stopping when `cancelled` answers true.
pub fn paginate(
    items: &[Item],
    setup: &PageSetup,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<Page>, String> {
    setup.check()?;
    let text: usize = items
        .iter()
        .map(|item| match item {
            Item::Heading { text, .. }
            | Item::Paragraph { text }
            | Item::Code { text }
            | Item::Quote { text }
            | Item::ListItem { text, .. } => text.len(),
            _ => 0,
        })
        .sum();
    if text > TEXT_LIMIT {
        return Err("the document's text exceeds its byte budget".into());
    }
    let (width, height) = setup.size();
    let mut flow = Flow {
        setup,
        pages: Vec::new(),
        ops: Vec::new(),
        y: setup.margins.top,
        width,
        height,
        left: setup.margins.left,
        top: setup.margins.top,
        bottom: height - setup.margins.bottom,
        image_bytes: 0,
    };
    for item in items {
        flow.item(item, cancelled)?;
    }
    if !flow.ops.is_empty() || flow.pages.is_empty() {
        flow.break_page()?;
    }
    let mut pages = flow.pages;
    let count = pages.len();
    for (i, page) in pages.iter_mut().enumerate() {
        let fill = |template: &str| {
            template
                .replace("{page}", &(i + 1).to_string())
                .replace("{pages}", &count.to_string())
        };
        let small = TextStyle::sans(setup.font_size * 0.8);
        let place = |text: String, y: f64, ops: &mut Vec<DrawOp>| {
            if text.is_empty() {
                return;
            }
            let laid = layout::layout(&text, &small, LayoutOptions::single_line());
            ops.push(DrawOp::Text {
                x: ((width - laid.width) / 2.0).round() as i64,
                y: y.round() as i64,
                text,
                style: small.clone(),
                color: Color::rgb(80, 80, 80),
            });
        };
        let line = setup.font_size * 1.4;
        place(
            fill(&setup.header),
            (setup.margins.top - line) / 2.0,
            &mut page.ops,
        );
        place(
            fill(&setup.footer),
            height - setup.margins.bottom + (setup.margins.bottom - line) / 2.0,
            &mut page.ops,
        );
    }
    Ok(pages)
}

/// Page `page` drawn at `scale` pixels per point on white paper.
pub fn preview(page: &Page, scale: f64) -> Result<crate::render::png::Image, String> {
    if !(0.05..=8.0).contains(&scale) {
        return Err("the preview scale must be 0.05 to 8".into());
    }
    let width = (page.width * scale).ceil() as i64;
    let height = (page.height * scale).ceil() as i64;
    if width > PREVIEW_PIXELS || height > PREVIEW_PIXELS {
        return Err("the preview exceeds its pixel size".into());
    }
    let mut canvas = crate::render::Canvas::try_new(width, height)?;
    canvas.fill(Color::white());
    let transform = Transform {
        x: 0.0,
        y: 0.0,
        scale_x: scale,
        scale_y: scale,
    };
    crate::render::display::draw_transformed(&mut canvas, &page.ops, transform);
    Ok(crate::render::png::Image {
        width,
        height,
        rgba: canvas.pixels,
    })
}

#[cfg(test)]
mod tests;
