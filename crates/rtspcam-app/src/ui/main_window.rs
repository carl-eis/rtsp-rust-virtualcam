//! The main window: stream list, preview pane, status bar, menu, tray icon.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use rtspcam_core::config::StreamConfig;
use rtspcam_core::constants::APP_DISPLAY_NAME;
use rtspcam_pipeline::{Frame, StreamState};
use uuid::Uuid;
use winsafe::{self as w, co, gui, prelude::*};

use super::Core;
use super::preview::PreviewPane;
use super::settings_dialog::SettingsDialog;
use super::stream_dialog::StreamDialog;
use super::tray::{AppIcon, Tray};
use crate::Preview;
use crate::status::{Activity, CameraStatus, VcamState};

/// Window messages posted to the main window.
pub(crate) const WM_TRAY: u32 = 0x8000 + 2;
pub(crate) const WM_SHOW_APP: u32 = 0x8000 + 3;

/// Where the single-instance listener finds the function that shows the window.
pub(crate) type ShowSlot = Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send>>>>;

const ID_ADD: u16 = 1001;
const ID_EDIT: u16 = 1002;
const ID_REMOVE: u16 = 1003;
const ID_TOGGLE: u16 = 1004;
const ID_SETTINGS: u16 = 1005;
const ID_QUIT: u16 = 1006;
const ID_LOGS: u16 = 1007;
const ID_ABOUT: u16 = 1008;
const ID_OPEN: u16 = 1009;
const ID_PAUSE: u16 = 1010;

const TIMER_STATUS: usize = 1;
const TIMER_PREVIEW: usize = 2;

#[derive(Clone)]
pub(crate) struct MainWindow {
    wnd: gui::WindowMain,
    btn_add: gui::Button,
    btn_edit: gui::Button,
    btn_remove: gui::Button,
    btn_toggle: gui::Button,
    btn_settings: gui::Button,
    list: gui::ListView<()>,
    preview: PreviewPane,
    info: gui::Label,
    status: gui::StatusBar,
    core: Rc<Core>,
    state: Rc<State>,
}

#[derive(Default)]
struct State {
    /// Stream id of every list row, in order.
    rows: RefCell<Vec<Uuid>>,
    /// The stream being previewed. Dropping the preview releases an on-demand stream.
    preview: RefCell<Option<(Uuid, Preview)>>,
    /// The picture last painted, to skip repainting an unchanged one.
    painted: RefCell<Option<Arc<Frame>>>,
    tray: RefCell<Option<Tray>>,
    icon: RefCell<Option<AppIcon>>,
    told_about_tray: Cell<bool>,
    told_about_discord: Cell<bool>,
    start_hidden: Cell<bool>,
    show_slot: RefCell<Option<ShowSlot>>,
}

