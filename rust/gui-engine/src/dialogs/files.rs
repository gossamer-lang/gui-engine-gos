use crate::paths;
use crate::strext::StrExt;
use std::{env, fs};

use windowing;

use crate::app::effects::Effect;
use crate::dialogs::Outcome;
use crate::dialogs::host::Instance;
use crate::dialogs::request::{ButtonRole, DialogAction, DialogRequest, FileMode};
use crate::render::icons::Icon;
use crate::services;
use crate::services::listing::{ListRequest, Listing, SortBy};
use crate::services::places::PlaceBook;
use crate::widgets::collections::{Collection, Column, Row};
use crate::widgets::completion::{Completion, CompletionItem};
use crate::widgets::cx::UiMsg;
use crate::widgets::state::StateStore;
use crate::widgets::textinput::{InputKind, TextInput};
use crate::widgets::{Axis, CrossAlign, MainAlign, Tone, Widget};

/// A directory entry retained while the file browser is open.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub path: String,
    pub name: String,
    pub directory: bool,
    /// Bytes, for files.
    pub size: i64,
    /// Milliseconds since the Unix epoch; 0 when unknown.
    pub modified_ms: i64,
}

fn cid(inst: &Instance, part: &str) -> String {
    format!("dlg:{}:{}", inst.id, part)
}
fn value(inst: &Instance, key: &str) -> String {
    inst.values.get(key).map_or("", String::as_str).to_string()
}
fn list(inst: &Instance, key: &str) -> Vec<String> {
    serde_json::from_str::<services::StringList>(&value(inst, key))
        .map(|v| v.items)
        .unwrap_or(vec![])
}
fn encoded(items: &[String]) -> String {
    crate::services::to_json(&services::StringList {
        items: items.to_vec(),
    })
}
// The `text/uri-list` body naming `paths`, one URI per CRLF-ended line.
fn uri_list(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| format!("{}\r\n", windowing::instance::file_uri(p)))
        .collect::<Vec<_>>()
        .join("")
}

fn settings_file(inst: &Instance) -> String {
    services::settings_path(&inst.app_id, "file-dialogs")
}
fn purpose(inst: &Instance) -> String {
    if let DialogRequest::File { ref purpose, .. } = inst.request
        && !purpose.is_empty()
    {
        return purpose.to_string();
    }
    "files".to_string()
}

fn favorites(inst: &Instance) -> PlaceBook {
    PlaceBook::decode(&value(inst, "places")).unwrap_or_default()
}
fn save_places(inst: Instance, book: &PlaceBook) -> Instance {
    let mut n = inst;
    n.values.insert("places".to_string(), book.encode());
    n.values.insert(
        "favorites".to_string(),
        encoded(
            &book
                .entries
                .iter()
                .map(|e| e.location.clone())
                .collect::<Vec<_>>(),
        ),
    );
    persist(n)
}

/// Opens the requested location and restores remembered browser preferences.
pub fn initialize(inst: Instance) -> Instance {
    let mut n = inst;
    let DialogRequest::File {
        ref directory,
        ref name,
        ..
    } = n.request
    else {
        return n;
    };
    let settings = services::load_settings(&settings_file(&n), 1, |_, s| s)
        .unwrap_or(services::Settings::default());
    let mut book = PlaceBook::new();
    let saved = settings.get("places", "");
    if saved.is_empty() {
        for location in settings.get_list("favorites") {
            let _ = book.add(&location, "");
        }
    } else {
        match PlaceBook::decode(&saved) {
            Ok(saved) => book = saved,
            Err(e) => {
                n.error = crate::localization::message(
                    "files",
                    "Cannot load favorite locations: {error}",
                    1,
                    &crate::hmap! { "error" => format!("{}", e) },
                )
            }
        }
    }
    n.values.insert("places".to_string(), book.encode());
    n.values.insert(
        "favorites".to_string(),
        encoded(
            &book
                .entries
                .iter()
                .map(|e| e.location.clone())
                .collect::<Vec<_>>(),
        ),
    );
    n.values.insert(
        "recent".to_string(),
        encoded(&settings.get_list(&format!("recent:{}", purpose(&n)))),
    );
    n.values.insert(
        "filter".to_string(),
        settings.get(&format!("filter:{}", purpose(&n)), "0"),
    );
    n.values.insert(
        "hidden".to_string(),
        settings.get(&format!("hidden:{}", purpose(&n)), "false"),
    );
    n.text = name.to_string();
    let initial = if directory.is_empty() {
        settings.get(
            &format!("directory:{}", purpose(&n)),
            &env::current_dir()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_string()),
        )
    } else {
        directory.to_string()
    };
    navigate(n, &initial)
}

// The current file type's patterns; empty when it accepts every file.
fn patterns(inst: &Instance) -> Vec<String> {
    let DialogRequest::File { ref filters, .. } = inst.request else {
        return vec![];
    };
    let i = value(inst, "filter").to_i64().unwrap_or(0);
    if i < 0 || i >= filters.len() as i64 {
        return vec![];
    };
    let chosen = &filters[i as usize].patterns;
    if chosen.iter().any(|p| p == "*" || p == "*.*") {
        vec![]
    } else {
        chosen.to_vec()
    }
}

fn folders_only(inst: &Instance) -> bool {
    matches!(
        inst.request,
        DialogRequest::File {
            mode: FileMode::Folder,
            ..
        }
    ) || matches!(
        inst.request,
        DialogRequest::File {
            mode: FileMode::FolderMany,
            ..
        }
    )
}

