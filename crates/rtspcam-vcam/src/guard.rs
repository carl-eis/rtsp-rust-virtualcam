//! Panic containment and module lifetime accounting.
//!
//! A panic unwinding out of a COM method would abort the Frame Server process, taking every
//! camera on the system down with it. Every COM entry point runs its body through [`guard`],
//! which turns a panic into `E_UNEXPECTED` and logs it.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::E_UNEXPECTED;

use crate::log::log;

/// Runs `f`, converting a panic into `E_UNEXPECTED`.
pub(crate) fn guard<T>(
    what: &str,
    f: impl FnOnce() -> windows_core::Result<T>,
) -> windows_core::Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_owned());
            log!("PANIC in {what}: {msg}");
            Err(E_UNEXPECTED.into())
        }
    }
}

/// COM objects, server locks and helper threads that keep the DLL from being unloaded.
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Whether nothing needs the DLL any more (`DllCanUnloadNow`).
pub(crate) fn can_unload() -> bool {
    LIVE.load(Ordering::SeqCst) == 0
}

/// Number of live objects, locks and threads (for tests).
pub fn live_count() -> usize {
    LIVE.load(Ordering::SeqCst)
}

/// Holds the DLL loaded while it exists. Embedded in every COM object and helper thread.
#[derive(Debug)]
pub(crate) struct ModuleRef(());

impl ModuleRef {
    pub(crate) fn new() -> Self {
        LIVE.fetch_add(1, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for ModuleRef {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `IClassFactory::LockServer`.
pub(crate) fn lock_server(lock: bool) {
    if lock {
        LIVE.fetch_add(1, Ordering::SeqCst);
    } else {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Wrapper to move free-threaded Media Foundation objects (event queues, samples) to a
/// helper thread. windows-rs marks interfaces `!Send`, but these objects are documented as
/// thread-safe.
pub(crate) struct Agile<T>(pub(crate) T);

// SAFETY: only used for Media Foundation objects that are free-threaded (event queues,
// attributes, media types).
unsafe impl<T> Send for Agile<T> {}
// SAFETY: as above.
unsafe impl<T> Sync for Agile<T> {}
