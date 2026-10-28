// svg: rasterises the SVG subset icon themes draw with.
//
// Drawn: `svg`, `g`, `symbol`, `use`, `path`, `rect` (with `rx`/`ry`),
// `circle`, `ellipse`, `line`, `polyline` and `polygon`; every
// `transform` form; solid, `currentColor`, linear and radial gradient
// paints with `href` inheritance, both unit systems and every spread
// method; `opacity`, `fill-opacity`, `stroke-opacity`, `fill-rule`,
// `stroke-width`, `display` and `visibility`; `clip-path`; filters made of
// `feGaussianBlur` alone. Properties come from presentation attributes,
// `<style>` rules for a tag, class or id, and `style` declarations, in
// that order. Text, images, masks, markers and patterns are not drawn, and
// an element whose filter uses other primitives draws without it.

use crate::errors;
use crate::strext::StrExt;
use std::collections::HashMap;

use crate::render::vector::{FillRule, Mask, Path};
use crate::style::color::Color;
use crate::text::font::{CUBIC, LINE, MOVE, QUAD};

const PI: f64 = std::f64::consts::PI;
const MAX_DEPTH: i64 = 24;

// Elements that hold resources or text rather than drawing where they sit.
const UNDRAWN: [&str; 16] = [
    "defs",
    "style",
    "linearGradient",
    "radialGradient",
    "clipPath",
    "mask",
    "filter",
    "pattern",
    "marker",
    "symbol",
    "metadata",
    "title",
    "desc",
    "text",
    "image",
    "script",
];

/// An RGBA image with straight alpha, `width * height * 4` bytes, rows packed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Picture {
    pub rgba: Vec<u8>,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Element {
    name: String,
    attrs: HashMap<String, String>,
    children: Vec<Element>,
    text: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
enum Paint {
    Nothing,
    Solid(Color),
    Current,
    Url(String),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Style {
    fill: Paint,
    fill_opacity: f64,
    fill_rule: FillRule,
    stroke: Paint,
    stroke_opacity: f64,
    stroke_width: f64,
    color: Color,
    visible: bool,
}

/// The affine map `(x, y) -> (a x + c y + e, b x + d y + f)`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Affine {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Affine {
    fn identity() -> Affine {
        Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    fn translate(x: f64, y: f64) -> Affine {
        Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: x,
            f: y,
        }
    }

    // `self` after `n`: `n` maps first.
    fn then(&self, n: Affine) -> Affine {
        Affine {
            a: self.a * n.a + self.c * n.b,
            b: self.b * n.a + self.d * n.b,
            c: self.a * n.c + self.c * n.d,
            d: self.b * n.c + self.d * n.d,
            e: self.a * n.e + self.c * n.f + self.e,
            f: self.b * n.e + self.d * n.f + self.f,
        }
    }

    fn inverse(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        };
        Some(Affine {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        })
    }

    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    // How much the map scales lengths, on average over directions.
    fn scale(&self) -> f64 {
        ((self.a * self.d - self.b * self.c).abs()).sqrt()
    }
}

// Premultiplied RGBA in 0..1, one f64 a channel.
#[derive(Debug, Clone, PartialEq)]
struct Layer {
    px: Vec<f64>,
    width: i64,
    height: i64,
    _lease: std::sync::Arc<crate::memory::Lease>,
}

impl Layer {
    fn bounds(&self) -> crate::geometry::Rect {
        crate::geometry::Rect::new(0, 0, self.width, self.height)
    }

    fn new(width: i64, height: i64, budget: &crate::memory::Budget) -> Result<Layer, String> {
        let pixels = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "SVG dimensions overflow their byte budget".to_owned())?;
        let bytes = pixels
            .checked_mul(size_of::<f64>())
            .ok_or_else(|| "SVG dimensions overflow their byte budget".to_owned())?;
        let lease = budget.reserve(bytes)?;
        Ok(Layer {
            px: vec![0.0; pixels],
            width,
            height,
            _lease: std::sync::Arc::new(lease),
        })
    }
}

struct Doc<'a> {
    ids: HashMap<String, Element>,
    rules: HashMap<String, HashMap<String, String>>,
    viewport: (f64, f64),
    budget: crate::memory::Budget,
    error: std::cell::RefCell<Option<String>>,
    active: &'a dyn Fn() -> bool,
}

/// Rasterises SVG `text` into a `size` x `size` picture, the drawing scaled
/// to fit and centred.
pub fn rasterize(text: &str, size: i64) -> Result<Picture, errors::Error> {
    rasterize_cancellable(text, size, &|| true)
}

pub fn rasterize_cancellable(
    text: &str,
    size: i64,
    active: &dyn Fn() -> bool,
) -> Result<Picture, errors::Error> {
    if text.len() > 2 * 1024 * 1024 || size <= 0 || size > 2048 {
        return Err(errors::new("SVG exceeds its source or pixel byte budget"));
    }
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: 16_384,
    };
    let document = roxmltree::Document::parse_with_options(text, options)
        .map_err(|e| errors::wrap(e, "reading the SVG document"))?;
    if document
        .descendants()
        .any(|node| node.ancestors().take(65).count() > 64)
    {
        return Err(errors::new("SVG exceeds its element depth budget"));
    }
    let root = element(document.root_element());
    if root.name != "svg" {
        return Err(errors::new(format!(
            "the root element is <{}>, not <svg>",
            root.name
        )));
    };
    if size <= 0 {
        return Err(errors::new(format!("cannot rasterise at {} pixels", size)));
    };
    let bounds = view_box(&root);
    let (_, _, vw, vh) = bounds;
    if vw <= 0.0 || vh <= 0.0 {
        return Err(errors::new("the drawing has no area"));
    };
    let mut ids = HashMap::new();
    let mut rules = HashMap::new();
    index(&root, &mut ids, &mut rules);
    let doc = Doc {
        ids,
        rules,
        viewport: (vw, vh),
        budget: crate::memory::Budget::new(128 * 1024 * 1024),
        error: std::cell::RefCell::new(None),
        active,
    };
    let s = size as f64;
    let k = (s / vw).min(s / vh);
    let (mx, my, _, _) = bounds;
    let base = Affine {
        a: k,
        b: 0.0,
        c: 0.0,
        d: k,
        e: (s - vw * k) / 2.0 - mx * k,
        f: (s - vh * k) / 2.0 - my * k,
    };
    let mut layer = Layer::new(size, size, &doc.budget).map_err(errors::new)?;
    let style = Style {
        fill: Paint::Solid(Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        }),
        fill_opacity: 1.0,
        fill_rule: FillRule::NonZero,
        stroke: Paint::Nothing,
        stroke_opacity: 1.0,
        stroke_width: 1.0,
        color: Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        },
        visible: true,
    };
    draw_children(&root, &doc, &base, &style, &mut layer, 0);
    if let Some(error) = doc.error.into_inner() {
        return Err(errors::new(error));
    }
    Ok(Picture {
        rgba: to_rgba(&layer),
        width: size,
        height: size,
    })
}

