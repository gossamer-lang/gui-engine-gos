pub mod bidi;
pub mod catalog;
pub mod folding;
pub mod font;
pub mod font_assets;
pub mod fonts;
pub mod indic;
pub mod layout;
pub mod linebreak;
pub mod offsets;
pub mod opentype;
pub mod syntax;
pub mod ucd;
pub mod variation;

// text: fonts, shaping, bidirectional layout, line breaking, and drawing.
// `layout` is the entry point every widget uses; `fonts` registers faces;
// `font` and `opentype` read font files; `bidi` and `linebreak` implement
// UAX #9 and UAX #14 over the pinned Unicode data in `ucd`.

use crate::render::Canvas;
use crate::style::color::Color;
use crate::text::layout::{LayoutOptions, TextStyle};

/// Draws one line of `text` with its line box's top-left at (`x`, `y`).
pub fn draw_text(canvas: &mut Canvas, x: i64, y: i64, text: &str, style: &TextStyle, color: Color) {
    let l = layout::layout(text, style, LayoutOptions::single_line());
    layout::draw(canvas, &l, x as f64, y as f64, style, color)
}

/// The pixel width of one line of `text`.
pub fn width(text: &str, style: &TextStyle) -> f64 {
    let (w, _) = layout::measure(text, style);
    w
}

pub mod document_layout;

pub mod document_syntax;
