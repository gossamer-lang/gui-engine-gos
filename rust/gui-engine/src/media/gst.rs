//! GStreamer 1.x, loaded when first needed. Nothing links against it, so an
//! application runs without it and reports media as unavailable.
//!
//! Only GStreamer's stable C ABI is used: functions, the public `GstMessage`
//! type field after its mini-object header, `GstMapInfo` and the appsink
//! callback table.

use std::ffi::{CStr, CString, c_char, c_double, c_int, c_uint, c_void};
use std::sync::LazyLock;

pub type Pointer = *mut c_void;

pub const STATE_NULL: c_int = 1;
pub const STATE_PAUSED: c_int = 3;
pub const STATE_PLAYING: c_int = 4;
pub const FORMAT_TIME: c_int = 3;
pub const SEEK_FLUSH: c_int = 1;
pub const SEEK_ACCURATE: c_int = 2;
pub const MESSAGE_EOS: c_uint = 1 << 0;
pub const MESSAGE_ERROR: c_uint = 1 << 1;
pub const MESSAGE_STATE_CHANGED: c_uint = 1 << 6;
pub const MESSAGE_APPLICATION: c_uint = 1 << 14;
pub const MESSAGE_DURATION_CHANGED: c_uint = 1 << 18;
pub const MESSAGE_ASYNC_DONE: c_uint = 1 << 21;
pub const CLOCK_TIME_NONE: u64 = u64::MAX;
pub const MAP_READ: c_int = 1;
/// The size of `GstMiniObject` on 64-bit targets, where `GstMessage`'s
/// type follows it.
const MINI_OBJECT_SIZE: usize = 64;

#[repr(C)]
pub struct GError {
    pub domain: u32,
    pub code: c_int,
    pub message: *mut c_char,
}

#[repr(C)]
pub struct MapInfo {
    memory: Pointer,
    flags: c_int,
    pub data: *mut u8,
    pub size: usize,
    maxsize: usize,
    user_data: [Pointer; 4],
    reserved: [Pointer; 4],
}

impl Default for MapInfo {
    fn default() -> Self {
        MapInfo {
            memory: std::ptr::null_mut(),
            flags: 0,
            data: std::ptr::null_mut(),
            size: 0,
            maxsize: 0,
            user_data: [std::ptr::null_mut(); 4],
            reserved: [std::ptr::null_mut(); 4],
        }
    }
}

pub type SampleCallback = unsafe extern "C" fn(sink: Pointer, data: Pointer) -> c_int;
pub type EosCallback = unsafe extern "C" fn(sink: Pointer, data: Pointer);
pub type DestroyNotify = unsafe extern "C" fn(data: Pointer);

/// `GstAppSinkCallbacks`, with room for the members later versions added.
#[repr(C)]
pub struct AppSinkCallbacks {
    pub eos: Option<EosCallback>,
    pub new_preroll: Option<SampleCallback>,
    pub new_sample: Option<SampleCallback>,
    pub rest: [Pointer; 8],
}

macro_rules! api {
    ($($name:ident: $ty:ty = $lib:ident . $sym:literal;)*) => {
        /// The GStreamer functions this module calls.
        pub struct Api {
            $(pub $name: $ty,)*
            _libraries: Vec<libloading::Library>,
        }

        fn bind(libraries: Vec<libloading::Library>) -> Result<Api, String> {
            let find = |index: usize, symbol: &[u8]| -> Result<*const c_void, String> {
                // SAFETY: the symbol is looked up by name and only used through
                // the function type the C headers declare for it.
                unsafe {
                    libraries[index]
                        .get::<*const c_void>(symbol)
                        .map(|s| *s)
                        .map_err(|e| format!("GStreamer lacks {}: {e}", String::from_utf8_lossy(symbol)))
                }
            };
            $(
                // SAFETY: the pointer is the named C function, whose signature
                // matches the declared type.
                let $name: $ty = unsafe { std::mem::transmute::<*const c_void, $ty>(find(api!(@lib $lib), $sym.as_bytes())?) };
            )*
            Ok(Api { $($name,)* _libraries: libraries })
        }
    };
    (@lib gst) => { 0 };
    (@lib app) => { 1 };
    (@lib gobject) => { 2 };
    (@lib glib) => { 3 };
}

