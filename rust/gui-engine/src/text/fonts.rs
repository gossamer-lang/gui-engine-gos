// fonts: the face registry. Bundled DejaVu faces are embedded in the
// binary so text renders identically on every machine; system faces are
// discovered on demand and consulted only as fallback for characters the
// bundled faces lack.

use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use crate::errors;

use crate::text::catalog;
use crate::text::catalog::FontRecord;
use crate::text::font::{Face, FontBytes};
use crate::text::ucd::EMOJI_PRESENTATION;
use crate::text::variation;

use crate::text::font_assets::{MONO, MONO_BOLD, SANS, SANS_BOLD};

/// A generic family request; the registry resolves it to faces.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Family {
    Sans,
    Monospace,
    /// A family by name, matched against discovered system faces first.
    Named(String),
}

/// The registry's identity for one loaded face.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct FaceId(pub i64);

impl From<i64> for FaceId {
    fn from(n: i64) -> FaceId {
        FaceId(n)
    }
}

impl From<FaceId> for i64 {
    fn from(id: FaceId) -> i64 {
        id.0
    }
}

// Faces by registry index.
static FACES: LazyLock<Mutex<BTreeMap<i64, Face>>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));
static AUTO_FACES: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static NEXT_FACE: AtomicI64 = AtomicI64::new(4);
// The registry indexes of faces that draw color bitmaps.
static COLOR_FACES: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static BUNDLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
static CATALOG_PENDING: AtomicBool = AtomicBool::new(false);
static CATALOG_REQUESTED: AtomicBool = AtomicBool::new(false);
type FaceRequests = BTreeMap<(String, i64, bool), FontRecord>;
static FACE_REQUESTS: LazyLock<Mutex<FaceRequests>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));
/// The installed faces, read from the catalog on first need.
static INSTALLED: Mutex<Vec<FontRecord>> = Mutex::new(vec![]);
static DISCOVERED: AtomicBool = AtomicBool::new(false);
/// System faces already loaded, by (path, index, bold), to load each once;
/// -1 for one that failed to load.
static LOADED_SYSTEM: crate::memory::SharedCache<(String, i64, bool), i64> =
    LazyLock::new(|| Mutex::new(crate::memory::ByteCache::new(2 * 1024 * 1024)));
/// Cached fallback answers, by primary face index and the characters asked
/// for; -1 when no face covers them.
static FALLBACK: crate::memory::SharedCache<(i64, Vec<char>), i64> =
    LazyLock::new(|| Mutex::new(crate::memory::ByteCache::new(2 * 1024 * 1024)));
// Whether installed fonts may supply named families and missing glyphs.
static SYSTEM_FONTS: AtomicBool = AtomicBool::new(true);

// Counts changes to the faces text may be drawn with, so layouts kept
// outside the shared caches know to lay out again.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The font registry's generation: it changes whenever a face or the
/// installed catalog arrives or leaves, which can change any fallback.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

