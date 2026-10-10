//! The main window: stream list, preview, status bar, menu, and the tray icon.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use rtspcam_core::config::{ConfigStore, StreamConfig};
use rtspcam_core::constants::APP_DISPLAY_NAME;
use rtspcam_core::{Config, Theme, paths};
use rtspcam_engine::{Activity, CameraManager, CameraStatus, VcamState};
use rtspcam_pipeline::StreamState;
use rtspcam_platform::{Autostart, InstanceLock};
use slint::winit_030::WinitWindowAccessor as _;
use slint::{
    CloseRequestResponse, ComponentHandle as _, Model as _, ModelRc, StandardListViewItem, Timer,
    TimerMode, VecModel,
};
use uuid::Uuid;

use super::core::Core;
use super::generated::{AppTray, MainWindow, Theme as UiTheme};
use super::preview::PreviewFeed;
use super::{discover_dialog, files, message, picture_dialog, settings_dialog, stream_dialog};

/// How the window starts.
pub(crate) struct RunOptions {
    pub(crate) store: ConfigStore,
    pub(crate) config: Config,
    pub(crate) manager: CameraManager,
    pub(crate) instance: Box<dyn InstanceLock>,
    pub(crate) autostart: Box<dyn Autostart>,
    /// Whether this OS can make virtual cameras (otherwise the streams only preview).
    pub(crate) cameras_supported: bool,
    /// `--minimized`: start in the tray (or minimized, if "Minimize to tray" is off).
    pub(crate) start_minimized: bool,
}

/// Where the answer to a message or question goes.
pub(crate) type Reply = Box<dyn FnOnce(bool)>;

/// Two presses on the same row within this time edit the stream.
const DOUBLE_PRESS: Duration = Duration::from_millis(500);

pub(crate) struct App {
    ui: slint::Weak<MainWindow>,
    pub(crate) core: Core,
    pub(crate) autostart: Box<dyn Autostart>,
    cameras_supported: bool,
    /// The list's rows, and the stream id of each.
    rows: Rc<VecModel<ModelRc<StandardListViewItem>>>,
    row_ids: RefCell<Vec<Uuid>>,
    preview: PreviewFeed,
    previewing: Cell<Option<Uuid>>,
    tray: RefCell<Option<AppTray>>,
    told_about_discord: Cell<bool>,
    last_press: Cell<Option<(i32, Instant)>>,
    timers: RefCell<Vec<Timer>>,
    /// Where the open message sheet's answer goes.
    pub(crate) message_reply: RefCell<Option<Reply>>,
    pub(crate) stream_dialog: RefCell<Option<stream_dialog::State>>,
    pub(crate) discover_dialog: RefCell<Option<discover_dialog::State>>,
    pub(crate) picture_dialog: RefCell<Option<picture_dialog::State>>,
    /// Bumped by each "Test connection", so a late answer for an earlier test is ignored.
    pub(crate) test_generation: Arc<AtomicU64>,
}

thread_local! {
    /// The running app, for results that arrive from other threads through the event loop.
    static CURRENT: RefCell<Weak<App>> = const { RefCell::new(Weak::new()) };
}

/// Runs `f` with the app, if it is still running. Call it on the UI thread (for example in a
/// closure passed to `slint::invoke_from_event_loop`).
pub(crate) fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = CURRENT.with(|c| c.borrow().upgrade());
    if let Some(app) = app {
        f(&app);
    }
}