api! {
    init_check: unsafe extern "C" fn(*mut c_int, *mut c_void, *mut *mut GError) -> c_int = gst."gst_init_check\0";
    factory_make: unsafe extern "C" fn(*const c_char, *const c_char) -> Pointer = gst."gst_element_factory_make\0";
    set_state: unsafe extern "C" fn(Pointer, c_int) -> c_int = gst."gst_element_set_state\0";
    query_position: unsafe extern "C" fn(Pointer, c_int, *mut i64) -> c_int = gst."gst_element_query_position\0";
    query_duration: unsafe extern "C" fn(Pointer, c_int, *mut i64) -> c_int = gst."gst_element_query_duration\0";
    seek_simple: unsafe extern "C" fn(Pointer, c_int, c_int, i64) -> c_int = gst."gst_element_seek_simple\0";
    get_bus: unsafe extern "C" fn(Pointer) -> Pointer = gst."gst_element_get_bus\0";
    bus_pop: unsafe extern "C" fn(Pointer, u64, c_uint) -> Pointer = gst."gst_bus_timed_pop_filtered\0";
    bus_post: unsafe extern "C" fn(Pointer, Pointer) -> c_int = gst."gst_bus_post\0";
    message_new_application: unsafe extern "C" fn(Pointer, Pointer) -> Pointer = gst."gst_message_new_application\0";
    structure_new_empty: unsafe extern "C" fn(*const c_char) -> Pointer = gst."gst_structure_new_empty\0";
    parse_error: unsafe extern "C" fn(Pointer, *mut *mut GError, *mut *mut c_char) = gst."gst_message_parse_error\0";
    parse_state_changed: unsafe extern "C" fn(Pointer, *mut c_int, *mut c_int, *mut c_int) = gst."gst_message_parse_state_changed\0";
    mini_object_unref: unsafe extern "C" fn(Pointer) = gst."gst_mini_object_unref\0";
    object_unref: unsafe extern "C" fn(Pointer) = gst."gst_object_unref\0";
    filename_to_uri: unsafe extern "C" fn(*const c_char, *mut *mut GError) -> *mut c_char = gst."gst_filename_to_uri\0";
    caps_from_string: unsafe extern "C" fn(*const c_char) -> Pointer = gst."gst_caps_from_string\0";
    caps_structure: unsafe extern "C" fn(Pointer, c_uint) -> Pointer = gst."gst_caps_get_structure\0";
    structure_int: unsafe extern "C" fn(Pointer, *const c_char, *mut c_int) -> c_int = gst."gst_structure_get_int\0";
    sample_buffer: unsafe extern "C" fn(Pointer) -> Pointer = gst."gst_sample_get_buffer\0";
    sample_caps: unsafe extern "C" fn(Pointer) -> Pointer = gst."gst_sample_get_caps\0";
    buffer_map: unsafe extern "C" fn(Pointer, *mut MapInfo, c_int) -> c_int = gst."gst_buffer_map\0";
    buffer_unmap: unsafe extern "C" fn(Pointer, *mut MapInfo) = gst."gst_buffer_unmap\0";
    pull_sample: unsafe extern "C" fn(Pointer) -> Pointer = app."gst_app_sink_pull_sample\0";
    pull_preroll: unsafe extern "C" fn(Pointer) -> Pointer = app."gst_app_sink_pull_preroll\0";
    set_callbacks: unsafe extern "C" fn(Pointer, *const AppSinkCallbacks, Pointer, Option<DestroyNotify>) = app."gst_app_sink_set_callbacks\0";
    set_property: unsafe extern "C" fn(Pointer, *const c_char, ...) = gobject."g_object_set\0";
    error_free: unsafe extern "C" fn(*mut GError) = glib."g_error_free\0";
    free: unsafe extern "C" fn(Pointer) = glib."g_free\0";
}

#[cfg(target_os = "linux")]
const LIBRARIES: [&[&str]; 4] = [
    &["libgstreamer-1.0.so.0"],
    &["libgstapp-1.0.so.0"],
    &["libgobject-2.0.so.0"],
    &["libglib-2.0.so.0"],
];
#[cfg(target_os = "macos")]
const LIBRARIES: [&[&str]; 4] = [
    &[
        "/Library/Frameworks/GStreamer.framework/Libraries/libgstreamer-1.0.0.dylib",
        "libgstreamer-1.0.0.dylib",
    ],
    &[
        "/Library/Frameworks/GStreamer.framework/Libraries/libgstapp-1.0.0.dylib",
        "libgstapp-1.0.0.dylib",
    ],
    &[
        "/Library/Frameworks/GStreamer.framework/Libraries/libgobject-2.0.0.dylib",
        "libgobject-2.0.0.dylib",
    ],
    &[
        "/Library/Frameworks/GStreamer.framework/Libraries/libglib-2.0.0.dylib",
        "libglib-2.0.0.dylib",
    ],
];
#[cfg(windows)]
const LIBRARIES: [&[&str]; 4] = [
    &["gstreamer-1.0-0.dll"],
    &["gstapp-1.0-0.dll"],
    &["gobject-2.0-0.dll"],
    &["glib-2.0-0.dll"],
];
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
const LIBRARIES: [&[&str]; 4] = [&[], &[], &[], &[]];

