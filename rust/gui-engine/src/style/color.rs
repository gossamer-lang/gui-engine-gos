// color: unpremultiplied sRGB colors with alpha, their HSV/HSL forms,
// and a bounded CSS color parser.
//
// Supported CSS subset: `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`;
// `rgb()`/`rgba()` and `hsl()`/`hsla()` in both the legacy comma form and
// the space/slash form, with numbers or percentages for channels,
// `deg`/`rad`/`grad`/`turn` or bare-number hues, and numeric or percentage
// alpha; the 148 CSS named colors; `transparent`. Parsing is locale
// independent. `currentColor` and `var()` need a caller resolver and
// answer `Unsupported`; every other function (`lab()`, `color()`, ...) is
// `Unsupported` too. Canonical output is `#rrggbb`, or `#rrggbbaa` when
// alpha is below 255.

/// An sRGB color with straight (unpremultiplied) alpha, 8 bits a channel.
use crate::strext::StrExt;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// The outcome of parsing CSS color text.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CssColor {
    Valid(Color),
    /// A prefix of something that could become valid (`#12`, `rgb(1, 2`).
    Intermediate,
    /// Well-formed syntax this parser declines (`var()`, `lab()`).
    Unsupported(String),
    Invalid(String),
}

impl Color {
    /// An opaque color.
    pub fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }

    pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }

    /// An opaque color from `0xRRGGBB`.
    pub fn hex(v: i64) -> Color {
        Color::rgb(
            ((v >> 16) & 255) as u8,
            ((v >> 8) & 255) as u8,
            (v & 255) as u8,
        )
    }

    pub fn transparent() -> Color {
        Color::rgba(0, 0, 0, 0)
    }
    pub fn white() -> Color {
        Color::rgb(255, 255, 255)
    }
    pub fn black() -> Color {
        Color::rgb(0, 0, 0)
    }
    pub fn red() -> Color {
        Color::rgb(255, 0, 0)
    }
    pub fn green() -> Color {
        Color::rgb(0, 255, 0)
    }
    pub fn blue() -> Color {
        Color::rgb(0, 0, 255)
    }

    /// This color with alpha `a` (0..=255).
    pub fn with_alpha(&self, a: i64) -> Color {
        Color {
            r: self.r,
            g: self.g,
            b: self.b,
            a: a.clamp(0, 255) as u8,
        }
    }

    /// The color `t` of the way from this one to `other`, per channel.
    pub fn mix(&self, other: Color, t: f64) -> Color {
        let k = t.clamp(0.0, 1.0);
        Color {
            r: lerp(self.r, other.r, k),
            g: lerp(self.g, other.g, k),
            b: lerp(self.b, other.b, k),
            a: lerp(self.a, other.a, k),
        }
    }

    /// This color composited over an opaque `below`.
    pub fn over(&self, below: Color) -> Color {
        let t = self.a as f64 / 255.0;
        Color {
            r: lerp(below.r, self.r, t),
            g: lerp(below.g, self.g, t),
            b: lerp(below.b, self.b, t),
            a: 255,
        }
    }

    /// WCAG relative luminance, 0..=1.
    pub fn luminance(&self) -> f64 {
        let ch = |v: u8| -> f64 {
            let c = v as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(self.r) + 0.7152 * ch(self.g) + 0.0722 * ch(self.b)
    }

    /// WCAG contrast ratio between this color and `other` (1..=21).
    pub fn contrast(&self, other: Color) -> f64 {
        let a = self.luminance();
        let b = other.luminance();
        let hi = a.max(b);
        let lo = a.min(b);
        (hi + 0.05) / (lo + 0.05)
    }

    /// From hue (degrees), saturation and value (0..=1), opaque.
    pub fn hsv(h: f64, s: f64, v: f64) -> Color {
        let (r, g, b) = hsv_to_rgb(h, s, v);
        Color::rgb(to_byte(r), to_byte(g), to_byte(b))
    }

    /// From hue (degrees), saturation and lightness (0..=1), opaque.
    pub fn hsl(h: f64, s: f64, l: f64) -> Color {
        let (r, g, b) = hsl_to_rgb(h, s, l);
        Color::rgb(to_byte(r), to_byte(g), to_byte(b))
    }

    /// Hue (degrees), saturation, value. Achromatic colors answer hue 0;
    /// editors that must keep a hue across grays carry it separately.
    pub fn hsv_parts(&self) -> (f64, f64, f64) {
        let r = self.r as f64 / 255.0;
        let g = self.g as f64 / 255.0;
        let b = self.b as f64 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        let h = hue_of(r, g, b, max, d);
        let s = if max <= 0.0 { 0.0 } else { d / max };
        (h, s, max)
    }

    /// Hue (degrees), saturation, lightness.
    pub fn hsl_parts(&self) -> (f64, f64, f64) {
        let r = self.r as f64 / 255.0;
        let g = self.g as f64 / 255.0;
        let b = self.b as f64 / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        let l = (max + min) / 2.0;
        let s = if d == 0.0 {
            0.0
        } else {
            d / (1.0 - (2.0 * l - 1.0).abs())
        };
        (hue_of(r, g, b, max, d), s, l)
    }

    /// Canonical CSS text: `#rrggbb`, or `#rrggbbaa` when translucent.
    pub fn css(&self) -> String {
        let base = format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b);
        if self.a == 255 {
            base
        } else {
            base + &format!("{:02x}", self.a)
        }
    }

    /// `rgb(r g b / a)` text, alpha as a rounded fraction.
    pub fn css_rgb(&self) -> String {
        if self.a == 255 {
            format!("rgb({} {} {})", self.r, self.g, self.b)
        } else {
            format!(
                "rgb({} {} {} / {})",
                self.r,
                self.g,
                self.b,
                round3(self.a as f64 / 255.0)
            )
        }
    }

    /// `hsl(h s% l% / a)` text with whole-number components.
    pub fn css_hsl(&self) -> String {
        let (h, s, l) = self.hsl_parts();
        let core = format!(
            "hsl({} {}% {}%",
            (h + 0.5).floor() as i64,
            (s * 100.0 + 0.5).floor() as i64,
            (l * 100.0 + 0.5).floor() as i64
        );
        if self.a == 255 {
            core + ")"
        } else {
            core + &format!(" / {})", round3(self.a as f64 / 255.0))
        }
    }

    /// Parses CSS color text (see the module comment for the subset).
    pub fn parse_css(text: &str) -> Option<Color> {
        match parse(text) {
            CssColor::Valid(c) => Some(c),
            _ => None,
        }
    }
}

