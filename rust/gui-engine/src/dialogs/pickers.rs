// Picker controllers own drafts. Exactly one terminal outcome is delivered by
// the host; live previews and explicit Apply have their own message variants.
use crate::app::effects::Effect;
use crate::dialogs::Outcome;
use crate::dialogs::host::Instance;
use crate::dialogs::request::{
    ButtonRole, DateMode, DialogAction, DialogRequest, PreferenceField, PreferenceKind,
};
use crate::services::palette::ColorPalette;
use crate::strext::StrExt;
use crate::style::color::Color;
use crate::style::{TextRole, Theme};
use crate::widgets::colorpicker::ColorPicker;
use crate::widgets::cx::UiMsg;
use crate::widgets::state::StateStore;
use crate::widgets::temporal::{Calendar, ClockTime, Date};
use crate::widgets::{Custom, Tone, Widget};
use std::collections::HashMap;

fn cid(inst: &Instance, part: &str) -> String {
    format!("dlg:{}:{}", inst.id, part)
}
fn value(inst: &Instance, key: &str) -> String {
    inst.values.get(key).map_or("", String::as_str).to_string()
}
fn fields(inst: &Instance) -> Vec<PreferenceField> {
    let mut out = vec![];
    match inst.request {
        DialogRequest::Preferences { ref pages, .. } => {
            for p in pages {
                for f in &p.fields {
                    out.push(f.clone())
                }
            }
        }
        DialogRequest::Wizard { ref pages, .. } => {
            for p in pages {
                for f in &p.fields {
                    out.push(f.clone())
                }
            }
        }
        DialogRequest::Form { ref fields, .. } => out.extend(fields.iter().cloned()),
        _ => (),
    }
    out
}
fn draft(inst: &Instance) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for f in fields(inst) {
        out.insert(f.key.to_string(), value(inst, &format!("field:{}", f.key)));
    }
    out
}
fn color_model(inst: &Instance) -> ColorPicker {
    let DialogRequest::Color { initial, alpha, .. } = inst.request else {
        return ColorPicker::new(&cid(inst, "color"), Color::black());
    };
    let mut p = ColorPicker::new(&cid(inst, "color"), initial);
    p.alpha = alpha;
    p.value = Color::parse_css(&value(inst, "color:value")).unwrap_or(initial);
    p.hue = value(inst, "color:hue").to_f64().unwrap_or(p.hue);
    p.draft = inst.text.clone();
    p.error = inst.error.clone();
    p
}
fn save_color(inst: Instance, p: &ColorPicker) -> Instance {
    let mut n = inst;
    n.text = p.draft.clone();
    n.error = p.error.clone();
    n.values.insert("color:value".to_string(), p.value.css());
    n.values
        .insert("color:hue".to_string(), format!("{}", p.hue));
    n
}
pub fn initialize(inst: Instance) -> Instance {
    let mut n = inst;
    match n.request {
        DialogRequest::File { .. } => return crate::dialogs::files::initialize(n),
        DialogRequest::Color { initial, alpha, .. } => {
            let mut p = ColorPicker::new(&cid(&n, "color"), initial);
            p.alpha = alpha;
            if !alpha {
                p.value.a = 255;
                p.draft = p.value.css()
            };
            n = save_color(n, &p);
            match ColorPalette::load(&crate::services::settings_path(&n.app_id, "color-swatches")) {
                Ok(book) => n
                    .values
                    .insert("palette".to_string(), crate::services::to_json(&book)),
                Err(e) => n.values.insert(
                    "palette:error".to_string(),
                    crate::localization::message(
                        "color",
                        "Cannot load swatches: {error}",
                        1,
                        &crate::hmap! { "error" => format!("{}", e) },
                    ),
                ),
            };
        }
        DialogRequest::Font { ref initial, .. } => {
            let parts = initial.split("|").collect::<Vec<_>>();
            n.values.insert(
                "font:family".to_string(),
                if parts.is_empty() || parts[0].is_empty() {
                    "DejaVu Sans".to_string()
                } else {
                    parts[0].to_string()
                },
            );
            n.values.insert(
                "font:style".to_string(),
                if parts.len() > 1 {
                    parts[1].to_string()
                } else {
                    "Regular".to_string()
                },
            );
            n.values.insert(
                "font:size".to_string(),
                if parts.len() > 2 {
                    parts[2].to_string()
                } else {
                    "14".to_string()
                },
            );
            if parts.len() > 3 {
                for setting in parts[3].split(",") {
                    let Some((tag, v)) = setting.split_once("=") else {
                        continue;
                    };
                    n.values
                        .insert(format!("font:axis:{}", tag).to_string(), v.to_string());
                }
            }
            n.values.insert(
                "font:sample".to_string(),
                crate::localization::tr("The quick brown fox jumps over the lazy dog. 0123456789"),
            );
            // The installed catalog loads off the UI thread; the list grows when it arrives.
            if !crate::text::fonts::installed_known() {
                n.values
                    .insert("font:catalog".to_string(), "loading".to_string());
                crate::perform(Effect::LoadFonts(cid(&n, "catalog")))
            }
        }
        DialogRequest::Date {
            ref initial, mode, ..
        } => {
            let p = initial.split("T").collect::<Vec<_>>();
            n.values.insert(
                "date".to_string(),
                if mode == DateMode::Time {
                    "2026-01-01".to_string()
                } else {
                    p[0].to_string()
                },
            );
            n.values.insert(
                "time".to_string(),
                if mode == DateMode::Time {
                    initial.to_string()
                } else if p.len() > 1 {
                    p[1].to_string()
                } else {
                    "00:00:00".to_string()
                },
            );
        }
        _ => (),
    }
    for f in fields(&n) {
        n.values
            .insert(format!("field:{}", f.key).to_string(), f.value.clone());
        n.values
            .insert(format!("baseline:{}", f.key).to_string(), f.value);
    }
    n
}
pub fn actions(inst: &Instance) -> Vec<DialogAction> {
    if let DialogRequest::File { .. } = inst.request {
        return crate::dialogs::files::actions(inst);
    };
    let mut out = vec![DialogAction::new("cancel", "Cancel", ButtonRole::Reject)];
    // While the application checks a step, the only choices are to stop
    // waiting or to close.
    if waiting(inst) {
        out.push(DialogAction::new("stop", "Stop", ButtonRole::Other));
        return out;
    }
    match inst.request {
        DialogRequest::Preferences { .. } => {
            out.push(DialogAction::new(
                "defaults",
                "Restore defaults",
                ButtonRole::Other,
            ));
            out.push(DialogAction::new("apply", "Apply", ButtonRole::Apply));
            out.push(DialogAction::new("ok", "OK", ButtonRole::Accept))
        }
        DialogRequest::Wizard { ref pages, .. } => {
            if inst.page > 0 {
                out.push(DialogAction::new("back", "Back", ButtonRole::Other))
            };
            let next = next_page(inst, 1);
            out.push(DialogAction::new(
                if next < pages.len() as i64 {
                    "next"
                } else {
                    "finish"
                },
                if next < pages.len() as i64 {
                    "Next"
                } else {
                    "Finish"
                },
                ButtonRole::Accept,
            ))
        }
        DialogRequest::Form { ref accept, .. } => {
            out.push(DialogAction::new("ok", accept, ButtonRole::Accept))
        }
        _ => out.push(DialogAction::new("ok", "OK", ButtonRole::Accept)),
    }
    out
}
pub fn initial_focus(inst: &Instance) -> String {
    match inst.request {
        DialogRequest::File { .. } => cid(inst, "filename"),
        DialogRequest::Color { .. } => cid(inst, "color:css"),
        DialogRequest::Font { .. } => cid(inst, "font:family"),
        DialogRequest::Date {
            mode: DateMode::Time,
            ..
        } => cid(inst, "time"),
        DialogRequest::Date { .. } => cid(inst, "date"),
        _ => {
            let shown = shown_fields(inst);
            if shown.is_empty() {
                cid(inst, "action:ok")
            } else {
                cid(inst, &format!("field:{}", shown[0].key))
            }
        }
    }
}
fn editor(inst: &Instance, key: &str, title: &str, text: &str) -> Widget {
    let mut field = crate::widgets::textinput::TextInput::new(
        &cid(inst, key),
        text,
        "",
        crate::widgets::textinput::InputKind::Line,
    );
    field.label = title.to_string();
    stacked(&[Widget::label(title), Widget::input(&field)], 4)
}