fn sort_by(inst: &Instance) -> SortBy {
    match value(inst, "sort").as_str() {
        "size" => SortBy::Size,
        "kind" => SortBy::Kind,
        "modified" => SortBy::Modified,
        _ => SortBy::Name,
    }
}

// Asks for `location`'s entries (relative to the current folder); `step`
// says how the answer moves the history: "open", "back", "forward" or "stay".
// The folder changes only when the answer arrives, so a failure leaves the
// browser where it was.
fn load_directory(inst: Instance, location: &str, step: &str) -> Instance {
    let mut n = inst;
    let current = value(&n, "directory");
    let base = if current.is_empty() {
        env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string())
    } else {
        current.to_string()
    };
    let requested = if paths::is_absolute(location) {
        location
    } else {
        &paths::join(&base, location)
    };
    let request = value(&n, "listing").to_i64().unwrap_or(0) + 1;
    n.values
        .insert("listing".to_string(), format!("{}", request));
    n.values
        .insert("listing:step".to_string(), step.to_string());
    n.values.insert("loading".to_string(), "true".to_string());
    let mut req = ListRequest::new(&cid(&n, "listing"), request, requested);
    req.hidden = value(&n, "hidden") == "true";
    req.folders_only = folders_only(&n);
    req.patterns = patterns(&n);
    req.sort = sort_by(&n);
    req.ascending = value(&n, "descending") != "true";
    crate::perform(Effect::ListDirectory(req));
    n
}

// Applies the answer to the latest listing request; earlier answers are dropped.
fn listed(inst: Instance, request: i64, found: &Listing) -> Instance {
    let mut n = inst;
    if format!("{}", request) != value(&n, "listing") {
        return n;
    };
    n.values.insert("loading".to_string(), "".to_string());
    if !found.error.is_empty() {
        n.error = crate::localization::message(
            "files",
            "Cannot open this folder: {error}",
            1,
            &crate::hmap! { "error" => found.error.clone() },
        );
        return n;
    }
    let previous = value(&n, "directory");
    let moved = previous != found.directory;
    n.files = found
        .entries
        .iter()
        .map(|e| Entry {
            path: e.path.clone(),
            name: e.name.clone(),
            directory: e.directory,
            size: e.size,
            modified_ms: e.modified_ms,
        })
        .collect::<Vec<_>>();
    n.values
        .insert("total".to_string(), format!("{}", found.total));
    if moved {
        n.values
            .insert("new-folder".to_string(), "false".to_string());
        n.values
            .insert("new-folder:name".to_string(), "".to_string());
    }
    n.values
        .insert("directory".to_string(), found.directory.clone());
    n.values
        .insert("location".to_string(), found.directory.clone());
    n.values.insert("overwrite".to_string(), "".to_string());
    n.values.insert("selection".to_string(), "".to_string());
    n.selected = "".to_string();
    n.page += 1;
    n.error = "".to_string();
    let keep_name = matches!(
        n.request,
        DialogRequest::File {
            mode: FileMode::Save,
            ..
        }
    );
    let step = value(&n, "listing:step");
    if step == "open" && moved {
        n.values.insert(
            "back".to_string(),
            history_push(list(&n, "back"), &previous),
        );
        n.values.insert("forward".to_string(), encoded(&[]));
        if !keep_name {
            n.text = "".to_string()
        }
    } else if step == "back" || step == "forward" {
        let into = if step == "back" { "forward" } else { "back" };
        let mut stack = list(&n, &step);
        let _ = stack.pop();
        n.values.insert(step.to_string(), encoded(&stack));
        n.values
            .insert(into.to_string(), history_push(list(&n, into), &previous));
        if !keep_name {
            n.text = "".to_string()
        }
    }
    n
}

// Asks for the folders the location field's text can continue into: those
// in the text's folder whose names start with what follows its last slash.
fn complete_location(inst: &Instance, request: i64, query: &str) {
    let slash = query.rfind("/").map_or(-1, |at| at as i64);
    let folder = if slash < 0 {
        value(inst, "directory")
    } else if slash == 0 {
        "/".to_string()
    } else {
        query.substring(0, slash).to_string()
    };
    let mut req = ListRequest::new(&cid(inst, "complete"), request, &folder);
    req.folders_only = true;
    req.hidden = value(inst, "hidden") == "true";
    req.prefix = query.substring(slash + 1, query.byte_len()).to_string();
    req.limit = 50;
    crate::perform(Effect::ListDirectory(req))
}

fn history_push(mut items: Vec<String>, location: &str) -> String {
    if !location.is_empty() && (items.is_empty() || items[items.len() - 1] != location) {
        items.push(location.to_string())
    };
    while items.len() > 32 {
        let _ = items.remove(0);
    }
    encoded(&items)
}

fn navigate(inst: Instance, location: &str) -> Instance {
    load_directory(inst, location, "open")
}

fn travel(inst: Instance, from: &str) -> Instance {
    let stack = list(&inst, from);
    if stack.is_empty() {
        return inst;
    };
    load_directory(inst, &stack[stack.len() - 1], from)
}

