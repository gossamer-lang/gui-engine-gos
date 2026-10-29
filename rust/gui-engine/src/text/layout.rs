// layout: shaped, wrapped, bidirectional text with caret and selection
// geometry. Every widget that shows or edits text goes through
// `TextLayout`, so menus, fields, editors, and the semantic tree agree on
// offsets: all offsets here are UTF-8 byte offsets into the source text,
// and carets stop only at extended grapheme cluster boundaries.

use crate::memory::{ByteCache, SharedCache};
use crate::strext::StrExt;
use crate::unicode;
use parking_lot::Mutex;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::geometry::Rect;
use crate::render;
use crate::render::Canvas;
use crate::render::vector::{FillRule, Mask};
use crate::style::color::Color;
use crate::text::bidi;
use crate::text::font::{ColorGlyph, Face};
use crate::text::fonts;
use crate::text::fonts::{FaceId, Family};
use crate::text::indic;
use crate::text::linebreak;
use crate::text::opentype;
use crate::text::opentype::GlyphInfo;
use crate::text::ucd::{JOINING, SCRIPT, SCRIPT_CODES, SCRIPT_NAMES};

thread_local! { static WORK_CONTEXT: std::cell::RefCell<Option<crate::services::jobs::Context>> = const { std::cell::RefCell::new(None) }; }
pub(crate) fn work_active() -> bool {
    WORK_CONTEXT.with_borrow(|context| context.as_ref().is_none_or(|context| context.active()))
}
pub(crate) fn document_paragraph(
    text: &str,
    style: &TextStyle,
    options: LayoutOptions,
    context: &crate::services::jobs::Context,
) -> Result<TextLayout, String> {
    if text.len() > 2 * 1024 * 1024 {
        return Err("document paragraph exceeds its layout byte budget".into());
    }
    context.check()?;
    let old = WORK_CONTEXT.with_borrow_mut(|current| current.replace(context.clone()));
    let _restore = scopeguard::guard(old, |old| {
        WORK_CONTEXT.with_borrow_mut(|current| *current = old)
    });
    let result = build(text, style, options);
    context.check()?;
    Ok(result)
}

/// How text is set: family, pixel size, and weight/slant.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextStyle {
    pub family: Family,
    /// Pixel size (the em square), already scaled for the display.
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
    /// Spaces per tab stop.
    pub tab_width: i64,
    /// Line height as a multiple of the font's natural height; 0 keeps it.
    pub line_spacing: f64,
}

impl TextStyle {
    /// Regular sans text at `size` pixels.
    pub fn sans(size: f64) -> TextStyle {
        TextStyle {
            family: Family::Sans,
            size,
            bold: false,
            italic: false,
            tab_width: 4,
            line_spacing: 0.0,
        }
    }

    /// Monospace text at `size` pixels.
    pub fn mono(size: f64) -> TextStyle {
        TextStyle {
            family: Family::Monospace,
            size,
            bold: false,
            italic: false,
            tab_width: 4,
            line_spacing: 0.0,
        }
    }

    /// This style in bold.
    pub fn with_bold(&self, bold: bool) -> TextStyle {
        let mut s = self.clone();
        s.bold = bold;
        s
    }

    /// This style in italic (synthesised by slanting when the family has
    /// no italic face).
    pub fn with_italic(&self, italic: bool) -> TextStyle {
        let mut s = self.clone();
        s.italic = italic;
        s
    }

    pub(crate) fn key(&self) -> String {
        let fam = match self.family {
            Family::Sans => "s",
            Family::Monospace => "m",
            Family::Named(ref n) => n,
        };
        format!(
            "{}|{}|{}|{}|{}|{}",
            fam,
            (self.size * 64.0) as i64,
            self.bold,
            self.italic,
            self.tab_width,
            (self.line_spacing * 100.0) as i64
        )
    }
}

/// Where lines wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Wrap {
    /// Only at newlines.
    NoWrap,
    /// At line-break opportunities, splitting words only when one word
    /// alone is too wide.
    Word,
    /// At any grapheme boundary.
    Anywhere,
}

/// Horizontal placement of each line within the layout width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextAlign {
    /// The paragraph's leading edge (left for LTR, right for RTL).
    Start,
    End,
    Left,
    Right,
    Center,
}

/// Options for one layout.
#[derive(Debug, Clone, PartialEq, Copy, serde::Serialize, serde::Deserialize)]
pub struct LayoutOptions {
    /// Width lines wrap within; negative means unbounded.
    pub max_width: f64,
    pub wrap: Wrap,
    pub align: TextAlign,
    pub direction: bidi::Direction,
    /// Lines kept; extra text is elided with an ellipsis. 0 keeps all.
    pub max_lines: i64,
}

impl LayoutOptions {
    /// One unwrapped line, start-aligned, direction from the text.
    pub fn single_line() -> LayoutOptions {
        LayoutOptions {
            max_width: -1.0,
            wrap: Wrap::NoWrap,
            align: TextAlign::Start,
            direction: bidi::Direction::Auto,
            max_lines: 0,
        }
    }

    /// Word-wrapped within `width` pixels.
    pub fn wrapped(width: f64) -> LayoutOptions {
        LayoutOptions {
            max_width: width,
            wrap: Wrap::Word,
            align: TextAlign::Start,
            direction: bidi::Direction::Auto,
            max_lines: 0,
        }
    }

    /// One line within `width` pixels, ending in an ellipsis when cut.
    pub fn elided(width: f64) -> LayoutOptions {
        LayoutOptions {
            max_width: width,
            wrap: Wrap::Anywhere,
            align: TextAlign::Start,
            direction: bidi::Direction::Auto,
            max_lines: 1,
        }
    }

    pub(crate) fn key(&self) -> String {
        let wrap = match self.wrap {
            Wrap::NoWrap => 0,
            Wrap::Word => 1,
            Wrap::Anywhere => 2,
        };
        let align = match self.align {
            TextAlign::Start => 0,
            TextAlign::End => 1,
            TextAlign::Left => 2,
            TextAlign::Right => 3,
            TextAlign::Center => 4,
        };
        let dir = match self.direction {
            bidi::Direction::Auto => 0,
            bidi::Direction::Ltr => 1,
            bidi::Direction::Rtl => 2,
        };
        format!(
            "{}|{}|{}|{}|{}",
            (self.max_width * 16.0) as i64,
            wrap,
            align,
            dir,
            self.max_lines
        )
    }
}

/// One positioned glyph of a laid-out line, in visual order.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlacedGlyph {
    pub face: FaceId,
    pub gid: i64,
    /// Pen x from the line's left edge, and offset from the baseline (y down).
    pub x: f64,
    pub y: f64,
    pub advance: f64,
    /// The grapheme-aligned source bytes this glyph stands for.
    pub start: i64,
    pub end: i64,
    pub rtl: bool,
    /// Resolved bidi embedding level of this glyph on its visual line.
    pub level: i64,
}

impl Default for PlacedGlyph {
    fn default() -> PlacedGlyph {
        PlacedGlyph {
            face: 0.into(),
            gid: 0,
            x: 0.0,
            y: 0.0,
            advance: 0.0,
            start: 0,
            end: 0,
            rtl: false,
            level: 0,
        }
    }
}

/// One visual line.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Line {
    /// Source byte range, excluding the newline that ended it.
    pub start: i64,
    pub end: i64,
    pub top: f64,
    pub baseline: f64,
    pub height: f64,
    /// Ink advance width of the line's content.
    pub width: f64,
    /// Offset of the line's left edge from the layout's left, from alignment.
    pub left: f64,
    pub glyphs: Vec<PlacedGlyph>,
    pub rtl: bool,
}

impl Default for Line {
    fn default() -> Line {
        Line {
            start: 0,
            end: 0,
            top: 0.0,
            baseline: 0.0,
            height: 0.0,
            width: 0.0,
            left: 0.0,
            glyphs: vec![],
            rtl: false,
        }
    }
}

/// Text after shaping, wrapping, and bidi reordering.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextLayout {
    pub text: String,
    /// Native viewport layouts use absolute source offsets with bounded text.
    #[serde(default)]
    pub source_origin: i64,
    #[serde(default)]
    pub source_length: i64,
    pub lines: Vec<Line>,
    pub width: f64,
    pub height: f64,
    pub ascent: f64,
    pub descent: f64,
    pub line_height: f64,
    /// Grapheme boundaries (byte offsets), including 0 and the length.
    pub boundaries: Vec<i64>,
    /// Whether the text was cut short by `max_lines`.
    pub elided: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl Default for TextLayout {
    fn default() -> TextLayout {
        TextLayout {
            text: String::new(),
            source_origin: 0,
            source_length: 0,
            lines: vec![],
            width: 0.0,
            height: 0.0,
            ascent: 0.0,
            descent: 0.0,
            line_height: 0.0,
            boundaries: vec![],
            elided: false,
            error: None,
        }
    }
}

/// A cached layout of `text`; identical requests share one result.
pub const FULL_LAYOUT_SOURCE_LIMIT: usize = 512 * 1024;

pub fn layout(text: &str, style: &TextStyle, options: LayoutOptions) -> TextLayout {
    try_layout(text, style, options).unwrap_or_else(|error| TextLayout {
        error: Some(error),
        ..Default::default()
    })
}

fn check_layout(text: &str, style: &TextStyle, options: &LayoutOptions) -> Result<(), String> {
    if text.len() > FULL_LAYOUT_SOURCE_LIMIT {
        return Err("text exceeds the full-layout byte budget".into());
    }
    if !style.size.is_finite()
        || !(0.0..=1024.0).contains(&style.size)
        || !style.line_spacing.is_finite()
        || !(0.0..=10.0).contains(&style.line_spacing)
        || !options.max_width.is_finite()
    {
        return Err("text style exceeds layout limits".into());
    }
    Ok(())
}

/// A layout of `text` made for the caller alone, outside the shared cache: for
/// text that seldom repeats, which would only fill that cache, and which the
/// caller caches to fit its own working set.
pub fn layout_uncached(text: &str, style: &TextStyle, options: LayoutOptions) -> TextLayout {
    match check_layout(text, style, &options) {
        Ok(()) => build_styled(text, style, options, &[], false),
        Err(error) => TextLayout {
            error: Some(error),
            ..Default::default()
        },
    }
}

