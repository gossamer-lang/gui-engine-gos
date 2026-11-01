// Characters a terminal draws as shapes on its cell grid instead of from a
// font: block elements, box drawing and sextants. A font's glyphs for them
// cover its own ascent and descent, which leaves gaps wherever the line is
// taller, so pictures built from them (as chafa's symbols are) break into
// stripes and box lines stop short of their neighbors.

use crate::geometry::Rect;
use crate::render::Canvas;
use crate::style::color::Color;

/// Draws `text`, when it is one block element, box-drawing character or
/// sextant, as shapes filling the cell `r` in `fg` over `bg`; answers whether
/// it did.
pub(super) fn draw(canvas: &mut Canvas, text: &str, r: Rect, fg: Color, bg: Color) -> bool {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return false;
    };
    if r.width <= 0 || r.height <= 0 {
        return false;
    }
    let c = u32::from(c);
    match c {
        0x2500..=0x257F => box_drawing(canvas, c, r, fg),
        0x2580..=0x259F => block(canvas, c, r, fg, bg),
        0x1FB00..=0x1FB3B => sextant(canvas, c - 0x1FB00, r, fg),
        _ => false,
    }
}

/// Whether `text` is one character [`draw`] draws as shapes: a piece of a
/// picture or a frame, rather than a letter.
pub(super) fn is_shape(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next().map(u32::from), chars.next()),
        (Some(0x2500..=0x259F | 0x1FB00..=0x1FB3B), None)
    )
}

// `n` eighths of `length`. Every fraction is measured from the cell's top or
// left edge, so complementary blocks, such as the upper and lower halves,
// share an edge.
fn eighths(length: i64, n: i64) -> i64 {
    (length * n + 4) / 8
}

fn fill(canvas: &mut Canvas, x0: i64, y0: i64, x1: i64, y1: i64, color: Color) {
    if x1 > x0 && y1 > y0 {
        canvas.rect(x0, y0, x1 - x0, y1 - y0, color)
    }
}

// Quadrant blocks U+2596..U+259F as bits: upper left 1, upper right 2,
// lower left 4, lower right 8.
const QUADRANTS: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];

fn block(canvas: &mut Canvas, c: u32, r: Rect, fg: Color, bg: Color) -> bool {
    let Rect {
        x,
        y,
        width: w,
        height: h,
    } = r;
    let (right, bottom) = (x + w, y + h);
    match c {
        0x2580 => fill(canvas, x, y, right, y + eighths(h, 4), fg),
        0x2581..=0x2588 => {
            let n = i64::from(c - 0x2580);
            fill(canvas, x, y + eighths(h, 8 - n), right, bottom, fg)
        }
        0x2589..=0x258F => {
            let n = i64::from(0x2590 - c);
            fill(canvas, x, y, x + eighths(w, n), bottom, fg)
        }
        0x2590 => fill(canvas, x + eighths(w, 4), y, right, bottom, fg),
        0x2591..=0x2593 => {
            let share = f64::from(c - 0x2590) / 4.0;
            fill(canvas, x, y, right, bottom, bg.mix(fg, share))
        }
        0x2594 => fill(canvas, x, y, right, y + eighths(h, 1), fg),
        0x2595 => fill(canvas, x + eighths(w, 7), y, right, bottom, fg),
        _ => {
            let bits = QUADRANTS[(c - 0x2596) as usize];
            let (mx, my) = (x + eighths(w, 4), y + eighths(h, 4));
            if bits & 1 != 0 {
                fill(canvas, x, y, mx, my, fg)
            }
            if bits & 2 != 0 {
                fill(canvas, mx, y, right, my, fg)
            }
            if bits & 4 != 0 {
                fill(canvas, x, my, mx, bottom, fg)
            }
            if bits & 8 != 0 {
                fill(canvas, mx, my, right, bottom, fg)
            }
        }
    }
    true
}

// Sextants U+1FB00..U+1FB3B are the 2x3 patterns 1..=62 without 21 and 42,
// which are the left and right half blocks. Bit 0 is the top left sixth,
// bit 1 the top right, and so on down the rows.
fn sextant(canvas: &mut Canvas, index: u32, r: Rect, fg: Color) -> bool {
    let mut pattern = index + 1;
    if pattern >= 21 {
        pattern += 1
    }
    if pattern >= 42 {
        pattern += 1
    }
    let Rect {
        x,
        y,
        width: w,
        height: h,
    } = r;
    let columns = [x, x + eighths(w, 4), x + w];
    let rows = [y, y + (h + 1) / 3, y + (2 * h + 1) / 3, y + h];
    for sixth in 0..6 {
        if pattern & (1 << sixth) != 0 {
            let (column, row) = (sixth % 2, sixth / 2);
            fill(
                canvas,
                columns[column],
                rows[row],
                columns[column + 1],
                rows[row + 1],
                fg,
            )
        }
    }
    true
}

