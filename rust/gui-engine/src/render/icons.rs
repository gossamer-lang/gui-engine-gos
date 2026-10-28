// icons: the built-in vector icon set, drawn on a 24-unit grid with
// round-capped strokes so they scale to any size and theme, and the icon sets
// applications register on the same grid for a theme to draw from. A custom
// icon no set has draws a neutral box, never nothing.

use crate::render::Canvas;
use crate::render::vector::Path;
use crate::style::color::Color;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// A built-in icon, or one an application registers under a name of its own.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Icon {
    Check,
    Close,
    Plus,
    Minus,
    ChevronDown,
    ChevronUp,
    ChevronLeft,
    ChevronRight,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Search,
    Menu,
    More,
    Folder,
    FolderOpen,
    File,
    FileText,
    Star,
    StarFilled,
    Info,
    Warning,
    Error,
    Eye,
    EyeOff,
    Home,
    Refresh,
    Settings,
    Trash,
    Edit,
    Copy,
    Paste,
    Cut,
    Undo,
    Redo,
    Save,
    Play,
    Pause,
    Stop,
    Dot,
    Calendar,
    Clock,
    User,
    Lock,
    Globe,
    Download,
    Upload,
    Link,
    Bookmark,
    Terminal,
    Code,
    Grid,
    List,
    SortAsc,
    SortDesc,
    Filter,
    Drag,
    External,
    App,
    /// An icon `register` adds under this name; drawn as a neutral box where no
    /// set the theme names has it.
    Custom(String),
}

impl Icon {
    /// The name icon sets know this icon by.
    pub fn name(&self) -> &str {
        match self {
            Icon::Check => "check",
            Icon::Close => "close",
            Icon::Plus => "plus",
            Icon::Minus => "minus",
            Icon::ChevronDown => "chevron-down",
            Icon::ChevronUp => "chevron-up",
            Icon::ChevronLeft => "chevron-left",
            Icon::ChevronRight => "chevron-right",
            Icon::ArrowLeft => "arrow-left",
            Icon::ArrowRight => "arrow-right",
            Icon::ArrowUp => "arrow-up",
            Icon::ArrowDown => "arrow-down",
            Icon::Search => "search",
            Icon::Menu => "menu",
            Icon::More => "more",
            Icon::Folder => "folder",
            Icon::FolderOpen => "folder-open",
            Icon::File => "file",
            Icon::FileText => "file-text",
            Icon::Star => "star",
            Icon::StarFilled => "star-filled",
            Icon::Info => "info",
            Icon::Warning => "warning",
            Icon::Error => "error",
            Icon::Eye => "eye",
            Icon::EyeOff => "eye-off",
            Icon::Home => "home",
            Icon::Refresh => "refresh",
            Icon::Settings => "settings",
            Icon::Trash => "trash",
            Icon::Edit => "edit",
            Icon::Copy => "copy",
            Icon::Paste => "paste",
            Icon::Cut => "cut",
            Icon::Undo => "undo",
            Icon::Redo => "redo",
            Icon::Save => "save",
            Icon::Play => "play",
            Icon::Pause => "pause",
            Icon::Stop => "stop",
            Icon::Dot => "dot",
            Icon::Calendar => "calendar",
            Icon::Clock => "clock",
            Icon::User => "user",
            Icon::Lock => "lock",
            Icon::Globe => "globe",
            Icon::Download => "download",
            Icon::Upload => "upload",
            Icon::Link => "link",
            Icon::Bookmark => "bookmark",
            Icon::Terminal => "terminal",
            Icon::Code => "code",
            Icon::Grid => "grid",
            Icon::List => "list",
            Icon::SortAsc => "sort-asc",
            Icon::SortDesc => "sort-desc",
            Icon::Filter => "filter",
            Icon::Drag => "drag",
            Icon::External => "external",
            Icon::App => "app",
            Icon::Custom(name) => name,
        }
    }

