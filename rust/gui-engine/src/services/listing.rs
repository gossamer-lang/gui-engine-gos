// listing: directory listings that never hold up the UI. `Effect::ListDirectory`
// queues a request that the toolkit's job worker (`services::jobs`) answers
// as `UiMsg::Listed`. A listing is bounded: however large the folder, at most
// `limit` entries come back, with the total that matched.

use crate::strext::StrExt;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs;

use crate::paths;

/// The most entries a listing answers unless its request says otherwise.
pub const DEFAULT_LIMIT: i64 = 10_000;

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DirEntry {
    pub path: String,
    pub name: String,
    pub directory: bool,
    /// Bytes, for files.
    pub size: i64,
    /// When it last changed, in milliseconds since the Unix epoch; 0 when unknown.
    pub modified_ms: i64,
}

/// The order of a listing; folders always come first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortBy {
    /// Case-insensitive name, ties by exact name.
    Name,
    Size,
    /// File name extension, then name.
    Kind,
    /// Last change, oldest first.
    Modified,
}

/// A request for one folder's entries, answered by `UiMsg::Listed(owner,
/// request, listing)`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListRequest {
    /// The widget or dialog the answer is for.
    pub owner: String,
    /// Distinguishes this request from the owner's earlier ones, so a late
    /// answer to a superseded request can be dropped.
    pub request: i64,
    pub directory: String,
    /// Include names starting with a dot.
    pub hidden: bool,
    /// Only folders, for folder pickers.
    pub folders_only: bool,
    /// Glob patterns files must match (`*.txt`, `*`); empty keeps every file.
    pub patterns: Vec<String>,
    /// Only entries whose name starts with this text (case-insensitive).
    pub prefix: String,
    pub sort: SortBy,
    pub ascending: bool,
    pub limit: i64,
}

impl ListRequest {
    /// Every entry of `directory`, by name, up to `DEFAULT_LIMIT`.
    pub fn new(owner: &str, request: i64, directory: &str) -> ListRequest {
        ListRequest {
            owner: owner.to_string(),
            request,
            directory: directory.to_string(),
            hidden: false,
            folders_only: false,
            patterns: vec![],
            prefix: String::new(),
            sort: SortBy::Name,
            ascending: true,
            limit: DEFAULT_LIMIT,
        }
    }
}

/// What a listing found.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Listing {
    /// The folder read, made absolute and canonical.
    pub directory: String,
    /// At most the request's `limit` entries, in order.
    pub entries: Vec<DirEntry>,
    /// How many entries matched before the limit applied.
    pub total: i64,
    /// Why the folder could not be read; "" on success.
    pub error: String,
}

/// Whether `name` matches a glob `pattern` of `*` and `?` (case-insensitive).
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    // Classic backtracking over the last `*`.
    let mut pi = 0;
    let mut ni = 0;
    let mut star: i64 = -1;
    let mut mark = 0;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi as i64;
            mark = ni;
            pi += 1;
        } else if star >= 0 {
            pi = (star + 1) as usize;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1
    }
    pi == p.len()
}

fn extension(name: &str) -> String {
    match name.rfind(".") {
        Some(at) if at > 0 => name
            .substring((at + 1) as i64, name.byte_len())
            .to_lowercase(),
        _ => String::new(),
    }
}

// The order `sort` puts `a` and `b` in, ascending: -1, 0 or 1.
fn compare(a: &DirEntry, b: &DirEntry, sort: SortBy) -> i64 {
    let by_name = {
        let left = a.name.to_lowercase();
        let right = b.name.to_lowercase();
        if left < right {
            -1
        } else if left > right {
            1
        } else if a.name < b.name {
            -1
        } else if a.name > b.name {
            1
        } else {
            0
        }
    };
    match sort {
        SortBy::Name => by_name,
        SortBy::Size => {
            if a.size < b.size {
                -1
            } else if a.size > b.size {
                1
            } else {
                by_name
            }
        }
        SortBy::Modified => {
            if a.modified_ms < b.modified_ms {
                -1
            } else if a.modified_ms > b.modified_ms {
                1
            } else {
                by_name
            }
        }
        SortBy::Kind => {
            let left = extension(&a.name);
            let right = extension(&b.name);
            if left < right {
                -1
            } else if left > right {
                1
            } else {
                by_name
            }
        }
    }
}