// The weight of each arm of U+2500..U+257F, in the order up, right, down,
// left: 0 none, 1 light, 2 heavy, 3 double. Dashed lines, arcs and
// diagonals are drawn by their own code and have no entry here.
const ARMS: [&str; 128] = [
    "0101", "0202", "1010", "2020", "", "", "", "", "", "", "", "", "0110", "0210", "0120", "0220",
    "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002",
    "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220", "1011", "1012", "2011", "1021",
    "2021", "2012", "1022", "2022", "0111", "0112", "0211", "0212", "0121", "0122", "0221", "0222",
    "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", "1111", "1112", "1211", "1212",
    "2111", "1121", "2121", "2112", "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222",
    "", "", "", "", "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", "1300", "3100",
    "3300", "1003", "3001", "3003", "1310", "3130", "3330", "1013", "3031", "3033", "0313", "0131",
    "0333", "1303", "3101", "3303", "1313", "3131", "3333", "", "", "", "", "", "", "", "0001",
    "1000", "0100", "0010", "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010",
];

// Line widths for a cell `width` pixels wide: light, heavy, and the distance
// of each line of a double from the center.
struct Pen {
    light: i64,
    heavy: i64,
    double: i64,
}

impl Pen {
    fn new(width: i64) -> Pen {
        let light = (width / 8).max(1);
        Pen {
            light,
            heavy: light * 2,
            double: light + light / 2,
        }
    }

    fn thickness(&self, weight: u8) -> i64 {
        if weight == 2 { self.heavy } else { self.light }
    }

    // How far past the center an arm of `weight` reaches across, so the arms
    // that meet it cover the joint.
    fn reach(&self, weight: u8) -> i64 {
        match weight {
            0 => 0,
            3 => self.double + (self.light + 1) / 2,
            w => (self.thickness(w) + 1) / 2,
        }
    }
}

fn box_drawing(canvas: &mut Canvas, c: u32, r: Rect, fg: Color) -> bool {
    let pen = Pen::new(r.width);
    match c {
        0x2504..=0x250B | 0x254C..=0x254F => dashed(canvas, c, r, &pen, fg),
        0x256D..=0x2570 => arc(canvas, c, r, &pen, fg),
        0x2571..=0x2573 => {
            let width = pen.light as f64;
            let (right, bottom) = (r.x + r.width, r.y + r.height);
            if c != 0x2572 {
                canvas.line_aa_w(right, r.y, r.x, bottom, width, fg)
            }
            if c != 0x2571 {
                canvas.line_aa_w(r.x, r.y, right, bottom, width, fg)
            }
        }
        _ => {
            let arms: Vec<u8> = ARMS[(c - 0x2500) as usize]
                .bytes()
                .map(|b| b - b'0')
                .collect();
            let [up, right, down, left] = arms[..] else {
                return false;
            };
            lines(canvas, r, &pen, [up, right, down, left], fg)
        }
    }
    true
}

// Where an arm's line starts past the center, toward its own end: `side` is
// the perpendicular arm on this line's side of the arm and `other` the one
// across. A double line meets a double perpendicular at its near line or
// reaches round to its far one, and runs to a single one's edge.
fn double_start(pen: &Pen, side: u8, other: u8) -> i64 {
    if side == 3 {
        pen.double
    } else if other == 3 {
        -pen.double
    } else if side != 0 || other != 0 {
        -(pen.thickness(side.max(other)) / 2)
    } else {
        0
    }
}

