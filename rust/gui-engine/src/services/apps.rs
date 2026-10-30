// apps: the applications installed on the machine, for launchers. Linux
// reads freedesktop desktop entries from the XDG data folders, macOS the
// bundles in the Applications folders, Windows the Start menu's shortcuts.
// `Effect::LoadApps` builds the catalog and `Effect::LaunchApp` starts one
// off the UI thread; neither runs a shell.

use crate::strext::StrExt;
use std::collections::{HashMap, HashSet};
use std::env;

use crate::errors;

use crate::paths;
use std::process::Command;

use crate::render::svg;
use crate::services;

/// One application a launcher can start.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppEntry {
    /// Unique within the catalog: the desktop file id, the bundle path, or
    /// the shortcut path.
    pub id: String,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    /// An icon name or file.
    pub icon: String,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    /// The program and its arguments, still holding the entry's field codes
    /// (`%f`, `%U`, ...) that `command_for` fills.
    pub exec: Vec<String>,
    /// Runs inside a terminal emulator.
    pub terminal: bool,
    /// The file the entry came from.
    pub source: String,
    /// The icon's pixels when `with_icons` found a PNG or SVG for it.
    pub image: Option<AppIcon>,
}

/// An application icon's RGBA pixels.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppIcon {
    pub rgba: Vec<u8>,
    pub width: i64,
    pub height: i64,
}

impl AppIcon {
    /// The icon encoded as a PNG file's bytes, transparency kept.
    pub fn to_png(&self) -> Vec<u8> {
        crate::render::to_png(&crate::render::Canvas {
            pixels: self.rgba.clone(),
            width: self.width,
            height: self.height,
            clip: crate::geometry::Rect::new(0, 0, self.width, self.height),
            opacity: 255,
            ..crate::render::Canvas::new(0, 0)
        })
    }

    /// A `size` pixel square icon: a rounded tile of `background` with the
    /// built-in `glyph` drawn on it in `foreground`, transparent outside.
    pub fn drawn(
        glyph: &crate::render::icons::Icon,
        size: i64,
        background: crate::style::color::Color,
        foreground: crate::style::color::Color,
    ) -> AppIcon {
        // The canvas is opaque, so each shape is drawn white on black and its
        // brightness read back as coverage.
        let mask = |paint: &dyn Fn(&mut crate::render::Canvas)| -> Vec<u8> {
            let mut c = crate::render::Canvas::new(size, size);
            c.fill(crate::style::color::Color::black());
            paint(&mut c);
            c.pixels
        };
        let tile = mask(&|c| {
            c.round_rect(
                0,
                0,
                size,
                size,
                size / 5,
                crate::style::color::Color::white(),
            )
        });
        let inset = size / 5;
        let mark = mask(&|c| {
            crate::render::icons::draw(
                c,
                glyph,
                inset,
                inset,
                size - inset * 2,
                crate::style::color::Color::white(),
            )
        });
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        for i in 0..size * size {
            let o = i * 4;
            let colored = background.mix(foreground, mark[o as usize] as f64 / 255.0);
            rgba[o as usize] = colored.r;
            rgba[(o + 1) as usize] = colored.g;
            rgba[(o + 2) as usize] = colored.b;
            rgba[(o + 3) as usize] = tile[o as usize]
        }
        AppIcon {
            rgba,
            width: size,
            height: size,
        }
    }
}

/// Whether `entry` matches every word of `query` in its name, generic name,
/// comment, keywords, command or id (case-insensitive).
pub fn matches_query(entry: &AppEntry, query: &str) -> bool {
    let mut fields = vec![
        entry.name.clone(),
        entry.generic_name.clone(),
        entry.comment.clone(),
    ];
    // People also type the name a program is installed under, which the
    // display name often omits (`gnome-terminal`, `org.gnome.Terminal` for
    // "Terminal").
    for name in entry.exec.first().into_iter().chain([&entry.id]) {
        if let Some(stem) = std::path::Path::new(name).file_stem() {
            fields.push(stem.to_string_lossy().into_owned())
        }
    }
    for k in &entry.keywords {
        fields.push(k.to_string())
    }
    let haystack = fields.join(" ").to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|w| haystack.contains(w))
}

// How `name` matches `q`, both lowercase: 3 when it starts with `q`, 2 when
// one of its words does, 1 when it contains `q`, else 0.
fn name_match(name: &str, q: &str) -> i64 {
    if name.starts_with(q) {
        return 3;
    };
    if name.split_whitespace().any(|w| w.starts_with(q)) {
        return 2;
    };
    if name.contains(q) { 1 } else { 0 }
}

/// The entries matching `query` (see `matches_query`), best first, at most
/// `limit`: a name starting with the query, then one with a word starting
/// with it, then one containing it, then a match elsewhere; a recent launch
/// ranks above a match of the same kind and newer above older, then names
/// sort alphabetically.
pub fn search(
    entries: &[AppEntry],
    query: &str,
    history: &AppHistory,
    limit: i64,
) -> Vec<AppEntry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    };
    let mut found = vec![];
    for e in entries {
        if !matches_query(e, &q) {
            continue;
        };
        let name = e.name.to_lowercase();
        let kind = name_match(&name, &q);
        let recency = match history.recent.iter().position(|r| *r == e.id) {
            Some(i) => RECENT_LIMIT - i as i64,
            None => 0,
        };
        found.push((kind * (RECENT_LIMIT + 1) + recency, name, e))
    }
    found.sort_by_key(|f| (std::cmp::Reverse(f.0), f.1.clone()));
    let ranked = found.into_iter().map(|f| f.2.clone()).collect::<Vec<_>>();
    if ranked.len() > limit as usize {
        ranked[..limit as usize].to_vec()
    } else {
        ranked
    }
}

// ---- desktop entries (freedesktop.org Desktop Entry Specification) ----

// A value with the specification's escapes (`\s`, `\n`, `\t`, `\r`, `\\`) undone.
fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            match c {
                's' => out.push(' '),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                _ => out.push(c),
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c)
        }
    }
    out
}

// A `;`-separated list value; a trailing `;` ends the last item.
fn list_value(value: &str) -> Vec<String> {
    unescape(value)
        .split(";")
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

// The `Exec` value split into arguments: double quotes group, and inside
// them `\"`, `` \` ``, `\$` and `\\` stand for the character.
fn exec_arguments(value: &str) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut escaped = false;
    for c in unescape(value).chars() {
        if quoted {
            if escaped {
                current.push(c);
                escaped = false
            } else if c == '\\' {
                escaped = true
            } else if c == '"' {
                quoted = false
            } else {
                current.push(c)
            }
        } else if c == '"' {
            quoted = true;
            started = true;
        } else if c == ' ' || c == '\t' {
            if started {
                out.push(current);
                current = String::new();
                started = false
            }
        } else {
            current.push(c);
            started = true;
        }
    }
    if started {
        out.push(current)
    };
    out
}

