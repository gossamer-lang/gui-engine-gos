// Catalog messages are keyed by context and source text. Placeholders use
// `{name}` and are replaced in one pass, so translated/user text is not code.
// The pseudo-locale `qps-ploc` brackets every translated string, so a
// journey shows any built-in text that does not pass through a catalog.
use crate::errors;
use crate::strext::StrExt;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub context: String,
    pub source: String,
    pub forms: HashMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Catalog {
    pub locale: String,
    pub messages: Vec<Message>,
}
impl Catalog {
    pub fn load(file: &str) -> Result<Catalog, errors::Error> {
        Ok(serde_json::from_str::<Catalog>(
            &crate::services::bounded_io::read_text(file, 8 * 1024 * 1024).map_err(errors::new)?,
        )?)
    }
    pub fn translate(
        &self,
        context: &str,
        source: &str,
        count: i64,
        arguments: &HashMap<String, String>,
    ) -> String {
        let form = plural(&self.locale, count);
        let mut selected = source;
        for entry in &self.messages {
            if entry.context == context && entry.source == source {
                selected = entry
                    .forms
                    .get(&form)
                    .or_else(|| entry.forms.get("other"))
                    .map_or(source, String::as_str);
                break;
            }
        }
        let mut values = arguments.clone();
        values.insert("count".to_string(), format!("{}", count));
        let text = substitute(selected, &values);
        if self.locale == PSEUDO_LOCALE {
            format!("[{}]", text)
        } else {
            text
        }
    }
    pub fn rtl(&self) -> bool {
        let language = self
            .locale
            .replace("_", "-")
            .split("-")
            .next()
            .unwrap_or_default()
            .to_string();
        language == "ar" || language == "he" || language == "fa" || language == "ur"
    }
}
pub fn substitute(text: &str, values: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut key = String::new();
    let mut opened = false;
    for ch in text.chars() {
        if opened {
            if ch == '}' {
                out += values.get(&key).unwrap_or(&("{".to_owned() + &key + "}"));
                key = "".to_string();
                opened = false
            } else {
                key.push(ch)
            }
        } else if ch == '{' {
            opened = true
        } else {
            out.push(ch)
        }
    }
    if opened {
        out += &("{".to_owned() + &key)
    };
    out.to_string()
}
/// CLDR integer cardinal categories for the shipped locale families. Other
/// locales use one/other. Noninteger/ordinal rules require a separate catalog.
pub fn plural(locale: &str, count: i64) -> String {
    let lang = locale
        .replace("_", "-")
        .split("-")
        .next()
        .unwrap_or_default()
        .to_string();
    let n = if count < 0 {
        -(count % 1_000_000)
    } else {
        count % 1_000_000
    };
    if lang == "ja" || lang == "zh" || lang == "ko" || lang == "th" {
        return "other".to_string();
    };
    if lang == "ar" {
        if count == 0 {
            return "zero".to_string();
        };
        if count == 1 || count == -1 {
            return "one".to_string();
        };
        if count == 2 || count == -2 {
            return "two".to_string();
        };
        if n % 100 >= 3 && n % 100 <= 10 {
            return "few".to_string();
        };
        if n % 100 >= 11 && n % 100 <= 99 {
            return "many".to_string();
        };
        return "other".to_string();
    }
    if lang == "ru" || lang == "uk" {
        if n % 10 == 1 && n % 100 != 11 {
            return "one".to_string();
        };
        if n % 10 >= 2 && n % 10 <= 4 && (n % 100 < 12 || n % 100 > 14) {
            return "few".to_string();
        };
        return "many".to_string();
    }
    if lang == "fr" {
        if count == 0 || count == 1 || count == -1 {
            return "one".to_string();
        };
        if n == 0 {
            return "many".to_string();
        }
    };
    if count == 1 || count == -1 {
        "one".to_string()
    } else {
        "other".to_string()
    }
}
/// How a locale writes numbers, dates and times.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Conventions {
    pub decimal: String,
    /// Between groups of three integer digits.
    pub group: String,
    /// "ymd", "mdy" or "dmy".
    pub date_order: String,
    pub date_separator: String,
    pub hour24: bool,
}

