// find: the find and replace bar. The application owns a `FindBar`, folds
// its messages through `FindBar::update`, runs the search the answered
// request names over its own text (`editing::search`), selects the result
// with `Effect::SelectText`, and records the count with `FindBar::found`.

use crate::editing::search::SearchOptions;
use crate::localization;
use crate::localization::tr;
use crate::strext::StrExt;
use crate::widgets::Widget;
use crate::widgets::cx::UiMsg;
use crate::widgets::textinput::{InputKind, TextInput};
use std::collections::HashMap;

/// What the application does after a find bar message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FindRequest {
    /// Nothing beyond showing the bar again.
    Nothing,
    /// The query or an option changed: recount and select the first match from the caret.
    Search,
    /// Select the next match after the selection.
    Next,
    /// Select the match before the selection.
    Previous,
    /// Replace the selection when it is a match, then select the next one.
    Replace,
    /// Replace every match as one edit.
    ReplaceAll,
    /// The bar closed: give focus back to the document.
    Close,
}

/// One find bar's state.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FindBar {
    pub id: String,
    pub open: bool,
    /// Whether the replacement row shows.
    pub replacing: bool,
    pub query: String,
    pub replacement: String,
    pub options: SearchOptions,
    /// Matches in the searched text.
    pub count: i64,
    /// The selected match's index, or -1 when the selection is not a match.
    pub current: i64,
}

impl FindBar {
    /// A closed bar whose controls use ids beginning with `id`.
    pub fn new(id: &str) -> FindBar {
        FindBar {
            id: id.to_string(),
            open: false,
            replacing: false,
            query: "".to_string(),
            replacement: "".to_string(),
            options: SearchOptions::default(),
            count: 0,
            current: -1,
        }
    }

    /// Opens the bar, with the replacement row when `replacing`.
    pub fn show(&mut self, replacing: bool) {
        self.open = true;
        self.replacing = replacing;
    }

    /// The query field's id, to focus when the bar opens.
    pub fn query_id(&self) -> String {
        format!("{}:query", self.id)
    }

    /// Records a search result: the number of matches and the selected one (-1 for none).
    pub fn found(&mut self, count: i64, current: i64) {
        self.count = count.max(0);
        self.current = if current >= 0 && current < self.count {
            current
        } else {
            -1
        };
    }

    /// "2 of 5", "5 matches", "No matches", or "" before anything is typed.
    pub fn summary(&self) -> String {
        if self.query.is_empty() {
            return "".to_string();
        }
        if self.count == 0 {
            return tr("No matches");
        }
        if self.current < 0 {
            return localization::message(
                "find",
                if self.count == 1 {
                    "{count} match"
                } else {
                    "{count} matches"
                },
                self.count,
                &HashMap::new(),
            );
        }
        localization::message(
            "find",
            "{current} of {count}",
            self.count,
            &crate::hmap! { "current" => format!("{}", self.current + 1) },
        )
    }

    /// Folds one message into the bar and answers the work it asks for;
    /// messages for other widgets answer `Nothing`.
    pub fn update(&mut self, msg: &UiMsg) -> FindRequest {
        let prefix = format!("{}:", self.id);
        match msg {
            UiMsg::Text(id, text) => {
                if *id == format!("{}query", prefix) {
                    if *text == self.query {
                        return FindRequest::Nothing;
                    };
                    self.query = text.to_string();
                    return FindRequest::Search;
                }
                if *id == format!("{}replacement", prefix) {
                    self.replacement = text.to_string()
                };
                FindRequest::Nothing
            }
            UiMsg::Submit(id, _) => {
                if *id == format!("{}query", prefix) {
                    FindRequest::Next
                } else if *id == format!("{}replacement", prefix) {
                    FindRequest::Replace
                } else {
                    FindRequest::Nothing
                }
            }
            UiMsg::Toggle(id, on) => {
                if *id == format!("{}case", prefix) {
                    self.options.match_case = *on;
                    FindRequest::Search
                } else if *id == format!("{}word", prefix) {
                    self.options.whole_word = *on;
                    FindRequest::Search
                } else {
                    FindRequest::Nothing
                }
            }
            UiMsg::Click(id) => {
                if !id.starts_with(&prefix) {
                    return FindRequest::Nothing;
                }
                match id.substring(prefix.byte_len(), id.byte_len()) {
                    "previous" => FindRequest::Previous,
                    "next" => FindRequest::Next,
                    "replace" => FindRequest::Replace,
                    "replace-all" => FindRequest::ReplaceAll,
                    "close" => {
                        self.open = false;
                        FindRequest::Close
                    }
                    _ => FindRequest::Nothing,
                }
            }
            _ => FindRequest::Nothing,
        }
    }

