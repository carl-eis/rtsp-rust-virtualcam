//! Per-camera endpoints between the app and a camera process that pulls frames from it.

use std::io::{self, Read, Write};

use rtspcam_ipc::server::FrameListener;
use uuid::Uuid;

/// A connected byte stream, as the camera side sees it.
pub trait ReadWrite: Read + Write + Send {}

impl<T: Read + Write + Send> ReadWrite for T {}

/// Where camera `id`'s frames are served.
///
/// - **Windows**: the named pipe `\\.\pipe\rtspcam\<id>`, with a DACL that lets the Frame
///   Server services (LOCAL SERVICE, SYSTEM) and the user in.
/// - **Linux and macOS**: the Unix socket `<runtime folder>/rtspcam/<id>.sock` in a folder only
///   the user can open. Used by tests today, and by a future macOS camera extension.
///
/// Serve a listener with [`rtspcam_ipc::server::serve`].
pub trait FrameTransport: Send + Sync {
    /// Starts accepting clients for camera `id`. Fails if something already serves it. Must be
    /// called inside a tokio runtime.
    fn listen(&self, id: Uuid) -> io::Result<Box<dyn FrameListener>>;

    /// Connects to camera `id` as a client (tests and tools; the Windows DLL opens the pipe
    /// itself).
    fn connect(&self, id: Uuid) -> io::Result<Box<dyn ReadWrite>>;

    /// Whether something serves camera `id`, checked without taking a connection away from a
    /// real client.
    fn is_served(&self, id: Uuid) -> bool;
}