pub fn try_layout(
    text: &str,
    style: &TextStyle,
    options: LayoutOptions,
) -> Result<TextLayout, String> {
    check_layout(text, style, &options)?;
    let cache_key = format!("{}#{}#{}", style.key(), options.key(), text);
    if let Some(found) = CACHE.lock().get(&cache_key) {
        CACHE_HITS.fetch_add(1, Ordering::Relaxed);
        return Ok(found.clone());
    }
    CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    let made = build(text, style, options);
    let bytes = cache_key.capacity() + made.owned_bytes();
    CACHE.lock().insert(cache_key, made.clone(), bytes);
    Ok(made)
}

/// Styled text keeps one paragraph bidi context and splits shaping only at
/// actual font boundaries. Geometry and painting share exactly the same spans.
pub fn try_layout_rich(
    text: &str,
    style: &TextStyle,
    options: LayoutOptions,
    spans: &[crate::rich::Span],
) -> Result<TextLayout, String> {
    if text.len() > crate::rich::PARAGRAPH_LIMIT || spans.len() > crate::rich::SPAN_LIMIT {
        return Err("rich paragraph exceeds layout limits".into());
    }
    if !style.size.is_finite()
        || !(1.0..=1024.0).contains(&style.size)
        || !style.line_spacing.is_finite()
        || !(0.0..=10.0).contains(&style.line_spacing)
        || !options.max_width.is_finite()
        || options.max_lines > 0
    {
        return Err("invalid rich text layout options".into());
    }
    let boundaries = grapheme_boundaries(text);
    let mut end = 0;
    for span in spans {
        if span.start != end
            || span.start >= span.end
            || boundaries.binary_search(&(span.start as i64)).is_err()
            || boundaries.binary_search(&(span.end as i64)).is_err()
        {
            return Err("invalid rich layout span boundaries".into());
        }
        if let Some(image) = &span.image
            && (!(0..=4096).contains(&image.width)
                || !(0..=4096).contains(&image.height)
                || text.get(span.start..span.end) != Some("\u{fffc}"))
        {
            return Err("invalid inline image layout".into());
        }
        end = span.end;
    }
    if end != text.len() {
        return Err("rich layout spans must cover text".into());
    }
    Ok(build_styled(text, style, options, spans, true))
}
fn rich_span(spans: &[crate::rich::Span], byte: i64) -> Option<&crate::rich::Span> {
    let index = spans.partition_point(|span| span.end <= byte.max(0) as usize);
    spans
        .get(index)
        .filter(|span| span.start <= byte.max(0) as usize)
}
pub(crate) fn image_size(image: &crate::rich::Image) -> (f64, f64) {
    (
        if image.width > 0 {
            image.width as f64
        } else {
            128.0
        },
        if image.height > 0 {
            image.height as f64
        } else {
            96.0
        },
    )
}

pub(crate) fn rich_paragraph(
    text: &str,
    style: &TextStyle,
    options: LayoutOptions,
    spans: &[crate::rich::Span],
    context: &crate::services::jobs::Context,
) -> Result<TextLayout, String> {
    context.check()?;
    let old = WORK_CONTEXT.with_borrow_mut(|current| current.replace(context.clone()));
    let _restore = scopeguard::guard(old, |old| {
        WORK_CONTEXT.with_borrow_mut(|current| *current = old)
    });
    let result = try_layout_rich(text, style, options, spans)?;
    context.check()?;
    Ok(result)
}

/// The width and height `text` occupies on one unwrapped line.
pub fn measure(text: &str, style: &TextStyle) -> (f64, f64) {
    let l = layout(text, style, LayoutOptions::single_line());
    (l.width, l.height)
}

/// The vertical metrics of a style: (ascent, descent, line height).
pub fn metrics(style: &TextStyle) -> (f64, f64, f64) {
    let face = fonts::face(fonts::primary(&style.family, style.bold));
    let scale = style.size / face.units_per_em as f64;
    let ascent = face.ascent as f64 * scale;
    let descent = -face.descent as f64 * scale;
    let natural = ascent + descent + face.line_gap.max(0) as f64 * scale;
    let line_h = if style.line_spacing > 0.0 {
        natural * style.line_spacing
    } else {
        natural
    };
    (ascent, descent, line_h)
}

/// Full layouts and paragraphs have separate caps, shared across sessions.
const LAYOUT_BUDGET: usize = 16 * 1024 * 1024;
static CACHE: LazyLock<Mutex<ByteCache<String, TextLayout>>> =
    LazyLock::new(|| Mutex::new(ByteCache::new(LAYOUT_BUDGET)));
static CACHE_HITS: AtomicI64 = AtomicI64::new(0);
static CACHE_MISSES: AtomicI64 = AtomicI64::new(0);
static PARAGRAPHS: LazyLock<Mutex<ByteCache<String, Vec<Line>>>> =
    LazyLock::new(|| Mutex::new(ByteCache::new(PARAGRAPH_BUDGET)));
static PARAGRAPH_HITS: AtomicI64 = AtomicI64::new(0);
static PARAGRAPH_MISSES: AtomicI64 = AtomicI64::new(0);
const PARAGRAPH_BUDGET: usize = 8 * 1024 * 1024;

/// Paragraph cache entries, hits, misses and accounted storage bytes.
/// Accounting includes source keys and a conservative per-line/glyph allowance.
pub fn paragraph_cache_stats() -> (i64, i64, i64, i64) {
    let cache = PARAGRAPHS.lock();
    (
        cache.len() as i64,
        PARAGRAPH_HITS.load(Ordering::Relaxed),
        PARAGRAPH_MISSES.load(Ordering::Relaxed),
        cache.bytes() as i64,
    )
}

/// Layout cache counters: (entries, hits, misses, evictions).
pub fn cache_stats() -> (i64, i64, i64, i64) {
    let cache = CACHE.lock();
    (
        cache.len() as i64,
        CACHE_HITS.load(Ordering::Relaxed),
        CACHE_MISSES.load(Ordering::Relaxed),
        cache.evictions() as i64,
    )
}

/// Drops every cached layout and glyph, as a low-memory response.
pub fn purge_caches() {
    crate::rich::layout_cache::purge();
    crate::widgets::colorpicker::purge_cache();
    fonts::purge_fallback_cache();
    opentype::purge_cache();
    CACHE.lock().clear();
    PARAGRAPHS.lock().clear();
    GLYPHS.lock().clear();
    COLOR_GLYPHS.lock().clear();
}

/// Accounted bytes and limit for full layouts, paragraphs and both glyph caches.
/// The process-wide sum is capped regardless of the number of sessions.
pub fn cache_bytes() -> (usize, usize) {
    (
        CACHE.lock().bytes()
            + PARAGRAPHS.lock().bytes()
            + GLYPHS.lock().bytes()
            + COLOR_GLYPHS.lock().bytes(),
        LAYOUT_BUDGET + PARAGRAPH_BUDGET + GLYPH_BUDGET,
    )
}

impl TextLayout {
    pub(crate) fn owned_bytes(&self) -> usize {
        self.text.capacity()
            + self.boundaries.capacity() * size_of::<i64>()
            + lines_bytes(&self.lines)
    }
}

fn lines_bytes(lines: &Vec<Line>) -> usize {
    lines.capacity() * size_of::<Line>()
        + lines
            .iter()
            .map(|line| line.glyphs.capacity() * size_of::<PlacedGlyph>())
            .sum::<usize>()
}

// ---- building ----

/// A shaped run of one paragraph: uniform face, level, and script.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Run {
    start: i64,
    end: i64,
    level: i64,
    face: FaceId,
    glyphs: Vec<GlyphInfo>,
    glyph_index: Vec<usize>,
}

fn build(text: &str, style: &TextStyle, options: LayoutOptions) -> TextLayout {
    build_styled(text, style, options, &[], true)
}
/// Lays out `text`; `shared` keeps its unstyled paragraphs in the shared
/// paragraph cache for the next request.
fn build_styled(
    text: &str,
    style: &TextStyle,
    options: LayoutOptions,
    spans: &[crate::rich::Span],
    shared: bool,
) -> TextLayout {
    let (ascent, descent, line_h) = metrics(style);
    let mut out = TextLayout {
        text: text.to_string(),
        ascent,
        descent,
        line_height: line_h,
        ..TextLayout::default()
    };
    out.boundaries = grapheme_boundaries(text);
    let mut para_start = 0;
    let total = text.byte_len();
    let mut top = 0.0;
    let mut max_w = 0.0;
    for paragraph in text.split("\n") {
        if !work_active() {
            break;
        }
        let para_end = para_start + paragraph.byte_len();
        let next = para_end + 1;
        let local = spans
            .iter()
            .filter(|span| span.start < para_end as usize && span.end > para_start as usize)
            .map(|span| crate::rich::Span {
                start: span.start.max(para_start as usize) - para_start as usize,
                end: span.end.min(para_end as usize) - para_start as usize,
                ..span.clone()
            })
            .collect::<Vec<_>>();
        let lines = if spans.is_empty() && shared {
            cached_paragraph(paragraph, style, options)
        } else {
            layout_paragraph(
                paragraph,
                0,
                paragraph.byte_len(),
                style,
                options,
                &grapheme_boundaries(paragraph),
                &local,
            )
        };
        for mut line in lines {
            line.start += para_start;
            line.end += para_start;
            let mut g = 0;
            while g < line.glyphs.len() {
                line.glyphs[g].start += para_start;
                line.glyphs[g].end += para_start;
                g += 1;
            }
            let image_height = line
                .glyphs
                .iter()
                .filter_map(|glyph| {
                    rich_span(spans, glyph.start).and_then(|span| span.image.as_ref())
                })
                .map(|image| image_size(image).1)
                .fold(0.0, f64::max);
            let actual_ascent = ascent.max(image_height);
            let actual_height = if spans.is_empty() {
                line_h
            } else {
                line_h.max(actual_ascent + descent)
            };
            line.top = top;
            line.baseline =
                top + actual_ascent + (actual_height - actual_ascent - descent).max(0.0) / 2.0;
            line.height = actual_height;
            top += actual_height;
            if line.width > max_w {
                max_w = line.width
            };
            out.lines.push(line);
            if options.max_lines > 0 && out.lines.len() >= options.max_lines as usize {
                break;
            }
        }
        if options.max_lines > 0 && out.lines.len() >= options.max_lines as usize {
            if next <= total || out.lines[out.lines.len() - 1].end < para_end {
                out.elided = true;
            }
            break;
        }
        if next > total {
            break;
        }
        para_start = next;
    }
    if out.elided {
        let last = out.lines.len() - 1;
        out.lines[last] = elide(
            text,
            out.lines[last].clone(),
            style,
            options,
            &out.boundaries,
        );
        if out.lines[last].width > max_w {
            max_w = out.lines[last].width
        }
    }
    let box_w = if options.max_width >= 0.0 && options.wrap != Wrap::NoWrap {
        options.max_width
    } else {
        max_w
    };
    out.width = max_w;
    out.height = top.max(line_h);
    let mut i = 0;
    while i < out.lines.len() {
        let line = &out.lines[i];
        let slack = (box_w - line.width).max(0.0);
        let left = match options.align {
            TextAlign::Left => 0.0,
            TextAlign::Right => slack,
            TextAlign::Center => slack / 2.0,
            TextAlign::Start => {
                if line.rtl {
                    slack
                } else {
                    0.0
                }
            }
            TextAlign::End => {
                if line.rtl {
                    0.0
                } else {
                    slack
                }
            }
        };
        out.lines[i].left = left;
        i += 1;
    }
    out
}