/// Parses CSS color text into a typed outcome.
pub fn parse(text: &str) -> CssColor {
    let t = text.trim().to_lowercase();
    if t.is_empty() {
        return CssColor::Intermediate;
    }
    if t.byte_len() > 256 {
        return CssColor::Invalid("color text is too long".to_string());
    }
    if t.starts_with("#") {
        return parse_hex(t.substring(1, t.byte_len()));
    }
    if t == "transparent" {
        return CssColor::Valid(Color::transparent());
    }
    if t == "currentcolor" {
        return CssColor::Unsupported("currentColor needs the element's color".to_string());
    }
    if let Some(open) = strings_find(&t, "(") {
        let name = t.substring(0, open).trim();
        let closed = t.ends_with(")");
        let inner = t.substring(
            open + 1,
            if closed {
                t.byte_len() - 1
            } else {
                t.byte_len()
            },
        );
        if name == "var" || name == "env" || name == "calc" {
            return CssColor::Unsupported(format!("{}() needs a resolver", name));
        }
        if name != "rgb" && name != "rgba" && name != "hsl" && name != "hsla" {
            return CssColor::Unsupported(format!("{}() is not supported", name));
        }
        if !closed {
            return CssColor::Intermediate;
        }
        return parse_function(name, inner);
    }
    match named(&t) {
        Some(c) => CssColor::Valid(c),
        None => {
            if is_name_prefix(&t) {
                CssColor::Intermediate
            } else {
                CssColor::Invalid(format!("unknown color name {}", t))
            }
        }
    }
}