// The drawing's user-space bounds: `viewBox`, else its width and height.
fn view_box(root: &Element) -> (f64, f64, f64, f64) {
    if let Some(v) = root.attrs.get("viewBox").map(String::as_str) {
        let n = numbers(v);
        if n.len() == 4 {
            return (n[0], n[1], n[2], n[3]);
        }
    }
    let w = root
        .attrs
        .get("width")
        .map(String::as_str)
        .and_then(length)
        .unwrap_or(100.0);
    let h = root
        .attrs
        .get("height")
        .map(String::as_str)
        .and_then(length)
        .unwrap_or(100.0);
    (0.0, 0.0, w, h)
}

// Elements and attributes go by local name, so `xlink:href` reads as `href`.
fn element(node: roxmltree::Node<'_, '_>) -> Element {
    let attrs = node
        .attributes()
        .map(|a| (a.name().to_string(), a.value().to_string()))
        .collect();
    let mut children = vec![];
    let mut text = String::new();
    for c in node.children() {
        if c.is_element() {
            children.push(element(c));
        } else if let Some(t) = c.text().filter(|_| c.is_text()) {
            text.push_str(t);
        }
    }
    Element {
        name: node.tag_name().name().to_string(),
        attrs,
        children,
        text,
    }
}

fn index(
    el: &Element,
    ids: &mut HashMap<String, Element>,
    rules: &mut HashMap<String, HashMap<String, String>>,
) {
    if let Some(id) = el.attrs.get("id") {
        ids.insert(id.clone(), el.clone());
    }
    if el.name == "style" {
        add_rules(&el.text, rules)
    };
    for c in &el.children {
        index(c, ids, rules)
    }
}

// Simple selectors only (`tag`, `.class`, `#id`, `*`), each list split on commas.
fn add_rules(css: &str, rules: &mut HashMap<String, HashMap<String, String>>) {
    let mut text = String::new();
    let mut rest = css;
    while let Some((before, after)) = rest.split_once("/*") {
        text += before;
        rest = match after.split_once("*/") {
            Some((_, tail)) => tail,
            None => "",
        };
    }
    text += rest;
    for block in text.split("}") {
        let Some((selectors, body)) = block.split_once("{") else {
            continue;
        };
        let decls = declarations(body);
        for sel in selectors.split(",") {
            let key = sel.trim();
            if key.is_empty() {
                continue;
            };
            let mut merged = rules.get(key).cloned().unwrap_or_default();
            for (k, v) in &decls {
                merged.insert(k.to_string(), v.to_string());
            }
            rules.insert(key.to_string(), merged);
        }
    }
}

fn declarations(body: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for d in body.split(";") {
        let Some((k, v)) = d.split_once(":") else {
            continue;
        };
        let key = k.trim();
        let raw = v.trim();
        let value = if raw.ends_with("!important") {
            raw.substring(0, raw.byte_len() - 10).trim()
        } else {
            raw
        };
        if !key.is_empty() {
            out.insert(key.to_string(), value.to_string());
        }
    }
    out
}

// The element's declared properties: presentation attributes, then style
// rules by tag, class and id, then its `style` attribute.
fn declared(el: &Element, doc: &Doc) -> HashMap<String, String> {
    let mut out = el.attrs.clone();
    let mut apply = |key: String| {
        if let Some(r) = doc.rules.get(&key) {
            for (k, v) in r {
                out.insert(k.clone(), v.clone());
            }
        }
    };
    apply("*".to_string());
    apply(el.name.clone());
    if let Some(classes) = el.attrs.get("class").map(String::as_str) {
        for c in classes.split_whitespace() {
            apply(format!(".{}", c))
        }
    }
    if let Some(id) = el.attrs.get("id").map(String::as_str) {
        apply(format!("#{}", id))
    };
    if let Some(s) = el.attrs.get("style").map(String::as_str) {
        for (k, v) in declarations(s) {
            out.insert(k.to_string(), v.to_string());
        }
    };
    out.clone()
}

fn inherit(parent: Style, d: &HashMap<String, String>) -> Style {
    let mut s = parent;
    if let Some(c) = d.get("color").map(String::as_str)
        && c != "inherit"
        && let Some(color) = Color::parse_css(c)
    {
        s.color = color
    }
    if let Some(p) = d.get("fill").map(String::as_str)
        && p != "inherit"
    {
        s.fill = paint(p)
    };
    if let Some(p) = d.get("stroke").map(String::as_str)
        && p != "inherit"
    {
        s.stroke = paint(p)
    };
    if let Some(v) = d.get("fill-opacity").map(String::as_str).and_then(fraction) {
        s.fill_opacity = v
    };
    if let Some(v) = d
        .get("stroke-opacity")
        .map(String::as_str)
        .and_then(fraction)
    {
        s.stroke_opacity = v
    };
    if let Some(v) = d.get("stroke-width").map(String::as_str).and_then(length) {
        s.stroke_width = v
    };
    if let Some(r) = d.get("fill-rule").map(String::as_str)
        && r != "inherit"
    {
        s.fill_rule = if r == "evenodd" {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        }
    };
    if let Some(v) = d.get("visibility").map(String::as_str)
        && v != "inherit"
    {
        s.visible = v == "visible"
    };
    s
}

fn paint(text: &str) -> Paint {
    let t = text.trim();
    if t == "none" {
        return Paint::Nothing;
    };
    if t == "currentColor" || t == "currentcolor" {
        return Paint::Current;
    };
    if t.starts_with("url(") {
        let Some((inside, _)) = t.substring(4, t.byte_len()).split_once(")") else {
            return Paint::Nothing;
        };
        return Paint::Url(
            inside
                .trim()
                .trim_matches('\'')
                .trim_matches('"')
                .trim_start_matches('#')
                .to_string(),
        );
    }
    match Color::parse_css(t) {
        Some(c) => Paint::Solid(c),
        None => Paint::Nothing,
    }
}

// `0.5` or `50%`, clamped to 0..1.
fn fraction(text: &str) -> Option<f64> {
    let t = text.trim();
    let v = if t.ends_with("%") {
        t.trim_end_matches("%").to_f64()? / 100.0
    } else {
        t.to_f64()?
    };
    Some(v.clamp(0.0, 1.0))
}

// A user-space length; units other than `px` read as pixels too.
fn length(text: &str) -> Option<f64> {
    let t = text.trim();
    let n = numbers(t);
    if n.is_empty() || t.ends_with("%") {
        None
    } else {
        Some(n[0])
    }
}

// A gradient coordinate: `50%` is a fraction of `extent`.
fn coordinate(text: &str, extent: f64) -> Option<f64> {
    let t = text.trim();
    if t.ends_with("%") {
        return Some(t.trim_end_matches("%").to_f64()? / 100.0 * extent);
    };
    let n = numbers(t);
    if n.is_empty() { None } else { Some(n[0]) }
}

fn url_ref(text: &str) -> Option<String> {
    match paint(text) {
        Paint::Url(id) => Some(id),
        _ => None,
    }
}

// ---- drawing ----

fn draw_children(
    el: &Element,
    doc: &Doc,
    m: &Affine,
    style: &Style,
    layer: &mut Layer,
    depth: i64,
) {
    for c in &el.children {
        draw(c, doc, m, style, layer, depth + 1)
    }
}

