// arrange: the cells `Effect::Arrange` gives windows laid out over a display.
// Cells tile the area exactly: integer edges, no overlap, and `gap` units
// between neighbours and around the area's edge.

use windowing::Rect;

use crate::app::effects::{Arrangement, Edge};

// The share a main window takes is kept where every window stays usable.
const SHARE_MIN: f64 = 0.1;
const SHARE_MAX: f64 = 0.9;

/// The cell of each of `count` windows, in order, laid out by `arrangement`
/// over `area` with `gap` units around and between them. The first window
/// takes the main cell of an arrangement that has one.
pub fn cells(area: Rect, count: usize, arrangement: Arrangement, gap: i64) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let gap = gap.clamp(0, (area.width.min(area.height) / 4).max(0));
    let inner = Rect {
        x: area.x + gap,
        y: area.y + gap,
        width: (area.width - 2 * gap).max(1),
        height: (area.height - 2 * gap).max(1),
    };
    match arrangement {
        Arrangement::Grid(columns) => {
            let columns = if columns > 0 {
                (columns as usize).min(count)
            } else {
                even_columns(count)
            };
            grid(inner, count, columns, gap)
        }
        Arrangement::Columns => grid(inner, count, count, gap),
        Arrangement::Rows => grid(inner, count, 1, gap),
        Arrangement::Main(edge, share) => main(inner, count, edge, share, gap),
        Arrangement::Centered(share) if count < 3 => main(inner, count, Edge::Left, share, gap),
        Arrangement::Centered(share) => centered(inner, count, share, gap),
    }
}

// The columns of the most even grid for `count` windows: as many as rows,
// or one more.
fn even_columns(count: usize) -> usize {
    let mut columns = 1;
    while columns * columns < count {
        columns += 1;
    }
    columns
}

// `len` units from `start` cut into `parts` runs with `gap` between them;
// the first runs take the remainder a unit each.
fn split(start: i64, len: i64, parts: usize, gap: i64) -> Vec<(i64, i64)> {
    let n = parts.max(1) as i64;
    let usable = (len - gap * (n - 1)).max(n);
    let (base, extra) = (usable / n, usable % n);
    let mut at = start;
    (0..n)
        .map(|i| {
            let size = base + i64::from(i < extra);
            let run = (at, size);
            at += size + gap;
            run
        })
        .collect()
}

// Rows of `columns` cells, the last row's windows sharing its whole width.
fn grid(area: Rect, count: usize, columns: usize, gap: i64) -> Vec<Rect> {
    let columns = columns.clamp(1, count);
    let rows = count.div_ceil(columns);
    let bands = split(area.y, area.height, rows, gap);
    let mut out = Vec::with_capacity(count);
    for (row, (y, height)) in bands.into_iter().enumerate() {
        let here = (count - row * columns).min(columns);
        for (x, width) in split(area.x, area.width, here, gap) {
            out.push(Rect {
                x,
                y,
                width,
                height,
            });
        }
    }
    out
}

// The main window along `edge`, `share` of the way across; the others split
// the rest side by side across the edge's direction.
fn main(area: Rect, count: usize, edge: Edge, share: f64, gap: i64) -> Vec<Rect> {
    if count == 1 {
        return vec![area];
    }
    let share = share.clamp(SHARE_MIN, SHARE_MAX);
    let across = match edge {
        Edge::Left | Edge::Right => area.width,
        Edge::Top | Edge::Bottom => area.height,
    };
    let first = (((across - gap) as f64) * share).round() as i64;
    let rest = across - gap - first;
    let (lead, tail) = match edge {
        Edge::Left | Edge::Top => (0, first + gap),
        Edge::Right | Edge::Bottom => (rest + gap, 0),
    };
    let others = count - 1;
    let mut out = Vec::with_capacity(count);
    match edge {
        Edge::Left | Edge::Right => {
            out.push(Rect {
                x: area.x + lead,
                y: area.y,
                width: first,
                height: area.height,
            });
            for (y, height) in split(area.y, area.height, others, gap) {
                out.push(Rect {
                    x: area.x + tail,
                    y,
                    width: rest,
                    height,
                });
            }
        }
        Edge::Top | Edge::Bottom => {
            out.push(Rect {
                x: area.x,
                y: area.y + lead,
                width: area.width,
                height: first,
            });
            for (x, width) in split(area.x, area.width, others, gap) {
                out.push(Rect {
                    x,
                    y: area.y + tail,
                    width,
                    height: rest,
                });
            }
        }
    }
    out
}

