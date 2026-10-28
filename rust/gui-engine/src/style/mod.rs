pub mod color;

// style: semantic color tokens, typography, shape, motion and icon tokens,
// density, text scale, and layout direction. Widgets ask the theme for a
// role ("the text on a selected row", "a control's height"), never for a
// literal, so a theme swap, a density change, or a 200% text size reaches
// every control. Resolution order for a widget: built-in defaults, the window's
// theme, a subtree override (`Widget::themed`), then the control's own
// interaction state.

use crate::style::color::Color;
use crate::text::fonts::Family;
use crate::text::layout::TextStyle;

/// Which palette a theme draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThemeKind {
    Light,
    Dark,
    HighContrastLight,
    HighContrastDark,
}

/// How tightly controls pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Density {
    Compact,
    Normal,
    Comfortable,
}

/// A text role; the theme maps each to a family, size, and weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextRole {
    Body,
    Small,
    Caption,
    Label,
    Heading,
    Title,
    Hero,
    Mono,
    /// Menu bar titles and menu entries, sized as the desktop's own menus.
    Menu,
}

/// One text role's face: its size in interface units and its weight.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Typeface {
    pub size: f64,
    pub bold: bool,
    pub mono: bool,
}

/// A theme's type ramp: one face per text role.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Typography {
    pub body: Typeface,
    pub small: Typeface,
    pub caption: Typeface,
    pub label: Typeface,
    pub heading: Typeface,
    pub title: Typeface,
    pub hero: Typeface,
    pub mono: Typeface,
    /// Body-sized until the desktop names its menu font size.
    #[serde(default = "Typography::standard_menu")]
    pub menu: Typeface,
}

impl Typography {
    /// The default ramp: 18-unit body text, 14 to 36 units across roles.
    pub fn standard() -> Typography {
        Typography {
            body: Typeface {
                size: 18.0,
                bold: false,
                mono: false,
            },
            small: Typeface {
                size: 16.0,
                bold: false,
                mono: false,
            },
            caption: Typeface {
                size: 14.0,
                bold: false,
                mono: false,
            },
            label: Typeface {
                size: 18.0,
                bold: true,
                mono: false,
            },
            heading: Typeface {
                size: 22.0,
                bold: true,
                mono: false,
            },
            title: Typeface {
                size: 28.0,
                bold: true,
                mono: false,
            },
            hero: Typeface {
                size: 36.0,
                bold: true,
                mono: false,
            },
            mono: Typeface {
                size: 16.0,
                bold: false,
                mono: true,
            },
            menu: Typography::standard_menu(),
        }
    }

    fn standard_menu() -> Typeface {
        Typeface {
            size: 18.0,
            bold: false,
            mono: false,
        }
    }

    /// This ramp with menus at `points`, the desktop's menu font size; a
    /// point is 96/72 interface units.
    pub fn with_menu_points(self, points: f64) -> Typography {
        let mut t = self;
        t.menu.size = points * 96.0 / 72.0;
        t
    }

    /// The face for `role`.
    pub fn face(&self, role: TextRole) -> Typeface {
        match role {
            TextRole::Body => self.body,
            TextRole::Small => self.small,
            TextRole::Caption => self.caption,
            TextRole::Label => self.label,
            TextRole::Heading => self.heading,
            TextRole::Title => self.title,
            TextRole::Hero => self.hero,
            TextRole::Mono => self.mono,
            TextRole::Menu => self.menu,
        }
    }
}

/// Size and shape tokens in interface units; heights and spacing are
/// density-adjusted when read.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Shape {
    /// Corner radius of controls, fields and panels.
    pub radius: i64,
    /// Height of a single-line control at normal density.
    pub control_height: i64,
    /// Height of one list or menu row at normal density.
    pub row_height: i64,
    /// Side of a checkbox or radio indicator.
    pub indicator: i64,
    /// One spacing step at normal density.
    pub space: i64,
    /// Width of control borders (high-contrast themes double it).
    pub border: i64,
    /// Width of the keyboard focus ring.
    pub focus: i64,
    /// A button's or list item's icon.
    pub icon: i64,
    /// Adornments inside a field: chevrons, steppers, search glasses.
    pub field_icon: i64,
    /// Icons in tabs, column headers and tree rows, and menu arrows.
    pub small_icon: i64,
    /// The close glyph of a tab or a token chip.
    pub close_icon: i64,
    /// An item's icon in an icon grid.
    pub grid_icon: i64,
    /// Drop shadows under tooltips, popups and dialogs.
    pub tooltip_elevation: Elevation,
    pub popup_elevation: Elevation,
    pub dialog_elevation: Elevation,
}

