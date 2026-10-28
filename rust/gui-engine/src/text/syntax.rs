use crate::strext::StrExt;
use crate::style::Theme;
use crate::style::color::Color;
use std::collections::HashSet;

/// Lexical category for a source byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Keyword,
    StringLiteral,
    Comment,
    Number,
    TypeName,
    Function,
}

/// A sorted, non-overlapping source byte range.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub start: i64,
    pub end: i64,
    pub kind: Kind,
}

/// A cached line with its outgoing multiline lexical state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceLine {
    pub text: String,
    pub spans: Vec<Span>,
    pub words: Vec<String>,
    pub incoming: i64,
    pub outgoing: i64,
}

/// Revision-aware source analysis shared by painting, completion, and status.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Document {
    pub text: String,
    pub language: String,
    pub lines: Vec<SourceLine>,
    pub spans: Vec<Span>,
    pub words: Vec<String>,
    /// Lines lexed during the latest update.
    pub scanned: i64,
    #[serde(default)]
    pub error: String,
}

impl Document {
    pub fn empty(language: &str) -> Self {
        Self {
            text: String::new(),
            language: language.to_owned(),
            lines: vec![],
            spans: vec![],
            words: vec![],
            scanned: 0,
            error: String::new(),
        }
    }

    /// Starts an analysis within the syntax byte budget.
    pub fn new(text: &str, language: &str) -> Document {
        let mut document = Self::empty(language);
        document.update(text);
        document
    }

    pub fn owned_bytes(&self) -> usize {
        self.text.capacity()
            + self.language.capacity()
            + self.error.capacity()
            + self.lines.capacity() * size_of::<SourceLine>()
            + self.spans.capacity() * size_of::<Span>()
            + self.words.capacity() * size_of::<String>()
            + self.words.iter().map(String::capacity).sum::<usize>()
            + self
                .lines
                .iter()
                .map(SourceLine::owned_bytes)
                .sum::<usize>()
    }

    /// Compatibility entry point. On a budget error, release the old analysis
    /// and expose the reason instead of retaining stale spans for new text.
    pub fn update(&mut self, text: &str) {
        if let Err(error) = self.update_cancellable(text, || true, &mut |_, _| {}) {
            let language = self.language.clone();
            *self = Self::empty(&language);
            self.error = error;
        }
    }

    /// Builds the next revision without mutating the old one until it succeeds.
    /// Source, line indexes, lexical spans, words and temporary lookup storage
    /// all have byte bounds. Cancellation is checked between source lines.
    pub fn update_cancellable(
        &mut self,
        text: &str,
        active: impl Fn() -> bool,
        progress: &mut impl FnMut(i64, i64),
    ) -> Result<(), String> {
        if text == self.text && !self.lines.is_empty() {
            self.scanned = 0;
            return Ok(());
        }
        if text.len() > SOURCE_BYTE_LIMIT {
            return Err("syntax source exceeds its byte budget".into());
        }
        let line_count = text.bytes().filter(|&b| b == b'\n').count() + 1;
        if line_count.saturating_mul(size_of::<SourceLine>() * 2 + size_of::<&str>())
            > SYNTAX_BYTE_LIMIT / 2
        {
            return Err("syntax line index exceeds its byte budget".into());
        }
        let parts: Vec<_> = text.split('\n').collect();
        let mut prefix = 0;
        while prefix < self.lines.len()
            && prefix < parts.len()
            && self.lines[prefix].text == parts[prefix]
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < self.lines.len() - prefix
            && suffix < parts.len() - prefix
            && self.lines[self.lines.len() - suffix - 1].text == parts[parts.len() - suffix - 1]
        {
            suffix += 1;
        }
        let mut made = Self::empty(&self.language);
        let mut seen = HashSet::new();
        let mut state = 0;
        let mut offset = 0;
        let mut charged = text.len() + parts.capacity() * size_of::<&str>() + 1024;
        for (i, part) in parts.iter().enumerate() {
            if !active() {
                return Err("syntax analysis cancelled or expired".into());
            }
            if part.len() > LINE_BYTE_LIMIT {
                return Err("syntax line exceeds its byte budget".into());
            }
            let old_i = if i < prefix {
                Some(i)
            } else if i >= parts.len() - suffix {
                Some(self.lines.len() - (parts.len() - i))
            } else {
                None
            };
            let line = if let Some(old) = old_i.filter(|&old| self.lines[old].incoming == state) {
                self.lines[old].clone()
            } else {
                made.scanned += 1;
                lex_line(part, &self.language, state)
            };
            charged = charged
                .saturating_add(line.owned_bytes().saturating_mul(3) + size_of::<SourceLine>() * 2);
            if charged > SYNTAX_BYTE_LIMIT {
                return Err("syntax analysis exceeds its byte budget".into());
            }
            state = line.outgoing;
            for span in &line.spans {
                made.spans.push(Span {
                    start: span.start + offset,
                    end: span.end + offset,
                    kind: span.kind,
                });
            }
            for word in &line.words {
                if seen.insert(word.clone()) {
                    made.words.push(word.clone());
                }
            }
            made.lines.push(line);
            offset += part.len() as i64 + 1;
            progress(offset.min(text.len() as i64), text.len() as i64);
        }
        made.text = text.to_owned();
        if made.owned_bytes() > SYNTAX_BYTE_LIMIT {
            return Err("syntax analysis exceeds its byte budget".into());
        }
        *self = made;
        Ok(())
    }
}