/// The conventions of `locale` (`de`, `en-GB`, `pt_BR`, ...): its language,
/// refined by its region where they differ. Other locales write English
/// numbers and ISO dates.
pub fn conventions(locale: &str) -> Conventions {
    let tag = locale.replace("_", "-");
    let parts = tag.split("-").collect::<Vec<_>>();
    let language = parts[0].to_lowercase();
    let region = if parts.len() > 1 {
        parts[1].to_uppercase()
    } else {
        String::new()
    };
    let c =
        |decimal: String, group: String, order: String, sep: String, hour24: bool| Conventions {
            decimal,
            group,
            date_order: order,
            date_separator: sep,
            hour24,
        };
    match language.as_str() {
        "en" => match region.as_str() {
            "US" | "" => c(
                ".".to_string(),
                ",".to_string(),
                "mdy".to_string(),
                "/".to_string(),
                false,
            ),
            "CA" => c(
                ".".to_string(),
                ",".to_string(),
                "ymd".to_string(),
                "-".to_string(),
                false,
            ),
            _ => c(
                ".".to_string(),
                ",".to_string(),
                "dmy".to_string(),
                "/".to_string(),
                true,
            ),
        },
        "de" | "da" | "id" | "tr" => c(
            ",".to_string(),
            ".".to_string(),
            "dmy".to_string(),
            ".".to_string(),
            true,
        ),
        "es" | "it" | "pt" | "el" => c(
            ",".to_string(),
            ".".to_string(),
            "dmy".to_string(),
            "/".to_string(),
            true,
        ),
        "nl" => c(
            ",".to_string(),
            ".".to_string(),
            "dmy".to_string(),
            "-".to_string(),
            true,
        ),
        "fr" => c(
            ",".to_string(),
            "\u{202F}".to_string(),
            "dmy".to_string(),
            "/".to_string(),
            true,
        ),
        "ru" | "uk" | "pl" | "cs" | "fi" | "nb" | "sv" => c(
            ",".to_string(),
            "\u{00A0}".to_string(),
            "dmy".to_string(),
            ".".to_string(),
            true,
        ),
        "ja" | "zh" | "ko" => c(
            ".".to_string(),
            ",".to_string(),
            "ymd".to_string(),
            "/".to_string(),
            true,
        ),
        "he" => c(
            ".".to_string(),
            ",".to_string(),
            "dmy".to_string(),
            ".".to_string(),
            true,
        ),
        "ar" | "fa" => c(
            "\u{066B}".to_string(),
            "\u{066C}".to_string(),
            "dmy".to_string(),
            "/".to_string(),
            true,
        ),
        _ => c(
            ".".to_string(),
            ",".to_string(),
            "ymd".to_string(),
            "-".to_string(),
            true,
        ),
    }
}

/// `value` with `decimals` digits after the decimal separator and grouped
/// thousands, as the installed catalog's locale writes it.
pub fn format_number(value: f64, decimals: i64) -> String {
    let conv = conventions(&locale());
    let plain = crate::widgets::numeric::format_number(value, decimals);
    let negative = plain.starts_with("-");
    let unsigned = if negative {
        plain.substring(1, plain.byte_len())
    } else {
        &plain
    };
    let (whole, fraction) = match unsigned.split_once(".") {
        Some((w, f)) => (w, f),
        None => (unsigned, ""),
    };
    let mut grouped = String::new();
    let digits: Vec<char> = whole.chars().collect();
    let mut i = 0;
    while i < digits.len() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped += &conv.group
        };
        grouped.push(digits[i]);
        i += 1;
    }
    let sign = if negative { "-" } else { "" };
    if fraction.is_empty() {
        format!("{}{}", sign, grouped)
    } else {
        format!("{}{}{}{}", sign, grouped, conv.decimal, fraction)
    }
}

// Whether `text` splits at `separator` into a leading part and groups of
// exactly three digits, as grouped thousands do.
fn grouped(text: &str, separator: &str) -> bool {
    let parts = text.split(&separator).collect::<Vec<_>>();
    if parts.len() < 2 || parts[0].is_empty() {
        return false;
    };
    let mut i = 1;
    while i < parts.len() {
        if parts[i].len() != 3 || !parts[i].chars().all(|c| c.is_ascii_digit()) {
            return false;
        };
        i += 1;
    }
    true
}