/// Byte offsets of extended grapheme cluster boundaries, 0 and len included.
pub fn grapheme_boundaries(text: &str) -> Vec<i64> {
    let mut out = vec![0];
    let mut at = 0;
    for g in unicode::graphemes(text) {
        at += g.byte_len();
        out.push(at);
        if out.len() % 1024 == 0 && !work_active() {
            break;
        }
    }
    out
}

fn script_of(c: char) -> i64 {
    let v = c as i64;
    let mut lo = 0;
    let mut hi = SCRIPT.len() / 3;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if v < SCRIPT[mid * 3] {
            hi = mid;
        } else if v > SCRIPT[mid * 3 + 1] {
            lo = mid + 1;
        } else {
            return SCRIPT[mid * 3 + 2];
        }
    }
    0
}

// The OpenType script tag for a Unicode script name.
fn script_tag(index: i64) -> String {
    if index <= 1 || index >= SCRIPT_CODES.len() as i64 {
        return "DFLT".to_string();
    }
    // OpenType tags are ISO 15924 codes in lower case, except where the
    // registry names them otherwise; Indic and Myanmar take their v2 tags.
    let code = SCRIPT_CODES[index as usize].to_lowercase();
    let tag = match code.as_str() {
        "zinh" | "zyyy" | "zzzz" => "DFLT",
        "hira" | "kana" | "hrkt" => "kana",
        "laoo" => "lao ",
        "yiii" => "yi  ",
        "nkoo" => "nko ",
        "vaii" => "vai ",
        "deva" => "dev2",
        "beng" => "bng2",
        "guru" => "gur2",
        "gujr" => "gjr2",
        "orya" => "ory2",
        "taml" => "tml2",
        "telu" => "tel2",
        "knda" => "knd2",
        "mlym" => "mlm2",
        "mymr" => "mym2",
        _ => return code,
    };
    tag.to_string()
}

fn joining_type(c: char) -> i64 {
    let v = c as i64;
    let mut lo = 0;
    let mut hi = JOINING.len() / 3;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if v < JOINING[mid * 3] {
            hi = mid;
        } else if v > JOINING[mid * 3 + 1] {
            lo = mid + 1;
        } else {
            return JOINING[mid * 3 + 2];
        }
    }
    // Unlisted marks and format characters are transparent.
    if unicode::is_mark(c) || v == 0x200b || (0x200e..=0x200f).contains(&v) {
        5
    } else {
        0
    }
}

// Arabic positional forms (U=0 R=1 L=2 D=3 C=4 T=5) for one run's chars.
fn arabic_forms(chars: &[char]) -> Vec<String> {
    let n = chars.len();
    let types = chars.iter().map(|&c| joining_type(c)).collect::<Vec<_>>();
    let mut forms = vec![String::new(); n];
    let mut i = 0;
    while i < n {
        let t = types[i];
        if t == 5 || t == 0 {
            i += 1;
            continue;
        }
        // Previous and next non-transparent neighbours.
        let mut p = i as i64 - 1;
        while p >= 0 && types[p as usize] == 5 {
            p -= 1
        }
        let mut q = i + 1;
        while q < n && types[q] == 5 {
            q += 1
        }
        let joins_prev =
            p >= 0 && matches!(types[p as usize], 2..=4) && (t == 3 || t == 1 || t == 4);
        let joins_next = q < n
            && (types[q] == 3 || types[q] == 1 || types[q] == 4)
            && (t == 3 || t == 2 || t == 4);
        forms[i] = if joins_prev && joins_next {
            "medi"
        } else if joins_prev {
            "fina"
        } else if joins_next {
            "init"
        } else {
            "isol"
        }
        .to_string();
        i += 1;
    }
    forms
}

fn cached_paragraph(text: &str, style: &TextStyle, options: LayoutOptions) -> Vec<Line> {
    let key = format!("{}#{}#{}", style.key(), options.key(), text);
    if let Some(lines) = PARAGRAPHS.lock().get(&key) {
        PARAGRAPH_HITS.fetch_add(1, Ordering::Relaxed);
        return lines.to_vec();
    };
    PARAGRAPH_MISSES.fetch_add(1, Ordering::Relaxed);
    let lines = layout_paragraph(
        text,
        0,
        text.byte_len(),
        style,
        options,
        &grapheme_boundaries(text),
        &[],
    );
    let bytes = key.capacity() + lines_bytes(&lines);
    if work_active() && bytes <= 8 * 1024 * 1024 {
        PARAGRAPHS.lock().insert(key, lines.clone(), bytes);
    }
    lines
}

fn layout_paragraph(
    text: &str,
    start: i64,
    end: i64,
    style: &TextStyle,
    options: LayoutOptions,
    boundaries: &[i64],
    spans: &[crate::rich::Span],
) -> Vec<Line> {
    let para = text.substring(start, end);
    let mut chars: Vec<char> = vec![];
    let mut offsets: Vec<i64> = vec![];
    let mut at = start;
    let mut any_rtl = false;
    for c in para.chars() {
        chars.push(c);
        offsets.push(at);
        at += c.len_utf8() as i64;
        let k = bidi::class(c);
        if k == bidi::BIDI_R
            || k == bidi::BIDI_AL
            || k == bidi::BIDI_RLE
            || k == bidi::BIDI_RLO
            || k == bidi::BIDI_RLI
        {
            any_rtl = true;
        }
    }
    let n = chars.len();
    let mut levels = vec![0; n];
    let mut para_level = 0;
    if any_rtl || options.direction == bidi::Direction::Rtl {
        let (lv, pl) = bidi::levels(&chars, options.direction);
        levels = lv;
        para_level = pl;
    }
    if n == 0 {
        return vec![Line {
            start,
            end,
            rtl: para_level == 1,
            ..Line::default()
        }];
    }
    let primary = fonts::primary(&style.family, style.bold);
    let runs = itemize_and_shape(&chars, &offsets, &levels, end, primary, style, spans);
    // Advance of each source char (glyph advances assigned to clusters).
    let scale = style.size / fonts::face(primary).units_per_em as f64;
    let mut adv = vec![0.0; n];
    let index_of = |byte: i64| -> i64 { offset_index(&offsets, byte) };
    for run in &runs {
        let run_scale = style.size / fonts::face(run.face).units_per_em as f64;
        for g in &run.glyphs {
            let k = index_of(g.cluster);
            if k >= 0 {
                adv[k as usize] += g.x_advance as f64 * run_scale
            }
        }
    }
    let space_w = space_advance(primary) * scale;
    let tab_w = space_w * style.tab_width.max(1) as f64;
    // Tabs advance to the next stop from the line start (logical).
    // Break into lines.
    let mut lines = vec![];
    let wrap_width = if options.wrap == Wrap::NoWrap || options.max_width < 0.0 {
        -1.0
    } else {
        options.max_width
    };
    let mut opportunities = vec![];
    if wrap_width >= 0.0 {
        if options.wrap == Wrap::Word {
            for b in linebreak::breaks(para) {
                opportunities.push(start + b.offset)
            }
        } else {
            let first = boundary_index(boundaries, start, true);
            let last = boundary_index(boundaries, end, true);
            opportunities = boundaries[first as usize..last as usize].to_vec();
        }
    }
    let opportunities = opportunities
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let mut line_start = 0;
    while line_start < n || lines.is_empty() {
        if !work_active() {
            return Vec::new();
        }
        let mut k = line_start;
        let mut width = 0.0;
        let mut last_fit: i64 = -1;
        if wrap_width < 0.0 {
            k = n;
        } else {
            while k < n {
                let w = if chars[k] == '\t' {
                    tab_w - (width % tab_w)
                } else {
                    adv[k]
                };
                let byte_after = if k + 1 < n { offsets[k + 1] } else { end };
                let trailing_space = unicode::is_space(chars[k]);
                if width + w > wrap_width && !trailing_space && k > line_start {
                    break;
                }
                width += w;
                if opportunities.contains(&byte_after) {
                    last_fit = (k + 1) as i64;
                }
                k += 1;
            }
            if k < n {
                if last_fit > line_start as i64 {
                    k = last_fit as usize;
                } else {
                    // No opportunity fits: break at the last grapheme
                    // boundary that does, keeping at least one grapheme.
                    let mut g = k;
                    while g > line_start + 1 && offset_index(boundaries, offsets[g]) < 0 {
                        g -= 1
                    }
                    k = g.max(line_start + 1);
                    while k < n && offset_index(boundaries, offsets[k]) < 0 {
                        k += 1
                    }
                }
            }
        }
        let line_end_byte = if k < n { offsets[k] } else { end };
        let line = place_line(
            &chars,
            &offsets,
            &levels,
            &runs,
            line_start as i64,
            k as i64,
            offsets[line_start.min(n - 1)],
            line_end_byte,
            para_level,
            style,
            tab_w,
            boundaries,
        );
        lines.push(line);
        if k <= line_start {
            break;
        }
        line_start = k;
    }
    lines
}

fn offset_index(offsets: &[i64], byte: i64) -> i64 {
    offsets.binary_search(&byte).map_or(-1, |i| i as i64)
}

