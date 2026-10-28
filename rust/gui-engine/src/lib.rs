//! The GUI toolkit's core: geometry, style, text, rendering, widgets, and the
//! session that runs them on gos-windowing.

/// A process-wide memo table, filled on first use.
pub(crate) type Cache<K, V> =
    std::sync::LazyLock<parking_lot::Mutex<std::collections::HashMap<K, V>>>;

pub mod accessibility;
pub mod app;
pub mod application;
pub mod callback;
pub mod chart;
pub mod commands;
pub mod devtools;
pub mod dialogs;
pub mod dock;
pub mod editing;
pub mod errors;
pub mod geometry;
pub mod imageview;
pub mod input;
pub mod localization;
pub mod media;
pub mod memory;
pub mod models;
pub mod paths;
pub mod print;
pub mod render;
pub mod rich;
pub mod scene;
pub mod services;
pub mod strext;
pub mod style;
pub mod terminal;
pub mod text;
pub mod unicode;
pub mod widgets;

pub use app::effects::perform;
pub use app::{Session, start};
pub use application::{App, Launch, application, launch};
pub use callback::Callback;

/// A `HashMap` from `key => value` pairs, the toolkit's spelling of a map
/// literal.
#[macro_export]
macro_rules! hmap {
    ($($k:expr => $v:expr),* $(,)?) => {{
        #[allow(unused_mut, reason = "an empty literal builds nothing")]
        let mut m = ::std::collections::HashMap::new();
        $( m.insert($k.into(), $v.into()); )*
        m
    }};
}
