//! Markdown and a deliberately bounded, non-executable XHTML-compatible subset.
use super::*;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Markdown,
    Html,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub offset: usize,
    pub message: String,
}
#[derive(Clone, Debug)]
pub struct Import {
    pub content: Content,
    pub diagnostics: Vec<Diagnostic>,
}
#[cfg(feature = "markup")]
mod read;
pub fn import(format: Format, source: &str, active: impl Fn() -> bool) -> Result<Import, String> {
    #[cfg(feature = "markup")]
    match format {
        Format::Markdown => read::markdown(source, &active),
        Format::Html => read::html(source, &active),
    }
    #[cfg(not(feature = "markup"))]
    {
        let _ = (source, active);
        Err(match format {
            Format::Markdown => "this build reads no Markdown".into(),
            Format::Html => "this build reads no HTML".into(),
        })
    }
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn inline_export(block: &Block) -> String {
    let mut output = String::new();
    for span in &block.spans {
        let m = &span.marks;
        let mut value = if let Some(image) = &span.image {
            let dimensions = format!(
                "{}{}",
                if image.width > 0 {
                    format!(" width=\"{}\"", image.width)
                } else {
                    String::new()
                },
                if image.height > 0 {
                    format!(" height=\"{}\"", image.height)
                } else {
                    String::new()
                }
            );
            format!(
                "<img src=\"{}\" alt=\"{}\" title=\"{}\"{dimensions}/>",
                escape(&image.source),
                escape(&image.alt),
                escape(&image.title)
            )
        } else {
            escape(&block.text.slice(span.start, span.end)).replace('\n', "<br/>")
        };
        for (on, tag) in [
            (m.code, "code"),
            (m.strong, "strong"),
            (m.emphasis, "em"),
            (m.underline, "u"),
            (m.strike, "s"),
        ] {
            if on {
                value = format!("<{tag}>{value}</{tag}>")
            }
        }
        if !m.link.is_empty() {
            value = format!(
                "<a href=\"{}\" title=\"{}\">{value}</a>",
                escape(&m.link),
                escape(&m.title)
            )
        }
        output.push_str(&value);
    }
    output
}
fn html_block(block: &Block) -> String {
    match &block.kind {
        Kind::Rule => "<hr/>".into(),
        Kind::Code(language) => format!(
            "<pre><code class=\"language-{}\">{}</code></pre>",
            escape(language),
            escape(&block.text.to_string())
        ),
        Kind::Paragraph => format!("<p>{}</p>", inline_export(block)),
        Kind::Heading(level) => format!("<h{level}>{}</h{level}>", inline_export(block)),
        kind => {
            let (tag, attrs) = match kind {
                Kind::Quote => ("blockquote", String::new()),
                Kind::List(None) => ("ul", String::new()),
                Kind::List(Some(start)) => ("ol", format!(" start=\"{start}\"")),
                Kind::Item => ("li", String::new()),
                _ => unreachable!(),
            };
            format!(
                "<{tag}{attrs}>{}</{tag}>",
                block
                    .children
                    .iter()
                    .map(|child| html_block(child))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    }
}
#[cfg(feature = "markup")]
fn markdown_text(text: &str) -> String {
    let mut output = String::new();
    for c in text.chars() {
        if c == '\n' {
            output.push_str("  \n")
        } else {
            if c.is_ascii_punctuation() {
                output.push('\\')
            }
            output.push(c)
        }
    }
    output
}
#[cfg(feature = "markup")]
fn markdown_inline(block: &Block) -> String {
    let mut output = String::new();
    for span in &block.spans {
        let marks = &span.marks;
        let text = block.text.slice(span.start, span.end);
        if marks.underline
            || span.image.is_some()
            || (!marks.title.is_empty())
            || text.starts_with(char::is_whitespace)
            || text.ends_with(char::is_whitespace)
        {
            let mut piece = block.clone();
            piece.text = Buffer::from(text);
            piece.spans = vec![Span {
                start: 0,
                end: piece.text.len(),
                ..span.clone()
            }];
            output.push_str(&inline_export(&piece));
            continue;
        }
        let mut value = if marks.code {
            let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1;
            let fence = "`".repeat(longest);
            let padding = if text.starts_with('`') || text.ends_with('`') {
                " "
            } else {
                ""
            };
            format!("{fence}{padding}{text}{padding}{fence}")
        } else {
            markdown_text(&text)
        };
        for (on, delimiter) in [
            (marks.strong, "**"),
            (marks.emphasis, "*"),
            (marks.strike, "~~"),
        ] {
            if on {
                value = format!("{delimiter}{value}{delimiter}")
            }
        }
        if !marks.link.is_empty() {
            value = format!(
                "[{value}](<{}>)",
                marks.link.replace('>', "%3E").replace('<', "%3C")
            )
        }
        output.push_str(&value);
    }
    output
}
#[cfg(feature = "markup")]
fn markdown_block(block: &Block) -> String {
    match &block.kind {
        Kind::Paragraph => markdown_inline(block),
        Kind::Heading(level) => {
            format!("{} {}", "#".repeat(*level as usize), markdown_inline(block))
        }
        Kind::Rule => "---".into(),
        Kind::Code(language) => {
            let text = block.text.to_string();
            let count = text
                .split(|c| c != '`')
                .map(str::len)
                .max()
                .unwrap_or(0)
                .max(2)
                + 1;
            let fence = "`".repeat(count);
            format!(
                "{fence}{language}\n{text}{}{fence}",
                if text.ends_with('\n') { "" } else { "\n" }
            )
        }
        Kind::Quote => block
            .children
            .iter()
            .map(|child| markdown_block(child))
            .collect::<Vec<_>>()
            .join("\n\n")
            .split('\n')
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Kind::List(start) => block
            .children
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let marker = start.map_or_else(
                    || "- ".into(),
                    |start| format!("{}. ", start + index as i64),
                );
                let indent = " ".repeat(marker.len());
                let body = item
                    .children
                    .iter()
                    .map(|child| markdown_block(child))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                body.split('\n')
                    .enumerate()
                    .map(|(i, line)| format!("{}{line}", if i == 0 { &marker } else { &indent }))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        Kind::Item => html_block(block),
    }
}
#[cfg(feature = "markup")]
fn semantic(content: &Content) -> serde_json::Value {
    let mut value = serde_json::to_value(content).unwrap();
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                fields.remove("id");
                for value in fields.values_mut() {
                    strip(value)
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    strip(value)
                }
            }
            _ => (),
        }
    }
    strip(&mut value);
    value
}
// Markdown that reads back as `content`. Without the reader the check
// cannot run, and HTML, which Markdown carries as written, stands in.
#[cfg(feature = "markup")]
fn faithful_markdown(content: &Content, active: &impl Fn() -> bool) -> Option<String> {
    let candidate = content
        .blocks
        .iter()
        .map(|block| markdown_block(block))
        .collect::<Vec<_>>()
        .join("\n\n");
    (candidate.len() <= SOURCE_LIMIT
        && read::markdown(&candidate, active)
            .is_ok_and(|roundtrip| semantic(&roundtrip.content) == semantic(content)))
    .then_some(candidate)
}
#[cfg(not(feature = "markup"))]
fn faithful_markdown(_: &Content, _: &impl Fn() -> bool) -> Option<String> {
    None
}
pub fn export(
    format: Format,
    content: &Content,
    active: impl Fn() -> bool,
) -> Result<String, String> {
    content.validate()?;
    if !active() {
        return Err("rich export cancelled".into());
    }
    let html = || {
        content
            .blocks
            .iter()
            .map(|block| html_block(block))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let output = match format {
        Format::Html => html(),
        Format::Markdown => faithful_markdown(content, &active).unwrap_or_else(html),
    };
    if !active() {
        return Err("rich export cancelled".into());
    }
    if output.len() > SOURCE_LIMIT {
        return Err("rich export exceeds round-trip source budget".into());
    }
    Ok(output)
}
