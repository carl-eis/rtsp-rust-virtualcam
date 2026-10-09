//! One copy per Windows session.
//!
//! The first copy holds the named mutex `Local\<name>.SingleInstance` and listens on the named
//! event `Local\<name>.Show`. A second copy finds the mutex taken, signals the event (so the
//! first brings its window forward) and exits. The app uses the name `RtspCam`.

use std::io;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows::core::HSTRING;

use crate::instance::{Instance, InstanceLock, SingleInstance};

/// The single-instance check for one name.
#[derive(Debug, Clone)]
pub(crate) struct NamedMutex {
    mutex: HSTRING,
    show: HSTRING,
}

impl NamedMutex {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            mutex: HSTRING::from(format!(r"Local\{name}.SingleInstance")),
            show: HSTRING::from(format!(r"Local\{name}.Show")),
        }
    }
}

impl SingleInstance for NamedMutex {
    fn acquire(&self) -> io::Result<Instance> {
        acquire(&self.mutex, &self.show).map_err(io::Error::other)
    }
}

/// Held for the life of the process by the first instance.
#[derive(Debug)]
struct InstanceGuard {
    mutex: usize,
    show: usize,
    stop: usize,
    listener: Option<JoinHandle<()>>,
}

/// Claims the single-instance mutex, or tells the running copy to show itself.
fn acquire(mutex_name: &HSTRING, show_name: &HSTRING) -> windows::core::Result<Instance> {
    // SAFETY: plain kernel object creation with valid names; handles are closed in Drop
    // (or right here when another instance exists).
    unsafe {
        let mutex = CreateMutexW(None, true, mutex_name)?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, show_name) {
                let _ = SetEvent(event);
                let _ = CloseHandle(event);
            }
            return Ok(Instance::AlreadyRunning);
        }
        let show = CreateEventW(None, false, false, show_name)?;
        let stop = CreateEventW(None, true, false, None)?;
        Ok(Instance::First(Box::new(InstanceGuard {
            mutex: mutex.0 as usize,
            show: show.0 as usize,
            stop: stop.0 as usize,
            listener: None,
        })))
    }
}

impl InstanceLock for InstanceGuard {
    fn on_show(&mut self, on_show: Box<dyn Fn() + Send>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_name_gives_the_same_kernel_object_names_as_before() {
        let names = NamedMutex::new("RtspCam");
        assert_eq!(names.mutex, r"Local\RtspCam.SingleInstance");
        assert_eq!(names.show, r"Local\RtspCam.Show");
    }
}