impl MainWindow {
    /// Builds the window and runs the message loop until the window closes.
    pub(crate) fn run(core: Core, start_minimized: bool, show_slot: ShowSlot) -> w::AnyResult<i32> {
        let menu = build_menu()?;
        let wnd = gui::WindowMain::new(gui::WindowMainOpts {
            title: APP_DISPLAY_NAME,
            size: (980, 600),
            style: gui::WindowMainOpts::default().style
                | co::WS::SIZEBOX
                | co::WS::MINIMIZEBOX
                | co::WS::MAXIMIZEBOX,
            menu,
            ..Default::default()
        });

        let button = |text: &str, i: i32| {
            gui::Button::new(
                &wnd,
                gui::ButtonOpts {
                    text,
                    position: (10 + i * 112, 10),
                    width: 106,
                    height: 28,
                    ..Default::default()
                },
            )
        };
        let btn_add = button("&Add stream", 0);
        let btn_edit = button("&Edit", 1);
        let btn_remove = button("&Remove", 2);
        let btn_toggle = button("&Stop", 3);
        let btn_settings = button("Se&ttings", 4);

        let list = gui::ListView::<()>::new(
            &wnd,
            gui::ListViewOpts {
                position: (10, 48),
                size: (960, 220),
                control_style: co::LVS::REPORT | co::LVS::SINGLESEL | co::LVS::SHOWSELALWAYS,
                control_ex_style: co::LVS_EX::FULLROWSELECT | co::LVS_EX::DOUBLEBUFFER,
                resize_behavior: (gui::Horz::Resize, gui::Vert::None),
                columns: &[
                    ("Name", 190),
                    ("Address", 250),
                    ("Status", 420),
                    ("Enabled", 80),
                ],
                ..Default::default()
            },
        );
        let preview = PreviewPane::new(&wnd, (10, 280), (480, 270));
        let info = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "",
                position: (504, 280),
                size: (466, 270),
                resize_behavior: (gui::Horz::Resize, gui::Vert::None),
                ..Default::default()
            },
        );
        let status = gui::StatusBar::new(
            &wnd,
            &[gui::SbPart::Proportional(3), gui::SbPart::Proportional(1)],
        );

        let state = Rc::new(State::default());
        state.start_hidden.set(start_minimized);
        *state.show_slot.borrow_mut() = Some(show_slot);
        let me = Self {
            wnd,
            btn_add,
            btn_edit,
            btn_remove,
            btn_toggle,
            btn_settings,
            list,
            preview,
            info,
            status,
            core: Rc::new(core),
            state,
        };
        me.events();

        let show = if start_minimized {
            if me.core.config.borrow().settings.minimize_to_tray {
                co::SW::HIDE
            } else {
                co::SW::SHOWMINNOACTIVE
            }
        } else {
            co::SW::SHOW
        };
        let code = me.wnd.run_main(Some(show));

        // The window is gone: take the tray icon away, then shut the cameras down in order.
        me.state.tray.borrow_mut().take();
        me.core.shutdown();
        code
    }

    fn events(&self) {
        let me = self.clone();
        self.wnd.on().wm_create(move |_| {
            me.on_create();
            Ok(0)
        });

        let me = self.clone();
        self.wnd.on().wm_timer(TIMER_STATUS, move || {
            me.tick_status();
            Ok(())
        });
        let me = self.clone();
        self.wnd.on().wm_timer(TIMER_PREVIEW, move || {
            me.tick_preview();
            Ok(())
        });

        let me = self.clone();
        self.wnd.on().wm_size(move |p| {
            if p.request == co::SIZE_R::MINIMIZED {
                me.on_minimized();
            }
            Ok(())
        });

        // Explorer restarted: the shell forgot our icon.
        let created = register_message("TaskbarCreated");
        let me = self.clone();
        self.wnd.on().wm(custom_wm(created), move |_| {
            me.add_tray();
            Ok(0)
        });
        let me = self.clone();
        self.wnd.on().wm(custom_wm(WM_TRAY), move |p| {
            me.on_tray(p.lparam as u32 & 0xffff);
            Ok(0)
        });
        let me = self.clone();
        self.wnd.on().wm(custom_wm(WM_SHOW_APP), move |_| {
            me.show_window();
            Ok(0)
        });

        let me = self.clone();
        self.wnd.on().wm_destroy(move || {
            me.state.preview.borrow_mut().take();
            Ok(())
        });

        macro_rules! command {
            ($id:expr, $btn:expr, $method:ident) => {{
                let me = self.clone();
                self.wnd.on().wm_command_acc_menu($id, move || {
                    me.$method();
                    Ok(())
                });
                if let Some(btn) = $btn {
                    let me = self.clone();
                    btn.on().bn_clicked(move || {
                        me.$method();
                        Ok(())
                    });
                }
            }};
        }
        command!(ID_ADD, Some(&self.btn_add), add_stream);
        command!(ID_EDIT, Some(&self.btn_edit), edit_stream);
        command!(ID_REMOVE, Some(&self.btn_remove), remove_stream);
        command!(ID_TOGGLE, Some(&self.btn_toggle), toggle_stream);
        command!(ID_SETTINGS, Some(&self.btn_settings), open_settings);
        command!(ID_QUIT, None::<&gui::Button>, quit);
        command!(ID_LOGS, None::<&gui::Button>, open_logs);
        command!(ID_ABOUT, None::<&gui::Button>, about);
        command!(ID_OPEN, None::<&gui::Button>, show_window);
        command!(ID_PAUSE, None::<&gui::Button>, toggle_pause);

        let me = self.clone();
        self.list.on().lvn_item_changed(move |_| {
            me.on_selection();
            Ok(())
        });
        let me = self.clone();
        self.list.on().nm_dbl_clk(move |_| {
            me.edit_stream();
            Ok(())
        });
    }

    // ----- lifecycle -----

    fn on_create(&self) {
        if let Some(slot) = self.state.show_slot.borrow().as_ref() {
            *slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(Box::new(Self::show_requester(self.wnd.hwnd())));
        }
        match AppIcon::create(32) {
            Ok(icon) => {
                // SAFETY: the icon handle stays valid for as long as `State::icon` holds it, which
                // outlives the window.
                unsafe {
                    self.wnd.hwnd().SendMessage(w::msg::WmSetIcon {
                        size: co::ICON_SZ::BIG,
                        hicon: w::HICON::from_ptr(icon.0.0),
                    });
                }
                *self.state.icon.borrow_mut() = Some(icon);
            }
            Err(e) => tracing::warn!(error = %e, "could not create the app icon"),
        }
        self.add_tray();
        self.refresh_list();
        let _ = self.wnd.hwnd().SetTimer(TIMER_STATUS, 500, None);
        let _ = self.wnd.hwnd().SetTimer(TIMER_PREVIEW, 40, None);
        self.on_selection();
    }

    fn add_tray(&self) {
        let icon = self.state.icon.borrow();
        let Some(icon) = icon.as_ref() else { return };
        let hwnd = windows::Win32::Foundation::HWND(self.wnd.hwnd().ptr());
        let mut tray = Tray::new(hwnd, icon.0, WM_TRAY);
        tray.add(&self.tooltip());
        *self.state.tray.borrow_mut() = Some(tray);
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

    fn on_minimized(&self) {
        if !self.core.config.borrow().settings.minimize_to_tray {
            return;
        }
        // Leaves the taskbar and Alt-Tab; the tray icon brings it back.
        self.wnd.hwnd().ShowWindow(co::SW::HIDE);
        if !self.state.told_about_tray.replace(true)
            && let Some(tray) = self.state.tray.borrow().as_ref()
        {
            tray.balloon(APP_DISPLAY_NAME, "RTSP Cam is still running in the tray.");
        }
    }

    fn show_window(&self) {
        let hwnd = self.wnd.hwnd();
        hwnd.ShowWindow(co::SW::RESTORE);
        hwnd.ShowWindow(co::SW::SHOW);
        let _ = hwnd.SetForegroundWindow();
    }

    fn quit(&self) {
        // Same as the close button: the window goes, the message loop ends, `run` shuts down.
        // SAFETY: WM_CLOSE carries no pointers; it is the same message the close button sends.
        unsafe { self.wnd.hwnd().SendMessage(w::msg::WmClose {}) };
    }

    fn on_tray(&self, event: u32) {
        const LBUTTONDBLCLK: u32 = 0x0203;
        const RBUTTONUP: u32 = 0x0205;
        match event {
            LBUTTONDBLCLK => self.show_window(),
            RBUTTONUP => self.tray_menu(),
            _ => {}
        }
    }

    fn tray_menu(&self) {
        let Ok(menu) = w::HMENU::CreatePopupMenu() else {
            return;
        };
        let pause = if self.core.is_paused() {
            "Resume all"
        } else {
            "Pause all"
        };
        let _ = menu.append_item(&[w::MenuItem::Entry {
            cmd_id: ID_OPEN,
            text: "Open",
        }]);
        let _ = menu.append_item(&[w::MenuItem::Entry {
            cmd_id: ID_PAUSE,
            text: pause,
        }]);
        let _ = menu.append_item(&[w::MenuItem::Separator]);
        let _ = menu.append_item(&[w::MenuItem::Entry {
            cmd_id: ID_QUIT,
            text: "Quit",
        }]);
        let hwnd = self.wnd.hwnd();
        if let Ok(pt) = w::GetCursorPos() {
            // Required so the menu closes when the user clicks elsewhere.
            let _ = hwnd.SetForegroundWindow();
            let _ = menu.TrackPopupMenu(co::TPM::RIGHTBUTTON, pt, hwnd);
        }
    }

    fn toggle_pause(&self) {
        self.core.set_paused(!self.core.is_paused());
        self.tick_status();
    }

    // ----- the list -----

    fn selected_id(&self) -> Option<Uuid> {
        let item = self.list.items().iter_selected().next()?;
        self.state.rows.borrow().get(item.index() as usize).copied()
    }

    fn selected_stream(&self) -> Option<StreamConfig> {
        let id = self.selected_id()?;
        self.core.config.borrow().stream(id).cloned()
    }

    /// Rebuilds the rows from the config, keeping the selection.
    fn refresh_list(&self) {
        let keep = self.selected_id();
        self.list.set_redraw(false);
        let _ = self.list.items().delete_all();
        let config = self.core.config.borrow();
        let mut rows = self.state.rows.borrow_mut();
        rows.clear();
        for s in &config.streams {
            let _ = self
                .list
                .items()
                .add(&[s.name.as_str(), "", "", ""], None, ());
            rows.push(s.id);
        }
        drop(rows);
        drop(config);
        self.update_rows();
        if let Some(id) = keep
            && let Some(i) = self.state.rows.borrow().iter().position(|r| *r == id)
        {
            let _ = self.list.items().get(i as u32).select(true);
        } else if !self.state.rows.borrow().is_empty() {
            // Nothing was selected: show the first stream.
            let _ = self.list.items().get(0).select(true);
        }
        self.list.set_redraw(true);
        self.on_selection();
    }

    /// Updates the text of every row from the current statuses.
    fn update_rows(&self) {
        let config = self.core.config.borrow();
        let statuses = self.core.statuses();
        for (i, id) in self.state.rows.borrow().iter().enumerate() {
            let Some(s) = config.stream(*id) else {
                continue;
            };
            let item = self.list.items().get(i as u32);
            let status = statuses.iter().find(|st| st.id == *id);
            let text = match (s.enabled, status) {
                (false, _) => "Disabled".to_owned(),
                (true, Some(st)) => status_text(st),
                (true, None) => "Starting...".to_owned(),
            };
            let address = s.url();
            let enabled = if s.enabled { "Yes" } else { "No" };
            for (col, value) in [
                (0, s.name.as_str()),
                (1, address.as_str()),
                (2, text.as_str()),
                (3, enabled),
            ] {
                if item.text(col) != value {
                    let _ = item.set_text(col, value);
                }
            }
        }
        drop(config);

        let statuses = self.core.statuses();
        let active = statuses
            .iter()
            .filter(|s| s.vcam == VcamState::Ready)
            .count();
        let problems = statuses.iter().filter(|s| s.has_problem()).count();
        self.status.parts().set_texts(&[
            Some(format!(
                "{active} camera{} active{}",
                if active == 1 { "" } else { "s" },
                if self.core.is_paused() {
                    " (paused)"
                } else {
                    ""
                }
            )),
            Some(if problems == 0 {
                "No errors".to_owned()
            } else {
                format!("{problems} with errors")
            }),
        ]);
    }

    fn on_selection(&self) {
        let selected = self.selected_stream();
        for b in [&self.btn_edit, &self.btn_remove, &self.btn_toggle] {
            b.hwnd().EnableWindow(selected.is_some());
        }
        if let Some(s) = &selected {
            let _ =
                self.btn_toggle
                    .hwnd()
                    .SetWindowText(if s.enabled { "&Stop" } else { "&Start" });
        }

        // Preview the selected stream (which makes an on-demand stream connect).
        let want = selected.filter(|s| s.enabled).map(|s| s.id);
        let have = self.state.preview.borrow().as_ref().map(|(id, _)| *id);
        if want != have {
            *self.state.preview.borrow_mut() = None;
            *self.state.painted.borrow_mut() = None;
            if let Some(id) = want
                && let Some(p) = self.core.open_preview(id)
            {
                *self.state.preview.borrow_mut() = Some((id, p));
            }
            self.preview.show(None, "");
        }
        self.tick_preview();
        self.update_info();
    }

    fn update_info(&self) {
        let Some(s) = self.selected_stream() else {
            set_label(&self.info, "Add a stream to get started.");
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
                    VcamState::Failed(why) => {
                        lines.push(format!("The camera could not be created: {why}"));
                    }
                }
            }
        }
        set_label(&self.info, &lines.join("\r\n"));
    }

    // ----- timers -----

    fn tick_status(&self) {
        // Edits made to config.json by hand.
        if let Some(config) = self.core.take_external_change() {
            *self.core.config.borrow_mut() = config;
            self.refresh_list();
        } else {
            self.update_rows();
            self.update_info();
        }
        if let Some(tray) = self.state.tray.borrow().as_ref() {
            tray.set_tip(&self.tooltip());
        }
        // Do not hold streams open for a window nobody can see.
        let visible = self.wnd.hwnd().IsWindowVisible() && !self.wnd.hwnd().IsIconic();
        if !visible {
            if self.state.preview.borrow_mut().take().is_some() {
                *self.state.painted.borrow_mut() = None;
            }
        } else if self.state.preview.borrow().is_none() {
            self.on_selection();
        }
    }

    fn tick_preview(&self) {
        let guard = self.state.preview.borrow();
        let Some((_, preview)) = guard.as_ref() else {
            return;
        };
        match preview.latest() {
            Some(frame) => {
                let same = self
                    .state
                    .painted
                    .borrow()
                    .as_ref()
                    .is_some_and(|p| Arc::ptr_eq(p, &frame));
                if !same {
                    self.preview.show(Some(&frame), "");
                    *self.state.painted.borrow_mut() = Some(frame);
                }
            }
            None => {
                self.state.painted.borrow_mut().take();
                self.preview.show(None, &preview.activity().summary());
            }
        }
    }

    // ----- actions -----

    fn add_stream(&self) {
        let others = self.core.config.borrow().streams.clone();
        let Some(stream) = StreamDialog::run(&self.wnd, None, others, self.core.runtime()) else {
            return;
        };
        let mut config = self.core.config.borrow().clone();
        let id = stream.id;
        config.streams.push(stream);
        if self.save(config) {
            if let Some(i) = self.state.rows.borrow().iter().position(|r| *r == id) {
                let _ = self.list.items().get(i as u32).select(true);
            }
            if !self.state.told_about_discord.replace(true) {
                let _ = self.wnd.hwnd().MessageBox(
                    "Restart Discord if the camera doesn't appear in Settings > Voice & Video.",
                    "Camera added",
                    co::MB::OK | co::MB::ICONINFORMATION,
                );
            }
        }
    }

    fn edit_stream(&self) {
        let Some(existing) = self.selected_stream() else {
            return;
        };
        let others = self.core.config.borrow().streams.clone();
        let Some(updated) =
            StreamDialog::run(&self.wnd, Some(&existing), others, self.core.runtime())
        else {
            return;
        };
        let mut config = self.core.config.borrow().clone();
        if let Some(slot) = config.stream_mut(updated.id) {
            *slot = updated;
            self.save(config);
        }
    }

    fn remove_stream(&self) {
        let Some(stream) = self.selected_stream() else {
            return;
        };
        let answer = self.wnd.hwnd().MessageBox(
            &format!(
                "Remove \"{}\"?\r\nThe camera disappears from every app that uses it.",
                stream.name
            ),
            "Remove stream",
            co::MB::YESNO | co::MB::ICONQUESTION,
        );
        if answer != Ok(co::DLGID::YES) {
            return;
        }
        let mut config = self.core.config.borrow().clone();
        config.streams.retain(|s| s.id != stream.id);
        self.save(config);
    }

    fn toggle_stream(&self) {
        let Some(stream) = self.selected_stream() else {
            return;
        };
        let mut config = self.core.config.borrow().clone();
        if let Some(s) = config.stream_mut(stream.id) {
            s.enabled = !s.enabled;
            self.save(config);
        }
    }

    fn open_settings(&self) {
        let current = self.core.config.borrow().settings.clone();
        let Some(settings) = SettingsDialog::run(&self.wnd, &current) else {
            return;
        };
        if settings.start_with_windows != current.start_with_windows
            && let Err(e) = crate::autostart::set(settings.start_with_windows)
        {
            let _ = self.wnd.hwnd().MessageBox(
                &format!("Could not change the Windows startup entry: {e}"),
                "Settings",
                co::MB::OK | co::MB::ICONWARNING,
            );
        }
        let mut config = self.core.config.borrow().clone();
        config.settings = settings;
        self.save(config);
    }

    /// Saves and applies a changed config; shows the error if saving failed.
    fn save(&self, config: rtspcam_core::Config) -> bool {
        match self.core.save(config) {
            Ok(()) => {
                self.refresh_list();
                true
            }
            Err(e) => {
                let _ = self.wnd.hwnd().MessageBox(
                    &format!("Could not save the settings: {e}"),
                    APP_DISPLAY_NAME,
                    co::MB::OK | co::MB::ICONERROR,
                );
                false
            }
        }
    }

    fn open_logs(&self) {
        if let Ok(dir) = rtspcam_core::paths::log_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
        }
    }

    fn about(&self) {
        let _ = self.wnd.hwnd().MessageBox(
            &format!(
                "{APP_DISPLAY_NAME} {}\r\n\r\nTurns RTSP streams into Windows virtual webcams.",
                env!("CARGO_PKG_VERSION")
            ),
            &format!("About {APP_DISPLAY_NAME}"),
            co::MB::OK | co::MB::ICONINFORMATION,
        );
    }

    /// Lets other threads ask the window to show itself (a second copy was started).
    pub(crate) fn show_requester(hwnd: &w::HWND) -> impl Fn() + Send + 'static {
        let raw = hwnd.ptr() as usize;
        move || {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
            // SAFETY: posting a message to a window handle is safe even if the window is gone.
            unsafe {
                let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_SHOW_APP, WPARAM(0), LPARAM(0));
            }
        }
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