fn draw(el: &Element, doc: &Doc, parent_m: &Affine, parent: &Style, layer: &mut Layer, depth: i64) {
    if !(doc.active)() {
        *doc.error.borrow_mut() = Some("SVG rendering cancelled or expired".into());
        return;
    }
    if doc.error.borrow().is_some() {
        return;
    }
    if depth > MAX_DEPTH {
        return;
    };
    if UNDRAWN.contains(&el.name.as_str()) {
        return;
    };
    let d = declared(el, doc);
    if d.get("display").map(String::as_str) == Some("none") {
        return;
    };
    let style = inherit(parent.clone(), &d);
    let mut m = parent_m.then(
        el.attrs
            .get("transform")
            .map(String::as_str)
            .map(transform)
            .unwrap_or(Affine::identity()),
    );
    if el.name == "use" || el.name == "svg" {
        let x = el
            .attrs
            .get("x")
            .map(String::as_str)
            .and_then(length)
            .unwrap_or(0.0);
        let y = el
            .attrs
            .get("y")
            .map(String::as_str)
            .and_then(length)
            .unwrap_or(0.0);
        m = m.then(Affine::translate(x, y));
    }
    let opacity = d
        .get("opacity")
        .map(String::as_str)
        .and_then(fraction)
        .unwrap_or(1.0);
    if opacity <= 0.0 {
        return;
    };
    let clip = d
        .get("clip-path")
        .map(String::as_str)
        .and_then(url_ref)
        .and_then(|id| doc.ids.get(&id));
    let blur = d
        .get("filter")
        .map(String::as_str)
        .and_then(url_ref)
        .and_then(|id| doc.ids.get(&id))
        .and_then(blur_of)
        .map(|s| s * m.scale());
    if opacity >= 1.0 && clip.is_none() && blur.is_none() {
        content(el, doc, &m, &style, layer, depth);
        return;
    }
    let mut sub = match Layer::new(layer.width, layer.height, &doc.budget) {
        Ok(layer) => layer,
        Err(error) => {
            *doc.error.borrow_mut() = Some(error);
            return;
        }
    };
    content(el, doc, &m, &style, &mut sub, depth);
    if let Some(sigma) = blur
        && let Err(error) = blur_layer(&mut sub, sigma, &doc.budget)
    {
        *doc.error.borrow_mut() = Some(error);
        return;
    };
    if let Some(c) = clip {
        let bbox = element_bbox(el, depth);
        let coverage = clip_coverage(c, doc, &m, bbox, layer.width, layer.height, depth);
        for (i, cov) in coverage.iter().enumerate() {
            let at = i * 4;
            sub.px[at] *= cov;
            sub.px[at + 1] *= cov;
            sub.px[at + 2] *= cov;
            sub.px[at + 3] *= cov
        }
    }
    composite(layer, &sub, opacity)
}

fn content(el: &Element, doc: &Doc, m: &Affine, style: &Style, layer: &mut Layer, depth: i64) {
    if el.name == "use" {
        let Some(id) = el
            .attrs
            .get("href")
            .map(String::as_str)
            .map(|h| h.trim_start_matches("#"))
        else {
            return;
        };
        let Some(target) = doc.ids.get(id) else {
            return;
        };
        if target.name == "symbol" {
            draw_children(target, doc, m, style, layer, depth + 1)
        } else {
            draw(target, doc, m, style, layer, depth + 1)
        };
        return;
    }
    match shape(el) {
        Some(path) => {
            if style.visible {
                paint_shape(&path, doc, m, style, layer)
            }
        }
        None => draw_children(el, doc, m, style, layer, depth),
    }
}

fn paint_shape(path: &Path, doc: &Doc, m: &Affine, style: &Style, layer: &mut Layer) {
    if path.cmds.is_empty() {
        return;
    };
    let bbox = path_bbox(&path.cmds, &Affine::identity());
    if style.fill != Paint::Nothing && style.fill_opacity > 0.0 {
        let mask = Mask::fill_within(
            &path.cmds,
            (m.a, m.b, m.c, m.d, m.e, m.f),
            style.fill_rule,
            Some(layer.bounds()),
        );
        fill_mask(
            layer,
            &mask,
            &shader(&style.fill, style.color, doc, m, bbox),
            style.fill_opacity,
        )
    }
    if style.stroke != Paint::Nothing && style.stroke_opacity > 0.0 && style.stroke_width > 0.0 {
        let band = path.stroke(style.stroke_width);
        let mask = Mask::fill_within(
            &band.cmds,
            (m.a, m.b, m.c, m.d, m.e, m.f),
            FillRule::NonZero,
            Some(layer.bounds()),
        );
        fill_mask(
            layer,
            &mask,
            &shader(&style.stroke, style.color, doc, m, bbox),
            style.stroke_opacity,
        )
    }
}

// The element's outline in its own user space, for the shapes that have one.
fn shape(el: &Element) -> Option<Path> {
    let num = |k: String| {
        el.attrs
            .get(&k)
            .map(String::as_str)
            .and_then(length)
            .unwrap_or(0.0)
    };
    match el.name.as_str() {
        "path" => Some(path_data(
            el.attrs.get("d").map(String::as_str).unwrap_or(""),
        )),
        "rect" => {
            let w = num("width".to_string());
            let h = num("height".to_string());
            if w <= 0.0 || h <= 0.0 {
                return Some(Path::new());
            };
            let rx_set = el.attrs.get("rx").map(String::as_str).and_then(length);
            let ry_set = el.attrs.get("ry").map(String::as_str).and_then(length);
            let rx = rx_set.or(ry_set).unwrap_or(0.0).max(0.0).min(w / 2.0);
            let ry = ry_set.or(rx_set).unwrap_or(0.0).max(0.0).min(h / 2.0);
            Some(rounded(
                num("x".to_string()),
                num("y".to_string()),
                w,
                h,
                rx,
                ry,
            ))
        }
        "circle" => {
            let r = num("r".to_string());
            Some(if r > 0.0 {
                Path::ellipse(num("cx".to_string()), num("cy".to_string()), r, r)
            } else {
                Path::new()
            })
        }
        "ellipse" => {
            let rx = num("rx".to_string());
            let ry = num("ry".to_string());
            Some(if rx > 0.0 && ry > 0.0 {
                Path::ellipse(num("cx".to_string()), num("cy".to_string()), rx, ry)
            } else {
                Path::new()
            })
        }
        "line" => {
            let mut p = Path::new();
            p.move_to(num("x1".to_string()), num("y1".to_string()));
            p.line_to(num("x2".to_string()), num("y2".to_string()));
            Some(p)
        }
        "polyline" | "polygon" => {
            let n = numbers(el.attrs.get("points").map(String::as_str).unwrap_or(""));
            let mut p = Path::new();
            let mut i = 0;
            while i + 1 < n.len() {
                if i == 0 {
                    p.move_to(n[i], n[i + 1])
                } else {
                    p.line_to(n[i], n[i + 1])
                };
                i += 2;
            }
            if el.name == "polygon" && n.len() >= 4 {
                p.close()
            };
            Some(p)
        }
        _ => None,
    }
}

