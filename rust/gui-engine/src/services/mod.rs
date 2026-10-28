pub mod apps;
pub(crate) mod bounded_io;
pub mod clipboard;
pub mod desktop;
pub mod jobs;
pub mod listing;
pub mod palette;
pub mod places;
pub mod work;

// services: desktop services behind toolkit-level calls: per-application
// storage directories, versioned settings with atomic writes, window
// placement memory, system appearance, and opening URLs or revealing
// files. Each call reports failure as a value; none blocks the UI thread
// on anything slower than a local file.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;

use crate::errors;

use crate::paths;
use windowing;

/// The directory an application keeps its settings in, created on demand:
/// `$XDG_CONFIG_HOME/<app>` (default `~/.config/<app>`) on Linux,
/// `~/Library/Application Support/<app>` on macOS, `%APPDATA%\<app>` on
/// Windows.
pub fn config_dir(app_id: &str) -> String {
    let base = if CONFIG_ROOT.lock().is_empty() {
        config_base()
    } else {
        CONFIG_ROOT.lock().to_string()
    };
    paths::join(&base, &sanitize(app_id))
}

// Replaces the platform default when set.
static CONFIG_ROOT: Mutex<String> = Mutex::new(String::new());

/// Keeps every application's settings and caches under `root` instead of the
/// platform defaults, for tests and portable installs; "" restores them.
pub fn set_config_root(root: &str) {
    *CONFIG_ROOT.lock() = root.to_string();
}

/// Points this test process's settings at a new empty directory, so a test
/// neither reads nor writes the user's configuration; answers the directory.
#[cfg(test)]
pub fn private_test_config() -> String {
    let root = paths::join(
        &std::env::temp_dir().to_string_lossy(),
        &format!("gui-gos-test-config-{}", std::process::id()),
    );
    let _ = fs::remove_dir_all(&root);
    set_config_root(&root);
    root
}

/// Removes a directory made by `private_test_config`.
#[cfg(test)]
pub fn remove_test_config(root: &str) {
    set_config_root("");
    let _ = fs::remove_dir_all(root);
}

#[cfg(target_os = "linux")]
fn config_base() -> String {
    let xdg = std::env::var("XDG_CONFIG_HOME").unwrap_or("".to_string());
    if !xdg.is_empty() {
        return xdg;
    };
    paths::join(
        &paths::home().unwrap_or_else(|| "/tmp".to_string()),
        ".config",
    )
}

#[cfg(target_os = "macos")]
fn config_base() -> String {
    paths::join(
        &paths::home().unwrap_or_else(|| "/tmp".to_string()),
        "Library/Application Support",
    )
}

#[cfg(target_os = "windows")]
fn config_base() -> String {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    if !appdata.is_empty() {
        return appdata;
    };
    paths::home().unwrap_or_else(|| "C:\\".to_string())
}

/// The directory an application keeps rebuildable data in, which callers
/// create: `$XDG_CACHE_HOME/<app>` (default `~/.cache/<app>`) on Linux,
/// `~/Library/Caches/<app>` on macOS, `%LOCALAPPDATA%\<app>` on Windows; under
/// `<root>/cache` when `set_config_root` gave a root.
pub fn cache_dir(app_id: &str) -> String {
    let base = if CONFIG_ROOT.lock().is_empty() {
        cache_base()
    } else {
        paths::join(&CONFIG_ROOT.lock(), "cache")
    };
    paths::join(&base, &sanitize(app_id))
}

#[cfg(target_os = "linux")]
fn cache_base() -> String {
    let xdg = std::env::var("XDG_CACHE_HOME").unwrap_or("".to_string());
    if !xdg.is_empty() {
        return xdg;
    };
    paths::join(
        &paths::home().unwrap_or_else(|| "/tmp".to_string()),
        ".cache",
    )
}

#[cfg(target_os = "macos")]
fn cache_base() -> String {
    paths::join(
        &paths::home().unwrap_or_else(|| "/tmp".to_string()),
        "Library/Caches",
    )
}

#[cfg(target_os = "windows")]
fn cache_base() -> String {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    if !local.is_empty() {
        return local;
    };
    config_base()
}

// App ids become one directory name.
fn sanitize(app_id: &str) -> String {
    let mut out = String::new();
    for c in app_id.chars() {
        if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
            out.push(c)
        } else {
            out.push('_')
        }
    }
    if out.is_empty() {
        "gui-gos-app".to_string()
    } else {
        out
    }
}

