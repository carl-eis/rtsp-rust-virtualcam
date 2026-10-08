//! A tiny logger for code running inside the Frame Server service.
//!
//! Lines go to `%ProgramData%\RtspCam\logs\vcam.log` (created with write access for LOCAL
//! SERVICE by `DllRegisterServer`) and to `OutputDebugString`, so DebugView shows them even
//! when the file can't be opened. Several processes (Frame Server, Frame Server Monitor, test
//! programs) append to the same file; each line names its process.
//!
//! Set `RTSPCAM_VCAM_LOG=stderr` to also print lines to stderr (tests, in-process tools).

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows_core::HSTRING;

/// Above this the file is renamed to `vcam.log.old` and a new one started.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

static FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// `%ProgramData%\RtspCam\logs`.
pub(crate) fn log_dir() -> Option<PathBuf> {
    let base = std::env::var_os("ProgramData")?;
    Some(
        PathBuf::from(base)
            .join(rtspcam_core::constants::APP_DIR_NAME)
            .join("logs"),
    )
}

fn open() -> Option<File> {
    let path = log_dir()?.join("vcam.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

pub(crate) fn write(args: fmt::Arguments<'_>) {
    // SAFETY: GetLocalTime and GetCurrentThreadId have no preconditions.
    let (t, tid) = unsafe { (GetLocalTime(), GetCurrentThreadId()) };
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{}:{}] {}\n",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        t.wMilliseconds,
        std::process::id(),
        tid,
        args
    );
    if std::env::var_os("RTSPCAM_VCAM_LOG").is_some_and(|v| v == "stderr") {
        eprint!("{line}");
    }
    // SAFETY: HSTRING is a valid null-terminated wide string for the call's duration.
    unsafe { OutputDebugStringW(&HSTRING::from(format!("rtspcam_vcam: {line}"))) };
    let file = FILE.get_or_init(|| Mutex::new(open()));
    if let Ok(mut guard) = file.lock()
        && let Some(f) = guard.as_mut()
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// `log!("...", args)`: formats and writes one line.
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(format_args!($($arg)*)) };
}
pub(crate) use log;
