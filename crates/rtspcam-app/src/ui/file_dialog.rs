//! The standard Open and Save dialogs, for importing and exporting streams.

use std::path::PathBuf;

use winsafe::prelude::shell_IFileDialog;
use winsafe::{self as w, co, prelude::*};

const FILTER: [(&str, &str); 2] = [("JSON files", "*.json"), ("All files", "*.*")];

/// Asks for an existing file to import. `None` if cancelled (or the dialog could not open).
pub(crate) fn open_json(parent: &w::HWND) -> Option<PathBuf> {
    let dialog = w::CoCreateInstance::<w::IFileOpenDialog>(
        &co::CLSID::FileOpenDialog,
        None::<&w::IUnknown>,
        co::CLSCTX::INPROC_SERVER,
    )
    .map_err(|e| tracing::warn!(error = %e, "could not create the Open dialog"))
    .ok()?;
    dialog
        .SetOptions(dialog.GetOptions().ok()? | co::FOS::FORCEFILESYSTEM | co::FOS::FILEMUSTEXIST)
        .ok()?;
    dialog.SetTitle("Import streams").ok()?;
    dialog.SetFileTypes(&FILTER).ok()?;
    dialog.SetFileTypeIndex(1).ok()?;
    chosen(&dialog, parent)
}

/// Asks where to save an export. `None` if cancelled.
pub(crate) fn save_json(parent: &w::HWND, suggested: &str) -> Option<PathBuf> {
    let dialog = w::CoCreateInstance::<w::IFileSaveDialog>(
        &co::CLSID::FileSaveDialog,
        None::<&w::IUnknown>,
        co::CLSCTX::INPROC_SERVER,
    )
    .map_err(|e| tracing::warn!(error = %e, "could not create the Save dialog"))
    .ok()?;
    dialog
        .SetOptions(dialog.GetOptions().ok()? | co::FOS::FORCEFILESYSTEM | co::FOS::OVERWRITEPROMPT)
        .ok()?;
    dialog.SetTitle("Export streams").ok()?;
    dialog.SetFileTypes(&FILTER).ok()?;
    dialog.SetFileTypeIndex(1).ok()?;
    dialog.SetDefaultExtension("json").ok()?;
    dialog.SetFileName(suggested).ok()?;
    chosen(&dialog, parent)
}

/// Shows the dialog and returns the path the user chose.
fn chosen(dialog: &impl shell_IFileDialog, parent: &w::HWND) -> Option<PathBuf> {
    if !dialog.Show(parent).ok()? {
        return None;
    }
    let path = dialog
        .GetResult()
        .ok()?
        .GetDisplayName(co::SIGDN::FILESYSPATH)
        .ok()?;
    Some(PathBuf::from(path))
}