/// The JSON text of a toolkit document.
pub fn to_json(value: &impl serde::Serialize) -> String {
    // serde_json fails only on non-string map keys or a failing `Serialize`
    // impl; toolkit documents are derived structs with string-keyed maps.
    serde_json::to_string(value).expect("toolkit documents encode as JSON")
}

pub(crate) fn bounded_json(
    value: &impl serde::Serialize,
    limit: usize,
) -> Result<Vec<u8>, errors::Error> {
    struct Writer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other(
                    "document JSON exceeds its byte budget",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}

/// Writes `bytes` to `file` atomically: a sibling temporary file is
/// written and synced, then renamed over the destination, so a crash
/// leaves either the old or the new contents.
pub fn write_atomic(file: &str, bytes: &[u8]) -> Result<(), errors::Error> {
    let dir = paths::parent(file).unwrap_or_default();
    if !dir.is_empty() {
        fs::create_dir_all(&dir)?;
    }
    let tmp = format!("{}.tmp-{}", file, std::process::id());
    if let Err(e) = stage(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(errors::wrap(e, format!("writing {tmp}")));
    }
    if let Err(e) = fs::rename(&tmp, file) {
        let _ = fs::remove_file(&tmp);
        return Err(errors::wrap(e, format!("replacing {file}")));
    }
    sync_dir(if dir.is_empty() { "." } else { &dir })
        .map_err(|e| errors::wrap(e, format!("syncing the directory of {file}")))
}

// Writes all of `bytes` to a new `file` and flushes it to storage.
fn stage(file: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = fs::File::create(file)?;
    f.write_all(bytes)?;
    f.sync_all()
}

// Flushes a rename in `dir` to storage.
#[cfg(unix)]
fn sync_dir(dir: &str) -> std::io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

// Windows commits a rename with the file system's metadata and opens no
// directory handle for syncing.
#[cfg(not(unix))]
fn sync_dir(_dir: &str) -> std::io::Result<()> {
    Ok(())
}

/// A versioned settings document: a schema version and string values.
/// Loading migrates older versions through `migrate`; an unreadable file
/// is reported, never silently replaced.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct Settings {
    pub version: i64,
    pub values: HashMap<String, String>,
}

impl Settings {
    pub fn get(&self, key: &str, default: &str) -> String {
        match self.values.get(key) {
            Some(v) => v.to_string(),
            None => default.to_string(),
        }
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.values.insert(key.to_string(), value.to_string());
    }

    pub fn remove(&mut self, key: &str) {
        let _ = self.values.remove(key);
    }

    pub fn get_list(&self, key: &str) -> Vec<String> {
        match self.values.get(key) {
            Some(text) => match serde_json::from_str::<StringList>(text) {
                Ok(v) => v.items,
                Err(_) => vec![],
            },
            None => vec![],
        }
    }

    pub fn set_list(&mut self, key: &str, items: &[String]) {
        self.values.insert(
            key.to_string(),
            crate::services::to_json(&StringList {
                items: items.to_vec(),
            }),
        );
    }
}

/// A list value stored inside one setting.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct StringList {
    pub items: Vec<String>,
}

/// Where `name` settings live for `app_id`.
pub fn settings_path(app_id: &str, name: &str) -> String {
    paths::join(&config_dir(app_id), &format!("{}.json", name))
}

/// Loads settings; a missing file answers empty settings at `version`.
/// Older files pass through `migrate(old_version, settings)`.
pub fn load_settings(
    file: &str,
    version: i64,
    migrate: impl Fn(i64, Settings) -> Settings,
) -> Result<Settings, errors::Error> {
    let text = match bounded_io::read_text(file, 1024 * 1024) {
        Ok(t) => t,
        Err(why) if std::path::Path::new(file).exists() => return Err(errors::new(why)),
        Err(_) => {
            return Ok(Settings {
                version,
                values: HashMap::new(),
            });
        }
    };
    let parsed = serde_json::from_str::<Settings>(&text)
        .map_err(|e| errors::wrap(e, format!("reading settings {}", file)))?;
    if parsed.version > version {
        return Err(errors::new(format!(
            "settings {} were written by a newer version ({} > {})",
            file, parsed.version, version
        )));
    }
    if parsed.version < version {
        let mut migrated = migrate(parsed.version, parsed);
        migrated.version = version;
        return Ok(migrated);
    }
    Ok(parsed)
}

pub fn save_settings(file: &str, settings: &Settings) -> Result<(), errors::Error> {
    write_atomic(file, &bounded_json(settings, 1024 * 1024)?)
}

/// The placement to restore a window to: position in native logical units,
/// size in interface units (pixels at scale 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct WindowPlacement {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub maximized: bool,
}