fn load() -> Result<Api, String> {
    let mut libraries = Vec::new();
    for names in LIBRARIES {
        let mut failure = String::from("no library name for this platform");
        let found = names.iter().find_map(|name| {
            // SAFETY: loading GStreamer runs its library constructors, which
            // have no preconditions.
            match unsafe { libloading::Library::new(name) } {
                Ok(library) => Some(library),
                Err(error) => {
                    failure = error.to_string();
                    None
                }
            }
        });
        libraries.push(found.ok_or_else(|| format!("GStreamer 1.x is not installed: {failure}"))?);
    }
    let api = bind(libraries)?;
    let mut error: *mut GError = std::ptr::null_mut();
    // SAFETY: null argc/argv is permitted; `error` receives a GError on failure.
    if unsafe { (api.init_check)(std::ptr::null_mut(), std::ptr::null_mut(), &mut error) } == 0 {
        return Err(format!(
            "GStreamer did not start: {}",
            take_error(&api, error)
        ));
    }
    Ok(api)
}

static API: LazyLock<Result<Api, String>> = LazyLock::new(load);

/// The loaded GStreamer, or why it is unavailable.
pub fn api() -> Result<&'static Api, String> {
    API.as_ref().map_err(Clone::clone)
}

/// A GError's message, freeing the error.
pub fn take_error(api: &Api, error: *mut GError) -> String {
    if error.is_null() {
        return "unknown error".into();
    }
    // SAFETY: a non-null GError from GStreamer has a NUL-terminated message.
    let text = unsafe {
        CStr::from_ptr((*error).message)
            .to_string_lossy()
            .into_owned()
    };
    // SAFETY: the error is owned here and freed once.
    unsafe { (api.error_free)(error) };
    text
}

/// The `GstMessageType` of a message.
///
/// # Safety
/// `message` must be a live `GstMessage`.
pub unsafe fn message_type(message: Pointer) -> c_uint {
    // SAFETY: GstMessage starts with a GstMiniObject, then its type.
    unsafe { *message.cast::<u8>().add(MINI_OBJECT_SIZE).cast::<c_uint>() }
}

/// The element that posted a message.
///
/// # Safety
/// `message` must be a live `GstMessage`.
pub unsafe fn message_source(message: Pointer) -> Pointer {
    // SAFETY: after the type (padded to 8 bytes) come the timestamp and src.
    unsafe {
        *message
            .cast::<u8>()
            .add(MINI_OBJECT_SIZE + 16)
            .cast::<Pointer>()
    }
}

pub fn c(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

/// Sets a pointer-valued property.
///
/// # Safety
/// `object` must be a live GObject with a pointer property `name`.
pub unsafe fn set_pointer(api: &Api, object: Pointer, name: &CStr, value: Pointer) {
    // SAFETY: g_object_set takes NULL-terminated name/value pairs.
    unsafe { (api.set_property)(object, name.as_ptr(), value, std::ptr::null::<c_char>()) }
}

/// Sets an integer- or boolean-valued property.
///
/// # Safety
/// `object` must be a live GObject with an int or gboolean property `name`.
pub unsafe fn set_int(api: &Api, object: Pointer, name: &CStr, value: c_int) {
    // SAFETY: as above, with an int value.
    unsafe { (api.set_property)(object, name.as_ptr(), value, std::ptr::null::<c_char>()) }
}

/// Sets a double-valued property.
///
/// # Safety
/// `object` must be a live GObject with a double property `name`.
pub unsafe fn set_double(api: &Api, object: Pointer, name: &CStr, value: c_double) {
    // SAFETY: as above, with a double value.
    unsafe { (api.set_property)(object, name.as_ptr(), value, std::ptr::null::<c_char>()) }
}