// A rectangle whose corners are quarter ellipses `rx` x `ry`.
fn rounded(x: f64, y: f64, w: f64, h: f64, rx: f64, ry: f64) -> Path {
    let mut p = Path::new();
    if rx <= 0.0 || ry <= 0.0 {
        p.move_to(x, y);
        p.line_to(x + w, y);
        p.line_to(x + w, y + h);
        p.line_to(x, y + h);
        p.close();
        return p;
    }
    // Control points of a cubic quarter circle sit this far along each tangent.
    let k = 0.5522847498;
    let kx = rx * k;
    let ky = ry * k;
    p.move_to(x + rx, y);
    p.line_to(x + w - rx, y);
    p.cubic_to(x + w - rx + kx, y, x + w, y + ry - ky, x + w, y + ry);
    p.line_to(x + w, y + h - ry);
    p.cubic_to(
        x + w,
        y + h - ry + ky,
        x + w - rx + kx,
        y + h,
        x + w - rx,
        y + h,
    );
    p.line_to(x + rx, y + h);
    p.cubic_to(x + rx - kx, y + h, x, y + h - ry + ky, x, y + h - ry);
    p.line_to(x, y + ry);
    p.cubic_to(x, y + ry - ky, x + rx - kx, y, x + rx, y);
    p.close();
    p
}

// The bounding bounds of a command stream's points mapped through `m`, as
// (x, y, width, height); control points count, which only widens it.
fn path_bbox(cmds: &[f64], m: &Affine) -> (f64, f64, f64, f64) {
    let mut minx = 1e18_f64;
    let mut miny = 1e18_f64;
    let mut maxx = -1e18_f64;
    let mut maxy = -1e18_f64;
    let mut i = 0;
    while i < cmds.len() {
        let op = cmds[i];
        let pairs = match op {
            MOVE | LINE => 1,
            QUAD => 2,
            CUBIC => 3,
            _ => 0,
        };
        i += 1;
        let mut k = 0;
        while k < pairs {
            let (x, y) = m.apply(cmds[i], cmds[i + 1]);
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
            i += 2;
            k += 1;
        }
    }
    if minx > maxx {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (minx, miny, maxx - minx, maxy - miny)
    }
}

// The user-space bounds of what an element draws, for `objectBoundingBox` clips.
fn element_bbox(el: &Element, depth: i64) -> (f64, f64, f64, f64) {
    if let Some(p) = shape(el) {
        return path_bbox(&p.cmds, &Affine::identity());
    };
    let mut minx = 1e18_f64;
    let mut miny = 1e18_f64;
    let mut maxx = -1e18_f64;
    let mut maxy = -1e18_f64;
    if depth > MAX_DEPTH {
        return (0.0, 0.0, 0.0, 0.0);
    };
    for c in &el.children {
        let t = c
            .attrs
            .get("transform")
            .map(String::as_str)
            .map(transform)
            .unwrap_or(Affine::identity());
        let (x, y, w, h) = element_bbox(c, depth + 1);
        if w <= 0.0 && h <= 0.0 {
            continue;
        };
        for (px, py) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
            let (tx, ty) = t.apply(px, py);
            minx = minx.min(tx);
            miny = miny.min(ty);
            maxx = maxx.max(tx);
            maxy = maxy.max(ty);
        }
    }
    if minx > maxx {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (minx, miny, maxx - minx, maxy - miny)
    }
}

// Coverage 0..1 per layer pixel of a clipPath's shapes, unioned.
fn clip_coverage(
    clip: &Element,
    doc: &Doc,
    m: &Affine,
    bbox: (f64, f64, f64, f64),
    width: i64,
    height: i64,
    depth: i64,
) -> Vec<f64> {
    let mut cov = vec![0.0; (width * height) as usize];
    let (bx, by, bw, bh) = bbox;
    let mut base = m.then(
        clip.attrs
            .get("transform")
            .map(|t| transform(t))
            .unwrap_or(Affine::identity()),
    );
    if clip.attrs.get("clipPathUnits").map(String::as_str) == Some("objectBoundingBox") {
        base = base.then(Affine {
            a: bw,
            b: 0.0,
            c: 0.0,
            d: bh,
            e: bx,
            f: by,
        })
    };
    for c in clip_shapes(clip.clone(), doc, depth) {
        let (el, t) = c;
        let Some(p) = shape(&el) else { continue };
        let d = declared(&el, doc);
        if d.get("display").map(String::as_str) == Some("none") {
            continue;
        };
        let rule = if d.get("clip-rule").map(String::as_str) == Some("evenodd") {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
        let full = base.then(t);
        let mask = Mask::fill_within(
            &p.cmds,
            (full.a, full.b, full.c, full.d, full.e, full.f),
            rule,
            Some(crate::geometry::Rect::new(0, 0, width, height)),
        );
        each_covered(&mask, width, height, |i, a| {
            cov[i as usize] = cov[i as usize] + a - cov[i as usize] * a
        })
    }
    cov
}

// A clipPath's shapes with the transform from the clipPath's space to each,
// `use` references resolved.
fn clip_shapes(clip: Element, doc: &Doc, depth: i64) -> Vec<(Element, Affine)> {
    let mut out = vec![];
    if depth > MAX_DEPTH {
        return out;
    };
    for c in clip.children {
        let t = c
            .attrs
            .get("transform")
            .map(String::as_str)
            .map(transform)
            .unwrap_or(Affine::identity());
        if c.name == "use" {
            let x = c
                .attrs
                .get("x")
                .map(String::as_str)
                .and_then(length)
                .unwrap_or(0.0);
            let y = c
                .attrs
                .get("y")
                .map(String::as_str)
                .and_then(length)
                .unwrap_or(0.0);
            let Some(target) = c
                .attrs
                .get("href")
                .map(String::as_str)
                .and_then(|h| doc.ids.get(h.trim_start_matches("#")))
            else {
                continue;
            };
            let at = t.then(Affine::translate(x, y));
            let inner = target
                .attrs
                .get("transform")
                .map(String::as_str)
                .map(transform)
                .unwrap_or(Affine::identity());
            out.push((target.clone(), at.then(inner)))
        } else if shape(&c).is_some() {
            out.push((c, t))
        } else if c.name == "g" {
            for inner in clip_shapes(c, doc, depth + 1) {
                let (el, it) = inner;
                out.push((el, t.then(it)))
            }
        }
    }
    out
}

// The standard deviation of a filter made of a single `feGaussianBlur`.
fn blur_of(filter: &Element) -> Option<f64> {
    if filter.children.len() != 1 || filter.children[0].name != "feGaussianBlur" {
        return None;
    };
    let n = numbers(
        filter.children[0]
            .attrs
            .get("stdDeviation")
            .map_or("", String::as_str),
    );
    if n.is_empty() { None } else { Some(n[0]) }
}

// Three bounds blurs approximate a Gaussian of `sigma` pixels.
fn blur_layer(layer: &mut Layer, sigma: f64, budget: &crate::memory::Budget) -> Result<(), String> {
    if sigma <= 0.0 {
        return Ok(());
    };
    let r = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0 + 0.5).floor() as i64;
    if r <= 0 {
        return Ok(());
    };
    let r = r.min(layer.width.max(layer.height));
    for _ in 0..3 {
        box_pass(layer, r, true, budget)?;
        box_pass(layer, r, false, budget)?;
    }
    Ok(())
}