    /// The built-in icon called `name`, for names that arrive as data.
    pub fn named(name: &str) -> Option<Icon> {
        match name {
            "check" => Some(Icon::Check),
            "close" => Some(Icon::Close),
            "plus" => Some(Icon::Plus),
            "minus" => Some(Icon::Minus),
            "chevron-down" => Some(Icon::ChevronDown),
            "chevron-up" => Some(Icon::ChevronUp),
            "chevron-left" => Some(Icon::ChevronLeft),
            "chevron-right" => Some(Icon::ChevronRight),
            "arrow-left" => Some(Icon::ArrowLeft),
            "arrow-right" => Some(Icon::ArrowRight),
            "arrow-up" => Some(Icon::ArrowUp),
            "arrow-down" => Some(Icon::ArrowDown),
            "search" => Some(Icon::Search),
            "menu" => Some(Icon::Menu),
            "more" => Some(Icon::More),
            "folder" => Some(Icon::Folder),
            "folder-open" => Some(Icon::FolderOpen),
            "file" => Some(Icon::File),
            "file-text" => Some(Icon::FileText),
            "star" => Some(Icon::Star),
            "star-filled" => Some(Icon::StarFilled),
            "info" => Some(Icon::Info),
            "warning" => Some(Icon::Warning),
            "error" => Some(Icon::Error),
            "eye" => Some(Icon::Eye),
            "eye-off" => Some(Icon::EyeOff),
            "home" => Some(Icon::Home),
            "refresh" => Some(Icon::Refresh),
            "settings" => Some(Icon::Settings),
            "trash" => Some(Icon::Trash),
            "edit" => Some(Icon::Edit),
            "copy" => Some(Icon::Copy),
            "paste" => Some(Icon::Paste),
            "cut" => Some(Icon::Cut),
            "undo" => Some(Icon::Undo),
            "redo" => Some(Icon::Redo),
            "save" => Some(Icon::Save),
            "play" => Some(Icon::Play),
            "pause" => Some(Icon::Pause),
            "stop" => Some(Icon::Stop),
            "dot" => Some(Icon::Dot),
            "calendar" => Some(Icon::Calendar),
            "clock" => Some(Icon::Clock),
            "user" => Some(Icon::User),
            "lock" => Some(Icon::Lock),
            "globe" => Some(Icon::Globe),
            "download" => Some(Icon::Download),
            "upload" => Some(Icon::Upload),
            "link" => Some(Icon::Link),
            "bookmark" => Some(Icon::Bookmark),
            "terminal" => Some(Icon::Terminal),
            "code" => Some(Icon::Code),
            "grid" => Some(Icon::Grid),
            "list" => Some(Icon::List),
            "sort-asc" => Some(Icon::SortAsc),
            "sort-desc" => Some(Icon::SortDesc),
            "filter" => Some(Icon::Filter),
            "drag" => Some(Icon::Drag),
            "external" => Some(Icon::External),
            "app" => Some(Icon::App),
            _ => None,
        }
    }
}

/// Every built-in icon.
pub const BUILT_IN: [Icon; 60] = [
    Icon::Check,
    Icon::Close,
    Icon::Plus,
    Icon::Minus,
    Icon::ChevronDown,
    Icon::ChevronUp,
    Icon::ChevronLeft,
    Icon::ChevronRight,
    Icon::ArrowLeft,
    Icon::ArrowRight,
    Icon::ArrowUp,
    Icon::ArrowDown,
    Icon::Search,
    Icon::Menu,
    Icon::More,
    Icon::Folder,
    Icon::FolderOpen,
    Icon::File,
    Icon::FileText,
    Icon::Star,
    Icon::StarFilled,
    Icon::Info,
    Icon::Warning,
    Icon::Error,
    Icon::Eye,
    Icon::EyeOff,
    Icon::Home,
    Icon::Refresh,
    Icon::Settings,
    Icon::Trash,
    Icon::Edit,
    Icon::Copy,
    Icon::Paste,
    Icon::Cut,
    Icon::Undo,
    Icon::Redo,
    Icon::Save,
    Icon::Play,
    Icon::Pause,
    Icon::Stop,
    Icon::Dot,
    Icon::Calendar,
    Icon::Clock,
    Icon::User,
    Icon::Lock,
    Icon::Globe,
    Icon::Download,
    Icon::Upload,
    Icon::Link,
    Icon::Bookmark,
    Icon::Terminal,
    Icon::Code,
    Icon::Grid,
    Icon::List,
    Icon::SortAsc,
    Icon::SortDesc,
    Icon::Filter,
    Icon::Drag,
    Icon::External,
    Icon::App,
];

fn seg(p: &mut Path, s: f64, ox: f64, oy: f64, pts: &[f64]) {
    let mut i = 0;
    while i + 1 < pts.len() {
        if i == 0 {
            p.move_to(ox + pts[0] * s, oy + pts[1] * s)
        } else {
            p.line_to(ox + pts[i] * s, oy + pts[i + 1] * s)
        };
        i += 2;
    }
}