// Every fallback answer may differ once the faces change.
fn faces_changed() {
    FALLBACK.lock().clear();
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn purge_fallback_cache() {
    faces_changed();
}

fn ensure_bundled() {
    BUNDLED.get_or_init(|| {
        for (index, encoded) in [SANS, SANS_BOLD, MONO, MONO_BOLD].iter().enumerate() {
            let mut face =
                Face::parse_bytes(FontBytes::Static(encoded), 0).expect("bundled fonts parse");
            face.slot = index as i64;
            FACES.lock().insert(index as i64, face);
        }
    });
}

/// The face `id` names.
pub fn face(id: FaceId) -> Face {
    ensure_bundled();
    let i: i64 = id.into();
    face_at(i)
}

fn face_at(i: i64) -> Face {
    FACES.lock().get(&i).cloned().unwrap_or_default()
}

pub const REGISTRY_BYTE_LIMIT: usize = 128 * 1024 * 1024;
pub const FONT_BYTE_LIMIT: usize = 32 * 1024 * 1024;

#[cfg(test)]
fn add_face(f: &Face) -> i64 {
    add_face_with_lifetime(f, true)
}

fn add_face_with_lifetime(f: &Face, permanent: bool) -> i64 {
    ensure_bundled();
    let mut faces = FACES.lock();
    let mut candidate = f.clone();
    for (&index, face) in faces.iter() {
        candidate.slot = face.slot;
        if face == &candidate {
            if permanent {
                AUTO_FACES.lock().remove(&index);
            }
            return index;
        }
    }
    if f.owned_bytes() > FONT_BYTE_LIMIT
        || faces_bytes(faces.values().chain([f])) > REGISTRY_BYTE_LIMIT
    {
        return -1;
    }
    let Ok(index) =
        NEXT_FACE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
    else {
        return -1;
    };
    let mut registered = f.clone();
    registered.slot = index;
    faces.insert(index, registered);
    if !permanent {
        AUTO_FACES.lock().insert(index);
    }
    drop(faces);
    if f.has_color_bitmaps() {
        COLOR_FACES.lock().insert(index);
    }
    index
}

pub fn unregister(ids: &[FaceId]) {
    let mut faces = FACES.lock();
    for id in ids {
        if id.0 >= 4 {
            faces.remove(&id.0);
            COLOR_FACES.lock().remove(&id.0);
            AUTO_FACES.lock().remove(&id.0);
        }
    }
    drop(faces);
    faces_changed();
    LOADED_SYSTEM.lock().clear();
}

/// Releases session-loaded faces while preserving explicit registrations and
/// stable ids. Removed ids are never reused for a different font.
pub(crate) fn end_session() {
    let mut faces = FACES.lock();
    for id in AUTO_FACES.lock().drain() {
        faces.remove(&id);
        COLOR_FACES.lock().remove(&id);
    }
    INSTANCES
        .lock()
        .retain(|name| faces.values().any(|face| face.family == *name));
    drop(faces);
    *INSTALLED.lock() = Vec::new();
    DISCOVERED.store(false, Ordering::Release);
    CATALOG_PENDING.store(false, Ordering::Release);
    CATALOG_REQUESTED.store(false, Ordering::Release);
    *FACE_REQUESTS.lock() = BTreeMap::new();
    LOADED_SYSTEM.lock().clear();
    faces_changed();
}

pub(crate) fn take_catalog_request() -> bool {
    CATALOG_REQUESTED.swap(false, Ordering::AcqRel) && !installed_known()
}
pub(crate) fn catalog_failed() {
    CATALOG_PENDING.store(false, Ordering::Release);
}
pub(crate) fn take_face_requests() -> Vec<(FontRecord, bool)> {
    std::mem::take(&mut *FACE_REQUESTS.lock())
        .into_iter()
        .map(|((_, _, bold), record)| (record, bold))
        .collect()
}

// The heap `faces` hold, counting a file several of them share once.
fn faces_bytes<'a>(faces: impl IntoIterator<Item = &'a Face>) -> usize {
    let mut files = HashSet::new();
    faces
        .into_iter()
        .map(|f| match f.shared_file() {
            Some((at, len)) if !files.insert(at) => f.owned_bytes() - len,
            _ => f.owned_bytes(),
        })
        .sum()
}

pub fn registry_bytes() -> (usize, usize) {
    (faces_bytes(FACES.lock().values()), REGISTRY_BYTE_LIMIT)
}

/// The primary face for a family and weight.
pub fn primary(family: &Family, bold: bool) -> FaceId {
    ensure_bundled();
    let base = match family {
        Family::Sans => 0,
        Family::Monospace => 2,
        Family::Named(name) => match find_named(name, bold) {
            Some(i) => return i.into(),
            None => match load_named(name, bold) {
                Some(i) => return i.into(),
                None => 0,
            },
        },
    };
    (base + if bold { 1 } else { 0 }).into()
}

/// Whether the face draws color bitmaps instead of outlines.
pub fn is_color(id: FaceId) -> bool {
    let i: i64 = id.into();
    COLOR_FACES.lock().contains(&i)
}

/// The face to draw `c` with; see `face_for_cluster`.
pub fn face_for(primary: FaceId, c: char) -> FaceId {
    face_for_cluster(primary, &[c])
}

