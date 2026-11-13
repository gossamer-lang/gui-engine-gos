// The platform's own font service, asked where one family's files are. It
// answers at once, where the installed fonts' catalog is read in the
// background, so a family can be drawn from the first frame.

/// The files the platform's font service holds family `name` in; empty when
/// it has none or no service answers. A file may hold other families too.
pub fn family_files(name: &str) -> Vec<String> {
    if name.is_empty() || name.contains('\0') {
        return Vec::new();
    }
    platform::family_files(name)
}

// Fontconfig, opened at run time, so a system without it still runs.
#[cfg(free_unix)]
mod platform {
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::sync::OnceLock;

    type Pattern = c_void;
    type ObjectSet = c_void;

    #[repr(C)]
    struct FontSet {
        nfont: c_int,
        sfont: c_int,
        fonts: *mut *mut Pattern,
    }

    const RESULT_MATCH: c_int = 0;

    struct Fontconfig {
        // Kept loaded for the process: fontconfig keeps global state.
        _library: libloading::Library,
        pattern_create: unsafe extern "C" fn() -> *mut Pattern,
        pattern_add_string: unsafe extern "C" fn(*mut Pattern, *const c_char, *const u8) -> c_int,
        pattern_get_string:
            unsafe extern "C" fn(*mut Pattern, *const c_char, c_int, *mut *const u8) -> c_int,
        pattern_destroy: unsafe extern "C" fn(*mut Pattern),
        object_set_create: unsafe extern "C" fn() -> *mut ObjectSet,
        object_set_add: unsafe extern "C" fn(*mut ObjectSet, *const c_char) -> c_int,
        object_set_destroy: unsafe extern "C" fn(*mut ObjectSet),
        font_list: unsafe extern "C" fn(*mut c_void, *mut Pattern, *mut ObjectSet) -> *mut FontSet,
        font_set_destroy: unsafe extern "C" fn(*mut FontSet),
    }

    // SAFETY: the function pointers are plain C entry points, and fontconfig
    // serializes access to its shared configuration itself.
    unsafe impl Send for Fontconfig {}
    // SAFETY: as above.
    unsafe impl Sync for Fontconfig {}

    fn fontconfig() -> Option<&'static Fontconfig> {
        static FONTCONFIG: OnceLock<Option<Fontconfig>> = OnceLock::new();
        FONTCONFIG.get_or_init(open).as_ref()
    }

    fn open() -> Option<Fontconfig> {
        // SAFETY: loading fontconfig runs only its own initializers.
        let library = ["libfontconfig.so.1", "libfontconfig.so"]
            .iter()
            .find_map(|&name| unsafe { libloading::Library::new(name) }.ok())?;
        // SAFETY: each symbol has this C signature in every fontconfig 2.x.
        unsafe {
            Some(Fontconfig {
                pattern_create: *library.get(b"FcPatternCreate\0").ok()?,
                pattern_add_string: *library.get(b"FcPatternAddString\0").ok()?,
                pattern_get_string: *library.get(b"FcPatternGetString\0").ok()?,
                pattern_destroy: *library.get(b"FcPatternDestroy\0").ok()?,
                object_set_create: *library.get(b"FcObjectSetCreate\0").ok()?,
                object_set_add: *library.get(b"FcObjectSetAdd\0").ok()?,
                object_set_destroy: *library.get(b"FcObjectSetDestroy\0").ok()?,
                font_list: *library.get(b"FcFontList\0").ok()?,
                font_set_destroy: *library.get(b"FcFontSetDestroy\0").ok()?,
                _library: library,
            })
        }
    }

    pub(super) fn family_files(name: &str) -> Vec<String> {
        let Some(fc) = fontconfig() else {
            return Vec::new();
        };
        let Ok(family) = CString::new(name) else {
            return Vec::new();
        };
        let mut files = Vec::new();
        // SAFETY: every object is created, used and destroyed here, and the
        // strings fontconfig answers live until their font set is destroyed.
        unsafe {
            let pattern = (fc.pattern_create)();
            let objects = (fc.object_set_create)();
            if !pattern.is_null() && !objects.is_null() {
                (fc.pattern_add_string)(pattern, c"family".as_ptr(), family.as_ptr().cast());
                (fc.object_set_add)(objects, c"file".as_ptr());
                let set = (fc.font_list)(std::ptr::null_mut(), pattern, objects);
                if !set.is_null() {
                    let count = usize::try_from((*set).nfont).unwrap_or(0);
                    for i in 0..count {
                        let font = *(*set).fonts.add(i);
                        let mut file: *const u8 = std::ptr::null();
                        if (fc.pattern_get_string)(font, c"file".as_ptr(), 0, &mut file)
                            == RESULT_MATCH
                            && !file.is_null()
                        {
                            let path = CStr::from_ptr(file.cast()).to_string_lossy().into_owned();
                            if !files.contains(&path) {
                                files.push(path);
                            }
                        }
                    }
                    (fc.font_set_destroy)(set);
                }
            }
            if !objects.is_null() {
                (fc.object_set_destroy)(objects);
            }
            if !pattern.is_null() {
                (fc.pattern_destroy)(pattern);
            }
        }
        files
    }
}