fn circle(p: &mut Path, s: f64, ox: f64, oy: f64, cx: f64, cy: f64, r: f64) {
    let e = Path::ellipse(ox + cx * s, oy + cy * s, r * s, r * s);
    for v in e.cmds {
        p.cmds.push(v)
    }
}

fn rect(p: &mut Path, s: f64, ox: f64, oy: f64, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let e = Path::rounded_rect(ox + x * s, oy + y * s, w * s, h * s, r * s);
    for v in e.cmds {
        p.cmds.push(v)
    }
}

/// An application's icon: outlines on the built-in set's 24-unit grid, the
/// `fill` filled (even-odd) and the `stroke` stroked as the built-in icons are.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IconArt {
    pub fill: Path,
    pub stroke: Path,
}

static SETS: LazyLock<Mutex<HashMap<String, HashMap<String, IconArt>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Adds `art` to the icon set `set` as `icon`; a theme whose `icons` names
/// the set draws it in place of the built-in one.
pub const ICON_BYTE_LIMIT: usize = 16 * 1024 * 1024;
pub fn try_register(set: &str, icon: &Icon, art: &IconArt) -> Result<(), String> {
    let cost = art
        .fill
        .cmds
        .len()
        .saturating_add(art.stroke.cmds.len())
        .saturating_mul(size_of::<f64>())
        .saturating_add(set.len() + icon.name().len() + 512);
    let mut sets = SETS.lock();
    let bytes: usize = sets
        .iter()
        .map(|(name, icons)| {
            name.capacity()
                + icons
                    .iter()
                    .map(|(name, art)| {
                        name.capacity()
                            + (art.fill.cmds.capacity() + art.stroke.cmds.capacity())
                                * size_of::<f64>()
                            + 512
                    })
                    .sum::<usize>()
        })
        .sum();
    if bytes.saturating_add(cost) > ICON_BYTE_LIMIT {
        return Err("icon registry exceeds its byte budget".into());
    }
    sets.entry(set.to_owned())
        .or_default()
        .insert(icon.name().to_owned(), art.clone());
    Ok(())
}
pub fn register(set: &str, icon: &Icon, art: &IconArt) {
    if let Err(error) = try_register(set, icon, art) {
        panic!("{error}");
    }
}

pub fn unregister_set(set: &str) {
    SETS.lock().remove(set);
}

/// Draws `icon` from the registered set `set` when it has one, else the
/// built-in icon.
pub fn draw_from(
    canvas: &mut Canvas,
    set: &str,
    icon: &Icon,
    x: i64,
    y: i64,
    size: i64,
    color: Color,
) {
    if !set.is_empty()
        && let Some(icons) = SETS.lock().get(set)
        && let Some(art) = icons.get(icon.name())
    {
        let s = size as f64 / 24.0;
        if !art.fill.cmds.is_empty() {
            let m = crate::render::vector::Mask::fill(
                &art.fill.cmds,
                s,
                0.0,
                0.0,
                s,
                x as f64,
                y as f64,
                crate::render::vector::FillRule::EvenOdd,
            );
            canvas.mask(&m, color)
        }
        if !art.stroke.cmds.is_empty() {
            canvas.stroke_path(
                &art.stroke.scaled(s, x as f64, y as f64),
                (2.0 * s).max(1.25),
                color,
            )
        }
        return;
    }
    draw(canvas, icon, x, y, size, color)
}

