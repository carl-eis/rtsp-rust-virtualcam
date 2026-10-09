//! Creating and removing the Windows virtual cameras.
//!
//! [`VcamBackend`] does it for real on a dedicated thread that owns Media Foundation and every
//! `IMFVirtualCamera` (COM objects stay on the thread that made them, and creating a camera
//! can take a second, so none of this may run on the UI thread). [`CameraBackend`] is the seam
//! that lets the manager's tests run without the DLL installed.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use rtspcam_core::constants::frame_pipe_name;
use rtspcam_vcam_mgr::{MfThread, VirtualCamera, list_devices};
use uuid::Uuid;

/// What the manager needs from Windows.
pub trait CameraBackend: Send + Sync + 'static {
    /// Creates and starts a camera for stream `id`. `preferred` is its (width, height, fps).
    /// Blocking. The error text is shown to the user.
    fn create(&self, name: &str, id: Uuid, preferred: (u32, u32, u32)) -> Result<(), String>;

    /// Removes one camera (no-op if it doesn't exist). Blocking.
    fn remove(&self, id: Uuid);

    /// Removes every camera this backend created, and releases whatever it holds.
    fn remove_all(&self);
}

enum Command {
    Create {
        name: String,
        id: Uuid,
        preferred: (u32, u32, u32),
        reply: Sender<Result<(), String>>,
    },
    Remove {
        id: Uuid,
        reply: Sender<()>,
    },
    RemoveAll {
        reply: Sender<()>,
    },
}

/// The real thing: `MFCreateVirtualCamera` through `rtspcam-vcam-mgr`.
#[derive(Debug)]
pub struct VcamBackend {
    tx: Mutex<Option<Sender<Command>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl VcamBackend {
    /// Starts the camera thread. Cameras left behind by a crashed run (Windows normally
    /// removes them with the process, but this is the safety net) are cleaned up first.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<Command>();
        let thread = thread::Builder::new()
            .name("virtual cameras".into())
            .spawn(move || {
                let mf = match MfThread::init() {
                    Ok(mf) => mf,
                    Err(e) => {
                        let msg = e.to_string();
                        tracing::error!(error = %msg, "Media Foundation did not start");
                        // Answer every request with the failure so nothing waits forever.
                        for cmd in rx {
                            match cmd {
                                Command::Create { reply, .. } => {
                                    let _ = reply.send(Err(msg.clone()));
                                }
                                Command::Remove { reply, .. } | Command::RemoveAll { reply } => {
                                    let _ = reply.send(());
                                }
                            }
                        }
                        return;
                    }
                };
                remove_stale();
                let mut cameras: HashMap<Uuid, VirtualCamera> = HashMap::new();
                for cmd in rx {
                    match cmd {
                        Command::Create {
                            name,
                            id,
                            preferred,
                            reply,
                        } => {
                            // Re-creating replaces the old one.
                            cameras.remove(&id);
                            let result = VirtualCamera::create(&name, id, Some(preferred))
                                .map(|c| {
                                    cameras.insert(id, c);
                                })
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        Command::Remove { id, reply } => {
                            cameras.remove(&id);
                            let _ = reply.send(());
                        }
                        Command::RemoveAll { reply } => {
                            cameras.clear();
                            let _ = reply.send(());
                            break;
                        }
                    }
                }
                // Anything still here (the sender was dropped) goes away with the thread.
                drop(cameras);
                drop(mf);
            })
            .expect("failed to spawn the virtual camera thread");
        Self {
            tx: Mutex::new(Some(tx)),
            thread: Mutex::new(Some(thread)),
        }
    }

    fn ask<T>(&self, make: impl FnOnce(Sender<T>) -> Command, gone: T) -> T {
        let (reply, answer) = mpsc::channel();
        let sent = self
            .tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|tx| tx.send(make(reply)).is_ok());
        if sent {
            answer.recv().unwrap_or(gone)
        } else {
            gone
        }
    }
}

impl CameraBackend for VcamBackend {
    fn create(&self, name: &str, id: Uuid, preferred: (u32, u32, u32)) -> Result<(), String> {
        self.ask(
            |reply| Command::Create {
                name: name.to_owned(),
                id,
                preferred,
                reply,
            },
            Err("the virtual camera thread has ended".to_owned()),
        )
    }

    fn remove(&self, id: Uuid) {
        self.ask(|reply| Command::Remove { id, reply }, ());
    }

    fn remove_all(&self) {
        self.ask(|reply| Command::RemoveAll { reply }, ());
        // Close the channel and wait for the thread, so Media Foundation is shut down.
        drop(
            self.tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        if let Some(t) = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = t.join();
        }
    }
}

impl Drop for VcamBackend {
    fn drop(&mut self) {
        self.remove_all();
    }
}

/// Removes RTSP Cam cameras that exist in Windows but that nothing serves.
///
/// A camera whose pipe doesn't exist belongs to no running process: a crash left it behind.
/// (`vcam add` from the CLI keeps its pipe, so its cameras are left alone.)
fn remove_stale() {
    let devices = match list_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "could not list cameras to look for stale ones");
            return;
        }
    };
    for device in devices {
        let Some(id) = device.rtspcam_id else {
            continue;
        };
        if pipe_exists(id) {
            continue;
        }
        tracing::warn!(%id, name = %device.name, "removing a stale virtual camera");
        // Creating a camera with the same name and id reopens it; removing that handle
        // removes it from the system.
        match VirtualCamera::create(&device.name, id, None).and_then(VirtualCamera::remove) {
            Ok(()) => {}
            Err(e) => tracing::warn!(%id, error = %e, "could not remove the stale camera"),
        }
    }
}

/// Whether anything serves the camera's pipe. Asks Windows without connecting: opening the
/// pipe (which `Path::exists` does) would take a server instance away from a real client.
pub fn pipe_exists(id: Uuid) -> bool {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, GetLastError};
    use windows::Win32::System::Pipes::WaitNamedPipeW;
    use windows_core::HSTRING;

    let name = HSTRING::from(frame_pipe_name(id));
    // SAFETY: a valid null-terminated pipe name; the call only queries.
    let found = unsafe { WaitNamedPipeW(&name, 1) }.as_bool();
    // SAFETY: reads the calling thread's last error.
    found || unsafe { GetLastError() } != ERROR_FILE_NOT_FOUND
}
