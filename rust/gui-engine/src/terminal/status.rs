//! The Program Status Protocol (OSC 7501, revision 0.4): a program reports
//! what it is doing (`ESC ] 7501 ; key=value : ... ST`) and asks whether the
//! terminal listens (`ESC ] 7501 ; ? ST`).

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::{Deserialize, Serialize};

/// What a program reported about itself or one of its tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The record reported on: a `/`-separated path, empty for the program's
    /// own.
    pub id: String,
    /// `idle`, `working`, `blocked`, `done` or `error`; `clear` removes the
    /// record and those under it, or every record when `id` is empty.
    pub state: String,
    /// The program's stable name, empty when it gave none.
    pub app: String,
    /// What a `blocked` program waits for: `permission`, `question` or
    /// `auth`; empty otherwise.
    pub kind: String,
    /// Percent done while `working` or `blocked`, or -1 when not known.
    pub progress: i64,
    /// A short label for the record.
    pub title: String,
    /// One line on what the program is doing, or why it waits.
    pub msg: String,
}

/// What the terminal answers a query with: it listens, for every state and
/// kind of this revision.
pub const ANSWER: &[u8] = b"\x1b]7501;?\x1b\\";

/// The OSC number the protocol uses, as it starts the string.
pub const PREFIX: &[u8] = b"\x1b]7501;";

const KEY_LIMIT: usize = 16;
const APP_LIMIT: usize = 32;
const ID_LIMIT: usize = 128;
const ID_SEGMENT_LIMIT: usize = 32;
const ID_SEGMENTS: usize = 8;
const TITLE_LIMIT: (usize, usize) = (256, 192);
const MSG_LIMIT: (usize, usize) = (2732, 2048);

// Standard base64, with or without its padding.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// What one OSC 7501 string asks of the terminal.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Query,
    Report(Status),
}

/// Reads the body of an OSC 7501 string (after `7501;`, before its end);
/// None when it is a report the protocol says to ignore.
pub fn read(body: &[u8]) -> Option<Request> {
    if body.first() == Some(&b'?') {
        return Some(Request::Query);
    }
    let text = std::str::from_utf8(body).ok()?;
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for pair in text.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.len() > KEY_LIMIT {
            return None;
        }
        let well_formed = !key.is_empty()
            && key.bytes().all(|b| b.is_ascii_lowercase())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b));
        if !well_formed {
            continue;
        }
        // A key given twice keeps its last value.
        pairs.retain(|(k, _)| *k != key);
        pairs.push((key, value));
    }
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .unwrap_or_default()
    };
    let state = get("state");
    if !matches!(
        state,
        "idle" | "working" | "blocked" | "done" | "error" | "clear"
    ) {
        return None;
    }
    let id = get("id");
    if id.len() > ID_LIMIT
        || (!id.is_empty()
            && (id.split('/').count() > ID_SEGMENTS
                || id
                    .split('/')
                    .any(|s| s.is_empty() || s.len() > ID_SEGMENT_LIMIT)))
    {
        return None;
    }
    let app = get("app");
    if app.len() > APP_LIMIT {
        return None;
    }
    let app = if app
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
    {
        app
    } else {
        ""
    };
    let kind = match (state, get("kind")) {
        ("blocked", kind @ ("permission" | "question" | "auth")) => kind,
        _ => "",
    };
    let progress = match state {
        "working" | "blocked" => get("progress")
            .parse::<i64>()
            .ok()
            .filter(|p| (0..=100).contains(p))
            .unwrap_or(-1),
        _ => -1,
    };
    Some(Request::Report(Status {
        id: id.to_owned(),
        state: state.to_owned(),
        app: app.to_owned(),
        kind: kind.to_owned(),
        progress,
        title: text_of(get("title"), TITLE_LIMIT)?,
        msg: text_of(get("msg"), MSG_LIMIT)?,
    }))
}