/// Runs the window until the user quits, then shuts the cameras down.
pub(crate) fn run(options: RunOptions) -> anyhow::Result<()> {
    let RunOptions {
        store,
        config,
        manager,
        mut instance,
        autostart,
        cameras_supported,
        start_minimized,
    } = options;

    let ui = MainWindow::new()?;
    let rows = Rc::new(VecModel::default());
    ui.set_rows(ModelRc::from(rows.clone()));

    let app = Rc::new(App {
        ui: ui.as_weak(),
        core: Core::new(store, config, manager),
        autostart,
        cameras_supported,
        rows,
        row_ids: RefCell::default(),
        preview: PreviewFeed::start(ui.as_weak()),
        previewing: Cell::new(None),
        tray: RefCell::new(None),
        told_about_discord: Cell::new(false),
        last_press: Cell::new(None),
        timers: RefCell::default(),
        message_reply: RefCell::default(),
        stream_dialog: RefCell::default(),
        discover_dialog: RefCell::default(),
        picture_dialog: RefCell::default(),
        test_generation: Arc::default(),
    });
    CURRENT.with(|c| *c.borrow_mut() = Rc::downgrade(&app));

    app.connect(&ui);
    message::connect(&app);
    stream_dialog::connect(&app);
    discover_dialog::connect(&app);
    picture_dialog::connect(&app);
    settings_dialog::connect(&app);
    app.add_tray();

    // A second launch asks this copy to come forward.
    let weak = ui.as_weak();
    instance.on_show(Box::new(move || {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                show_window(&ui);
            }
        });
    }));

    // Logout and shutdown (SIGTERM on Linux) quit like the tray's Quit, so the cameras are
    // removed instead of left behind.
    if let Some(runtime) = app.core.runtime() {
        runtime.spawn(async {
            crate::termination_requested().await;
            tracing::info!("the system asked the app to quit");
            let _ = slint::invoke_from_event_loop(|| with_app(|app| app.quit()));
        });
    }

    app.refresh_list();
    app.start_timers();

    if start_minimized && app.core.config.borrow().settings.minimize_to_tray {
        // Only the tray icon.
    } else {
        ui.show()?;
        if start_minimized {
            ui.window().set_minimized(true);
        }
    }

    // Hidden in the tray the window isn't visible, so run until Quit, not until no window.
    slint::run_event_loop_until_quit()?;

    // The window is gone: take the tray icon away, then shut the cameras down in order.
    let _ = ui.hide();
    app.timers.borrow_mut().clear();
    app.tray.borrow_mut().take();
    app.preview.set(None);
    app.core.shutdown();
    drop(instance);
    Ok(())
}

/// Brings the window back from the tray or the taskbar, in front.
fn show_window(ui: &MainWindow) {
    let _ = ui.show();
    let window = ui.window();
    window.set_minimized(false);
    window.with_winit_window(|w| w.focus_window());
}

/// Whether the OS is in dark mode; `None` before the window first opens, or when the OS
/// doesn't say.
fn os_dark(ui: &MainWindow) -> Option<bool> {
    ui.window()
        .with_winit_window(|w| w.theme())
        .flatten()
        .map(|theme| theme == slint::winit_030::winit::window::Theme::Dark)
}

impl App {
    pub(crate) fn ui(&self) -> Option<MainWindow> {
        self.ui.upgrade()
    }

    fn connect(self: &Rc<Self>, ui: &MainWindow) {
        macro_rules! on {
            ($callback:ident, $method:ident) => {{
                let me = Rc::downgrade(self);
                ui.$callback(move || {
                    if let Some(app) = me.upgrade() {
                        app.$method();
                    }
                });
            }};
        }
        on!(on_add_stream, add_stream);
        on!(on_find_cameras, find_cameras);
        on!(on_import_streams, import_streams);
        on!(on_export_streams, export_streams);
        on!(on_edit_stream, edit_stream);
        on!(on_remove_stream, remove_stream);
        on!(on_toggle_stream, toggle_stream);
        on!(on_open_settings, open_settings);
        on!(on_toggle_theme, toggle_theme);
        on!(on_quit, quit);
        on!(on_open_logs, open_logs);
        on!(on_about, about);

        let me = Rc::downgrade(self);
        ui.on_row_selected(move |_| {
            if let Some(app) = me.upgrade() {
                app.on_selection();
            }
        });
        let me = Rc::downgrade(self);
        ui.on_row_pressed(move |row| {
            if let Some(app) = me.upgrade() {
                app.row_pressed(row);
            }
        });

        // Close (X) quits, like the tray's Quit.
        let me = Rc::downgrade(self);
        ui.window().on_close_requested(move || {
            if let Some(app) = me.upgrade() {
                app.quit();
            }
            CloseRequestResponse::HideWindow
        });
    }

    fn add_tray(self: &Rc<Self>) {
        let tray = match AppTray::new() {
            Ok(tray) => tray,
            Err(e) => {
                tracing::warn!(error = %e, "no tray icon");
                return;
            }
        };
        tray.set_tip(self.tooltip().into());
        let me = Rc::downgrade(self);
        tray.on_open(move || {
            if let Some(ui) = me.upgrade().and_then(|app| app.ui()) {
                show_window(&ui);
            }
        });
        let me = Rc::downgrade(self);
        tray.on_toggle_pause(move || {
            if let Some(app) = me.upgrade() {
                app.toggle_pause();
            }
        });
        let me = Rc::downgrade(self);
        tray.on_quit(move || {
            if let Some(app) = me.upgrade() {
                app.quit();
            }
        });
        *self.tray.borrow_mut() = Some(tray);
    }

