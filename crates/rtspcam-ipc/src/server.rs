//! Async pipe server, used by the app (and the CLI's test pattern camera). One server per
//! camera; each connected client gets frames in the format it asked for, paced at its fps.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::watch;
use uuid::Uuid;

use crate::frame_pipe_name;
use crate::protocol::{
    FrameHeader, HEADER_LEN, Message, ProtocolError, StreamStatus, VideoFormat, encode,
    frame_prefix, parse_body, parse_header,
};

/// Where a camera's frames come from. Implemented by the app's camera manager.
pub trait FrameSource: Send + Sync + 'static {
    /// A consumer started using the camera (called once per connection).
    fn client_connected(&self, _format: VideoFormat) {}

    /// That consumer went away.
    fn client_disconnected(&self) {}

    /// Writes the newest picture, converted to `format`, into `out` if it is newer than
    /// `after`, and returns its sequence number. `None` means nothing new.
    fn next_frame(&self, format: VideoFormat, after: Option<u64>, out: &mut Vec<u8>)
    -> Option<u64>;

    /// Current state, shown by the camera when there are no frames.
    fn status(&self) -> (StreamStatus, String);
}

/// How long a client gets to send its `Hello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Status is re-sent this often even if unchanged, as a heartbeat.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);

/// Accepts camera clients on `\\.\pipe\rtspcam\<camera_id>` until the task is dropped.
pub async fn serve(camera_id: Uuid, source: Arc<dyn FrameSource>) -> io::Result<()> {
    let name = frame_pipe_name(camera_id);
    let mut server = create_instance(&name, true)?;
    loop {
        server.connect().await?;
        let connected = server;
        // Create the next instance before serving, so new clients never see "no pipe".
        server = create_instance(&name, false)?;
        let source = source.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_client(connected, source).await {
                tracing::debug!(%camera_id, error = %e, "camera client ended");
            }
        });
    }
}

fn create_instance(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first)
        .reject_remote_clients(true)
        .out_buffer_size(1 << 20);
    let security = security::PipeSecurity::new()?;
    // SAFETY: `security` holds a valid SECURITY_ATTRIBUTES (and its descriptor) for the
    // duration of this call; the pipe copies what it needs.
    unsafe { opts.create_with_security_attributes_raw(name, security.as_ptr()) }
}

async fn serve_client(
    mut pipe: NamedPipeServer,
    source: Arc<dyn FrameSource>,
) -> Result<(), ProtocolError> {
    let mut buf = Vec::new();
    let (pid, format) = match tokio::time::timeout(HELLO_TIMEOUT, read(&mut pipe, &mut buf)).await {
        Ok(Ok(Owned::Hello { pid, format })) => (pid, format),
        Ok(Ok(_)) => return Err(ProtocolError::Invalid("expected Hello".into())),
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(ProtocolError::Invalid("no Hello in time".into())),
    };
    tracing::info!(pid, %format, "camera client connected");
    source.client_connected(format);
    let result = stream_to(pipe, format, &*source).await;
    source.client_disconnected();
    tracing::info!(pid, "camera client disconnected");
    result
}

async fn stream_to(
    pipe: NamedPipeServer,
    format: VideoFormat,
    source: &dyn FrameSource,
) -> Result<(), ProtocolError> {
    let (mut rd, wr) = tokio::io::split(pipe);
    let (format_tx, format_rx) = watch::channel(format);
    // Control messages from the client arrive independently of frames going out.
    let reader = async move {
        let mut buf = Vec::new();
        loop {
            match read(&mut rd, &mut buf).await? {
                Owned::SetFormat(f) => {
                    tracing::info!(format = %f, "camera client changed format");
                    format_tx.send_replace(f);
                }
                Owned::Goodbye => return Ok::<(), ProtocolError>(()),
                _ => {}
            }
        }
    };
    tokio::select! {
        r = reader => r,
        r = write_frames(wr, format_rx, source) => r,
    }
}