fn lines(canvas: &mut Canvas, r: Rect, pen: &Pen, arms: [u8; 4], fg: Color) {
    let [up, right, down, left] = arms;
    let Rect {
        x,
        y,
        width: w,
        height: h,
    } = r;
    let (mx, my) = (x + w / 2, y + h / 2);
    let (x_end, y_end) = (x + w, y + h);
    let across_vertical = pen.reach(up).max(pen.reach(down));
    let across_horizontal = pen.reach(left).max(pen.reach(right));
    // A horizontal band of `t` pixels centered on `at`, and a vertical one.
    let band_h = |canvas: &mut Canvas, x0: i64, x1: i64, at: i64, t: i64| {
        fill(canvas, x0, at - t / 2, x1, at - t / 2 + t, fg)
    };
    for (weight, toward_right) in [(left, false), (right, true)] {
        match weight {
            0 => (),
            3 => {
                for (offset, side, other) in [(-pen.double, up, down), (pen.double, down, up)] {
                    let start = double_start(pen, side, other);
                    let (x0, x1) = if toward_right {
                        (mx + start, x_end)
                    } else {
                        (x, mx - start)
                    };
                    band_h(canvas, x0, x1, my + offset, pen.light)
                }
            }
            _ => {
                let (x0, x1) = if toward_right {
                    (mx - across_vertical, x_end)
                } else {
                    (x, mx + across_vertical)
                };
                band_h(canvas, x0, x1, my, pen.thickness(weight))
            }
        }
    }
    let band_v = |canvas: &mut Canvas, y0: i64, y1: i64, at: i64, t: i64| {
        fill(canvas, at - t / 2, y0, at - t / 2 + t, y1, fg)
    };
    for (weight, toward_down) in [(up, false), (down, true)] {
        match weight {
            0 => (),
            3 => {
                for (offset, side, other) in [(-pen.double, left, right), (pen.double, right, left)]
                {
                    let start = double_start(pen, side, other);
                    let (y0, y1) = if toward_down {
                        (my + start, y_end)
                    } else {
                        (y, my - start)
                    };
                    band_v(canvas, y0, y1, mx + offset, pen.light)
                }
            }
            _ => {
                let (y0, y1) = if toward_down {
                    (my - across_horizontal, y_end)
                } else {
                    (y, my + across_horizontal)
                };
                band_v(canvas, y0, y1, mx, pen.thickness(weight))
            }
        }
    }
}

fn dashed(canvas: &mut Canvas, c: u32, r: Rect, pen: &Pen, fg: Color) {
    // Light and heavy alternate through each run of four.
    let (count, base) = match c {
        0x2504..=0x2507 => (3, 0x2504),
        0x2508..=0x250B => (4, 0x2508),
        _ => (2, 0x254C),
    };
    let variant = c - base;
    let horizontal = variant < 2;
    let t = if variant % 2 == 1 {
        pen.heavy
    } else {
        pen.light
    };
    let (mx, my) = (r.x + r.width / 2, r.y + r.height / 2);
    let length = if horizontal { r.width } else { r.height };
    let gap = (length / count / 4).max(1);
    for i in 0..count {
        let a = i * length / count + gap / 2;
        let b = (i + 1) * length / count - (gap - gap / 2);
        if horizontal {
            fill(canvas, r.x + a, my - t / 2, r.x + b, my - t / 2 + t, fg)
        } else {
            fill(canvas, mx - t / 2, r.y + a, mx - t / 2 + t, r.y + b, fg)
        }
    }
}