/// A drop shadow: how far it falls below its surface and how wide it blurs,
/// in interface units.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Elevation {
    pub offset: i64,
    pub blur: i64,
}

impl Shape {
    pub fn standard() -> Shape {
        Shape {
            radius: 7,
            control_height: 42,
            row_height: 44,
            indicator: 24,
            space: 4,
            border: 1,
            focus: 2,
            icon: 20,
            field_icon: 18,
            small_icon: 16,
            close_icon: 14,
            grid_icon: 48,
            tooltip_elevation: Elevation { offset: 1, blur: 3 },
            popup_elevation: Elevation { offset: 2, blur: 6 },
            dialog_elevation: Elevation {
                offset: 4,
                blur: 12,
            },
        }
    }
}

/// Timing tokens in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Motion {
    /// Each half of a text caret's blink.
    pub caret_blink: i64,
    /// Longest gap between the clicks of a double click.
    pub double_click: i64,
    /// How long the pointer rests on a control before its tooltip shows.
    pub tooltip_delay: i64,
}

impl Motion {
    pub fn standard() -> Motion {
        Motion {
            caret_blink: 530,
            double_click: 450,
            tooltip_delay: 600,
        }
    }
}

/// The non-color tokens an application sets as one: they stay when the
/// palette changes (`Theme::with_kind`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThemeTokens {
    pub typography: Typography,
    pub shape: Shape,
    pub motion: Motion,
    /// A set registered with `render::icons::register`, or "" for the
    /// built-in icons.
    pub icons: String,
}

impl ThemeTokens {
    pub fn standard() -> ThemeTokens {
        ThemeTokens {
            typography: Typography::standard(),
            shape: Shape::standard(),
            motion: Motion::standard(),
            icons: String::new(),
        }
    }
}

/// The tokens and metrics a window paints with.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Theme {
    pub kind: ThemeKind,
    /// Behind everything.
    pub background: Color,
    /// A raised surface: buttons, cards, menus.
    pub surface: Color,
    /// A second surface level: headers, sidebars, toolbars.
    pub surface_alt: Color,
    /// A sunken input: fields, list bodies.
    pub field: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_disabled: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub hover: Color,
    pub pressed: Color,
    pub selection: Color,
    pub selection_text: Color,
    /// The selection highlight while the window is inactive.
    pub selection_inactive: Color,
    pub danger: Color,
    pub danger_text: Color,
    pub success: Color,
    pub warning: Color,
    pub info: Color,
    pub border: Color,
    pub border_strong: Color,
    pub focus: Color,
    /// Translucent black under popups and dialogs.
    pub shadow: Color,
    pub scrim: Color,
    pub link: Color,
    /// Display content scale: pixels per interface unit.
    pub scale: f64,
    /// User text size multiplier (1.0 = 100%).
    pub text_scale: f64,
    pub density: Density,
    pub rtl: bool,
    pub reduced_motion: bool,
    pub typography: Typography,
    pub shape: Shape,
    pub motion: Motion,
    /// The registered icon set icons are drawn from first (see
    /// `render::icons::register`); "" draws the built-in set only.
    pub icons: String,
}

impl Theme {
    /// A light theme: near-black text on a warm off-white ground (about
    /// 15:1); muted text stays above 6:1.
    pub fn light() -> Theme {
        Theme {
            kind: ThemeKind::Light,
            background: Color::hex(0xF7F5F0),
            surface: Color::hex(0xE8E4DB),
            surface_alt: Color::hex(0xEFEBE3),
            field: Color::hex(0xFFFFFF),
            text: Color::hex(0x1A1918),
            text_muted: Color::hex(0x5F5B55),
            text_disabled: Color::hex(0xA8A39B),
            accent: Color::hex(0x0B5CAD),
            accent_text: Color::hex(0xFFFFFF),
            hover: Color::hex(0xDFDAD0),
            pressed: Color::hex(0xD2CCBF),
            selection: Color::hex(0x0B5CAD),
            selection_text: Color::hex(0xFFFFFF),
            selection_inactive: Color::hex(0xC9C4B8),
            danger: Color::hex(0xB3261E),
            danger_text: Color::hex(0xFFFFFF),
            success: Color::hex(0x1B6E3C),
            warning: Color::hex(0x9A5B00),
            info: Color::hex(0x0B5CAD),
            border: Color::hex(0xC9C4B8),
            border_strong: Color::hex(0x8E897F),
            focus: Color::hex(0xE07A08),
            shadow: Color::rgba(0, 0, 0, 60),
            scrim: Color::rgba(0, 0, 0, 90),
            link: Color::hex(0x0B5CAD),
            scale: 1.0,
            text_scale: 1.0,
            density: Density::Normal,
            rtl: false,
            reduced_motion: false,
            typography: Typography::standard(),
            shape: Shape::standard(),
            motion: Motion::standard(),
            icons: String::new(),
        }
    }