fn space_advance(face: FaceId) -> f64 {
    let f = fonts::face(face);
    let (adv, _) = f.h_metrics(f.glyph_for(' '));
    adv as f64
}

// Splits a paragraph into runs of uniform level, face, and script, then
// shapes each.
// Whether `c` belongs to the cluster before it for face selection.
fn joins_previous(c: char) -> bool {
    let v = c as i64;
    unicode::is_mark(c)
        || v == 0x200d
        || (0xfe00..=0xfe0f).contains(&v)
        || (0x1f3fb..=0x1f3ff).contains(&v)
        || (0xe0020..=0xe007f).contains(&v)
}

fn itemize_and_shape(
    chars: &[char],
    offsets: &[i64],
    levels: &[i64],
    end: i64,
    primary: FaceId,
    style: &TextStyle,
    spans: &[crate::rich::Span],
) -> Vec<Run> {
    let n = chars.len();
    let primary_face = fonts::face(primary);
    let ascii_coverage = (0..128)
        .map(|code| primary_face.covers(char::from(code)))
        .collect::<Vec<_>>();
    let styled_faces = spans
        .iter()
        .map(|span| {
            fonts::primary(
                if span.marks.code {
                    &Family::Monospace
                } else {
                    &style.family
                },
                style.bold || span.marks.strong,
            )
        })
        .collect::<Vec<_>>();
    let contextual_forms =
        if !spans.is_empty() && chars.iter().any(|c| (0x600..=0x8ff).contains(&(*c as u32))) {
            Some(arabic_forms(chars))
        } else {
            None
        };
    let mut faces = vec![];
    let mut scripts = vec![];
    let mut k = 0;
    while k < n {
        let c = chars[k];
        let mut script = script_of(c);
        // Common/inherited characters join the run they sit in.
        if script <= 1 || SCRIPT_NAMES[script as usize] == "Inherited" {
            script = if k > 0 { scripts[k - 1] } else { script };
        }
        scripts.push(script);
        k += 1;
    }
    // One face per cluster: a base with the marks, joiners and variation
    // selectors after it, and what a zero-width joiner joins on.
    k = 0;
    while k < n {
        let mut m = k + 1;
        while m < n && (joins_previous(chars[m]) || chars[m - 1] as i64 == 0x200d) {
            m += 1
        }
        if k % 1024 == 0 && !work_active() {
            return Vec::new();
        }
        let span_index = spans.partition_point(|span| span.end <= offsets[k] as usize);
        let preferred =
            if span_index < spans.len() && spans[span_index].start <= offsets[k] as usize {
                styled_faces[span_index]
            } else {
                primary
            };
        let face = if preferred == primary
            && m == k + 1
            && chars[k].is_ascii()
            && ascii_coverage[chars[k] as usize]
        {
            primary
        } else {
            fonts::face_for_cluster(preferred, &chars[k..m])
        };
        while faces.len() < m {
            faces.push(face)
        }
        k = m;
    }
    let mut runs = vec![];
    let mut s = 0;
    while s < n {
        if !work_active() {
            return Vec::new();
        }
        let mut e = s + 1;
        let style_end = rich_span(spans, offsets[s]).map_or(i64::MAX, |span| span.end as i64);
        while e < n
            && offsets[e] < style_end
            && levels[e] == levels[s]
            && faces[e] == faces[s]
            && scripts[e] == scripts[s]
        {
            e += 1;
        }
        let run_end = if e < n { offsets[e] } else { end };
        let mut glyphs = shape_run(
            &chars[s..e],
            &offsets[s..e],
            levels[s] % 2 == 1,
            faces[s],
            &script_tag(scripts[s]),
            contextual_forms.as_ref().map(|forms| &forms[s..e]),
        );
        for glyph in &mut glyphs {
            if let Some(image) =
                rich_span(spans, glyph.cluster).and_then(|span| span.image.as_ref())
            {
                glyph.x_advance = (image_size(image).0 * fonts::face(faces[s]).units_per_em as f64
                    / style.size)
                    .round() as i64;
                glyph.x_offset = 0;
                glyph.y_offset = 0;
            }
        }
        runs.push(Run {
            start: offsets[s],
            end: run_end,
            level: levels[s],
            face: faces[s],
            glyph_index: {
                let mut index = (0..glyphs.len()).collect::<Vec<_>>();
                index.sort_by_key(|&index| glyphs[index].cluster);
                index
            },
            glyphs,
        });
        s = e;
    }
    runs
}

/// Shapes one run in logical order: cmap, GSUB, advances, GPOS.
fn shape_run(
    chars: &[char],
    offsets: &[i64],
    rtl: bool,
    face_id: FaceId,
    script: &str,
    contextual_forms: Option<&[String]>,
) -> Vec<GlyphInfo> {
    let face = fonts::face(face_id);
    if indic::is_indic(script) {
        let mut glyphs = indic::shape(&face, script, chars, offsets);
        position_run(&face, script, &mut glyphs, rtl);
        return glyphs;
    }
    let forms = if script == "arab" || script == "syrc" {
        contextual_forms.map_or_else(|| arabic_forms(chars), |forms| forms.to_vec())
    } else {
        vec![String::new(); chars.len()]
    };
    let mut glyphs = vec![];
    let mut i = 0;
    while i < chars.len() {
        let c = if rtl {
            bidi::mirror(chars[i])
        } else {
            chars[i]
        };
        let gid = if c == '\t' {
            face.glyph_for(' ')
        } else {
            face.glyph_for(c)
        };
        let mut g = GlyphInfo::new(gid, offsets[i], &forms[i]);
        g.ignorable = fonts::is_default_ignorable(c);
        glyphs.push(g);
        i += 1;
    }
    let mut features = vec!["ccmp", "locl", "rlig", "liga", "clig", "calt"];
    if script == "arab" || script == "syrc" {
        features = vec![
            "ccmp", "locl", "isol", "fina", "medi", "init", "rlig", "calt", "liga",
        ];
    }
    if script == "latn" || script == "DFLT" || script == "cyrl" || script == "grek" {
        // Discretionary ligatures stay off; standard ones are on.
        features = vec!["ccmp", "locl", "liga", "clig", "calt"];
    }
    opentype::substitute(&face, script, &mut glyphs, &features);
    position_run(&face, script, &mut glyphs, rtl);
    glyphs
}

// Sets advances from the font's metrics, then applies GPOS (or `kern`).
// Default-ignorable characters the substitutions left alone become the
// space glyph with no advance, so they never show.
fn position_run(face: &Face, script: &str, glyphs: &mut Vec<GlyphInfo>, rtl: bool) {
    let space = face.glyph_for(' ');
    for g in glyphs.iter_mut() {
        if g.ignorable {
            g.gid = space
        };
        let (adv, _) = face.h_metrics(g.gid);
        g.x_advance = if g.ignorable { 0 } else { adv };
    }
    if !opentype::position(
        face,
        script,
        glyphs,
        &["kern", "mark", "mkmk", "curs", "dist", "abvm", "blwm"],
        rtl,
    ) {
        opentype::kern_table(face, glyphs)
    }
    for g in glyphs.iter_mut().filter(|g| g.ignorable) {
        g.x_advance = 0;
        g.x_offset = 0;
    }
}

// Positions one line's glyphs: slices each run to the line, orders runs
// visually (L2 at run granularity), and assigns pen positions.
fn place_line(
    chars: &[char],
    offsets: &[i64],
    levels: &[i64],
    runs: &[Run],
    from: i64,
    to: i64,
    start_byte: i64,
    end_byte: i64,
    para_level: i64,
    style: &TextStyle,
    tab_w: f64,
    boundaries: &[i64],
) -> Line {
    let mut line = Line {
        start: start_byte,
        end: end_byte,
        rtl: para_level == 1,
        ..Line::default()
    };
    if from >= to {
        return line;
    }
    // Trailing whitespace takes the paragraph level (L1).
    let mut line_levels = levels[from as usize..to as usize].to_vec();
    for t in (0..line_levels.len()).rev() {
        if !unicode::is_space(chars[from as usize + t]) {
            break;
        }
        line_levels[t] = para_level;
    }
    // Pieces: (run index, level) for each maximal run slice within the line.
    let mut pieces: Vec<(i64, i64, i64, i64)> = vec![];
    let mut r = 0;
    while r < runs.len() {
        let run = &runs[r];
        let s = run.start.max(start_byte);
        let e = run.end.min(end_byte);
        if s < e {
            // Split the slice where trailing whitespace changed its level.
            let mut a = s;
            while a < e {
                let ia = offset_index(offsets, a) - from;
                let lv = line_levels[ia as usize];
                let mut b = a;
                while b < e {
                    let ib = offset_index(offsets, b);
                    if ib < 0 || line_levels[(ib - from) as usize] != lv {
                        break;
                    };
                    b = if ib + 1 < offsets.len() as i64 {
                        offsets[(ib + 1) as usize]
                    } else {
                        e
                    };
                }
                if b == a {
                    b = e
                };
                pieces.push((r as i64, a, b.min(e), lv));
                a = b.min(e);
            }
        }
        r += 1;
    }
    let order = bidi::visual_order(&pieces.iter().map(|p| p.3).collect::<Vec<_>>());
    let mut pen = 0.0;
    for pi in order {
        let (ri, s, e, lv) = pieces[pi as usize];
        let run = &runs[ri as usize];
        let face = fonts::face(run.face);
        let scale = style.size / face.units_per_em as f64;
        let rtl = lv % 2 == 1;
        let begin = run
            .glyph_index
            .partition_point(|&index| run.glyphs[index].cluster < s);
        let end = run
            .glyph_index
            .partition_point(|&index| run.glyphs[index].cluster < e);
        let mut indexes = run.glyph_index[begin..end].to_vec();
        indexes.sort_unstable();
        let slice = indexes
            .iter()
            .map(|&index| &run.glyphs[index])
            .collect::<Vec<_>>();
        // Cluster ends in logical order, snapped to grapheme boundaries.
        let mut ends = vec![0; slice.len()];
        let mut next_start = e;
        for k in (0..slice.len()).rev() {
            if k + 1 < slice.len() && slice[k + 1].cluster != slice[k].cluster {
                next_start = slice[k + 1].cluster;
            }
            ends[k] = next_start;
        }
        let mut idx = vec![];
        let mut q = 0;
        while q < slice.len() {
            idx.push(q);
            q += 1;
        }
        if rtl {
            idx.reverse()
        };
        for q2 in idx {
            let g = &slice[q2];
            let mut advance = g.x_advance as f64 * scale;
            let ci = offset_index(offsets, g.cluster);
            if ci >= 0 && chars[ci as usize] == '\t' {
                advance = tab_w - (pen % tab_w);
            }
            let start_g = snap_down(boundaries, g.cluster);
            let end_g = snap_up(boundaries, ends[q2]);
            line.glyphs.push(PlacedGlyph {
                face: run.face,
                gid: g.gid,
                x: pen + g.x_offset as f64 * scale,
                y: -(g.y_offset as f64) * scale,
                advance,
                start: start_g,
                end: end_g,
                rtl,
                level: lv,
            });
            pen += advance;
        }
    }
    line.width = pen;
    line
}