fn box_pass(
    layer: &mut Layer,
    r: i64,
    horizontal: bool,
    budget: &crate::memory::Budget,
) -> Result<(), String> {
    let w = layer.width;
    let h = layer.height;
    let lines = if horizontal { h } else { w };
    let len = if horizontal { w } else { h };
    let span = (2 * r + 1) as f64;
    let _scratch = budget.reserve(layer.px.len() * size_of::<f64>())?;
    let src = layer.px.clone();
    for line in 0..lines {
        let at = |i: i64| -> i64 {
            if horizontal {
                (line * w + i) * 4
            } else {
                (i * w + line) * 4
            }
        };
        for ch in 0..4 {
            let mut sum = 0.0;
            for i in -r..r + 1 {
                if i >= 0 && i < len {
                    sum += src[(at(i) + ch) as usize]
                }
            }
            for i in 0..len {
                layer.px[(at(i) + ch) as usize] = sum / span;
                let out = i - r;
                let inn = i + r + 1;
                if out >= 0 {
                    sum -= src[(at(out) + ch) as usize]
                };
                if inn < len {
                    sum += src[(at(inn) + ch) as usize]
                }
            }
        }
    }
    Ok(())
}

// ---- paint ----

// A paint resolved for one shape: a colour per device pixel centre.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
enum Shader {
    Flat(f64, f64, f64, f64),
    Linear(Affine, f64, f64, f64, f64, Vec<Stop>, i64),
    Radial(Affine, f64, f64, f64, f64, f64, Vec<Stop>, i64),
    Skip,
}

// A premultiplied gradient stop.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Stop {
    offset: f64,
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

fn flat(c: Color) -> Shader {
    let a = c.a as f64 / 255.0;
    Shader::Flat(
        c.r as f64 / 255.0 * a,
        c.g as f64 / 255.0 * a,
        c.b as f64 / 255.0 * a,
        a,
    )
}

fn shader(p: &Paint, current: Color, doc: &Doc, m: &Affine, bbox: (f64, f64, f64, f64)) -> Shader {
    match p {
        Paint::Nothing => Shader::Skip,
        Paint::Solid(c) => flat(*c),
        Paint::Current => flat(current),
        Paint::Url(id) => gradient(id, current, doc, m, bbox),
    }
}

// A gradient attribute, inherited through its `href` chain.
fn grad_attr(doc: &Doc, start: &Element, name: &str) -> Option<String> {
    let mut el = start;
    for _ in 0..8 {
        if let Some(v) = el.attrs.get(name).map(String::as_str) {
            return Some(v.to_string());
        };
        let next = el
            .attrs
            .get("href")
            .map(String::as_str)
            .and_then(|h| doc.ids.get(h.trim_start_matches("#")))?;
        el = next;
    }
    None
}

fn stops(doc: &Doc, start: &Element, current: Color) -> Vec<Stop> {
    let mut el = start;
    for _ in 0..8 {
        let own = el
            .children
            .iter()
            .filter(|c| c.name == "stop")
            .cloned()
            .collect::<Vec<_>>();
        if !own.is_empty() {
            let mut out = vec![];
            let mut last = 0.0;
            for s in own {
                let d = declared(&s, doc);
                let offset = d
                    .get("offset")
                    .map(String::as_str)
                    .and_then(fraction)
                    .unwrap_or(0.0)
                    .max(last);
                last = offset;
                let color = match d.get("stop-color").map(String::as_str) {
                    Some(t) => {
                        if t == "currentColor" || t == "currentcolor" {
                            current
                        } else {
                            Color::parse_css(t).unwrap_or(Color {
                                r: 0,
                                g: 0,
                                b: 0,
                                a: 255,
                            })
                        }
                    }
                    None => Color {
                        r: 0,
                        g: 0,
                        b: 0,
                        a: 255,
                    },
                };
                let a = color.a as f64 / 255.0
                    * d.get("stop-opacity")
                        .map(String::as_str)
                        .and_then(fraction)
                        .unwrap_or(1.0);
                out.push(Stop {
                    offset,
                    r: color.r as f64 / 255.0 * a,
                    g: color.g as f64 / 255.0 * a,
                    b: color.b as f64 / 255.0 * a,
                    a,
                })
            }
            return out;
        }
        let Some(next) = el
            .attrs
            .get("href")
            .map(String::as_str)
            .and_then(|h| doc.ids.get(h.trim_start_matches("#")))
        else {
            return vec![];
        };
        el = next;
    }
    vec![]
}

fn gradient(id: &str, current: Color, doc: &Doc, m: &Affine, bbox: (f64, f64, f64, f64)) -> Shader {
    let Some(g) = doc.ids.get(id) else {
        return Shader::Skip;
    };
    if g.name != "linearGradient" && g.name != "radialGradient" {
        return Shader::Skip;
    };
    let list = stops(doc, g, current);
    if list.is_empty() {
        return Shader::Skip;
    };
    if list.len() == 1 {
        return Shader::Flat(list[0].r, list[0].g, list[0].b, list[0].a);
    };
    let user = grad_attr(doc, g, "gradientUnits") == Some("userSpaceOnUse".to_string());
    let (bx, by, bw, bh) = bbox;
    if !user && (bw <= 0.0 || bh <= 0.0) {
        return Shader::Skip;
    };
    let units = if user {
        Affine::identity()
    } else {
        Affine {
            a: bw,
            b: 0.0,
            c: 0.0,
            d: bh,
            e: bx,
            f: by,
        }
    };
    let gt = grad_attr(doc, g, "gradientTransform")
        .map(|t| transform(&t))
        .unwrap_or(Affine::identity());
    let Some(inv) = m.then(units).then(gt).inverse() else {
        return Shader::Skip;
    };
    let spread = match grad_attr(doc, g, "spreadMethod") {
        Some(s) => {
            if s == "reflect" {
                1
            } else if s == "repeat" {
                2
            } else {
                0
            }
        }
        None => 0,
    };
    let (vw, vh) = doc.viewport;
    let ex = if user { vw } else { 1.0 };
    let ey = if user { vh } else { 1.0 };
    let diag = if user {
        ((vw * vw + vh * vh) / 2.0).sqrt()
    } else {
        1.0
    };
    let at = |name: String, extent: f64, fallback: f64| {
        grad_attr(doc, g, &name)
            .and_then(|t| coordinate(&t, extent))
            .unwrap_or(fallback)
    };
    if g.name == "linearGradient" {
        return Shader::Linear(
            inv,
            at("x1".to_string(), ex, 0.0),
            at("y1".to_string(), ey, 0.0),
            at("x2".to_string(), ex, ex),
            at("y2".to_string(), ey, 0.0),
            list,
            spread,
        );
    }
    let cx = at("cx".to_string(), ex, ex * 0.5);
    let cy = at("cy".to_string(), ey, ey * 0.5);
    let r = at("r".to_string(), diag, diag * 0.5);
    if r <= 0.0 {
        return Shader::Flat(
            list[list.len() - 1].r,
            list[list.len() - 1].g,
            list[list.len() - 1].b,
            list[list.len() - 1].a,
        );
    };
    let mut fx = at("fx".to_string(), ex, cx);
    let mut fy = at("fy".to_string(), ey, cy);
    // A focus outside the circle moves onto its edge.
    let dist = ((fx - cx) * (fx - cx) + (fy - cy) * (fy - cy)).sqrt();
    if dist > r * 0.999 {
        fx = cx + (fx - cx) * r * 0.999 / dist;
        fy = cy + (fy - cy) * r * 0.999 / dist;
    }
    Shader::Radial(inv, cx, cy, r, fx, fy, list, spread)
}