// The value of `key` for the most specific of `locale`'s forms
// (`ll_CC@MOD`, `ll_CC`, `ll@MOD`, `ll`), else the plain value.
fn localized(values: &HashMap<String, String>, key: &str, locale: &str) -> String {
    let tag = locale.split(".").next().unwrap_or_default().to_string();
    let (lang_country, modifier) = match tag.split_once("@") {
        Some((a, b)) => (a, b),
        None => (tag.as_str(), ""),
    };
    let lang = lang_country.split("_").next().unwrap_or_default();
    let mut forms = vec![];
    if !modifier.is_empty() {
        forms.push(format!("{}@{}", lang_country, modifier))
    };
    forms.push(lang_country.to_string());
    if !modifier.is_empty() {
        forms.push(format!("{}@{}", lang, modifier))
    };
    forms.push(lang.to_string());
    for form in forms {
        if let Some(v) = values.get(&format!("{}[{}]", key, form)) {
            return unescape(v);
        }
    }
    unescape(values.get(key).map_or("", String::as_str))
}

/// The application a desktop entry describes, or `None` when it is not one
/// to show: not `Type=Application`, `NoDisplay` or `Hidden`, shown only in
/// other desktops (`OnlyShowIn` / `NotShowIn` against `desktops`), or
/// without an `Exec`. `locale` picks localized names (`de_DE`).
pub fn parse_desktop_entry(
    text: &str,
    id: &str,
    source: &str,
    locale: &str,
    desktops: &[String],
) -> Option<AppEntry> {
    let values = ini_groups(text)
        .get("Desktop Entry")
        .cloned()
        .unwrap_or_default();
    if values.get("Type").map_or("", String::as_str) != "Application" {
        return None;
    };
    if values.get("NoDisplay").map_or("", String::as_str) == "true"
        || values.get("Hidden").map_or("", String::as_str) == "true"
    {
        return None;
    };
    let only = list_value(values.get("OnlyShowIn").map_or("", String::as_str));
    if !only.is_empty() && !only.iter().any(|d| desktops.contains(d)) {
        return None;
    };
    let not = list_value(values.get("NotShowIn").map_or("", String::as_str));
    if not.iter().any(|d| desktops.contains(d)) {
        return None;
    };
    let exec = exec_arguments(values.get("Exec").map_or("", String::as_str));
    if exec.is_empty() {
        return None;
    };
    let name = localized(&values.clone(), "Name", locale);
    if name.is_empty() {
        return None;
    };
    Some(AppEntry {
        id: id.to_string(),
        name,
        generic_name: localized(&values.clone(), "GenericName", locale),
        comment: localized(&values.clone(), "Comment", locale),
        icon: localized(&values.clone(), "Icon", locale),
        keywords: list_value(&localized(&values.clone(), "Keywords", locale)),
        categories: list_value(values.get("Categories").map_or("", String::as_str)),
        exec,
        terminal: values.get("Terminal").map_or("", String::as_str) == "true",
        source: source.to_string(),
        image: None,
    })
}

/// The arguments that start `entry` on `files` (paths): `%f`/`%u` take the
/// first file and `%F`/`%U` all of them, `%i` the icon (`--icon NAME`), `%c`
/// the name, `%k` the entry's file, `%%` a percent sign; deprecated codes
/// are dropped.
pub fn command_for(entry: &AppEntry, files: &[String]) -> Vec<String> {
    let mut out = vec![];
    for arg in &entry.exec {
        match arg.as_str() {
            "%f" | "%u" => {
                if !files.is_empty() {
                    out.push(files[0].clone())
                }
            }
            "%F" | "%U" => {
                for f in files {
                    out.push(f.to_string())
                }
            }
            "%i" => {
                if !entry.icon.is_empty() {
                    out.push("--icon".to_string());
                    out.push(entry.icon.clone())
                }
            }
            "%c" => out.push(entry.name.clone()),
            "%k" => out.push(entry.source.clone()),
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => (),
            _ => {
                let mut text = String::new();
                let mut percent = false;
                for c in arg.chars() {
                    if percent {
                        match c {
                            '%' => text.push('%'),
                            'c' => text += &entry.name,
                            'k' => text += &entry.source,
                            _ => (),
                        }
                        percent = false;
                    } else if c == '%' {
                        percent = true;
                    } else {
                        text.push(c)
                    }
                }
                out.push(text)
            }
        }
    }
    out
}

// Whether `program` can run: an existing absolute path, or a name found on PATH.
fn on_path(program: &str) -> bool {
    if paths::is_absolute(program) {
        return paths::is_file(program);
    };
    env::var("PATH")
        .unwrap_or("".to_string())
        .split(":")
        .any(|dir| !dir.is_empty() && paths::is_file(&paths::join(dir, program)))
}

/// The XDG data folders' `applications` folders, most important first.
pub fn desktop_entry_dirs() -> Vec<String> {
    let home = paths::home().unwrap_or_default();
    let data_home = env::var("XDG_DATA_HOME").unwrap_or(paths::join(&home, ".local/share"));
    let mut out = vec![paths::join(&data_home, "applications")];
    for d in env::var("XDG_DATA_DIRS")
        .unwrap_or("/usr/local/share:/usr/share".to_string())
        .split(":")
    {
        if !d.is_empty() {
            out.push(paths::join(d, "applications"))
        }
    }
    out
}

pub const CATALOG_BYTE_LIMIT: usize = 16 * 1024 * 1024;
const ENTRY_BYTE_LIMIT: usize = 1024 * 1024;

impl AppEntry {
    pub fn owned_bytes(&self) -> usize {
        size_of::<Self>()
            + self.id.capacity()
            + self.name.capacity()
            + self.generic_name.capacity()
            + self.comment.capacity()
            + self.icon.capacity()
            + self.source.capacity()
            + [&self.keywords, &self.categories, &self.exec]
                .iter()
                .map(|values| {
                    values.capacity() * size_of::<String>()
                        + values.iter().map(String::capacity).sum::<usize>()
                })
                .sum::<usize>()
            + self.image.as_ref().map_or(0, |image| image.rgba.capacity())
    }
}

pub fn scan_desktop_entries(dirs: &[String], locale: &str, desktops: &[String]) -> Vec<AppEntry> {
    scan_desktop_entries_cancellable(dirs, locale, desktops, &|| true, &mut |_, _| {})
        .unwrap_or_default()
}