// First boundary >= b, or > b when strict. Queries must not rescan the
// document's complete boundary index for every glyph in every paragraph.
fn boundary_index(boundaries: &[i64], b: i64, strict: bool) -> i64 {
    let mut lo = 0;
    let mut hi = boundaries.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if boundaries[mid] < b || (strict && boundaries[mid] == b) {
            lo = mid + 1
        } else {
            hi = mid
        }
    }
    lo as i64
}

fn snap_down(boundaries: &[i64], b: i64) -> i64 {
    let i = boundary_index(boundaries, b, true);
    if i == 0 {
        0
    } else {
        boundaries[(i - 1) as usize]
    }
}

fn snap_up(boundaries: &[i64], b: i64) -> i64 {
    let i = boundary_index(boundaries, b, false);
    if i == boundaries.len() as i64 {
        b
    } else {
        boundaries[i as usize]
    }
}

// Replaces the end of `line` with an ellipsis so it fits the width.
fn elide(
    text: &str,
    line: Line,
    style: &TextStyle,
    options: LayoutOptions,
    boundaries: &[i64],
) -> Line {
    let ell = "\u{2026}";
    let (ew, _) = measure(ell, style);
    let limit = if options.max_width >= 0.0 {
        options.max_width
    } else {
        line.width
    };
    let mut cut = line.end;
    loop {
        let candidate = text.substring(line.start, cut);
        let (w, _) = measure(candidate, style);
        if w + ew <= limit || cut <= line.start {
            break;
        }
        let mut prev = line.start;
        for b in boundaries {
            if *b < cut { prev = *b } else { break }
        }
        cut = prev;
    }
    let shown = text.substring(line.start, cut).to_string() + ell;
    let one = layout(&shown, style, LayoutOptions::single_line());
    let mut out = one.lines[0].clone();
    // The ellipsis maps to the elided range.
    let mut i = 0;
    while i < out.glyphs.len() {
        let g = out.glyphs[i].clone();
        if g.start >= cut - line.start {
            out.glyphs[i].start = cut;
            out.glyphs[i].end = line.end;
        } else {
            out.glyphs[i].start = g.start + line.start;
            out.glyphs[i].end = g.end + line.start;
        }
        i += 1;
    }
    out.start = line.start;
    out.end = line.end;
    // The shortened line keeps the place of the line it replaces.
    out.top = line.top;
    out.baseline = line.baseline;
    out.height = line.height;
    out.clone()
}

// ---- geometry queries ----

impl TextLayout {
    /// The index of the line holding byte `offset` (the last line for the end).
    pub fn line_of(&self, offset: i64) -> i64 {
        let mut lo = 0;
        let mut hi = self.lines.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.lines[mid].start <= offset {
                lo = mid + 1
            } else {
                hi = mid
            }
        }
        if lo == 0 { 0 } else { (lo - 1) as i64 }
    }

    /// The caret's x for byte `offset` within its line, from the layout left.
    pub fn caret_x(&self, offset: i64) -> f64 {
        if self.lines.is_empty() {
            return 0.0;
        }
        let line = &self.lines[(self.line_of(offset)) as usize];
        line.left + caret_in_line(line, offset, &self.boundaries)
    }

    /// The caret rectangle for byte `offset`, one pixel wide.
    pub fn caret_rect(&self, offset: i64) -> Rect {
        if self.lines.is_empty() {
            return Rect::new(0, 0, 1, self.line_height as i64);
        }
        let li = self.line_of(offset);
        let line = &self.lines[li as usize];
        let x = line.left + caret_in_line(line, offset, &self.boundaries);
        Rect::new(
            x.floor() as i64,
            line.top as i64,
            1,
            line.height.max(1.0) as i64,
        )
    }

    pub(crate) fn hit_line(&self, line: i64, x: f64) -> i64 {
        let Some(line) = self.lines.get(line.max(0) as usize) else {
            return self.text.byte_len();
        };
        hit_in_line(line, x - line.left, &self.boundaries)
    }

    /// The byte offset nearest the point (x, y) in layout coordinates.
    pub fn hit(&self, x: f64, y: f64) -> i64 {
        if self.lines.is_empty() {
            return 0;
        }
        let mut li = 0;
        while li + 1 < self.lines.len() && y >= self.lines[li].top + self.lines[li].height {
            li += 1;
        }
        let line = &self.lines[li];
        hit_in_line(line, x - line.left, &self.boundaries)
    }

    /// Rectangles covering the bytes `[from, to)`, one or more per line.
    pub fn selection_rects(&self, from: i64, to: i64) -> Vec<Rect> {
        let a = from.min(to);
        let b = from.max(to);
        let mut out = vec![];
        if a == b {
            return out;
        }
        for line in &self.lines {
            if line.end < a || line.start > b {
                continue;
            }
            let mut spans: Vec<(f64, f64)> = vec![];
            for g in &line.glyphs {
                if g.end <= a || g.start >= b {
                    continue;
                }
                let (left, right) = sub_span(g, a.max(g.start), b.min(g.end), &self.boundaries);
                spans.push((left, right))
            }
            // A selection running past the line end shows its newline.
            if b > line.end && a <= line.end {
                let w = self.line_height / 3.0;
                if line.rtl {
                    spans.push((-w, 0.0))
                } else {
                    spans.push((line.width, line.width + w))
                }
            }
            spans.sort_by_key(|s| (s.0 * 64.0) as i64);
            let mut merged: Vec<(f64, f64)> = vec![];
            for s in spans {
                let n = merged.len();
                if n > 0 && s.0 <= merged[n - 1].1 + 0.5 {
                    merged[n - 1] = (merged[n - 1].0, merged[n - 1].1.max(s.1))
                } else {
                    merged.push(s)
                }
            }
            for s in merged {
                let x0 = (line.left + s.0).floor() as i64;
                let x1 = (line.left + s.1 + 0.5).floor() as i64;
                out.push(Rect::new(
                    x0,
                    line.top as i64,
                    (x1 - x0).max(1),
                    line.height as i64,
                ))
            }
        }
        out
    }

    /// Window-pixel boxes for UTF-8 bytes `[from, to)` at `(x, y)`, split by line and bidi level.
    pub fn range_rects(&self, from: i64, to: i64, x: i64, y: i64) -> Vec<Rect> {
        let end = self.source_length.max(self.text.byte_len());
        let lo = from.min(to).max(0).min(end);
        let hi = from.max(to).max(0).min(end);
        if lo == hi {
            return vec![
                self.caret_rect(snap_down(&self.boundaries, lo))
                    .offset(x, y),
            ];
        }
        let a = snap_down(&self.boundaries, lo);
        let b = snap_up(&self.boundaries, hi);
        let mut out = vec![];
        for line in &self.lines {
            if line.end < a || line.start >= b {
                continue;
            };
            let mut level = -1;
            let mut span: Option<(f64, f64)> = None;
            for g in &line.glyphs {
                // Adjacent directions may touch on screen but must stay
                // separate for assistive technology's visual range queries.
                if level != g.level {
                    if let Some((left, right)) = span {
                        out.push(range_box(line, left, right).offset(x, y))
                    }
                    span = None;
                    level = g.level;
                }
                if g.end <= a || g.start >= b {
                    continue;
                };
                let (left, right) = sub_span(g, a.max(g.start), b.min(g.end), &self.boundaries);
                span = Some(match span {
                    Some((l, r)) => (l.min(left), r.max(right)),
                    None => (left, right),
                });
            }
            if let Some((left, right)) = span {
                out.push(range_box(line, left, right).offset(x, y))
            }
            // A newline has no glyph advance; expose its end caret rather
            // than the decorative newline width used by selection painting.
            if a <= line.end
                && b > line.end
                && line.end < end
                && self.text.byte_at(line.end - self.source_origin) == 10
            {
                let caret = self.caret_rect(line.end);
                out.push(caret.offset(x, y))
            }
        }
        out
    }

    /// The previous grapheme boundary before `offset` (logical order).
    pub fn prev_boundary(&self, offset: i64) -> i64 {
        let i = boundary_index(&self.boundaries, offset, false);
        if i == 0 {
            0
        } else {
            self.boundaries[(i - 1) as usize]
        }
    }

    /// The next grapheme boundary after `offset` (logical order).
    pub fn next_boundary(&self, offset: i64) -> i64 {
        let i = boundary_index(&self.boundaries, offset, true);
        if i == self.boundaries.len() as i64 {
            self.source_length.max(self.text.byte_len())
        } else {
            self.boundaries[i as usize]
        }
    }

    /// The grapheme boundaries of line `li` in display order, left to right;
    /// carets at the same x keep logical order.
    pub fn visual_carets(&self, li: i64) -> Vec<i64> {
        self.carets_by_x(li).iter().map(|c| c.1).collect::<Vec<_>>()
    }

    fn carets_by_x(&self, li: i64) -> Vec<(f64, i64)> {
        let mut carets: Vec<(f64, i64)> = vec![];
        if li < 0 || li >= self.lines.len() as i64 {
            return carets;
        }
        let line = &self.lines[li as usize];
        for b in &self.boundaries {
            if *b >= line.start && *b <= line.end {
                carets.push((caret_in_line(line, *b, &self.boundaries), *b))
            }
        }
        carets.sort_by_key(|c| ((c.0 * 64.0) as i64, c.1));
        carets
    }

    /// The caret position one step visually left (`dir` < 0) or right of
    /// `offset`, crossing bidi run boundaries in display order.
    pub fn visual_step(&self, offset: i64, dir: i64) -> i64 {
        if self.lines.is_empty() {
            return offset;
        }
        let li = self.line_of(offset);
        let line = &self.lines[li as usize];
        let carets = self.carets_by_x(li);
        let here = caret_in_line(line, offset, &self.boundaries);
        let mut at: i64 = -1;
        let mut i = 0;
        while i < carets.len() {
            if carets[i].1 == offset {
                at = i as i64
            };
            i += 1;
        }
        if at < 0 {
            return offset;
        }
        let target = at + dir;
        if target < 0 {
            return if li > 0 {
                if line.rtl {
                    self.lines[(li - 1) as usize].start
                } else {
                    self.lines[(li - 1) as usize].end
                }
            } else {
                offset
            };
        }
        if target >= carets.len() as i64 {
            return if li + 1 < self.lines.len() as i64 {
                if line.rtl {
                    self.lines[(li + 1) as usize].end
                } else {
                    self.lines[(li + 1) as usize].start
                }
            } else {
                offset
            };
        }
        // Skip carets at the same x (run boundaries) so each press moves.
        let mut t = target;
        while t >= 0
            && t < carets.len() as i64
            && (carets[t as usize].0 - here).abs() < 0.01
            && carets[t as usize].1 != offset
        {
            t += dir;
        }
        if t < 0 || t >= carets.len() as i64 {
            return carets[target as usize].1;
        };
        carets[t as usize].1
    }

    /// The offset on the line above (`dir` < 0) or below nearest to x.
    pub fn vertical_step(&self, offset: i64, dir: i64, x: f64) -> i64 {
        let li = self.line_of(offset);
        let target = li + dir;
        if target < 0 {
            return 0;
        }
        if target >= self.lines.len() as i64 {
            return self.lines.last().map_or(0, |line| line.end);
        }
        let line = &self.lines[target as usize];
        hit_in_line(line, x - line.left, &self.boundaries)
    }
}