/// The placement saved for one window of `app_id`, if any.
pub fn load_window_placement(app_id: &str, window: &str) -> Option<WindowPlacement> {
    let file = paths::join(
        &config_dir(app_id),
        &format!("window-{}.json", sanitize(window)),
    );
    let text = bounded_io::read_text(file, 64 * 1024).ok()?;
    let p = serde_json::from_str::<WindowPlacement>(&text).ok()?;
    if p.width <= 0 || p.height <= 0 {
        return None;
    }
    Some(p)
}

pub fn save_window_placement(
    app_id: &str,
    window: &str,
    placement: WindowPlacement,
) -> Result<(), errors::Error> {
    let file = paths::join(
        &config_dir(app_id),
        &format!("window-{}.json", sanitize(window)),
    );
    write_atomic(&file, crate::services::to_json(&placement).as_bytes())
}

/// The desktop's contrast, motion, accent and text-size preferences; each is
/// `None` where the platform does not say.
pub fn system_preferences() -> windowing::system::Preferences {
    windowing::system::preferences()
}

/// The desktop's light/dark preference, when it states one.
pub fn system_prefers_dark() -> Option<bool> {
    match windowing::system::theme() {
        windowing::system::Theme::Dark => Some(true),
        windowing::system::Theme::Light => Some(false),
        _ => None,
    }
}

/// Opens `url` in the user's default handler. Only http, https, mailto,
/// and file URLs are passed on; anything else is refused, so text typed
/// by a user can never become a command.
pub fn open_url(url: &str) -> Result<(), errors::Error> {
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("file://"))
    {
        return Err(errors::new(format!(
            "refusing to open {}: only http, https, mailto, and file URLs are opened",
            url
        )));
    }
    Ok(windowing::system::open_url(url)?)
}

/// Shows `file` in the platform file manager.
pub fn reveal(file: &str) -> Result<(), errors::Error> {
    Ok(windowing::system::reveal(file)?)
}

/// The user's preferred locales, most preferred first (`en-US` style).
pub fn locales() -> Vec<String> {
    match windowing::system::locales() {
        Ok(list) => list
            .iter()
            .map(|l| match l.country {
                Some(ref c) => format!("{}-{}", l.language, c),
                None => l.language.clone(),
            })
            .collect::<Vec<_>>(),
        Err(_) => vec!["en-US".to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::{Settings, load_settings, save_settings, write_atomic};
    use crate::paths;
    use std::collections::HashMap;
    use std::fs;
    use std::process;

    #[test]
    fn settings_round_trip_and_migrate() {
        let dir = paths::join(
            &std::env::temp_dir().to_string_lossy(),
            &format!("gui-settings-{}", process::id()),
        );
        let file = paths::join(&dir, "s.json");
        let mut s = Settings {
            version: 1,
            values: HashMap::new(),
        };
        s.set("theme", "dark");
        s.set_list("recent", &["a".to_string(), "b".to_string()]);
        save_settings(&file, &s).unwrap();
        let back = load_settings(&file, 1, |_v, x| x).unwrap();
        assert_eq!(back.get("theme", ""), "dark");
        assert_eq!(back.get_list("recent"), vec!["a", "b"]);
        let migrated = load_settings(&file, 2, |v, mut x| {
            x.set("from", &format!("{}", v));
            x
        })
        .unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(migrated.get("from", ""), "1");
        assert!(load_settings(&file, 0, |_v, x| x).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn oversized_settings_save_preserves_the_previous_readable_document() {
        let file = std::env::temp_dir().join(format!("gui-settings-limit-{}", std::process::id()));
        let _cleanup = scopeguard::guard((), |_| {
            let _ = fs::remove_file(&file);
        });
        fs::write(&file, b"old contents").unwrap();
        let mut settings = super::Settings::default();
        settings
            .values
            .insert("large".into(), "x".repeat(1024 * 1024));
        assert!(
            super::save_settings(&file.to_string_lossy(), &settings)
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
        assert_eq!(fs::read(&file).unwrap(), b"old contents");
    }

    #[test]
    fn atomic_write_replaces_whole_contents() {
        let file = paths::join(
            &std::env::temp_dir().to_string_lossy(),
            &format!("gui-atomic-{}.txt", process::id()),
        );
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(fs::read_to_string(file.clone()).unwrap(), "two");
        let _ = fs::remove_file(file);
    }

    #[test]
    fn urls_outside_the_allowed_schemes_are_refused() {
        assert!(super::open_url("javascript:alert(1)").is_err());
        assert!(super::open_url("rm -rf /").is_err())
    }
}