fn spread_t(t: f64, spread: i64) -> f64 {
    match spread {
        1 => {
            let m = t.abs() % 2.0;
            if m > 1.0 { 2.0 - m } else { m }
        }
        2 => t - t.floor(),
        _ => t.clamp(0.0, 1.0),
    }
}

fn color_at(list: &[Stop], t: f64) -> (f64, f64, f64, f64) {
    let first = &list[0];
    if t <= first.offset {
        return (first.r, first.g, first.b, first.a);
    };
    for i in 1..list.len() {
        let s = &list[i];
        if t <= s.offset {
            let p = &list[i - 1];
            let span = s.offset - p.offset;
            let u = if span <= 0.0 {
                1.0
            } else {
                (t - p.offset) / span
            };
            return (
                p.r + (s.r - p.r) * u,
                p.g + (s.g - p.g) * u,
                p.b + (s.b - p.b) * u,
                p.a + (s.a - p.a) * u,
            );
        }
    }
    let last = &list[list.len() - 1];
    (last.r, last.g, last.b, last.a)
}

fn shade(s: &Shader, x: f64, y: f64) -> (f64, f64, f64, f64) {
    match s {
        Shader::Flat(r, g, b, a) => (*r, *g, *b, *a),
        Shader::Linear(inv, x1, y1, x2, y2, list, spread) => {
            let (px, py) = inv.apply(x, y);
            let dx = x2 - x1;
            let dy = y2 - y1;
            let dd = dx * dx + dy * dy;
            let t = if dd <= 0.0 {
                1.0
            } else {
                ((px - x1) * dx + (py - y1) * dy) / dd
            };
            color_at(list, spread_t(t, *spread))
        }
        Shader::Radial(inv, cx, cy, r, fx, fy, list, spread) => {
            let (px, py) = inv.apply(x, y);
            // The t whose circle, centred f + t (c - f) with radius t r, passes through p.
            let dx = px - fx;
            let dy = py - fy;
            let cdx = cx - fx;
            let cdy = cy - fy;
            let qa = cdx * cdx + cdy * cdy - r * r;
            let qb = dx * cdx + dy * cdy;
            let qc = dx * dx + dy * dy;
            let t = if qa.abs() < 1e-12 {
                if qb.abs() < 1e-12 {
                    0.0
                } else {
                    qc / (2.0 * qb)
                }
            } else {
                (qb - ((qb * qb - qa * qc).max(0.0)).sqrt()) / qa
            };
            color_at(list, spread_t(t, *spread))
        }
        Shader::Skip => (0.0, 0.0, 0.0, 0.0),
    }
}

// Calls `f(index, coverage)` for each covered pixel of `mask` inside the layer.
fn each_covered(mask: &Mask, width: i64, height: i64, mut f: impl FnMut(i64, f64)) {
    let x0 = mask.left.max(0);
    let y0 = mask.top.max(0);
    let x1 = (mask.left + mask.width).min(width);
    let y1 = (mask.top + mask.height).min(height);
    let mut y = y0;
    while y < y1 {
        let row = (y - mask.top) * mask.width - mask.left;
        let mut x = x0;
        while x < x1 {
            let c = mask.coverage[(row + x) as usize];
            if c > 0 {
                f(y * width + x, c as f64 / 255.0)
            };
            x += 1;
        }
        y += 1;
    }
}

fn fill_mask(layer: &mut Layer, mask: &Mask, s: &Shader, opacity: f64) {
    if let Shader::Skip = s {
        return;
    };
    let width = layer.width;
    let x0 = mask.left.max(0);
    let x1 = (mask.left + mask.width).min(width);
    let y1 = (mask.top + mask.height).min(layer.height);
    let mut y = mask.top.max(0);
    while y < y1 {
        let row = (y - mask.top) * mask.width - mask.left;
        let mut x = x0;
        while x < x1 {
            let c = mask.coverage[(row + x) as usize];
            x += 1;
            if c == 0 {
                continue;
            };
            let (r, g, b, a) = shade(s, x as f64 - 0.5, y as f64 + 0.5);
            let k = c as f64 / 255.0 * opacity;
            let sa = a * k;
            if sa <= 0.0 {
                continue;
            };
            let at = (y * width + x - 1) * 4;
            let keep = 1.0 - sa;
            layer.px[at as usize] = r * k + layer.px[at as usize] * keep;
            layer.px[(at + 1) as usize] = g * k + layer.px[(at + 1) as usize] * keep;
            layer.px[(at + 2) as usize] = b * k + layer.px[(at + 2) as usize] * keep;
            layer.px[(at + 3) as usize] = sa + layer.px[(at + 3) as usize] * keep
        }
        y += 1;
    }
}

fn composite(dst: &mut Layer, src: &Layer, opacity: f64) {
    let n = dst.width * dst.height;
    for i in 0..n {
        let at = i * 4;
        let sa = src.px[(at + 3) as usize] * opacity;
        if sa <= 0.0 {
            continue;
        };
        let keep = 1.0 - sa;
        dst.px[at as usize] = src.px[at as usize] * opacity + dst.px[at as usize] * keep;
        dst.px[(at + 1) as usize] =
            src.px[(at + 1) as usize] * opacity + dst.px[(at + 1) as usize] * keep;
        dst.px[(at + 2) as usize] =
            src.px[(at + 2) as usize] * opacity + dst.px[(at + 2) as usize] * keep;
        dst.px[(at + 3) as usize] = sa + dst.px[(at + 3) as usize] * keep
    }
}

fn to_rgba(layer: &Layer) -> Vec<u8> {
    let n = layer.width * layer.height;
    let mut out = vec![0u8; (n * 4) as usize];
    for i in 0..n {
        let at = i * 4;
        let a = layer.px[(at + 3) as usize].clamp(0.0, 1.0);
        if a <= 0.0 {
            continue;
        };
        for ch in 0..3 {
            let v = (layer.px[(at + ch) as usize] / a).clamp(0.0, 1.0);
            out[(at + ch) as usize] = (v * 255.0 + 0.5).floor() as u8
        }
        out[(at + 3) as usize] = (a * 255.0 + 0.5).floor() as u8
    }
    out
}

// ---- text forms ----

// Reads numbers in SVG's packed syntax, where `1-2.5.5` is 1, -2.5 and 0.5.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Scanner {
    s: String,
    i: i64,
}

impl Scanner {
    fn new(s: &str) -> Scanner {
        Scanner {
            s: s.to_string(),
            i: 0,
        }
    }

    fn at_end(&self) -> bool {
        self.i >= self.s.byte_len()
    }

    fn peek(&self) -> i64 {
        if self.at_end() {
            0
        } else {
            self.s.byte_at(self.i)
        }
    }

