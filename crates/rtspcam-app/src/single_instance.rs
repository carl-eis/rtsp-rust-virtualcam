//! One `rtspcam.exe` per Windows session.
//!
//! The first copy holds a named mutex and listens on a named event. A second copy finds the
//! mutex taken, signals the event (so the first brings its window forward) and exits.

use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows::core::w;

/// Held for the life of the process by the first instance.
#[derive(Debug)]
pub struct InstanceGuard {
    mutex: usize,
    show: usize,
    stop: usize,
    listener: Option<JoinHandle<()>>,
}

/// The outcome of [`acquire`].
#[derive(Debug)]
pub enum Instance {
    /// This is the only copy.
    First(InstanceGuard),
    /// Another copy runs; it has been asked to show its window.
    AlreadyRunning,
}

/// Claims the single-instance mutex, or tells the running copy to show itself.
pub fn acquire() -> windows::core::Result<Instance> {
    // SAFETY: plain kernel object creation with static names; handles are closed in Drop
    // (or right here when another instance exists).
    unsafe {
        let mutex = CreateMutexW(None, true, w!(r"Local\RtspCam.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, w!(r"Local\RtspCam.Show")) {
                let _ = SetEvent(event);
                let _ = CloseHandle(event);
            }
            return Ok(Instance::AlreadyRunning);
        }
        let show = CreateEventW(None, false, false, w!(r"Local\RtspCam.Show"))?;
        let stop = CreateEventW(None, true, false, None)?;
        Ok(Instance::First(InstanceGuard {
            mutex: mutex.0 as usize,
            show: show.0 as usize,
            stop: stop.0 as usize,
            listener: None,
        }))
    }
}

impl InstanceGuard {
    /// Calls `on_show` (on a background thread) each time a second copy is started.
    pub fn on_show(&mut self, on_show: impl Fn() + Send + 'static) {
        let (show, stop) = (self.show, self.stop);
        self.listener = Some(
            thread::Builder::new()
                .name("single instance".into())
                .spawn(move || {
                    let handles = [HANDLE(show as *mut _), HANDLE(stop as *mut _)];
                    loop {
                        // SAFETY: both handles stay open until this thread is joined in Drop.
                        let r = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                        if r == WAIT_OBJECT_0 {
                            on_show();
                        } else {
                            break;
                        }
                    }
                })
                .expect("failed to spawn the single-instance thread"),
        );
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: the handles were created in `acquire` and are closed exactly once, after
        // the listener (the only other user) has stopped.
        unsafe {
            let _ = SetEvent(HANDLE(self.stop as *mut _));
            if let Some(t) = self.listener.take() {
                let _ = t.join();
            }
            for h in [self.show, self.stop, self.mutex] {
                let _ = CloseHandle(HANDLE(h as *mut _));
            }
        }
    }
}