pub fn scan_desktop_entries_cancellable(
    dirs: &[String],
    locale: &str,
    desktops: &[String],
    active: &dyn Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<AppEntry>, String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut charged = 0usize;
    let mut visited = 0;
    for root in dirs {
        super::bounded_io::walk(std::slice::from_ref(root), active, |file| {
            let path = file.path();
            if path.extension().is_none_or(|ext| ext != "desktop") {
                return Ok(());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "-");
            if seen.contains(&relative) {
                return Ok(());
            }
            charged += relative.len() + size_of::<String>() * 4;
            if charged > CATALOG_BYTE_LIMIT {
                return Err("application catalog exceeds its byte budget".into());
            }
            seen.insert(relative.clone());
            let Ok(bytes) = super::bounded_io::read(&path, ENTRY_BYTE_LIMIT, active) else {
                if !active() {
                    return Err("application catalog cancelled or expired".into());
                }
                return Ok(());
            };
            let text = String::from_utf8_lossy(&bytes);
            let try_exec = text
                .lines()
                .find_map(|line| line.strip_prefix("TryExec="))
                .unwrap_or("")
                .trim();
            if !try_exec.is_empty() && !on_path(try_exec) {
                return Ok(());
            }
            if let Some(entry) =
                parse_desktop_entry(&text, &relative, &path.to_string_lossy(), locale, desktops)
            {
                charged += entry.owned_bytes() * 2;
                if charged > CATALOG_BYTE_LIMIT {
                    return Err("application catalog exceeds its byte budget".into());
                }
                out.push(entry);
            }
            visited += 1;
            progress(visited, 0);
            Ok(())
        })?;
    }
    out.sort_by_cached_key(|entry| entry.name.to_lowercase());
    Ok(out)
}

// ---- macOS bundles ----

/// The string values of an XML property list's top-level dictionary, by key.
/// Binary property lists answer an empty map.
pub fn plist_strings(xml: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<key>") {
        let after = &rest[at + 5..];
        let Some(end) = after.find("</key>") else {
            break;
        };
        let key = &after[..end];
        let tail = after[end + 6..].trim_start();
        if let Some(body) = tail.strip_prefix("<string>")
            && let Some(close) = body.find("</string>")
        {
            let value = body[..close]
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&");
            out.insert(key.to_string(), value);
        }
        rest = tail;
    }
    out
}

/// The application in the bundle at `bundle` (`Name.app`), named from its
/// `Info.plist` when that is XML, else from the folder; it starts with
/// `open -a`, which also handles bundles whose list is binary.
pub fn bundle_entry(bundle: &str, info: &str) -> AppEntry {
    let keys = plist_strings(info);
    let folder = paths::file_name(bundle).unwrap_or_else(|| bundle.to_string());
    let fallback = folder.strip_suffix(".app").unwrap_or(&folder).to_string();
    let name = keys
        .get("CFBundleDisplayName")
        .or_else(|| keys.get("CFBundleName"))
        .cloned()
        .unwrap_or(fallback);
    AppEntry {
        id: bundle.to_string(),
        name,
        generic_name: String::new(),
        comment: String::new(),
        icon: keys.get("CFBundleIconFile").cloned().unwrap_or_default(),
        keywords: vec![],
        categories: vec![],
        exec: vec![
            "/usr/bin/open".to_string(),
            "-a".to_string(),
            bundle.to_string(),
            "%F".to_string(),
        ],
        terminal: false,
        source: paths::join(bundle, "Contents/Info.plist"),
        image: None,
    }
}