/// A number typed in the installed catalog's locale. Its decimal separator
/// makes the group separators and spaces between groups ignorable; without
/// it, a group separator counts only between groups of three digits, and any
/// other `.` or `,` is the decimal point. `None` for anything else.
pub fn parse_number(text: &str) -> Option<f64> {
    let conv = conventions(&locale());
    let mut t = text
        .trim()
        .replace(" ", "")
        .replace("\u{00A0}", "")
        .replace("\u{202F}", "");
    if t.contains(&conv.decimal) {
        t = t.replace(&conv.group, "").replace(&conv.decimal, ".");
    } else if t.contains(&conv.group) && grouped(&t, &conv.group) {
        t = t.replace(&conv.group, "");
    } else {
        t = t.replace(",", ".");
    }
    t.to_f64()
}

/// A date as the installed catalog's locale writes it, with two-digit days
/// and months and four-digit years.
pub fn format_date(year: i64, month: i64, day: i64) -> String {
    let conv = conventions(&locale());
    let two = |n: i64| {
        if n < 10 {
            format!("0{}", n)
        } else {
            format!("{}", n)
        }
    };
    let y = format!("{}", year);
    let parts = match conv.date_order.as_str() {
        "mdy" => vec![two(month), two(day), y],
        "dmy" => vec![two(day), two(month), y],
        _ => vec![y, two(month), two(day)],
    };
    parts.join(&conv.date_separator)
}

/// A time of day as the installed catalog's locale writes it: 24-hour, or
/// 12-hour with a translated AM/PM.
pub fn format_time(hour: i64, minute: i64) -> String {
    let m = if minute < 10 {
        format!("0{}", minute)
    } else {
        format!("{}", minute)
    };
    if conventions(&locale()).hour24 {
        return format!(
            "{}:{}",
            if hour < 10 {
                format!("0{}", hour)
            } else {
                format!("{}", hour)
            },
            m
        );
    }
    let h = if hour % 12 == 0 { 12 } else { hour % 12 };
    message(
        "time",
        if hour < 12 { "{time} AM" } else { "{time} PM" },
        1,
        &crate::hmap! { "time" => format!("{}:{}", h, m) },
    )
}

/// The moment `unix_ms` (milliseconds since the Unix epoch) as a date and
/// time in the system's time zone, written as the installed locale does.
pub fn format_timestamp(unix_ms: i64) -> String {
    let Ok(at) = jiff::Timestamp::from_millisecond(unix_ms) else {
        return String::new();
    };
    let c = at.to_zoned(jiff::tz::TimeZone::system()).datetime();
    let date = format_date(
        i64::from(c.year()),
        i64::from(c.month()),
        i64::from(c.day()),
    );
    format!(
        "{} {}",
        date,
        format_time(i64::from(c.hour()), i64::from(c.minute()))
    )
}

/// The name of month `month` (1 to 12).
pub fn month_name(month: i64) -> String {
    let names = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    tr(names[(month - 1).clamp(0, 11) as usize])
}

/// The pseudo-locale that marks every translated string with brackets.
pub const PSEUDO_LOCALE: &str = "qps-ploc";

static CURRENT: LazyLock<Mutex<Catalog>> = LazyLock::new(|| {
    Mutex::new(Catalog {
        locale: "en".to_string(),
        messages: vec![],
    })
});
pub fn try_install(catalog: &Catalog) -> Result<(), String> {
    if crate::memory::measure(catalog) > 8 * 1024 * 1024 {
        return Err("translation catalog exceeds its byte budget".into());
    }
    *CURRENT.lock() = catalog.clone();
    Ok(())
}
pub fn install(catalog: &Catalog) {
    if let Err(error) = try_install(catalog) {
        panic!("{error}");
    }
}
pub fn locale() -> String {
    CURRENT.lock().locale.clone()
}
pub fn rtl() -> bool {
    CURRENT.lock().rtl()
}
pub fn tr(source: &str) -> String {
    CURRENT.lock().translate("", source, 1, &HashMap::new())
}
pub fn message(context: &str, source: &str, count: i64, args: &HashMap<String, String>) -> String {
    CURRENT.lock().translate(context, source, count, args)
}
#[cfg(test)]
mod pseudo_tests {
    use super::{Catalog, PSEUDO_LOCALE, install, tr};
    use crate::accessibility::AccessNode;
    use crate::app::effects::Effect;
    use crate::dialogs::request;
    use crate::paths;

    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;
    use std::collections::HashMap;
    use std::{env, fs};

    fn view(_m: &i64) -> Widget {
        Widget::column(
            &[
                Widget::label(&tr("Pseudo journey")),
                Widget::button("go", &tr("Go")),
            ],
            8,
        )
        .padding(8)
    }