// Core Text, opened at run time like the Linux service, so the program's
// link needs no framework of its own for it.
#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    type CfType = *const c_void;
    type CfIndex = isize;

    const UTF8: u32 = 0x0800_0100;

    struct CoreText {
        _foundation: libloading::Library,
        _text: libloading::Library,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
        family_attribute: CfType,
        url_attribute: CfType,
        string_create: unsafe extern "C" fn(CfType, *const u8, CfIndex, u32, u8) -> CfType,
        dictionary_create: unsafe extern "C" fn(
            CfType,
            *const CfType,
            *const CfType,
            CfIndex,
            *const c_void,
            *const c_void,
        ) -> CfType,
        array_count: unsafe extern "C" fn(CfType) -> CfIndex,
        array_at: unsafe extern "C" fn(CfType, CfIndex) -> CfType,
        url_path: unsafe extern "C" fn(CfType, u8, *mut u8, CfIndex) -> u8,
        release: unsafe extern "C" fn(CfType),
        descriptor_create: unsafe extern "C" fn(CfType) -> CfType,
        matching: unsafe extern "C" fn(CfType, CfType) -> CfType,
        copy_attribute: unsafe extern "C" fn(CfType, CfType) -> CfType,
    }

    // SAFETY: Core Foundation objects and Core Text descriptors may be used
    // from any thread, and the attribute keys are immutable constants.
    unsafe impl Send for CoreText {}
    // SAFETY: as above.
    unsafe impl Sync for CoreText {}

    fn core_text() -> Option<&'static CoreText> {
        static CORE_TEXT: OnceLock<Option<CoreText>> = OnceLock::new();
        CORE_TEXT.get_or_init(open).as_ref()
    }

    fn open() -> Option<CoreText> {
        // SAFETY: both are system frameworks every macOS process may load.
        let foundation = unsafe {
            libloading::Library::new(
                "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation",
            )
        }
        .ok()?;
        // SAFETY: as above.
        let text = unsafe {
            libloading::Library::new("/System/Library/Frameworks/CoreText.framework/CoreText")
        }
        .ok()?;
        // SAFETY: each symbol has this C type in the frameworks; the attribute
        // keys are CFStringRef constants and the callbacks are structs, whose
        // addresses are passed on.
        unsafe {
            Some(CoreText {
                key_callbacks: *foundation
                    .get::<*const c_void>(b"kCFTypeDictionaryKeyCallBacks\0")
                    .ok()?,
                value_callbacks: *foundation
                    .get::<*const c_void>(b"kCFTypeDictionaryValueCallBacks\0")
                    .ok()?,
                family_attribute: **text
                    .get::<*const CfType>(b"kCTFontFamilyNameAttribute\0")
                    .ok()?,
                url_attribute: **text.get::<*const CfType>(b"kCTFontURLAttribute\0").ok()?,
                string_create: *foundation.get(b"CFStringCreateWithBytes\0").ok()?,
                dictionary_create: *foundation.get(b"CFDictionaryCreate\0").ok()?,
                array_count: *foundation.get(b"CFArrayGetCount\0").ok()?,
                array_at: *foundation.get(b"CFArrayGetValueAtIndex\0").ok()?,
                url_path: *foundation.get(b"CFURLGetFileSystemRepresentation\0").ok()?,
                release: *foundation.get(b"CFRelease\0").ok()?,
                descriptor_create: *text.get(b"CTFontDescriptorCreateWithAttributes\0").ok()?,
                matching: *text
                    .get(b"CTFontDescriptorCreateMatchingFontDescriptors\0")
                    .ok()?,
                copy_attribute: *text.get(b"CTFontDescriptorCopyAttribute\0").ok()?,
                _foundation: foundation,
                _text: text,
            })
        }
    }

    pub(super) fn family_files(name: &str) -> Vec<String> {
        let Some(ct) = core_text() else {
            return Vec::new();
        };
        let Ok(length) = CfIndex::try_from(name.len()) else {
            return Vec::new();
        };
        let mut files = Vec::new();
        // SAFETY: every object created here is released here; the array and
        // its descriptors are read while the array is alive.
        unsafe {
            let family = (ct.string_create)(std::ptr::null(), name.as_ptr(), length, UTF8, 0);
            if family.is_null() {
                return files;
            }
            let keys = [ct.family_attribute];
            let values = [family];
            let attributes = (ct.dictionary_create)(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                ct.key_callbacks,
                ct.value_callbacks,
            );
            (ct.release)(family);
            if attributes.is_null() {
                return files;
            }
            let descriptor = (ct.descriptor_create)(attributes);
            (ct.release)(attributes);
            if descriptor.is_null() {
                return files;
            }
            let matches = (ct.matching)(descriptor, std::ptr::null());
            (ct.release)(descriptor);
            if matches.is_null() {
                return files;
            }
            for i in 0..(ct.array_count)(matches) {
                let found = (ct.array_at)(matches, i);
                let url = (ct.copy_attribute)(found, ct.url_attribute);
                if url.is_null() {
                    continue;
                }
                let mut buffer = [0u8; 4096];
                if (ct.url_path)(url, 1, buffer.as_mut_ptr(), buffer.len() as CfIndex) != 0
                    && let Ok(path) = std::ffi::CStr::from_bytes_until_nul(&buffer)
                {
                    let path = path.to_string_lossy().into_owned();
                    if !files.contains(&path) {
                        files.push(path);
                    }
                }
                (ct.release)(url);
            }
            (ct.release)(matches);
        }
        files
    }
}