/// The face to draw a grapheme cluster with, so a base and its marks come
/// from one face: the primary face when it covers every character, then a
/// loaded face that does (bundled and registered first), then the best
/// installed one. A cluster shown as emoji (an emoji-presentation character,
/// or one asked for with U+FE0F) takes a color face first. Failing all, the
/// face for the base alone; the primary face when nothing covers it, which
/// draws the missing glyph.
pub fn face_for_cluster(primary: FaceId, cluster: &[char]) -> FaceId {
    ensure_bundled();
    let p: i64 = primary.into();
    let pf = face_at(p);
    let shown = cluster
        .iter()
        .filter(|c| !is_default_ignorable(**c))
        .cloned()
        .collect::<Vec<_>>();
    let emoji = shown_as_emoji(cluster);
    if (!emoji || is_color(primary)) && shown.iter().all(|c| pf.covers(*c)) {
        return primary;
    }
    let key = (p, cluster.to_vec());
    let cached = FALLBACK.lock().get(&key).copied();
    if let Some(i) = cached {
        return if i >= 0 {
            i.into()
        } else {
            base_face(primary, &shown)
        };
    }
    let mut found = -1;
    if emoji {
        found = covering_face(&shown, true);
        if found < 0
            && let Some(r) = fallback_record(&pf, &shown, true)
            && let Some(loaded) = load_installed(&r, pf.bold)
        {
            found = loaded;
        }
        if found < 0 && shown.iter().all(|c| pf.covers(*c)) {
            found = p;
        }
    }
    if found < 0 {
        found = covering_face(&shown, false);
    }
    if found < 0
        && let Some(r) = fallback_record(&pf, &shown, false)
        && let Some(loaded) = load_installed(&r, pf.bold)
    {
        found = loaded;
    }
    let bytes = key.1.capacity() * size_of::<char>();
    FALLBACK.lock().insert(key, found, bytes);
    if found >= 0 {
        found.into()
    } else {
        base_face(primary, &shown)
    }
}

// The first loaded face covering every character of `chars`; only color
// faces when `color`. -1 when none does.
fn covering_face(chars: &[char], color: bool) -> i64 {
    let faces = FACES.lock();
    for (&id, face) in faces.iter() {
        if color && !COLOR_FACES.lock().contains(&id) {
            continue;
        }
        if chars.iter().all(|c| face.covers(*c)) {
            return id;
        }
    }
    -1
}

// Whether a cluster is shown as emoji: U+FE0F asks for it, U+FE0E asks for
// text, and otherwise an emoji-presentation first character takes it.
fn shown_as_emoji(cluster: &[char]) -> bool {
    if cluster.contains(&'\u{FE0F}') {
        return true;
    };
    if cluster.contains(&'\u{FE0E}') || cluster.is_empty() {
        return false;
    };
    in_ranges(&EMOJI_PRESENTATION, cluster[0] as i64)
}

// Whether `code` falls in a `start, end, value` table.
fn in_ranges(table: &[i64], code: i64) -> bool {
    let mut lo = 0;
    let mut hi = table.len() / 3;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if code < table[mid * 3] {
            hi = mid;
        } else if code > table[mid * 3 + 1] {
            lo = mid + 1;
        } else {
            return true;
        }
    }
    false
}

// The face for a cluster's base when no face covers the whole cluster.
fn base_face(primary: FaceId, shown: &[char]) -> FaceId {
    if shown.len() > 1 {
        face_for_cluster(primary, &[shown[0]])
    } else {
        primary
    }
}

/// Whether `c` is a joiner or variation selector: fonts may use it in
/// substitutions, but it never shows.
pub fn is_default_ignorable(c: char) -> bool {
    let v = c as i64;
    v == 0x200b
        || v == 0x200c
        || v == 0x200d
        || v == 0xfeff
        || (0xfe00..=0xfe0f).contains(&v)
        || v == 0x2060
}

/// Registers font bytes (a TTF/OTF/TTC file's contents) and answers the
/// ids of the faces it held.
pub fn register(data: &[u8]) -> Result<Vec<FaceId>, errors::Error> {
    register_bytes(FontBytes::owned(data.to_vec()))
}

/// Registers a font file built into the program, read where it lies.
pub fn register_static(data: &'static [u8]) -> Result<Vec<FaceId>, errors::Error> {
    register_bytes(FontBytes::Static(data))
}