fn breadcrumbs(inst: &Instance) -> Widget {
    let mut ancestors = vec![];
    let mut directory = value(inst, "directory");
    let mut depth = 0;
    while !directory.is_empty() && depth < 64 {
        ancestors.push(directory.clone());
        let parent = paths::parent(&directory).unwrap_or("".to_string());
        if parent == directory {
            break;
        };
        directory = parent;
        depth += 1;
    }
    let mut parts = vec![];
    let mut i = ancestors.len();
    while i > 0 {
        i -= 1;
        let folder = &ancestors[i];
        let label = paths::file_name(folder).unwrap_or(folder.to_string());
        if !parts.is_empty() {
            parts.push(Widget::muted("/"))
        };
        parts.push(if *folder == value(inst, "directory") {
            Widget::label(&label).accessible(&format!("{}, current folder", label))
        } else {
            Widget::flat_button(&cid(inst, &format!("breadcrumb:{}", folder)), &label)
        })
    }
    Widget::flow(&parts, 2)
        .role(crate::accessibility::Role::Pane)
        .accessible(&crate::localization::tr("Folder breadcrumbs"))
}

fn create_folder(inst: Instance) -> Instance {
    let mut n = inst;
    let raw_name = value(&n, "new-folder:name");
    let name = raw_name.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains("/")
        || name.contains("\\")
        || name.contains("\0")
    {
        n.error = crate::localization::tr("Enter one folder name without path separators");
        return n;
    }
    let location = paths::join(&value(&n, "directory"), name);
    match fs::create_dir(&location) {
        Ok(()) => {
            n.values
                .insert("new-folder".to_string(), "false".to_string());
            n = navigate(n, &location)
        }
        Err(e) => {
            n.error = crate::localization::message(
                "files",
                "Cannot create this folder: {error}",
                1,
                &crate::hmap! { "error" => format!("{}", e) },
            )
        }
    }
    n
}

/// The file mode's affirmative action and a cancel action that always dismisses.
pub fn actions(inst: &Instance) -> Vec<DialogAction> {
    let label = if let DialogRequest::File {
        mode: FileMode::Save,
        ..
    } = inst.request
    {
        "Save"
    } else {
        "Open"
    };
    vec![
        DialogAction::new("cancel", "Cancel", ButtonRole::Reject),
        DialogAction::new("choose", label, ButtonRole::Accept),
    ]
}

fn field(inst: &Instance, part: &str, label: &str, text: &str) -> Widget {
    let mut t = TextInput::new(&cid(inst, part), text, "", InputKind::Line);
    t.label = label.to_string();
    Widget::input(&t).grow(1)
}

// A byte count as people read it: bytes, then KB, MB and GB to one decimal.
fn size_text(bytes: i64) -> String {
    if bytes < 1000 {
        return crate::localization::message(
            "files",
            "{size} B",
            1,
            &crate::hmap! { "size" => crate::localization::format_number(bytes as f64, 0) },
        );
    };
    // Each unit is its own catalog entry, so a language can respell it.
    let units = ["{size} KB", "{size} MB", "{size} GB", "{size} TB"];
    let mut v = bytes as f64 / 1000.0;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    crate::localization::message(
        "files",
        units[u],
        1,
        &crate::hmap! { "size" => crate::localization::format_number(v, 1) },
    )
}

fn kind_text(e: &Entry) -> String {
    if e.directory {
        return crate::localization::tr("Folder");
    };
    match e.name.rfind(".") {
        Some(at) if at > 0 => crate::localization::message(
            "files",
            "{kind} file",
            1,
            &crate::hmap! { "kind" => e.name.substring((at + 1) as i64, e.name.byte_len()).to_uppercase() },
        ),
        _ => crate::localization::tr("File"),
    }
}

// The folder field, completing folder names as they are typed.
fn location_field(inst: &Instance) -> Widget {
    let mut t = TextInput::new(
        &cid(inst, "location"),
        &value(inst, "location"),
        "",
        InputKind::Line,
    );
    t.label = crate::localization::tr("Folder");
    t.completion = Some(Completion::asynchronous(120));
    Widget::input(&t).grow(1)
}