// The main window in the middle `share` of the width; the others alternate
// into stacks on its left and right, the left taking the first.
fn centered(area: Rect, count: usize, share: f64, gap: i64) -> Vec<Rect> {
    let share = share.clamp(SHARE_MIN, SHARE_MAX);
    let middle = (((area.width - 2 * gap) as f64) * share).round() as i64;
    let sides = area.width - 2 * gap - middle;
    let left_width = sides / 2 + sides % 2;
    let right_width = sides / 2;
    let others = count - 1;
    let (on_left, on_right) = (others.div_ceil(2), others / 2);
    let left = split(area.y, area.height, on_left, gap);
    let right = split(area.y, area.height, on_right, gap);
    let mut out = Vec::with_capacity(count);
    out.push(Rect {
        x: area.x + left_width + gap,
        y: area.y,
        width: middle,
        height: area.height,
    });
    for i in 0..others {
        let (x, width, (y, height)) = if i % 2 == 0 {
            (area.x, left_width, left[i / 2])
        } else {
            (
                area.x + left_width + gap + middle + gap,
                right_width,
                right[i / 2],
            )
        };
        out.push(Rect {
            x,
            y,
            width,
            height,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 30,
        width: 1200,
        height: 800,
    };

    fn rect(x: i64, y: i64, width: i64, height: i64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    // Every cell lies inside the area and no two overlap.
    fn tiles(area: Rect, cells: &[Rect]) -> bool {
        let inside = cells.iter().all(|c| {
            c.width > 0
                && c.height > 0
                && c.x >= area.x
                && c.y >= area.y
                && c.x + c.width <= area.x + area.width
                && c.y + c.height <= area.y + area.height
        });
        let apart = cells.iter().enumerate().all(|(i, a)| {
            cells[i + 1..].iter().all(|b| {
                a.x + a.width <= b.x
                    || b.x + b.width <= a.x
                    || a.y + a.height <= b.y
                    || b.y + b.height <= a.y
            })
        });
        inside && apart
    }

    fn covered(cells: &[Rect]) -> i64 {
        cells.iter().map(|c| c.width * c.height).sum()
    }

    #[test]
    fn one_window_fills_the_area_in_every_arrangement() {
        for a in [
            Arrangement::Grid(0),
            Arrangement::Grid(3),
            Arrangement::Columns,
            Arrangement::Rows,
            Arrangement::Main(Edge::Right, 0.6),
            Arrangement::Centered(0.5),
        ] {
            assert_eq!(cells(AREA, 1, a, 0), vec![AREA]);
        }
    }

    #[test]
    fn four_windows_make_two_by_two() {
        let got = cells(AREA, 4, Arrangement::Grid(0), 0);
        assert_eq!(
            got,
            vec![
                rect(0, 30, 600, 400),
                rect(600, 30, 600, 400),
                rect(0, 430, 600, 400),
                rect(600, 430, 600, 400),
            ]
        );
    }

    #[test]
    fn a_short_last_row_shares_the_whole_width() {
        let got = cells(AREA, 3, Arrangement::Grid(0), 0);
        assert_eq!(got[2], rect(0, 430, 1200, 400));
        let five = cells(AREA, 5, Arrangement::Grid(0), 0);
        assert_eq!(five[3].width + five[4].width, 1200);
        assert_eq!(covered(&five), 1200 * 800);
    }

    #[test]
    fn a_fixed_column_count_wraps_into_rows() {
        let got = cells(AREA, 7, Arrangement::Grid(3), 0);
        assert_eq!(got.iter().filter(|c| c.y == 30).count(), 3);
        assert_eq!(got[6].width, 1200);
        assert!(tiles(AREA, &got));
        // Fewer windows than columns make one row of them.
        assert_eq!(cells(AREA, 2, Arrangement::Grid(3), 0)[1].width, 600);
    }

    #[test]
    fn columns_and_rows_split_one_way() {
        let columns = cells(AREA, 3, Arrangement::Columns, 0);
        assert!(columns.iter().all(|c| c.height == 800));
        assert_eq!(
            columns.iter().map(|c| c.width).collect::<Vec<_>>(),
            [400, 400, 400]
        );
        let rows = cells(AREA, 3, Arrangement::Rows, 0);
        assert!(rows.iter().all(|c| c.width == 1200));
        assert_eq!(rows.iter().map(|c| c.height).sum::<i64>(), 800);
    }

    #[test]
    fn the_main_window_takes_its_edge_and_share() {
        let left = cells(AREA, 3, Arrangement::Main(Edge::Left, 0.6), 0);
        assert_eq!(left[0], rect(0, 30, 720, 800));
        assert_eq!(left[1], rect(720, 30, 480, 400));
        assert_eq!(left[2], rect(720, 430, 480, 400));
        let right = cells(AREA, 3, Arrangement::Main(Edge::Right, 0.6), 0);
        assert_eq!(right[0], rect(480, 30, 720, 800));
        assert_eq!(right[1].x, 0);
        let top = cells(AREA, 3, Arrangement::Main(Edge::Top, 0.5), 0);
        assert_eq!(top[0], rect(0, 30, 1200, 400));
        assert_eq!(top[1], rect(0, 430, 600, 400));
        let bottom = cells(AREA, 3, Arrangement::Main(Edge::Bottom, 0.5), 0);
        assert_eq!(bottom[0], rect(0, 430, 1200, 400));
        assert_eq!(bottom[2], rect(600, 30, 600, 400));
    }

    #[test]
    fn a_centered_main_window_has_stacks_on_both_sides() {
        let got = cells(AREA, 4, Arrangement::Centered(0.5), 0);
        assert_eq!(got[0], rect(300, 30, 600, 800));
        assert_eq!(got[1], rect(0, 30, 300, 400));
        assert_eq!(got[2], rect(900, 30, 300, 800));
        assert_eq!(got[3], rect(0, 430, 300, 400));
        assert_eq!(covered(&got), 1200 * 800);
        // Two windows have no second side, so the main one leads on the left.
        let two = cells(AREA, 2, Arrangement::Centered(0.5), 0);
        assert_eq!(two[0].x, 0);
    }

    #[test]
    fn gaps_separate_cells_and_frame_the_area() {
        for a in [
            Arrangement::Grid(0),
            Arrangement::Columns,
            Arrangement::Rows,
            Arrangement::Main(Edge::Bottom, 0.7),
            Arrangement::Centered(0.4),
        ] {
            for n in 1..=9 {
                let got = cells(AREA, n, a, 8);
                assert_eq!(got.len(), n);
                let inner = rect(8, 38, 1184, 784);
                assert!(tiles(inner, &got), "{a:?} with {n} windows");
            }
        }
    }

    #[test]
    fn cells_cover_the_area_exactly_without_gaps() {
        for a in [
            Arrangement::Grid(0),
            Arrangement::Grid(2),
            Arrangement::Columns,
            Arrangement::Rows,
            Arrangement::Main(Edge::Left, 0.65),
            Arrangement::Centered(0.5),
        ] {
            for n in 1..=12 {
                let area = rect(5, 7, 1001, 777);
                let got = cells(area, n, a, 0);
                assert!(tiles(area, &got), "{a:?} with {n} windows");
                assert_eq!(covered(&got), 1001 * 777, "{a:?} with {n} windows");
            }
        }
    }

    #[test]
    fn a_share_out_of_range_is_kept_usable() {
        let got = cells(AREA, 2, Arrangement::Main(Edge::Left, 5.0), 0);
        assert_eq!(got[0].width, 1080);
        assert!(got[1].width > 0);
    }

    #[test]
    fn no_windows_have_no_cells() {
        assert!(cells(AREA, 0, Arrangement::Grid(0), 4).is_empty());
    }
}
