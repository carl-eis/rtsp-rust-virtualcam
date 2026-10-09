//! Unix sockets `<runtime folder>/<camera id>.sock`, in a folder only the user can open.

use std::fs;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use rtspcam_ipc::server::{Accept, BoxedConnection, FrameListener};
use tokio::net::UnixListener;
use uuid::Uuid;

use super::{check_socket_path, private_dir};
use crate::transport::{FrameTransport, ReadWrite};

/// The Linux and macOS [`FrameTransport`].
#[derive(Debug, Clone)]
pub(crate) struct UnixSockets {
    dir: PathBuf,
}

impl UnixSockets {
    pub(crate) fn new() -> Self {
        Self {
            dir: super::runtime_dir().join("cam"),
        }
    }

    fn path(&self, id: Uuid) -> PathBuf {
        // The short form keeps the path under the OS's limit.
        self.dir.join(format!("{}.sock", id.simple()))
    }
}

impl FrameTransport for UnixSockets {
    fn listen(&self, id: Uuid) -> io::Result<Box<dyn FrameListener>> {
        private_dir(&self.dir)?;
        let path = self.path(id);
        check_socket_path(&path)?;
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another program already serves this camera",
                ));
            }
            // Left over from a copy that ended without cleaning up.
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        Ok(Box::new(SocketListener { listener, path }))
    }

    fn connect(&self, id: Uuid) -> io::Result<Box<dyn ReadWrite>> {
        Ok(Box::new(UnixStream::connect(self.path(id))?))
    }

    /// The socket file exists exactly while a listener is alive (it removes the file when
    /// dropped), so no connection is needed to tell.
    fn is_served(&self, id: Uuid) -> bool {
        self.path(id).exists()
    }
}

struct SocketListener {
    listener: UnixListener,
    path: PathBuf,
}

impl FrameListener for SocketListener {
    fn accept(&mut self) -> Accept<'_> {
        Box::pin(async move {
            let (stream, _) = self.listener.accept().await?;
            Ok(Box::new(stream) as BoxedConnection)
        })
    }
}

impl Drop for SocketListener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