#[cfg(target_os = "macos")]
fn scan_bundles(
    dirs: &[String],
    active: &dyn Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<AppEntry>, String> {
    let mut out = Vec::new();
    let mut charged = 0usize;
    super::bounded_io::walk(dirs, active, |file| {
        let path = file.path();
        if path.extension().is_none_or(|ext| ext != "app") {
            return Ok(());
        }
        let info =
            super::bounded_io::read(&path.join("Contents/Info.plist"), ENTRY_BYTE_LIMIT, active)
                .unwrap_or_default();
        if !active() {
            return Err("application catalog cancelled or expired".into());
        }
        let entry = bundle_entry(&path.to_string_lossy(), &String::from_utf8_lossy(&info));
        charged += entry.owned_bytes() * 2;
        if charged > CATALOG_BYTE_LIMIT {
            return Err("application catalog exceeds its byte budget".into());
        }
        out.push(entry);
        progress(out.len() as i64, 0);
        Ok(())
    })?;
    out.sort_by_cached_key(|entry| entry.name.to_lowercase());
    Ok(out)
}

// ---- Windows shortcuts (MS-SHLLINK) ----

fn le32(b: &[u8], at: i64) -> i64 {
    if at < 0 || at + 4 > b.len() as i64 {
        return -1;
    };
    (b[at as usize] as i64)
        | ((b[(at + 1) as usize] as i64) << 8)
        | ((b[(at + 2) as usize] as i64) << 16)
        | ((b[(at + 3) as usize] as i64) << 24)
}

fn le16(b: &[u8], at: i64) -> i64 {
    if at < 0 || at + 2 > b.len() as i64 {
        return -1;
    };
    (b[at as usize] as i64) | ((b[(at + 1) as usize] as i64) << 8)
}

/// The local path a shell link (`.lnk`) points at, from its LinkInfo's
/// local base path; `None` for links without one (network or item-list only).
pub fn shell_link_target(b: &[u8]) -> Option<String> {
    if le32(b, 0) != 0x4C {
        return None;
    };
    let flags = le32(b, 20);
    let mut at = 0x4C;
    // HasLinkTargetIDList: a size-prefixed item list comes first.
    if flags & 1 != 0 {
        at += 2 + le16(b, at)
    };
    // HasLinkInfo.
    if flags & 2 == 0 {
        return None;
    };
    let info = at;
    let info_size = le32(b, info);
    let info_flags = le32(b, info + 8);
    if info_size <= 0 || info_flags & 1 == 0 {
        return None;
    };
    let base = info + le32(b, info + 16);
    let mut out = String::new();
    let mut i = base;
    while i < b.len() as i64 && i < info + info_size && b[i as usize] != 0u8 {
        out.push(char::from_u32(b[i as usize] as u32).unwrap_or('?'));
        i += 1;
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(windows)]
fn scan_shortcuts(
    dirs: &[String],
    active: &dyn Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<AppEntry>, String> {
    let mut out = Vec::new();
    let mut charged = 0usize;
    super::bounded_io::walk(dirs, active, |file| {
        let path = file.path();
        if path
            .extension()
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("lnk"))
        {
            return Ok(());
        }
        let Ok(bytes) = super::bounded_io::read(&path, ENTRY_BYTE_LIMIT, active) else {
            if !active() {
                return Err("application catalog cancelled or expired".into());
            }
            return Ok(());
        };
        let Some(target) = shell_link_target(&bytes) else {
            return Ok(());
        };
        if !target.to_lowercase().ends_with(".exe") {
            return Ok(());
        }
        let source = path.to_string_lossy().into_owned();
        let entry = AppEntry {
            id: source.clone(),
            name: path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            generic_name: String::new(),
            comment: String::new(),
            icon: target.clone(),
            keywords: vec![],
            categories: vec![],
            exec: vec![target, "%F".into()],
            terminal: false,
            source,
            image: None,
        };
        charged += entry.owned_bytes() * 2;
        if charged > CATALOG_BYTE_LIMIT {
            return Err("application catalog exceeds its byte budget".into());
        }
        out.push(entry);
        progress(out.len() as i64, 0);
        Ok(())
    })?;
    out.sort_by_cached_key(|entry| entry.name.to_lowercase());
    Ok(out)
}

// ---- icons ----

// The groups of an ini-style file, each `[name]` mapped to its `key=value`
// lines, later keys winning.
fn ini_groups(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut groups: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("#") {
            continue;
        };
        if line.starts_with("[") && line.ends_with("]") {
            current = line.substring(1, line.byte_len() - 1).to_string();
            continue;
        }
        let Some((key, value)) = line.split_once("=") else {
            continue;
        };
        let mut group = groups.get(&current).cloned().unwrap_or_default();
        group.insert(key.trim().to_string(), value.trim().to_string());
        groups.insert(current.to_string(), group.clone());
    }
    groups
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum IconDirKind {
    Fixed,
    Scalable,
    Threshold,
}

// One folder of the icon theme, the pixel sizes it serves, and its files.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct IconDir {
    priority: usize,
    folder: String,
    kind: IconDirKind,
    size: i64,
    min: i64,
    max: i64,
    threshold: i64,
    files: HashSet<String>,
}

impl IconDir {
    // How far this folder's sizes are from `px` pixels; 0 when it serves them
    // (the Icon Theme Specification's DirectorySizeDistance).
    fn distance(&self, px: i64) -> i64 {
        let (lo, hi) = match self.kind {
            IconDirKind::Fixed => (self.size, self.size),
            IconDirKind::Scalable => (self.min, self.max),
            IconDirKind::Threshold => (self.size - self.threshold, self.size + self.threshold),
        };
        if px < lo {
            lo - px
        } else if px > hi {
            px - hi
        } else {
            0
        }
    }
}

// The formats `load_icon_file` reads, in the order a folder prefers them.
const ICON_EXTENSIONS: [&str; 2] = ["png", "svg"];

/// Icons from the desktop theme, its parents, hicolor, then pixmaps.
/// Each folder is indexed once, so individual lookups read no directories.
#[derive(Debug, Clone, PartialEq)]
pub struct IconLookup {
    dirs: Vec<IconDir>,
}

impl Default for IconLookup {
    fn default() -> Self {
        Self::new()
    }
}

impl IconLookup {
    /// The `hicolor` folders under the XDG data folders, the user's first.
    pub fn new() -> IconLookup {
        let home = paths::home().unwrap_or_default();
        let mut roots = vec![paths::join(
            &env::var("XDG_DATA_HOME").unwrap_or(paths::join(&home, ".local/share")),
            "icons",
        )];
        for d in env::var("XDG_DATA_DIRS")
            .unwrap_or("/usr/local/share:/usr/share".to_string())
            .split(":")
        {
            if !d.is_empty() {
                roots.push(paths::join(d, "icons"))
            }
        }
        roots.insert(0, paths::join(&home, ".icons"));
        Self::for_theme(&roots, &configured_icon_theme(&home))
    }

    /// The hicolor folders under each root, the user's first.
    pub fn in_roots(roots: &[String]) -> IconLookup {
        Self::in_theme(roots, "hicolor")
    }

    /// Searches a theme and its parents before the required hicolor fallback.
    pub fn for_theme(roots: &[String], theme: &str) -> IconLookup {
        let mut lookup = Self { dirs: vec![] };
        let mut visited = HashSet::new();
        lookup.add_theme(roots, theme, &mut visited);
        let priority = visited.len();
        for mut dir in Self::in_theme(roots, "hicolor").dirs {
            dir.priority = priority;
            lookup.dirs.push(dir);
        }
        lookup
    }

    fn add_theme(&mut self, roots: &[String], theme: &str, visited: &mut HashSet<String>) {
        if theme.is_empty()
            || theme == "hicolor"
            || visited.len() >= 32
            || !visited.insert(theme.to_string())
        {
            return;
        }
        let priority = visited.len() - 1;
        for mut dir in Self::in_theme(roots, theme).dirs {
            dir.priority = priority;
            self.dirs.push(dir);
        }
        let index = roots
            .iter()
            .map(|root| paths::join(&paths::join(root, theme), "index.theme"))
            .find_map(|file| super::bounded_io::read_text(file, ENTRY_BYTE_LIMIT).ok())
            .unwrap_or_default();
        if let Some(head) = ini_groups(&index).get("Icon Theme") {
            for parent in head.get("Inherits").map_or("", String::as_str).split(',') {
                self.add_theme(roots, parent.trim(), visited);
            }
        }
    }

    fn in_theme(roots: &[String], theme: &str) -> IconLookup {
        let homes = roots
            .iter()
            .map(|r| paths::join(r, theme))
            .collect::<Vec<_>>()
            .iter()
            .filter(|d| paths::is_dir(d))
            .cloned()
            .collect::<Vec<_>>();
        let mut dirs = vec![];
        let Some(index) = homes
            .iter()
            .map(|d| paths::join(d, "index.theme"))
            .find(|f| paths::is_file(f))
        else {
            return IconLookup { dirs };
        };
        let Ok(text) = super::bounded_io::read_text(index, ENTRY_BYTE_LIMIT) else {
            return IconLookup { dirs };
        };
        let groups = ini_groups(&text);
        let head = groups.get("Icon Theme").cloned().unwrap_or_default();
        for sub in head
            .get("Directories")
            .map_or("", String::as_str)
            .split(",")
        {
            let Some(g) = groups.get(sub.trim()) else {
                continue;
            };
            let num = |k: String, d: i64| g.get(&k).and_then(|v| v.to_i64()).unwrap_or(d);
            // Folders for other scales serve their pixel size: 24 at @2 is 48.
            let scale = num("Scale".to_string(), 1).max(1);
            let size = num("Size".to_string(), 0) * scale;
            if size <= 0 {
                continue;
            };
            let kind = match g.get("Type").map_or("Threshold", String::as_str) {
                "Fixed" => IconDirKind::Fixed,
                "Scalable" => IconDirKind::Scalable,
                _ => IconDirKind::Threshold,
            };
            for home in &homes {
                // Theme directories are written with `/` on every platform.
                let folder = sub
                    .trim()
                    .split('/')
                    .fold(home.to_string(), |at, step| paths::join(&at, step));
                let Ok(entries) = paths::read_dir(&folder) else {
                    continue;
                };
                let mut files = HashSet::new();
                for e in entries {
                    files.insert(e.name);
                }
                dirs.push(IconDir {
                    priority: 0,
                    folder,
                    kind,
                    size,
                    min: num("MinSize".to_string(), size / scale) * scale,
                    max: num("MaxSize".to_string(), size / scale) * scale,
                    threshold: num("Threshold".to_string(), 2) * scale,
                    files,
                })
            }
        }
        IconLookup { dirs }
    }

    /// The file for icon `name` nearest `px` pixels: `name` itself when it is
    /// a path, else the theme folder serving that size (PNG before SVG) or
    /// the nearest one, else `pixmaps`.
    pub fn find(&self, name: &str, px: i64) -> Option<String> {
        if name.is_empty() {
            return None;
        };
        if paths::is_absolute(name) {
            return if readable_icon(name) && paths::is_file(name) {
                Some(name.to_string())
            } else {
                None
            };
        };
        let lower = name.to_lowercase();
        // Desktop entries sometimes name the file; the theme holds the stem.
        let stem = if lower.ends_with(".png") || lower.ends_with(".svg") {
            name.substring(0, name.byte_len() - 4)
        } else {
            name
        };
        let mut best = None;
        let mut best_rank = (usize::MAX, i64::MAX);
        for d in &self.dirs {
            for ext in ICON_EXTENSIONS {
                let file = format!("{}.{}", stem, ext);
                if !d.files.contains(&file) {
                    continue;
                };
                let rank = (d.priority, d.distance(px));
                if rank < best_rank {
                    best = Some(paths::join(&d.folder, &file));
                    best_rank = rank;
                }
            }
        }
        if best.is_some() {
            return best;
        };
        for ext in ICON_EXTENSIONS {
            let file = paths::join("/usr/share/pixmaps", &format!("{}.{}", stem, ext));
            if paths::is_file(&file) {
                return Some(file);
            }
        }
        None
    }
}

fn configured_icon_theme(home: &str) -> String {
    let config = env::var("XDG_CONFIG_HOME").unwrap_or(paths::join(home, ".config"));
    let mut sources = vec![];
    if env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|d| d == "KDE")
    {
        sources.push(("kdeglobals", "Icons", "Theme"));
    }
    sources.extend([
        ("gtk-3.0/settings.ini", "Settings", "gtk-icon-theme-name"),
        ("gtk-4.0/settings.ini", "Settings", "gtk-icon-theme-name"),
    ]);
    for (file, group, key) in sources {
        let text = super::bounded_io::read_text(paths::join(&config, file), ENTRY_BYTE_LIMIT)
            .unwrap_or_default();
        if let Some(theme) = ini_groups(&text).get(group).and_then(|g| g.get(key))
            && !theme.is_empty()
        {
            return theme.clone();
        }
    }
    "hicolor".to_string()
}

