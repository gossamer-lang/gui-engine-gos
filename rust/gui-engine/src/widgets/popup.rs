// popup: placement of popups (menus, lists, tooltips, pickers) inside the
// window. Popups are drawn in the window's own overlay layer, so placement
// never depends on a backend's absolute desktop positioning: a popup
// prefers its requested side, flips when that side lacks room, shrinks to
// fit when neither side has room, and always ends inside the window.

use crate::geometry::{Rect, Size};
use crate::widgets::cx::Placement;

/// Where a popup of `size` goes for `anchor` inside `window` (all pixels).
pub fn place(anchor: Rect, size: Size, window: Rect, placement: Placement, rtl: bool) -> Rect {
    let w = size.width.min(window.width).max(1);
    let mut h = size.height.min(window.height).max(1);
    match placement {
        Placement::Center => {
            return Rect::new(
                window.x + (window.width - w) / 2,
                window.y + (window.height - h) / 2,
                w,
                h,
            );
        }
        Placement::AtPoint => {
            let mut x = if rtl { anchor.x - w } else { anchor.x };
            let mut y = anchor.y;
            if x + w > window.right() {
                x = window.right() - w
            };
            if x < window.x {
                x = window.x
            };
            if y + h > window.bottom() {
                y = (anchor.y - h).max(window.y)
            };
            if y + h > window.bottom() {
                h = window.bottom() - y
            };
            return Rect::new(x, y, w, h);
        }
        Placement::Beside => {
            let right_room = window.right() - anchor.right();
            let left_room = anchor.x - window.x;
            let mut x = if rtl {
                if left_room >= w || left_room >= right_room {
                    anchor.x - w
                } else {
                    anchor.right()
                }
            } else if right_room >= w || right_room >= left_room {
                anchor.right()
            } else {
                anchor.x - w
            };
            x = x.max(window.x).min(window.right() - w);
            let mut y = anchor.y;
            if y + h > window.bottom() {
                y = (window.bottom() - h).max(window.y)
            };
            return Rect::new(x, y, w, h);
        }
        _ => (),
    }
    let below = window.bottom() - anchor.bottom();
    let above = anchor.y - window.y;
    let prefer_below = placement == Placement::Below;
    let y = if (prefer_below && (below >= h || below >= above))
        || (!prefer_below && above < h && below > above)
    {
        if below < h {
            h = below.max(1)
        };
        anchor.bottom()
    } else {
        if above < h {
            h = above.max(1)
        };
        anchor.y - h
    };
    let mut x = if rtl { anchor.right() - w } else { anchor.x };
    if x + w > window.right() {
        x = window.right() - w
    };
    if x < window.x {
        x = window.x
    };
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::place;
    use crate::geometry::{Rect, Size};

    #[test]
    fn lists_open_below_and_flip_above_near_the_bottom() {
        let win = Rect::new(0, 0, 400, 300);
        let r = place(
            Rect::new(10, 20, 100, 30),
            Size::new(100, 120),
            win,
            crate::widgets::cx::Placement::Below,
            false,
        );
        assert_eq!(r, Rect::new(10, 50, 100, 120));
        let flipped = place(
            Rect::new(10, 250, 100, 30),
            Size::new(100, 120),
            win,
            crate::widgets::cx::Placement::Below,
            false,
        );
        assert_eq!(flipped.y, 130)
    }

    #[test]
    fn popups_stay_inside_and_shrink_when_needed() {
        let win = Rect::new(0, 0, 200, 100);
        let r = place(
            Rect::new(150, 40, 40, 20),
            Size::new(120, 400),
            win,
            crate::widgets::cx::Placement::Below,
            false,
        );
        assert!(r.right() <= 200);
        assert!(r.y >= 0 && r.bottom() <= 100)
    }

    #[test]
    fn submenus_open_beside_and_switch_sides() {
        let win = Rect::new(0, 0, 300, 300);
        let r = place(
            Rect::new(100, 10, 100, 30),
            Size::new(80, 60),
            win,
            crate::widgets::cx::Placement::Beside,
            false,
        );
        assert_eq!(r.x, 200);
        let left = place(
            Rect::new(200, 10, 90, 30),
            Size::new(80, 60),
            win,
            crate::widgets::cx::Placement::Beside,
            false,
        );
        assert_eq!(left.x, 120);
        let rtl = place(
            Rect::new(100, 10, 100, 30),
            Size::new(80, 60),
            win,
            crate::widgets::cx::Placement::Beside,
            true,
        );
        assert_eq!(rtl.x, 20)
    }
}