fn strings_find(s: &str, needle: &str) -> Option<i64> {
    let n = needle.byte_len();
    let mut i = 0;
    while i + n <= s.byte_len() {
        if s.substring(i, i + n) == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_hex(digits: &str) -> CssColor {
    let mut values = vec![];
    for c in digits.chars() {
        match c.to_digit(16) {
            Some(d) => values.push(d as i64),
            None => return CssColor::Invalid(format!("'{}' is not a hex digit", c)),
        }
    }
    let n = values.len();
    match n {
        3 | 4 => {
            let a = if n == 4 { values[3] * 17 } else { 255 };
            CssColor::Valid(Color::rgba(
                (values[0] * 17) as u8,
                (values[1] * 17) as u8,
                (values[2] * 17) as u8,
                a as u8,
            ))
        }
        6 | 8 => {
            let a = if n == 8 {
                values[6] * 16 + values[7]
            } else {
                255
            };
            CssColor::Valid(Color::rgba(
                (values[0] * 16 + values[1]) as u8,
                (values[2] * 16 + values[3]) as u8,
                (values[4] * 16 + values[5]) as u8,
                a as u8,
            ))
        }
        0 | 1 | 2 | 5 | 7 => CssColor::Intermediate,
        _ => CssColor::Invalid("hex colors have 3, 4, 6, or 8 digits".to_string()),
    }
}

// Splits function arguments in the comma or the space/slash form; answers
// (components, alpha text, legacy-comma form).
fn split_args(inner: &str) -> Option<(Vec<String>, String, bool)> {
    let s = inner.trim();
    if s.contains(",") {
        let parts: Vec<String> = s.split(",").map(|p| p.trim().to_string()).collect();
        if parts.iter().any(|p| p.is_empty() || p.contains("/")) {
            return None;
        }
        if parts.len() == 3 {
            return Some((parts, "".to_string(), true));
        };
        if parts.len() == 4 {
            return Some((parts[0..3].to_vec(), parts[3].clone(), true));
        };
        return None;
    }
    let mut alpha = String::new();
    let mut main = s;
    if s.contains("/") {
        let halves = s.split("/").collect::<Vec<_>>();
        if halves.len() != 2 {
            return None;
        };
        main = halves[0].trim();
        alpha = halves[1].trim().to_string();
        if alpha.is_empty() {
            return None;
        }
    }
    let parts: Vec<String> = main.split_whitespace().map(String::from).collect();
    if parts.len() != 3 {
        return None;
    };
    Some((parts, alpha.to_string(), false))
}

fn number(text: &str) -> Option<f64> {
    if text.is_empty() || text.byte_len() > 32 {
        return None;
    };
    for c in text.chars() {
        if !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e') {
            return None;
        }
    }
    text.to_f64()
}

fn parse_function(name: &str, inner: &str) -> CssColor {
    let Some((parts, alpha_text, _legacy)) = split_args(inner) else {
        return CssColor::Invalid(format!(
            "{}() takes three components and an optional alpha",
            name
        ));
    };
    let alpha = if alpha_text.is_empty() {
        1.0
    } else if alpha_text.ends_with("%") {
        match number(alpha_text.substring(0, alpha_text.byte_len() - 1)) {
            Some(v) => v / 100.0,
            None => return CssColor::Invalid("alpha is not a number".to_string()),
        }
    } else {
        match number(&alpha_text) {
            Some(v) => v,
            None => return CssColor::Invalid("alpha is not a number".to_string()),
        }
    };
    let a = to_byte(alpha.clamp(0.0, 1.0));
    if name.starts_with("rgb") {
        let mut ch = vec![];
        for p in parts {
            let v = if p.ends_with("%") {
                match number(p.substring(0, p.byte_len() - 1)) {
                    Some(x) => x * 255.0 / 100.0,
                    None => return CssColor::Invalid(format!("'{}' is not a channel", p)),
                }
            } else {
                match number(&p) {
                    Some(x) => x,
                    None => return CssColor::Invalid(format!("'{}' is not a channel", p)),
                }
            };
            if !(0.0..=255.0).contains(&v) {
                return CssColor::Invalid(format!("channel {} is outside 0..255", p));
            }
            ch.push((v + 0.5) as u8)
        }
        return CssColor::Valid(Color::rgba(ch[0], ch[1], ch[2], a));
    }
    let Some(h) = hue(&parts[0]) else {
        return CssColor::Invalid(format!("'{}' is not a hue", parts[0]));
    };
    let mut sl = vec![];
    for p in [&parts[1], &parts[2]] {
        if !p.ends_with("%") {
            return CssColor::Invalid(format!("'{}' needs a percent sign", p));
        }
        match number(p.substring(0, p.byte_len() - 1)) {
            Some(v) => {
                if !(0.0..=100.0).contains(&v) {
                    return CssColor::Invalid(format!("{} is outside 0%..100%", p));
                }
                sl.push(v / 100.0)
            }
            None => return CssColor::Invalid(format!("'{}' is not a percentage", p)),
        }
    }
    let c = Color::hsl(h, sl[0], sl[1]);
    CssColor::Valid(c.with_alpha(a as i64))
}

fn hue(text: &str) -> Option<f64> {
    let units = [
        ("deg", 1.0),
        ("grad", 0.9),
        ("rad", 57.29577951308232),
        ("turn", 360.0),
    ];
    for (suffix, factor) in units {
        if text.ends_with(suffix) {
            return number(text.substring(0, text.byte_len() - suffix.byte_len()))
                .map(|v| wrap_hue(v * factor));
        }
    }
    number(text).map(wrap_hue)
}

fn wrap_hue(h: f64) -> f64 {
    let m = h % 360.0;
    if m < 0.0 { m + 360.0 } else { m }
}

fn hue_of(r: f64, g: f64, b: f64, max: f64, d: f64) -> f64 {
    if d == 0.0 {
        return 0.0;
    }
    let h = if max == r {
        60.0 * ((g - b) / d % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    wrap_hue(h)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    let hh = wrap_hue(h) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((hh % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = rgb_sector(hh, c, x);
    (r + m, g + m, b + m)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let hh = wrap_hue(h) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hh % 2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = rgb_sector(hh, c, x);
    (r + m, g + m, b + m)
}

fn rgb_sector(hh: f64, c: f64, x: f64) -> (f64, f64, f64) {
    if hh < 1.0 {
        (c, x, 0.0)
    } else if hh < 2.0 {
        (x, c, 0.0)
    } else if hh < 3.0 {
        (0.0, c, x)
    } else if hh < 4.0 {
        (0.0, x, c)
    } else if hh < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    }
}

fn to_byte(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn lerp(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t + 0.5).clamp(0.0, 255.0) as u8
}

fn round3(v: f64) -> String {
    let k = (v * 1000.0 + 0.5).floor() / 1000.0;
    format!("{}", k)
}

const NAMES: [(&str, i64); 148] = [
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

/// The CSS named color `name` (lowercase), if any.
pub fn named(name: &str) -> Option<Color> {
    for (n, v) in NAMES {
        if n == name {
            return Some(Color::hex(v));
        }
    }
    None
}

/// Every CSS color name, for completion lists.
pub fn color_names() -> Vec<String> {
    NAMES.iter().map(|p| p.0.to_string()).collect::<Vec<_>>()
}

fn is_name_prefix(t: &str) -> bool {
    if t == "transparent".substring(0, t.byte_len().min(11)) {
        return true;
    }
    for (n, _) in NAMES {
        if n.starts_with(t) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{Color, CssColor, named, parse};

    #[test]
    fn hex_forms_keep_alpha() {
        assert_eq!(Color::parse_css("#ff8000"), Some(Color::rgb(255, 128, 0)));
        assert_eq!(Color::parse_css("#f80"), Some(Color::rgb(255, 136, 0)));
        assert_eq!(
            Color::parse_css("#f808"),
            Some(Color::rgba(255, 136, 0, 136))
        );
        assert_eq!(
            Color::parse_css("#12345678"),
            Some(Color::rgba(0x12, 0x34, 0x56, 0x78))
        );
        assert_eq!(Color::parse_css("#FF8000"), Some(Color::rgb(255, 128, 0)))
    }

    #[test]
    fn functional_comma_and_space_forms() {
        assert_eq!(
            Color::parse_css("rgb(10, 20, 30)"),
            Some(Color::rgb(10, 20, 30))
        );
        assert_eq!(
            Color::parse_css("rgb(10 20 30 / 50%)"),
            Some(Color::rgba(10, 20, 30, 128))
        );
        assert_eq!(
            Color::parse_css("rgba(0, 0, 255, 0.5)"),
            Some(Color::rgba(0, 0, 255, 128))
        );
        assert_eq!(Color::parse_css("rgb(100%, 0%, 0%)"), Some(Color::red()));
        assert_eq!(
            Color::parse_css("hsl(120, 100%, 50%)"),
            Some(Color::green())
        );
        assert_eq!(
            Color::parse_css("hsl(0.5turn 100% 50%)"),
            Some(Color::rgb(0, 255, 255))
        );
        assert_eq!(
            Color::parse_css("hsla(0deg, 0%, 100%, 1)"),
            Some(Color::white())
        )
    }

    #[test]
    fn names_and_transparent() {
        assert_eq!(
            Color::parse_css("Rebeccapurple"),
            Some(Color::rgb(102, 51, 153))
        );
        assert_eq!(Color::parse_css("tomato"), Some(Color::rgb(255, 99, 71)));
        assert_eq!(Color::parse_css("transparent"), Some(Color::transparent()));
        assert_eq!(named("navy"), Some(Color::rgb(0, 0, 128)))
    }

    #[test]
    fn states_distinguish_incomplete_unsupported_and_invalid() {
        assert_eq!(parse("#12"), CssColor::Intermediate);
        assert_eq!(parse("rgb(1, 2"), CssColor::Intermediate);
        assert_eq!(parse("toma"), CssColor::Intermediate);
        assert!(matches!(parse("var(--x)"), CssColor::Unsupported(_)));
        assert!(matches!(parse("lab(50% 0 0)"), CssColor::Unsupported(_)));
        assert!(matches!(parse("currentColor"), CssColor::Unsupported(_)));
        assert!(matches!(parse("#zzzzzz"), CssColor::Invalid(_)));
        assert!(matches!(parse("rgb(300, 0, 0)"), CssColor::Invalid(_)));
        assert!(matches!(parse("hsl(10, 5%)"), CssColor::Invalid(_)));
        assert!(matches!(parse("rgb(1, 2 3)"), CssColor::Invalid(_)))
    }

    #[test]
    fn canonical_round_trips() {
        let c = Color::rgba(0x12, 0x9a, 0xbc, 0x80);
        assert_eq!(c.css(), "#129abc80");
        assert_eq!(Color::parse_css(&c.css()), Some(c));
        assert_eq!(Color::parse_css(&c.css_rgb()), Some(c));
        let opaque = Color::rgb(0x12, 0x9a, 0xbc);
        assert_eq!(Color::parse_css(&opaque.css_rgb()), Some(opaque))
    }

    #[test]
    fn hsv_and_hsl_round_trip_within_a_unit() {
        let c = Color::rgb(200, 100, 50);
        let (h, s, v) = c.hsv_parts();
        let back = Color::hsv(h, s, v);
        assert!(
            (back.r as i64 - 200).abs() <= 1
                && (back.g as i64 - 100).abs() <= 1
                && (back.b as i64 - 50).abs() <= 1
        );
        let (hh, ss, ll) = c.hsl_parts();
        let back2 = Color::hsl(hh, ss, ll);
        assert!((back2.g as i64 - 100).abs() <= 1)
    }

    #[test]
    fn contrast_of_black_on_white_is_21() {
        assert!((Color::black().contrast(Color::white()) - 21.0).abs() < 0.01)
    }
}