fn readable_icon(file: &str) -> bool {
    let lower = file.to_lowercase();
    lower.ends_with(".png") || lower.ends_with(".svg")
}

/// The file for icon `name` nearest `size` pixels (see `IconLookup::find`).
pub fn icon_file(name: &str, size: i64) -> Option<String> {
    IconLookup::new().find(name, size)
}

// `rgba` (w x h) shrunk to fit `size` x `size` by averaging the pixels each
// target pixel covers.
fn shrink(rgba: &[u8], w: i64, h: i64, size: i64) -> AppIcon {
    if w <= size && h <= size {
        return AppIcon {
            rgba: rgba.to_vec(),
            width: w,
            height: h,
        };
    };
    let scale = (w.max(h) as f64) / size as f64;
    let tw = ((w as f64 / scale) as i64).max(1);
    let th = ((h as f64 / scale) as i64).max(1);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    let mut y = 0;
    while y < th {
        let y0 = (y as f64 * scale) as i64;
        let y1 = (((y + 1) as f64 * scale) as i64).min(h).max(y0 + 1);
        let mut x = 0;
        while x < tw {
            let x0 = (x as f64 * scale) as i64;
            let x1 = (((x + 1) as f64 * scale) as i64).min(w).max(x0 + 1);
            let mut sums = [0, 0, 0, 0];
            let mut sy = y0;
            while sy < y1 {
                let mut sx = x0;
                while sx < x1 {
                    let at = (sy * w + sx) * 4;
                    // Colors weigh by their coverage so transparent edges do not darken.
                    let a = rgba[(at + 3) as usize] as i64;
                    sums[0] += rgba[at as usize] as i64 * a;
                    sums[1] += rgba[(at + 1) as usize] as i64 * a;
                    sums[2] += rgba[(at + 2) as usize] as i64 * a;
                    sums[3] += a;
                    sx += 1;
                }
                sy += 1;
            }
            let count = (y1 - y0) * (x1 - x0);
            let o = (y * tw + x) * 4;
            if sums[3] > 0 {
                out[o as usize] = (sums[0] / sums[3]) as u8;
                out[(o + 1) as usize] = (sums[1] / sums[3]) as u8;
                out[(o + 2) as usize] = (sums[2] / sums[3]) as u8
            }
            out[(o + 3) as usize] = (sums[3] / count) as u8;
            x += 1;
        }
        y += 1;
    }
    AppIcon {
        rgba: out,
        width: tw,
        height: th,
    }
}

/// A pixel size for `with_icons`: room for a 64-unit icon at scale 1.5.
pub const ICON_SIZE: i64 = 96;

/// The icon for `name` fit to `size` pixels, when the theme has one.
pub fn load_icon(name: &str, size: i64) -> Option<AppIcon> {
    load_icon_file(&icon_file(name, size)?, size)
}

/// The PNG in `file` fit to `size` pixels, or the SVG in it drawn at `size`.
pub fn load_icon_file(file: &str, size: i64) -> Option<AppIcon> {
    if file.to_lowercase().ends_with(".svg") {
        let picture = svg::rasterize(
            &super::bounded_io::read_text(file, ENTRY_BYTE_LIMIT).ok()?,
            size,
        )
        .ok()?;
        return Some(AppIcon {
            rgba: picture.rgba,
            width: picture.width,
            height: picture.height,
        });
    }
    let bytes =
        super::bounded_io::read(std::path::Path::new(file), 32 * 1024 * 1024, &|| true).ok()?;
    let img = crate::render::png::decode(&bytes)?;
    Some(shrink(&img.rgba, img.width, img.height, size))
}

