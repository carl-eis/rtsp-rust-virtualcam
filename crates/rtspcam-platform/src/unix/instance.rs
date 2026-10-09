//! One copy per user on Linux and macOS.
//!
//! The first copy holds an exclusive lock on `<name>.lock` in the runtime folder (the OS
//! releases it when the process ends, however it ends) and listens on the Unix socket
//! `<name>.sock`. A second copy fails to take the lock, connects to the socket (which asks the
//! first copy to show its window) and exits.

use std::fs::{self, File, TryLockError};
use std::io::{self, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use super::{check_socket_path, private_dir};
use crate::instance::{Instance, InstanceLock, SingleInstance};

/// The single-instance check for one name.
#[derive(Debug, Clone)]
pub(crate) struct LockFile {
    lock: PathBuf,
    socket: PathBuf,
}

impl LockFile {
    pub(crate) fn new(name: &str) -> Self {
        let dir = super::runtime_dir();
        Self {
            lock: dir.join(format!("{name}.lock")),
            socket: dir.join(format!("{name}.sock")),
        }
    }
}

impl SingleInstance for LockFile {
    fn acquire(&self) -> io::Result<Instance> {
        if let Some(dir) = self.lock.parent() {
            private_dir(dir)?;
        }
        check_socket_path(&self.socket)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock)?;
        match file.try_lock() {
            Ok(()) => {
                // We hold the lock, so any socket file left over is from a copy that died.
                let _ = fs::remove_file(&self.socket);
                let listener = UnixListener::bind(&self.socket)?;
                Ok(Instance::First(Box::new(Guard {
                    _file: file,
                    socket: self.socket.clone(),
                    listener: Some(listener),
                    stop: Arc::default(),
                    thread: None,
                })))
            }
            Err(TryLockError::WouldBlock) => {
                // Ask the running copy to show itself; if it doesn't answer it is busy
                // starting or quitting, and there is nothing more to do.
                if let Ok(mut stream) = UnixStream::connect(&self.socket) {
                    let _ = stream.write_all(b"show\n");
                }
                Ok(Instance::AlreadyRunning)
            }
            Err(TryLockError::Error(e)) => Err(e),
        }
    }
}

/// Held for the life of the process by the first copy.
#[derive(Debug)]
struct Guard {
    /// Holds the lock; closing the file releases it.
    _file: File,
    socket: PathBuf,
    listener: Option<UnixListener>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl InstanceLock for Guard {
    fn on_show(&mut self, on_show: Box<dyn Fn() + Send>) {
        let Some(listener) = self.listener.take() else {
            return;
        };
        let stop = self.stop.clone();
        self.thread = Some(
            thread::Builder::new()
                .name("single instance".into())
                .spawn(move || {
                    for connection in listener.incoming() {
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        if connection.is_ok() {
                            on_show();
                        }
                    }
                })
                .expect("failed to spawn the single-instance thread"),
        );
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            // Wake the listener so it sees the stop flag.
            let _ = UnixStream::connect(&self.socket);
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.socket);
    }
}