    fn start_timers(self: &Rc<Self>) {
        let mut timers = self.timers.borrow_mut();
        let status = Timer::default();
        let me = Rc::downgrade(self);
        status.start(TimerMode::Repeated, Duration::from_millis(500), move || {
            if let Some(app) = me.upgrade() {
                app.tick_status();
            }
        });
        timers.push(status);
        // Minimizing has no event of its own; check often so the taskbar button goes quickly.
        let window = Timer::default();
        let me = Rc::downgrade(self);
        window.start(TimerMode::Repeated, Duration::from_millis(150), move || {
            if let Some(app) = me.upgrade() {
                app.tick_window();
            }
        });
        timers.push(window);
    }

    fn tooltip(&self) -> String {
        let statuses = self.core.statuses();
        let in_use = statuses.iter().filter(|s| s.clients > 0).count();
        let n = statuses.len();
        let cameras = if n == 1 { "camera" } else { "cameras" };
        if in_use > 0 {
            format!("{APP_DISPLAY_NAME} - {n} {cameras} active, {in_use} in use")
        } else {
            format!("{APP_DISPLAY_NAME} - {n} {cameras} active")
        }
    }

    fn quit(&self) {
        // The event loop ends; `run` then shuts everything down in order.
        if let Some(ui) = self.ui() {
            let _ = ui.hide();
        }
        let _ = slint::quit_event_loop();
    }

    fn toggle_pause(self: &Rc<Self>) {
        self.core.set_paused(!self.core.is_paused());
        self.tick_status();
    }

    // ----- the list -----

    fn selected_id(&self) -> Option<Uuid> {
        let row = usize::try_from(self.ui()?.get_current_row()).ok()?;
        self.row_ids.borrow().get(row).copied()
    }

    fn selected_stream(&self) -> Option<StreamConfig> {
        let id = self.selected_id()?;
        self.core.config.borrow().stream(id).cloned()
    }

    fn select(&self, id: Uuid) {
        let Some(ui) = self.ui() else { return };
        if let Some(row) = self.row_ids.borrow().iter().position(|r| *r == id) {
            ui.set_current_row(row as i32);
        }
    }

    /// Rebuilds the rows from the config, keeping the selection.
    pub(crate) fn refresh_list(self: &Rc<Self>) {
        let Some(ui) = self.ui() else { return };
        let keep = self.selected_id();
        {
            let config = self.core.config.borrow();
            ui.set_theme(match config.settings.theme {
                Theme::System => UiTheme::System,
                Theme::Light => UiTheme::Light,
                Theme::Dark => UiTheme::Dark,
            });
            let rows: Vec<ModelRc<StandardListViewItem>> = config
                .streams
                .iter()
                .map(|s| {
                    ModelRc::new(VecModel::from(vec![
                        StandardListViewItem::from(s.name.as_str()),
                        StandardListViewItem::default(),
                        StandardListViewItem::default(),
                        StandardListViewItem::default(),
                    ]))
                })
                .collect();
            self.rows.set_vec(rows);
            *self.row_ids.borrow_mut() = config.streams.iter().map(|s| s.id).collect();
        }
        self.update_rows();
        let ids = self.row_ids.borrow();
        let row = keep
            .and_then(|id| ids.iter().position(|r| *r == id))
            // Nothing was selected: show the first stream.
            .or_else(|| (!ids.is_empty()).then_some(0));
        drop(ids);
        ui.set_current_row(row.map_or(-1, |r| r as i32));
        self.on_selection();
    }