// Every face of a collection shares the one copy of its file.
fn register_bytes(data: FontBytes) -> Result<Vec<FaceId>, errors::Error> {
    ensure_bundled();
    if data.len() > FONT_BYTE_LIMIT {
        return Err(errors::new("font exceeds its byte budget"));
    }
    let count = Face::count(&data);
    if count > 4096 {
        return Err(errors::new("font collection exceeds its byte budget"));
    }
    // Parse and validate the entire transaction before publishing any face.
    let mut parsed = Vec::new();
    for index in 0..count {
        let face = Face::parse_bytes(data.clone(), index)?;
        if face.owned_bytes() > FONT_BYTE_LIMIT
            || faces_bytes(parsed.iter().chain([&face])) > REGISTRY_BYTE_LIMIT
        {
            return Err(errors::new("font collection exceeds its byte budget"));
        }
        parsed.push(face);
    }
    let mut faces = FACES.lock();
    let mut pending = BTreeMap::<i64, Face>::new();
    let mut ids = Vec::new();
    for mut candidate in parsed {
        let existing = faces.iter().chain(pending.iter()).find_map(|(&id, face)| {
            candidate.slot = face.slot;
            (face == &candidate).then_some(id)
        });
        let id = if let Some(id) = existing {
            id
        } else {
            let used = faces_bytes(faces.values().chain(pending.values()).chain([&candidate]));
            if used > REGISTRY_BYTE_LIMIT {
                return Err(errors::new("font registry exceeds its byte budget"));
            }
            let id = NEXT_FACE
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .map_err(|_| errors::new("font identity space exhausted"))?;
            candidate.slot = id;
            pending.insert(id, candidate);
            id
        };
        ids.push(FaceId(id));
    }
    let mut colors = COLOR_FACES.lock();
    for (&id, face) in &pending {
        if face.has_color_bitmaps() {
            colors.insert(id);
        }
    }
    faces.append(&mut pending);
    let mut automatic = AUTO_FACES.lock();
    for id in &ids {
        automatic.remove(&id.0);
    }
    drop(automatic);
    drop(colors);
    drop(faces);
    faces_changed();
    Ok(ids)
}

/// Directories searched for system fonts on this platform.
#[cfg(target_os = "linux")]
pub fn system_font_dirs() -> Vec<String> {
    let home = env::var("HOME").unwrap_or("".to_string());
    let mut dirs = vec![
        "/usr/share/fonts".to_string(),
        "/usr/local/share/fonts".to_string(),
    ];
    if !home.is_empty() {
        dirs.push(format!("{}/.local/share/fonts", home));
        dirs.push(format!("{}/.fonts", home))
    }
    dirs
}

/// Directories searched for system fonts on this platform.
#[cfg(target_os = "macos")]
pub fn system_font_dirs() -> Vec<String> {
    let home = env::var("HOME").unwrap_or_default();
    let mut dirs = vec![
        "/System/Library/Fonts".to_string(),
        "/Library/Fonts".to_string(),
    ];
    if !home.is_empty() {
        dirs.push(format!("{}/Library/Fonts", home))
    };
    dirs
}

/// Directories searched for system fonts on this platform.
#[cfg(target_os = "windows")]
pub fn system_font_dirs() -> Vec<String> {
    let root = env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
    let mut dirs = vec![format!("{}\\Fonts", root)];
    let local = env::var("LOCALAPPDATA").unwrap_or_default();
    if !local.is_empty() {
        dirs.push(format!("{}\\Microsoft\\Windows\\Fonts", local))
    };
    dirs
}

/// Keeps every face to the bundled fonts (`false`), so text renders the same
/// on every machine, as screenshot tests need. Call it before any text is laid
/// out; installed fonts already loaded stay loaded.
pub fn use_system_fonts(enabled: bool) {
    SYSTEM_FONTS.store(enabled, Ordering::Relaxed);
}

// Summarises the installed faces once (see `catalog`). Symlinked directories
// are not followed, so a link cycle cannot trap the walk; font files reached
// through links are kept.
fn ensure_installed() {
    if installed_known() {
        return;
    }
    if !CATALOG_PENDING.swap(true, Ordering::AcqRel) {
        CATALOG_REQUESTED.store(true, Ordering::Release);
    }
}

/// Whether the installed-font catalog is loaded (or system fonts are off), so
/// asking for it costs nothing.
pub fn installed_known() -> bool {
    DISCOVERED.load(Ordering::Relaxed) || !SYSTEM_FONTS.load(Ordering::Relaxed)
}