/// `entries` with their icons loaded at `size` pixels where the theme has them.
pub fn with_icons(entries: &[AppEntry], size: i64) -> Vec<AppEntry> {
    let lookup = IconLookup::new();
    let mut cache = HashMap::new();
    entries
        .iter()
        .map(|e| {
            let mut out = e.clone();
            out.image = cache
                .entry(e.icon.clone())
                .or_insert_with(|| {
                    lookup
                        .find(&e.icon, size)
                        .and_then(|file| load_icon_file(&file, size))
                })
                .clone();
            out
        })
        .collect::<Vec<_>>()
}

// ---- the platform catalog ----

/// The applications installed for this user, by name.
#[cfg(target_os = "linux")]
pub fn installed() -> Vec<AppEntry> {
    installed_cancellable(&|| true, &mut |_, _| {}).unwrap_or_default()
}

#[cfg(target_os = "linux")]
pub fn installed_cancellable(
    active: &dyn Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<AppEntry>, String> {
    let locale = env::var("LC_ALL")
        .unwrap_or(env::var("LC_MESSAGES").unwrap_or(env::var("LANG").unwrap_or("".to_string())));
    let desktops = env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or("".to_string())
        .split(":")
        .filter(|d| !d.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
    scan_desktop_entries_cancellable(&desktop_entry_dirs(), &locale, &desktops, active, progress)
}

/// The applications installed for this user, by name.
#[cfg(target_os = "macos")]
pub fn installed() -> Vec<AppEntry> {
    installed_cancellable(&|| true, &mut |_, _| {}).unwrap_or_default()
}

#[cfg(target_os = "macos")]
pub fn installed_cancellable(
    active: &dyn Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<AppEntry>, String> {
    scan_bundles(
        &[
            "/Applications".to_string(),
            "/System/Applications".to_string(),
            paths::join(&paths::home().unwrap_or_default(), "Applications"),
        ],
        active,
        progress,
    )
}

/// The applications installed for this user, by name.
#[cfg(target_os = "windows")]
pub fn installed() -> Vec<AppEntry> {
    installed_cancellable(&|| true, &mut |_, _| {}).unwrap_or_default()
}

#[cfg(target_os = "windows")]
pub fn installed_cancellable(
    active: &dyn Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<AppEntry>, String> {
    let dirs: Vec<String> = ["APPDATA", "ProgramData"]
        .iter()
        .filter_map(|v| env::var(v).ok())
        .map(|d| paths::join(&d, "Microsoft\\Windows\\Start Menu\\Programs"))
        .collect();
    scan_shortcuts(&dirs, active, progress)
}

/// Starts `entry` on `files` as its own process, never through a shell; an
/// entry that wants a terminal runs in `$TERMINAL` (or `x-terminal-emulator`).
pub fn launch(entry: &AppEntry, files: &[String]) -> Result<(), errors::Error> {
    let mut argv = command_for(entry, files);
    if argv.is_empty() {
        return Err(errors::new(format!("{} has no command", entry.name)));
    };
    if entry.terminal {
        let terminal = env::var("TERMINAL").unwrap_or("x-terminal-emulator".to_string());
        let mut wrapped = vec![terminal, "-e".to_string()];
        for a in argv {
            wrapped.push(a)
        }
        argv = wrapped;
    }
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    detach(&mut cmd);
    let mut child = cmd.spawn()?;
    if cfg!(unix) {
        // The spawned process is the intermediate, which exits at once.
        child.wait()?;
    }
    Ok(())
}

// The program outlives the launcher and is never its child: a second fork
// hands it to init (or the session's subreaper), which reaps it when it
// exits, and setsid keeps it out of the launcher's session and its signals.
// std's exec-error pipe reaches the grandchild, so a failed exec still fails
// `spawn`.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the hook runs in the forked child before exec and calls only
    // async-signal-safe functions (setsid, fork, _exit) and reads errno.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                0 => Ok(()),
                _ => libc::_exit(0),
            }
        });
    }
}

// A Windows process has no parent that must reap it; dropping the handle
// releases it.
#[cfg(not(unix))]
fn detach(_: &mut Command) {}

// ---- favorites and recent launches ----

/// The applications a user pinned and the ones they started last, by id.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppHistory {
    pub favorites: Vec<String>,
    /// Newest first.
    pub recent: Vec<String>,
}

/// The most recent launches `AppHistory` keeps.
pub const RECENT_LIMIT: i64 = 20;

impl AppHistory {
    /// This history with `id` started now: first in `recent`, once.
    pub fn launched(&self, id: &str) -> AppHistory {
        let mut recent = vec![id.to_string()];
        for r in &self.recent {
            if r != id && recent.len() < RECENT_LIMIT as usize {
                recent.push(r.clone())
            }
        }
        AppHistory {
            favorites: self.favorites.clone(),
            recent,
        }
    }

    /// This history with `id` pinned or unpinned.
    pub fn toggled_favorite(&self, id: &str) -> AppHistory {
        let mut favorites = self
            .favorites
            .iter()
            .filter(|f| *f != id)
            .cloned()
            .collect::<Vec<_>>();
        if favorites.len() == self.favorites.len() {
            favorites.push(id.to_string())
        };
        AppHistory {
            favorites,
            recent: self.recent.clone(),
        }
    }
}

fn history_file(app_id: &str) -> String {
    services::settings_path(app_id, "launcher-history")
}

/// The launcher history saved for `app_id`; empty when none was saved.
pub fn load_history(app_id: &str) -> Result<AppHistory, errors::Error> {
    let s = services::load_settings(&history_file(app_id), 1, |_, s| s)?;
    Ok(AppHistory {
        favorites: s.get_list("favorites"),
        recent: s.get_list("recent"),
    })
}

/// Saves `history` for `app_id`.
pub fn save_history(app_id: &str, history: &AppHistory) -> Result<(), errors::Error> {
    let mut s = services::Settings {
        version: 1,
        values: HashMap::new(),
    };
    s.set_list("favorites", &history.favorites);
    s.set_list("recent", &history.recent);
    services::save_settings(&history_file(app_id), &s)
}

#[cfg(test)]
mod tests {
    use super::{
        AppEntry, AppHistory, IconLookup, bundle_entry, command_for, load_history, matches_query,
        parse_desktop_entry, plist_strings, save_history, scan_desktop_entries, search,
        shell_link_target,
    };
    use crate::paths;
    use crate::strext::StrExt;
    use std::fs;

    // `rel`, written with `/`, under `base` in the platform's spelling.
    fn under(base: &str, rel: &str) -> String {
        rel.split('/')
            .fold(base.to_string(), |at, step| paths::join(&at, step))
    }

