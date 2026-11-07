//! Streaming IO primitives used by background providers. Files and directory
//! walks have independent byte/depth limits and check cancellation between IOs.
use std::io::Read;

pub(crate) fn open_regular(path: &std::path::Path) -> Result<std::fs::File, String> {
    if !std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("provider requires a regular file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Avoid blocking if a regular path is replaced by a FIFO after stat.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("provider requires a regular file".into());
    }
    Ok(file)
}

pub(crate) fn read_text(path: impl AsRef<std::path::Path>, limit: usize) -> Result<String, String> {
    String::from_utf8(read(path.as_ref(), limit, &|| true)?).map_err(|e| e.to_string())
}

pub(crate) fn read(
    path: &std::path::Path,
    limit: usize,
    active: &dyn Fn() -> bool,
) -> Result<Vec<u8>, String> {
    const CHUNK: usize = 64 * 1024;
    if !active() {
        return Err("IO cancelled or expired".into());
    }
    let mut file = open_regular(path)?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len > limit as u64 {
        return Err("file exceeds provider byte budget".into());
    }
    // One allocation at the size the file reports; a file that grows after the
    // check still reads in full, up to the limit.
    let mut out = Vec::with_capacity(len as usize);
    loop {
        if !active() {
            return Err("IO cancelled or expired".into());
        }
        let want = CHUNK.min((limit - out.len()).saturating_add(1));
        let count = (&mut file)
            .take(want as u64)
            .read_to_end(&mut out)
            .map_err(|e| e.to_string())?;
        if out.len() > limit {
            return Err("file exceeds provider byte budget".into());
        }
        if count < want {
            return Ok(out);
        }
    }
}

/// A read-only mapping of the regular file at `path`, or `None` when some
/// process of this user could shorten the file while it is mapped, which would
/// fault every later read of the lost pages.
pub(crate) fn map_unchangeable(
    path: &std::path::Path,
    limit: usize,
    active: &dyn Fn() -> bool,
) -> Result<Option<memmap2::Mmap>, String> {
    if !active() {
        return Err("IO cancelled or expired".into());
    }
    let file = open_regular(path)?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if metadata.len() > limit as u64 {
        return Err("file exceeds provider byte budget".into());
    }
    if metadata.len() == 0 || !unchangeable(&metadata) {
        return Ok(None);
    }
    // SAFETY: the mapping is read-only and private, and `unchangeable` holds:
    // no process of this user can write or truncate the file, so its length
    // and contents stay fixed while mapped. Replacing it by rename, as package
    // managers do, leaves the mapped pages intact.
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| e.to_string())?;
    Ok(Some(map))
}

// Checked on the open descriptor's metadata, so the file judged is the file
// mapped. The owner can always make a file writable again, and root can write
// any file, so neither maps.
#[cfg(unix)]
fn unchangeable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    metadata.uid() != euid && metadata.mode() & 0o022 == 0
}

// Windows refuses to truncate a file while a view of it is mapped.
#[cfg(windows)]
fn unchangeable(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(not(any(unix, windows)))]
fn unchangeable(_metadata: &std::fs::Metadata) -> bool {
    false
}

pub(crate) fn walk(
    roots: &[String],
    active: &dyn Fn() -> bool,
    mut visit: impl FnMut(&std::fs::DirEntry) -> Result<(), String>,
) -> Result<(), String> {
    if roots.len() > 256 || roots.iter().map(String::len).sum::<usize>() > 128 * 1024 {
        return Err("provider roots exceed their byte budget".into());
    }
    let mut visited = 0usize;
    for root in roots {
        if !active() {
            return Err("catalog cancelled or expired".into());
        }
        let Ok(directory) = std::fs::read_dir(root) else {
            continue;
        };
        let mut stack = vec![directory];
        while let Some(directory) = stack.last_mut() {
            if !active() {
                return Err("catalog cancelled or expired".into());
            }
            let Some(entry) = directory.next() else {
                stack.pop();
                continue;
            };
            let entry = entry.map_err(|e| e.to_string())?;
            visited += 1;
            if visited > 250_000 {
                return Err("catalog walk exceeds its work budget".into());
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            visit(&entry)?;
            // Never recurse through symlinked directories.
            if kind.is_dir() {
                if stack.len() >= 64 {
                    return Err("catalog exceeds its depth budget".into());
                }
                if let Ok(directory) = std::fs::read_dir(entry.path()) {
                    stack.push(directory);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_reads_reject_oversized_files_before_allocating_and_non_files() {
        let path = std::env::temp_dir().join(format!("gui-provider-limit-{}", std::process::id()));
        let _cleanup = scopeguard::guard((), |_| {
            let _ = std::fs::remove_file(&path);
        });
        std::fs::File::create(&path).unwrap().set_len(1025).unwrap();
        assert!(
            super::read(&path, 1024, &|| true)
                .unwrap_err()
                .contains("budget")
        );
        assert!(super::open_regular(&std::env::temp_dir()).is_err());
    }
    #[test]
    fn a_read_allocates_once_at_the_file_size() {
        let path = std::env::temp_dir().join(format!("gui-provider-exact-{}", std::process::id()));
        let _cleanup = scopeguard::guard((), |_| {
            let _ = std::fs::remove_file(&path);
        });
        let data: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let read = super::read(&path, data.len(), &|| true).unwrap();
        assert_eq!(read, data);
        assert_eq!(read.capacity(), data.len());
        assert!(super::read(&path, data.len() - 1, &|| true).is_err());
    }
    #[test]
    fn a_file_this_user_owns_is_not_mapped() {
        let path = std::env::temp_dir().join(format!("gui-provider-owned-{}", std::process::id()));
        let _cleanup = scopeguard::guard((), |_| {
            let _ = std::fs::remove_file(&path);
        });
        std::fs::write(&path, b"font bytes").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let read_only = std::fs::Permissions::from_mode(0o444);
            std::fs::set_permissions(&path, read_only).unwrap();
        }
        let mapped = super::map_unchangeable(&path, 1024, &|| true).unwrap();
        // Windows cannot truncate a mapped file, so every file maps there.
        assert_eq!(mapped.is_some(), cfg!(windows));
        if let Some(map) = mapped {
            assert_eq!(&map[..], b"font bytes");
        }
    }
    #[cfg(unix)]
    #[test]
    fn a_file_only_another_user_can_write_is_mapped_unless_this_is_root() {
        let path = std::path::Path::new("/etc/passwd");
        let mapped = super::map_unchangeable(path, 16 * 1024 * 1024, &|| true).unwrap();
        // SAFETY: geteuid has no preconditions and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        assert_eq!(mapped.is_some(), !root);
        if let Some(map) = mapped {
            assert_eq!(&map[..], &std::fs::read(path).unwrap()[..]);
        }
    }
    #[cfg(unix)]
    #[test]
    fn a_fifo_cannot_block_provider_reads_or_shutdown() {
        let path = std::env::temp_dir().join(format!("gui-provider-fifo-{}", std::process::id()));
        let _cleanup = scopeguard::guard((), |_| {
            let _ = std::fs::remove_file(&path);
        });
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let began = std::time::Instant::now();
        assert!(
            super::read(&path, 1024, &|| true)
                .unwrap_err()
                .contains("regular file")
        );
        assert!(began.elapsed() < std::time::Duration::from_secs(1));
    }
}