    // Accessible names and descriptions that did not pass through a catalog;
    // `data` (folder names on screen) is the user's, not the toolkit's.
    fn untranslated(n: &AccessNode, data: &[String], out: &mut Vec<String>) {
        for text in [n.name.clone(), n.description.clone()] {
            let words = text.chars().any(|c| c.is_alphabetic());
            if words
                && !text.starts_with("[")
                && !data.contains(&text)
                && !out.contains(&format!("{} {:?}", n.role.name(), text))
            {
                out.push(format!("{} {:?}", n.role.name(), text))
            }
        }
        for c in n.children.clone() {
            untranslated(&c, data, out)
        }
    }

    fn gallery(_m: &i64) -> Widget {
        let items = vec![
            crate::widgets::completion::CompletionItem::new("a", &tr("Apple")),
            crate::widgets::completion::CompletionItem::new("b", &tr("Banana")),
        ];
        let list = crate::widgets::collections::Collection::new("list", 0, |i| {
            crate::widgets::collections::Row::new(&format!("{}", i), &tr("Row"))
        })
        .label(&tr("List"))
        .empty_text(&tr("Nothing here"))
        .rows(3);
        let tabs = crate::widgets::tabs::Tabs {
            id: "tabs".to_string(),
            items: vec![
                crate::widgets::tabs::TabItem::new("one", &tr("One")),
                crate::widgets::tabs::TabItem::new("two", &tr("Two")),
            ],
            active: "one".to_string(),
            content: Box::new(Widget::label(&tr("Page"))),
            pages: HashMap::new(),
            workspace: "".to_string(),
            add_button: true,
            detachable: false,
        };
        Widget::column(
            &[
                Widget::date_field("date", "2026-10-20"),
                Widget::Calendar(crate::widgets::temporal::Calendar::new(
                    "cal",
                    crate::widgets::temporal::Date {
                        year: 2026,
                        month: 10,
                        day: 20,
                    },
                )),
                Widget::select("select", &items, "gone", &tr("Fruit")),
                Widget::picker(
                    "picker",
                    &items,
                    &["a".to_string(), "gone".to_string()],
                    true,
                    &tr("Fruits"),
                ),
                Widget::Collection(list),
                Widget::Tabs(tabs),
                crate::widgets::feedback::banner(
                    "banner",
                    crate::widgets::feedback::Level::Warning,
                    &tr("Heads up"),
                    &tr("Something"),
                    &[],
                    true,
                ),
                crate::widgets::feedback::progress("progress", -1.0),
                Widget::range_slider("range", 2.0, 8.0, 0.0, 10.0, 1.0).accessible(&tr("Range")),
                Widget::password_field("password", "", &tr("Password")),
                Widget::search_field("search", "", &tr("Search")),
            ],
            8,
        )
        .padding(8)
    }

    fn german() -> Catalog {
        let entry = |source: String, text: String| super::Message {
            context: "".to_string(),
            source,
            forms: crate::hmap! { "other" => text },
        };
        Catalog {
            locale: "de-DE".to_string(),
            messages: vec![
                entry("Pseudo journey".to_string(), "Pseudo-Reise".to_string()),
                entry("Cancel".to_string(), "Abbrechen".to_string()),
                entry("Close".to_string(), "Schlie\u{00DF}en".to_string()),
            ],
        }
    }