// A column whose children take its whole width, as a form's fields do.
pub(crate) fn stacked(children: &[Widget], spacing: i64) -> Widget {
    Widget::flex(
        crate::widgets::Axis::Vertical,
        children,
        spacing,
        crate::widgets::MainAlign::Start,
        crate::widgets::CrossAlign::Stretch,
    )
}

// A settings dialog this small reads at a glance; search helps a long one.
const SEARCHABLE_FIELDS: usize = 12;
fn shown_fields(inst: &Instance) -> Vec<PreferenceField> {
    match inst.request {
        DialogRequest::Preferences { ref pages, .. } => {
            let search = value(inst, "search").to_lowercase();
            if !search.is_empty() {
                return fields(inst)
                    .iter()
                    .filter(|f| {
                        f.label.to_lowercase().contains(&search)
                            || f.help.to_lowercase().contains(&search)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
            }
            if pages.is_empty() {
                vec![]
            } else {
                pages[(inst.page.max(0).min(pages.len() as i64 - 1)) as usize]
                    .fields
                    .clone()
            }
        }
        DialogRequest::Wizard { ref pages, .. } => {
            if pages.is_empty() {
                vec![]
            } else {
                pages[(inst.page.max(0).min(pages.len() as i64 - 1)) as usize]
                    .fields
                    .clone()
            }
        }
        DialogRequest::Form { ref fields, .. } => fields.clone(),
        _ => vec![],
    }
}
fn field_view(inst: &Instance, field: &PreferenceField) -> Widget {
    let id = cid(inst, &format!("field:{}", field.key));
    let text = value(inst, &format!("field:{}", field.key));
    let control = match field.kind {
        PreferenceKind::Toggle => Widget::checkbox(&id, &field.label, text == "true"),
        PreferenceKind::Choice(ref choices) => {
            let items = choices
                .iter()
                .map(|p| crate::widgets::completion::CompletionItem::new(&p.0, &p.1))
                .collect::<Vec<_>>();
            stacked(
                &[
                    Widget::label(&field.label),
                    Widget::select(&id, &items, &text, &field.label),
                ],
                4,
            )
        }
        // A number keeps its box's own width; the label column sets none.
        PreferenceKind::Number(min, max) => Widget::column(
            &[
                Widget::label(&field.label),
                Widget::spin_box(&id, text.to_f64().unwrap_or(min), min, max, 1.0, 2)
                    .accessible(&field.label),
            ],
            4,
        ),
        PreferenceKind::Integer(min, max) => Widget::column(
            &[
                Widget::label(&field.label),
                Widget::spin_box(
                    &id,
                    text.to_f64().unwrap_or(min as f64),
                    min as f64,
                    max as f64,
                    1.0,
                    0,
                )
                .accessible(&field.label),
            ],
            4,
        ),
        _ => editor(inst, &format!("field:{}", field.key), &field.label, &text),
    };
    if field.help.is_empty() {
        return control;
    }
    stacked(&[control, Widget::muted(&field.help)], 2)
}
pub fn body(inst: &Instance, theme: &Theme) -> Widget {
    let mut rows = vec![];
    if value(inst, "discard-review") == "true" {
        rows.push(Widget::paragraph(&crate::localization::tr(
            "Discard unapplied settings changes?",
        )));
        rows.push(Widget::row(
            &[
                Widget::danger_button(
                    &cid(inst, "discard"),
                    &crate::localization::tr("Discard changes"),
                ),
                Widget::button(
                    &cid(inst, "keep-editing"),
                    &crate::localization::tr("Keep editing"),
                ),
            ],
            8,
        ))
    }
    match inst.request {
        DialogRequest::File { .. } => return crate::dialogs::files::body(inst),
        DialogRequest::Color { .. } => {
            rows.push(color_model(inst).view());
            rows.push(palette_view(inst))
        }
        DialogRequest::Date {
            mode,
            ref min,
            ref max,
            ..
        } => {
            if mode != DateMode::Time {
                let date = Date::parse(&value(inst, "date")).unwrap_or(Date {
                    year: 2026,
                    month: 1,
                    day: 1,
                });
                let mut cal = Calendar::new(&cid(inst, "calendar"), date);
                cal.min = Date::parse(min.split("T").next().unwrap_or_default()).unwrap_or(cal.min);
                cal.max = Date::parse(max.split("T").next().unwrap_or_default()).unwrap_or(cal.max);
                rows.push(Widget::Calendar(cal));
                rows.push(editor(
                    inst,
                    "date",
                    &crate::localization::tr("Date (YYYY-MM-DD)"),
                    &value(inst, "date"),
                ))
            }
            if mode != DateMode::Date {
                rows.push(editor(
                    inst,
                    "time",
                    &crate::localization::tr("Time (HH:MM:SS)"),
                    &value(inst, "time"),
                ))
            }
        }
        DialogRequest::Font { .. } => {
            let family = value(inst, "font:family");
            let style = value(inst, "font:style");
            let size = value(inst, "font:size")
                .to_f64()
                .unwrap_or(14.0)
                .clamp(4.0, 256.0);
            let axes = crate::text::fonts::family_axes(&family);
            let settings = axis_settings(inst, &axes);
            let mut sample = theme.font(TextRole::Body);
            sample.family = crate::text::fonts::Family::Named(crate::text::fonts::instance_family(
                &family, &settings,
            ));
            sample.size = size * theme.scale * theme.text_scale;
            sample.bold = style.contains("Bold") && !settings.contains_key("wght");
            sample.italic = style.contains("Italic");
            let text = value(inst, "font:sample");
            let value = text.clone();
            let preview = Custom {
                id: cid(inst, "font:preview"),
                focusable: false,
                role: crate::accessibility::Role::Label,
                label: text.clone(),
                measure: crate::Callback::new(std::rc::Rc::new(move |c, t| {
                    c.constrain(crate::geometry::Size::new(t.px(400), t.px(96)))
                })),
                draw: crate::Callback::new(std::rc::Rc::new(move |_s, t| {
                    vec![crate::render::display::DrawOp::Text {
                        x: 4,
                        y: 4,
                        text: value.clone(),
                        style: sample.clone(),
                        color: t.text,
                    }]
                })),
                input: crate::Callback::new(std::rc::Rc::new(move |_i| vec![])),
                captures_input: false,
            };
            let mut families = vec![];
            for name in crate::text::fonts::known_families() {
                families.push(crate::widgets::completion::CompletionItem::new(
                    &name, &name,
                ))
            }
            rows.push(
                Widget::combo(
                    &cid(inst, "font:family"),
                    &family,
                    &families,
                    false,
                    &crate::localization::tr("Search font families"),
                )
                .accessible(&crate::localization::tr("Font family")),
            );
            let loading = self::value(inst, "font:catalog") == "loading";
            if loading {
                rows.push(Widget::text(
                    &crate::localization::tr("Loading installed fonts\u{2026}"),
                    TextRole::Small,
                    Tone::Muted,
                ))
            } else if !crate::text::fonts::family_known(&family) {
                rows.push(Widget::text(
                    &crate::localization::tr(
                        "This font is not installed, so the preview uses the default font.",
                    ),
                    TextRole::Small,
                    Tone::Warning,
                ))
            };
            let styles = ["Regular", "Bold", "Italic", "Bold Italic"]
                .iter()
                .map(|s| {
                    crate::widgets::completion::CompletionItem::new(s, &crate::localization::tr(s))
                })
                .collect::<Vec<_>>();
            rows.push(Widget::select(
                &cid(inst, "font:style"),
                &styles,
                &style,
                &crate::localization::tr("Style"),
            ));
            rows.push(
                Widget::spin_box(&cid(inst, "font:size"), size, 4.0, 256.0, 1.0, 0)
                    .accessible(&crate::localization::tr("Font size")),
            );
            for axis in axes {
                let current = settings.get(&axis.tag).unwrap_or(&axis.default);
                let name = axis_name(&axis.tag);
                let step = if axis.tag == "wght" || axis.tag == "opsz" {
                    1.0
                } else {
                    0.1
                };
                rows.push(Widget::row(
                    &[
                        Widget::label(&format!("{}: {}", name, current)).width(160),
                        Widget::slider(
                            &cid(inst, &format!("font:axis:{}", axis.tag)),
                            *current,
                            axis.min,
                            axis.max,
                            step,
                        )
                        .accessible(&name)
                        .grow(1),
                    ],
                    8,
                ))
            }
            rows.push(editor(
                inst,
                "font:sample",
                &crate::localization::tr("Sample"),
                &text,
            ));
            rows.push(Widget::Custom(preview))
        }
        DialogRequest::Preferences { ref pages, .. } => {
            if fields(inst).len() > SEARCHABLE_FIELDS {
                rows.push(editor(
                    inst,
                    "search",
                    &crate::localization::tr("Search settings"),
                    &value(inst, "search"),
                ));
            }
            if pages.len() > 1 {
                let mut nav = vec![];
                for (i, p) in pages.iter().enumerate() {
                    nav.push(Widget::flat_button(
                        &cid(inst, &format!("page:{}", i)),
                        &p.title,
                    ));
                }
                rows.push(Widget::flow(&nav, 4));
            }
            for field in shown_fields(inst) {
                rows.push(field_view(inst, &field))
            }
        }
        DialogRequest::Form { ref fields, .. } => {
            for field in fields {
                rows.push(field_view(inst, field))
            }
        }
        DialogRequest::Wizard { ref pages, .. } if !pages.is_empty() => {
            let page = &pages[(inst.page.max(0).min(pages.len() as i64 - 1)) as usize];
            rows.push(Widget::heading(&page.title));
            rows.push(Widget::paragraph(&page.description));
            for field in &page.fields {
                rows.push(field_view(inst, field))
            }
        }
        _ => (),
    }
    if !inst.error.is_empty() {
        rows.push(Widget::text(&inst.error, TextRole::Small, Tone::Danger))
    };
    // The drafts being checked stay as they were sent.
    Widget::scroll_to_fit(
        &cid(inst, "content"),
        stacked(&rows, 8).disabled(waiting(inst)),
    )
    .max_height(600)
}
fn validate_fields(inst: &Instance, fields: &[PreferenceField]) -> Option<String> {
    for f in fields {
        let text = value(inst, &format!("field:{}", f.key));
        match f.kind {
            PreferenceKind::Number(min, max) => match text.to_f64() {
                Some(n) => {
                    if !(n >= min && n <= max) {
                        return Some(crate::localization::message(
                            "preferences",
                            "{field}: enter a number from {min} to {max}",
                            1,
                            &crate::hmap! { "field" => f.label.clone(), "min" => format!("{}", min), "max" => format!("{}", max) },
                        ));
                    }
                }
                None => {
                    return Some(crate::localization::message(
                        "preferences",
                        "{field}: enter a number",
                        1,
                        &crate::hmap! { "field" => f.label.clone() },
                    ));
                }
            },
            PreferenceKind::Integer(min, max) => match text.parse::<i64>() {
                Ok(n) if (min..=max).contains(&n) => (),
                _ => {
                    return Some(crate::localization::message(
                        "preferences",
                        "{field}: enter a whole number from {min} to {max}",
                        1,
                        &crate::hmap! { "field" => f.label.clone(), "min" => format!("{}", min), "max" => format!("{}", max) },
                    ));
                }
            },
            PreferenceKind::Color if Color::parse_css(&text).is_none() => {
                return Some(crate::localization::message(
                    "preferences",
                    "{field}: enter a CSS color",
                    1,
                    &crate::hmap! { "field" => f.label.clone() },
                ));
            }
            PreferenceKind::Toggle if text != "true" && text != "false" => {
                return Some(crate::localization::message(
                    "preferences",
                    "{field}: choose on or off",
                    1,
                    &crate::hmap! { "field" => f.label.clone() },
                ));
            }
            PreferenceKind::Choice(ref choices) if !choices.iter().any(|c| c.0 == text) => {
                return Some(crate::localization::message(
                    "preferences",
                    "{field}: choose an available value",
                    1,
                    &crate::hmap! { "field" => f.label.clone() },
                ));
            }
            _ => (),
        }
    }
    None
}
fn next_page(inst: &Instance, direction: i64) -> i64 {
    let DialogRequest::Wizard { ref pages, .. } = inst.request else {
        return inst.page;
    };
    let mut i = inst.page + direction;
    while i >= 0 && i < pages.len() as i64 {
        let skip = &pages[i as usize].skip_if;
        if skip.is_empty() || value(inst, &format!("field:{}", skip)) != "true" {
            break;
        };
        i += direction;
    }
    i
}
// The axis values set away from their defaults, by tag.
fn axis_settings(inst: &Instance, axes: &[crate::text::variation::Axis]) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for axis in axes {
        let Some(v) = value(inst, &format!("font:axis:{}", axis.tag)).to_f64() else {
            continue;
        };
        let clamped = v.max(axis.min).min(axis.max);
        if clamped != axis.default {
            out.insert(axis.tag.to_string(), clamped);
        }
    }
    out
}

fn axis_name(tag: &str) -> String {
    match tag {
        "wght" => crate::localization::tr("Weight"),
        "wdth" => crate::localization::tr("Width"),
        "slnt" => crate::localization::tr("Slant"),
        "ital" => crate::localization::tr("Italic"),
        "opsz" => crate::localization::tr("Optical size"),
        _ => tag.to_string(),
    }
}

// `family|style|size`, then `|tag=value,..` for the variation axes set away
// from their defaults, in tag order.
fn font_value(inst: &Instance) -> Result<String, String> {
    let family_raw = value(inst, "font:family");
    let family = family_raw.trim();
    let size = value(inst, "font:size")
        .to_f64()
        .ok_or(crate::localization::tr("Enter a font size"))?;
    if !(4.0..=256.0).contains(&size) {
        return Err(crate::localization::tr("Font size must be from 4 to 256"));
    };
    if family.is_empty() {
        return Err(crate::localization::tr("Choose a font family"));
    };
    if !crate::text::fonts::family_known(family) {
        return Err(crate::localization::tr(
            if value(inst, "font:catalog") == "loading" {
                "Installed fonts are still loading."
            } else {
                "This font is unavailable. Choose an installed or registered family."
            },
        ));
    }
    let style = value(inst, "font:style");
    if !["Regular", "Bold", "Italic", "Bold Italic"].contains(&style.as_str()) {
        return Err(crate::localization::tr("Choose a font style"));
    };
    let settings = axis_settings(inst, &crate::text::fonts::family_axes(family));
    if settings.is_empty() {
        return Ok(format!("{}|{}|{}", family, style, size));
    };
    let mut tags: Vec<&String> = settings.keys().collect();
    tags.sort();
    Ok(format!(
        "{}|{}|{}|{}",
        family,
        style,
        size,
        tags.iter()
            .map(|t| format!("{}={}", t, settings[*t]))
            .collect::<Vec<_>>()
            .join(",")
    ))
}
pub fn click(
    inst: Instance,
    part: &str,
    st: &mut StateStore,
) -> (Instance, Option<Outcome>, Vec<Effect>) {
    message(inst.clone(), &UiMsg::Click(cid(&inst, part)), part, st)
}
pub fn message(
    inst: Instance,
    m: &UiMsg,
    part: &str,
    st: &mut StateStore,
) -> (Instance, Option<Outcome>, Vec<Effect>) {
    if let UiMsg::Click(_) = m
        && matches!(inst.request, DialogRequest::Preferences { .. })
        && value(&inst, "discard-review") == "true"
    {
        if part == "discard" {
            return (inst, Some(Outcome::Cancelled), vec![]);
        };
        if part == "keep-editing" {
            let mut n = inst;
            n.values
                .insert("discard-review".to_string(), "".to_string());
            return (n, None, vec![]);
        }
    }
    if let DialogRequest::File { .. } = inst.request {
        return crate::dialogs::files::message(inst, m, part, st);
    };
    let mut n = inst.clone();
    let mut fx = vec![];
    if let DialogRequest::Color { .. } = inst.request {
        let mut p = color_model(&inst);
        let mut book = palette(&inst);
        let mut control = m.clone();
        if part == "swatch-name" {
            if let UiMsg::Text(_, text) = m {
                n.values.insert(part.to_string(), text.to_string());
            };
            return (n, None, fx);
        };
        if let UiMsg::Click(_) = m {
            if part == "copy-css" {
                fx.push(Effect::SetClipboard(p.value.css()));
                return (n, None, fx);
            };
            if part == "swatch-add" || part == "swatch-rename" || part == "swatch-remove" {
                let selected = value(&n, "swatch:selected");
                if part == "swatch-add" {
                    match book.add(&value(&n, "swatch-name"), p.value) {
                        Ok(id) => {
                            n.values.insert("swatch:selected".to_string(), id);
                        }
                        Err(e) => {
                            n.values
                                .insert("palette:error".to_string(), format!("{}", e));
                            return (n, None, fx);
                        }
                    }
                } else if part == "swatch-rename" {
                    if !book.rename(&selected, &value(&n, "swatch-name")) {
                        n.values.insert(
                            "palette:error".to_string(),
                            crate::localization::tr("Enter a swatch name"),
                        );
                        return (n, None, fx);
                    }
                } else {
                    book.remove(&selected);
                    n.values
                        .insert("swatch:selected".to_string(), "".to_string());
                };
                n = persist_palette(n, &book);
                return (n, None, fx);
            }
            if part.starts_with("swatch:") {
                let id = part.substring(7, part.byte_len());
                for swatch in &book.favorites {
                    if swatch.id == id {
                        n.values
                            .insert("swatch:selected".to_string(), id.to_string());
                        n.values
                            .insert("swatch-name".to_string(), swatch.name.clone());
                        control = UiMsg::Text(cid(&inst, "color:css"), swatch.css.clone())
                    }
                }
            }
            if part.starts_with("recent:") {
                let i = part.substring(7, part.byte_len()).to_i64().unwrap_or(-1);
                if i >= 0 && i < book.recent.len() as i64 {
                    control = UiMsg::Text(cid(&inst, "color:css"), book.recent[i as usize].clone())
                }
            }
        }
        if let Some(color) = p.update(&control) {
            fx.push(Effect::Emit(UiMsg::DialogPreview(
                inst.id.clone(),
                Outcome::Color(color),
            )))
        };
        let selected = value(&n, "swatch:selected");
        let name = value(&n, "swatch-name");
        n = save_color(n, &p);
        n.values.insert("swatch:selected".to_string(), selected);
        n.values.insert("swatch-name".to_string(), name);
        return (n, None, fx);
    }
    match m {
        UiMsg::Click(_) if part.starts_with("page:") => {
            n.page = part.substring(5, part.byte_len()).to_i64().unwrap_or(0);
            n.error = "".to_string()
        }
        UiMsg::Text(_, text) => {
            n.values.insert(part.to_string(), text.to_string());
            n.error = "".to_string()
        }
        UiMsg::Value(_, v) => {
            n.values.insert(part.to_string(), format!("{}", v));
            n.error = "".to_string()
        }
        UiMsg::Choose(_, key) | UiMsg::Select(_, key) => {
            n.values.insert(
                if part == "font:choose" {
                    "font:family"
                } else {
                    part
                }
                .to_string(),
                key.to_string(),
            );
            n.error = "".to_string()
        }
        UiMsg::Toggle(_, on) => {
            n.values.insert(
                part.to_string(),
                if *on {
                    "true".to_string()
                } else {
                    "false".to_string()
                },
            );
            n.error = "".to_string()
        }
        UiMsg::Date(_, date) => {
            n.values.insert("date".to_string(), date.to_string());
            n.error = "".to_string()
        }
        UiMsg::Submit(_, _) => return action(n, "ok", &fx),
        UiMsg::FontsLoaded(_, records) if part == "catalog" => {
            crate::text::fonts::adopt_installed(records);
            n.values.insert("font:catalog".to_string(), "".to_string());
        }
        _ => (),
    }
    if let DialogRequest::Font { .. } = n.request
        && font_value(&n).ok() != font_value(&inst).ok()
        && let Ok(font) = font_value(&n)
    {
        fx.push(Effect::Emit(UiMsg::DialogPreview(
            n.id.clone(),
            Outcome::Font(font),
        )))
    }
    (n, None, fx)
}
// Whether `inst` is checked: it asks the application before a step.
fn is_checked(inst: &Instance) -> bool {
    match inst.request {
        DialogRequest::Preferences { checked, .. } | DialogRequest::Wizard { checked, .. } => {
            checked
        }
        _ => false,
    }
}

/// Whether a check the application was asked for is outstanding.
pub(crate) fn waiting(inst: &Instance) -> bool {
    !value(inst, "check:step").is_empty()
}

// Asks the application about `step` with `values` instead of taking it now.
fn hold(
    inst: Instance,
    step: &str,
    values: &HashMap<String, String>,
    fx: &[Effect],
) -> (Instance, Option<Outcome>, Vec<Effect>) {
    let mut n = inst;
    let mut effects = fx.to_vec();
    let request = value(&n, "check:request").to_i64().unwrap_or(0) + 1;
    n.values
        .insert("check:request".to_string(), format!("{}", request));
    n.values.insert("check:step".to_string(), step.to_string());
    n.error = "".to_string();
    effects.push(Effect::Emit(UiMsg::DialogCheck(
        n.id.clone(),
        request,
        step.to_string(),
        Outcome::Values(values.clone()),
    )));
    (n, None, effects.to_vec())
}

/// Takes the application's answer to check `request`: "" carries out the
/// step that waited, anything else is shown and leaves the drafts as they
/// are. An answer to a stopped or earlier check changes nothing.
pub fn check_answered(
    inst: Instance,
    request: i64,
    error: &str,
) -> (Instance, Option<Outcome>, Vec<Effect>) {
    let mut n = inst;
    if !waiting(&n) || value(&n, "check:request") != format!("{}", request) {
        return (n, None, vec![]);
    };
    let step = value(&n, "check:step");
    n.values.insert("check:step".to_string(), "".to_string());
    if !error.is_empty() {
        n.error = error.to_string();
        return (n, None, vec![]);
    }
    n.values
        .insert("check:approved".to_string(), "true".to_string());
    let (next, outcome, effects) = action(n, &step, &[]);
    let mut done = next;
    done.values
        .insert("check:approved".to_string(), "".to_string());
    (done, outcome, effects)
}

pub fn action(
    inst: Instance,
    action: &str,
    fx: &[Effect],
) -> (Instance, Option<Outcome>, Vec<Effect>) {
    if let DialogRequest::File { .. } = inst.request {
        return crate::dialogs::files::accept(inst, false);
    };
    let mut n = inst.clone();
    let mut effects = fx.to_vec();
    if action == "stop" {
        if waiting(&n) {
            let request = value(&n, "check:request").to_i64().unwrap_or(0);
            n.values.insert("check:step".to_string(), "".to_string());
            effects.push(Effect::Emit(UiMsg::DialogCheckStopped(
                n.id.clone(),
                request,
            )))
        }
        return (n, None, effects.to_vec());
    }
    if waiting(&n) {
        return (n, None, effects.to_vec());
    };
    let ask = is_checked(&n) && value(&n, "check:approved") != "true";
    match &inst.request {
        DialogRequest::Color { .. } => {
            let p = color_model(&inst);
            if !p.error.is_empty() {
                return (n, None, effects.to_vec());
            };
            let mut book = palette(&n);
            book.remember(p.value);
            n = persist_palette(n, &book);
            (n, Some(Outcome::Color(p.value)), effects.to_vec())
        }
        DialogRequest::Font { .. } => match font_value(&n) {
            Ok(font) => (n, Some(Outcome::Font(font)), effects.to_vec()),
            Err(why) => {
                n.error = why;
                (n, None, effects.to_vec())
            }
        },
        DialogRequest::Date { mode, min, max, .. } => {
            let mode = *mode;
            let mut text = value(&n, "date");
            let date = Date::parse(&text);
            let clock = ClockTime::parse(&value(&n, "time"));
            if (mode != DateMode::Time && date.is_none())
                || (mode != DateMode::Date && clock.is_none())
            {
                n.error = crate::localization::tr("Enter a valid date and time");
                return (n, None, effects.to_vec());
            }
            if mode == DateMode::Time {
                text = clock.unwrap().iso()
            } else if mode == DateMode::DateTime {
                text = format!("{}T{}", date.unwrap().iso(), clock.unwrap().iso())
            };
            let kind = match mode {
                DateMode::Date => crate::widgets::temporal::TemporalKind::DateOnly,
                DateMode::Time => crate::widgets::temporal::TemporalKind::TimeOnly,
                DateMode::DateTime => crate::widgets::temporal::TemporalKind::DateAndTime,
            };
            let mut bounds = crate::widgets::temporal::TemporalEditor::new("", &text, kind);
            bounds.min = min.clone();
            bounds.max = max.clone();
            if bounds.canonical(&text).is_none() {
                let template = match mode {
                    DateMode::Date => "Choose a date from {min} to {max}",
                    DateMode::Time => "Choose a time from {min} to {max}",
                    DateMode::DateTime => "Choose a date and time from {min} to {max}",
                };
                n.error = crate::localization::message(
                    "date",
                    template,
                    1,
                    &crate::hmap! { "min" => min, "max" => max },
                );
                return (n, None, effects.to_vec());
            }
            (n, Some(Outcome::Date(text)), effects.to_vec())
        }
        DialogRequest::Preferences { .. } => {
            if action == "defaults" {
                for f in fields(&n) {
                    n.values
                        .insert(format!("field:{}", f.key).to_string(), f.default);
                }
                return (n, None, effects.to_vec());
            }
            if let Some(why) = validate_fields(&n, &fields(&n)) {
                n.error = why;
                return (n, None, effects.to_vec());
            };
            let values = draft(&n);
            if ask {
                return hold(n, action, &values, &effects);
            };
            if action == "apply" {
                let mut changed = false;
                for f in fields(&n) {
                    if value(&n, &format!("baseline:{}", f.key))
                        != value(&n, &format!("field:{}", f.key))
                    {
                        changed = true
                    };
                    n.values.insert(
                        format!("baseline:{}", f.key).to_string(),
                        value(&n, &format!("field:{}", f.key)),
                    );
                }
                if changed {
                    effects.push(Effect::Emit(UiMsg::DialogApplied(
                        n.id.clone(),
                        Outcome::Values(values),
                    )))
                };
                return (n, None, effects.to_vec());
            }
            (n, Some(Outcome::Values(values)), effects.to_vec())
        }
        DialogRequest::Form { .. } => {
            if let Some(why) = validate_fields(&n, &fields(&n)) {
                n.error = why;
                return (n, None, effects.to_vec());
            };
            (
                n.clone(),
                Some(Outcome::Values(draft(&n))),
                effects.to_vec(),
            )
        }
        DialogRequest::Wizard { pages, .. } => {
            if action == "back" {
                n.page = next_page(&n, -1).max(0);
                n.error = "".to_string();
                return (n, None, effects.to_vec());
            };
            if let Some(why) = validate_fields(&n, &shown_fields(&n)) {
                n.error = why;
                return (n, None, effects.to_vec());
            };
            let next = next_page(&n, 1);
            if ask {
                return hold(
                    n.clone(),
                    if next < pages.len() as i64 {
                        "next"
                    } else {
                        "finish"
                    },
                    &draft(&n),
                    &effects,
                );
            };
            if (action == "next" || action == "ok") && next < pages.len() as i64 {
                n.page = next;
                return (n, None, effects.to_vec());
            }
            // Conditional hidden pages do not prevent Finish; every visible
            // page is validated, including pages revisited after a dependency edit.
            for page in pages {
                if (page.skip_if.is_empty()
                    || value(&n, &format!("field:{}", page.skip_if)) != "true")
                    && let Some(why) = validate_fields(&n, &page.fields)
                {
                    n.error = why;
                    return (n, None, effects.to_vec());
                }
            }
            (
                n.clone(),
                Some(Outcome::Values(draft(&n))),
                effects.to_vec(),
            )
        }
        _ => (n, None, effects.to_vec()),
    }
}
/// The last committed baseline is restored before a cancellation notification.
pub fn rollback(inst: &Instance, outcome: Outcome) -> Option<Outcome> {
    if outcome != Outcome::Cancelled && outcome != Outcome::Disposed {
        return None;
    };
    match inst.request {
        DialogRequest::Color { initial, .. } => {
            if color_model(inst).value != initial {
                Some(Outcome::Color(initial))
            } else {
                None
            }
        }
        DialogRequest::Font { ref initial, .. } => {
            if font_value(inst).unwrap_or(initial.clone()) != *initial {
                Some(Outcome::Font(initial.to_string()))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn palette(inst: &Instance) -> ColorPalette {
    ColorPalette::decode(&value(inst, "palette")).unwrap_or_default()
}
fn persist_palette(inst: Instance, book: &ColorPalette) -> Instance {
    let mut n = inst;
    n.values
        .insert("palette".to_string(), crate::services::to_json(&book));
    n.values.insert(
        "palette:error".to_string(),
        match book.save(&crate::services::settings_path(&n.app_id, "color-swatches")) {
            Ok(()) => "".to_string(),
            Err(e) => crate::localization::message(
                "color",
                "Cannot save swatches: {error}",
                1,
                &crate::hmap! { "error" => format!("{}", e) },
            ),
        },
    );
    n
}
fn palette_view(inst: &Instance) -> Widget {
    let book = palette(inst);
    let selected = value(inst, "swatch:selected");
    let tr = |text: String| crate::localization::tr(&text);
    let mut favorites = vec![];
    for s in book.favorites {
        let Some(color) = Color::parse_css(&s.css) else {
            continue;
        };
        let mut b = crate::widgets::buttons::Button::new(
            &cid(inst, &format!("swatch:{}", s.id)),
            &s.name,
            crate::widgets::buttons::ButtonKind::Tool,
        );
        b.swatch = Some(color);
        b.toggled = if s.id == selected { 1 } else { 0 };
        favorites.push(Widget::Button(b).tooltip(&format!("{} {}", s.name, s.css)))
    }
    let mut recent = vec![];
    for i in 0..book.recent.len() {
        let Some(color) = Color::parse_css(&book.recent[i]) else {
            continue;
        };
        recent.push(Widget::swatch_button(
            &cid(inst, &format!("recent:{}", i)),
            color,
            &book.recent[i],
        ))
    }
    let none = selected.is_empty();
    let mut name = crate::widgets::textinput::TextInput::new(
        &cid(inst, "swatch-name"),
        &value(inst, "swatch-name"),
        "",
        crate::widgets::textinput::InputKind::Line,
    );
    name.label = tr("Swatch name".to_string());
    let actions = vec![
        Widget::input(&name).width(240),
        Widget::button(
            &cid(inst, "swatch-add"),
            &tr("Add to favorites".to_string()),
        ),
        Widget::button(&cid(inst, "swatch-rename"), &tr("Rename".to_string())).disabled(none),
        Widget::button(&cid(inst, "swatch-remove"), &tr("Remove".to_string())).disabled(none),
        Widget::button(&cid(inst, "copy-css"), &tr("Copy CSS".to_string())),
    ];
    let section = |title: String| {
        Widget::text(&title, TextRole::Label, Tone::Normal).padding_sides(0, 8, 0, 0)
    };
    let mut rows = vec![section(tr("Favorite colors".to_string()))];
    rows.push(if favorites.is_empty() {
        Widget::muted(&tr(
            "Name the current color and add it to keep it here.".to_string()
        ))
    } else {
        Widget::flow(&favorites, 4)
    });
    rows.push(Widget::column(
        &[
            Widget::label(&tr("Swatch name".to_string())),
            Widget::flow(&actions, 8),
        ],
        4,
    ));
    rows.push(section(tr("Recent colors".to_string())));
    rows.push(if recent.is_empty() {
        Widget::muted(&tr("Colors you choose appear here.".to_string()))
    } else {
        Widget::flow(&recent, 4)
    });
    let error = value(inst, "palette:error");
    if !error.is_empty() {
        rows.push(Widget::text(&error, TextRole::Small, Tone::Danger))
    };
    Widget::column(&rows, 8)
}

pub fn dirty(inst: &Instance) -> bool {
    for field in fields(inst) {
        if value(inst, &format!("field:{}", field.key))
            != value(inst, &format!("baseline:{}", field.key))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{action, message, rollback};
    use crate::dialogs::Outcome;
    use crate::dialogs::host::{Instance, update};
    use crate::dialogs::request::{
        self, DateMode, PreferenceField, PreferenceKind, PreferencePage, WizardPage,
    };

    use crate::style::color::Color;
    use crate::widgets::cx::UiMsg;
    use crate::widgets::state::StateStore;
    use std::collections::HashMap;
    fn field() -> PreferenceField {
        PreferenceField {
            key: "count".to_string(),
            label: "Count".to_string(),
            kind: PreferenceKind::Number(1.0, 10.0),
            value: "2".to_string(),
            default: "3".to_string(),
            help: "Allowed count".to_string(),
        }
    }
    #[test]
    fn picker_font_preview_is_deduplicated_and_missing_fonts_are_rejected() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let inst = Instance::new(
            &request::font("f", "Font", "DejaVu Sans|Regular|14"),
            "test",
        );
        let mut st = StateStore::new();
        assert!(
            message(
                inst.clone(),
                &UiMsg::Text(
                    "dlg:f:font:sample".to_string(),
                    "Another sample".to_string()
                ),
                "font:sample",
                &mut st
            )
            .2
            .is_empty()
        );
        assert!(
            message(
                inst.clone(),
                &UiMsg::Value("dlg:f:font:size".to_string(), 14.0),
                "font:size",
                &mut st
            )
            .2
            .is_empty()
        );
        let (changed, _, fx) = message(
            inst.clone(),
            &UiMsg::Value("dlg:f:font:size".to_string(), 20.0),
            "font:size",
            &mut st,
        );
        assert_eq!(fx.len(), 1);
        assert_eq!(
            rollback(&changed, Outcome::Cancelled),
            Some(Outcome::Font("DejaVu Sans|Regular|14".to_string()))
        );
        let (missing, _, _) = message(
            inst,
            &UiMsg::Text(
                "dlg:f:font:family".to_string(),
                "gui-gos-absent-font-for-test".to_string(),
            ),
            "font:family",
            &mut st,
        );
        assert_eq!(action(missing, "ok", &[]).1, None)
    }
    #[test]
    fn a_checked_apply_waits_for_the_service_and_commits_when_it_agrees() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let req = request::checked(request::preferences(
            "prefs",
            "Settings",
            &[PreferencePage {
                key: "main".to_string(),
                title: "Main".to_string(),
                fields: vec![field()],
            }],
        ));
        let inst = Instance::new(&req, "test");
        let mut st = StateStore::new();
        let (edited, _, _) = message(
            inst,
            &UiMsg::Text("dlg:prefs:field:count".to_string(), "5".to_string()),
            "field:count",
            &mut st,
        );
        let (asked, none, fx) = action(edited, "apply", &[]);
        assert_eq!(none, None);
        let mut values: HashMap<String, String> = HashMap::new();
        values.insert("count".to_string(), "5".to_string());
        assert_eq!(
            fx,
            vec![crate::app::effects::Effect::Emit(UiMsg::DialogCheck(
                "prefs".to_string(),
                1,
                "apply".to_string(),
                Outcome::Values(values.clone())
            ))]
        );
        // A refusal is shown and keeps the draft for another try.
        let (refused, _, _) = super::check_answered(asked, 1, "The service is offline");
        assert_eq!(refused.error, "The service is offline");
        assert_eq!(super::value(&refused, "field:count"), "5");
        let (again, _, _) = action(refused, "apply", &[]);
        let (applied, none, fx) = super::check_answered(again, 2, "");
        assert_eq!(none, None);
        assert_eq!(
            fx,
            vec![crate::app::effects::Effect::Emit(UiMsg::DialogApplied(
                "prefs".to_string(),
                Outcome::Values(values)
            ))]
        );
        assert_eq!(super::value(&applied, "baseline:count"), "5")
    }

    #[test]
    fn a_variable_family_offers_its_axes_and_answers_their_settings() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        crate::text::fonts::register_test_family(
            "Var Test",
            &crate::text::font::variable_test_face(false),
        );
        let axes = crate::text::fonts::family_axes("Var Test");
        assert_eq!(
            axes.iter().map(|a| a.tag.clone()).collect::<Vec<_>>(),
            vec!["wght"]
        );
        assert_eq!(axes[0].default, 400.0);
        let heavy =
            crate::text::fonts::instance_family("Var Test", &crate::hmap! { "wght" => 650.0 });
        assert_eq!(heavy, "Var Test (wght 650)");
        assert_eq!(
            crate::text::fonts::instance_family("Var Test", &crate::hmap! { "wght" => 650.0 }),
            heavy
        );
        assert!(!crate::text::fonts::known_families().contains(&heavy));
        let inst = Instance::new(
            &request::font("v", "Font", "Var Test|Regular|14|wght=300"),
            "test",
        );
        assert_eq!(
            super::font_value(&inst),
            Ok("Var Test|Regular|14|wght=300".to_string())
        );
        let mut st = StateStore::new();
        let (_, _, fx) = message(
            inst.clone(),
            &UiMsg::Value("dlg:v:font:axis:wght".to_string(), 650.0),
            "font:axis:wght",
            &mut st,
        );
        assert_eq!(
            fx,
            vec![crate::app::effects::Effect::Emit(UiMsg::DialogPreview(
                "v".to_string(),
                Outcome::Font("Var Test|Regular|14|wght=650".to_string())
            ))]
        );
        // Back at the default, the axis drops out of the answer.
        let (plain, _, _) = message(
            inst,
            &UiMsg::Value("dlg:v:font:axis:wght".to_string(), 400.0),
            "font:axis:wght",
            &mut st,
        );
        assert_eq!(
            super::font_value(&plain),
            Ok("Var Test|Regular|14".to_string())
        )
    }

    #[test]
    fn the_installed_font_catalog_arrives_as_a_job_answer() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let _ = crate::app::effects::take();
        let inst = Instance::new(
            &request::font("c", "Font", "DejaVu Sans|Regular|14"),
            "test",
        );
        if crate::text::fonts::installed_known() {
            return;
        };
        let mut asked = vec![];
        for e in crate::app::effects::take() {
            if let crate::app::effects::Effect::LoadFonts(owner) = e {
                asked.push(owner)
            }
        }
        assert_eq!(asked, vec!["dlg:c:catalog"]);
        assert_eq!(super::value(&inst, "font:catalog"), "loading");
        let answer = crate::services::jobs::answer(&crate::services::jobs::Job::Fonts(
            "dlg:c:catalog".to_string(),
        ));
        let mut st = StateStore::new();
        let (loaded, _, _) = message(inst, &answer, "catalog", &mut st);
        assert_eq!(super::value(&loaded, "font:catalog"), "");
        assert!(crate::text::fonts::installed_known())
    }

    #[test]
    fn picker_color_drafts_preview_cancel_and_typed_accept() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let original = Instance::new(&request::color("c", "Color", Color::red()), "test");
        let mut st = StateStore::new();
        let (invalid, outcome, effects) = message(
            original,
            &UiMsg::Text("dlg:c:color:css".to_string(), "#12".to_string()),
            "color:css",
            &mut st,
        );
        assert_eq!(outcome, None);
        assert!(effects.is_empty());
        assert_eq!(action(invalid.clone(), "ok", &[]).1, None);
        let (valid, _, effects) = message(
            invalid,
            &UiMsg::Text("dlg:c:color:css".to_string(), "#0000ff80".to_string()),
            "color:css",
            &mut st,
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(
            rollback(&valid, Outcome::Cancelled),
            Some(Outcome::Color(Color::red()))
        );
        assert_eq!(
            action(valid.clone(), "ok", &[]).1,
            Some(Outcome::Color(Color::rgba(0, 0, 255, 128)))
        );
        assert_eq!(
            update(
                valid,
                &UiMsg::Click("dlg:c:action:unknown".to_string()),
                &mut st
            )
            .1,
            None
        )
    }
    #[test]
    fn picker_preferences_apply_is_nonterminal_and_validates_hidden_pages() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let pages = vec![PreferencePage {
            key: "general".to_string(),
            title: "General".to_string(),
            fields: vec![field()],
        }];
        let inst = Instance::new(&request::preferences("p", "Preferences", &pages), "test");
        let mut st = StateStore::new();
        let (bad, _, _) = message(
            inst,
            &UiMsg::Text("dlg:p:field:count".to_string(), "99".to_string()),
            "field:count",
            &mut st,
        );
        let (failed, no, fx) = action(bad, "apply", &[]);
        assert_eq!(no, None);
        assert!(fx.is_empty());
        assert!(!failed.error.is_empty());
        let (good, _, _) = message(
            failed,
            &UiMsg::Text("dlg:p:field:count".to_string(), "7".to_string()),
            "field:count",
            &mut st,
        );
        let (applied, no, fx) = action(good, "apply", &[]);
        assert_eq!(no, None);
        assert_eq!(fx.len(), 1);
        assert_eq!(action(applied.clone(), "apply", &[]).2.len(), 0);
        let (restored, _, _) = action(applied, "defaults", &[]);
        assert_eq!(super::value(&restored, "field:count"), "3");
        assert_eq!(super::value(&restored, "baseline:count"), "7")
    }
    #[test]
    fn picker_wizard_retains_drafts_skips_pages_and_checks_finish() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let toggle = PreferenceField {
            key: "skip".to_string(),
            label: "Skip optional".to_string(),
            kind: PreferenceKind::Toggle,
            value: "true".to_string(),
            default: "false".to_string(),
            help: "".to_string(),
        };
        let pages = vec![
            WizardPage {
                key: "one".to_string(),
                title: "One".to_string(),
                description: "".to_string(),
                fields: vec![toggle],
                skip_if: "".to_string(),
            },
            WizardPage {
                key: "optional".to_string(),
                title: "Optional".to_string(),
                description: "".to_string(),
                fields: vec![],
                skip_if: "skip".to_string(),
            },
            WizardPage {
                key: "last".to_string(),
                title: "Last".to_string(),
                description: "".to_string(),
                fields: vec![field()],
                skip_if: "".to_string(),
            },
        ];
        let inst = Instance::new(&request::wizard("w", "Wizard", &pages), "test");
        let (next, none, _) = action(inst, "next", &[]);
        assert_eq!(none, None);
        assert_eq!(next.page, 2);
        let (back, _, _) = action(next.clone(), "back", &[]);
        assert_eq!(back.page, 0);
        let mut st = StateStore::new();
        let (bad, _, _) = message(
            next.clone(),
            &UiMsg::Text("dlg:w:field:count".to_string(), "0".to_string()),
            "field:count",
            &mut st,
        );
        assert_eq!(action(bad, "finish", &[]).1, None);
        assert!(matches!(
            action(next, "finish", &[]).1,
            Some(Outcome::Values(_))
        ))
    }
    #[test]
    fn picker_date_rejects_invalid_and_out_of_bounds() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let inst = Instance::new(
            &request::date(
                "d",
                "Date",
                "2026-02-28",
                DateMode::Date,
                "2026-02-01",
                "2026-03-02",
            ),
            "test",
        );
        let mut st = StateStore::new();
        let (bad, _, _) = message(
            inst,
            &UiMsg::Text("dlg:d:date".to_string(), "2026-02-29".to_string()),
            "date",
            &mut st,
        );
        assert_eq!(action(bad.clone(), "ok", &[]).1, None);
        let (good, _, _) = message(
            bad,
            &UiMsg::Date("dlg:d:calendar".to_string(), "2026-03-01".to_string()),
            "calendar",
            &mut st,
        );
        assert_eq!(
            action(good, "ok", &[]).1,
            Some(Outcome::Date("2026-03-01".to_string()))
        )
    }

    fn plain(key: &str, kind: PreferenceKind, value: &str) -> PreferenceField {
        PreferenceField {
            key: key.to_string(),
            label: key.to_string(),
            kind,
            value: value.to_string(),
            default: value.to_string(),
            help: "".to_string(),
        }
    }

    #[test]
    fn a_form_answers_its_fields_under_its_own_accept_button() {
        let fields = [
            plain("query", PreferenceKind::Text, "needle"),
            plain("regex", PreferenceKind::Toggle, "false"),
        ];
        let inst = Instance::new(&request::form("find", "Find", &fields, "Find"), "test");
        let actions = super::actions(&inst);
        assert!(actions.iter().any(|a| a.id == "ok" && a.label == "Find"));
        assert_eq!(super::initial_focus(&inst), "dlg:find:field:query");
        let mut st = StateStore::new();
        let (checked, _, _) = message(
            inst,
            &UiMsg::Toggle("dlg:find:field:regex".to_string(), true),
            "field:regex",
            &mut st,
        );
        let mut want = HashMap::new();
        want.insert("query".to_string(), "needle".to_string());
        want.insert("regex".to_string(), "true".to_string());
        assert_eq!(action(checked, "ok", &[]).1, Some(Outcome::Values(want)));
    }

    #[test]
    fn a_whole_number_field_refuses_fractions_and_values_out_of_range() {
        let fields = [plain("rows", PreferenceKind::Integer(0, 100), "10")];
        let inst = Instance::new(&request::form("f", "Rows", &fields, "OK"), "test");
        let mut st = StateStore::new();
        for (text, accepted) in [("50", true), ("2.5", false), ("101", false)] {
            let (n, _, _) = message(
                inst.clone(),
                &UiMsg::Text("dlg:f:field:rows".to_string(), text.to_string()),
                "field:rows",
                &mut st,
            );
            assert_eq!(action(n, "ok", &[]).1.is_some(), accepted, "{text}");
        }
    }

    // The dialog's laid-out height in a window `height` tall, and the number
    // of scrolling viewports in it.
    fn dialog_height(inst: &Instance, height: i64) -> (i64, usize) {
        use crate::geometry::{Constraints, Size};
        let theme = crate::style::Theme::light();
        let mut cx =
            crate::widgets::cx::LayoutCx::new(&theme, &StateStore::new(), Size::new(760, height));
        let view = crate::dialogs::host::view(inst, &theme);
        let node = view.layout(Constraints::loose(Size::new(760, height)), &mut cx);
        (
            node.size().height,
            format!("{view:?}").matches("Scroll(").count(),
        )
    }

    #[test]
    fn a_form_takes_its_content_height_and_scrolls_once_when_the_window_is_short() {
        let fields = [
            plain("query", PreferenceKind::Text, "needle"),
            plain("regex", PreferenceKind::Toggle, "false"),
        ];
        let inst = Instance::new(&request::form("find", "Find", &fields, "Find"), "test");
        let (tall, viewports) = dialog_height(&inst, 2000);
        assert!(tall < 600, "{tall}");
        assert_eq!(viewports, 1);
        let page = PreferencePage {
            key: "general".to_string(),
            title: "General".to_string(),
            fields: (0..10)
                .map(|i| plain(&format!("f{i}"), PreferenceKind::Text, "x"))
                .collect(),
        };
        let settings = Instance::new(&request::preferences("p", "Settings", &[page]), "test");
        let (short, viewports) = dialog_height(&settings, 400);
        assert!(short <= 400, "{short}");
        assert_eq!(viewports, 1);
    }

    #[test]
    fn a_short_single_page_settings_dialog_shows_no_search_or_page_list() {
        let page = PreferencePage {
            key: "general".to_string(),
            title: "General".to_string(),
            fields: vec![field()],
        };
        let inst = Instance::new(&request::preferences("p", "Settings", &[page]), "test");
        let shown = format!("{:?}", super::body(&inst, &crate::style::Theme::light()));
        assert!(!shown.contains("dlg:p:search"), "{shown}");
        assert!(!shown.contains("dlg:p:page:0"), "{shown}");
        assert!(shown.contains("dlg:p:field:count"), "{shown}");
    }
}
