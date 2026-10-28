// Paths travel through the toolkit as UTF-8 strings: they come from and go
// back to the application as text, so every helper answers a `String`.

use std::path::{Component, Path, PathBuf};

/// `rest` under `base`; an absolute `rest` stands alone.
pub fn join(base: &str, rest: &str) -> String {
    Path::new(base).join(rest).to_string_lossy().into_owned()
}

/// The last component, if the path names one.
pub fn file_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// The path without its last component, if it has one.
pub fn parent(path: &str) -> Option<String> {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Whether the path starts at a root.
pub fn is_absolute(path: &str) -> bool {
    Path::new(path).is_absolute()
}

/// The path with `.` removed and each `..` folded into its parent, without
/// touching the file system.
pub fn normalize(path: &str) -> String {
    let mut out = PathBuf::new();
    for c in Path::new(path).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        return ".".to_string();
    }
    out.to_string_lossy().into_owned()
}

/// Whether a directory exists at `path`.
pub fn is_dir(path: &str) -> bool {
    Path::new(path).is_dir()
}

/// Whether a regular file exists at `path`.
pub fn is_file(path: &str) -> bool {
    Path::new(path).is_file()
}

/// Whether anything exists at `path`.
pub fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// The user's home directory.
pub fn home() -> Option<String> {
    std::env::home_dir().map(|p| p.to_string_lossy().into_owned())
}

/// One directory entry, its kind and size read with the listing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub size: i64,
    pub modified_ms: i64,
}

/// The entries of the directory at `path`, in the order the system lists them.
pub fn read_dir(path: &str) -> std::io::Result<Vec<DirEntry>> {
    let mut out = vec![];
    let mut bytes = 0usize;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        // A symlink reports its target's kind and size, as a listing shows it.
        let meta = std::fs::metadata(entry.path()).or_else(|_| entry.metadata())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path().to_string_lossy().into_owned();
        bytes = bytes.saturating_add(name.capacity() + path.capacity() + size_of::<DirEntry>() * 2);
        if bytes > 8 * 1024 * 1024 {
            return Err(std::io::Error::other("directory exceeds its byte budget"));
        }
        out.push(DirEntry {
            name,
            path,
            is_dir: meta.is_dir(),
            is_file: meta.is_file(),
            is_symlink: kind.is_symlink(),
            size: meta.len() as i64,
            modified_ms: modified_ms(&meta),
        });
    }
    Ok(out)
}

/// Milliseconds since the Unix epoch of the last modification; 0 when unknown.
pub fn modified_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_parent_steps_lexically() {
        assert_eq!(normalize("/a/./b/../c"), "/a/c");
        assert_eq!(normalize("a/../.."), "..");
        assert_eq!(normalize("./"), ".");
    }
}
