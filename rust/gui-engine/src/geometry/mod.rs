// geometry: integer pixel rectangles, sizes, insets, and the layout
// constraints that flow from parents to children. Layout and painting
// work in framebuffer pixels; interface units convert to pixels through
// the window's theme (`Theme::px`), so fractional display scales are
// resolved in one place and edges snap to whole pixels.

/// A distance meaning "no limit" in a constraint.
pub const UNBOUNDED: i64 = 1 << 40;

/// A width and a height, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Size {
    pub width: i64,
    pub height: i64,
}

impl Size {
    pub fn new(width: i64, height: i64) -> Size {
        Size { width, height }
    }

    pub fn zero() -> Size {
        Size {
            width: 0,
            height: 0,
        }
    }

    /// This size grown by `padding` on every side.
    pub fn padded(&self, padding: i64) -> Size {
        Size {
            width: self.width + padding * 2,
            height: self.height + padding * 2,
        }
    }

    /// This size grown by `insets`.
    pub fn inflate(&self, insets: Insets) -> Size {
        Size {
            width: self.width + insets.left + insets.right,
            height: self.height + insets.top + insets.bottom,
        }
    }
}

/// A point in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Point {
    pub x: i64,
    pub y: i64,
}

impl Point {
    pub fn new(x: i64, y: i64) -> Point {
        Point { x, y }
    }
}

/// Edge distances: padding, margins, borders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Insets {
    pub left: i64,
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
}

impl Insets {
    pub fn all(v: i64) -> Insets {
        Insets {
            left: v,
            top: v,
            right: v,
            bottom: v,
        }
    }

    pub fn symmetric(horizontal: i64, vertical: i64) -> Insets {
        Insets {
            left: horizontal,
            top: vertical,
            right: horizontal,
            bottom: vertical,
        }
    }

    pub fn new(left: i64, top: i64, right: i64, bottom: i64) -> Insets {
        Insets {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn horizontal(&self) -> i64 {
        self.left + self.right
    }

    pub fn vertical(&self) -> i64 {
        self.top + self.bottom
    }

    /// These insets with left and right exchanged, for right-to-left layout.
    pub fn mirrored(&self) -> Insets {
        Insets {
            left: self.right,
            top: self.top,
            right: self.left,
            bottom: self.bottom,
        }
    }
}

/// A rectangle in pixels: `x`, `y` from the top left, then width, height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    pub fn new(x: i64, y: i64, width: i64, height: i64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn from_size(size: Size) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        }
    }

    pub fn right(&self) -> i64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i64 {
        self.y + self.height
    }

