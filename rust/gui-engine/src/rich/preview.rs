//! A structured, selectable preview composed from normal accessible widgets.
//! Pagination bounds descriptions; paragraph shaping uses the shared worker.
use super::*;
use crate::{
    accessibility::Role,
    style::TextRole,
    widgets::{
        Widget,
        display::Label,
        textinput::{InputKind, TextInput},
    },
};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlineRow {
    pub block: i64,
    pub depth: i64,
    pub kind: Kind,
    pub label: String,
}
/// The first 80 characters of a block's text, with each inline image's
/// placeholder character replaced by its alternative text.
fn readable(block: &Block) -> String {
    let mut out = String::new();
    let mut at = 0;
    for c in block.text.chars().take(80) {
        if c == '\u{FFFC}' {
            let alt = block
                .spans
                .iter()
                .find(|s| s.start == at)
                .and_then(|s| s.image.as_ref())
                .map(|image| image.alt.trim())
                .unwrap_or("");
            out.push_str(if alt.is_empty() { "Image" } else { alt });
        } else {
            out.push(c);
        }
        at += c.len_utf8();
    }
    out
}

pub fn outline(document: i64, start: usize, count: usize) -> Result<Vec<OutlineRow>, String> {
    if count > 256 {
        return Err("rich outline page exceeds 256 rows".into());
    }
    let snapshot = registry::snapshot(document)?;
    let mut rows = Vec::new();
    let mut cursor = 0;
    fn visit(
        block: &Block,
        depth: i64,
        cursor: &mut usize,
        start: usize,
        count: usize,
        rows: &mut Vec<OutlineRow>,
    ) {
        if block.kind.container() {
            for child in &block.children {
                visit(child, depth + 1, cursor, start, count, rows)
            }
        } else {
            if *cursor >= start && rows.len() < count {
                let label = if block.kind == Kind::Rule {
                    "Horizontal rule".into()
                } else {
                    readable(block)
                };
                rows.push(OutlineRow {
                    block: block.id,
                    depth,
                    kind: block.kind.clone(),
                    label,
                });
            }
            *cursor += 1;
        }
    }
    for block in &snapshot.content.blocks {
        visit(block, 0, &mut cursor, start, count, &mut rows)
    }
    Ok(rows)
}
pub fn widget(document: i64, prefix: &str, start: usize, count: usize) -> Result<Widget, String> {
    if count > 64 || prefix.len() > 2048 {
        return Err("rich preview page exceeds its limits".into());
    }
    let snapshot = registry::snapshot(document)?;
    let _temporary = crate::app::effects::current_budget().reserve(
        snapshot
            .content
            .owned_bytes()
            .saturating_mul(3)
            .saturating_add(1024 * 1024),
    )?;
    let mut cursor = 0;
    fn visit(
        block: &Block,
        document: i64,
        prefix: &str,
        cursor: &mut usize,
        start: usize,
        count: usize,
        ordinal: Option<i64>,
    ) -> Option<Widget> {
        if block.kind.container() {
            let mut children = Vec::new();
            for (index, child) in block.children.iter().enumerate() {
                let marker = if let Kind::List(number) = block.kind {
                    Some(number.map_or(0, |n| n + index as i64))
                } else {
                    None
                };
                if let Some(widget) = visit(child, document, prefix, cursor, start, count, marker) {
                    children.push(widget);
                }
            }
            if children.is_empty() {
                return None;
            }
            let content = Widget::column(&children, 8);
            return Some(match block.kind {
                Kind::Quote => content
                    .padding_sides(16, 4, 0, 4)
                    .role(Role::Group)
                    .accessible("Quotation"),
                Kind::List(start) => content.role(Role::Group).accessible(if start.is_some() {
                    "Ordered list"
                } else {
                    "Unordered list"
                }),
                Kind::Item => {
                    let marker = if ordinal == Some(0) {
                        "•".into()
                    } else {
                        format!("{}.", ordinal.unwrap_or(1))
                    };
                    Widget::row(&[Widget::label(&marker), content.grow(1)], 8).role(Role::ListItem)
                }
                _ => content,
            });
        }
        let visible = *cursor >= start && *cursor < start.saturating_add(count);
        *cursor += 1;
        if !visible {
            return None;
        }
        if block.kind == Kind::Rule {
            return Some(Widget::separator());
        }
        let role = match block.kind {
            Kind::Heading(1) => TextRole::Title,
            Kind::Heading(_) => TextRole::Heading,
            Kind::Code(_) => TextRole::Mono,
            _ => TextRole::Body,
        };
        let mut label = Label::new(&block.text.to_string(), role);
        label.id = format!("{prefix}:block:{}", block.id);
        label.wrap = true;
        label.selectable = true;
        label.spans = block.spans.clone();
        label.rich_document = document;
        if let Kind::Heading(level) = block.kind {
            label.heading_level = level;
        }
        Some(Widget::Text(label))
    }
    let mut children = Vec::new();
    for block in &snapshot.content.blocks {
        if let Some(widget) = visit(block, document, prefix, &mut cursor, start, count, None) {
            children.push(widget);
        }
    }
    Ok(Widget::column(&children, 12)
        .key(prefix)
        .role(Role::Document)
        .accessible("Document preview"))
}
pub fn input(document: i64, block: i64, id: &str) -> Result<TextInput, String> {
    let snapshot = registry::snapshot(document)?;
    let info = registry::info(document)?;
    let leaf = snapshot
        .content
        .find(block)
        .ok_or("rich input block does not exist")?;
    if leaf.kind.container() || leaf.kind == Kind::Rule {
        return Err("rich input needs a text block".into());
    }
    let mut input = TextInput::new(id, "", "", InputKind::Area);
    input.rich_document = document;
    input.rich_block = block;
    input.read_only = info.readonly;
    input.label = "Document block".into();
    input.rows = 8;
    input.mono = matches!(leaf.kind, Kind::Code(_));
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geometry::{Constraints, Size},
        memory::Budget,
        text::{
            fonts::{self, Family},
            layout::{self, LayoutOptions, TextStyle},
        },
        widgets::{cx::LayoutCx, state::StateStore, tree},
    };
    #[test]
    fn styled_geometry_uses_font_runs_without_breaking_arabic_joining_context() {
        let source = "سلام";
        let first = Marks {
            strong: true,
            ..Marks::default()
        };
        let spans = vec![
            Span {
                start: 0,
                end: 2,
                marks: first,
                image: None,
            },
            Span {
                start: 2,
                end: source.len(),
                marks: Marks::default(),
                image: None,
            },
        ];
        let styled = layout::try_layout_rich(
            source,
            &TextStyle::sans(18.0),
            LayoutOptions::single_line(),
            &spans,
        )
        .unwrap();
        let whole = layout::layout(
            source,
            &TextStyle::sans(18.0).with_bold(true),
            LayoutOptions::single_line(),
        );
        let actual = styled.lines[0]
            .glyphs
            .iter()
            .find(|g| g.start == 0)
            .unwrap();
        let expected = whole.lines[0].glyphs.iter().find(|g| g.start == 0).unwrap();
        assert_eq!(actual.face, fonts::primary(&Family::Sans, true));
        assert_eq!(actual.gid, expected.gid);
    }
    #[test]
    fn decorations_split_ligatures_at_style_boundaries_and_images_reserve_real_geometry() {
        let italic = Marks {
            emphasis: true,
            ..Marks::default()
        };
        let spans = vec![
            Span {
                start: 0,
                end: 1,
                marks: italic,
                image: None,
            },
            Span {
                start: 1,
                end: 2,
                marks: Marks::default(),
                image: None,
            },
        ];
        let styled = layout::try_layout_rich(
            "fi",
            &TextStyle::sans(18.0),
            LayoutOptions::single_line(),
            &spans,
        )
        .unwrap();
        assert!(
            styled.lines[0]
                .glyphs
                .iter()
                .all(|g| !(g.start < 1 && g.end > 1))
        );
        let image = Image {
            width: 80,
            height: 40,
            source: "asset:test".into(),
            alt: "Diagram".into(),
            title: String::new(),
        };
        let spans = vec![Span {
            start: 0,
            end: 3,
            marks: Marks::default(),
            image: Some(image),
        }];
        let styled = layout::try_layout_rich(
            "\u{fffc}",
            &TextStyle::sans(18.0),
            LayoutOptions::single_line(),
            &spans,
        )
        .unwrap();
        assert!(styled.width >= 79.0 && styled.width <= 81.0);
        assert!(styled.height >= 40.0);
    }
    #[test]
    fn outline_rows_name_inline_images_by_their_alternative_text() {
        let id = registry::import(
            markup::Format::Markdown,
            "# Notes\n\nSee ![Sample image](asset:sample) and ![](asset:bare)\n",
            crate::memory::Budget::new(1 << 26),
            || true,
        )
        .unwrap();
        let labels: Vec<String> = outline(id, 0, 16)
            .unwrap()
            .into_iter()
            .map(|r| r.label)
            .collect();
        assert_eq!(labels, ["Notes", "See Sample image and Image"]);
    }

    #[test]
    fn preview_exports_document_heading_link_and_image_semantics_with_actions() {
        let id=registry::import(markup::Format::Html,"<h3>Heading</h3><p><a href=\"https://example.com\">Link</a><img src=\"asset:diagram\" alt=\"Diagram\" width=\"40\" height=\"20\"/></p>",Budget::new(256*1024*1024),||true).unwrap();
        let widget = widget(id, "preview-test", 0, 16).unwrap();
        let theme = crate::style::Theme::light();
        let states = StateStore::new();
        let mut cx = LayoutCx::new(&theme, &states, Size::new(500, 500));
        let node = widget.layout(Constraints::tight(Size::new(500, 500)), &mut cx);
        let semantics = tree::semantics(&widget, &node, &states, &theme, "", "Rich document");
        assert_eq!(semantics.find_role(Role::Document).len(), 1);
        assert_eq!(semantics.find_role(Role::Heading)[0].level, 3);
        let link = &semantics.find_role(Role::Link)[0];
        assert!(link.actions.iter().any(|a| a == "press"));
        assert_eq!(link.value, "https://example.com");
        assert!(link.bounds.width > 0);
        assert_eq!(semantics.find_role(Role::Image)[0].name, "Diagram");
        registry::close(id);
    }
}
