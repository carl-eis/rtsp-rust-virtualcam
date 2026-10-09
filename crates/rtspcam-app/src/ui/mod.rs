//! The interface, built with Slint: the main window, its dialogs (drawn as sheets inside it)
//! and the tray icon. Layout is in `ui/*.slint`; this module fills in the properties and
//! handles the callbacks. The models behind the dialogs (validation, connection tests, ONVIF
//! helpers) are in `rtspcam-engine`.
//!
//! Everything here runs on the UI thread. Work that takes time (connection tests, network
//! scans, preview pictures) runs elsewhere and comes back with `slint::invoke_from_event_loop`.

mod app;
mod core;
mod discover_dialog;
mod files;
mod message;
mod picture_dialog;
mod preview;
mod settings_dialog;
mod stream_dialog;

/// The Rust API generated from `ui/app.slint`.
#[allow(
    unreachable_pub,
    missing_debug_implementations,
    clippy::all,
    clippy::todo,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::undocumented_unsafe_blocks
)]
mod generated {
    slint::include_modules!();
}

pub(crate) use app::{RunOptions, run};

/// Tells the user why the app can't start, in a message box (the release build has no
/// console). Used before the window exists.
pub(crate) fn fatal_error(message: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(rtspcam_core::constants::APP_DISPLAY_NAME)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}