    pub fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }

    pub fn center_x(&self) -> i64 {
        self.x + self.width / 2
    }

    pub fn center_y(&self) -> i64 {
        self.y + self.height / 2
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// The rectangle `by` pixels smaller on every side.
    pub fn inset(&self, by: i64) -> Rect {
        Rect {
            x: self.x + by,
            y: self.y + by,
            width: (self.width - by * 2).max(0),
            height: (self.height - by * 2).max(0),
        }
    }

    /// The rectangle shrunk by `insets`.
    pub fn deflate(&self, insets: Insets) -> Rect {
        Rect {
            x: self.x + insets.left,
            y: self.y + insets.top,
            width: (self.width - insets.left - insets.right).max(0),
            height: (self.height - insets.top - insets.bottom).max(0),
        }
    }

    /// The rectangle moved by (`dx`, `dy`).
    pub fn offset(&self, dx: i64, dy: i64) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            width: self.width,
            height: self.height,
        }
    }

    /// Whether the point (`x`, `y`) lies inside (right and bottom exclusive).
    pub fn contains(&self, x: i64, y: i64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// The overlap of two rectangles (empty when they do not meet).
    pub fn intersect(&self, other: Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        Rect {
            x: x0,
            y: y0,
            width: (x1 - x0).max(0),
            height: (y1 - y0).max(0),
        }
    }

    /// The smallest rectangle holding both (an empty side is ignored).
    pub fn union(&self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        };
        if other.is_empty() {
            return *self;
        };
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    pub fn intersects(&self, other: Rect) -> bool {
        !self.intersect(other).is_empty()
    }

    /// This rectangle mirrored inside `within`, for right-to-left layout.
    pub fn mirror_in(&self, within: Rect) -> Rect {
        Rect {
            x: within.x + within.right() - self.right(),
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

/// The sizes a parent allows a child: inclusive minimums and maximums,
/// `UNBOUNDED` when an axis has no limit (inside a scroll area).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Constraints {
    pub min_width: i64,
    pub max_width: i64,
    pub min_height: i64,
    pub max_height: i64,
}

impl Constraints {
    pub fn new(min_width: i64, max_width: i64, min_height: i64, max_height: i64) -> Constraints {
        Constraints {
            min_width,
            max_width,
            min_height,
            max_height,
        }
    }

    /// Exactly `size`.
    pub fn tight(size: Size) -> Constraints {
        Constraints {
            min_width: size.width,
            max_width: size.width,
            min_height: size.height,
            max_height: size.height,
        }
    }

    /// Anything from zero up to `size`.
    pub fn loose(size: Size) -> Constraints {
        Constraints {
            min_width: 0,
            max_width: size.width,
            min_height: 0,
            max_height: size.height,
        }
    }

    /// No limit on either axis.
    pub fn unbounded() -> Constraints {
        Constraints {
            min_width: 0,
            max_width: UNBOUNDED,
            min_height: 0,
            max_height: UNBOUNDED,
        }
    }

    pub fn has_bounded_width(&self) -> bool {
        self.max_width < UNBOUNDED
    }

    pub fn has_bounded_height(&self) -> bool {
        self.max_height < UNBOUNDED
    }

    /// `size` clamped into these constraints.
    pub fn constrain(&self, size: Size) -> Size {
        Size {
            width: size.width.max(self.min_width).min(self.max_width).max(0),
            height: size.height.max(self.min_height).min(self.max_height).max(0),
        }
    }

    /// These constraints with the minimums dropped.
    pub fn loosen(&self) -> Constraints {
        Constraints {
            min_width: 0,
            max_width: self.max_width,
            min_height: 0,
            max_height: self.max_height,
        }
    }

    /// These constraints shrunk by `insets` on both bounds.
    pub fn deflate(&self, insets: Insets) -> Constraints {
        let h = insets.horizontal();
        let v = insets.vertical();
        Constraints {
            min_width: (self.min_width - h).max(0),
            max_width: if self.max_width >= UNBOUNDED {
                UNBOUNDED
            } else {
                (self.max_width - h).max(0)
            },
            min_height: (self.min_height - v).max(0),
            max_height: if self.max_height >= UNBOUNDED {
                UNBOUNDED
            } else {
                (self.max_height - v).max(0)
            },
        }
    }

    /// Whether the constraints describe at least one size.
    pub fn is_valid(&self) -> bool {
        self.min_width >= 0
            && self.min_height >= 0
            && self.min_width <= self.max_width
            && self.min_height <= self.max_height
    }

    /// The largest size allowed, with unbounded axes replaced by `fallback`.
    pub fn biggest(&self, fallback: Size) -> Size {
        Size {
            width: if self.max_width >= UNBOUNDED {
                fallback.width.max(self.min_width)
            } else {
                self.max_width
            },
            height: if self.max_height >= UNBOUNDED {
                fallback.height.max(self.min_height)
            } else {
                self.max_height
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Constraints, Insets, Rect, Size, UNBOUNDED};

    #[test]
    fn inset_shrinks_every_side() {
        assert_eq!(
            Rect::new(10, 20, 50, 30).inset(4),
            Rect::new(14, 24, 42, 22)
        );
        assert_eq!(Rect::new(0, 0, 4, 4).inset(10).width, 0)
    }

    #[test]
    fn contains_is_top_left_inclusive_right_bottom_exclusive() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(r.contains(0, 0));
        assert!(r.contains(9, 9));
        assert!(!r.contains(10, 5));
        assert!(!r.contains(5, -1))
    }

    #[test]
    fn intersect_and_union() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(b), Rect::new(0, 0, 15, 15));
        assert!(a.intersect(Rect::new(20, 20, 1, 1)).is_empty())
    }

    #[test]
    fn constraints_clamp_and_deflate() {
        let c = Constraints {
            min_width: 10,
            max_width: 100,
            min_height: 0,
            max_height: UNBOUNDED,
        };
        assert_eq!(c.constrain(Size::new(5, 7)), Size::new(10, 7));
        assert_eq!(c.constrain(Size::new(500, 7)), Size::new(100, 7));
        let d = c.deflate(Insets::all(4));
        assert_eq!(d.max_width, 92);
        assert_eq!(d.max_height, UNBOUNDED);
        assert!(
            !Constraints {
                min_width: 5,
                max_width: 4,
                min_height: 0,
                max_height: 0
            }
            .is_valid()
        )
    }

    #[test]
    fn mirroring_reflects_within_the_parent() {
        let parent = Rect::new(0, 0, 100, 10);
        assert_eq!(
            Rect::new(10, 0, 20, 10).mirror_in(parent),
            Rect::new(70, 0, 20, 10)
        );
        assert_eq!(Insets::new(1, 2, 3, 4).mirrored(), Insets::new(3, 2, 1, 4))
    }
}
