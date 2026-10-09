//! Blocking client, used by the camera side (on Windows the virtual camera DLL inside the
//! Frame Server service). No tokio, no background threads of its own: the caller owns the
//! reading thread.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use crate::protocol::{Message, ProtocolError, VideoFormat, read_message, write_message};

/// A connection to the app for one camera, over any byte stream.
#[derive(Debug)]
pub struct FrameClient<S = File> {
    stream: S,
    buf: Vec<u8>,
}

impl FrameClient<File> {
    /// Opens an endpoint that is reached through the file system, such as a Windows named
    /// pipe (`\.\pipe\rtspcam\<id>`, see [`frame_pipe_name`](crate::frame_pipe_name)), and asks
    /// for frames in `format`.
    pub fn open(path: impl AsRef<Path>, format: VideoFormat) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Self::new(file, format)
    }
}

impl<S: Read + Write> FrameClient<S> {
    /// Starts a session on a connected `stream`: asks for frames in `format`.
    pub fn new(stream: S, format: VideoFormat) -> io::Result<Self> {
        let mut client = Self {
            stream,
            buf: Vec::new(),
        };
        write_message(
            &mut client.stream,
            &Message::Hello {
                pid: std::process::id(),
                format,
            },
        )?;
        Ok(client)
    }

    /// Tells the app the consumer switched formats.
    pub fn set_format(&mut self, format: VideoFormat) -> io::Result<()> {
        write_message(&mut self.stream, &Message::SetFormat(format))
    }

    /// Blocks until the next message. Frame data borrows an internal buffer.
    pub fn read(&mut self) -> Result<Message<'_>, ProtocolError> {
        read_message(&mut self.stream, &mut self.buf)
    }

    /// Says goodbye; errors are ignored since the connection is ending anyway.
    pub fn close(mut self) {
        let _ = write_message(&mut self.stream, &Message::Goodbye);
    }
}