// The rounded corners U+256D..U+2570: a quarter circle joining the middle
// of two edges, straight where the cell is longer than the circle is wide.
fn arc(canvas: &mut Canvas, c: u32, r: Rect, pen: &Pen, fg: Color) {
    let Rect {
        x,
        y,
        width: w,
        height: h,
    } = r;
    let (mx, my) = (x + w / 2, y + h / 2);
    let radius = (w - w / 2).min(h - h / 2);
    // Whether the corner opens to the right and downward.
    let (rightward, downward) = match c {
        0x256D => (true, true),
        0x256E => (false, true),
        0x256F => (false, false),
        _ => (true, false),
    };
    let sx = if rightward { 1 } else { -1 };
    let sy = if downward { 1 } else { -1 };
    let (cx, cy) = (mx + sx * radius, my + sy * radius);
    let width = pen.light as f64;
    let edge_y = if downward { y + h } else { y };
    let edge_x = if rightward { x + w } else { x };
    canvas.line_aa_w(mx, edge_y, mx, cy, width, fg);
    canvas.line_aa_w(cx, my, edge_x, my, width, fg);
    let steps = 12;
    let point = |i: i64| {
        let angle = std::f64::consts::FRAC_PI_2 * i as f64 / steps as f64;
        (
            cx - sx * (radius as f64 * angle.cos()).round() as i64,
            cy - sy * (radius as f64 * angle.sin()).round() as i64,
        )
    };
    for i in 0..steps {
        let (x0, y0) = point(i);
        let (x1, y1) = point(i + 1);
        canvas.line_aa_w(x0, y0, x1, y1, width, fg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    const BG: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    // The red channel of each pixel of a `w` by `h` canvas after drawing `c`
    // white over black into the whole of it.
    fn drawn(c: char, w: i64, h: i64) -> Vec<Vec<u8>> {
        let mut canvas = Canvas::new(w, h);
        canvas.fill(BG);
        assert!(draw(
            &mut canvas,
            &c.to_string(),
            Rect::new(0, 0, w, h),
            FG,
            BG
        ));
        let rgba = canvas.read_rect(Rect::new(0, 0, w, h));
        (0..h as usize)
            .map(|row| {
                (0..w as usize)
                    .map(|col| rgba[(row * w as usize + col) * 4])
                    .collect()
            })
            .collect()
    }

    #[test]
    fn half_blocks_fill_exactly_their_half_of_a_cell_of_any_height() {
        for h in [16, 17, 19] {
            let upper = drawn('▀', 8, h);
            for (row, pixels) in upper.iter().enumerate() {
                let lit = row < ((h * 4 + 4) / 8) as usize;
                assert!(
                    pixels.iter().all(|&p| (p == 255) == lit),
                    "row {row} of {h}"
                );
            }
            let lower = drawn('▄', 8, h);
            for (row, pixels) in lower.iter().enumerate() {
                assert_eq!(pixels[0] == 255, upper[row][0] != 255, "row {row} of {h}");
            }
        }
    }

    #[test]
    fn eighths_quadrants_and_shades_cover_their_parts() {
        let left = drawn('▊', 8, 16);
        assert!(left.iter().all(|row| row[..6].iter().all(|&p| p == 255)));
        assert!(left.iter().all(|row| row[6..].iter().all(|&p| p == 0)));
        let quadrant = drawn('▚', 8, 16);
        assert_eq!(
            (
                quadrant[0][0],
                quadrant[0][7],
                quadrant[15][0],
                quadrant[15][7]
            ),
            (255, 0, 0, 255)
        );
        let shade = drawn('▒', 4, 4)[2][2];
        assert!((120..=135).contains(&shade), "half shade drew {shade}");
        assert!(!draw(
            &mut Canvas::new(4, 4),
            "a",
            Rect::new(0, 0, 4, 4),
            FG,
            BG
        ));
    }

    #[test]
    fn box_lines_reach_every_edge_they_name_and_join_at_the_center() {
        let cross = drawn('┼', 9, 18);
        let (mx, my) = (4, 9);
        assert_eq!(cross[my][0], 255);
        assert_eq!(cross[my][8], 255);
        assert_eq!(cross[0][mx], 255);
        assert_eq!(cross[17][mx], 255);
        let corner = drawn('┘', 9, 18);
        assert_eq!(corner[my][0], 255);
        assert_eq!(corner[0][mx], 255);
        assert_eq!(corner[my][mx], 255);
        assert_eq!(corner[my][8], 0);
        assert_eq!(corner[17][mx], 0);
        let half = drawn('╴', 9, 18);
        assert_eq!(half[my][0], 255);
        assert_eq!(half[my][8], 0);
    }

    #[test]
    fn double_lines_run_as_two_and_a_double_corner_closes() {
        let pen = Pen::new(16);
        let double = drawn('═', 16, 32);
        let (mx, my) = (8, 16);
        assert_eq!(double[(my as i64 - pen.double) as usize][0], 255);
        assert_eq!(double[(my as i64 + pen.double) as usize][15], 255);
        assert_eq!(double[my][mx], 0);
        let corner = drawn('╔', 16, 32);
        // The outer lines meet at the top left of the joint.
        let outer = (
            (my as i64 - pen.double) as usize,
            (mx as i64 - pen.double) as usize,
        );
        assert_eq!(corner[outer.0][outer.1], 255);
        assert_eq!(corner[outer.0][15], 255);
        assert_eq!(corner[31][outer.1], 255);
    }

    #[test]
    fn blocks_box_lines_and_sextants_are_shapes_and_letters_are_not() {
        assert!(is_shape("▄"));
        assert!(is_shape("─"));
        assert!(is_shape("\u{1FB3B}"));
        assert!(!is_shape("a"));
        assert!(!is_shape("▄▄"));
    }

    #[test]
    fn sextants_skip_the_half_blocks() {
        // U+1FB00 is the top left sixth alone.
        let first = drawn('\u{1FB00}', 8, 18);
        assert_eq!((first[0][0], first[0][7], first[9][0]), (255, 0, 0));
        // U+1FB14 follows pattern 20 with 22 (top right, middle left, bottom
        // left): 21, the left half block, is skipped.
        let skipped = drawn('\u{1FB14}', 8, 18);
        assert_eq!(
            (skipped[0][0], skipped[0][7], skipped[9][0], skipped[17][0]),
            (0, 255, 255, 255)
        );
    }
}
