// Navigation uses the standard selection controller, so arrow/typeahead keys,
// disabled entries and native accessibility share the control's stable keys.
use crate::accessibility::Role;
use crate::widgets::collections::{Collection, Row};
use crate::widgets::{Tone, Widget, buttons};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Destination {
    pub key: String,
    pub label: String,
    pub icon: Option<crate::render::icons::Icon>,
    pub disabled: bool,
}
impl Destination {
    pub fn new(key: &str, label: &str) -> Destination {
        Destination {
            key: key.to_string(),
            label: label.to_string(),
            icon: None,
            disabled: false,
        }
    }
}
/// Sidebar selection reports UiMsg::Select(id, destination key).
pub fn sidebar(id: &str, destinations: &[Destination], selected: &str) -> Widget {
    let count = destinations.len();
    let destinations = destinations.to_vec();
    let list = Collection::new(id, count as i64, move |i| {
        let item = &destinations[i as usize];
        let mut row = Row::new(&item.key, &item.label);
        row.icon = item.icon.clone();
        row.disabled = item.disabled;
        row
    });
    Widget::collection(
        &list
            .selected(&[selected.to_string()])
            .label(&crate::localization::tr("Navigation"))
            .empty_text("No destinations")
            .rows(count.max(1) as i64),
    )
    .role(Role::Pane)
    .accessible("Navigation")
}
/// Each ancestor is a standard keyboard button named `{id}#{key}`. The current
/// item is readable text. Activation reports UiMsg::Click with that button id.
pub fn breadcrumb(id: &str, ancestors: &[Destination], current: &str) -> Widget {
    let mut children = vec![];
    for item in ancestors {
        if !children.is_empty() {
            children.push(Widget::muted("/").decorative())
        };
        children.push(if item.key == current {
            Widget::label(&item.label)
                .padding_xy(12, 0)
                .role(Role::Label)
                .accessible(&format!("{}, current location", item.label))
        } else {
            Widget::flat_button(&format!("{}#{}", id, item.key), &item.label)
                .disabled(item.disabled)
        })
    }
    Widget::flow(&children, 4)
        .key(id)
        .role(Role::Pane)
        .accessible(&crate::localization::tr("Breadcrumb"))
}
pub fn toolbar(id: &str, controls: &[Widget]) -> Widget {
    Widget::flow(controls, 4).key(id).role(Role::Toolbar)
}
pub fn status_bar(id: &str, text: &str) -> Widget {
    Widget::text(text, crate::style::TextRole::Small, Tone::Muted)
        .padding(6)
        .key(id)
        .role(Role::StatusBar)
}
pub fn accordion(id: &str, sections: &[(String, String, Widget)], opened: &[String]) -> Widget {
    let mut children = vec![];
    for (key, title, content) in sections {
        children.push(disclosure(
            &format!("{}#{}", id, key),
            title,
            content.clone(),
            opened.contains(key),
        ))
    }
    Widget::column(&children, 4).key(id).role(Role::Group)
}

/// The header reports UiMsg::Toggle(id, open); the caller retains expansion.
pub fn disclosure(id: &str, title: &str, content: Widget, opened: bool) -> Widget {
    let mut header = buttons::Button::new(id, title, buttons::ButtonKind::Flat);
    header.toggled = if opened { 1 } else { 0 };
    header.expander = true;
    let mut children = vec![Widget::Button(header)];
    if opened {
        children.push(content)
    };
    Widget::column(&children, 4).role(Role::Group)
}

#[cfg(test)]
mod session_tests {
    use super::{Destination, breadcrumb, disclosure};
    use crate::widgets::Widget;
    use crate::widgets::cx::UiMsg;
    use windowing::{Key, Mods};

    fn view(open: &bool) -> Widget {
        Widget::column(
            &[
                breadcrumb(
                    "crumb",
                    &[
                        Destination::new("home", "Home"),
                        Destination::new("docs", "Docs"),
                        Destination::new("page", "Page"),
                    ],
                    "page",
                ),
                disclosure("more", "Details", Widget::label("Hidden content"), *open),
            ],
            8,
        )
        .padding(8)
    }

    fn update(open: bool, msg: UiMsg) -> bool {
        match msg {
            UiMsg::Toggle(id, on) => {
                if id == "more" {
                    on
                } else {
                    open
                }
            }
            _ => open,
        }
    }

    #[test]
    fn a_disclosure_reports_expanded_and_toggles_from_the_keyboard() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s =
                crate::application("Nav", "dev.gossamer.gui-nav-test", || false, update, view)
                    .size(420, 300)
                    .start()
                    .unwrap();
            let closed = s.semantics(0).dump();
            assert!(
                closed.contains("button \"Details\" [collapsed]")
                    && !closed.contains("Hidden content"),
                "{}",
                closed
            );
            assert!(s.focus("more").unwrap());
            s.press(Key::Return, Mods::default()).unwrap();
            let open = s.semantics(0).dump();
            assert!(
                open.contains("button \"Details\" [focused expanded]")
                    && open.contains("Hidden content"),
                "{}",
                open
            );
            s.press(Key::Space, Mods::default()).unwrap();
            assert!(
                s.semantics(0)
                    .dump()
                    .contains("button \"Details\" [focused collapsed]"),
                "{}",
                s.semantics(0).dump()
            );
            s.shutdown()
        })
    }

    #[test]
    fn breadcrumb_separators_are_not_read() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s =
                crate::application("Nav", "dev.gossamer.gui-nav-test", || false, update, view)
                    .size(420, 300)
                    .start()
                    .unwrap();
            let dump = s.semantics(0).dump();
            assert!(
                dump.contains("button \"Home\"") && dump.contains("Page, current location"),
                "{}",
                dump
            );
            assert!(!dump.contains("\"/\""), "{}", dump);
            s.shutdown()
        })
    }

    fn tools_view(_m: &i64) -> Widget {
        Widget::column(
            &[
                Widget::button("before", "Before"),
                super::toolbar(
                    "tools",
                    &[
                        Widget::button("cut", "Cut"),
                        Widget::button("copy", "Copy"),
                        Widget::button("paste", "Paste"),
                    ],
                ),
                Widget::button("after", "After"),
            ],
            8,
        )
        .padding(8)
    }

    #[test]
    fn a_toolbar_is_one_tab_stop_and_the_arrows_move_inside_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::application(
                "Tools",
                "dev.gossamer.gui-toolbar-test",
                || 0,
                |m: i64, _msg: UiMsg| m,
                tools_view,
            )
            .size(420, 300)
            .start()
            .unwrap();
            assert!(s.focus("before").unwrap());
            s.press(Key::Tab, Mods::default()).unwrap();
            assert_eq!(s.focused(), "cut");
            s.press(Key::Right, Mods::default()).unwrap();
            s.press(Key::Right, Mods::default()).unwrap();
            assert_eq!(s.focused(), "paste");
            s.press(Key::Right, Mods::default()).unwrap();
            assert_eq!(s.focused(), "cut");
            s.press(Key::Left, Mods::default()).unwrap();
            assert_eq!(s.focused(), "paste");
            // Tab leaves the toolbar; coming back returns to where it was.
            s.press(Key::Tab, Mods::default()).unwrap();
            assert_eq!(s.focused(), "after");
            s.press(
                Key::Tab,
                Mods {
                    shift: true,
                    ..Mods::default()
                },
            )
            .unwrap();
            assert_eq!(s.focused(), "paste");
            s.press(
                Key::Tab,
                Mods {
                    shift: true,
                    ..Mods::default()
                },
            )
            .unwrap();
            assert_eq!(s.focused(), "before");
            s.shutdown()
        })
    }
}
