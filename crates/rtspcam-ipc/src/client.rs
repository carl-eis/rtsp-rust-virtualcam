//! Blocking pipe client, used by the virtual camera DLL inside the Frame Server service.
//! No tokio, no background threads of its own: the caller owns the reading thread.

use std::fs::{File, OpenOptions};
use std::io;

use uuid::Uuid;

use crate::frame_pipe_name;
use crate::protocol::{Message, ProtocolError, VideoFormat, read_message, write_message};

/// `ERROR_PIPE_BUSY`: every pipe instance is in use; try again shortly.
pub const ERROR_PIPE_BUSY: i32 = 231;
/// `ERROR_FILE_NOT_FOUND`: no such pipe, i.e. the app isn't running (or has no such camera).
pub const ERROR_FILE_NOT_FOUND: i32 = 2;

/// A connection to the app's pipe for one camera.
#[derive(Debug)]
pub struct FrameClient {
    pipe: File,
    buf: Vec<u8>,
}

impl FrameClient {
    /// Opens the camera's pipe and asks for frames in `format`.
    pub fn connect(camera_id: Uuid, format: VideoFormat) -> io::Result<Self> {
        let pipe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(frame_pipe_name(camera_id))?;
        let mut client = Self {
            pipe,
            buf: Vec::new(),
        };
        write_message(
            &mut client.pipe,
            &Message::Hello {
                pid: std::process::id(),
                format,
            },
        )?;
        Ok(client)
    }

    /// Tells the app the consumer switched formats.
    pub fn set_format(&mut self, format: VideoFormat) -> io::Result<()> {
        write_message(&mut self.pipe, &Message::SetFormat(format))
    }

    /// Blocks until the next message. Frame data borrows an internal buffer.
    pub fn read(&mut self) -> Result<Message<'_>, ProtocolError> {
        read_message(&mut self.pipe, &mut self.buf)
    }

    /// Says goodbye; errors are ignored since the connection is ending anyway.
    pub fn close(mut self) {
        let _ = write_message(&mut self.pipe, &Message::Goodbye);
    }
}