async fn write_frames(
    mut wr: tokio::io::WriteHalf<NamedPipeServer>,
    mut format_rx: watch::Receiver<VideoFormat>,
    source: &dyn FrameSource,
) -> Result<(), ProtocolError> {
    let mut format = *format_rx.borrow_and_update();
    let mut tick = tokio::time::interval(frame_interval(format));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_seq = None;
    let mut last_status = None;
    let mut status_due = tokio::time::Instant::now();
    let mut data = Vec::new();
    loop {
        tick.tick().await;
        if format_rx.has_changed().unwrap_or(false) {
            format = *format_rx.borrow_and_update();
            tick = tokio::time::interval(frame_interval(format));
            last_seq = None;
        }
        let now = tokio::time::Instant::now();
        let status = source.status();
        if last_status.as_ref() != Some(&status) || now >= status_due {
            let msg = Message::Status {
                status: status.0,
                message: status.1.clone(),
            };
            wr.write_all(&encode(&msg)).await?;
            last_status = Some(status);
            status_due = now + STATUS_INTERVAL;
        }
        if let Some(seq) = source.next_frame(format, last_seq, &mut data) {
            if data.len() != format.frame_len() {
                tracing::error!(len = data.len(), %format, "frame source returned a wrong-sized frame");
                continue;
            }
            let header = FrameHeader {
                seq,
                timestamp: 0,
                format,
            };
            wr.write_all(&frame_prefix(&header, data.len())).await?;
            wr.write_all(&data).await?;
            last_seq = Some(seq);
        }
    }
}

fn frame_interval(format: VideoFormat) -> Duration {
    Duration::from_secs(1) / format.fps.max(1)
}

/// Control messages, owned (frames never travel client → server).
enum Owned {
    Hello { pid: u32, format: VideoFormat },
    SetFormat(VideoFormat),
    Goodbye,
    Other,
}

async fn read<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Result<Owned, ProtocolError> {
    let mut h = [0u8; HEADER_LEN];
    r.read_exact(&mut h).await?;
    let header = parse_header(&h)?;
    // Clients only send small control messages.
    if header.body_len > 4096 {
        return Err(ProtocolError::TooLarge(header.body_len));
    }
    buf.resize(header.body_len, 0);
    r.read_exact(buf).await?;
    Ok(match parse_body(header.kind, buf)? {
        Message::Hello { pid, format } => Owned::Hello { pid, format },
        Message::SetFormat(f) => Owned::SetFormat(f),
        Message::Goodbye => Owned::Goodbye,
        _ => Owned::Other,
    })
}

mod security {
    //! The pipe's DACL: the Frame Server runs as LOCAL SERVICE and Frame Server Monitor as
    //! SYSTEM, while the app runs as the signed-in user, so all three need access.

    use std::ffi::c_void;
    use std::io;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::core::w;

    /// SYSTEM, LOCAL SERVICE and the pipe's owner (the user running the app): full access.
    /// Protected, so nothing is inherited.
    const SDDL: windows::core::PCWSTR = w!("D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;OW)");

    pub(super) struct PipeSecurity {
        descriptor: PSECURITY_DESCRIPTOR,
        attributes: SECURITY_ATTRIBUTES,
    }

    impl PipeSecurity {
        pub(super) fn new() -> io::Result<Self> {
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            // SAFETY: valid SDDL string; on success the descriptor is LocalAlloc'ed and freed
            // in Drop.
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    SDDL,
                    SDDL_REVISION_1,
                    &mut descriptor,
                    None,
                )
            }
            .map_err(io::Error::other)?;
            Ok(Self {
                descriptor,
                attributes: SECURITY_ATTRIBUTES {
                    nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: descriptor.0,
                    bInheritHandle: false.into(),
                },
            })
        }

        pub(super) fn as_ptr(&self) -> *mut c_void {
            (&raw const self.attributes).cast_mut().cast()
        }
    }

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe {
                LocalFree(Some(HLOCAL(self.descriptor.0)));
            }
        }
    }
}
