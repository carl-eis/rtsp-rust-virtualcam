//! Replacing `config.json` with `ReplaceFileW`, which keeps the file's ACL and attributes and
//! makes the backup in the same step.

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;

use rtspcam_core::config::{CopyThenRename, FileReplace};
use windows::Win32::Storage::FileSystem::{REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW};
use windows::core::PCWSTR;

/// `ReplaceFileW`, falling back to [`CopyThenRename`] if it fails.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ReplaceFile;

impl FileReplace for ReplaceFile {
    fn replace(&self, tmp: &Path, target: &Path, backup: &Path) -> io::Result<()> {
        match replace_file(tmp, target, backup) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::debug!(error = %e, "ReplaceFileW failed, falling back to copy + rename");
                CopyThenRename.replace(tmp, target, backup)
            }
        }
    }
}

fn replace_file(tmp: &Path, target: &Path, backup: &Path) -> io::Result<()> {
    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    let (target, tmp, backup) = (wide(target), wide(tmp), wide(backup));
    // SAFETY: all three are NUL-terminated UTF-16 strings that outlive the call.
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(tmp.as_ptr()),
            PCWSTR(backup.as_ptr()),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            None,
            None,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn replaces_and_keeps_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (tmp, target, backup) = (
            dir.path().join("c.json.tmp"),
            dir.path().join("c.json"),
            dir.path().join("c.json.bak"),
        );
        fs::write(&target, "old").unwrap();
        fs::write(&tmp, "new").unwrap();
        ReplaceFile.replace(&tmp, &target, &backup).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "old");
        assert!(!tmp.exists());
    }
}
