//! The Settings dialog: minimize to tray, start at login, log level, theme.

use std::rc::Rc;

use rtspcam_core::{LogLevel, Theme};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};

use super::app::App;
use super::generated::SettingsDialog;
use super::message;

const LEVELS: [(&str, LogLevel); 5] = [
    ("Error", LogLevel::Error),
    ("Warning", LogLevel::Warn),
    ("Info", LogLevel::Info),
    ("Debug", LogLevel::Debug),
    ("Trace", LogLevel::Trace),
];

const THEMES: [(&str, Theme); 3] = [
    ("Same as the system", Theme::System),
    ("Light", Theme::Light),
    ("Dark", Theme::Dark),
];

/// Shows the dialog with the current settings.
pub(crate) fn open(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<SettingsDialog>();
    let current = app.core.config.borrow().settings.clone();
    d.set_minimize_to_tray(current.minimize_to_tray);
    d.set_autostart(current.start_with_windows);
    d.set_autostart_label(app.autostart.label().into());
    d.set_levels(ModelRc::new(VecModel::from(
        LEVELS
            .iter()
            .map(|(name, _)| SharedString::from(*name))
            .collect::<Vec<_>>(),
    )));
    d.set_level(
        LEVELS
            .iter()
            .position(|(_, l)| *l == current.log_level)
            .unwrap_or(2) as i32,
    );
    d.set_themes(ModelRc::new(VecModel::from(
        THEMES
            .iter()
            .map(|(name, _)| SharedString::from(*name))
            .collect::<Vec<_>>(),
    )));
    d.set_theme(
        THEMES
            .iter()
            .position(|(_, t)| *t == current.theme)
            .unwrap_or(0) as i32,
    );
    d.set_open(true);
}

fn apply(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<SettingsDialog>();
    d.set_open(false);
    let mut config = app.core.config.borrow().clone();
    let current = config.settings.clone();
    let mut settings = current.clone();
    settings.minimize_to_tray = d.get_minimize_to_tray();
    settings.start_with_windows = d.get_autostart();
    settings.log_level = usize::try_from(d.get_level())
        .ok()
        .and_then(|i| LEVELS.get(i))
        .map_or(current.log_level, |(_, l)| *l);
    settings.theme = usize::try_from(d.get_theme())
        .ok()
        .and_then(|i| THEMES.get(i))
        .map_or(current.theme, |(_, t)| *t);

    let autostart_failed = (settings.start_with_windows != current.start_with_windows)
        .then(|| app.autostart.set(settings.start_with_windows).err())
        .flatten();
    config.settings = settings;
    if app.save(config)
        && let Some(e) = autostart_failed
    {
        message::inform(
            app,
            "Settings",
            &format!("Could not change \"{}\": {e}", app.autostart.label()),
        );
    }
}

/// Wires the sheet's callbacks. Called once.
pub(crate) fn connect(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<SettingsDialog>();
    let me = Rc::downgrade(app);
    d.on_ok(move || {
        if let Some(app) = me.upgrade() {
            apply(&app);
        }
    });
    let me = Rc::downgrade(app);
    d.on_cancel(move || {
        if let Some(ui) = me.upgrade().and_then(|app| app.ui()) {
            ui.global::<SettingsDialog>().set_open(false);
        }
    });
}