pub const SOURCE_BYTE_LIMIT: usize = 16 * 1024 * 1024;
pub const SYNTAX_BYTE_LIMIT: usize = 256 * 1024 * 1024;
pub const LINE_BYTE_LIMIT: usize = 2 * 1024 * 1024;
impl SourceLine {
    pub(crate) fn owned_bytes(&self) -> usize {
        self.text.capacity()
            + self.spans.capacity() * size_of::<Span>()
            + self.words.capacity() * size_of::<String>()
            + self.words.iter().map(String::capacity).sum::<usize>()
    }
}

fn ident(b: u8) -> bool {
    b >= 128
        || (65..=90).contains(&b)
        || (97..=122).contains(&b)
        || b == 95
        || (48..=57).contains(&b)
}

fn keyword(word: &str) -> bool {
    [
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "cohort",
        "comptime",
        "const",
        "continue",
        "def",
        "defer",
        "del",
        "else",
        "enum",
        "except",
        "export",
        "extends",
        "false",
        "False",
        "finally",
        "fn",
        "for",
        "from",
        "function",
        "if",
        "impl",
        "import",
        "in",
        "interface",
        "let",
        "loop",
        "match",
        "mod",
        "mut",
        "new",
        "None",
        "null",
        "pass",
        "pub",
        "raise",
        "return",
        "self",
        "spawn",
        "static",
        "struct",
        "super",
        "switch",
        "throw",
        "trait",
        "true",
        "True",
        "try",
        "type",
        "unsafe",
        "use",
        "var",
        "while",
        "with",
        "yield",
    ]
    .contains(&word)
}

/// The language a file's extension names, for `Document::new`: a code
/// language the lexer colors, or "text" for plain text.
pub fn language_for(path: &str) -> String {
    let dot = path.rfind(".").map_or(-1, |at| at as i64);
    let slash = path.rfind("/").map_or(-1, |at| at as i64);
    if dot <= slash + 1 {
        return "text".to_string();
    }
    let ext = path.substring(dot + 1, path.byte_len()).to_lowercase();
    if CODE_LANGUAGES.contains(&ext.as_str()) {
        ext
    } else {
        "text".to_string()
    }
}

// Extensions of the C-family and Python-like languages the lexer colors.
const CODE_LANGUAGES: [&str; 14] = [
    "gos", "rs", "c", "h", "cc", "cpp", "hpp", "go", "java", "js", "ts", "swift", "kt", "py",
];

