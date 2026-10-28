//! Markdown and a deliberately bounded, non-executable XHTML-compatible subset.
use super::*;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
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
fn limit(source: &str) -> Result<(), String> {
    if source.len() > SOURCE_LIMIT {
        Err("rich import source exceeds 8 MiB".into())
    } else {
        Ok(())
    }
}
fn finish_block(stack: &mut Vec<Block>, roots: &mut Vec<Arc<Block>>) -> Result<(), String> {
    let mut block = stack.pop().ok_or("malformed markup block stack")?;
    block.normalize();
    if let Some(parent) = stack.last_mut() {
        parent.children.push(Arc::new(block))
    } else {
        roots.push(Arc::new(block))
    }
    Ok(())
}
fn leaf<'a>(stack: &'a mut Vec<Block>, roots: &[Arc<Block>]) -> Result<&'a mut Block, String> {
    if stack.last().is_none_or(|block| block.kind.container()) {
        if roots.len() + stack.iter().map(|b| b.children.len()).sum::<usize>() > NODE_LIMIT {
            return Err("too many rich blocks".into());
        }
        stack.push(Block::new(Kind::Paragraph));
    }
    Ok(stack.last_mut().unwrap())
}
fn inline_html(
    source: &str,
    marks: &mut Marks,
    stack: &mut Vec<(String, Marks)>,
) -> Result<Option<Image>, String> {
    if source.starts_with("</") {
        let name = source.trim_start_matches("</").trim_end_matches('>').trim();
        let (open, previous) = stack.pop().ok_or("unmatched inline HTML close")?;
        if open != name {
            return Err("mismatched inline HTML close".into());
        }
        *marks = previous;
        return Ok(None);
    }
    let normalized = if source.trim_end().ends_with("/>") {
        source.to_string()
    } else {
        format!("{}/>", source.trim_end().trim_end_matches('>'))
    };
    let document = roxmltree::Document::parse_with_options(
        &normalized,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 32,
        },
    )
    .map_err(|e| format!("inline HTML: {e}"))?;
    let node = document.root_element();
    let name = node.tag_name().name();
    for attribute in node.attributes() {
        if !matches!(
            (name, attribute.name()),
            ("a", "href" | "title") | ("img", "src" | "alt" | "title" | "width" | "height")
        ) {
            return Err(format!(
                "unsupported inline HTML attribute {}",
                attribute.name()
            ));
        }
    }
    if name == "img" {
        return Ok(Some(Image {
            width: node
                .attribute("width")
                .unwrap_or("0")
                .parse()
                .map_err(|_| "invalid image width")?,
            height: node
                .attribute("height")
                .unwrap_or("0")
                .parse()
                .map_err(|_| "invalid image height")?,
            source: node.attribute("src").unwrap_or("").into(),
            alt: node.attribute("alt").unwrap_or("").into(),
            title: node.attribute("title").unwrap_or("").into(),
        }));
    }
    let previous = marks.clone();
    match name {
        "strong" | "b" => marks.strong = true,
        "em" | "i" => marks.emphasis = true,
        "u" => marks.underline = true,
        "s" | "del" => marks.strike = true,
        "code" => marks.code = true,
        "a" => {
            marks.link = node.attribute("href").unwrap_or("").into();
            marks.title = node.attribute("title").unwrap_or("").into();
        }
        "br" => return Err("line break sentinel".into()),
        _ => return Err(format!("unsupported inline HTML element {name}")),
    }
    if !safe_url(&marks.link) {
        return Err("unsafe inline HTML link".into());
    }
    stack.push((name.into(), previous));
    Ok(None)
}
pub fn import(format: Format, source: &str, active: impl Fn() -> bool) -> Result<Import, String> {
    match format {
        Format::Markdown => markdown(source, &active),
        Format::Html => html(source, &active),
    }
}
fn markdown(source: &str, active: &impl Fn() -> bool) -> Result<Import, String> {
    limit(source)?;
    let mut roots = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut marks = Marks::default();
    let mut inline = Vec::new();
    let mut html_inline = Vec::new();
    let mut images: Vec<Image> = Vec::new();
    let mut raw_html = String::new();
    let options = Options::ENABLE_STRIKETHROUGH;
    let mut block_count = 0;
    let mut token_count = 0;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        token_count += 1;
        if token_count > SPAN_LIMIT * 8 + NODE_LIMIT * 8 {
            return Err("too many rich markup tokens".into());
        }
        if !active() {
            return Err("rich import cancelled".into());
        }
        if blocks.len() + inline.len() + html_inline.len() > DEPTH_LIMIT {
            return Err("rich import nesting limit".into());
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph
                | Tag::Heading { .. }
                | Tag::CodeBlock(_)
                | Tag::BlockQuote(_)
                | Tag::List(_)
                | Tag::Item => {
                    if blocks.last().is_some_and(|b| !b.kind.container()) {
                        finish_block(&mut blocks, &mut roots)?;
                    }
                    let kind = match tag {
                        Tag::Paragraph => Kind::Paragraph,
                        Tag::Heading { level, .. } => Kind::Heading(level as i64),
                        Tag::CodeBlock(kind) => Kind::Code(match kind {
                            pulldown_cmark::CodeBlockKind::Fenced(language) => language.to_string(),
                            _ => String::new(),
                        }),
                        Tag::BlockQuote(_) => Kind::Quote,
                        Tag::List(start) => Kind::List(start.map(|n| n as i64)),
                        Tag::Item => Kind::Item,
                        _ => unreachable!(),
                    };
                    block_count += 1;
                    if block_count > NODE_LIMIT {
                        return Err("too many Markdown blocks".into());
                    }
                    blocks.push(Block::new(kind));
                }
                Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } => {
                    inline.push(marks.clone());
                    match tag {
                        Tag::Emphasis => marks.emphasis = true,
                        Tag::Strong => marks.strong = true,
                        Tag::Strikethrough => marks.strike = true,
                        Tag::Link {
                            dest_url, title, ..
                        } => {
                            marks.link = dest_url.to_string();
                            marks.title = title.to_string();
                            if !safe_url(&marks.link) {
                                return Err(format!("unsafe Markdown link at {}", range.start));
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                Tag::Image {
                    dest_url, title, ..
                } => {
                    if !safe_url(&dest_url) {
                        return Err("unsafe image URL".into());
                    }
                    images.push(Image {
                        width: 0,
                        height: 0,
                        source: dest_url.into_string(),
                        title: title.into_string(),
                        alt: String::new(),
                    });
                }
                Tag::HtmlBlock => raw_html.clear(),
                _ => return Err(format!("unsupported Markdown construct at {}", range.start)),
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock => {
                    finish_block(&mut blocks, &mut roots)?
                }
                TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item => {
                    if blocks.last().is_some_and(|b| !b.kind.container()) {
                        finish_block(&mut blocks, &mut roots)?;
                    }
                    finish_block(&mut blocks, &mut roots)?;
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                    marks = inline.pop().ok_or("unmatched Markdown inline close")?
                }
                TagEnd::Image => {
                    let image = images.pop().ok_or("unmatched Markdown image")?;
                    leaf(&mut blocks, &roots)?.append("\u{fffc}", &marks, Some(image));
                }
                TagEnd::HtmlBlock => {
                    let imported = html(&raw_html, active)?;
                    if let Some(parent) = blocks.last_mut() {
                        parent.children.extend(imported.content.blocks)
                    } else {
                        roots.extend(imported.content.blocks)
                    }
                    raw_html.clear();
                }
                _ => return Err("unsupported Markdown close".into()),
            },
            Event::Text(text) => {
                if let Some(image) = images.last_mut() {
                    image.alt.push_str(&text)
                } else {
                    leaf(&mut blocks, &roots)?.append(&text, &marks, None)
                }
            }
            Event::Code(text) => {
                if let Some(image) = images.last_mut() {
                    image.alt.push_str(&text)
                } else {
                    let mut code = marks.clone();
                    code.code = true;
                    leaf(&mut blocks, &roots)?.append(&text, &code, None)
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(image) = images.last_mut() {
                    image.alt.push(' ')
                } else {
                    leaf(&mut blocks, &roots)?.append("\n", &marks, None)
                }
            }
            Event::InlineHtml(text) => {
                if matches!(text.trim(), "<br>" | "<br/>" | "<br />") {
                    leaf(&mut blocks, &roots)?.append("\n", &marks, None)
                } else if let Some(image) = inline_html(&text, &mut marks, &mut html_inline)? {
                    leaf(&mut blocks, &roots)?.append("\u{fffc}", &marks, Some(image))
                }
            }
            Event::Html(text) => raw_html.push_str(&text),
            Event::Rule => {
                if blocks.last().is_some_and(|b| !b.kind.container()) {
                    finish_block(&mut blocks, &mut roots)?;
                }
                let rule = Arc::new(Block::new(Kind::Rule));
                if let Some(parent) = blocks.last_mut() {
                    parent.children.push(rule)
                } else {
                    roots.push(rule)
                }
            }
            _ => return Err(format!("unsupported Markdown token at {}", range.start)),
        }
    }
    while !blocks.is_empty() {
        finish_block(&mut blocks, &mut roots)?;
    }
    if !inline.is_empty() || !html_inline.is_empty() || !images.is_empty() {
        return Err("unclosed rich inline markup".into());
    }
    let content = if roots.is_empty() {
        Content::empty()
    } else {
        Content { blocks: roots }
    };
    content.validate()?;
    Ok(Import {
        content,
        diagnostics: Vec::new(),
    })
}
fn html(source: &str, active: &impl Fn() -> bool) -> Result<Import, String> {
    limit(source)?;
    // XML parsing deliberately rejects DTDs, external entities, malformed nesting
    // and browser repair rules. The exported subset is always well-formed.
    let wrapped = format!("<rich-root>{source}</rich-root>");
    let document = roxmltree::Document::parse_with_options(
        &wrapped,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: (SPAN_LIMIT * 4 + NODE_LIMIT * 4) as u32,
        },
    )
    .map_err(|error| format!("unsupported/malformed HTML subset: {error}"))?;
    fn attr(node: roxmltree::Node<'_, '_>, name: &str) -> String {
        node.attribute(name).unwrap_or("").to_string()
    }
    fn inlines(
        node: roxmltree::Node<'_, '_>,
        marks: &Marks,
        block: &mut Block,
        depth: usize,
        active: &impl Fn() -> bool,
    ) -> Result<(), String> {
        if depth > DEPTH_LIMIT || !active() {
            return Err("HTML nesting limit or cancellation".into());
        }
        for child in node.children() {
            if child.is_text() {
                block.append(child.text().unwrap_or(""), marks, None);
                continue;
            }
            if !child.is_element() {
                continue;
            }
            let name = child.tag_name().name();
            if child.tag_name().namespace().is_some() {
                return Err("unsupported HTML namespace".into());
            }
            if matches!(name, "br" | "img")
                && child
                    .children()
                    .any(|node| node.is_element() || node.is_text())
            {
                return Err("HTML void element has children".into());
            }
            for a in child.attributes() {
                if !matches!(
                    (name, a.name()),
                    ("a", "href" | "title") | ("img", "src" | "alt" | "title" | "width" | "height")
                ) {
                    return Err(format!("unsupported HTML inline attribute {}", a.name()));
                }
            }
            let mut next = marks.clone();
            match name {
                "strong" | "b" => next.strong = true,
                "em" | "i" => next.emphasis = true,
                "u" => next.underline = true,
                "s" | "del" => next.strike = true,
                "code" => next.code = true,
                "a" => {
                    next.link = attr(child, "href");
                    next.title = attr(child, "title");
                    if !safe_url(&next.link) {
                        return Err("unsafe HTML link".into());
                    }
                }
                "br" => {
                    block.append("\n", marks, None);
                    continue;
                }
                "img" => {
                    let image = Image {
                        width: child
                            .attribute("width")
                            .unwrap_or("0")
                            .parse()
                            .map_err(|_| "invalid image width")?,
                        height: child
                            .attribute("height")
                            .unwrap_or("0")
                            .parse()
                            .map_err(|_| "invalid image height")?,
                        source: attr(child, "src"),
                        alt: attr(child, "alt"),
                        title: attr(child, "title"),
                    };
                    if !safe_url(&image.source) {
                        return Err("unsafe HTML image URL".into());
                    }
                    block.append("\u{fffc}", marks, Some(image));
                    continue;
                }
                _ => return Err(format!("unsupported HTML inline element {name}")),
            }
            inlines(child, &next, block, depth + 1, active)?;
        }
        Ok(())
    }
    fn blocks(
        node: roxmltree::Node<'_, '_>,
        depth: usize,
        active: &impl Fn() -> bool,
        count: &mut usize,
    ) -> Result<Vec<Arc<Block>>, String> {
        if depth > DEPTH_LIMIT || !active() {
            return Err("HTML nesting limit or cancellation".into());
        }
        let mut output = Vec::new();
        for child in node.children() {
            if child.is_text() {
                if !child.text().unwrap_or("").trim().is_empty() {
                    return Err("HTML text must be inside a paragraph/heading/code block".into());
                }
                continue;
            }
            if !child.is_element() {
                continue;
            }
            *count += 1;
            if *count > NODE_LIMIT {
                return Err("too many HTML blocks".into());
            }
            let name = child.tag_name().name();
            if child.tag_name().namespace().is_some() {
                return Err("unsupported HTML namespace".into());
            }
            if name == "hr"
                && child
                    .children()
                    .any(|node| node.is_element() || node.is_text())
            {
                return Err("HTML rule has children".into());
            }
            if matches!(name, "html" | "body" | "div") {
                if child.attributes().len() > 0 {
                    return Err("unsupported HTML container attributes".into());
                }
                output.extend(blocks(child, depth + 1, active, count)?);
                continue;
            }
            let kind = match name {
                "p" => Kind::Paragraph,
                "h1" => Kind::Heading(1),
                "h2" => Kind::Heading(2),
                "h3" => Kind::Heading(3),
                "h4" => Kind::Heading(4),
                "h5" => Kind::Heading(5),
                "h6" => Kind::Heading(6),
                "pre" => Kind::Code(String::new()),
                "blockquote" => Kind::Quote,
                "ul" => Kind::List(None),
                "ol" => Kind::List(Some(
                    child
                        .attribute("start")
                        .unwrap_or("1")
                        .parse()
                        .map_err(|_| "invalid list start")?,
                )),
                "li" => Kind::Item,
                "hr" => Kind::Rule,
                _ => return Err(format!("unsupported HTML block {name}")),
            };
            for a in child.attributes() {
                if !(name == "ol" && a.name() == "start") {
                    return Err(format!("unsupported HTML block attribute {}", a.name()));
                }
            }
            let mut block = Block::new(kind);
            if block.kind.container() {
                if block.kind == Kind::Item
                    && child.children().all(|node| {
                        !node.is_element()
                            || matches!(
                                node.tag_name().name(),
                                "strong"
                                    | "b"
                                    | "em"
                                    | "i"
                                    | "u"
                                    | "s"
                                    | "del"
                                    | "code"
                                    | "a"
                                    | "img"
                                    | "br"
                            )
                    })
                {
                    // Tight HTML list items without p are normalized to one paragraph.
                    let mut paragraph = Block::new(Kind::Paragraph);
                    inlines(child, &Marks::default(), &mut paragraph, depth + 1, active)?;
                    block.children.push(Arc::new(paragraph));
                } else {
                    block.children = blocks(child, depth + 1, active, count)?;
                }
            } else if matches!(block.kind, Kind::Code(_)) {
                let code = child.children().find(|node| node.is_element());
                let text_node = if let Some(code) = code {
                    if code.tag_name().name() != "code" || code.children().any(|n| n.is_element()) {
                        return Err("pre must contain plain code text".into());
                    }
                    for a in code.attributes() {
                        if a.name() != "class" {
                            return Err("unsupported code attribute".into());
                        }
                    }
                    if child.children().any(|node| {
                        node != code
                            && (node.is_element()
                                || (node.is_text() && !node.text().unwrap_or("").trim().is_empty()))
                    }) {
                        return Err("pre has content outside its code element".into());
                    }
                    let class = code.attribute("class").unwrap_or("");
                    let language = if class.is_empty() {
                        ""
                    } else {
                        class
                            .strip_prefix("language-")
                            .ok_or("unsupported code class")?
                    };
                    block.kind = Kind::Code(language.into());
                    code
                } else {
                    child
                };
                let text = text_node
                    .children()
                    .filter_map(|node| node.text())
                    .collect::<String>();
                block.append(&text, &Marks::default(), None);
            } else if block.kind != Kind::Rule {
                inlines(child, &Marks::default(), &mut block, depth + 1, active)?;
            }
            block.normalize();
            output.push(Arc::new(block));
        }
        Ok(output)
    }
    let mut count = 0;
    let blocks = blocks(document.root_element(), 1, active, &mut count)?;
    let content = if blocks.is_empty() {
        Content::empty()
    } else {
        Content { blocks }
    };
    content.validate()?;
    Ok(Import {
        content,
        diagnostics: Vec::new(),
    })
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
        Format::Markdown => {
            let candidate = content
                .blocks
                .iter()
                .map(|block| markdown_block(block))
                .collect::<Vec<_>>()
                .join("\n\n");
            if candidate.len() <= SOURCE_LIMIT
                && markdown(&candidate, &active)
                    .is_ok_and(|roundtrip| semantic(&roundtrip.content) == semantic(content))
            {
                candidate
            } else {
                html()
            }
        }
    };
    if !active() {
        return Err("rich export cancelled".into());
    }
    if output.len() > SOURCE_LIMIT {
        return Err("rich export exceeds round-trip source budget".into());
    }
    Ok(output)
}