/// The installed-font catalog read from the system font folders and the
/// catalog cache. It touches no registry state, so a worker can build it;
/// `adopt_installed` hands it to the registry.
pub fn scan_installed() -> Vec<FontRecord> {
    scan_installed_cancellable(&|| true, &mut |_, _| {}).unwrap_or_default()
}

pub fn scan_installed_cancellable(
    active: &dyn Fn() -> bool,
    progress: &mut impl FnMut(i64, i64),
) -> Result<Vec<FontRecord>, String> {
    let mut files = Vec::new();
    let mut bytes = 0usize;
    crate::services::bounded_io::walk(&system_font_dirs(), active, |entry| {
        let path = entry.path().to_string_lossy().into_owned();
        let lower = path.to_lowercase();
        if [".ttf", ".otf", ".ttc", ".otc"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
        {
            bytes += path.capacity() + size_of::<String>() * 2;
            if bytes > 2 * 1024 * 1024 {
                return Err("font paths exceed their byte budget".into());
            }
            files.push(path);
        }
        Ok(())
    })?;
    files.sort();
    catalog::load_cancellable(&files, &catalog::cache_file(), active, progress)
}

/// Installs a catalog `scan_installed` built elsewhere, unless one is loaded.
pub fn adopt_installed(records: &[FontRecord]) {
    if installed_known() {
        return;
    };
    if records.iter().map(FontRecord::owned_bytes).sum::<usize>() > catalog::CATALOG_BYTE_LIMIT {
        return;
    }
    DISCOVERED.store(true, Ordering::Release);
    CATALOG_PENDING.store(false, Ordering::Release);
    CATALOG_REQUESTED.store(false, Ordering::Release);
    *INSTALLED.lock() = records.to_vec();
    // Characters no loaded face covered may now have an installed one.
    faces_changed();
}

/// Family names of the faces loaded so far and of the installed catalog when
/// it is known, sorted; never scans the system.
pub fn known_families() -> Vec<String> {
    ensure_bundled();
    let mut names = HashSet::new();
    for f in FACES.lock().values() {
        if !f.family.is_empty() && !INSTANCES.lock().contains(&f.family) {
            names.insert(f.family.clone());
        }
    }
    for r in INSTALLED.lock().iter() {
        if !r.family.is_empty() {
            names.insert(r.family.clone());
        }
    }
    let mut out: Vec<String> = names.into_iter().collect();
    out.sort();
    out
}

// Derived family names of registered variation instances.
static INSTANCES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

// Family `name`'s upright face: a loaded one, or one loaded from the
// installed catalog when that is known (never scanning for it).
fn upright(name: &str) -> Option<i64> {
    match find_named(name, false) {
        Some(i) => Some(i),
        None => {
            if installed_known() {
                load_named(name, false)
            } else {
                None
            }
        }
    }
}

/// Registers the face in `data` as family `name` (tests that need a face the
/// bundled set lacks, such as a variable one).
#[cfg(test)]
pub fn register_test_family(name: &str, data: &[u8]) {
    ensure_bundled();
    let mut f = Face::parse(data, 0).unwrap();
    f.family = name.to_string();
    let _ = add_face(&f);
}

/// Whether family `name` is loaded, or in the installed catalog when that is
/// known; unlike `family_available` it never scans the system.
pub fn family_known(name: &str) -> bool {
    ensure_bundled();
    if find_named(name, false).is_some() || find_named(name, true).is_some() {
        return true;
    };
    let want = name.to_lowercase();
    INSTALLED
        .lock()
        .iter()
        .any(|r| r.family.to_lowercase() == want)
}

/// The variation axes of family `name`'s upright face; empty for a static
/// or unknown family.
pub fn family_axes(name: &str) -> Vec<variation::Axis> {
    ensure_bundled();
    match upright(name) {
        Some(i) => variation::axes(&face_at(i)),
        None => vec![],
    }
}

/// A family name that draws `name` at `values` (axis tag to value), with
/// that instance registered once; `name` itself when `values` is empty or the
/// family is unknown.
pub fn instance_family(name: &str, values: &HashMap<String, f64>) -> String {
    if values.is_empty() {
        return name.to_string();
    };
    let mut tags: Vec<&String> = values.keys().collect();
    tags.sort();
    let derived = format!(
        "{} ({})",
        name,
        tags.iter()
            .map(|t| format!("{} {}", t, values[*t]))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if INSTANCES.lock().contains(&derived) {
        return derived;
    };
    let Some(i) = upright(name) else {
        return name.to_string();
    };
    let mut f = face_at(i).instance(values);
    f.family = derived.clone();
    // Looked up as an upright family; its weight is in its coordinates.
    f.bold = false;
    let permanent = !AUTO_FACES.lock().contains(&i);
    if add_face_with_lifetime(&f, permanent) < 0 {
        return name.to_string();
    }
    INSTANCES.lock().insert(derived.clone());
    derived
}

// The installed face to fall back to for `chars`, which must all be covered:
// one styled like `primary` (weight, slant, spacing), then a regional variant
// matching the locale, then sans and Noto families; ties keep catalog order.
// Only color faces when `color`.
fn fallback_record(primary: &Face, chars: &[char], color: bool) -> Option<FontRecord> {
    ensure_installed();
    let region = locale_region();
    let mut best: Option<FontRecord> = None;
    let mut best_rank = 0;
    for r in INSTALLED.lock().iter() {
        if (color && !r.color) || !chars.iter().all(|c| r.covers(*c)) {
            continue;
        };
        let rank = fallback_rank(r, primary, &region);
        if best.is_none() || rank < best_rank {
            best = Some(r.clone());
            best_rank = rank;
        }
    }
    best
}

fn fallback_rank(r: &FontRecord, primary: &Face, region: &str) -> i64 {
    let family = r.family.to_lowercase();
    let mut rank = 0;
    if r.bold != primary.bold {
        rank += 16
    };
    if r.italic != primary.italic {
        rank += 16
    };
    if r.monospace != primary.monospace {
        rank += 8
    };
    let suffix = family.split(" ").last().unwrap_or("");
    if !region.is_empty() && REGIONS.contains(&suffix) && suffix != region {
        rank += 4
    };
    if !family.contains("sans") {
        rank += 2
    };
    if !family.starts_with("noto") {
        rank += 1
    };
    rank
}

// Regional variants of the CJK families, by the family name's last word.
const REGIONS: [&str; 5] = ["jp", "kr", "sc", "tc", "hk"];

// The CJK region the user's locale prefers, as a family name suffix.
fn locale_region() -> String {
    let mut locale = env::var("LC_ALL").unwrap_or("".to_string());
    if locale.is_empty() {
        locale = env::var("LC_CTYPE").unwrap_or("".to_string())
    };
    if locale.is_empty() {
        locale = env::var("LANG").unwrap_or("".to_string())
    };
    let lower = locale.to_lowercase();
    if lower.starts_with("ja") {
        return "jp".to_string();
    };
    if lower.starts_with("ko") {
        return "kr".to_string();
    };
    if lower.starts_with("zh_tw") || lower.starts_with("zh-tw") {
        return "tc".to_string();
    };
    if lower.starts_with("zh_hk") || lower.starts_with("zh-hk") {
        return "hk".to_string();
    };
    if lower.starts_with("zh") {
        return "sc".to_string();
    };
    "".to_string()
}

// Loads one installed face once per weight; a variable face comes at the
// weight asked for.
fn load_installed(r: &FontRecord, bold: bool) -> Option<i64> {
    let key = (r.path.clone(), r.index, bold);
    if let Some(&loaded) = LOADED_SYSTEM.lock().get(&key) {
        return (loaded >= 0).then_some(loaded);
    }
    let mut requests = FACE_REQUESTS.lock();
    let bytes: usize = requests.values().map(FontRecord::owned_bytes).sum();
    if bytes.saturating_add(r.owned_bytes()) <= 2 * 1024 * 1024 {
        requests.entry(key).or_insert_with(|| r.clone());
    }
    None
}

pub(crate) fn prepare_installed(
    r: &FontRecord,
    bold: bool,
    active: &dyn Fn() -> bool,
) -> Result<i64, String> {
    let key = (r.path.clone(), r.index, bold);
    let result = (|| {
        let data = crate::services::bounded_io::read(
            std::path::Path::new(&r.path),
            FONT_BYTE_LIMIT,
            active,
        )?;
        if !active() {
            return Err("font load cancelled or expired".into());
        }
        let face = Face::parse_bytes(FontBytes::owned(data), r.index).map_err(|e| e.to_string())?;
        if !active() {
            return Err("font load cancelled or expired".into());
        }
        let id = add_face_with_lifetime(&at_weight(face, bold), false);
        if id < 0 {
            return Err("font registry exceeds its byte budget".into());
        }
        Ok(id)
    })();
    if active() {
        LOADED_SYSTEM
            .lock()
            .insert(key, result.as_ref().copied().unwrap_or(-1), r.path.len());
    }
    result
}

// A variable face at regular (400) or bold (700) weight, within what its
// `wght` axis allows; a static face as it is.
fn at_weight(face: Face, bold: bool) -> Face {
    let axes = variation::axes(&face);
    let Some(axis) = axes.iter().find(|a| a.tag == "wght") else {
        return face;
    };
    let weight = (if bold { 700.0_f64 } else { 400.0 })
        .max(axis.min)
        .min(axis.max);
    let mut out = face.instance(&crate::hmap! { "wght" => weight });
    out.bold = weight >= 600.0;
    out
}

fn find_named(name: &str, bold: bool) -> Option<i64> {
    let want = name.to_lowercase();
    FACES.lock().iter().find_map(|(&id, face)| {
        (face.family.to_lowercase() == want && face.bold == bold).then_some(id)
    })
}

/// Family names of the bundled and installed faces, sorted, for font pickers.
pub fn available_families() -> Vec<String> {
    ensure_bundled();
    ensure_installed();
    let mut names = HashSet::new();
    for f in FACES.lock().values() {
        if !f.family.is_empty() && !INSTANCES.lock().contains(&f.family) {
            names.insert(f.family.clone());
        }
    }
    for r in INSTALLED.lock().iter() {
        if !r.family.is_empty() {
            names.insert(r.family.clone());
        }
    }
    let mut out: Vec<String> = names.into_iter().collect();
    out.sort();
    out
}

// Loads the installed face of family `name` closest to `bold`: an upright
// face of that weight, then any upright face (a variable one takes the
// weight), then italic ones.
fn load_named(name: &str, bold: bool) -> Option<i64> {
    ensure_installed();
    let want = name.to_lowercase();
    let candidates = INSTALLED
        .lock()
        .iter()
        .filter(|r| r.family.to_lowercase() == want)
        .cloned()
        .collect::<Vec<_>>();
    let mut ordered = candidates
        .iter()
        .filter(|r| r.bold == bold && !r.italic)
        .cloned()
        .collect::<Vec<_>>();
    for r in candidates
        .iter()
        .filter(|r| r.bold != bold && !r.italic)
        .cloned()
        .collect::<Vec<_>>()
    {
        ordered.push(r)
    }
    for r in candidates
        .iter()
        .filter(|r| r.italic)
        .cloned()
        .collect::<Vec<_>>()
    {
        ordered.push(r)
    }
    for r in ordered {
        if let Some(loaded) = load_installed(&r, bold) {
            return Some(loaded);
        }
    }
    None
}

/// Whether a family is bundled, registered or installed.
pub fn family_available(name: &str) -> bool {
    if loaded_family(name) {
        return true;
    }
    // Only a family no loaded face has needs the installed catalog.
    ensure_installed();
    let want = name.to_lowercase();
    INSTALLED
        .lock()
        .iter()
        .any(|r| r.family.to_lowercase() == want)
}

// Whether a loaded face, bundled or registered, is of family `name`.
fn loaded_family(name: &str) -> bool {
    ensure_bundled();
    find_named(name, false).is_some() || find_named(name, true).is_some()
}

/// Number of loaded faces, for diagnostics.
pub fn loaded_count() -> i64 {
    ensure_bundled();
    (FACES.lock().len()) as i64
}

#[cfg(test)]
mod tests {
    use super::{face, face_for, face_for_cluster, primary, shown_as_emoji};

    #[test]
    fn bundled_faces_are_read_in_place_and_a_shared_file_is_charged_once() {
        use crate::text::font::{Face, FontBytes};
        let sans = face(primary(&super::Family::Sans, false));
        assert!(sans.shared_file().is_none());
        assert!(sans.owned_bytes() < super::SANS.len());
        let bytes = FontBytes::owned(super::SANS.to_vec());
        let a = Face::parse_bytes(bytes.clone(), 0).unwrap();
        let b = Face::parse_bytes(bytes, 0).unwrap();
        assert_eq!(
            super::faces_bytes([&a, &b]),
            a.owned_bytes() + b.owned_bytes() - super::SANS.len()
        );
    }

    #[test]
    fn a_loaded_family_is_found_without_the_installed_catalog() {
        assert!(super::loaded_family("DejaVu Sans"));
        assert!(!super::loaded_family("No Such Family"));
    }

    #[test]
    fn bundled_faces_parse_with_sane_metrics() {
        let sans = face(primary(&super::Family::Sans, false));
        assert_eq!(sans.family, "DejaVu Sans");
        assert_eq!(sans.units_per_em, 2048);
        assert!(sans.ascent > 0);
        assert!(sans.descent < 0);
        let mono = face(primary(&super::Family::Monospace, true));
        assert!(mono.monospace);
        assert!(mono.bold)
    }

    #[test]
    fn cmap_maps_latin_greek_and_arabic() {
        let sans = face(primary(&super::Family::Sans, false));
        assert!(sans.glyph_for('A') != 0);
        assert!(sans.glyph_for('Ω') != 0);
        assert!(sans.glyph_for('\u{0628}') != 0);
        assert_eq!(sans.glyph_for('\u{10FFFD}'), 0)
    }

    #[test]
    fn outlines_have_contours_and_advances() {
        let sans = face(primary(&super::Family::Sans, false));
        let o = sans.outline(sans.glyph_for('O'));
        assert!(o.path.len() > 20);
        assert!(o.advance > 1000);
        let space = sans.outline(sans.glyph_for(' '));
        assert_eq!(space.path.len(), 0);
        assert!(space.advance > 0)
    }

    #[test]
    fn fallback_keeps_the_primary_for_covered_characters() {
        let p = primary(&super::Family::Monospace, false);
        assert_eq!(face_for(p, 'a'), p)
    }

    #[test]
    fn emoji_presentation_follows_the_character_and_its_selector() {
        assert!(shown_as_emoji(&['\u{1F600}']));
        assert!(!shown_as_emoji(&['\u{2764}']));
        assert!(shown_as_emoji(&['\u{2764}', '\u{FE0F}']));
        assert!(!shown_as_emoji(&['\u{1F600}', '\u{FE0E}']));
        assert!(!shown_as_emoji(&['a']))
    }

    #[test]
    fn a_cluster_takes_one_face_that_covers_its_marks() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let sans = primary(&super::Family::Sans, false);
        let mono = primary(&super::Family::Monospace, false);
        // Mono has `a` but not the Cyrillic titlo; Sans has both.
        assert_eq!(face_for_cluster(mono, &['a', '\u{483}']), sans);
        assert_eq!(face_for_cluster(mono, &['\u{5D0}', '\u{5B7}']), sans);
        assert_eq!(face_for_cluster(sans, &['a', '\u{301}']), sans);
        // A variation selector is default-ignorable and asks for nothing.
        assert_eq!(face_for_cluster(mono, &['a', '\u{FE0F}']), mono)
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    #[test]
    fn session_loaded_faces_are_disposed_without_reusing_ids_or_removing_registered_faces() {
        let mut automatic = Face::parse(SANS, 0).unwrap();
        automatic.family = "Temporary session family".into();
        let temporary = add_face_with_lifetime(&automatic, false);
        assert!(temporary >= 4);
        let mut permanent = automatic.clone();
        permanent.family = "Explicit family".into();
        let explicit = add_face_with_lifetime(&permanent, true);
        end_session();
        assert!(!FACES.lock().contains_key(&temporary));
        assert!(FACES.lock().contains_key(&explicit));
        let replacement = add_face_with_lifetime(&automatic, false);
        assert!(replacement > temporary && replacement > explicit);
        assert!(registry_bytes().0 <= registry_bytes().1);
        unregister(&[FaceId(explicit), FaceId(replacement)]);
    }
}

#[cfg(test)]
mod fallback_lock_tests {
    use super::*;
    #[test]
    fn repeated_missing_joined_clusters_can_reenter_base_fallback_without_holding_the_cache_lock() {
        let primary = primary(&Family::Monospace, false);
        let missing = ['\u{10fffd}', '\u{200d}', '\u{10fffc}'];
        let first = face_for_cluster(primary, &missing);
        let second = face_for_cluster(primary, &missing);
        assert_eq!(first, second);
    }
}