pub(crate) fn lex_line(text: &str, language: &str, incoming: i64) -> SourceLine {
    if !CODE_LANGUAGES.contains(&language) {
        return plain_line(text);
    }
    let b = text.bytes().collect::<Vec<_>>();
    let mut spans = vec![];
    let mut words = vec![];
    let mut i = 0;
    let mut state = incoming;
    while i < b.len() {
        let start = i;
        let c = b[i];
        let next = if i + 1 < b.len() { b[i + 1] } else { 0u8 };
        if state > 0 || (state == 0 && c == 47 && next == 42) {
            if state == 0 {
                state = 1;
                i += 2
            };
            while i < b.len() {
                if i + 1 < b.len() && b[i] == 47 && b[i + 1] == 42 {
                    state += 1;
                    i += 2
                } else if i + 1 < b.len() && b[i] == 42 && b[i + 1] == 47 {
                    state -= 1;
                    i += 2;
                    if state == 0 {
                        break;
                    }
                } else {
                    i += 1
                }
            }
            spans.push(Span {
                start: start as i64,
                end: i as i64,
                kind: Kind::Comment,
            })
        } else if state == 0 && ((c == 47 && next == 47) || (language == "py" && c == 35)) {
            spans.push(Span {
                start: start as i64,
                end: b.len() as i64,
                kind: Kind::Comment,
            });
            break;
        } else if state < 0 || c == 34 || c == 39 || c == 96 {
            let quote = if state < 0 { (-state) as u8 } else { c };
            let triple = state < 0
                || (language == "py" && i + 2 < b.len() && b[i + 1] == quote && b[i + 2] == quote);
            if state == 0 {
                i += if triple { 3 } else { 1 }
            };
            state = if triple { -(quote as i64) } else { 0 };
            while i < b.len() {
                if b[i] == 92 {
                    i = (i + 2).min(b.len())
                } else if b[i] == quote
                    && (!triple || (i + 2 < b.len() && b[i + 1] == quote && b[i + 2] == quote))
                {
                    i += if triple { 3 } else { 1 };
                    state = 0;
                    break;
                } else {
                    i += 1
                }
            }
            spans.push(Span {
                start: start as i64,
                end: i as i64,
                kind: Kind::StringLiteral,
            })
        } else if (48..=57).contains(&c) {
            i += 1;
            while i < b.len() && (ident(b[i]) || b[i] == 46) {
                i += 1
            }
            spans.push(Span {
                start: start as i64,
                end: i as i64,
                kind: Kind::Number,
            })
        } else if ident(c) {
            i += 1;
            while i < b.len() && ident(b[i]) {
                i += 1
            }
            let word = text.substring(start as i64, i as i64);
            if word.byte_len() > 2 {
                words.push(word.to_string())
            };
            let mut after = i;
            while after < b.len() && (b[after] == 32 || b[after] == 9) {
                after += 1
            }
            let kind = if keyword(word) {
                Some(Kind::Keyword)
            } else if (65..=90).contains(&c) {
                Some(Kind::TypeName)
            } else if after < b.len() && b[after] == 40 {
                Some(Kind::Function)
            } else {
                None
            };
            if let Some(k) = kind {
                spans.push(Span {
                    start: start as i64,
                    end: i as i64,
                    kind: k,
                })
            }
        } else {
            i += 1
        }
    }
    SourceLine {
        text: text.to_string(),
        spans,
        words,
        incoming,
        outgoing: state,
    }
}

// Plain text carries no colored spans; its words still feed completion.
fn plain_line(text: &str) -> SourceLine {
    let mut words = vec![];
    let b = text.bytes().collect::<Vec<_>>();
    let mut i = 0;
    while i < b.len() {
        if ident(b[i]) {
            let start = i;
            while i < b.len() && ident(b[i]) {
                i += 1
            }
            if i - start > 2 {
                words.push(text.substring(start as i64, i as i64).to_string())
            }
        } else {
            i += 1
        }
    }
    SourceLine {
        text: text.to_string(),
        spans: vec![],
        words,
        incoming: 0,
        outgoing: 0,
    }
}

