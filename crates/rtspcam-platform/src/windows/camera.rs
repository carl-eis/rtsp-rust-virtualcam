//! Creating and removing the Windows virtual cameras, and serving their frames.
//!
//! [`VcamBackend`] keeps every `IMFVirtualCamera` on a dedicated thread that owns Media
//! Foundation (COM objects stay on the thread that made them, and creating a camera can take a
//! second, so none of this may run on the UI thread). Each camera's frames are served on its
//! named pipe, which `rtspcam_vcam.dll` inside Frame Server connects to. The pipe is served
//! before the camera is created, so the DLL finds it as soon as an app opens the camera.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use rtspcam_ipc::FrameSource;
use rtspcam_ipc::server::serve;
use rtspcam_vcam_mgr::{MfThread, VirtualCamera};
use tokio::runtime::Handle;
use uuid::Uuid;

use super::pipe::NamedPipes;
use crate::camera::{CameraError, CameraSpec, VirtualCameraBackend};
use crate::transport::FrameTransport;

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

/// The real thing: `MFCreateVirtualCamera` through `rtspcam-vcam-mgr`, fed over named pipes.
#[derive(Debug)]
pub(crate) struct VcamBackend {
    tx: Mutex<Option<Sender<Command>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// The task serving each camera's pipe.
    servers: Mutex<HashMap<Uuid, tokio::task::JoinHandle<()>>>,
}

impl VcamBackend {
    /// Starts the camera thread. (Cameras have session lifetime: Windows removes them when the
    /// process ends, including after a crash or a hard kill.)
    pub(crate) fn start() -> Self {
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
                                .map_err(|e| explain(&e.to_string()));
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
            servers: Mutex::default(),
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

    /// Starts serving camera `id`'s pipe; replaces (and stops) an older server for it.
    fn serve(
        &self,
        id: Uuid,
        frames: Arc<dyn FrameSource>,
        runtime: &Handle,
    ) -> Result<(), CameraError> {
        self.stop_serving(id);
        let listener = {
            // Creating a pipe registers it with the runtime's reactor.
            let _enter = runtime.enter();
            NamedPipes.listen(id)
        }
        .map_err(|e| {
            tracing::error!(%id, error = %e, "the camera's pipe could not be served");
            CameraError::Failed(format!(
                "could not serve the camera's pipe ({e}); is another copy of RTSP Cam running?"
            ))
        })?;
        let task = runtime.spawn(async move {
            if let Err(e) = serve(listener, frames).await {
                tracing::error!(%id, error = %e, "the camera's pipe stopped");
            }
        });
        self.servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, task);
        Ok(())
    }

    fn stop_serving(&self, id: Uuid) {
        if let Some(task) = self
            .servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id)
        {
            // Dropping the server task also ends every connected client.
            task.abort();
        }
    }
}

impl VirtualCameraBackend for VcamBackend {
    fn check(&self) -> Result<(), CameraError> {
        if rtspcam_vcam_mgr::is_supported() {
            Ok(())
        } else {
            Err(CameraError::Failed(
                "RTSP Cam requires Windows 11 (virtual cameras are not available).".to_owned(),
            ))
        }
    }

    fn create(
        &self,
        spec: &CameraSpec,
        frames: Arc<dyn FrameSource>,
        runtime: &Handle,
    ) -> Result<(), CameraError> {
        // The pipe first: creating the camera is the slow part.
        self.serve(spec.id, frames, runtime)?;
        self.ask(
            |reply| Command::Create {
                name: spec.name.clone(),
                id: spec.id,
                preferred: spec.preferred,
                reply,
            },
            Err("the virtual camera thread has ended".to_owned()),
        )
        .map_err(CameraError::Failed)
    }

    fn remove(&self, id: Uuid) {
        self.stop_serving(id);
        self.ask(|reply| Command::Remove { id, reply }, ());
    }

    fn remove_all(&self) {
        // Stop the pipes first, so no new consumer arrives while cameras go away.
        for (_, task) in self
            .servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
        {
            task.abort();
        }
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

/// Adds what to do about "Access is denied", which Windows reports both when the media source
/// is not registered and when Frame Server cannot read its folder.
fn explain(error: &str) -> String {
    if error.contains("0x80070005") {
        format!(
            "{error}. The media source is not installed or Frame Server cannot read it \
             (see tools/vcam/install-dev.ps1)"
        )
    } else {
        error.to_owned()
    }
}