    /// The bar's widgets; nothing while it is closed.
    pub fn view(&self) -> Widget {
        if !self.open {
            return Widget::Empty;
        }
        let mut query = TextInput::new(
            &self.query_id(),
            &self.query,
            &tr("Find"),
            InputKind::Search,
        );
        query.label = tr("Find");
        let found = !self.query.is_empty() && self.count > 0;
        let mut rows = vec![Widget::row(
            &[
                Widget::input(&query).grow(1),
                Widget::toggle_button(
                    &format!("{}:case", self.id),
                    &tr("Match case"),
                    self.options.match_case,
                ),
                Widget::toggle_button(
                    &format!("{}:word", self.id),
                    &tr("Whole words"),
                    self.options.whole_word,
                ),
                Widget::muted(&self.summary()).min_width(96),
                Widget::button(&format!("{}:previous", self.id), &tr("Previous")).disabled(!found),
                Widget::button(&format!("{}:next", self.id), &tr("Next")).disabled(!found),
                Widget::button(&format!("{}:close", self.id), &tr("Close")),
            ],
            6,
        )];
        if self.replacing {
            let mut replacement = TextInput::new(
                &format!("{}:replacement", self.id),
                &self.replacement,
                &tr("Replace with"),
                InputKind::Line,
            );
            replacement.label = tr("Replace with");
            rows.push(Widget::row(
                &[
                    Widget::input(&replacement).grow(1),
                    Widget::button(&format!("{}:replace", self.id), &tr("Replace"))
                        .disabled(!found),
                    Widget::button(&format!("{}:replace-all", self.id), &tr("Replace All"))
                        .disabled(!found),
                ],
                6,
            ))
        }
        Widget::column(&rows, 6)
            .padding_xy(8, 6)
            .role(crate::accessibility::Role::Group)
            .accessible(&tr("Find and replace"))
    }
}

#[cfg(test)]
mod tests {
    use super::{FindBar, FindRequest};

    use crate::widgets::cx::UiMsg;

    #[test]
    fn query_and_option_changes_ask_for_a_search() {
        let mut bar = FindBar::new("find");
        bar.show(true);
        assert_eq!(
            bar.update(&UiMsg::Text("find:query".to_string(), "cat".to_string())),
            FindRequest::Search
        );
        assert_eq!(
            bar.update(&UiMsg::Text("find:query".to_string(), "cat".to_string())),
            FindRequest::Nothing
        );
        assert_eq!(
            bar.update(&UiMsg::Toggle("find:case".to_string(), true)),
            FindRequest::Search
        );
        assert!(bar.options.match_case);
        assert_eq!(
            bar.update(&UiMsg::Toggle("find:word".to_string(), true)),
            FindRequest::Search
        );
        assert!(bar.options.whole_word);
        assert_eq!(
            bar.update(&UiMsg::Text(
                "find:replacement".to_string(),
                "dog".to_string()
            )),
            FindRequest::Nothing
        );
        assert_eq!(bar.replacement, "dog")
    }

    #[test]
    fn buttons_and_enter_step_replace_and_close() {
        let mut bar = FindBar::new("find");
        bar.show(false);
        assert_eq!(
            bar.update(&UiMsg::Submit("find:query".to_string(), "cat".to_string())),
            FindRequest::Next
        );
        assert_eq!(
            bar.update(&UiMsg::Click("find:previous".to_string())),
            FindRequest::Previous
        );
        assert_eq!(
            bar.update(&UiMsg::Submit(
                "find:replacement".to_string(),
                "".to_string()
            )),
            FindRequest::Replace
        );
        assert_eq!(
            bar.update(&UiMsg::Click("find:replace-all".to_string())),
            FindRequest::ReplaceAll
        );
        assert_eq!(
            bar.update(&UiMsg::Click("other:next".to_string())),
            FindRequest::Nothing
        );
        assert_eq!(
            bar.update(&UiMsg::Click("find:close".to_string())),
            FindRequest::Close
        );
        assert!(!bar.open)
    }

    #[test]
    fn the_summary_counts_from_one_and_bounds_the_selection() {
        let mut bar = FindBar::new("find");
        assert_eq!(bar.summary(), "");
        bar.query = "x".to_string();
        bar.found(0, 0);
        assert_eq!(bar.summary(), "No matches");
        bar.found(5, 1);
        assert_eq!(bar.summary(), "2 of 5");
        bar.found(5, 7);
        assert_eq!(bar.current, -1);
        assert_eq!(bar.summary(), "5 matches");
        bar.found(1, -1);
        assert_eq!(bar.summary(), "1 match")
    }
}