    fn skip(&mut self) {
        while !self.at_end() {
            let c = self.peek();
            if c == 32 || c == 9 || c == 10 || c == 13 || c == 44 {
                self.i += 1
            } else {
                break;
            }
        }
    }

    fn number(&mut self) -> Option<f64> {
        self.skip();
        let start = self.i;
        if self.peek() == 43 || self.peek() == 45 {
            self.i += 1
        };
        let mut digits = false;
        while self.peek() >= 48 && self.peek() <= 57 {
            self.i += 1;
            digits = true
        }
        if self.peek() == 46 {
            self.i += 1;
            while self.peek() >= 48 && self.peek() <= 57 {
                self.i += 1;
                digits = true
            }
        }
        if !digits {
            self.i = start;
            return None;
        };
        if self.peek() == 101 || self.peek() == 69 {
            let mark = self.i;
            self.i += 1;
            if self.peek() == 43 || self.peek() == 45 {
                self.i += 1
            };
            let exp_start = self.i;
            while self.peek() >= 48 && self.peek() <= 57 {
                self.i += 1
            }
            if self.i == exp_start {
                self.i = mark
            }
        }
        self.s.substring(start, self.i).to_f64()
    }

    // An arc flag: one `0` or `1`, which needs no separator after it.
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = self.peek();
        if c == 48 || c == 49 {
            self.i += 1;
            Some(c == 49)
        } else {
            None
        }
    }
}

fn numbers(text: &str) -> Vec<f64> {
    let mut sc = Scanner::new(text);
    let mut out = vec![];
    while let Some(n) = sc.number() {
        out.push(n)
    }
    out
}

fn transform(text: &str) -> Affine {
    let mut m = Affine::identity();
    let mut rest = text;
    while let Some((head, tail)) = rest.split_once("(") {
        let name = head.trim().trim_start_matches(",").trim();
        let Some((args, after)) = tail.split_once(")") else {
            break;
        };
        rest = after;
        let n = numbers(args);
        let arg = |i: i64, d: f64| if i < n.len() as i64 { n[i as usize] } else { d };
        let t = match name {
            "matrix" => {
                if n.len() == 6 {
                    Affine {
                        a: n[0],
                        b: n[1],
                        c: n[2],
                        d: n[3],
                        e: n[4],
                        f: n[5],
                    }
                } else {
                    Affine::identity()
                }
            }
            "translate" => Affine::translate(arg(0, 0.0), arg(1, 0.0)),
            "scale" => {
                let sx = arg(0, 1.0);
                Affine {
                    a: sx,
                    b: 0.0,
                    c: 0.0,
                    d: arg(1, sx),
                    e: 0.0,
                    f: 0.0,
                }
            }
            "rotate" => {
                let rad = arg(0, 0.0) * PI / 180.0;
                let rot = Affine {
                    a: rad.cos(),
                    b: rad.sin(),
                    c: -rad.sin(),
                    d: rad.cos(),
                    e: 0.0,
                    f: 0.0,
                };
                if n.len() == 3 {
                    Affine::translate(n[1], n[2])
                        .then(rot)
                        .then(Affine::translate(-n[1], -n[2]))
                } else {
                    rot
                }
            }
            "skewX" => Affine {
                a: 1.0,
                b: 0.0,
                c: (arg(0, 0.0) * PI / 180.0).tan(),
                d: 1.0,
                e: 0.0,
                f: 0.0,
            },
            "skewY" => Affine {
                a: 1.0,
                b: (arg(0, 0.0) * PI / 180.0).tan(),
                c: 0.0,
                d: 1.0,
                e: 0.0,
                f: 0.0,
            },
            _ => Affine::identity(),
        };
        m = m.then(t);
    }
    m
}

// Path data into pen commands; parsing stops at the first error, keeping
// what came before it, as the SVG error rules say.
fn path_data(d: &str) -> Path {
    let mut p = Path::new();
    let mut sc = Scanner::new(d);
    let mut cmd = 0;
    let mut x = 0.0;
    let mut y = 0.0;
    let mut sx = 0.0;
    let mut sy = 0.0;
    // The last control point, for the smooth curve commands.
    let mut cx = 0.0;
    let mut cy = 0.0;
    let mut last = 0;
    loop {
        sc.skip();
        if sc.at_end() {
            break;
        };
        let c = sc.peek();
        let is_letter = (65..=90).contains(&c) || (97..=122).contains(&c);
        if is_letter {
            cmd = c;
            sc.i += 1;
        } else if cmd == 0 {
            break;
        }
        let rel = cmd >= 97;
        let upper = if rel { cmd - 32 } else { cmd };
        let ox = if rel { x } else { 0.0 };
        let oy = if rel { y } else { 0.0 };
        match upper {
            90 => {
                p.close();
                x = sx;
                y = sy;
                last = 90;
                continue;
            }
            77 => {
                let Some(nx) = sc.number() else { break };
                let Some(ny) = sc.number() else { break };
                x = ox + nx;
                y = oy + ny;
                sx = x;
                sy = y;
                p.move_to(x, y);
                // Coordinates after a moveto are implicit linetos.
                cmd = if rel { 108 } else { 76 };
            }
            76 => {
                let Some(nx) = sc.number() else { break };
                let Some(ny) = sc.number() else { break };
                x = ox + nx;
                y = oy + ny;
                p.line_to(x, y)
            }
            72 => {
                let Some(nx) = sc.number() else { break };
                x = ox + nx;
                p.line_to(x, y)
            }
            86 => {
                let Some(ny) = sc.number() else { break };
                y = oy + ny;
                p.line_to(x, y)
            }
            67 | 83 => {
                let mut c1x = x;
                let mut c1y = y;
                if upper == 67 {
                    let Some(a) = sc.number() else { break };
                    let Some(b) = sc.number() else { break };
                    c1x = ox + a;
                    c1y = oy + b;
                } else if last == 67 || last == 83 {
                    c1x = 2.0 * x - cx;
                    c1y = 2.0 * y - cy;
                }
                let Some(a) = sc.number() else { break };
                let Some(b) = sc.number() else { break };
                let Some(e) = sc.number() else { break };
                let Some(f) = sc.number() else { break };
                cx = ox + a;
                cy = oy + b;
                x = ox + e;
                y = oy + f;
                p.cubic_to(c1x, c1y, cx, cy, x, y)
            }
            81 | 84 => {
                if upper == 81 {
                    let Some(a) = sc.number() else { break };
                    let Some(b) = sc.number() else { break };
                    cx = ox + a;
                    cy = oy + b;
                } else if last == 81 || last == 84 {
                    cx = 2.0 * x - cx;
                    cy = 2.0 * y - cy;
                } else {
                    cx = x;
                    cy = y;
                }
                let Some(e) = sc.number() else { break };
                let Some(f) = sc.number() else { break };
                x = ox + e;
                y = oy + f;
                p.quad_to(cx, cy, x, y)
            }
            65 => {
                let Some(rx) = sc.number() else { break };
                let Some(ry) = sc.number() else { break };
                let Some(rot) = sc.number() else { break };
                let Some(large) = sc.flag() else { break };
                let Some(sweep) = sc.flag() else { break };
                let Some(e) = sc.number() else { break };
                let Some(f) = sc.number() else { break };
                arc(&mut p, x, y, rx, ry, rot, large, sweep, ox + e, oy + f);
                x = ox + e;
                y = oy + f;
            }
            _ => break,
        }
        last = upper;
    }
    p
}

