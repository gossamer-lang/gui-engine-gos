//! Typed bounded adapter shared by VM, JIT and native Gossamer execution.
use super::{markup::Format, registry, *};
#[derive(Serialize)]
enum Answer {
    Id(i64),
    Info(registry::Info),
    Block(Arc<Block>),
    Blocks(Vec<Arc<Block>>),
    Text(String),
    Outline(Vec<super::preview::OutlineRow>),
    Done,
}
fn args<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}
fn out(value: Answer) -> Result<String, String> {
    let budget = crate::app::effects::current_budget();
    let bytes = crate::memory::measure(&value);
    if bytes > 128 * 1024 * 1024 {
        return Err("rich API result exceeds its byte budget".into());
    }
    let _value = budget.reserve(bytes.saturating_add(4096))?;
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > 64 * 1024 * 1024 {
                return Err(std::io::Error::other(
                    "rich API result exceeds its wire budget",
                ));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, &value).map_err(|error| error.to_string())?;
    let _output = budget.reserve(counter.0.saturating_mul(2).saturating_add(4096))?;
    serde_json::to_string(&value).map_err(|error| error.to_string())
}

pub fn call(method: &str, id: i64, text: &str) -> Result<String, String> {
    if text.len() > 64 * 1024 * 1024 {
        return Err("rich API request exceeds its byte budget".into());
    }
    let _request = crate::app::effects::current_budget()
        .reserve(text.len().saturating_mul(8).saturating_add(4096))?;
    match method {
        "new" => out(Answer::Id(registry::create(
            Content::empty(),
            crate::app::effects::current_budget(),
        )?)),
        "import" => {
            let (format, source): (Format, String) = args(text)?;
            out(Answer::Id(registry::import(
                format,
                &source,
                crate::app::effects::current_budget(),
                || true,
            )?))
        }
        "create" => {
            let content: Content = args(text)?;
            out(Answer::Id(registry::create(
                content,
                crate::app::effects::current_budget(),
            )?))
        }
        "outline" => {
            let (start, count): (usize, usize) = args(text)?;
            out(Answer::Outline(super::preview::outline(id, start, count)?))
        }
        "wrap" => {
            let kind: Kind = args(text)?;
            out(Answer::Info(super::editor::wrap(id, kind)?))
        }
        "enter" => out(Answer::Info(super::editor::enter(id)?)),
        "indent" => {
            let outdent: bool = args(text)?;
            out(Answer::Info(super::editor::indent(id, outdent)?))
        }
        "info" => out(Answer::Info(registry::info(id)?)),
        "freeze" => out(Answer::Id(registry::freeze(id)?)),
        "close" => {
            registry::close(id);
            out(Answer::Done)
        }
        "block" => {
            let block: i64 = args(text)?;
            let snapshot = registry::snapshot(id)?;
            out(Answer::Block(
                snapshot
                    .content
                    .shared_block(block)
                    .ok_or("rich block does not exist")?,
            ))
        }
        "blocks" => {
            let (start, count): (usize, usize) = args(text)?;
            if count > 256 {
                return Err("rich block page exceeds 256 entries".into());
            }
            let snapshot = registry::snapshot(id)?;
            out(Answer::Blocks(
                snapshot
                    .content
                    .blocks
                    .iter()
                    .skip(start)
                    .take(count)
                    .cloned()
                    .collect(),
            ))
        }
        "plain" => out(Answer::Text(registry::snapshot(id)?.content.plain_text())),
        "select" => {
            let selection: Selection = args(text)?;
            registry::select(id, selection)?;
            out(Answer::Done)
        }
        "transact" => {
            let (revision, edits): (i64, Vec<Edit>) = args(text)?;
            out(Answer::Info(registry::transact(
                id,
                revision,
                &edits,
                || true,
            )?))
        }
        "undo" => {
            let revision: i64 = args(text)?;
            out(Answer::Info(registry::undo(id, revision)?))
        }
        "redo" => {
            let revision: i64 = args(text)?;
            out(Answer::Info(registry::redo(id, revision)?))
        }
        "saved" => {
            let snapshot: i64 = args(text)?;
            registry::mark_saved(id, snapshot)?;
            out(Answer::Done)
        }
        "export" => {
            let (revision, format): (i64, Format) = args(text)?;
            out(Answer::Text(registry::export(
                id,
                revision,
                format,
                || true,
            )?))
        }
        _ => Err("unknown rich document operation".into()),
    }
}