    fn named(id: &str, name: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: name.to_string(),
            generic_name: String::new(),
            comment: String::new(),
            icon: String::new(),
            keywords: vec![],
            categories: vec![],
            exec: vec![id.to_string()],
            terminal: false,
            source: String::new(),
            image: None,
        }
    }

    #[test]
    fn search_ranks_name_prefixes_then_word_prefixes_then_the_rest_and_recent_within_each() {
        let mut viewer = named("viewer", "Image Viewer");
        viewer.keywords = vec!["pictures".to_string()];
        let entries = vec![
            named("files", "Files"),
            named("profile", "Profile Editor"),
            viewer,
            named("firefox", "Firefox"),
        ];
        let none = AppHistory::default();
        assert_eq!(
            search(&entries, "fi", &none, 10)
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
            vec!["files", "firefox", "profile"]
        );
        assert_eq!(
            search(&entries, "PICT", &none, 10)
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
            vec!["viewer"]
        );
        assert_eq!(search(&entries, "  ", &none, 10).len(), 0);
        let recent = none.launched("profile").launched("firefox");
        assert_eq!(
            search(&entries, "fi", &recent, 10)
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
            vec!["firefox", "files", "profile"]
        );
        assert_eq!(
            search(&entries, "fi", &recent, 1)
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
            vec!["firefox"]
        )
    }

    #[test]
    fn icon_lookup_prefers_the_current_theme_then_its_parents_and_hicolor() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let roots = vec![paths::join(&config, "icons")];
        for (name, size, parents) in [
            ("current", 16, "parent"),
            ("parent", 32, "current"),
            ("hicolor", 32, ""),
        ] {
            let home = paths::join(&roots[0], name);
            fs::create_dir_all(paths::join(&home, "apps")).unwrap();
            fs::write(paths::join(&home, "index.theme"), format!("[Icon Theme]\nDirectories=apps\nInherits={parents}\n[apps]\nSize={size}\nType=Fixed\n")).unwrap();
            fs::write(paths::join(&home, &format!("apps/{name}.svg")), "").unwrap();
            fs::write(under(&home, "apps/shared.svg"), "").unwrap();
        }
        let lookup = IconLookup::for_theme(&roots, "current");
        // A current-theme icon wins even if the parent has an exact-size match.
        assert_eq!(
            lookup.find("shared", 32),
            Some(under(&roots[0], "current/apps/shared.svg"))
        );
        assert_eq!(
            lookup.find("parent", 32),
            Some(under(&roots[0], "parent/apps/parent.svg"))
        );
        assert_eq!(
            lookup.find("hicolor", 32),
            Some(under(&roots[0], "hicolor/apps/hicolor.svg"))
        );
        assert!(lookup.find("missing", 32).is_none());
        let svg = paths::join(&config, "application.svg");
        fs::write(&svg, "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 2 2\"><rect width=\"2\" height=\"2\" fill=\"#3366cc\"/></svg>").unwrap();
        let mut entry = named("app", "App");
        entry.icon = svg;
        let loaded = super::with_icons(&[entry.clone(), entry], 32);
        assert_eq!(loaded[0].image, loaded[1].image);
        assert_eq!(loaded[0].image.as_ref().unwrap().width, 32);
    }

    #[test]
    fn icons_come_from_the_folder_serving_the_size_and_svgs_are_drawn() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let theme = paths::join(&paths::join(&config, "icons"), "hicolor");
        for sub in ["48x48/apps", "scalable/apps", "16x16/apps"] {
            let _ = fs::create_dir_all(paths::join(&theme, sub));
        }
        fs::write(paths::join(&theme, "index.theme"), "[Icon Theme]\nName=Hicolor\nDirectories=48x48/apps,scalable/apps,16x16/apps\n\n[48x48/apps]\nSize=48\nType=Threshold\n\n[scalable/apps]\nSize=128\nMinSize=1\nMaxSize=512\nType=Scalable\n\n[16x16/apps]\nSize=16\nType=Fixed").unwrap();
        let square = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 2 2\"><rect width=\"2\" height=\"2\" fill=\"#3366cc\"/></svg>";
        fs::write(under(&theme, "scalable/apps/drawn.svg"), square).unwrap();
        fs::write(under(&theme, "16x16/apps/tiny.svg"), square).unwrap();
        let lookup = IconLookup::in_roots(&[paths::join(&config, "icons")]);
        assert_eq!(
            lookup.find("drawn", 96),
            Some(under(&theme, "scalable/apps/drawn.svg"))
        );
        assert_eq!(
            lookup.find("drawn.svg", 96),
            Some(under(&theme, "scalable/apps/drawn.svg"))
        );
        assert_eq!(
            lookup.find("tiny", 96),
            Some(under(&theme, "16x16/apps/tiny.svg"))
        );
        assert_eq!(lookup.find("missing", 96), None);
        let Some(icon) = super::load_icon_file(&paths::join(&theme, "scalable/apps/drawn.svg"), 24)
        else {
            panic!("the SVG did not draw");
        };
        assert_eq!((icon.width, icon.height), (24, 24));
        assert_eq!(
            (icon.rgba[0], icon.rgba[1], icon.rgba[2], icon.rgba[3]),
            (0x33u8, 0x66u8, 0xccu8, 255u8)
        )
    }

    const EDITOR: &str = "[Desktop Entry]\nType=Application\nName=Text Editor\nName[de]=Texteditor\nName[de_CH]=Texteditor (CH)\nGenericName=Editor\nComment=Edit text files\nKeywords=notes;plain text;\nExec=gedit --new-window \"%c x\" %U\nIcon=org.gnome.gedit\nCategories=Utility;TextEditor;\n\n[Desktop Action new]\nName=New Window\nExec=gedit --new";

    #[test]
    fn a_desktop_entry_reads_localized_names_and_quoted_arguments() {
        let e = parse_desktop_entry(
            EDITOR,
            "org.gnome.gedit.desktop",
            "/x/gedit.desktop",
            "de_DE.UTF-8",
            &["GNOME".to_string()],
        )
        .unwrap();
        assert_eq!(e.name, "Texteditor");
        assert_eq!(
            parse_desktop_entry(EDITOR, "id", "src", "de_CH", &[])
                .unwrap()
                .name,
            "Texteditor (CH)"
        );
        assert_eq!(
            parse_desktop_entry(EDITOR, "id", "src", "fr", &[])
                .unwrap()
                .name,
            "Text Editor"
        );
        assert_eq!(e.exec, vec!["gedit", "--new-window", "%c x", "%U"]);
        assert_eq!(e.keywords, vec!["notes", "plain text"]);
        assert_eq!(e.categories, vec!["Utility", "TextEditor"]);
        assert_eq!(
            command_for(&e, &["/a.txt".to_string(), "/b.txt".to_string()]),
            vec!["gedit", "--new-window", "Texteditor x", "/a.txt", "/b.txt"]
        );
        assert!(matches_query(&e, "plain edit"));
        assert!(!matches_query(&e, "browser"))
    }

    #[test]
    fn a_program_is_found_by_its_command_and_desktop_id() {
        let mut e = named("org.gnome.Terminal.desktop", "Terminal");
        e.exec = vec![
            "/usr/bin/gnome-terminal".to_string(),
            "--window".to_string(),
        ];
        assert!(matches_query(&e, "gnome-terminal"));
        assert!(matches_query(&e, "gnome term"));
        assert!(matches_query(&e, "org.gnome.terminal"));
        assert!(!matches_query(&e, "desktop"));
        assert!(!matches_query(&e, "window"))
    }

    #[test]
    fn hidden_and_other_desktop_entries_are_left_out() {
        let hidden = "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=true\n";
        assert!(parse_desktop_entry(hidden, "x", "x", "", &[]).is_none());
        let kde_only = "[Desktop Entry]\nType=Application\nName=K\nExec=k\nOnlyShowIn=KDE;\n";
        assert!(parse_desktop_entry(kde_only, "k", "k", "", &["GNOME".to_string()]).is_none());
        assert!(parse_desktop_entry(kde_only, "k", "k", "", &["KDE".to_string()]).is_some());
        let link = "[Desktop Entry]\nType=Link\nName=L\nURL=https://example.com\n";
        assert!(parse_desktop_entry(link, "l", "l", "", &[]).is_none());
        let percent =
            "[Desktop Entry]\nType=Application\nName=P\nExec=p --rate=50%% %f %i\nIcon=pi\n";
        assert_eq!(
            command_for(
                &parse_desktop_entry(percent, "p", "p", "", &[]).unwrap(),
                &[]
            ),
            vec!["p", "--rate=50%", "--icon", "pi"]
        )
    }

    #[test]
    fn earlier_data_folders_hide_later_entries_and_try_exec_filters() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let user = paths::join(&config, "user/applications");
        let system = paths::join(&config, "system/applications");
        fs::create_dir_all(paths::join(&system, "vendor")).unwrap();
        fs::create_dir_all(user.clone()).unwrap();
        fs::write(
            paths::join(&system, "editor.desktop"),
            "[Desktop Entry]\nType=Application\nName=System Editor\nExec=edit\n",
        )
        .unwrap();
        fs::write(
            paths::join(&user, "editor.desktop"),
            "[Desktop Entry]\nType=Application\nName=My Editor\nExec=edit\n",
        )
        .unwrap();
        fs::write(
            paths::join(&system, "vendor/tool.desktop"),
            "[Desktop Entry]\nType=Application\nName=Tool\nExec=tool\n",
        )
        .unwrap();
        fs::write(paths::join(&system, "gone.desktop"), "[Desktop Entry]\nType=Application\nName=Gone\nExec=gone\nTryExec=gui-gos-no-such-program\n").unwrap();
        let found = scan_desktop_entries(&[user, system], "", &[]);
        assert_eq!(
            found.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
            vec!["My Editor", "Tool"]
        );
        assert_eq!(
            found.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            vec!["editor.desktop", "vendor-tool.desktop"]
        )
    }

    #[test]
    fn bundles_and_shortcuts_name_their_programs() {
        let xml = "<plist><dict><key>CFBundleName</key><string>Maps &amp; More</string><key>CFBundleExecutable</key><string>Maps</string></dict></plist>";
        assert_eq!(plist_strings(xml)["CFBundleName"], "Maps & More");
        let maps = bundle_entry("/Applications/Maps.app", xml);
        assert_eq!(maps.name, "Maps & More");
        assert_eq!(
            command_for(&maps, &["/tmp/a.gpx".to_string()]),
            vec![
                "/usr/bin/open",
                "-a",
                "/Applications/Maps.app",
                "/tmp/a.gpx"
            ]
        );
        assert_eq!(
            bundle_entry("/Applications/Old.app", "bplist00...").name,
            "Old"
        );
        // A shell link whose LinkInfo (right after the 0x4C-byte header) names a local path.
        let target = "C:\\Tools\\app.exe";
        let mut lnk = vec![0u8; 0x4C + 28];
        put32(&mut lnk, 0, 0x4C);
        put32(&mut lnk, 20, 2);
        put32(&mut lnk, 0x4C, 28 + target.byte_len() + 1);
        put32(&mut lnk, 0x4C + 4, 28);
        put32(&mut lnk, 0x4C + 8, 1);
        put32(&mut lnk, 0x4C + 16, 28);
        for b in target.bytes() {
            lnk.push(b)
        }
        lnk.push(0u8);
        assert_eq!(shell_link_target(&lnk), Some(target.to_string()));
        assert_eq!(shell_link_target(&[1u8, 2u8]), None)
    }

    fn put32(b: &mut [u8], at: i64, v: i64) {
        b[at as usize] = (v % 256) as u8;
        b[(at + 1) as usize] = ((v / 256) % 256) as u8;
        b[(at + 2) as usize] = ((v / 65_536) % 256) as u8;
        b[(at + 3) as usize] = ((v / 16_777_216) % 256) as u8
    }

    #[test]
    fn favorites_and_recent_launches_persist() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut h = AppHistory::default();
        for id in ["a", "b", "a", "c"] {
            h = h.launched(id)
        }
        assert_eq!(h.recent, vec!["c", "a", "b"]);
        h = h
            .toggled_favorite("b")
            .toggled_favorite("c")
            .toggled_favorite("b");
        assert_eq!(h.favorites, vec!["c"]);
        save_history("dev.gossamer.gui-apps-test", &h).unwrap();
        assert_eq!(load_history("dev.gossamer.gui-apps-test").unwrap(), h)
    }

    #[cfg(unix)]
    #[test]
    fn a_launched_program_is_not_a_child_of_the_launcher() {
        use std::io::BufRead;
        use std::os::fd::FromRawFd;

        // The program inherits the write end, which has no close-on-exec.
        let mut ends = [0; 2];
        // SAFETY: ends has room for the two descriptors pipe writes.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        let mut entry = named("sh", "Shell");
        entry.exec = vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo $PPID >&$0".to_string(),
            ends[1].to_string(),
        ];
        super::launch(&entry, &[]).unwrap();
        // SAFETY: both descriptors came from pipe above and are owned here.
        let read = unsafe {
            libc::close(ends[1]);
            fs::File::from_raw_fd(ends[0])
        };
        let mut parent = String::new();
        std::io::BufReader::new(read)
            .read_line(&mut parent)
            .unwrap();
        let parent: u32 = parent.trim().parse().unwrap();
        assert_ne!(parent, std::process::id());
    }

    #[test]
    fn launching_a_missing_program_fails() {
        let entry = named("gui-launch-test-no-such-program", "Missing");
        assert!(super::launch(&entry, &[]).is_err());
    }
}