/// A location bar, persistent locations, virtual directory list, and filename field.
pub fn body(inst: &Instance) -> Widget {
    let entries = inst.files.clone();
    let selected = list(inst, "selection");
    let multiple = matches!(
        inst.request,
        DialogRequest::File {
            mode: FileMode::OpenMany,
            ..
        }
    ) || matches!(
        inst.request,
        DialogRequest::File {
            mode: FileMode::FolderMany,
            ..
        }
    );
    let columns = vec![
        Column::new("name", &crate::localization::tr("Name"), 260),
        Column::new("size", &crate::localization::tr("Size"), 90),
        Column::new("kind", &crate::localization::tr("Kind"), 90),
        Column::new("modified", &crate::localization::tr("Modified"), 150),
    ];
    let sort = if value(inst, "sort").is_empty() {
        "name"
    } else {
        &value(inst, "sort")
    };
    let loading = value(inst, "loading") == "true";
    let shown = entries.clone();
    let entries_view = Collection::new(&cid(inst, "entries"), entries.len() as i64, move |i| {
        let e = &shown[i as usize];
        let mut r = Row::new(&e.path, &e.name);
        r.icon = Some(if e.directory {
            Icon::Folder
        } else {
            Icon::File
        });
        r.cells = vec![
            e.name.clone(),
            if e.directory {
                String::new()
            } else {
                size_text(e.size)
            },
            kind_text(e),
            if e.modified_ms > 0 {
                crate::localization::format_timestamp(e.modified_ms)
            } else {
                String::new()
            },
        ];
        r
    })
    .revision(inst.page, &[])
    .selected(&selected)
    .multiple(multiple)
    .columns(&columns)
    .sorted_by(sort, value(inst, "descending") != "true")
    .label(&crate::localization::tr("Files and folders"))
    .empty_text(&crate::localization::tr(if loading {
        "Loading\u{2026}"
    } else {
        "No matching files"
    }))
    .rows(6)
    .draggable(|paths: Vec<String>| {
        vec![windowing::DragItem {
            mime: "text/uri-list".to_string(),
            data: uri_list(&paths).into_bytes(),
        }]
    });
    let mut places = vec![
        Widget::flat_button(&cid(inst, "home"), &crate::localization::tr("Home")),
        Widget::label(&crate::localization::tr("Favorite locations")),
        Widget::flat_button(
            &cid(inst, "favorite"),
            &crate::localization::tr("Add current folder"),
        ),
    ];
    for favorite in favorites(inst).entries {
        let id = favorite.id;
        places.push(
            Widget::flat_button(&cid(inst, &format!("favorite:{}", id)), &favorite.label)
                .tooltip(&favorite.location),
        );
        if value(inst, "favorite:selected") == id {
            if !paths::is_dir(&favorite.location) {
                places.push(Widget::text(
                    &crate::localization::tr("This folder no longer exists."),
                    crate::style::TextRole::Small,
                    Tone::Warning,
                ))
            };
            places.push(field(
                inst,
                "favorite:label",
                &crate::localization::tr("Favorite label"),
                &value(inst, "favorite:label"),
            ));
            places.push(Widget::flow(
                &[
                    Widget::flat_button(
                        &cid(inst, "favorite:rename"),
                        &crate::localization::tr("Rename"),
                    ),
                    Widget::flat_button(
                        &cid(inst, "favorite:up"),
                        &crate::localization::tr("Move up"),
                    ),
                    Widget::flat_button(
                        &cid(inst, "favorite:down"),
                        &crate::localization::tr("Move down"),
                    ),
                    Widget::flat_button(
                        &cid(inst, "favorite:relink"),
                        &crate::localization::tr("Use current folder"),
                    ),
                    Widget::flat_button(
                        &cid(inst, "favorite:remove"),
                        &crate::localization::tr("Remove"),
                    ),
                ],
                4,
            ))
        }
    }
    places.push(Widget::muted(&crate::localization::tr("Recent")));
    for recent in list(inst, "recent") {
        if !recent.is_empty() {
            places.push(
                Widget::flat_button(
                    &cid(inst, &format!("place:{}", recent)),
                    &paths::file_name(&recent).unwrap_or_else(|| recent.clone()),
                )
                .tooltip(&recent),
            )
        }
    }
    let mut rows = vec![
        Widget::row(
            &[
                Widget::tool_button(
                    &cid(inst, "back"),
                    &Icon::ChevronLeft,
                    &crate::localization::tr("Back"),
                )
                .disabled(list(inst, "back").is_empty()),
                Widget::tool_button(
                    &cid(inst, "forward"),
                    &Icon::ChevronRight,
                    &crate::localization::tr("Forward"),
                )
                .disabled(list(inst, "forward").is_empty()),
                Widget::tool_button(
                    &cid(inst, "up"),
                    &Icon::ChevronUp,
                    &crate::localization::tr("Parent folder"),
                ),
                location_field(inst),
                Widget::button(&cid(inst, "go"), &crate::localization::tr("Go")),
            ],
            6,
        ),
        breadcrumbs(inst),
        Widget::flow(
            &[
                Widget::flat_button(&cid(inst, "refresh"), &crate::localization::tr("Refresh")),
                Widget::flat_button(
                    &cid(inst, "new-folder"),
                    &crate::localization::tr("New folder"),
                ),
                Widget::checkbox(
                    &cid(inst, "hidden"),
                    &crate::localization::tr("Show hidden files"),
                    value(inst, "hidden") == "true",
                ),
            ],
            6,
        ),
        Widget::row(
            &[
                Widget::scroll(&cid(inst, "places"), Widget::column(&places, 4)).width(160),
                Widget::collection(&entries_view).grow(1),
            ],
            8,
        )
        .grow(1),
        Widget::row(
            &[
                Widget::label(&crate::localization::tr("Name")),
                field(
                    inst,
                    "filename",
                    &crate::localization::tr("File name"),
                    &inst.text,
                ),
            ],
            8,
        ),
    ];
    if value(inst, "new-folder") == "true" {
        rows.insert(
            3,
            Widget::column(
                &[
                    Widget::label(&crate::localization::tr("New folder name")),
                    field(
                        inst,
                        "new-folder:name",
                        &crate::localization::tr("New folder name"),
                        &value(inst, "new-folder:name"),
                    ),
                    Widget::row(
                        &[
                            Widget::button(
                                &cid(inst, "new-folder:create"),
                                &crate::localization::tr("Create folder"),
                            ),
                            Widget::button(
                                &cid(inst, "new-folder:cancel"),
                                &crate::localization::tr("Cancel new folder"),
                            ),
                        ],
                        6,
                    ),
                ],
                4,
            ),
        )
    }
    if let DialogRequest::File { filters, .. } = &inst.request
        && !filters.is_empty()
    {
        let mut choices = vec![];
        for (i, f) in filters.iter().enumerate() {
            choices.push(crate::widgets::completion::CompletionItem::new(
                &format!("{}", i),
                &f.name,
            ));
        }
        rows.push(Widget::select(
            &cid(inst, "filter"),
            &choices,
            &value(inst, "filter"),
            &crate::localization::tr("File type"),
        ))
    }
    let total = value(inst, "total").to_i64().unwrap_or(0);
    if total > entries.len() as i64 {
        rows.push(Widget::text(&crate::localization::message("files", "Showing the first {shown} of {count} items. Type a name or choose a file type to narrow them.", total, &crate::hmap! { "shown" => format!("{}", entries.len()) }), crate::style::TextRole::Small, Tone::Muted))
    }
    if !inst.error.is_empty() {
        rows.push(Widget::text(
            &inst.error,
            crate::style::TextRole::Small,
            Tone::Danger,
        ))
    };
    if !value(inst, "overwrite").is_empty() {
        rows.push(Widget::paragraph(&crate::localization::tr(
            "This file already exists. Replace it?",
        )));
        rows.push(Widget::row(
            &[
                Widget::danger_button(
                    &cid(inst, "replace"),
                    &crate::localization::tr("Replace file"),
                ),
                Widget::button(
                    &cid(inst, "keep"),
                    &crate::localization::tr("Keep existing file"),
                ),
            ],
            8,
        ))
    }
    Widget::flex(
        Axis::Vertical,
        &rows,
        8,
        MainAlign::Start,
        CrossAlign::Stretch,
    )
}