fn caret_in_line(line: &Line, offset: i64, boundaries: &[i64]) -> f64 {
    if line.glyphs.is_empty() {
        return 0.0;
    }
    for g in &line.glyphs {
        if offset >= g.start && offset < g.end {
            if offset == g.start {
                return if g.rtl { g.x + g.advance } else { g.x };
            }
            let (left, right) = sub_span(g, g.start, offset, boundaries);
            return if g.rtl { left } else { right };
        }
    }
    // The line end: the trailing edge of the logically last glyph.
    let mut last = &line.glyphs[0];
    for g in &line.glyphs {
        if g.end >= last.end {
            last = g
        }
    }
    if offset >= line.end || offset >= last.end {
        return if last.rtl {
            last.x
        } else {
            last.x + last.advance
        };
    }
    if line.rtl { line.width } else { 0.0 }
}

// Round outward so a fractional edge never excludes part of a text cell.
fn range_box(line: &Line, left: f64, right: f64) -> Rect {
    let x0 = (line.left + left).floor() as i64;
    let x1 = (line.left + right).ceil() as i64;
    let y0 = line.top.floor() as i64;
    let y1 = (line.top + line.height).ceil() as i64;
    Rect::new(x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
}

// The x span of the part of glyph `g` covering bytes [a, b), splitting a
// ligature's advance evenly between the graphemes it stands for.
fn sub_span(g: &PlacedGlyph, a: i64, b: i64, boundaries: &[i64]) -> (f64, f64) {
    let start = boundary_index(boundaries, g.start, false);
    let end = boundary_index(boundaries, g.end, true);
    let count = (end - start - 1).max(1);
    let ai = boundary_index(boundaries, a, false);
    let bi = boundary_index(boundaries, b, false);
    let ia = if ai >= start && ai < end && boundaries[ai as usize] == a {
        ai - start
    } else {
        0
    };
    let ib = if bi >= start && bi < end && boundaries[bi as usize] == b {
        bi - start
    } else {
        count
    };
    let step = g.advance / count as f64;
    if g.rtl {
        (
            g.x + g.advance - ib as f64 * step,
            g.x + g.advance - ia as f64 * step,
        )
    } else {
        (g.x + ia as f64 * step, g.x + ib as f64 * step)
    }
}

fn hit_in_line(line: &Line, x: f64, boundaries: &[i64]) -> i64 {
    if line.glyphs.is_empty() {
        return line.start;
    }
    if x <= 0.0 {
        let g = &line.glyphs[0];
        return if g.rtl { g.end.min(line.end) } else { g.start };
    }
    if x >= line.width {
        let g = &line.glyphs[line.glyphs.len() - 1];
        return if g.rtl { g.start } else { g.end.min(line.end) };
    }
    for g in &line.glyphs {
        if x >= g.x && x < g.x + g.advance.max(0.001) {
            let first = boundary_index(boundaries, g.start, false);
            let last = boundary_index(boundaries, g.end, true);
            let parts = &boundaries[first as usize..last as usize];
            let count = (parts.len() - 1).max(1);
            let step = g.advance / count as f64;
            let mut rel = (x - g.x) / step.max(0.001);
            if g.rtl {
                rel = count as f64 - rel
            };
            let k = (rel + 0.5) as i64;
            let k2 = k.max(0).min(count as i64);
            return parts[(k2.min(parts.len() as i64 - 1)) as usize].min(line.end);
        }
    }
    line.end
}

// ---- painting ----

/// A rasterised glyph mask and its placement relative to the pen.
#[derive(Debug, Clone, PartialEq, Default)]
struct GlyphBitmap {
    mask: Mask,
}

const SUBPIXEL: i64 = 4;
const GLYPH_BUDGET: usize = 24_000_000;
static GLYPHS: SharedCache<(i64, i64, i64, i64, bool), GlyphBitmap> =
    LazyLock::new(|| Mutex::new(ByteCache::new(GLYPH_BUDGET / 2)));

/// Glyph cache counters: (glyphs, bytes).
pub fn glyph_cache_stats() -> (i64, i64) {
    let masks = GLYPHS.lock();
    let colors = COLOR_GLYPHS.lock();
    (
        (masks.len() + colors.len()) as i64,
        (masks.bytes() + colors.bytes()) as i64,
    )
}

fn glyph_mask(face_id: FaceId, gid: i64, size: f64, sub: i64, italic: bool) -> Mask {
    let fid: i64 = face_id.into();
    let key = (fid, gid, (size * 64.0) as i64, sub, italic);
    if let Some(found) = GLYPHS.lock().get(&key) {
        return found.mask.clone();
    }
    let face = fonts::face(face_id);
    let outline = face.outline(gid);
    let s = size / face.units_per_em as f64;
    let slant = if italic && !face.italic { 0.2 * s } else { 0.0 };
    let mask = Mask::fill(
        &outline.path,
        s,
        0.0,
        slant,
        -s,
        sub as f64 / SUBPIXEL as f64,
        0.0,
        FillRule::NonZero,
    );
    let bytes = mask.coverage.capacity();
    GLYPHS
        .lock()
        .insert(key, GlyphBitmap { mask: mask.clone() }, bytes);
    mask
}

// A color glyph sized for one text size: drawn `width` by `height` from
// `rgba` (`source_w` by `source_h`), offset from the pen and the baseline.
#[derive(Debug, Clone, PartialEq, Default)]
struct ColorImage {
    left: i64,
    top: i64,
    width: i64,
    height: i64,
    rgba: Vec<u8>,
    source_w: i64,
    source_h: i64,
}

static COLOR_GLYPHS: SharedCache<(i64, i64, i64), ColorImage> =
    LazyLock::new(|| Mutex::new(ByteCache::new(GLYPH_BUDGET / 2)));

// The color bitmap of glyph `gid` at `size` pixels per em; empty when the
// face has none for it or its PNG does not decode.
fn color_glyph(face_id: FaceId, gid: i64, size: f64) -> ColorImage {
    let fid: i64 = face_id.into();
    let key = (fid, gid, (size * 64.0) as i64);
    if let Some(found) = COLOR_GLYPHS.lock().get(&key) {
        return found.clone();
    }
    let image = match fonts::face(face_id).color_bitmap(gid, size) {
        Some(g) => sized_color_glyph(&g, size),
        None => ColorImage::default(),
    };
    let bytes = image.rgba.capacity();
    COLOR_GLYPHS.lock().insert(key, image.clone(), bytes);
    image
}

// Shrinks a strike's bitmap to `size` by area averaging; one drawn larger
// than its strike keeps its pixels and is scaled while drawing.
fn sized_color_glyph(g: &ColorGlyph, size: f64) -> ColorImage {
    let Some(decoded) = render::png::decode(&g.png) else {
        return ColorImage::default();
    };
    let src_w = decoded.width;
    let src_h = decoded.height;
    let scale = size / g.ppem as f64;
    let w = (src_w as f64 * scale + 0.5).max(1.0) as i64;
    let h = (src_h as f64 * scale + 0.5).max(1.0) as i64;
    let pixels = decoded.rgba;
    let shrinking = w < src_w && h < src_h;
    let rgba = if shrinking {
        render::shrink(&pixels, src_w, src_h, w, h)
    } else {
        pixels
    };
    let left = (g.bearing_x as f64 * scale + 0.5).floor() as i64;
    let top = -((g.bearing_y as f64 * scale + 0.5).floor() as i64);
    ColorImage {
        left,
        top,
        width: w,
        height: h,
        rgba,
        source_w: if shrinking { w } else { src_w },
        source_h: if shrinking { h } else { src_h },
    }
}

/// Draws `layout` with its top-left at (`x`, `y`) in `color`.
pub fn draw(
    canvas: &mut Canvas,
    layout: &TextLayout,
    x: f64,
    y: f64,
    style: &TextStyle,
    color: Color,
) {
    if let Some(error) = &layout.error {
        canvas.paint_error = Some(error.clone());
        return;
    }
    for line in &layout.lines {
        // Skip lines entirely outside the clip.
        let top = y + line.top;
        if top > (canvas.clip.y + canvas.clip.height) as f64
            || top + line.height < canvas.clip.y as f64
        {
            continue;
        }
        draw_line(canvas, line, x + line.left, y + line.baseline, style, color)
    }
}

/// Paints rich text and inline image objects using their shaped geometry.
pub fn draw_rich(
    canvas: &mut Canvas,
    layout: &TextLayout,
    x: f64,
    y: f64,
    style: &TextStyle,
    base: Color,
    spans: &[crate::rich::Span],
    document: i64,
    theme: &crate::style::Theme,
) {
    if let Some(error) = &layout.error {
        let caption = format!(
            "Preview unavailable: {}",
            error.chars().take(160).collect::<String>()
        );
        let label = self::layout(
            &caption,
            &TextStyle::sans(14.0),
            LayoutOptions::wrapped(480.0),
        );
        draw(
            canvas,
            &label,
            x,
            y,
            &TextStyle::sans(14.0),
            theme.text_muted,
        );
        return;
    }
    for line in &layout.lines {
        if y + line.top > canvas.clip.bottom() as f64
            || y + line.top + line.height < canvas.clip.y as f64
        {
            continue;
        }
        for glyph in &line.glyphs {
            let span = rich_span(spans, glyph.start);
            let mut effective = style.clone();
            let mut color = base;
            if let Some(span) = span {
                effective.bold |= span.marks.strong;
                effective.italic |= span.marks.emphasis;
                if !span.marks.link.is_empty() {
                    color = theme.link;
                }
                if let Some(image) = &span.image {
                    let (width, height) = image_size(image);
                    let left = x + line.left + glyph.x;
                    let top = y + line.baseline - height;
                    if let Some(asset) = crate::rich::registry::image(document, &image.source) {
                        canvas.image_scaled(
                            left.round() as i64,
                            top.round() as i64,
                            width.round() as i64,
                            height.round() as i64,
                            &asset.rgba,
                            asset.width,
                            asset.height,
                        );
                    } else {
                        canvas.rect(
                            left.round() as i64,
                            top.round() as i64,
                            width.round() as i64,
                            height.round() as i64,
                            theme.surface_alt,
                        );
                        let caption = self::layout(
                            &image.alt.chars().take(256).collect::<String>(),
                            &TextStyle::sans(style.size.min(16.0)),
                            LayoutOptions::elided(width.max(1.0)),
                        );
                        draw(
                            canvas,
                            &caption,
                            left,
                            top,
                            &TextStyle::sans(style.size.min(16.0)),
                            theme.text_muted,
                        );
                    }
                    continue;
                }
                if span.marks.underline || !span.marks.link.is_empty() {
                    canvas.rect(
                        (x + line.left + glyph.x).floor() as i64,
                        (y + line.baseline + 2.0).round() as i64,
                        glyph.advance.ceil().max(1.0) as i64,
                        1,
                        color,
                    );
                }
                if span.marks.strike {
                    canvas.rect(
                        (x + line.left + glyph.x).floor() as i64,
                        (y + line.baseline - style.size * 0.3).round() as i64,
                        glyph.advance.ceil().max(1.0) as i64,
                        1,
                        color,
                    );
                }
            }
            draw_glyph(
                canvas,
                glyph,
                x + line.left,
                y + line.baseline,
                &effective,
                color,
            );
        }
    }
}

/// Draws source spans without changing shaping, caret positions, or selection geometry.
pub fn draw_syntax(
    canvas: &mut Canvas,
    layout: &TextLayout,
    x: f64,
    y: f64,
    style: &TextStyle,
    base: Color,
    spans: &[crate::text::syntax::Span],
    theme: &crate::style::Theme,
) {
    if let Some(error) = &layout.error {
        canvas.paint_error = Some(error.clone());
        return;
    }
    for line in &layout.lines {
        if y + line.top > canvas.clip.bottom() as f64
            || y + line.top + line.height < canvas.clip.y as f64
        {
            continue;
        };
        for glyph in &line.glyphs {
            let mut lo = 0;
            let mut hi = spans.len();
            while lo < hi {
                let mid = (lo + hi) / 2;
                if spans[mid].end <= glyph.start {
                    lo = mid + 1
                } else {
                    hi = mid
                }
            }
            let color = if lo < spans.len() && spans[lo].start <= glyph.start {
                crate::text::syntax::color(spans[lo].kind, theme)
            } else {
                base
            };
            draw_glyph(
                canvas,
                glyph,
                x + line.left,
                y + line.baseline,
                style,
                color,
            )
        }
    }
}

/// Draws one line's glyphs with the baseline at `baseline`.
pub fn draw_line(
    canvas: &mut Canvas,
    line: &Line,
    x: f64,
    baseline: f64,
    style: &TextStyle,
    color: Color,
) {
    for g in &line.glyphs {
        draw_glyph(canvas, g, x, baseline, style, color)
    }
}

fn draw_glyph(
    canvas: &mut Canvas,
    g: &PlacedGlyph,
    x: f64,
    baseline: f64,
    style: &TextStyle,
    color: Color,
) {
    let gx = x + g.x;
    if gx + g.advance < canvas.clip.x as f64 - style.size
        || gx > canvas.clip.right() as f64 + style.size
    {
        return;
    };
    let whole = gx.floor();
    if fonts::is_color(g.face) {
        // Color glyphs keep their own colors and sit on whole pixels.
        let c = color_glyph(g.face, g.gid, style.size);
        if c.width > 0 {
            canvas.image_scaled(
                whole as i64 + c.left,
                (baseline + g.y + 0.5).floor() as i64 + c.top,
                c.width,
                c.height,
                &c.rgba,
                c.source_w,
                c.source_h,
            )
        }
        return;
    }
    let sub = ((gx - whole) * SUBPIXEL as f64) as i64;
    let m = glyph_mask(
        g.face,
        g.gid,
        style.size,
        sub.min(SUBPIXEL - 1),
        style.italic,
    );
    if m.width > 0 {
        let gy = (baseline + g.y + 0.5).floor() as i64;
        canvas.coverage(
            whole as i64 + m.left,
            gy + m.top,
            m.width,
            m.height,
            &m.coverage,
            color,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{LayoutOptions, PlacedGlyph, TextStyle, draw_glyph, layout, measure};

    #[test]
    fn an_uncached_layout_leaves_the_shared_caches_alone() {
        let style = TextStyle::mono(14.0);
        let options = LayoutOptions::single_line();
        let text = "a row the shared caches never see 0x5e1f";
        let laid = super::layout_uncached(text, &style, options);
        assert!(laid.error.is_none());
        assert_eq!(laid, layout(text, &style, options));
        let key = format!("{}#{}#{}", style.key(), options.key(), text);
        super::CACHE.lock().remove(&key);
        super::PARAGRAPHS.lock().remove(&key);
        super::layout_uncached(text, &style, options);
        assert!(super::CACHE.lock().get(&key).is_none());
        assert!(super::PARAGRAPHS.lock().get(&key).is_none());
    }
    use crate::render::Canvas;
    use crate::strext::StrExt;
    use crate::style::color::Color;
    use crate::text::bidi;
    use crate::text::{font, fonts};

    fn sans() -> TextStyle {
        TextStyle::sans(16.0)
    }

    #[test]
    fn a_color_glyph_draws_its_own_pixels_at_the_text_size() {
        let ids = fonts::register(&font::color_test_face()).unwrap();
        assert!(fonts::is_color(ids[0]));
        let mut c = Canvas::new(20, 20);
        c.rect(0, 0, 20, 20, Color::white());
        // A 2-pixel strike at 20 ppem drawn at 40 px: 4 by 4, its top 4 px
        // above the baseline, in red whatever the text color.
        let g = PlacedGlyph {
            face: ids[0],
            gid: 1,
            ..PlacedGlyph::default()
        };
        draw_glyph(
            &mut c,
            &g,
            5.0,
            12.0,
            &TextStyle::sans(40.0),
            Color::black(),
        );
        let at = |x: i64, y: i64| -> (u8, u8, u8) {
            (
                c.pixels[((y * 20 + x) * 4) as usize],
                c.pixels[((y * 20 + x) * 4 + 1) as usize],
                c.pixels[((y * 20 + x) * 4 + 2) as usize],
            )
        };
        assert_eq!(at(6, 9), (255, 0, 0));
        assert_eq!(at(5, 8), (255, 0, 0));
        assert_eq!(at(10, 9), (255, 255, 255));
        assert_eq!(at(6, 13), (255, 255, 255))
    }

    #[test]
    fn joiners_take_no_room() {
        let (plain, _) = measure("ab", &sans());
        let (joined, _) = measure("a\u{200D}b\u{FE0F}", &sans());
        assert_eq!(joined, plain)
    }

    #[test]
    fn right_to_left_marks_sit_over_their_base() {
        // Bet with patah and beh with fatha: logical base then mark, drawn
        // mark first once the run is reversed.
        for text in ["\u{5D1}\u{5B7}", "\u{628}\u{64E}"] {
            let line = layout(text, &sans(), LayoutOptions::single_line()).lines[0].clone();
            assert_eq!(line.glyphs.len(), 2);
            let mark = line.glyphs[0].clone();
            let base = line.glyphs[1].clone();
            assert!(
                mark.x > base.x - base.advance / 2.0 && mark.x < base.x + base.advance,
                "{}: mark at {}, base at {}+{}",
                text,
                mark.x,
                base.x,
                base.advance
            )
        }
    }

    #[test]
    fn single_line_measures_positive_and_grows() {
        let (a, h) = measure("ab", &sans());
        let (b, _) = measure("abcd", &sans());
        assert!(a > 0.0);
        assert!(b > a);
        assert!(h > 15.0)
    }

    #[test]
    fn wrapping_splits_at_spaces_and_keeps_all_text() {
        let l = layout(
            "alpha beta gamma delta",
            &sans(),
            LayoutOptions::wrapped(80.0),
        );
        assert!(l.lines.len() >= 2);
        assert_eq!(l.lines[0].start, 0);
        let last = &l.lines[l.lines.len() - 1];
        assert_eq!(last.end, 22);
        for line in l.lines {
            assert!(line.width <= 80.5 || line.glyphs.len() <= 6)
        }
    }

    #[test]
    fn newlines_start_new_lines() {
        let l = layout("a\nb\n", &sans(), LayoutOptions::single_line());
        assert_eq!(l.lines.len(), 3);
        assert_eq!(l.lines[1].start, 2);
        assert_eq!(l.lines[2].start, 4)
    }

    #[test]
    fn carets_advance_left_to_right_for_latin() {
        let l = layout("abc", &sans(), LayoutOptions::single_line());
        assert_eq!(l.caret_x(0), 0.0);
        assert!(l.caret_x(1) > 0.0);
        assert!(l.caret_x(3) > l.caret_x(2));
        assert_eq!(l.hit(0.0, 1.0), 0);
        assert_eq!(l.hit(1000.0, 1.0), 3)
    }

    #[test]
    fn hebrew_carets_run_right_to_left() {
        let l = layout(
            "\u{05d0}\u{05d1}\u{05d2}",
            &sans(),
            LayoutOptions::single_line(),
        );
        assert!(l.lines[0].rtl);
        // The logical start sits at the right edge.
        assert!(l.caret_x(0) > l.caret_x(2));
        assert!(l.caret_x(6) < 1.0)
    }

    #[test]
    fn combining_marks_are_one_caret_stop() {
        let l = layout("e\u{0301}x", &sans(), LayoutOptions::single_line());
        assert_eq!(l.boundaries, vec![0, 3, 4]);
        assert_eq!(l.next_boundary(0), 3)
    }

    #[test]
    fn unchanged_paragraphs_reuse_shaping_and_rebase_utf8_offsets() {
        super::purge_caches();
        let _ = layout("é first\nsecond", &sans(), LayoutOptions::single_line());
        let (_, hits, misses, _) = super::paragraph_cache_stats();
        let l = layout(
            "heading\né first\nsecond",
            &sans(),
            LayoutOptions::single_line(),
        );
        let (_, after_hits, after_misses, bytes) = super::paragraph_cache_stats();
        assert_eq!(after_misses - misses, 1);
        assert_eq!(after_hits - hits, 2);
        assert_eq!(l.lines[1].start, 8);
        assert_eq!(l.lines[2].start, 17);
        assert_eq!(
            l.text
                .substring(l.lines[1].glyphs[0].start, l.lines[1].glyphs[0].end),
            "é"
        );
        assert!(bytes <= super::PARAGRAPH_BUDGET as i64);
        let warm = l.caret_rect(17);
        super::purge_caches();
        let cold = layout(
            "heading\né first\nsecond",
            &sans(),
            LayoutOptions::single_line(),
        );
        assert_eq!(cold.caret_rect(17), warm)
    }

    #[test]
    fn line_and_cluster_queries_keep_newline_and_utf8_edges() {
        let l = layout("e\u{0301}\n\n👩‍💻!", &sans(), LayoutOptions::single_line());
        assert_eq!(l.line_of(3), 0);
        assert_eq!(l.line_of(4), 1);
        assert_eq!(l.line_of(5), 2);
        assert_eq!(l.line_of(17), 2);
        assert_eq!(l.prev_boundary(16), 5);
        assert_eq!(l.next_boundary(5), 16);
        assert_eq!(super::snap_down(&l.boundaries, 9), 5);
        assert_eq!(super::snap_up(&l.boundaries, 9), 16)
    }

    #[test]
    fn range_boxes_follow_window_origin_and_line_alignment() {
        let mut options = LayoutOptions::wrapped(200.0);
        options.align = super::TextAlign::Center;
        let l = layout("hello", &sans(), options);
        let local = l.range_rects(0, 5, 0, 0);
        let window = l.range_rects(0, 5, 23, -7);
        assert_eq!(local.len(), 1);
        assert!(local[0].x > 50);
        assert_eq!(window[0], local[0].offset(23, -7));
        assert_eq!(l.range_rects(5, 0, 23, -7), window)
    }

    #[test]
    fn range_boxes_split_touching_bidi_runs() {
        let l = layout(
            "ab \u{05d0}\u{05d1} cd",
            &sans(),
            LayoutOptions::single_line(),
        );
        let boxes = l.range_rects(0, l.text.byte_len(), 0, 0);
        assert_eq!(boxes.len(), 3);
        assert!(boxes[0].x < boxes[1].x && boxes[1].x < boxes[2].x);
        assert!(boxes.iter().all(|r| r.width > 0 && r.height > 0))
    }

    #[test]
    fn range_boxes_split_lines_and_represent_empty_line_newlines() {
        let l = layout("a\n\nb", &sans(), LayoutOptions::single_line());
        let boxes = l.range_rects(0, 4, 0, 0);
        assert_eq!(boxes.len(), 4);
        assert_eq!(boxes[0].y, boxes[1].y);
        assert!(boxes[2].y > boxes[1].y);
        assert!(boxes[3].y > boxes[2].y);
        assert_eq!(boxes[2].width, 1);
        let wrapped = layout("one two three four", &sans(), LayoutOptions::wrapped(40.0));
        let ranges = wrapped.range_rects(0, wrapped.text.byte_len(), 0, 0);
        assert_eq!(ranges.len(), wrapped.lines.len())
    }

    #[test]
    fn range_boxes_clamp_offsets_and_keep_graphemes_whole() {
        let l = layout("e\u{0301}x", &sans(), LayoutOptions::single_line());
        assert_eq!(l.range_rects(1, 2, 0, 0), l.range_rects(0, 3, 0, 0));
        assert_eq!(l.range_rects(-20, 200, 0, 0), l.range_rects(0, 4, 0, 0));
        assert_eq!(
            l.range_rects(200, 200, 4, 8),
            vec![l.caret_rect(4).offset(4, 8)]
        );
        let empty = layout("", &sans(), LayoutOptions::single_line());
        assert_eq!(empty.range_rects(0, 0, 4, 8).len(), 1)
    }

    #[test]
    fn range_boxes_split_ligature_advances_at_grapheme_boundaries() {
        let l = layout("ffi", &sans(), LayoutOptions::single_line());
        let full = l.range_rects(0, 3, 0, 0);
        let part = l.range_rects(1, 2, 0, 0);
        assert_eq!(part.len(), 1);
        assert!(part[0].width < full[0].width);
        assert!(part[0].x >= full[0].x);
        assert!(part[0].right() <= full[0].right())
    }

    #[test]
    fn selection_covers_the_range() {
        let l = layout("hello world", &sans(), LayoutOptions::single_line());
        let rects = l.selection_rects(0, 5);
        assert_eq!(rects.len(), 1);
        assert!(rects[0].width > 10)
    }

    #[test]
    fn mixed_direction_selection_can_be_split() {
        let l = layout(
            "ab \u{05d0}\u{05d1} cd",
            &sans(),
            LayoutOptions::single_line(),
        );
        let rects = l.selection_rects(1, 5);
        assert!(!rects.is_empty())
    }

    #[test]
    fn an_elided_line_keeps_its_place_below_the_lines_before_it() {
        let style = crate::style::Theme::light().font(crate::style::TextRole::Body);
        let mut o = super::LayoutOptions::wrapped(70.0);
        o.max_lines = 2;
        let l = super::layout("Budget 2026.ods quarterly", &style, o);
        assert!(l.elided);
        assert_eq!(l.lines.len(), 2);
        assert!(
            l.lines[1].top >= l.lines[0].top + l.lines[0].height,
            "{} overlaps {}",
            l.lines[1].top,
            l.lines[0].top
        );
        assert!(l.lines[1].baseline > l.lines[0].baseline)
    }

    #[test]
    fn max_lines_elides_with_an_ellipsis() {
        let mut o = LayoutOptions::wrapped(60.0);
        o.max_lines = 1;
        let l = layout("one two three four five", &sans(), o);
        assert_eq!(l.lines.len(), 1);
        assert!(l.elided);
        assert!(l.lines[0].width <= 61.0)
    }

    #[test]
    fn center_alignment_offsets_short_lines() {
        let mut o = LayoutOptions::wrapped(200.0);
        o.align = super::TextAlign::Center;
        let l = layout("hi", &sans(), o);
        assert!(l.lines[0].left > 50.0)
    }

    #[test]
    fn rtl_direction_aligns_start_to_the_right() {
        let mut o = LayoutOptions::wrapped(200.0);
        o.direction = bidi::Direction::Rtl;
        let l = layout("abc", &sans(), o);
        assert!(l.lines[0].left > 100.0)
    }

    #[test]
    fn arabic_shapes_into_joined_forms() {
        // Three letters: the middle one takes its medial form, so the
        // glyphs differ from the isolated cmap glyphs.
        let isolated = layout("\u{0628}", &sans(), LayoutOptions::single_line());
        let joined = layout(
            "\u{0628}\u{0628}\u{0628}",
            &sans(),
            LayoutOptions::single_line(),
        );
        let iso_gid = isolated.lines[0].glyphs[0].gid;
        let mut differs = false;
        for g in &joined.lines[0].glyphs {
            if g.gid != iso_gid {
                differs = true
            }
        }
        assert!(differs)
    }

    #[test]
    fn drawing_puts_ink_inside_the_box() {
        let mut c = Canvas::new(80, 30);
        c.fill(Color::white());
        let l = layout("Hi", &sans(), LayoutOptions::single_line());
        super::draw(&mut c, &l, 2.0, 2.0, &sans(), Color::black());
        let mut dark = 0;
        let mut i = 0;
        while i < c.pixels.len() {
            if c.pixels[i] < 128 {
                dark += 1
            };
            i += 4;
        }
        assert!(dark > 10)
    }

    #[test]
    fn long_words_break_anywhere_when_needed() {
        let mut o = LayoutOptions::wrapped(30.0);
        o.wrap = super::Wrap::Word;
        let l = layout("abcdefghijkl", &sans(), o);
        assert!(l.lines.len() >= 2)
    }

    #[test]
    fn tabs_advance_to_stops() {
        let (a, _) = measure("\tx", &TextStyle::mono(10.0));
        let (b, _) = measure("    x", &TextStyle::mono(10.0));
        assert!((a - b).abs() < 0.5)
    }
}

#[cfg(test)]
mod byte_budget_tests {
    use super::*;

    #[test]
    fn purge_releases_color_glyphs_and_the_complete_accounted_layout_storage() {
        let image = ColorImage {
            rgba: vec![255; 64],
            ..Default::default()
        };
        COLOR_GLYPHS.lock().insert((-1, -1, 12), image, 64);
        assert!(glyph_cache_stats().1 >= 64);
        let _ = layout(
            "cache bytes",
            &TextStyle::sans(12.0),
            LayoutOptions::single_line(),
        );
        let (bytes, limit) = cache_bytes();
        assert!(bytes > 0 && bytes <= limit);
        purge_caches();
        assert_eq!(cache_bytes().0, 0);
        assert_eq!(glyph_cache_stats(), (0, 0));
    }
}