/// Resolves syntax colors against the current theme at paint time.
pub fn color(kind: Kind, theme: &Theme) -> Color {
    let dark = theme.field.luminance() < 0.3;
    match kind {
        Kind::Keyword => {
            if dark {
                Color::hex(0xc792ea)
            } else {
                Color::hex(0x7830a5)
            }
        }
        Kind::StringLiteral => {
            if dark {
                Color::hex(0xc3e88d)
            } else {
                Color::hex(0x286b24)
            }
        }
        Kind::Comment => theme.text_muted,
        Kind::Number => {
            if dark {
                Color::hex(0xf7b267)
            } else {
                Color::hex(0x994500)
            }
        }
        Kind::TypeName => {
            if dark {
                Color::hex(0xffcb6b)
            } else {
                Color::hex(0x865900)
            }
        }
        Kind::Function => {
            if dark {
                Color::hex(0x82aaff)
            } else {
                Color::hex(0x135c9b)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::strext::StrExt;
    #[test]
    fn plain_text_has_no_colors_and_extensions_pick_the_language() {
        let d = super::Document::new(
            "Dragged from the widget showcase.",
            &super::language_for("/tmp/notes.txt"),
        );
        assert!(d.spans.is_empty());
        assert!(d.words.contains(&"Dragged".to_string()));
        assert_eq!(super::language_for("src/main.gos"), "gos");
        assert_eq!(super::language_for("/tmp/Script.PY"), "py");
        assert_eq!(super::language_for("/tmp/.bashrc"), "text");
        assert_eq!(super::language_for("/home/a.b/README"), "text")
    }

    #[test]
    fn syntax_offsets_are_utf8_bytes_and_comment_state_converges() {
        let mut d =
            super::Document::new("let café = \"é\"\n/* start\ninside\n*/ fn done() {}", "gos");
        assert_eq!(d.lines.len(), 4);
        assert_eq!(d.lines[1].outgoing, 1);
        assert_eq!(d.lines[3].outgoing, 0);
        assert_eq!(d.text.substring(d.spans[1].start, d.spans[1].end), "\"é\"");
        d.update("let café = \"é\"\n// start\ninside\n*/ fn done() {}");
        assert_eq!(d.scanned, 3);
        assert_eq!(d.lines[2].spans.len(), 0);
        d.update("let café = \"é\"\n// start\ninside\n*/ fn done() {}");
        assert_eq!(d.scanned, 0)
    }

    #[test]
    fn unchanged_lines_are_reused_after_insertion() {
        let mut d = super::Document::new("fn first() {}\nfn second() {}", "gos");
        d.update("// heading\nfn first() {}\nfn second() {}");
        assert_eq!(d.scanned, 1);
        assert_eq!(d.text.substring(d.spans[1].start, d.spans[1].end), "fn")
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    #[test]
    fn an_oversized_source_or_line_is_rejected_without_retaining_it() {
        let mut document = Document::new("let previous = 1", "gos");
        let old = document.clone();
        let text = "x".repeat(SOURCE_BYTE_LIMIT + 1);
        assert!(
            document
                .update_cancellable(&text, || true, &mut |_, _| {})
                .is_err()
        );
        assert_eq!(document, old);
        document.update(&"x".repeat(LINE_BYTE_LIMIT + 1));
        assert!(!document.error.is_empty());
        assert_eq!(document.text, "");
        assert!(document.owned_bytes() < SYNTAX_BYTE_LIMIT);
    }
    #[test]
    fn a_cancelled_rebuild_keeps_the_previous_immutable_analysis() {
        let mut document = Document::new("let a = 1\nlet b = 2", "gos");
        let old = document.clone();
        assert!(
            document
                .update_cancellable("let a = 3\nlet b = 4", || false, &mut |_, _| {})
                .is_err()
        );
        assert_eq!(document, old);
    }
}