fn persist(inst: Instance) -> Instance {
    let mut n = inst;
    let file = settings_file(&n);
    let mut settings = match services::load_settings(&file, 1, |_, s| s) {
        Ok(s) => s,
        Err(e) => {
            n.error = crate::localization::message(
                "files",
                "Cannot read file browser settings: {error}",
                1,
                &crate::hmap! { "error" => format!("{}", e) },
            );
            return n;
        }
    };
    settings.set_list("favorites", &list(&n, "favorites"));
    settings.set("places", &value(&n, "places"));
    settings.set(
        &format!("directory:{}", purpose(&n)),
        &value(&n, "directory"),
    );
    settings.set(&format!("filter:{}", purpose(&n)), &value(&n, "filter"));
    settings.set(&format!("hidden:{}", purpose(&n)), &value(&n, "hidden"));
    let directory = value(&n, "directory");
    let mut recent = vec![directory];
    for p in settings.get_list(&format!("recent:{}", purpose(&n))) {
        if !recent.contains(&p) && recent.len() < 8 {
            recent.push(p)
        }
    }
    settings.set_list(&format!("recent:{}", purpose(&n)), &recent);
    if let Err(e) = services::save_settings(&file, &settings) {
        n.error = crate::localization::message(
            "files",
            "Cannot save file browser settings: {error}",
            1,
            &crate::hmap! { "error" => format!("{}", e) },
        )
    };
    n
}

/// Commits a validated selection, leaving invalid paths and overwrite review editable.
pub fn accept(inst: Instance, replace: bool) -> (Instance, Option<Outcome>, Vec<Effect>) {
    let mut n = inst;
    let DialogRequest::File { mode, .. } = n.request else {
        return (n, None, vec![]);
    };
    let directory = value(&n, "directory");
    let folder = mode == FileMode::Folder || mode == FileMode::FolderMany;
    let mut paths = vec![];
    if mode == FileMode::OpenMany || mode == FileMode::FolderMany {
        paths = list(&n, "selection");
    }
    if paths.is_empty() {
        let name = n.text.trim();
        if name.is_empty() && !folder {
            n.error = crate::localization::tr("Enter a file name");
            return (n, None, vec![]);
        };
        paths.push(if name.is_empty() {
            directory
        } else if paths::is_absolute(name) {
            name.to_string()
        } else {
            paths::join(&directory, name)
        })
    }
    for p in &paths {
        if paths::is_dir(p) && !folder {
            return (navigate(n, p), None, vec![]);
        };
        if folder && !paths::is_dir(p) {
            n.error = crate::localization::tr("Choose an existing folder");
            return (n, None, vec![]);
        };
        if mode != FileMode::Save && !folder && !paths::is_file(p) {
            n.error = crate::localization::tr("Choose an existing file");
            return (n, None, vec![]);
        };
        if mode == FileMode::Save {
            if !paths::parent(p)
                .map(|parent| paths::is_dir(&parent))
                .unwrap_or(false)
            {
                n.error = crate::localization::tr("The parent folder does not exist");
                return (n, None, vec![]);
            };
            if paths::exists(p) && (!replace || value(&n, "overwrite") != *p) {
                n.values.insert("overwrite".to_string(), p.to_string());
                return (n, None, vec![]);
            }
        }
    }
    n = persist(n);
    (n, Some(Outcome::Paths(paths)), vec![])
}