    #[test]
    fn changing_language_rebuilds_windows_dialogs_numbers_and_direction() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let _defer = scopeguard::guard((), |_| {
                install(&Catalog {
                    locale: "en".to_string(),
                    messages: vec![],
                });
            });
            let view = |_m: &i64| {
                Widget::column(
                    &[
                        Widget::label(&tr("Pseudo journey")),
                        Widget::spin_box("amount", 1234.5, 0.0, 10000.0, 0.5, 1),
                    ],
                    8,
                )
                .padding(8)
            };
            let mut s = crate::application(
                "Languages",
                "dev.gossamer.gui-language-test",
                || 0,
                |m: i64, _msg: UiMsg| m,
                view,
            )
            .size(640, 480)
            .start()
            .unwrap();
            crate::perform(Effect::OpenDialog(request::prompt(
                "ask", "Name", "Name", "",
            )));
            s.flush().unwrap();
            let before = s.semantics(0).dump();
            assert!(
                before.contains("label \"Pseudo journey\"")
                    && before.contains("button \"Cancel\"")
                    && before.contains("1,234.5"),
                "{}",
                before
            );
            crate::perform(Effect::SetLocale(german()));
            s.flush().unwrap();
            let after = s.semantics(0).dump();
            assert!(
                after.contains("label \"Pseudo-Reise\"")
                    && after.contains("button \"Abbrechen\"")
                    && after.contains("button \"Schlie\u{00DF}en\""),
                "{}",
                after
            );
            assert!(after.contains("1.234,5"), "{}", after);
            assert!(!s.windows[0].theme.rtl);
            crate::perform(Effect::SetLocale(Catalog {
                locale: "ar".to_string(),
                messages: vec![],
            }));
            s.flush().unwrap();
            assert!(s.windows[0].theme.rtl);
            assert!(s.dialog_ids() == vec!["ask"], "{:?}", s.dialog_ids());
            s.shutdown()
        })
    }

    #[test]
    fn every_built_in_widget_string_passes_through_the_catalog() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            install(&Catalog {
                locale: PSEUDO_LOCALE.to_string(),
                messages: vec![],
            });
            let _defer = scopeguard::guard((), |_| {
                install(&Catalog {
                    locale: "en".to_string(),
                    messages: vec![],
                });
            });
            let mut s = crate::application(
                &tr("Strings"),
                "dev.gossamer.gui-pseudo-test",
                || 0,
                |m: i64, _msg: UiMsg| m,
                gallery,
            )
            .size(900, 1400)
            .start()
            .unwrap();
            let mut found: Vec<String> = vec![];
            untranslated(&s.semantics(0), &["gone".to_string()], &mut found);
            s.shutdown();
            assert!(found.is_empty(), "{}", found.join("\n"))
        })
    }

    #[test]
    fn every_built_in_dialog_string_passes_through_the_catalog() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            fs::create_dir_all(config.clone()).unwrap();
            install(&Catalog {
                locale: PSEUDO_LOCALE.to_string(),
                messages: vec![],
            });
            let _defer = scopeguard::guard((), |_| {
                install(&Catalog {
                    locale: "en".to_string(),
                    messages: vec![],
                });
            });
            // File browsers open in the working folder: its path and entries are data.
            let mut data: Vec<String> = vec![];
            for folder in [
                fs::canonicalize(&config)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                env::current_dir().unwrap().to_string_lossy().into_owned(),
            ] {
                let mut at = folder.clone();
                while !at.is_empty() && at != "/" {
                    let name = paths::file_name(&at).unwrap_or_else(|| at.clone());
                    data.push(format!("{name}, current folder"));
                    data.push(name);
                    at = paths::parent(&at).unwrap_or_default();
                }
                data.push(folder.clone());
                for e in paths::read_dir(&folder).unwrap_or_default() {
                    data.push(e.name)
                }
            }
            let field = request::PreferenceField {
                key: "count".to_string(),
                label: tr("Count"),
                kind: request::PreferenceKind::Number(1.0, 10.0),
                value: "2".to_string(),
                default: "3".to_string(),
                help: tr("How many"),
            };
            let requests = vec![
                request::info("info", &tr("Title"), &tr("Text")),
                request::warning("warning", &tr("Title"), &tr("Text")),
                request::error("error", &tr("Title"), &tr("Text"), &tr("Detail"), true),
                request::confirm("confirm", &tr("Title"), &tr("Text"), &tr("Accept"), true),
                request::confirm_overwrite("overwrite", "notes.txt"),
                request::confirm_delete("delete", "notes.txt"),
                request::confirm_unsaved("unsaved", "notes.txt"),
                request::prompt("prompt", &tr("Title"), &tr("Label"), ""),
                request::go_to_line("line", 40, ""),
                request::color(
                    "color",
                    &tr("Title"),
                    crate::style::color::Color::rgb(10, 20, 30),
                ),
                request::font("font", &tr("Title"), "DejaVu Sans|Regular|14"),
                request::date(
                    "date",
                    &tr("Title"),
                    "2026-10-20",
                    request::DateMode::DateTime,
                    "",
                    "",
                ),
                request::preferences(
                    "prefs",
                    &tr("Title"),
                    &[request::PreferencePage {
                        key: "main".to_string(),
                        title: tr("Main"),
                        fields: vec![field.clone()],
                    }],
                ),
                request::wizard(
                    "wizard",
                    &tr("Title"),
                    &[request::WizardPage {
                        key: "one".to_string(),
                        title: tr("One"),
                        description: tr("First"),
                        fields: vec![field],
                        skip_if: "".to_string(),
                    }],
                ),
                request::about("about", &tr("Name"), "1.0", &tr("Description")),
                request::toolkit_file(request::open_file("open", &tr("Title"), &[])),
                request::toolkit_file(request::save_file("save", &tr("Title"), "notes.txt", &[])),
            ];
            let mut s = crate::application(
                &tr("Strings"),
                "dev.gossamer.gui-pseudo-test",
                || 0,
                |m: i64, _msg: UiMsg| m,
                view,
            )
            .size(900, 760)
            .start()
            .unwrap();
            let mut found: Vec<String> = vec![];
            for req in requests {
                let id = req.id();
                crate::perform(Effect::OpenDialog(req));
                s.flush().unwrap();
                let mut here = vec![];
                untranslated(&s.semantics(0), &data, &mut here);
                for h in here {
                    found.push(format!("{}: {}", id, h))
                }
                s.close_dialog(&id, &crate::dialogs::Outcome::Cancelled);
                s.flush().unwrap()
            }
            s.shutdown();
            assert!(found.is_empty(), "{}", found.join("\n"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Catalog, Message, plural};

    #[test]
    fn numbers_and_dates_follow_the_locale() {
        let at = |locale: String| {
            super::install(&Catalog {
                locale,
                messages: vec![],
            })
        };
        let _defer = scopeguard::guard((), |_| {
            super::install(&Catalog {
                locale: "en".to_string(),
                messages: vec![],
            });
        });
        at("en-US".to_string());
        assert_eq!(super::format_number(1234567.891, 2), "1,234,567.89");
        assert_eq!(super::format_number(-1234.5, 1), "-1,234.5");
        assert_eq!(super::format_date(2026, 3, 7), "03/07/2026");
        assert_eq!(super::format_time(14, 5), "2:05 PM");
        assert_eq!(super::parse_number("1,234.5"), Some(1234.5));
        assert_eq!(super::parse_number("1,234"), Some(1234.0));
        assert_eq!(super::parse_number("1,5"), Some(1.5));
        assert_eq!(super::parse_number("1x"), None);
        at("de-DE".to_string());
        assert_eq!(super::parse_number("1.234"), Some(1234.0));
        assert_eq!(super::format_number(1234.5, 1), "1.234,5");
        assert_eq!(super::format_date(2026, 3, 7), "07.03.2026");
        assert_eq!(super::format_time(14, 5), "14:05");
        assert_eq!(super::parse_number("1.234,5"), Some(1234.5));
        assert_eq!(super::parse_number("0,75"), Some(0.75));
        assert_eq!(super::parse_number("0.75"), Some(0.75));
        at("fr".to_string());
        assert_eq!(super::format_number(1234.5, 1), "1\u{202F}234,5");
        assert_eq!(super::parse_number("1 234,5"), Some(1234.5));
        at("en-GB".to_string());
        assert_eq!(super::format_date(2026, 3, 7), "07/03/2026");
        at("ja".to_string());
        assert_eq!(super::format_date(2026, 3, 7), "2026/03/07");
        at("xx".to_string());
        assert_eq!(super::format_date(2026, 3, 7), "2026-03-07");
        assert_eq!(super::month_name(10), "October");
        // An hour into 2026 in the system zone is some time on New Year's Eve or Day.
        let stamp = super::format_timestamp(1_767_229_200_000);
        assert!(
            stamp.contains("2025") || stamp.contains("2026"),
            "{}",
            stamp
        );
        assert!(stamp.contains(":"), "{}", stamp)
    }

    #[test]
    fn catalogs_context_plurals_and_safe_substitution() {
        let c = Catalog {
            locale: "ar".to_string(),
            messages: vec![Message {
                context: "files".to_string(),
                source: "{count} files in {folder}".to_string(),
                forms: crate::hmap! { "two" => "ملفان في {folder}", "other" => "{count} ملفات في {folder}" },
            }],
        };
        assert!(c.rtl());
        assert_eq!(
            c.translate(
                "files",
                "{count} files in {folder}",
                2,
                &crate::hmap! { "folder" => "{count}" }
            ),
            "ملفان في {count}"
        );
        assert_eq!(plural("ru", 21), "one");
        assert_eq!(plural("ru", 12), "many");
        assert_eq!(plural("ar", 3), "few");
        assert_eq!(plural("ja", 1), "other")
    }
}