    /// A dark theme with the same token meanings: off-white text on a deep
    /// blue-gray ground (about 15:1); muted text stays above 7:1.
    pub fn dark() -> Theme {
        Theme {
            kind: ThemeKind::Dark,
            background: Color::hex(0x15171C),
            surface: Color::hex(0x23262E),
            surface_alt: Color::hex(0x1C1F25),
            field: Color::hex(0x0F1115),
            text: Color::hex(0xF2F0EA),
            text_muted: Color::hex(0xADA9A1),
            text_disabled: Color::hex(0x62605C),
            accent: Color::hex(0x2F6FE0),
            accent_text: Color::hex(0xFFFFFF),
            hover: Color::hex(0x2C303A),
            pressed: Color::hex(0x383D49),
            selection: Color::hex(0x2A4A6B),
            selection_text: Color::hex(0xF2F0EA),
            selection_inactive: Color::hex(0x30343D),
            danger: Color::hex(0xF28B82),
            danger_text: Color::hex(0x15171C),
            success: Color::hex(0x7AD19A),
            warning: Color::hex(0xF5C06B),
            info: Color::hex(0x8AB4F8),
            border: Color::hex(0x3C414C),
            border_strong: Color::hex(0x6B7180),
            focus: Color::hex(0xF5A83B),
            shadow: Color::rgba(0, 0, 0, 110),
            scrim: Color::rgba(0, 0, 0, 140),
            link: Color::hex(0x8AB4F8),
            scale: 1.0,
            text_scale: 1.0,
            density: Density::Normal,
            rtl: false,
            reduced_motion: false,
            typography: Typography::standard(),
            shape: Shape::standard(),
            motion: Motion::standard(),
            icons: String::new(),
        }
    }

    /// Black on white with solid borders; every text pair exceeds 7:1.
    pub fn high_contrast_light() -> Theme {
        let mut t = Theme::light();
        t.kind = ThemeKind::HighContrastLight;
        t.background = Color::white();
        t.surface = Color::white();
        t.surface_alt = Color::white();
        t.field = Color::white();
        t.text = Color::black();
        t.text_muted = Color::hex(0x202020);
        t.text_disabled = Color::hex(0x5A5A5A);
        t.accent = Color::hex(0x00007A);
        t.hover = Color::hex(0xD8D8FF);
        t.pressed = Color::hex(0xB0B0FF);
        t.selection = Color::hex(0x00007A);
        t.selection_inactive = Color::hex(0x404040);
        t.selection_text = Color::white();
        t.border = Color::black();
        t.border_strong = Color::black();
        t.focus = Color::hex(0xC00000);
        t.link = Color::hex(0x00007A);
        t
    }

    /// White on black with yellow focus; every text pair exceeds 7:1.
    pub fn high_contrast_dark() -> Theme {
        let mut t = Theme::dark();
        t.kind = ThemeKind::HighContrastDark;
        t.background = Color::black();
        t.surface = Color::black();
        t.surface_alt = Color::black();
        t.field = Color::black();
        t.text = Color::white();
        t.text_muted = Color::hex(0xE0E0E0);
        t.text_disabled = Color::hex(0xA0A0A0);
        t.accent = Color::hex(0x1AEBFF);
        t.accent_text = Color::black();
        t.hover = Color::hex(0x00393F);
        t.pressed = Color::hex(0x005860);
        t.selection = Color::hex(0x1AEBFF);
        t.selection_text = Color::black();
        t.selection_inactive = Color::hex(0x606060);
        t.border = Color::white();
        t.border_strong = Color::white();
        t.focus = Color::hex(0xFFFF00);
        t.link = Color::hex(0xFFFF00);
        t
    }