/// Reads `req.directory` now: filtered, folders first, sorted, and cut to
/// `req.limit` entries.
pub fn list(req: &ListRequest) -> Listing {
    list_cancellable(req, || true)
}

/// Storage cap independent of the requested entry count. Requests cannot
/// disable either limit. The returned error reports byte truncation explicitly.
pub const LIST_BYTE_LIMIT: usize = 1024 * 1024;

struct RankedEntry {
    entry: DirEntry,
    sort: SortBy,
    ascending: bool,
    bytes: usize,
}
impl PartialEq for RankedEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for RankedEntry {}
impl PartialOrd for RankedEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RankedEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.entry.directory != other.entry.directory {
            return other.entry.directory.cmp(&self.entry.directory);
        }
        let order = compare(&self.entry, &other.entry, self.sort);
        (if self.ascending { order } else { -order }).cmp(&0)
    }
}

/// Streams entries from the OS, retaining only the best bounded set instead of
/// collecting and sorting the entire directory. Cancellation is checked between
/// entries, including filtered entries; OS metadata calls themselves may block.
pub fn list_cancellable(req: &ListRequest, active: impl Fn() -> bool) -> Listing {
    let mut result = Listing {
        directory: req.directory.clone(),
        entries: vec![],
        total: 0,
        error: String::new(),
    };
    if !active() {
        result.error = "listing cancelled or expired".into();
        return result;
    }
    result.directory = fs::canonicalize(&req.directory)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| paths::normalize(&req.directory));
    let found = match fs::read_dir(&result.directory) {
        Ok(entries) => entries,
        Err(e) => {
            result.error = e.to_string();
            return result;
        }
    };
    let limit = req.limit.clamp(0, DEFAULT_LIMIT) as usize;
    let prefix = req.prefix.to_lowercase();
    let mut kept = BinaryHeap::<RankedEntry>::new();
    let mut bytes = 0;
    for item in found {
        if !active() {
            result.error = "listing cancelled or expired".into();
            return result;
        }
        let e = match item {
            Ok(e) => e,
            Err(e) => {
                result.error = e.to_string();
                return result;
            }
        };
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !req.hidden {
            continue;
        }
        if !prefix.is_empty() && !name.to_lowercase().starts_with(&prefix) {
            continue;
        }
        // Preserve the public listing behavior for links to folders/files.
        let metadata = match fs::metadata(e.path()).or_else(|_| e.metadata()) {
            Ok(m) => m,
            Err(e) => {
                result.error = e.to_string();
                return result;
            }
        };
        let directory = metadata.is_dir();
        if !directory
            && (req.folders_only
                || (!req.patterns.is_empty()
                    && !req.patterns.iter().any(|p| glob_matches(p, &name))))
        {
            continue;
        }
        result.total += 1;
        if limit == 0 {
            continue;
        }
        let entry = DirEntry {
            path: e.path().to_string_lossy().into_owned(),
            name,
            directory,
            size: if directory {
                0
            } else {
                metadata.len().min(i64::MAX as u64) as i64
            },
            modified_ms: paths::modified_ms(&metadata),
        };
        let entry_bytes =
            entry.path.capacity() + entry.name.capacity() + 2 * size_of::<RankedEntry>();
        let ranked = RankedEntry {
            entry,
            sort: req.sort,
            ascending: req.ascending,
            bytes: entry_bytes,
        };
        if kept.len() >= limit && kept.peek().is_some_and(|worst| ranked >= *worst) {
            continue;
        }
        if entry_bytes > LIST_BYTE_LIMIT {
            result.error = "listing byte budget reached".into();
            continue;
        }
        kept.push(ranked);
        bytes += entry_bytes;
        while kept.len() > limit || bytes > LIST_BYTE_LIMIT {
            if bytes > LIST_BYTE_LIMIT {
                result.error = "listing byte budget reached".into();
            }
            bytes -= kept.pop().unwrap().bytes;
        }
    }
    result.entries = kept
        .into_sorted_vec()
        .into_iter()
        .map(|ranked| ranked.entry)
        .collect();
    result
}

#[cfg(test)]
mod tests {
    use super::{ListRequest, SortBy, glob_matches, list};
    use crate::paths;

    use std::fs;

