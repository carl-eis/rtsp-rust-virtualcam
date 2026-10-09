//! Open and save dialogs for exported stream lists, through `rfd` (the system's own dialogs:
//! Windows' file dialog, macOS' panel, the XDG desktop portal on Linux).

use std::path::PathBuf;

use slint::ComponentHandle as _;

use super::generated::MainWindow;

fn dialog(ui: &MainWindow, title: &str) -> rfd::AsyncFileDialog {
    rfd::AsyncFileDialog::new()
        .set_title(title)
        .add_filter("Stream lists (JSON)", &["json"])
        .set_parent(&ui.window().window_handle())
}

/// Asks for a stream list to import.
pub(crate) async fn open_json(ui: &MainWindow) -> Option<PathBuf> {
    let file = dialog(ui, "Import streams").pick_file().await?;
    Some(file.path().to_owned())
}

/// Asks where to export the stream list.
pub(crate) async fn save_json(ui: &MainWindow, suggested: &str) -> Option<PathBuf> {
    let file = dialog(ui, "Export streams")
        .set_file_name(suggested)
        .save_file()
        .await?;
    Some(file.path().to_owned())
}