    /// The theme for `kind`, keeping this theme's scale, density, text
    /// scale, direction, motion settings, and typography, shape, motion and
    /// icon tokens: a kind is a palette.
    pub fn with_kind(&self, kind: ThemeKind) -> Theme {
        let mut t = match kind {
            ThemeKind::Light => Theme::light(),
            ThemeKind::Dark => Theme::dark(),
            ThemeKind::HighContrastLight => Theme::high_contrast_light(),
            ThemeKind::HighContrastDark => Theme::high_contrast_dark(),
        };
        t.scale = self.scale;
        t.text_scale = self.text_scale;
        t.density = self.density;
        t.rtl = self.rtl;
        t.reduced_motion = self.reduced_motion;
        t.typography = self.typography;
        t.shape = self.shape;
        t.motion = self.motion;
        t.icons = self.icons.clone();
        t
    }

    /// This theme's typography, shape, motion and icon tokens.
    pub fn tokens(&self) -> ThemeTokens {
        ThemeTokens {
            typography: self.typography,
            shape: self.shape,
            motion: self.motion,
            icons: self.icons.clone(),
        }
    }

    /// This theme with `tokens` in place of its own.
    pub fn with_tokens(&self, tokens: &ThemeTokens) -> Theme {
        let mut t = self.clone();
        t.typography = tokens.typography;
        t.shape = tokens.shape;
        t.motion = tokens.motion;
        t.icons = tokens.icons.clone();
        t
    }

    /// This palette with `accent` (a desktop's accent color) for its accent
    /// and selection. Text on them takes whichever of black and white reads
    /// better, and an accent too close to the background for a checkbox or a
    /// selected row to show is mixed toward the text color until it shows.
    pub fn with_accent(&self, accent: Color) -> Theme {
        let mut t = self.clone();
        let mut a = accent;
        let mut step = 1;
        while a.contrast(t.background) < 3.0 && step <= 10 {
            a = accent.mix(t.text, step as f64 / 10.0);
            step += 1;
        }
        let on = if a.contrast(Color::white()) >= a.contrast(Color::black()) {
            Color::white()
        } else {
            Color::black()
        };
        t.accent = a;
        t.accent_text = on;
        t.info = a;
        t.selection = if t.is_dark() {
            a.mix(t.background, 0.45)
        } else {
            a
        };
        t.selection_text = if t.selection.contrast(t.text) >= t.selection.contrast(on) {
            t.text
        } else {
            on
        };
        t
    }

    pub fn is_dark(&self) -> bool {
        self.kind == ThemeKind::Dark || self.kind == ThemeKind::HighContrastDark
    }

    pub fn is_high_contrast(&self) -> bool {
        self.kind == ThemeKind::HighContrastLight || self.kind == ThemeKind::HighContrastDark
    }

    /// `units` interface units in pixels at this theme's scale.
    pub fn px(&self, units: i64) -> i64 {
        let v = units as f64 * self.scale;
        let rounded = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
        rounded as i64
    }

    /// Fractional pixels for `units` interface units.
    /// Pixels for `units` of a length that holds text, such as a table
    /// column: it grows with the text size as well as the display scale.
    pub fn text_px(&self, units: i64) -> i64 {
        self.px((units as f64 * self.text_scale + 0.5) as i64)
    }

    pub fn pxf(&self, units: f64) -> f64 {
        units * self.scale
    }

    fn density_factor(&self) -> f64 {
        match self.density {
            Density::Compact => 0.8,
            Density::Normal => 1.0,
            Density::Comfortable => 1.2,
        }
    }

    /// Spacing step `n` (multiples of the shape's spacing step,
    /// density-adjusted), pixels.
    pub fn space(&self, n: i64) -> i64 {
        self.px(((n * self.shape.space) as f64 * self.density_factor() + 0.5) as i64)
    }

    /// Inner padding of a control, pixels.
    pub fn padding(&self) -> i64 {
        self.space(3)
    }

    /// Height of a single-line control (button, field), pixels; grows with
    /// the text scale so large text never clips.
    pub fn control_height(&self) -> i64 {
        let base = (self.shape.control_height as f64 * self.density_factor()) as i64;
        let (_, _, line) = crate::text::layout::metrics(&self.font(TextRole::Body));
        self.px(base).max(line as i64 + self.space(2))
    }

    /// Height of one list/menu row, pixels.
    pub fn row_height(&self) -> i64 {
        let base = (self.shape.row_height as f64 * self.density_factor()) as i64;
        let (_, _, line) = crate::text::layout::metrics(&self.font(TextRole::Body));
        self.px(base).max(line as i64 + self.space(2))
    }

    /// The (offset, blur) of a drop shadow at `e`, pixels.
    pub fn shadow_of(&self, e: Elevation) -> (i64, i64) {
        (self.px(e.offset), self.px(e.blur))
    }

