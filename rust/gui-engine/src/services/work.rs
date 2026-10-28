//! Revisioned work shared by file IO, source analysis and completion. Every
//! operation uses the session's bounded worker, cancellation token and mailbox.

use super::jobs::Context;
use crate::text::syntax;
use crate::widgets::completion::{self, CompletionItem, MatchMode};
use crate::widgets::cx::UiMsg;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

pub const TEXT_LIMIT: usize = 16 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Operation {
    ImportRich(crate::rich::markup::Format, String),
    ExportRich(i64, crate::rich::markup::Format),
    ReadRichImage(i64, String, String),
    ReadText(String),
    ReadDocument(String),
    SaveSession(
        String,
        Vec<crate::editing::documents::persistence::Record>,
        String,
    ),
    RestoreSession(String),
    SearchDocument(i64, String, crate::editing::search::SearchOptions, i64),
    ReplaceDocument(i64, Vec<crate::text::offsets::ByteRange>, String),
    WriteDocument(i64, String),
    AnalyzeDocument(i64, String),
    CompleteDocument(
        i64,
        std::sync::Arc<[CompletionItem]>,
        String,
        MatchMode,
        i64,
    ),
    WriteText(String, String),
    Analyze(String, String),
    Complete(std::sync::Arc<[CompletionItem]>, String, MatchMode, i64),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Work {
    /// Live widget, document or application owner. Removing it cancels its work.
    pub owner: String,
    pub request: i64,
    pub revision: i64,
    pub timeout_ms: i64,
    pub operation: Operation,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Outcome {
    RichImported(crate::rich::registry::Record),
    RichExported(i64, String),
    RichImageLoaded(i64, String),
    Text(String, String),
    DocumentLoaded(crate::editing::documents::persistence::Record, String),
    Saved(String, String),
    DocumentSaved(i64, String),
    Analyzed(i64, i64),
    Search(Vec<crate::text::offsets::ByteRange>),
    Replaced(i64, i64, i64),
    SessionSaved(String),
    SessionRestored(Vec<crate::editing::documents::persistence::Record>, String),
    Analysis(std::sync::Arc<syntax::Document>),
    Completions(Vec<CompletionItem>),
    Progress(i64, i64),
    Failed(String),
    Cancelled,
}

impl Work {
    pub fn message(&self, outcome: Outcome) -> UiMsg {
        UiMsg::Worked(self.owner.clone(), self.request, self.revision, outcome)
    }
    pub fn kind(&self) -> u64 {
        match self.operation {
            Operation::ImportRich(..) => 30,
            Operation::ExportRich(..) => 31,
            Operation::ReadRichImage(..) => 32,
            Operation::ReadText(_) | Operation::ReadDocument(_) => 10,
            Operation::SaveSession(..) => 16,
            Operation::RestoreSession(..) => 17,
            Operation::SearchDocument(..) => 14,
            Operation::ReplaceDocument(..) => 15,
            Operation::WriteDocument(..) => 11,
            Operation::AnalyzeDocument(..) => 12,
            Operation::CompleteDocument(..) => 13,
            Operation::WriteText(..) => 11,
            Operation::Analyze(..) => 12,
            Operation::Complete(..) => 13,
        }
    }
    pub fn answer(&self, context: &Context, progress: &mut impl FnMut(i64, i64)) -> UiMsg {
        self.answer_with_previous(context, None, progress)
    }

    pub(crate) fn answer_with_previous(
        &self,
        context: &Context,
        previous: Option<&syntax::Document>,
        progress: &mut impl FnMut(i64, i64),
    ) -> UiMsg {
        let result = match &self.operation {
            Operation::ImportRich(format, source) => {
                progress(0, source.len() as i64);
                crate::rich::registry::import(
                    format.clone(),
                    source,
                    context.budget.clone(),
                    || context.active(),
                )
                .map(|id| Outcome::RichImported(crate::rich::registry::Record::pending(id)))
            }
            Operation::ExportRich(id, format) => {
                progress(0, 1);
                crate::rich::registry::export(*id, self.revision, format.clone(), || {
                    context.active()
                })
                .map(|text| Outcome::RichExported(*id, text))
            }
            Operation::ReadRichImage(id, source, path) => crate::services::bounded_io::read(
                std::path::Path::new(path),
                32 * 1024 * 1024,
                &|| context.active(),
            )
            .and_then(|bytes| {
                crate::rich::registry::set_image(*id, source, &bytes, || context.active())
            })
            .map(|()| Outcome::RichImageLoaded(*id, source.clone())),
            Operation::SaveSession(path, records, metadata) => {
                crate::editing::documents::persistence::save(
                    path, records, metadata, context, progress,
                )
                .map(|()| Outcome::SessionSaved(path.clone()))
            }
            Operation::RestoreSession(path) => {
                crate::editing::documents::persistence::restore(path, context, progress)
                    .map(|(records, metadata)| Outcome::SessionRestored(records, metadata))
            }
            Operation::SearchDocument(id, query, options, limit) => {
                crate::editing::documents::snapshot(*id).and_then(|(text, revision)| {
                    if revision != self.revision {
                        return Ok(Outcome::Cancelled);
                    }
                    crate::editing::search::find_all_cancellable(
                        &text,
                        query,
                        *options,
                        *limit,
                        || context.active(),
                    )
                    .map(Outcome::Search)
                })
            }
            Operation::ReplaceDocument(id, ranges, replacement) => {
                crate::editing::documents::replace_many(
                    *id,
                    self.revision,
                    ranges,
                    replacement,
                    || context.active(),
                )
                .map(|info| Outcome::Replaced(*id, info.revision, ranges.len() as i64))
            }
            Operation::WriteDocument(id, path) => crate::editing::documents::snapshot(*id)
                .and_then(|(text, _)| {
                    write_text(path, &text, context, progress)
                        .map(|()| Outcome::DocumentSaved(*id, path.clone()))
                }),
            Operation::AnalyzeDocument(id, language) => crate::editing::documents::snapshot(*id)
                .and_then(|(text, revision)| {
                    if revision != self.revision {
                        return Ok(Outcome::Cancelled);
                    }
                    let previous = crate::editing::documents::analysis(*id);
                    let analysis = crate::text::document_syntax::update(
                        text,
                        revision,
                        language,
                        previous.as_deref(),
                        context,
                        progress,
                    )?;
                    let scanned = analysis.scanned;
                    context.check()?;
                    if let Err(why) =
                        crate::editing::documents::set_analysis(*id, revision, analysis)
                    {
                        if crate::editing::documents::info(*id)
                            .map_or(true, |info| info.revision != revision)
                        {
                            return Ok(Outcome::Cancelled);
                        }
                        return Err(why);
                    }
                    Ok(Outcome::Analyzed(*id, scanned))
                }),
            Operation::CompleteDocument(id, base, query, mode, limit) => (|| {
                let mut best = completion::rank_cancellable(
                    base,
                    query,
                    *mode,
                    *limit,
                    || context.active(),
                    &mut |_, _| {},
                )?;
                if let Some(analysis) = crate::editing::documents::analysis(*id) {
                    for (index, chunk) in analysis.words.chunks(512).enumerate() {
                        context.check()?;
                        let mut candidates = std::mem::take(&mut best);
                        for word in chunk {
                            if word.len() <= 4096 && !base.iter().any(|item| item.key == *word) {
                                candidates.push(CompletionItem::new(word, word));
                            }
                        }
                        best = completion::rank_cancellable(
                            &candidates,
                            query,
                            *mode,
                            *limit,
                            || context.active(),
                            &mut |_, _| {},
                        )?;
                        progress(
                            ((index + 1) * 512).min(analysis.words.len()) as i64,
                            analysis.words.len() as i64,
                        );
                    }
                }
                Ok(Outcome::Completions(best))
            })(),
            Operation::ReadDocument(path) => read_text(path, context, progress).and_then(|text| {
                context.check()?;
                let id = crate::editing::documents::create(&text, context.budget.clone())?;
                Ok(Outcome::DocumentLoaded(
                    crate::editing::documents::persistence::Record::temporary(
                        self.owner.clone(),
                        id,
                    ),
                    path.clone(),
                ))
            }),
            Operation::ReadText(path) => {
                read_text(path, context, progress).map(|text| Outcome::Text(path.clone(), text))
            }
            Operation::WriteText(path, text) => write_text(path, text, context, progress)
                .map(|()| Outcome::Saved(path.clone(), text.clone())),
            Operation::Analyze(text, language) => {
                let mut document = previous
                    .filter(|document| document.language == *language)
                    .cloned()
                    .unwrap_or_else(|| syntax::Document::empty(language));
                document
                    .update_cancellable(text, || context.active(), progress)
                    .map(|()| Outcome::Analysis(std::sync::Arc::new(document)))
            }
            Operation::Complete(items, query, mode, limit) => completion::rank_cancellable(
                items,
                query,
                *mode,
                *limit,
                || context.active(),
                progress,
            )
            .map(Outcome::Completions),
        };
        self.message(match result {
            Ok(value) => value,
            Err(why) if context.cancelled() => {
                let _ = why;
                Outcome::Cancelled
            }
            Err(why) => Outcome::Failed(why),
        })
    }
}

fn read_text(
    path: &str,
    context: &Context,
    progress: &mut impl FnMut(i64, i64),
) -> Result<String, String> {
    context.check()?;
    let mut file = super::bounded_io::open_regular(std::path::Path::new(path))?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size > TEXT_LIMIT as u64 {
        return Err("file exceeds the text byte budget".into());
    }
    let mut bytes = Vec::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        context.check()?;
        let count = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > TEXT_LIMIT {
            return Err("file exceeds the text byte budget".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        progress(bytes.len() as i64, size as i64);
    }
    context.check()?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn write_text(
    path: &str,
    text: &str,
    context: &Context,
    progress: &mut impl FnMut(i64, i64),
) -> Result<(), String> {
    if text.len() > TEXT_LIMIT {
        return Err("file exceeds the text byte budget".into());
    }
    write_bytes(path, text.as_bytes(), context, progress)
}
pub(crate) fn write_bytes(
    path: &str,
    bytes: &[u8],
    context: &Context,
    progress: &mut impl FnMut(i64, i64),
) -> Result<(), String> {
    context.check()?;
    let path = std::path::Path::new(path);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(
        ".gui-write-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let cleanup = scopeguard::guard(temp, |temp| {
        let _ = std::fs::remove_file(temp);
    });
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&*cleanup)
        .map_err(|e| e.to_string())?;
    let mut done = 0;
    for chunk in bytes.chunks(64 * 1024) {
        context.check()?;
        file.write_all(chunk).map_err(|e| e.to_string())?;
        done += chunk.len();
        progress(done as i64, bytes.len() as i64);
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    context.check()?;
    std::fs::rename(&*cleanup, path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn work(operation: Operation) -> Work {
        Work {
            owner: "document".into(),
            request: 1,
            revision: 7,
            timeout_ms: 1000,
            operation,
        }
    }
    #[test]
    fn superseded_native_analysis_and_search_cancel_without_user_errors() {
        let id = crate::editing::documents::create(
            "let a = 1",
            crate::memory::Budget::new(16 * 1024 * 1024),
        )
        .unwrap();
        let _cleanup = scopeguard::guard((), |_| crate::editing::documents::close(id));
        crate::editing::documents::replace(id, 0, 0, 0, "new ").unwrap();
        for operation in [
            Operation::AnalyzeDocument(id, "gos".into()),
            Operation::SearchDocument(
                id,
                "a".into(),
                crate::editing::search::SearchOptions::default(),
                10,
            ),
        ] {
            let mut request = work(operation);
            request.revision = 0;
            let context = Context::standalone(Duration::from_secs(5));
            assert!(matches!(
                request.answer(&context, &mut |_, _| {}),
                UiMsg::Worked(_, _, _, Outcome::Cancelled)
            ));
        }
    }
    #[test]
    fn analysis_superseded_during_progress_cannot_publish_or_report_failure() {
        let id = crate::editing::documents::create(
            "let a = 1\nlet b = 2",
            crate::memory::Budget::new(16 * 1024 * 1024),
        )
        .unwrap();
        let _cleanup = scopeguard::guard((), |_| crate::editing::documents::close(id));
        let mut request = work(Operation::AnalyzeDocument(id, "gos".into()));
        request.revision = 0;
        let context = Context::standalone(Duration::from_secs(5));
        let mut changed = false;
        let outcome = request.answer(&context, &mut |_, _| {
            if !changed {
                changed = true;
                crate::editing::documents::replace(id, 0, 0, 0, "new ").unwrap();
            }
        });
        assert!(changed);
        assert!(matches!(
            outcome,
            UiMsg::Worked(_, _, _, Outcome::Cancelled)
        ));
        assert_eq!(crate::editing::documents::info(id).unwrap().revision, 1);
    }

    #[test]
    fn cancelled_atomic_save_preserves_the_old_file_and_removes_temporary_storage() {
        let folder = std::env::temp_dir().join(format!("gui-cancel-save-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let _cleanup = scopeguard::guard((), |_| {
            let _ = std::fs::remove_dir_all(&folder);
        });
        let file = folder.join("document");
        std::fs::write(&file, b"old contents").unwrap();
        let context = Context::standalone(Duration::from_secs(5));
        let result = work(Operation::WriteText(
            file.to_string_lossy().into_owned(),
            "x".repeat(200_000),
        ))
        .answer(&context, &mut |_, _| context.cancel());
        assert!(matches!(result, UiMsg::Worked(_, 1, 7, Outcome::Cancelled)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old contents");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
    }
    #[test]
    fn file_reads_are_utf8_checked_and_bounded_before_allocation() {
        let path = std::env::temp_dir().join(format!("gui-bounded-read-{}", std::process::id()));
        let _cleanup = scopeguard::guard((), |_| {
            let _ = std::fs::remove_file(&path);
        });
        std::fs::write(&path, "hello אבג 👩🏽‍💻").unwrap();
        let context = Context::standalone(Duration::from_secs(5));
        let request = work(Operation::ReadText(path.to_string_lossy().into_owned()));
        assert!(
            matches!(request.answer(&context,&mut |_,_|{}),UiMsg::Worked(_,_,_,Outcome::Text(_,text)) if text == "hello אבג 👩🏽‍💻")
        );
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(TEXT_LIMIT as u64 + 1)
            .unwrap();
        assert!(
            matches!(request.answer(&context,&mut |_,_|{}),UiMsg::Worked(_,_,_,Outcome::Failed(error)) if error.contains("byte budget"))
        );
    }
    #[test]
    fn analysis_and_completion_check_cancellation_during_provider_work() {
        let context = Context::standalone(Duration::from_secs(5));
        let request = work(Operation::Analyze(
            "let first = 1\nlet second = 2\nlet third = 3".into(),
            "gos".into(),
        ));
        let mut progress = 0;
        let result = request.answer(&context, &mut |_, _| {
            progress += 1;
            context.cancel();
        });
        assert!(progress > 0);
        assert!(matches!(result, UiMsg::Worked(_, _, _, Outcome::Cancelled)));
        let context = Context::standalone(Duration::from_secs(5));
        let items: std::sync::Arc<[CompletionItem]> = (0..1024)
            .map(|i| CompletionItem::new(&i.to_string(), &format!("item {i}")))
            .collect::<Vec<_>>()
            .into();
        let result = work(Operation::Complete(
            items,
            "item".into(),
            MatchMode::Prefix,
            20,
        ))
        .answer(&context, &mut |_, _| context.cancel());
        assert!(matches!(result, UiMsg::Worked(_, _, _, Outcome::Cancelled)));
    }
}