    fn folder(name: &str) -> String {
        let root = paths::join(&crate::services::private_test_config(), name);
        fs::create_dir_all(paths::join(&root, "sub")).unwrap();
        fs::create_dir_all(paths::join(&root, "Alpha")).unwrap();
        for (file, body) in [
            ("b.txt", "12345"),
            ("a.TXT", "1"),
            ("c.md", "123"),
            (".hidden", "x"),
        ] {
            fs::write(paths::join(&root, file), body).unwrap()
        }
        root
    }

    #[test]
    fn globs_match_stars_and_question_marks_without_case() {
        assert!(glob_matches("*.txt", "notes.TXT"));
        assert!(glob_matches("a?c", "abc"));
        assert!(glob_matches("*", ""));
        assert!(glob_matches("*a*b", "xxaYYb"));
        assert!(!glob_matches("*.txt", "notes.md"));
        assert!(!glob_matches("a?c", "ac"))
    }

    #[test]
    fn a_listing_puts_folders_first_filters_sorts_and_bounds() {
        let root = folder("listing-a");
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&paths::parent(&root).unwrap());
        });
        let names = |req: ListRequest| {
            list(&req)
                .entries
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(ListRequest::new("o", 1, &root)),
            vec!["Alpha", "sub", "a.TXT", "b.txt", "c.md"]
        );
        let mut req = ListRequest::new("o", 1, &root);
        req.hidden = true;
        assert!(names(req).contains(&".hidden".to_string()));
        req = ListRequest::new("o", 1, &root);
        req.patterns = vec!["*.txt".to_string()];
        assert_eq!(names(req), vec!["Alpha", "sub", "a.TXT", "b.txt"]);
        req = ListRequest::new("o", 1, &root);
        req.sort = SortBy::Size;
        req.ascending = false;
        assert_eq!(names(req), vec!["sub", "Alpha", "b.txt", "c.md", "a.TXT"]);
        req = ListRequest::new("o", 1, &root);
        req.sort = SortBy::Kind;
        assert_eq!(names(req), vec!["Alpha", "sub", "c.md", "a.TXT", "b.txt"]);
        req = ListRequest::new("o", 1, &root);
        req.folders_only = true;
        req.prefix = "s".to_string();
        assert_eq!(names(req), vec!["sub"]);
        req = ListRequest::new("o", 1, &root);
        req.limit = 2;
        let bounded = list(&req);
        assert_eq!(bounded.entries.len(), 2);
        assert_eq!(bounded.total, 5);
        assert!(
            list(&ListRequest::new("o", 1, &root))
                .entries
                .iter()
                .all(|e| e.modified_ms > 0)
        );
        req = ListRequest::new("o", 1, &root);
        req.sort = SortBy::Modified;
        assert_eq!(list(&req).entries.len(), 5);
        let missing = list(&ListRequest::new("o", 1, &paths::join(&root, "missing")));
        assert!(!missing.error.is_empty())
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn cancellation_and_negative_limits_do_not_retain_entries() {
        let request = ListRequest::new("cancel", 1, ".");
        let calls = Cell::new(0);
        let result = list_cancellable(&request, || {
            calls.set(calls.get() + 1);
            calls.get() < 2
        });
        assert!(result.error.contains("cancelled"));
        assert!(result.entries.is_empty());
        let mut request = request;
        request.limit = -1;
        let result = list(&request);
        assert!(result.error.is_empty());
        assert!(result.entries.is_empty());
        assert!(result.total > 0);
    }
}

#[cfg(all(test, unix))]
mod link_tests {
    use super::*;
    #[test]
    fn streamed_listings_still_follow_links_to_folders_and_files() {
        let root =
            std::env::temp_dir().join(format!("gui-stream-list-links-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("folder")).unwrap();
        fs::write(root.join("file"), b"abc").unwrap();
        std::os::unix::fs::symlink("folder", root.join("folder-link")).unwrap();
        std::os::unix::fs::symlink("file", root.join("file-link")).unwrap();
        let _cleanup = scopeguard::guard((), |_| {
            let _ = fs::remove_dir_all(&root);
        });
        let request = ListRequest::new("links", 1, &root.to_string_lossy());
        let result = list(&request);
        assert!(result.error.is_empty());
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "folder-link" && entry.directory)
        );
        assert!(
            result
                .entries
                .iter()
                .any(|entry| entry.name == "file-link" && !entry.directory && entry.size == 3)
        );
    }
}
