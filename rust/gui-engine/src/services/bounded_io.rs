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
    if !active() {
        return Err("IO cancelled or expired".into());
    }
    let mut file = open_regular(path)?;
    if file.metadata().map_err(|e| e.to_string())?.len() > limit as u64 {
        return Err("file exceeds provider byte budget".into());
    }
    let mut out = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        if !active() {
            return Err("IO cancelled or expired".into());
        }
        let count = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if out.len() + count > limit {
            return Err("file exceeds provider byte budget".into());
        }
        out.extend_from_slice(&chunk[..count]);
    }
    Ok(out)
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