/// Handles browser controls while preserving drafts until acceptance.
pub fn message(
    inst: Instance,
    msg: &UiMsg,
    part: &str,
    _st: &mut StateStore,
) -> (Instance, Option<Outcome>, Vec<Effect>) {
    let mut n = inst;
    let mut effects = vec![];
    match msg {
        UiMsg::Click(_) => match part {
            "back" => n = travel(n, "back"),
            "forward" => n = travel(n, "forward"),
            "refresh" => n = load_directory(n.clone(), &value(&n, "directory"), "stay"),
            "new-folder" => {
                n.values
                    .insert("new-folder".to_string(), "true".to_string());
                n.values
                    .insert("new-folder:name".to_string(), "".to_string());
                n.error = "".to_string();
                effects.push(Effect::Focus(cid(&n, "new-folder:name")))
            }
            "new-folder:create" => {
                n = create_folder(n);
                if n.error.is_empty() {
                    effects.push(Effect::Focus(cid(&n, "entries")))
                }
            }
            "new-folder:cancel" => {
                n.values
                    .insert("new-folder".to_string(), "false".to_string());
                n.error = "".to_string();
                effects.push(Effect::Focus(cid(&n, "new-folder")))
            }
            "up" => {
                n = navigate(
                    n.clone(),
                    &paths::parent(&value(&n, "directory")).unwrap_or(value(&n, "directory")),
                )
            }
            "home" => n = navigate(n, &paths::home().unwrap_or_else(|| ".".to_string())),
            "go" => n = navigate(n.clone(), &value(&n, "location")),
            "favorite" => {
                let directory = value(&n, "directory");
                let mut book = favorites(&n);
                match book.add(&directory, "") {
                    Ok(id) => {
                        n.values.insert("favorite:selected".to_string(), id);
                        n.values.insert(
                            "favorite:label".to_string(),
                            paths::file_name(&directory).unwrap_or(directory),
                        );
                        n = save_places(n, &book)
                    }
                    Err(e) => n.error = format!("{}", e),
                }
            }
            "favorite:rename" | "favorite:remove" | "favorite:up" | "favorite:down"
            | "favorite:relink" => {
                let mut book = favorites(&n);
                let id = value(&n, "favorite:selected");
                match part {
                    "favorite:rename" => {
                        if !book.rename(&id, &value(&n, "favorite:label")) {
                            n.error = crate::localization::tr("Enter a favorite label")
                        }
                    }
                    "favorite:remove" => {
                        book.remove(&id);
                        n.values
                            .insert("favorite:selected".to_string(), "".to_string());
                    }
                    "favorite:relink" => {
                        if !book.relink(&id, &value(&n, "directory")) {
                            n.error = crate::localization::tr("That location is already a favorite")
                        }
                    }
                    _ => {
                        let index = book.entries.iter().position(|e| e.id == id).unwrap_or(0);
                        let _ = book.move_to(
                            &id,
                            index as i64 + if part == "favorite:up" { -1 } else { 1 },
                        );
                    }
                }
                n = save_places(n, &book);
            }
            "replace" => return accept(n, true),
            "keep" => {
                n.values.insert("overwrite".to_string(), "".to_string());
            }
            _ => {
                if part.starts_with("favorite:") {
                    let id = part.substring(9, part.byte_len());
                    for entry in favorites(&n).entries {
                        if entry.id == id {
                            n.values
                                .insert("favorite:selected".to_string(), id.to_string());
                            n.values.insert("favorite:label".to_string(), entry.label);
                            n = navigate(n, &entry.location)
                        }
                    }
                } else if part.starts_with("place:") {
                    n = navigate(n, part.substring(6, part.byte_len()))
                } else if part.starts_with("breadcrumb:") {
                    n = navigate(n, part.substring(11, part.byte_len()))
                }
            }
        },
        UiMsg::Text(_, text) => {
            if part == "filename" {
                n.text = text.to_string();
                n.values.insert("overwrite".to_string(), "".to_string());
                n.error = "".to_string()
            };
            if part == "location" {
                n.values.insert("location".to_string(), text.to_string());
            }
            if part == "favorite:label" {
                n.values.insert(part.to_string(), text.to_string());
            }
            if part == "new-folder:name" {
                n.values.insert(part.to_string(), text.to_string());
                n.error = "".to_string()
            }
        }
        UiMsg::Submit(_, _) => {
            if part == "filename" {
                return accept(n, false);
            };
            if part == "location" {
                n = navigate(n.clone(), &value(&n, "location"))
            };
            if part == "new-folder:name" {
                n = create_folder(n);
                if n.error.is_empty() {
                    effects.push(Effect::Focus(cid(&n, "entries")))
                }
            }
        }
        UiMsg::Toggle(_, on) if part == "hidden" => {
            n.values.insert(
                "hidden".to_string(),
                if *on {
                    "true".to_string()
                } else {
                    "false".to_string()
                },
            );
            n = load_directory(n.clone(), &value(&n, "directory"), "stay");
            n = persist(n)
        }
        UiMsg::Select(_, key) => {
            if part == "filter" {
                n.values.insert("filter".to_string(), key.to_string());
                return (
                    load_directory(n.clone(), &value(&n, "directory"), "stay"),
                    None,
                    vec![],
                );
            }
            n.selected = key.to_string();
            n.values
                .insert("selection".to_string(), encoded(std::slice::from_ref(key)));
            n.text = paths::file_name(key).unwrap_or(key.to_string());
        }
        UiMsg::SelectMany(_, keys) => {
            n.values.insert("selection".to_string(), encoded(keys));
        }
        UiMsg::Activate(_, key) => {
            if paths::is_dir(key) {
                n = navigate(n, key)
            } else {
                n.text = key.to_string();
                return accept(n, false);
            }
        }
        UiMsg::Choose(_, key) if part == "filter" => {
            n.values.insert("filter".to_string(), key.to_string());
            n = load_directory(n.clone(), &value(&n, "directory"), "stay")
        }
        UiMsg::Sort(_, column, ascending) if part == "entries" => {
            n.values.insert("sort".to_string(), column.to_string());
            n.values.insert(
                "descending".to_string(),
                if *ascending {
                    "false".to_string()
                } else {
                    "true".to_string()
                },
            );
            n = load_directory(n.clone(), &value(&n, "directory"), "stay");
        }
        UiMsg::Listed(_, request, found) => {
            if part == "listing" {
                n = listed(n, *request, found)
            };
            if part == "complete" {
                effects.push(Effect::Complete(
                    cid(&n, "location"),
                    *request,
                    found
                        .entries
                        .iter()
                        .map(|e| CompletionItem::new(&paths::join(&e.path, ""), &e.name))
                        .collect::<Vec<_>>(),
                    found.error.clone(),
                ))
            }
        }
        UiMsg::CompletionRequest(_, request, query, _) if part == "location" => {
            complete_location(&n, *request, query)
        }
        _ => (),
    }
    (n, None, effects)
}

