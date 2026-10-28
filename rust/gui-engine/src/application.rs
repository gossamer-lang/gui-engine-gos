use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use windowing::instance;

use crate::app::{self, ClosePolicy, Session, WindowStyle};
use crate::commands::Command;
use crate::errors;
use crate::paths;
use crate::services::apps::AppIcon;
use crate::widgets::Widget;
use crate::widgets::cx::UiMsg;

type Update<M> = Box<dyn Fn(M, UiMsg) -> M>;
type View<M> = Box<dyn Fn(&M) -> Widget>;
type Commands<M> = Box<dyn Fn(&M) -> Vec<Command>>;
type WindowView<M> = Box<dyn Fn(&M, &str) -> Widget>;

/// An application: model, update, and view, plus window settings.
pub struct App<M> {
    pub name: String,
    pub app_id: String,
    pub title: String,
    pub width: i64,
    pub height: i64,
    pub min_width: i64,
    pub min_height: i64,
    pub init: Box<dyn Fn() -> M>,
    pub update: Update<M>,
    pub view: View<M>,
    pub commands: Commands<M>,
    /// Builds windows opened with `Effect::OpenWindow`, by key.
    pub window_view: WindowView<M>,
    pub close_policy: ClosePolicy,
    /// The widget focused when the first window opens.
    pub initial_focus: String,
    /// Whether the first window grows downward to show popups below their
    /// anchors (`grows_for_popups`).
    pub grows_for_popups: bool,
    /// How the first window looks and where it opens.
    pub style: WindowStyle,
    /// Files a later launch hands to the running instance, when the
    /// application keeps one instance.
    pub instance_files: Option<Vec<String>>,
    /// The icon every window of the application shows.
    pub icon: Option<AppIcon>,
}

/// An application with default settings: an 800 by 600 window titled
/// `name`, no extra commands, and windows that close when asked.
pub fn application<M>(
    name: &str,
    app_id: &str,
    init: impl Fn() -> M + 'static,
    update: impl Fn(M, UiMsg) -> M + 'static,
    view: impl Fn(&M) -> Widget + 'static,
) -> App<M> {
    App {
        name: name.to_string(),
        app_id: app_id.to_string(),
        title: name.to_string(),
        width: 800,
        height: 600,
        min_width: 0,
        min_height: 0,
        init: Box::new(init),
        update: Box::new(update),
        view: Box::new(view),
        commands: Box::new(|_| Vec::new()),
        window_view: Box::new(|_, _| Widget::Empty),
        close_policy: ClosePolicy::Close,
        initial_focus: String::new(),
        grows_for_popups: false,
        style: WindowStyle::default(),
        instance_files: None,
        icon: None,
    }
}