    pub fn radius(&self) -> i64 {
        self.px(self.shape.radius)
    }

    pub fn border_width(&self) -> i64 {
        let units = if self.is_high_contrast() {
            self.shape.border * 2
        } else {
            self.shape.border
        };
        self.px(units).max(units)
    }

    pub fn focus_width(&self) -> i64 {
        self.px(self.shape.focus).max(self.shape.focus)
    }

    /// Side of a checkbox or radio indicator, pixels.
    pub fn indicator(&self) -> i64 {
        self.px(self.shape.indicator)
    }

    /// The text style for a role at this theme's scale and text size.
    pub fn font(&self, role: TextRole) -> TextStyle {
        let face = self.typography.face(role);
        let size = face.size * self.scale * self.text_scale;
        let mut style = if face.mono {
            TextStyle::mono(size)
        } else {
            TextStyle::sans(size)
        };
        style.bold = face.bold;
        style
    }

    /// The style a named family takes at a role's size.
    pub fn font_family(&self, role: TextRole, family: &Family) -> TextStyle {
        let mut s = self.font(role);
        s.family = family.clone();
        s
    }
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::light()
    }
}

#[cfg(test)]
mod tests {
    use super::Theme;

    #[test]
    fn text_pairs_meet_contrast_targets() {
        for t in [Theme::light(), Theme::dark()] {
            assert!(t.text.contrast(t.background) >= 7.0);
            assert!(t.text_muted.contrast(t.background) >= 4.5);
            assert!(t.accent_text.contrast(t.accent) >= 4.5);
            assert!(t.selection_text.contrast(t.selection) >= 4.5)
        }
        for t in [Theme::high_contrast_light(), Theme::high_contrast_dark()] {
            assert!(t.text.contrast(t.background) >= 7.0);
            assert!(t.text_disabled.contrast(t.background) >= 4.5);
            assert!(t.selection_text.contrast(t.selection) >= 7.0)
        }
    }

    #[test]
    fn scale_and_text_scale_grow_metrics() {
        let mut t = Theme::light();
        let base = t.control_height();
        t.scale = 2.0;
        assert_eq!(t.px(10), 20);
        assert!(t.control_height() >= base * 2 - 1);
        let mut big = Theme::light();
        big.text_scale = 2.0;
        assert!(
            big.font(super::TextRole::Body).size
                > Theme::light().font(super::TextRole::Body).size * 1.9
        );
        assert!(big.control_height() > base)
    }

    #[test]
    fn standard_tokens_keep_the_metrics_controls_were_drawn_with() {
        let t = Theme::light();
        assert_eq!(t.radius(), 7);
        assert_eq!(t.indicator(), 24);
        assert_eq!(t.border_width(), 1);
        assert_eq!(Theme::high_contrast_light().border_width(), 2);
        assert_eq!(t.focus_width(), 2);
        assert_eq!(t.space(3), 12);
        assert_eq!(t.font(super::TextRole::Body).size, 18.0);
        assert!(t.font(super::TextRole::Heading).bold);
        assert_eq!(t.motion.caret_blink, 530)
    }

    #[test]
    fn tokens_drive_metrics_and_survive_a_palette_change() {
        let mut tokens = super::ThemeTokens::standard();
        tokens.shape.radius = 12;
        tokens.shape.space = 5;
        tokens.typography.body.size = 20.0;
        tokens.typography.body.bold = true;
        tokens.motion.tooltip_delay = 900;
        tokens.icons = "brand".to_string();
        let t = Theme::light().with_tokens(&tokens);
        assert_eq!(t.radius(), 12);
        assert_eq!(t.space(2), 10);
        assert_eq!(t.font(super::TextRole::Body).size, 20.0);
        assert!(t.font(super::TextRole::Body).bold);
        let dark = t.with_kind(super::ThemeKind::HighContrastDark);
        assert_eq!(dark.tokens(), tokens);
        assert!(dark.is_high_contrast());
        let mut scaled = dark;
        scaled.scale = 2.0;
        assert_eq!(scaled.radius(), 24)
    }

    #[test]
    fn with_kind_keeps_user_settings() {
        let mut t = Theme::light();
        t.scale = 1.5;
        t.rtl = true;
        let d = t.with_kind(super::ThemeKind::Dark);
        assert!(d.is_dark());
        assert_eq!(d.scale, 1.5);
        assert!(d.rtl)
    }
}