// The text base64 `encoded` holds, within `limit` (encoded, decoded bytes);
// None when it breaks a limit, does not decode, or holds a control character.
// Characters that reorder or hide text are left out.
fn text_of(encoded: &str, limit: (usize, usize)) -> Option<String> {
    if encoded.len() > limit.0 {
        return None;
    }
    let bytes = BASE64.decode(encoded).ok()?;
    if bytes.len() > limit.1 {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    Some(text.chars().filter(|c| !hides_text(*c)).collect())
}

// Bidirectional overrides and isolates, and characters that draw nothing.
fn hides_text(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(body: &str) -> Option<Status> {
        match read(body.as_bytes()) {
            Some(Request::Report(status)) => Some(status),
            _ => None,
        }
    }

    #[test]
    fn a_query_is_told_apart_from_reports() {
        assert_eq!(read(b"?"), Some(Request::Query));
        assert!(report("state=idle").is_some());
    }

    #[test]
    fn a_blocked_report_carries_its_kind_message_and_app() {
        let msg = BASE64.encode("Allow touch probe.txt?");
        let status = report(&format!(
            "state=blocked:app=codex:kind=permission:progress=40:msg={msg}"
        ))
        .unwrap();
        assert_eq!(
            status,
            Status {
                id: String::new(),
                state: "blocked".into(),
                app: "codex".into(),
                kind: "permission".into(),
                progress: 40,
                title: String::new(),
                msg: "Allow touch probe.txt?".into(),
            }
        );
    }

    #[test]
    fn reports_without_a_known_state_are_ignored() {
        assert_eq!(report("app=codex"), None);
        assert_eq!(report("state=thinking"), None);
        assert_eq!(report("state=Idle"), None);
    }

    #[test]
    fn malformed_pairs_are_skipped_and_the_last_of_a_key_wins() {
        let status =
            report(" state = working : app=co dex : Kind=x : state=blocked : nokey").unwrap();
        assert_eq!(status.state, "blocked");
        assert_eq!(status.app, "");
        assert_eq!(status.kind, "");
        assert_eq!(status.progress, -1);
    }

    #[test]
    fn kind_and_progress_count_only_where_they_apply() {
        let done = report("state=done:kind=permission:progress=50").unwrap();
        assert_eq!((done.kind.as_str(), done.progress), ("", -1));
        let odd = report("state=blocked:kind=coffee:progress=101").unwrap();
        assert_eq!((odd.kind.as_str(), odd.progress), ("", -1));
    }

    #[test]
    fn a_report_past_a_limit_or_with_text_that_does_not_read_is_dropped() {
        assert_eq!(report(&format!("state=idle:app={}", "a".repeat(33))), None);
        assert_eq!(report(&format!("state=idle:id={}", "a".repeat(33))), None);
        assert_eq!(report("state=idle:id=a/b/c/d/e/f/g/h/i"), None);
        assert_eq!(report("state=idle:id=a//b"), None);
        assert_eq!(report("state=idle:msg=a"), None);
        assert_eq!(report("state=idle:msg=//8"), None);
        // A value outside the protocol's characters is a malformed pair,
        // skipped like any other.
        assert_eq!(report("state=idle:msg=%%%").unwrap().msg, "");
        let control = BASE64.encode("two\nlines");
        assert_eq!(report(&format!("state=idle:msg={control}")), None);
        let long = BASE64.encode("t".repeat(193));
        assert_eq!(report(&format!("state=idle:title={long}")), None);
        assert!(report("state=idle:id=build/test").is_some());
    }

    #[test]
    fn text_decodes_without_padding_and_loses_characters_that_hide_text() {
        let padded = BASE64.encode("ok");
        let bare = padded.trim_end_matches('=');
        assert_eq!(report(&format!("state=idle:msg={bare}")).unwrap().msg, "ok");
        let sneaky = BASE64.encode("safe\u{202E}txt.exe\u{200B}");
        assert_eq!(
            report(&format!("state=idle:title={sneaky}")).unwrap().title,
            "safetxt.exe"
        );
    }
}