    /// Updates the text of every row, and the status bar, from the current statuses.
    fn update_rows(&self) {
        let Some(ui) = self.ui() else { return };
        let statuses = self.core.statuses();
        {
            let config = self.core.config.borrow();
            for (i, id) in self.row_ids.borrow().iter().enumerate() {
                let (Some(s), Some(row)) = (config.stream(*id), self.rows.row_data(i)) else {
                    continue;
                };
                let status = statuses.iter().find(|st| st.id == *id);
                let text = match (s.enabled, status) {
                    (false, _) => "Disabled".to_owned(),
                    (true, Some(st)) => status_text(st),
                    (true, None) => "Starting...".to_owned(),
                };
                let address = s.url();
                let enabled = if s.enabled { "Yes" } else { "No" };
                for (col, value) in [s.name.as_str(), address.as_str(), text.as_str(), enabled]
                    .into_iter()
                    .enumerate()
                {
                    if row.row_data(col).is_none_or(|item| item.text != value) {
                        row.set_row_data(col, StandardListViewItem::from(value));
                    }
                }
            }
        }

        let paused = if self.core.is_paused() {
            " (paused)"
        } else {
            ""
        };
        let cameras = if self.cameras_supported {
            let active = statuses
                .iter()
                .filter(|s| s.vcam == VcamState::Ready)
                .count();
            format!(
                "{active} camera{} active{paused}",
                if active == 1 { "" } else { "s" }
            )
        } else {
            format!("{}{paused}", rtspcam_platform::UNSUPPORTED_MESSAGE)
        };
        let problems = statuses.iter().filter(|s| s.has_problem()).count();
        ui.set_status_cameras(cameras.into());
        ui.set_status_errors(
            if problems == 0 {
                "No errors".to_owned()
            } else {
                format!("{problems} with errors")
            }
            .into(),
        );
    }

    fn on_selection(self: &Rc<Self>) {
        let Some(ui) = self.ui() else { return };
        let selected = self.selected_stream();
        ui.set_has_selection(selected.is_some());
        if let Some(s) = &selected {
            ui.set_toggle_text(if s.enabled { "Stop" } else { "Start" }.into());
        }
        self.sync_preview();
        self.update_info();
    }

    fn row_pressed(self: &Rc<Self>, row: i32) {
        let now = Instant::now();
        let double = self
            .last_press
            .get()
            .is_some_and(|(r, at)| r == row && now - at < DOUBLE_PRESS);
        self.last_press
            .set(if double { None } else { Some((row, now)) });
        if double {
            self.edit_stream();
        }
    }

    // ----- preview and details -----

    /// Previews the selected stream (which makes an on-demand stream connect) while the window
    /// can be seen, and nothing otherwise.
    fn sync_preview(&self) {
        let Some(ui) = self.ui() else { return };
        let window = ui.window();
        let visible = window.is_visible() && !window.is_minimized();
        let want = visible
            .then(|| self.selected_stream())
            .flatten()
            .filter(|s| s.enabled)
            .map(|s| s.id);
        if want == self.previewing.get() && (want.is_none() || self.preview.is_set()) {
            return;
        }
        // The camera may not exist yet (it was just enabled); the next tick tries again.
        let preview = want.and_then(|id| self.core.open_preview(id));
        self.previewing.set(preview.as_ref().and(want));
        self.preview.set(preview);
        ui.set_preview_image(slint::Image::default());
        self.update_preview_message();
    }

    fn update_preview_message(&self) {
        let Some(ui) = self.ui() else { return };
        let text = match self.preview.activity() {
            Some(activity) if !self.preview.has_picture() => activity.summary(),
            _ => String::new(),
        };
        if ui.get_preview_message() != text.as_str() {
            ui.set_preview_message(text.into());
        }
    }

    fn update_info(&self) {
        let Some(ui) = self.ui() else { return };
        let Some(s) = self.selected_stream() else {
            ui.set_info("Add a stream to get started.".into());
            return;
        };
        let status = self.core.status(s.id);
        let mut lines = vec![
            s.name.clone(),
            s.url(),
            format!(
                "Output: {}x{} @ {} fps, {}",
                s.output.width,
                s.output.height,
                s.output.fps,
                if s.on_demand {
                    "on demand"
                } else {
                    "always connected"
                }
            ),
            String::new(),
        ];
        match status {
            None if !s.enabled => lines.push("Disabled. Press Start to turn it on.".to_owned()),
            None => lines.push("Starting...".to_owned()),
            Some(st) => {
                lines.push(st.activity.summary());
                if let Activity::Running(StreamState::Streaming(stats)) = &st.activity {
                    lines.push(format!(
                        "{} {}x{}, {:.1} fps, {:.1} Mbit/s, {} ms, {}",
                        stats.codec,
                        stats.width,
                        stats.height,
                        stats.fps,
                        stats.bitrate as f64 / 1e6,
                        stats.latency.as_millis(),
                        stats.decoder
                    ));
                }
                if let Some(hint) = st.activity.hint() {
                    lines.push(hint);
                }
                match &st.vcam {
                    VcamState::Pending => lines.push("Creating the camera...".to_owned()),
                    VcamState::Ready => lines.push(format!(
                        "Camera \"{}\" is available to apps{}.",
                        s.name,
                        if st.clients > 0 {
                            format!(" ({} using it)", st.clients)
                        } else {
                            String::new()
                        }
                    )),
                    VcamState::Unsupported(why) => lines.push(format!("{why}.")),
                    VcamState::Failed(why) => {
                        lines.push(format!("The camera could not be created: {why}"));
                    }
                }
            }
        }
        let text = lines.join("\n");
        if ui.get_info() != text.as_str() {
            ui.set_info(text.into());
        }
    }