/// Draws built-in icon `name` in the `size`-pixel square at (`x`, `y`).
pub fn draw(canvas: &mut Canvas, icon: &Icon, x: i64, y: i64, size: i64, color: Color) {
    let s = size as f64 / 24.0;
    let ox = x as f64;
    let oy = y as f64;
    let width = (2.0 * s).max(1.25);
    let mut stroke = Path::new();
    let mut fill = Path::new();
    match icon {
        Icon::Check => seg(&mut stroke, s, ox, oy, &[5.0, 12.5, 10.0, 17.5, 19.0, 7.0]),
        Icon::Close => {
            seg(&mut stroke, s, ox, oy, &[6.0, 6.0, 18.0, 18.0]);
            seg(&mut stroke, s, ox, oy, &[18.0, 6.0, 6.0, 18.0])
        }
        Icon::Plus => {
            seg(&mut stroke, s, ox, oy, &[12.0, 5.0, 12.0, 19.0]);
            seg(&mut stroke, s, ox, oy, &[5.0, 12.0, 19.0, 12.0])
        }
        Icon::Minus => seg(&mut stroke, s, ox, oy, &[5.0, 12.0, 19.0, 12.0]),
        Icon::ChevronDown => seg(&mut stroke, s, ox, oy, &[6.0, 9.0, 12.0, 15.0, 18.0, 9.0]),
        Icon::ChevronUp => seg(&mut stroke, s, ox, oy, &[6.0, 15.0, 12.0, 9.0, 18.0, 15.0]),
        Icon::ChevronLeft => seg(&mut stroke, s, ox, oy, &[15.0, 6.0, 9.0, 12.0, 15.0, 18.0]),
        Icon::ChevronRight => seg(&mut stroke, s, ox, oy, &[9.0, 6.0, 15.0, 12.0, 9.0, 18.0]),
        Icon::ArrowLeft => {
            seg(&mut stroke, s, ox, oy, &[19.0, 12.0, 5.0, 12.0]);
            seg(&mut stroke, s, ox, oy, &[11.0, 6.0, 5.0, 12.0, 11.0, 18.0])
        }
        Icon::ArrowRight => {
            seg(&mut stroke, s, ox, oy, &[5.0, 12.0, 19.0, 12.0]);
            seg(&mut stroke, s, ox, oy, &[13.0, 6.0, 19.0, 12.0, 13.0, 18.0])
        }
        Icon::ArrowUp => {
            seg(&mut stroke, s, ox, oy, &[12.0, 19.0, 12.0, 5.0]);
            seg(&mut stroke, s, ox, oy, &[6.0, 11.0, 12.0, 5.0, 18.0, 11.0])
        }
        Icon::ArrowDown => {
            seg(&mut stroke, s, ox, oy, &[12.0, 5.0, 12.0, 19.0]);
            seg(&mut stroke, s, ox, oy, &[6.0, 13.0, 12.0, 19.0, 18.0, 13.0])
        }
        Icon::Search => {
            circle(&mut fill, s, ox, oy, 10.5, 10.5, 6.5);
            circle(&mut fill, s, ox, oy, 10.5, 10.5, 4.6);
            seg(&mut stroke, s, ox, oy, &[15.5, 15.5, 20.0, 20.0])
        }
        Icon::Menu => {
            seg(&mut stroke, s, ox, oy, &[4.0, 7.0, 20.0, 7.0]);
            seg(&mut stroke, s, ox, oy, &[4.0, 12.0, 20.0, 12.0]);
            seg(&mut stroke, s, ox, oy, &[4.0, 17.0, 20.0, 17.0])
        }
        Icon::More => {
            circle(&mut fill, s, ox, oy, 6.0, 12.0, 1.8);
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 1.8);
            circle(&mut fill, s, ox, oy, 18.0, 12.0, 1.8)
        }
        Icon::Folder => seg(
            &mut stroke,
            s,
            ox,
            oy,
            &[
                3.0, 7.0, 3.0, 19.0, 21.0, 19.0, 21.0, 9.0, 11.0, 9.0, 9.0, 6.0, 3.0, 6.0, 3.0, 7.0,
            ],
        ),
        Icon::FolderOpen => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    3.0, 19.0, 3.0, 6.0, 9.0, 6.0, 11.0, 9.0, 19.0, 9.0, 19.0, 11.0,
                ],
            );
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[3.0, 19.0, 6.0, 11.0, 22.0, 11.0, 19.0, 19.0, 3.0, 19.0],
            )
        }
        Icon::File => seg(
            &mut stroke,
            s,
            ox,
            oy,
            &[
                6.0, 3.0, 14.0, 3.0, 19.0, 8.0, 19.0, 21.0, 6.0, 21.0, 6.0, 3.0,
            ],
        ),
        Icon::FileText => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    6.0, 3.0, 14.0, 3.0, 19.0, 8.0, 19.0, 21.0, 6.0, 21.0, 6.0, 3.0,
                ],
            );
            seg(&mut stroke, s, ox, oy, &[9.0, 12.0, 16.0, 12.0]);
            seg(&mut stroke, s, ox, oy, &[9.0, 16.0, 16.0, 16.0])
        }
        Icon::Star => seg(
            &mut stroke,
            s,
            ox,
            oy,
            &[
                12.0, 3.0, 14.6, 9.0, 21.0, 9.5, 16.0, 13.8, 17.6, 20.5, 12.0, 17.0, 6.4, 20.5,
                8.0, 13.8, 3.0, 9.5, 9.4, 9.0, 12.0, 3.0,
            ],
        ),
        Icon::StarFilled => {
            seg(
                &mut fill,
                s,
                ox,
                oy,
                &[
                    12.0, 3.0, 14.6, 9.0, 21.0, 9.5, 16.0, 13.8, 17.6, 20.5, 12.0, 17.0, 6.4, 20.5,
                    8.0, 13.8, 3.0, 9.5, 9.4, 9.0, 12.0, 3.0,
                ],
            );
            fill.close()
        }
        Icon::Info => {
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 9.5);
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 7.7);
            seg(&mut stroke, s, ox, oy, &[12.0, 11.0, 12.0, 17.0]);
            circle(&mut fill, s, ox, oy, 12.0, 7.5, 1.2)
        }
        Icon::Warning => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[12.0, 3.5, 21.5, 20.0, 2.5, 20.0, 12.0, 3.5],
            );
            seg(&mut stroke, s, ox, oy, &[12.0, 9.5, 12.0, 14.0]);
            circle(&mut fill, s, ox, oy, 12.0, 17.0, 1.2)
        }
        Icon::Error => {
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 9.5);
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 7.7);
            seg(&mut stroke, s, ox, oy, &[9.0, 9.0, 15.0, 15.0]);
            seg(&mut stroke, s, ox, oy, &[15.0, 9.0, 9.0, 15.0])
        }
        Icon::Eye => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    2.0, 12.0, 6.0, 7.0, 12.0, 5.0, 18.0, 7.0, 22.0, 12.0, 18.0, 17.0, 12.0, 19.0,
                    6.0, 17.0, 2.0, 12.0,
                ],
            );
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 3.0)
        }
        Icon::EyeOff => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    2.0, 12.0, 6.0, 7.0, 12.0, 5.0, 18.0, 7.0, 22.0, 12.0, 18.0, 17.0, 12.0, 19.0,
                    6.0, 17.0, 2.0, 12.0,
                ],
            );
            seg(&mut stroke, s, ox, oy, &[4.0, 4.0, 20.0, 20.0])
        }
        Icon::Home => seg(
            &mut stroke,
            s,
            ox,
            oy,
            &[
                3.0, 11.0, 12.0, 3.0, 21.0, 11.0, 19.0, 11.0, 19.0, 20.0, 5.0, 20.0, 5.0, 11.0,
                3.0, 11.0,
            ],
        ),
        Icon::Refresh => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    19.0, 8.0, 16.5, 5.5, 12.0, 4.0, 7.0, 5.7, 4.5, 10.0, 4.6, 14.5, 7.0, 18.3,
                    12.0, 20.0, 16.5, 18.5, 19.5, 14.5,
                ],
            );
            seg(&mut stroke, s, ox, oy, &[19.0, 3.0, 19.0, 8.0, 14.0, 8.0])
        }
        Icon::Settings => {
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 8.0);
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 3.0);
            seg(&mut stroke, s, ox, oy, &[12.0, 1.5, 12.0, 4.0]);
            seg(&mut stroke, s, ox, oy, &[12.0, 20.0, 12.0, 22.5]);
            seg(&mut stroke, s, ox, oy, &[1.5, 12.0, 4.0, 12.0]);
            seg(&mut stroke, s, ox, oy, &[20.0, 12.0, 22.5, 12.0])
        }
        Icon::Trash => {
            seg(&mut stroke, s, ox, oy, &[4.0, 6.0, 20.0, 6.0]);
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[6.0, 6.0, 7.0, 21.0, 17.0, 21.0, 18.0, 6.0],
            );
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[9.0, 6.0, 9.0, 3.0, 15.0, 3.0, 15.0, 6.0],
            )
        }
        Icon::Edit => seg(
            &mut stroke,
            s,
            ox,
            oy,
            &[
                4.0, 20.0, 5.0, 15.0, 16.0, 4.0, 20.0, 8.0, 9.0, 19.0, 4.0, 20.0,
            ],
        ),
        Icon::Copy => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[8.0, 8.0, 20.0, 8.0, 20.0, 20.0, 8.0, 20.0, 8.0, 8.0],
            );
            seg(&mut stroke, s, ox, oy, &[4.0, 16.0, 4.0, 4.0, 16.0, 4.0])
        }
        Icon::Paste => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    8.0, 4.0, 5.0, 4.0, 5.0, 21.0, 19.0, 21.0, 19.0, 4.0, 16.0, 4.0,
                ],
            );
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[8.0, 3.0, 16.0, 3.0, 16.0, 6.0, 8.0, 6.0, 8.0, 3.0],
            )
        }
        Icon::Cut => {
            circle(&mut fill, s, ox, oy, 6.0, 18.0, 3.0);
            circle(&mut fill, s, ox, oy, 6.0, 18.0, 1.5);
            circle(&mut fill, s, ox, oy, 18.0, 18.0, 3.0);
            circle(&mut fill, s, ox, oy, 18.0, 18.0, 1.5);
            seg(&mut stroke, s, ox, oy, &[8.0, 16.0, 18.0, 3.0]);
            seg(&mut stroke, s, ox, oy, &[16.0, 16.0, 6.0, 3.0])
        }
        Icon::Undo => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    4.0, 10.0, 15.0, 10.0, 18.5, 11.5, 20.0, 15.0, 18.5, 18.5, 15.0, 20.0, 9.0,
                    20.0,
                ],
            );
            seg(&mut stroke, s, ox, oy, &[8.0, 6.0, 4.0, 10.0, 8.0, 14.0])
        }
        Icon::Redo => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    20.0, 10.0, 9.0, 10.0, 5.5, 11.5, 4.0, 15.0, 5.5, 18.5, 9.0, 20.0, 15.0, 20.0,
                ],
            );
            seg(&mut stroke, s, ox, oy, &[16.0, 6.0, 20.0, 10.0, 16.0, 14.0])
        }
        Icon::Save => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    4.0, 4.0, 17.0, 4.0, 20.0, 7.0, 20.0, 20.0, 4.0, 20.0, 4.0, 4.0,
                ],
            );
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[8.0, 4.0, 8.0, 9.0, 15.0, 9.0, 15.0, 4.0],
            );
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[7.0, 20.0, 7.0, 14.0, 17.0, 14.0, 17.0, 20.0],
            )
        }
        Icon::Play => {
            seg(
                &mut fill,
                s,
                ox,
                oy,
                &[7.0, 4.0, 20.0, 12.0, 7.0, 20.0, 7.0, 4.0],
            );
            fill.close()
        }
        Icon::Pause => {
            rect(&mut fill, s, ox, oy, 6.0, 4.0, 4.0, 16.0, 1.0);
            rect(&mut fill, s, ox, oy, 14.0, 4.0, 4.0, 16.0, 1.0)
        }
        Icon::Stop => rect(&mut fill, s, ox, oy, 5.0, 5.0, 14.0, 14.0, 2.0),
        Icon::Dot => circle(&mut fill, s, ox, oy, 12.0, 12.0, 4.0),
        Icon::Calendar => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[4.0, 6.0, 20.0, 6.0, 20.0, 20.0, 4.0, 20.0, 4.0, 6.0],
            );
            seg(&mut stroke, s, ox, oy, &[4.0, 10.0, 20.0, 10.0]);
            seg(&mut stroke, s, ox, oy, &[8.0, 3.0, 8.0, 7.0]);
            seg(&mut stroke, s, ox, oy, &[16.0, 3.0, 16.0, 7.0])
        }
        Icon::Clock => {
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 9.5);
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 7.7);
            seg(&mut stroke, s, ox, oy, &[12.0, 7.0, 12.0, 12.0, 15.5, 14.0])
        }
        Icon::User => {
            circle(&mut fill, s, ox, oy, 12.0, 8.0, 4.0);
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    4.0, 21.0, 5.0, 17.0, 8.0, 14.5, 16.0, 14.5, 19.0, 17.0, 20.0, 21.0,
                ],
            )
        }
        Icon::Lock => {
            rect(&mut fill, s, ox, oy, 5.0, 11.0, 14.0, 10.0, 2.0);
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    8.0, 11.0, 8.0, 7.0, 10.0, 4.0, 14.0, 4.0, 16.0, 7.0, 16.0, 11.0,
                ],
            )
        }
        Icon::Globe => {
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 9.5);
            circle(&mut fill, s, ox, oy, 12.0, 12.0, 7.7);
            seg(&mut stroke, s, ox, oy, &[3.0, 12.0, 21.0, 12.0]);
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    12.0, 2.5, 9.0, 7.0, 8.0, 12.0, 9.0, 17.0, 12.0, 21.5, 15.0, 17.0, 16.0, 12.0,
                    15.0, 7.0, 12.0, 2.5,
                ],
            )
        }
        Icon::Download => {
            seg(&mut stroke, s, ox, oy, &[12.0, 3.0, 12.0, 15.0]);
            seg(&mut stroke, s, ox, oy, &[7.0, 10.0, 12.0, 15.0, 17.0, 10.0]);
            seg(&mut stroke, s, ox, oy, &[4.0, 19.0, 20.0, 19.0])
        }
        Icon::Upload => {
            seg(&mut stroke, s, ox, oy, &[12.0, 15.0, 12.0, 3.0]);
            seg(&mut stroke, s, ox, oy, &[7.0, 8.0, 12.0, 3.0, 17.0, 8.0]);
            seg(&mut stroke, s, ox, oy, &[4.0, 19.0, 20.0, 19.0])
        }
        Icon::Link => {
            seg(&mut stroke, s, ox, oy, &[10.0, 14.0, 14.0, 10.0]);
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    11.0, 7.0, 13.0, 5.0, 17.0, 5.0, 19.0, 7.0, 19.0, 11.0, 17.0, 13.0,
                ],
            );
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[
                    13.0, 17.0, 11.0, 19.0, 7.0, 19.0, 5.0, 17.0, 5.0, 13.0, 7.0, 11.0,
                ],
            )
        }
        Icon::Bookmark => seg(
            &mut stroke,
            s,
            ox,
            oy,
            &[
                6.0, 3.0, 18.0, 3.0, 18.0, 21.0, 12.0, 16.0, 6.0, 21.0, 6.0, 3.0,
            ],
        ),
        Icon::Terminal => {
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[3.0, 4.0, 21.0, 4.0, 21.0, 20.0, 3.0, 20.0, 3.0, 4.0],
            );
            seg(&mut stroke, s, ox, oy, &[7.0, 9.0, 10.0, 12.0, 7.0, 15.0]);
            seg(&mut stroke, s, ox, oy, &[12.0, 15.0, 17.0, 15.0])
        }
        Icon::Code => {
            seg(&mut stroke, s, ox, oy, &[8.0, 7.0, 3.0, 12.0, 8.0, 17.0]);
            seg(&mut stroke, s, ox, oy, &[16.0, 7.0, 21.0, 12.0, 16.0, 17.0])
        }
        Icon::Grid => {
            rect(&mut fill, s, ox, oy, 4.0, 4.0, 7.0, 7.0, 1.0);
            rect(&mut fill, s, ox, oy, 13.0, 4.0, 7.0, 7.0, 1.0);
            rect(&mut fill, s, ox, oy, 4.0, 13.0, 7.0, 7.0, 1.0);
            rect(&mut fill, s, ox, oy, 13.0, 13.0, 7.0, 7.0, 1.0)
        }
        Icon::List => {
            seg(&mut stroke, s, ox, oy, &[8.0, 7.0, 20.0, 7.0]);
            seg(&mut stroke, s, ox, oy, &[8.0, 12.0, 20.0, 12.0]);
            seg(&mut stroke, s, ox, oy, &[8.0, 17.0, 20.0, 17.0]);
            circle(&mut fill, s, ox, oy, 4.5, 7.0, 1.3);
            circle(&mut fill, s, ox, oy, 4.5, 12.0, 1.3);
            circle(&mut fill, s, ox, oy, 4.5, 17.0, 1.3)
        }
        Icon::SortAsc => {
            seg(
                &mut fill,
                s,
                ox,
                oy,
                &[6.0, 15.0, 12.0, 8.0, 18.0, 15.0, 6.0, 15.0],
            );
            fill.close()
        }
        Icon::SortDesc => {
            seg(
                &mut fill,
                s,
                ox,
                oy,
                &[6.0, 9.0, 12.0, 16.0, 18.0, 9.0, 6.0, 9.0],
            );
            fill.close()
        }
        Icon::Filter => seg(
            &mut stroke,
            s,
            ox,
            oy,
            &[
                3.0, 5.0, 21.0, 5.0, 14.0, 13.0, 14.0, 20.0, 10.0, 18.0, 10.0, 13.0, 3.0, 5.0,
            ],
        ),
        Icon::Drag => {
            circle(&mut fill, s, ox, oy, 9.0, 6.0, 1.5);
            circle(&mut fill, s, ox, oy, 15.0, 6.0, 1.5);
            circle(&mut fill, s, ox, oy, 9.0, 12.0, 1.5);
            circle(&mut fill, s, ox, oy, 15.0, 12.0, 1.5);
            circle(&mut fill, s, ox, oy, 9.0, 18.0, 1.5);
            circle(&mut fill, s, ox, oy, 15.0, 18.0, 1.5)
        }
        Icon::External => {
            seg(&mut stroke, s, ox, oy, &[14.0, 4.0, 20.0, 4.0, 20.0, 10.0]);
            seg(&mut stroke, s, ox, oy, &[20.0, 4.0, 11.0, 13.0]);
            seg(
                &mut stroke,
                s,
                ox,
                oy,
                &[17.0, 14.0, 17.0, 20.0, 4.0, 20.0, 4.0, 7.0, 10.0, 7.0],
            )
        }
        Icon::App => {
            rect(&mut fill, s, ox, oy, 4.0, 4.0, 16.0, 16.0, 4.0);
            rect(&mut fill, s, ox, oy, 6.5, 6.5, 11.0, 11.0, 2.5)
        }
        Icon::Custom(_) => seg(
            &mut stroke,
            s,
            ox,
            oy,
            &[5.0, 5.0, 19.0, 5.0, 19.0, 19.0, 5.0, 19.0, 5.0, 5.0],
        ),
    }
    if !fill.cmds.is_empty() {
        let m = crate::render::vector::Mask::fill(
            &fill.cmds,
            1.0,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            crate::render::vector::FillRule::EvenOdd,
        );
        canvas.mask(&m, color)
    }
    if !stroke.cmds.is_empty() {
        canvas.stroke_path(&stroke, width, color)
    }
}