impl<M: 'static> App<M> {
    pub fn title(mut self, title: &str) -> Self {
        self.title = title.to_string();
        self
    }

    /// The first window's size in interface units.
    pub fn size(mut self, width: i64, height: i64) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    pub fn min_size(mut self, width: i64, height: i64) -> Self {
        self.min_width = width;
        self.min_height = height;
        self
    }

    /// Makes the first window as tall as `size` says and lets it grow
    /// downward while a popup below its anchor needs the room, as a run box
    /// shows its suggestions, back once the popup closes.
    pub fn grows_for_popups(mut self) -> Self {
        self.grows_for_popups = true;
        self
    }

    /// The icon every window shows (taskbar, title bar, Dock).
    pub fn icon(mut self, icon: AppIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The first window without title bar or border.
    pub fn frameless(mut self) -> Self {
        self.style.frameless = true;
        self
    }

    /// Opens the first window centered along the top of the primary
    /// display's usable area, rather than where it last was.
    pub fn at_top_center(mut self) -> Self {
        self.style.top_center = true;
        self
    }

    /// Closes the first window on Escape, ahead of any popup, and when it
    /// stops being the active window, as a run box does.
    pub fn dismissible(mut self) -> Self {
        self.style.dismissible = true;
        self
    }

    /// The commands (with shortcuts and enabled state) the model offers.
    pub fn commands(mut self, commands: impl Fn(&M) -> Vec<Command> + 'static) -> Self {
        self.commands = Box::new(commands);
        self
    }

    pub fn windows(mut self, view: impl Fn(&M, &str) -> Widget + 'static) -> Self {
        self.window_view = Box::new(view);
        self
    }

    /// Reports window close requests as `UiMsg::CloseRequested` instead of
    /// closing, so documents can ask to save first.
    pub fn ask_before_close(mut self) -> Self {
        self.close_policy = ClosePolicy::Ask;
        self
    }

    /// Focuses the widget `id` when the first window opens.
    pub fn focus(mut self, id: &str) -> Self {
        self.initial_focus = id.to_string();
        self
    }

    /// Keeps one running instance: a later launch hands `files` (its own
    /// file arguments) to the first, which receives each as `UiMsg::Open`,
    /// and exits. Where the platform cannot claim an instance, every launch
    /// runs on its own.
    pub fn single_instance(mut self, files: Vec<String>) -> Self {
        self.instance_files = Some(files);
        self
    }

    /// Opens the first window and answers the session without running the
    /// loop (tests and embedding drive `step`).
    pub fn start(self) -> Result<Session, errors::Error> {
        self.begin(false)
    }

    // Opens the session; one that will `run` holds its jobs for the worker.
    fn begin(self, for_run: bool) -> Result<Session, errors::Error> {
        let mut session = app::start(&self.name, &self.app_id)?;
        if for_run {
            session.hold_jobs_for_run();
        }
        // The model is taken out for the length of each update, which owns it.
        let model = Rc::new(RefCell::new(Some((self.init)())));
        let (update, view, commands, window_view) =
            (self.update, self.view, self.commands, self.window_view);
        let (m1, m2, m3) = (model.clone(), model.clone(), model);
        session.set_shell(
            move |msg: UiMsg| {
                let mut slot = m1.borrow_mut();
                if let Some(m) = slot.take() {
                    *slot = Some(update(m, msg));
                }
            },
            move |key: &str| match m2.borrow().as_ref() {
                Some(m) if key == "main" => view(m),
                Some(m) => window_view(m, key),
                None => Widget::Empty,
            },
            move || m3.borrow().as_ref().map(&commands).unwrap_or_default(),
        );
        let opened = (|| {
            if let Some(icon) = &self.icon {
                session.set_icon(icon)?;
            }
            session.prepare_window_with(
                "main",
                &self.title,
                self.width,
                self.height,
                self.style,
            )?;
            session.set_close_policy("main", self.close_policy);
            if self.grows_for_popups {
                session.grow_for_popups("main", self.height)?;
            }
            if self.min_width > 0 || self.min_height > 0 {
                session.set_min_size("main", self.min_width, self.min_height)?;
            }
            if !self.initial_focus.is_empty() {
                session.focus(&self.initial_focus)?;
            }
            session.show_window("main")
        })();
        match opened {
            Ok(()) => Ok(session),
            Err(e) => {
                session.shutdown();
                Err(e)
            }
        }
    }

    /// Runs the application until its last window closes. With
    /// `GUI_EXPORT_ICON` set to a path, it instead writes its icon there as a
    /// PNG file and returns: packaging uses that for the desktop entry and
    /// the application bundle.
    pub fn run(self) -> Result<(), errors::Error> {
        if let Ok(path) = std::env::var("GUI_EXPORT_ICON") {
            let Some(icon) = &self.icon else {
                return Err(errors::new("this application sets no icon to export"));
            };
            return std::fs::write(&path, icon.to_png())
                .map_err(|e| errors::new(format!("writing the icon to {path}: {e}")));
        }
        let Some(files) = self.instance_files.clone() else {
            return self.begin(true)?.run();
        };
        let listener = match launch(&self.app_id, &files) {
            Launch::Forwarded => return Ok(()),
            Launch::Failed(error) => return Err(errors::new(error)),
            Launch::Alone => return self.begin(true)?.run(),
            Launch::Primary(l) => l,
        };
        let mut session = match self.begin(true) {
            Ok(s) => s,
            Err(e) => {
                listener.close();
                return Err(e);
            }
        };
        session.serve_instance(listener);
        session.run()
    }
}

impl<M> fmt::Debug for App<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("App")
            .field("name", &self.name)
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

/// How a launch of a single-instance application proceeds.
pub enum Launch {
    /// This process is the instance: hand the listener to
    /// `Session::serve_instance` and watch it beside the session.
    Primary(instance::Listener),
    /// The running instance received this launch's files; exit.
    Forwarded,
    /// Ownership or forwarding failed. Do not start a duplicate instance.
    Failed(String),
    /// The platform cannot keep one instance; run on its own.
    Alone,
}

/// Claims the application's single instance, handing `files` (relative
/// paths resolved against the working directory) to it when another
/// process already runs.
pub fn launch(app_id: &str, files: &[String]) -> Launch {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string());
    let uris: Vec<String> = files
        .iter()
        .map(|f| {
            instance::file_uri(&if paths::is_absolute(f) {
                f.clone()
            } else {
                paths::join(&cwd, f)
            })
        })
        .collect();
    let request = if uris.is_empty() {
        instance::Request::Activate
    } else {
        instance::Request::Open { uris }
    };
    match instance::claim(app_id, request) {
        Ok(instance::Claim::Forwarded) => Launch::Forwarded,
        Ok(instance::Claim::Primary(l)) => Launch::Primary(l),
        Err(error) => Launch::Failed(error.to_string()),
    }
}