fn build_menu() -> w::AnyResult<w::HMENU> {
    let menu = w::HMENU::CreateMenu()?;
    let file = w::HMENU::CreatePopupMenu()?;
    file.append_item(&[w::MenuItem::Entry {
        cmd_id: ID_SETTINGS,
        text: "&Settings...",
    }])?;
    file.append_item(&[w::MenuItem::Separator])?;
    file.append_item(&[w::MenuItem::Entry {
        cmd_id: ID_QUIT,
        text: "&Quit\tAlt+F4",
    }])?;
    let help = w::HMENU::CreatePopupMenu()?;
    help.append_item(&[w::MenuItem::Entry {
        cmd_id: ID_LOGS,
        text: "Open &logs folder",
    }])?;
    help.append_item(&[w::MenuItem::Entry {
        cmd_id: ID_ABOUT,
        text: "&About",
    }])?;
    menu.append_item(&[w::MenuItem::Submenu {
        submenu: &file,
        text: "&File",
    }])?;
    menu.append_item(&[w::MenuItem::Submenu {
        submenu: &help,
        text: "&Help",
    }])?;
    Ok(menu)
}

fn register_message(name: &str) -> u32 {
    w::RegisterWindowMessage(name).unwrap_or(0xC000)
}

fn set_label(label: &gui::Label, text: &str) {
    let _ = label.hwnd().SetWindowText(text);
}

/// A message id from `WM_APP` or `RegisterWindowMessage`.
fn custom_wm(id: u32) -> co::WM {
    // SAFETY: `co::WM` is a plain newtype over the message number; any value is a valid
    // message id.
    unsafe { co::WM::from_raw(id) }
}