#[cfg(test)]
mod tests {
    use super::{BUILT_IN, Icon, IconArt, draw, draw_from, register};
    use crate::render::Canvas;
    use crate::render::vector::Path;
    use crate::style::color::Color;

    fn pixels(set: &str, icon: &Icon) -> Vec<u8> {
        let mut c = Canvas::new(48, 48);
        draw_from(&mut c, set, icon, 0, 0, 48, Color::white());
        c.pixels
    }

    #[test]
    fn a_registered_set_draws_its_icons_and_falls_back_to_the_built_in_ones() {
        let mut fill = Path::new();
        fill.move_to(0.0, 0.0);
        fill.line_to(12.0, 0.0);
        fill.line_to(12.0, 12.0);
        fill.line_to(0.0, 12.0);
        fill.close();
        register(
            "test-brand",
            &Icon::Save,
            &IconArt {
                fill: fill.clone(),
                stroke: Path::new(),
            },
        );
        register(
            "test-brand",
            &Icon::Custom("mascot".to_string()),
            &IconArt {
                fill,
                stroke: Path::new(),
            },
        );
        let mine = pixels("test-brand", &Icon::Save);
        assert!(mine != pixels("", &Icon::Save));
        assert_eq!(
            pixels("test-brand", &Icon::Custom("mascot".to_string())),
            mine
        );
        // The art fills the top-left quarter of the square, scaled from the 24-unit grid.
        assert!(mine[(10 * 48 + 10) * 4] > 200 && mine[(36 * 48 + 36) * 4] == 0);
        assert_eq!(pixels("test-brand", &Icon::Copy), pixels("", &Icon::Copy));
        assert_eq!(pixels("no-such-set", &Icon::Save), pixels("", &Icon::Save))
    }

    #[test]
    fn every_icon_puts_ink_in_its_square() {
        for icon in BUILT_IN {
            let mut c = Canvas::new(24, 24);
            draw(&mut c, &icon, 0, 0, 24, Color::white());
            let mut lit = 0;
            let mut i = 0;
            while i < c.pixels.len() {
                if c.pixels[i] > 100 {
                    lit += 1
                };
                i += 4;
            }
            assert!(lit > 6, "{}", icon.name());
            assert_eq!(Icon::named(icon.name()), Some(icon))
        }
        assert_eq!(Icon::named("no-such-icon"), None)
    }
}