// An elliptical arc from (x1, y1) to (x2, y2) as cubic segments of at most
// a quarter turn (SVG implementation notes, F.6.5).
fn arc(
    p: &mut Path,
    x1: f64,
    y1: f64,
    rx_in: f64,
    ry_in: f64,
    rot: f64,
    large: bool,
    sweep: bool,
    x2: f64,
    y2: f64,
) {
    if x1 == x2 && y1 == y2 {
        return;
    };
    let mut rx = rx_in.abs();
    let mut ry = ry_in.abs();
    if rx == 0.0 || ry == 0.0 {
        p.line_to(x2, y2);
        return;
    }
    let phi = rot * PI / 180.0;
    let cp = phi.cos();
    let sp = phi.sin();
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let x1p = cp * dx + sp * dy;
    let y1p = -sp * dx + cp * dy;
    let lambda = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den == 0.0 {
        0.0
    } else {
        ((num / den).max(0.0)).sqrt()
    };
    if large == sweep {
        coef = -coef
    };
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;
    let ccx = cp * cxp - sp * cyp + (x1 + x2) / 2.0;
    let ccy = sp * cxp + cp * cyp + (y1 + y2) / 2.0;
    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;
    let start = uy.atan2(ux);
    let mut delta = (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    if !sweep && delta > 0.0 {
        delta -= 2.0 * PI
    };
    if sweep && delta < 0.0 {
        delta += 2.0 * PI
    };
    let segments = (delta.abs() / (PI / 2.0)).ceil().max(1.0) as i64;
    let step = delta / segments as f64;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let map = |ex: f64, ey: f64| -> (f64, f64) {
        (
            ccx + rx * ex * cp - ry * ey * sp,
            ccy + rx * ex * sp + ry * ey * cp,
        )
    };
    for i in 0..segments {
        let t1 = start + step * i as f64;
        let t2 = t1 + step;
        let (ax, ay) = map(t1.cos() - k * t1.sin(), t1.sin() + k * t1.cos());
        let (bx, by) = map(t2.cos() + k * t2.sin(), t2.sin() - k * t2.cos());
        let (ex, ey) = if i == segments - 1 {
            (x2, y2)
        } else {
            map(t2.cos(), t2.sin())
        };
        p.cubic_to(ax, ay, bx, by, ex, ey)
    }
}

#[cfg(test)]
mod tests {

    fn pixel(p: &super::Picture, x: i64, y: i64) -> (i64, i64, i64, i64) {
        let at = (y * p.width + x) * 4;
        (
            p.rgba[at as usize] as i64,
            p.rgba[(at + 1) as usize] as i64,
            p.rgba[(at + 2) as usize] as i64,
            p.rgba[(at + 3) as usize] as i64,
        )
    }

    #[test]
    fn shapes_fill_with_colors_transforms_and_the_view_box_scale() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\">\n  <rect x=\"0\" y=\"0\" width=\"5\" height=\"10\" fill=\"#ff0000\"/>\n  <g transform=\"translate(5 0)\"><path d=\"m0 0h5v5h-5z\" fill=\"blue\"/></g>\n  <circle cx=\"7.5\" cy=\"7.5\" r=\"2\" style=\"fill:#00ff00\"/>\n</svg>";
        let Ok(p) = super::rasterize(svg, 20) else {
            panic!("the document did not parse");
        };
        assert_eq!((p.width, p.height), (20, 20));
        assert_eq!(pixel(&p, 2, 10), (255, 0, 0, 255));
        assert_eq!(pixel(&p, 15, 2), (0, 0, 255, 255));
        assert_eq!(pixel(&p, 15, 15), (0, 255, 0, 255));
        assert_eq!(pixel(&p, 11, 19).3, 0)
    }

    #[test]
    fn gradients_inherit_through_href_and_follow_their_units() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"100\" height=\"10\">\n  <defs>\n    <linearGradient id=\"base\"><stop offset=\"0\" stop-color=\"#000\"/><stop offset=\"1\" stop-color=\"#fff\"/></linearGradient>\n    <linearGradient id=\"user\" xlink:href=\"#base\" gradientUnits=\"userSpaceOnUse\" x1=\"0\" x2=\"100\"/>\n  </defs>\n  <rect width=\"100\" height=\"10\" fill=\"url(#user)\"/>\n</svg>";
        let Ok(p) = super::rasterize(svg, 100) else {
            panic!("the document did not parse");
        };
        let (r, _, _, a) = pixel(&p, 50, 50);
        assert_eq!(a, 255);
        assert!((r - 128).abs() <= 2, "{}", r);
        assert!(pixel(&p, 1, 50).0 < 8)
    }

    #[test]
    fn class_rules_and_current_color_reach_the_shapes() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 4 4\">\n  <style>.ColorScheme-Text { color: #232629; } /* a comment */ .lit { fill: #11223a !important }</style>\n  <rect class=\"ColorScheme-Text\" width=\"4\" height=\"2\" fill=\"currentColor\"/>\n  <rect class=\"lit\" y=\"2\" width=\"4\" height=\"2\" fill=\"#000\"/>\n</svg>";
        let Ok(p) = super::rasterize(svg, 4) else {
            panic!("the document did not parse");
        };
        assert_eq!(pixel(&p, 1, 1), (0x23, 0x26, 0x29, 255));
        assert_eq!(pixel(&p, 1, 3), (0x11, 0x22, 0x3a, 255))
    }

    #[test]
    fn packed_path_numbers_and_arc_flags_parse() {
        let p = super::path_data("M1-2.5.5-1L3,4a1 1 0 01 2 2z");
        assert!(p.cmds.len() > 10);
        assert_eq!(super::numbers("1-2.5.5e1"), vec![1.0, -2.5, 5.0]);
        let m = super::transform("translate(0-28) scale(2)");
        assert_eq!(m.apply(1.0, 1.0), (2.0, -26.0))
    }

    #[test]
    fn clip_paths_and_group_opacity_limit_what_shows() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\">\n  <clipPath id=\"left\"><rect width=\"5\" height=\"10\"/></clipPath>\n  <rect width=\"10\" height=\"10\" fill=\"#fff\" clip-path=\"url(#left)\"/>\n  <g opacity=\"0.5\"><rect x=\"5\" width=\"5\" height=\"10\" fill=\"#000\"/></g>\n</svg>";
        let Ok(p) = super::rasterize(svg, 10) else {
            panic!("the document did not parse");
        };
        assert_eq!(pixel(&p, 2, 5), (255, 255, 255, 255));
        let (r, _, _, a) = pixel(&p, 7, 5);
        assert!(
            (a - 128).abs() <= 1 && r == 0,
            "{:?} {:?} {:?}",
            pixel(&p, 7, 5),
            pixel(&p, 5, 5),
            pixel(&p, 9, 0)
        )
    }

    #[test]
    fn a_document_without_an_svg_root_is_an_error() {
        assert!(super::rasterize("<html/>", 16).is_err());
        assert!(super::rasterize("not xml <", 16).is_err())
    }
}
