// catalog: every installed font face, summarised from the tables that name it
// and map its characters, without reading its outlines. Summaries are cached
// by file path, size and modification time, so a later run reads only the
// files that changed.

use std::collections::{HashMap, HashSet};
use std::fs;

use crate::errors;

use crate::paths;

use crate::services;
use crate::services::bounded_io;
pub const CATALOG_BYTE_LIMIT: usize = 16 * 1024 * 1024;
const SUMMARY_BYTE_LIMIT: usize = 8 * 1024 * 1024;
use crate::text::font::{Face, be_u16, be_u32, tag};

/// One face of an installed font file.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FontRecord {
    pub path: String,
    /// The face's index in its file (non-zero inside a collection).
    pub index: i64,
    pub family: String,
    /// The family name ID 1 gives where it differs from `family`.
    #[serde(default)]
    pub legacy_family: String,
    pub style: String,
    pub bold: bool,
    pub italic: bool,
    /// The weight on the CSS scale (400 regular, 700 bold); 0 when unknown.
    pub weight: i64,
    pub monospace: bool,
    /// Whether the face draws in color (`COLR`, `CBDT`, or `sbix`).
    pub color: bool,
    /// The scalars the face has glyphs for, as inclusive (first, last)
    /// pairs flattened into one sorted list. Faces that cover the same
    /// scalars, as a family's weights and styles mostly do, share one list.
    pub coverage: std::sync::Arc<[u32]>,
}

impl FontRecord {
    /// Whether the face has a glyph for `c`.
    pub fn covers(&self, c: char) -> bool {
        let code = u32::from(c);
        let mut lo = 0;
        let mut hi = self.coverage.len() / 2;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if code < self.coverage[mid * 2] {
                hi = mid;
            } else if code > self.coverage[mid * 2 + 1] {
                lo = mid + 1;
            } else {
                return true;
            }
        }
        false
    }
}

// The summaries of one file, valid while its size and modification time hold.
#[derive(Clone, Debug, Default, PartialEq)]
struct CachedFile {
    path: String,
    size: i64,
    modified: i64,
    faces: Vec<FontRecord>,
}

// The cache file's contents: each distinct coverage list once, as faces of a
// family mostly share one, and each file's faces naming theirs by index.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct CacheDocument {
    lists: Vec<Vec<u32>>,
    files: Vec<StoredFile>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StoredFile {
    path: String,
    size: i64,
    modified: i64,
    faces: Vec<StoredFace>,
}

// A `FontRecord` as the cache keeps it: its file's path is the entry's, and
// its coverage an index into the document's lists.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StoredFace {
    index: i64,
    family: String,
    legacy_family: String,
    style: String,
    bold: bool,
    italic: bool,
    weight: i64,
    monospace: bool,
    color: bool,
    coverage: usize,
}

impl CacheDocument {
    // The document for `entries`, each coverage list stored once.
    fn of(entries: &[CachedFile]) -> CacheDocument {
        let mut lists: Vec<Vec<u32>> = vec![];
        let mut known: HashMap<std::sync::Arc<[u32]>, usize> = HashMap::new();
        let files = entries
            .iter()
            .map(|entry| StoredFile {
                path: entry.path.clone(),
                size: entry.size,
                modified: entry.modified,
                faces: entry
                    .faces
                    .iter()
                    .map(|face| {
                        let coverage = *known
                            .entry(std::sync::Arc::clone(&face.coverage))
                            .or_insert_with(|| {
                                lists.push(face.coverage.to_vec());
                                lists.len() - 1
                            });
                        StoredFace {
                            index: face.index,
                            family: face.family.clone(),
                            legacy_family: face.legacy_family.clone(),
                            style: face.style.clone(),
                            bold: face.bold,
                            italic: face.italic,
                            weight: face.weight,
                            monospace: face.monospace,
                            color: face.color,
                            coverage,
                        }
                    })
                    .collect(),
            })
            .collect();
        CacheDocument { lists, files }
    }