// The fonts Windows registers for every user and for this one, by the names
// it lists them under ("Cascadia Mono Bold (TrueType)").
#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
        RegEnumValueW, RegOpenKeyExW,
    };

    const FONTS_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts";

    pub(super) fn family_files(name: &str) -> Vec<String> {
        let fonts =
            std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string()) + "\\Fonts";
        let mut files = Vec::new();
        for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            for (listed, file) in registered(root) {
                if super::listed_as(&listed, name) {
                    let path = if file.contains('\\') || file.contains('/') {
                        file
                    } else {
                        format!("{fonts}\\{file}")
                    };
                    if !files.contains(&path) {
                        files.push(path);
                    }
                }
            }
        }
        files
    }

    // Every (listed name, file) pair under the fonts key of `root`.
    fn registered(root: HKEY) -> Vec<(String, String)> {
        let key_name: Vec<u16> = FONTS_KEY.encode_utf16().chain([0]).collect();
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: key_name is NUL-terminated and key receives the handle.
        if unsafe { RegOpenKeyExW(root, key_name.as_ptr(), 0, KEY_READ, &mut key) } != ERROR_SUCCESS
        {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut index = 0;
        loop {
            let mut name = [0u16; 512];
            let mut name_len = name.len() as u32;
            let mut data = [0u16; 1024];
            let mut data_len = (data.len() * 2) as u32;
            let mut kind = 0u32;
            // SAFETY: the buffers and their lengths match, and key is open.
            let status = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null(),
                    &mut kind,
                    data.as_mut_ptr().cast(),
                    &mut data_len,
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            index += 1;
            if status != ERROR_SUCCESS || (kind != REG_SZ && kind != REG_EXPAND_SZ) {
                continue;
            }
            let data = &data[..(data_len as usize / 2).min(data.len())];
            let file = String::from_utf16_lossy(data)
                .trim_end_matches('\0')
                .to_string();
            let listed = String::from_utf16_lossy(&name[..name_len as usize]);
            if !file.is_empty() {
                out.push((listed, file));
            }
        }
        // SAFETY: key was opened above.
        unsafe { RegCloseKey(key) };
        out
    }
}

#[cfg(not(any(free_unix, target_os = "macos", windows)))]
mod platform {
    pub(super) fn family_files(_name: &str) -> Vec<String> {
        Vec::new()
    }
}

// Whether a font Windows lists as `listed` ("Cascadia Mono Bold (TrueType)",
// "Cambria & Cambria Math (TrueType)") may hold family `name`: one of its
// names is the family, or the family followed by more words. A listing
// cannot tell a style from a longer family's name, so the faces each file
// holds are matched by family once it is read.
#[cfg(any(windows, test))]
fn listed_as(listed: &str, name: &str) -> bool {
    let listed = match listed.rfind(" (") {
        Some(at) if listed.ends_with(')') => &listed[..at],
        _ => listed,
    };
    let name = name.to_lowercase();
    listed.split(" & ").any(|entry| {
        let entry = entry.trim().to_lowercase();
        entry == name
            || entry
                .strip_prefix(&name)
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

#[cfg(test)]
mod tests {
    use super::{family_files, listed_as};

    #[test]
    fn a_listed_font_holds_its_family_and_its_styles_but_not_longer_families() {
        assert!(listed_as("Cascadia Mono (TrueType)", "Cascadia Mono"));
        assert!(listed_as("Consolas Bold Italic (TrueType)", "consolas"));
        assert!(listed_as(
            "Cambria & Cambria Math (TrueType)",
            "Cambria Math"
        ));
        assert!(!listed_as("Cascadia MonoPL (TrueType)", "Cascadia Mono"));
    }

    #[test]
    fn no_service_holds_an_unnamed_family() {
        assert!(family_files("").is_empty());
        assert!(family_files("a\0b").is_empty());
    }
}