#[cfg(test)]
mod tests {
    use crate::app::effects::Effect;
    use crate::dialogs::host::Instance;
    use crate::dialogs::request;
    use crate::paths;

    use crate::widgets::cx::UiMsg;
    use crate::widgets::state::StateStore;
    use std::{env, fs, process};

    // The browser once the listings it asked for are answered.
    fn answered(inst: Instance) -> Instance {
        let mut n = inst;
        for e in crate::app::effects::take() {
            if let Effect::ListDirectory(req) = e {
                n = super::listed(n, req.request, &crate::services::listing::list(&req))
            }
        }
        n
    }

    #[test]
    fn save_validates_paths_and_requires_explicit_overwrite() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let dir = paths::join(
            &env::temp_dir().to_string_lossy(),
            &format!("gui-file-test-{}", process::id()),
        );
        fs::create_dir_all(dir.clone()).unwrap();
        let file = paths::join(&dir, "existing.gos");
        fs::write(file.clone(), "keep").unwrap();
        let inst = Instance::new(
            &request::toolkit_file(request::save_file("save", "Save", "", &[])),
            "dev.gossamer.gui-file-test",
        );
        let mut st = StateStore::new();
        let (empty, none, _) = super::accept(inst, false);
        assert_eq!(none, None);
        assert!(!empty.error.is_empty());
        let (edited, _, _) = super::message(
            empty,
            &UiMsg::Text("dlg:save:filename".to_string(), file.clone()),
            "filename",
            &mut st,
        );
        let (review, none, _) = super::accept(edited, false);
        assert_eq!(none, None);
        assert_eq!(super::value(&review, "overwrite"), file);
        let (changed, _, _) = super::message(
            review,
            &UiMsg::Text("dlg:save:filename".to_string(), format!("{}.new", file)),
            "filename",
            &mut st,
        );
        assert_eq!(super::value(&changed, "overwrite"), "");
        assert_eq!(fs::read_to_string(file.clone()).unwrap(), "keep");
        let _ = fs::remove_file(file);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn filter_changes_do_not_become_a_filename() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let filters = vec![
            request::FileFilter {
                name: "Gossamer".to_string(),
                patterns: vec!["*.gos".to_string()],
            },
            request::FileFilter {
                name: "All".to_string(),
                patterns: vec!["*".to_string()],
            },
        ];
        let inst = Instance::new(
            &request::toolkit_file(request::save_file("filter", "Save", "draft.gos", &filters)),
            "dev.gossamer.gui-file-test",
        );
        let mut st = StateStore::new();
        let (updated, _, _) = super::message(
            inst,
            &UiMsg::Select("dlg:filter:filter".to_string(), "1".to_string()),
            "filter",
            &mut st,
        );
        assert_eq!(updated.text, "draft.gos");
        assert_eq!(super::value(&updated, "filter"), "1")
    }

    #[test]
    fn navigation_history_hidden_files_and_new_folder_are_transactional() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let root = paths::join(
            &env::temp_dir().to_string_lossy(),
            &format!("gui-navigation-{}", process::id()),
        );
        fs::create_dir_all(paths::join(&root, "a")).unwrap();
        fs::create_dir_all(paths::join(&root, "b")).unwrap();
        fs::write(paths::join(&root, ".hidden"), "secret").unwrap();
        fs::write(paths::join(&root, "visible.gos"), "text").unwrap();
        let root = fs::canonicalize(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let app_id = format!("dev.gossamer.gui-navigation-test-{}", process::id());
        let mut inst = answered(Instance::new(
            &request::toolkit_file(request::save_file("nav", "Save", "draft.gos", &[])),
            &app_id,
        ));
        inst = answered(super::navigate(inst, &root));
        assert!(!inst.files.iter().any(|e| e.name == ".hidden"));
        inst = answered(super::navigate(inst, "a"));
        assert_eq!(super::value(&inst, "directory"), paths::join(&root, "a"));
        assert_eq!(inst.text, "draft.gos");
        inst = answered(super::navigate(inst, "../b"));
        assert_eq!(super::value(&inst, "directory"), paths::join(&root, "b"));
        inst = answered(super::travel(inst, "back"));
        assert_eq!(super::value(&inst, "directory"), paths::join(&root, "a"));
        inst = answered(super::travel(inst, "forward"));
        assert_eq!(super::value(&inst, "directory"), paths::join(&root, "b"));
        inst = answered(super::travel(inst, "back"));
        inst = answered(super::navigate(inst, &root));
        assert!(super::list(&inst, "forward").is_empty());
        let back = super::list(&inst, "back");
        let failed = answered(super::navigate(inst.clone(), "missing"));
        assert!(!failed.error.is_empty());
        assert_eq!(super::value(&failed, "directory"), root);
        assert_eq!(super::list(&failed, "back"), back);
        let mut st = StateStore::new();
        let (shown, _, _) = super::message(
            inst,
            &UiMsg::Toggle("dlg:nav:hidden".to_string(), true),
            "hidden",
            &mut st,
        );
        inst = answered(shown);
        assert!(inst.files.iter().any(|e| e.name == ".hidden"));
        let settings = super::settings_file(&inst);
        assert_eq!(
            crate::services::load_settings(&settings, 1, |_, s| s)
                .unwrap()
                .get("hidden:files", ""),
            "true"
        );
        inst.values
            .insert("new-folder:name".to_string(), "../escape".to_string());
        inst = answered(super::create_folder(inst));
        assert!(!inst.error.is_empty());
        assert_eq!(super::value(&inst, "directory"), root);
        inst.values
            .insert("new-folder:name".to_string(), "new café".to_string());
        inst = answered(super::create_folder(inst));
        assert_eq!(
            super::value(&inst, "directory"),
            paths::join(&root, "new café")
        );
        assert!(paths::is_dir(&paths::join(&root, "new café")));
        assert_eq!(inst.text, "draft.gos");
        inst = answered(super::navigate(inst, &root));
        inst.values
            .insert("new-folder:name".to_string(), "new café".to_string());
        inst = answered(super::create_folder(inst));
        assert!(!inst.error.is_empty());
        assert_eq!(super::value(&inst, "directory"), root);
        for i in 0..40 {
            inst = answered(super::navigate(
                inst,
                &paths::join(&root, if i % 2 == 0 { "a" } else { "b" }),
            ))
        }
        assert_eq!(super::list(&inst, "back").len(), 32);
        fs::remove_dir_all(root).unwrap();
        let _ = fs::remove_file(settings);
    }

    #[test]
    fn a_superseded_listing_is_dropped_and_sorting_asks_again() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let root = paths::join(&config, "sorted");
        fs::create_dir_all(root.clone()).unwrap();
        fs::write(paths::join(&root, "big.txt"), "123456789").unwrap();
        fs::write(paths::join(&root, "small.md"), "1").unwrap();
        let mut inst = answered(Instance::new(
            &request::toolkit_file(request::open_file("sort", "Open", &[])),
            "dev.gossamer.gui-file-test",
        ));
        inst = super::navigate(inst, &root);
        let early = crate::app::effects::take();
        inst = answered(super::navigate(inst, &root));
        // The first answer arrives late: it belongs to a superseded request.
        for e in early {
            if let Effect::ListDirectory(req) = e {
                inst = super::listed(
                    inst,
                    req.request,
                    &crate::services::listing::Listing {
                        directory: "/elsewhere".to_string(),
                        entries: vec![],
                        total: 0,
                        error: "".to_string(),
                    },
                )
            }
        }
        assert_eq!(
            super::value(&inst, "directory"),
            fs::canonicalize(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        );
        assert_eq!(
            inst.files
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>(),
            vec!["big.txt", "small.md"]
        );
        let mut st = StateStore::new();
        let (sorted, _, _) = super::message(
            inst,
            &UiMsg::Sort("dlg:sort:entries".to_string(), "size".to_string(), true),
            "entries",
            &mut st,
        );
        inst = answered(sorted);
        assert_eq!(
            inst.files
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>(),
            vec!["small.md", "big.txt"]
        );
        assert_eq!(super::size_text(9), "9 B");
        assert_eq!(super::size_text(1_500_000), "1.5 MB")
    }

    #[test]
    fn the_folder_field_completes_folder_names_from_what_is_typed() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        fs::create_dir_all(config.clone()).unwrap();
        let root = fs::canonicalize(&config)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for d in ["src", "scripts", "docs"] {
            fs::create_dir_all(paths::join(&root, d)).unwrap()
        }
        fs::write(paths::join(&root, "setup.txt"), "x").unwrap();
        let inst = answered(Instance::new(
            &request::toolkit_file(request::open_file("pick", "Open", &[])),
            "dev.gossamer.gui-file-test",
        ));
        let mut st = StateStore::new();
        let (asked, _, _) = super::message(
            inst,
            &UiMsg::CompletionRequest(
                "dlg:pick:location".to_string(),
                3,
                paths::join(&root, "s"),
                0,
            ),
            "location",
            &mut st,
        );
        let mut answers = vec![];
        for e in crate::app::effects::take() {
            if let Effect::ListDirectory(req) = e {
                assert_eq!(req.owner, "dlg:pick:complete");
                let (_, _, out) = super::message(
                    asked.clone(),
                    &UiMsg::Listed(
                        req.owner.clone(),
                        req.request,
                        crate::services::listing::list(&req),
                    ),
                    "complete",
                    &mut st,
                );
                answers = out;
            }
        }
        let Some(Effect::Complete(owner, request, items, error)) = answers.first() else {
            panic!("no completion answer");
        };
        assert_eq!(owner, "dlg:pick:location");
        assert_eq!(*request, 3);
        assert_eq!(error, "");
        assert_eq!(
            items.iter().map(|i| i.label.clone()).collect::<Vec<_>>(),
            vec!["scripts", "src"]
        );
        assert_eq!(
            items[0].key,
            paths::join(&paths::join(&root, "scripts"), "")
        )
    }

    #[test]
    fn multiple_selection_preserves_unicode_and_newlines_in_paths() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let inst = Instance::new(
            &request::toolkit_file(request::open_file("many", "Open", &[])),
            "dev.gossamer.gui-file-test",
        );
        let paths = vec![
            "/tmp/café.gos".to_string(),
            "/tmp/line\nbreak.gos".to_string(),
        ];
        let mut st = StateStore::new();
        let (updated, _, _) = super::message(
            inst,
            &UiMsg::SelectMany("dlg:many:entries".to_string(), paths.clone()),
            "entries",
            &mut st,
        );
        assert_eq!(super::list(&updated, "selection"), paths)
    }
}