    // ----- timers -----

    fn tick_status(self: &Rc<Self>) {
        // Edits made to config.json by hand.
        if let Some(config) = self.core.take_external_change() {
            *self.core.config.borrow_mut() = config;
            self.refresh_list();
        } else {
            self.update_rows();
            self.update_info();
        }
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.set_tip(self.tooltip().into());
            tray.set_paused(self.core.is_paused());
        }
        // Do not hold streams open for a window nobody can see, and open the preview of a
        // camera that has just been created.
        self.sync_preview();
        self.update_preview_message();
    }

    fn tick_window(&self) {
        let Some(ui) = self.ui() else { return };
        let window = ui.window();
        if window.is_visible()
            && window.is_minimized()
            && self.core.config.borrow().settings.minimize_to_tray
        {
            // Leaves the taskbar and the app switcher; the tray icon brings it back.
            let _ = ui.hide();
            self.sync_preview();
        }
        if let Some(dark) = os_dark(&ui) {
            ui.set_os_dark(dark);
        }
        // The preview is scaled to the box's size in physical pixels.
        let scale = window.scale_factor();
        self.preview.set_target(
            (ui.get_preview_width() * scale) as u32,
            (ui.get_preview_height() * scale) as u32,
        );
    }

    // ----- actions -----

    fn add_stream(self: &Rc<Self>) {
        let others = self.core.config.borrow().streams.clone();
        stream_dialog::open(
            self,
            StreamConfig::default(),
            stream_dialog::Mode::Add,
            others,
            Box::new(|app, stream| app.add_new(stream)),
        );
    }

    /// Scans for ONVIF cameras, then lets the user review the chosen one like any new stream.
    fn find_cameras(self: &Rc<Self>) {
        let others = self.core.config.borrow().streams.clone();
        discover_dialog::open(
            self,
            others,
            Box::new(|app, found| {
                let others = app.core.config.borrow().streams.clone();
                stream_dialog::open(
                    app,
                    found,
                    stream_dialog::Mode::Add,
                    others,
                    Box::new(|app, stream| app.add_new(stream)),
                );
            }),
        );
    }

    /// Adds the streams from a file chosen by the user.
    fn import_streams(self: &Rc<Self>) {
        let Some(ui) = self.ui() else { return };
        let me = Rc::downgrade(self);
        let _ = slint::spawn_local(async move {
            let Some(path) = files::open_json(&ui).await else {
                return;
            };
            let Some(app) = me.upgrade() else { return };
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => {
                    let text = format!("Could not read {}: {e}", path.display());
                    message::inform(&app, "Import streams", &text);
                    return;
                }
            };
            let mut config = app.core.config.borrow().clone();
            let report = match rtspcam_core::config::import_streams(&mut config, &text) {
                Ok(report) => report,
                Err(e) => {
                    message::inform(&app, "Import streams", &e.to_string());
                    return;
                }
            };
            if !report.added.is_empty() && !app.save(config) {
                return;
            }
            let summary = report.summary().replace("\r\n", "\n");
            message::inform(&app, "Import streams", &summary);
        });
    }

    /// Writes all streams, without passwords, to a file chosen by the user.
    fn export_streams(self: &Rc<Self>) {
        let streams = self.core.config.borrow().streams.clone();
        if streams.is_empty() {
            message::inform(self, "Export streams", "There are no streams to export.");
            return;
        }
        let Some(ui) = self.ui() else { return };
        let me = Rc::downgrade(self);
        let _ = slint::spawn_local(async move {
            let Some(path) = files::save_json(&ui, "rtspcam-streams.json").await else {
                return;
            };
            let Some(app) = me.upgrade() else { return };
            let result = rtspcam_core::config::export_streams(&streams)
                .map_err(|e| e.to_string())
                .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
            let text = match result {
                Ok(()) => format!(
                    "Exported {} stream{} to {}.\n\nPasswords are not included.",
                    streams.len(),
                    if streams.len() == 1 { "" } else { "s" },
                    path.display()
                ),
                Err(e) => format!("Could not write {}: {e}", path.display()),
            };
            message::inform(&app, "Export streams", &text);
        });
    }

    fn add_new(self: &Rc<Self>, stream: StreamConfig) {
        let mut config = self.core.config.borrow().clone();
        let id = stream.id;
        config.streams.push(stream);
        if self.save(config) {
            self.select(id);
            self.on_selection();
            if self.cameras_supported && !self.told_about_discord.replace(true) {
                message::inform(
                    self,
                    "Camera added",
                    "Restart Discord if the camera doesn't appear in Settings > Voice & Video.",
                );
            }
        }
    }

    fn edit_stream(self: &Rc<Self>) {
        let Some(existing) = self.selected_stream() else {
            return;
        };
        let others = self.core.config.borrow().streams.clone();
        stream_dialog::open(
            self,
            existing,
            stream_dialog::Mode::Edit,
            others,
            Box::new(|app, updated| {
                let mut config = app.core.config.borrow().clone();
                if let Some(slot) = config.stream_mut(updated.id) {
                    *slot = updated;
                    app.save(config);
                }
            }),
        );
    }

    fn remove_stream(self: &Rc<Self>) {
        let Some(stream) = self.selected_stream() else {
            return;
        };
        let me = Rc::downgrade(self);
        message::ask(
            self,
            "Remove stream",
            &format!(
                "Remove \"{}\"?\nThe camera disappears from every app that uses it.",
                stream.name
            ),
            move |yes| {
                let Some(app) = me.upgrade() else { return };
                if yes {
                    let mut config = app.core.config.borrow().clone();
                    config.streams.retain(|s| s.id != stream.id);
                    app.save(config);
                }
            },
        );
    }

    fn toggle_stream(self: &Rc<Self>) {
        let Some(stream) = self.selected_stream() else {
            return;
        };
        let mut config = self.core.config.borrow().clone();
        if let Some(s) = config.stream_mut(stream.id) {
            s.enabled = !s.enabled;
            self.save(config);
        }
    }

    fn open_settings(self: &Rc<Self>) {
        settings_dialog::open(self);
    }

    /// Switches between light and dark. Picking what the OS uses follows the OS again.
    fn toggle_theme(self: &Rc<Self>) {
        let Some(ui) = self.ui() else { return };
        let dark = !ui.get_dark();
        let mut config = self.core.config.borrow().clone();
        config.settings.theme = if os_dark(&ui) == Some(dark) {
            Theme::System
        } else if dark {
            Theme::Dark
        } else {
            Theme::Light
        };
        self.save(config);
    }

    /// Saves and applies a changed config; shows the error if saving failed.
    pub(crate) fn save(self: &Rc<Self>, config: Config) -> bool {
        match self.core.save(config) {
            Ok(()) => {
                self.refresh_list();
                true
            }
            Err(e) => {
                message::inform(
                    self,
                    APP_DISPLAY_NAME,
                    &format!("Could not save the settings: {e}"),
                );
                false
            }
        }
    }

    fn open_logs(self: &Rc<Self>) {
        let opened = paths::log_dir().map_err(|e| e.to_string()).and_then(|dir| {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            rtspcam_platform::desktop::open_folder(&dir)
                .map_err(|e| format!("Could not open {}: {e}", dir.display()))
        });
        if let Err(e) = opened {
            message::inform(self, "Open logs folder", &e);
        }
    }

    fn about(self: &Rc<Self>) {
        message::inform(
            self,
            &format!("About {APP_DISPLAY_NAME}"),
            &format!(
                "{APP_DISPLAY_NAME} {}\n\nTurns RTSP streams into virtual webcams.",
                env!("CARGO_PKG_VERSION")
            ),
        );
    }
}

fn status_text(st: &CameraStatus) -> String {
    match &st.vcam {
        VcamState::Failed(why) => format!("Camera error: {why}"),
        _ if st.clients > 0 && st.activity.is_streaming() => {
            format!("In use by an app - {}", st.activity.summary())
        }
        _ => st.activity.summary(),
    }
}