    // The cached files, their faces sharing the document's lists; `None`
    // when a face names a list the document lacks.
    fn into_files(self) -> Option<Vec<CachedFile>> {
        let lists: Vec<std::sync::Arc<[u32]>> = self.lists.into_iter().map(Into::into).collect();
        self.files
            .into_iter()
            .map(|file| {
                let faces = file
                    .faces
                    .into_iter()
                    .map(|face| {
                        Some(FontRecord {
                            path: file.path.clone(),
                            index: face.index,
                            family: face.family,
                            legacy_family: face.legacy_family,
                            style: face.style,
                            bold: face.bold,
                            italic: face.italic,
                            weight: face.weight,
                            monospace: face.monospace,
                            color: face.color,
                            coverage: std::sync::Arc::clone(lists.get(face.coverage)?),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(CachedFile {
                    path: file.path,
                    size: file.size,
                    modified: file.modified,
                    faces,
                })
            })
            .collect()
    }
}

// The tables a summary reads.
const SUMMARY_TABLES: [&str; 7] = ["head", "maxp", "hhea", "OS/2", "post", "name", "cmap"];

// Up to `len` bytes of `f` from `offset`; fewer where the file ends.
fn read_at(mut f: &fs::File, len: i64, offset: i64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    if len < 0 || len as u64 > SUMMARY_BYTE_LIMIT as u64 {
        return Err(std::io::Error::other(
            "font summary exceeds its byte budget",
        ));
    }
    let range =
        |n: i64| u64::try_from(n).map_err(|_| std::io::Error::other("a negative file range"));
    f.seek(SeekFrom::Start(range(offset)?))?;
    let mut buf = vec![];
    f.take(range(len)?).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Summarises every face of the font file at `file`. A face whose tables
/// cannot be read is left out.
pub fn scan_file(file: &str) -> Result<Vec<FontRecord>, errors::Error> {
    let f = fs::File::open(file)?;
    let size = f.metadata()?.len() as i64;
    let header = read_at(&f, 12, 0)?;
    if header.len() < 12 {
        return Err(errors::new(format!("{} is too short to be a font", file)));
    }
    let mut bases = vec![0];
    if tag(&header, 0) == "ttcf" {
        let count = be_u32(&header, 8);
        if count > 4096 || count > (size - 12) / 4 {
            return Err(errors::new("font collection exceeds its table budget"));
        }
        let table = read_at(&f, count * 4, 12)?;
        bases = (0..count).map(|i| be_u32(&table, i * 4)).collect();
    }
    let mut out = vec![];
    for (index, base) in bases.iter().enumerate() {
        let Ok((tables, color)) = summary_tables(&f, size, *base) else {
            continue;
        };
        let Ok(face) = Face::summary(&tables) else {
            continue;
        };
        out.push(FontRecord {
            path: file.to_string(),
            index: index as i64,
            family: face.family.clone(),
            legacy_family: face.legacy_family.clone(),
            style: face.subfamily.clone(),
            bold: face.bold,
            italic: face.italic,
            weight: face.weight,
            monospace: face.monospace,
            color,
            coverage: face
                .coverage()
                .into_iter()
                .map(|code| u32::try_from(code).unwrap_or(u32::MAX))
                .collect(),
        })
    }
    Ok(out)
}

// A single-face font holding only the summary tables of the face whose table
// directory starts at `base`, read positionally from `f`, and whether the face
// draws in color.
fn summary_tables(f: &fs::File, size: i64, base: i64) -> Result<(Vec<u8>, bool), errors::Error> {
    let head = read_at(f, 12, base)?;
    let count = be_u16(&head, 4);
    let directory = read_at(f, count * 16, base + 12)?;
    if directory.len() < (count * 16) as usize {
        return Err(errors::new("font table directory is truncated"));
    }
    let mut picked = vec![];
    let mut names = HashSet::new();
    for i in 0..count {
        let name = tag(&directory, i * 16);
        names.insert(name.clone());
        let offset = be_u32(&directory, i * 16 + 8);
        let length = be_u32(&directory, i * 16 + 12);
        if SUMMARY_TABLES.contains(&name.as_str()) && offset + length <= size {
            picked.push((name, offset, length))
        }
    }
    let mut out = head[0..4].to_vec();
    push_u16(&mut out, picked.len() as i64);
    for _ in 0..3 {
        push_u16(&mut out, 0)
    }
    let mut at = 12 + picked.len() * 16;
    let mut bodies = vec![];
    for (name, offset, length) in picked {
        if at.saturating_add(length as usize) > SUMMARY_BYTE_LIMIT {
            return Err(errors::new("font summary exceeds its byte budget"));
        }
        let body = read_at(f, length, offset)?;
        for b in name.bytes() {
            out.push(b)
        }
        push_u32(&mut out, 0);
        push_u32(&mut out, at as i64);
        push_u32(&mut out, body.len() as i64);
        at += body.len();
        bodies.push(body)
    }
    for body in bodies {
        out.extend_from_slice(&body);
    }
    Ok((
        out.to_vec(),
        (names.contains("CBDT") && names.contains("CBLC"))
            || names.contains("sbix")
            || (names.contains("COLR") && names.contains("CPAL")),
    ))
}

fn push_u16(out: &mut Vec<u8>, v: i64) {
    out.push(((v >> 8) & 0xff) as u8);
    out.push((v & 0xff) as u8)
}

fn push_u32(out: &mut Vec<u8>, v: i64) {
    push_u16(out, (v >> 16) & 0xffff);
    push_u16(out, v & 0xffff)
}

/// The faces of `files`, reusing the summaries cached under `cache` for
/// files whose size and modification time are unchanged and rewriting the
/// cache when any were read anew.
pub fn load(files: &[String], cache: &str) -> Vec<FontRecord> {
    load_cancellable(files, cache, &|| true, &mut |_, _| {}).unwrap_or_default()
}

impl FontRecord {
    pub fn owned_bytes(&self) -> usize {
        size_of::<Self>()
            + self.path.capacity()
            + self.family.capacity()
            + self.legacy_family.capacity()
            + self.style.capacity()
            + self.coverage.len() * size_of::<u32>()
    }
}

pub fn load_cancellable(
    files: &[String],
    cache: &str,
    active: &dyn Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<FontRecord>, String> {
    let mut known = HashMap::<String, CachedFile>::new();
    if let Ok(bytes) = bounded_io::read(std::path::Path::new(cache), 8 * 1024 * 1024, active)
        && let Ok(document) = serde_json::from_slice::<CacheDocument>(&bytes)
        && let Some(cached) = document.into_files()
    {
        let bytes: usize = cached
            .iter()
            .map(|entry| {
                entry.path.capacity()
                    + size_of::<CachedFile>()
                    + entry
                        .faces
                        .iter()
                        .map(FontRecord::owned_bytes)
                        .sum::<usize>()
            })
            .sum();
        if bytes <= CATALOG_BYTE_LIMIT {
            for entry in cached {
                known.insert(entry.path.clone(), entry);
            }
        }
    }
    let mut entries = Vec::new();
    let mut charged = 0usize;
    let mut changed = known.len() != files.len();
    for (i, file) in files.iter().enumerate() {
        if !active() {
            return Err("font catalog cancelled or expired".into());
        }
        let Ok(meta) = fs::metadata(file) else {
            continue;
        };
        let entry = match known.remove(file) {
            Some(entry)
                if entry.size == meta.len() as i64
                    && entry.modified == paths::modified_ms(&meta) =>
            {
                entry
            }
            _ => {
                changed = true;
                CachedFile {
                    path: file.clone(),
                    size: meta.len() as i64,
                    modified: paths::modified_ms(&meta),
                    faces: scan_file(file).unwrap_or_default(),
                }
            }
        };
        charged = charged.saturating_add(
            entry.path.capacity()
                + size_of::<CachedFile>() * 2
                + entry
                    .faces
                    .iter()
                    .map(|face| face.owned_bytes() + size_of::<FontRecord>())
                    .sum::<usize>(),
        );
        if charged > CATALOG_BYTE_LIMIT {
            return Err("font catalog exceeds its byte budget".into());
        }
        entries.push(entry);
        progress(i as i64 + 1, files.len() as i64);
    }
    if changed
        && active()
        && let Some(dir) = paths::parent(cache)
        && fs::create_dir_all(dir).is_ok()
    {
        // The cache streams to disk, never whole in memory, and stops at its
        // own disk cap.
        let document = CacheDocument::of(&entries);
        struct Limited<'a> {
            out: &'a mut dyn std::io::Write,
            written: usize,
        }
        impl std::io::Write for Limited<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.written.saturating_add(bytes.len()) > 8 * 1024 * 1024 {
                    return Err(std::io::Error::other("font cache exceeds disk byte budget"));
                }
                let n = self.out.write(bytes)?;
                self.written += n;
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.out.flush()
            }
        }
        let _ = services::write_atomic_with(cache, |out| {
            serde_json::to_writer(&mut Limited { out, written: 0 }, &document)?;
            if active() {
                Ok(())
            } else {
                Err(std::io::Error::other("font catalog cancelled or expired"))
            }
        });
    }
    mark_used(cache);
    remove_unused_formats(cache, std::time::SystemTime::now());
    let mut records: Vec<FontRecord> = entries.into_iter().flat_map(|entry| entry.faces).collect();
    share_coverage(&mut records);
    Ok(records)
}

// Equal coverage lists become one shared allocation.
fn share_coverage(records: &mut [FontRecord]) {
    let mut lists: HashSet<std::sync::Arc<[u32]>> = HashSet::new();
    for record in records {
        match lists.get(&record.coverage) {
            Some(list) => record.coverage = std::sync::Arc::clone(list),
            None => {
                lists.insert(std::sync::Arc::clone(&record.coverage));
            }
        }
    }
}

/// Where the toolkit caches the summaries of installed fonts. Every
/// application shares the one file: installed fonts are a fact of the
/// machine, so a second copy would only repeat the cold scan.
pub fn cache_file() -> String {
    paths::join(&services::cache_dir("gui-gos"), "fonts-v6.json")
}

/// How long a catalog of another format must go unused before any toolkit
/// version removes it. Each version marks the format it reads on every load,
/// so a format an installed application still reads stays.
const UNUSED_FORMAT_AGE: std::time::Duration = std::time::Duration::from_hours(30 * 24);

// Records a load of `cache`, whether or not it was rewritten, as the time
// other versions judge the format by.
fn mark_used(cache: &str) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(cache) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

// Removes the catalogs of other formats beside `cache` (`fonts-v2.json` and
// on, older or newer) that nothing has used for `UNUSED_FORMAT_AGE` by `now`.
fn remove_unused_formats(cache: &str, now: std::time::SystemTime) {
    let Some(dir) = paths::parent(cache) else {
        return;
    };
    let Ok(listing) = fs::read_dir(dir) else {
        return;
    };
    let current = std::path::Path::new(cache).file_name();
    for entry in listing.flatten() {
        let name = entry.file_name();
        let text = name.to_string_lossy();
        let catalog = text
            .strip_prefix("fonts-v")
            .and_then(|rest| rest.strip_suffix(".json"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !catalog || Some(name.as_os_str()) == current {
            continue;
        }
        let unused = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|used| {
                now.duration_since(used)
                    .is_ok_and(|age| age >= UNUSED_FORMAT_AGE)
            });
        if unused {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UNUSED_FORMAT_AGE, load, scan_file};
    use crate::paths;
    use crate::text::font::Face;
    use std::fs;
    use std::time::{Duration, SystemTime};

    const SANS: &str = "assets/fonts/DejaVuSans.ttf";

    #[test]
    fn a_summary_names_the_face_and_matches_its_character_map() {
        let records = scan_file(SANS).unwrap();
        assert_eq!(records.len(), 1);
        let r = records[0].clone();
        assert_eq!(
            (r.family.clone(), r.index, r.bold, r.italic, r.monospace),
            ("DejaVu Sans".to_string(), 0, false, false, false)
        );
        let face = Face::parse(&fs::read(SANS).unwrap(), 0).unwrap();
        let mut differ = vec![];
        let mut code = 0;
        while code < 0x20000 {
            if let Some(c) = char::from_u32(code as u32)
                && r.covers(c) != face.covers(c)
            {
                differ.push(code)
            };
            code += 1;
        }
        assert!(differ.is_empty(), "{:?}", differ);
        // A file that is not a font has no faces.
        assert_eq!(scan_file("Cargo.toml").unwrap(), vec![])
    }

    #[test]
    fn the_cache_answers_unchanged_files_without_reading_them() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let cache = paths::join(&config, "fonts.json");
        let first = load(&[SANS.to_string()], &cache);
        assert_eq!(first.len(), 1);
        assert!(paths::exists(&cache));
        // A family name changed in the cache shows the entry was reused.
        let text = fs::read_to_string(cache.clone()).unwrap();
        fs::write(&cache, text.replace("DejaVu Sans", "Cached Sans")).unwrap();
        assert_eq!(load(&[SANS.to_string()], &cache)[0].family, "Cached Sans");
        assert_eq!(load(&[], &cache).len(), 0)
    }

    fn age(file: &str, by: Duration) {
        fs::OpenOptions::new()
            .write(true)
            .open(file)
            .and_then(|f| f.set_modified(SystemTime::now() - by))
            .unwrap();
    }

    #[test]
    fn every_load_marks_the_catalog_used() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let cache = paths::join(&config, "fonts-v6.json");
        assert_eq!(load(&[SANS.to_string()], &cache).len(), 1);
        age(&cache, UNUSED_FORMAT_AGE * 2);
        // Unchanged fonts leave the file as it was, but the load still counts.
        assert_eq!(load(&[SANS.to_string()], &cache).len(), 1);
        let used = fs::metadata(&cache).unwrap().modified().unwrap();
        assert!(SystemTime::now().duration_since(used).unwrap() < UNUSED_FORMAT_AGE);
    }

    #[test]
    fn a_load_removes_other_formats_only_once_they_go_unused() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        fs::create_dir_all(&config).unwrap();
        let unused = [
            paths::join(&config, "fonts-v2.json"),
            paths::join(&config, "fonts-v7.json"),
        ];
        let in_use = paths::join(&config, "fonts-v5.json");
        let unrelated = paths::join(&config, "fonts-vx.json");
        for file in unused.iter().chain([&in_use, &unrelated]) {
            fs::write(file, "{}").unwrap();
        }
        for file in unused.iter().chain([&unrelated]) {
            age(file, UNUSED_FORMAT_AGE + Duration::from_hours(1));
        }
        age(&in_use, UNUSED_FORMAT_AGE - Duration::from_hours(1));
        let cache = paths::join(&config, "fonts-v6.json");
        assert_eq!(load(&[SANS.to_string()], &cache).len(), 1);
        assert!(paths::exists(&cache));
        assert!(unused.iter().all(|file| !paths::exists(file)));
        assert!(
            paths::exists(&in_use),
            "a format read this month was removed"
        );
        assert!(paths::exists(&unrelated));
    }

    #[test]
    fn faces_covering_the_same_scalars_share_one_list() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let cache = paths::join(&config, "fonts.json");
        let mono = "assets/fonts/DejaVuSansMono.ttf".to_string();
        for _ in 0..2 {
            let records = load(&[SANS.to_string(), mono.clone(), SANS.to_string()], &cache);
            assert_eq!(records.len(), 3);
            assert!(std::sync::Arc::ptr_eq(
                &records[0].coverage,
                &records[2].coverage
            ));
            assert!(!std::sync::Arc::ptr_eq(
                &records[0].coverage,
                &records[1].coverage
            ));
            assert!(records[0].covers('A') && !records[0].covers('\u{10FFFF}'));
        }
    }

    #[test]
    fn the_cache_stores_each_coverage_list_once() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let cache = paths::join(&config, "fonts.json");
        let mono = "assets/fonts/DejaVuSansMono.ttf".to_string();
        let records = load(&[SANS.to_string(), mono, SANS.to_string()], &cache);
        assert_eq!(records.len(), 3);
        let stored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!(stored["lists"].as_array().unwrap().len(), 2);
        let first = &stored["files"][0]["faces"][0];
        assert_eq!(first["family"], "DejaVu Sans");
        assert_eq!(first["coverage"], 0);
    }
}
