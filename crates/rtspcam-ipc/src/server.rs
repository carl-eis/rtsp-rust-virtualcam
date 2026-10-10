//! Async server, used by the app (and the CLI's test pattern camera). One server per camera;
//! each connected client gets frames in the format it asked for, paced at its fps.
//!
//! The server doesn't know how clients reach it: it accepts them from a [`FrameListener`]
//! (`rtspcam-platform` has one for Windows named pipes and one for Unix sockets).

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use rtspcam_core::profile::{self, Stage};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::protocol::{
    FrameHeader, HEADER_LEN, Message, ProtocolError, StreamStatus, VideoFormat, encode,
    frame_prefix, parse_body, parse_header,
};
pub use crate::source::FrameSource;

/// A connected client: any async byte stream.
pub trait Connection: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Connection for T {}

/// A boxed [`Connection`].
pub type BoxedConnection = Box<dyn Connection>;

/// What [`FrameListener::accept`] returns.
pub type Accept<'a> = Pin<Box<dyn Future<Output = io::Result<BoxedConnection>> + Send + 'a>>;

/// Hands out the clients of one camera's endpoint.
pub trait FrameListener: Send + 'static {
    /// Waits for the next client. An error ends [`serve`].
    fn accept(&mut self) -> Accept<'_>;
}

/// How long a client gets to send its `Hello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Status is re-sent this often even if unchanged, as a heartbeat.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
/// How often a frame slot checks again for a picture that is late, when the source can't
/// signal one.
const LATE_POLL: Duration = Duration::from_millis(3);

/// Serves `source` to every client `listener` accepts, until the task is dropped (which also
/// ends every connected client) or accepting fails.
pub async fn serve(
    listener: Box<dyn FrameListener>,
    source: Arc<dyn FrameSource>,
) -> io::Result<()> {
    serve_until(listener, source, std::future::pending()).await
}

/// [`serve`], until `stop` finishes too. Then it ends every connected client, waits for their
/// tasks to finish and drops the listener before returning, so when it returns nothing holds
/// the endpoint any more and it can be served again at once. (Dropping a [`serve`] task, or
/// aborting it, closes them a little later on another thread instead.)
pub async fn serve_until(
    mut listener: Box<dyn FrameListener>,
    source: Arc<dyn FrameSource>,
    stop: impl Future<Output = ()> + Send,
) -> io::Result<()> {
    let mut clients = JoinSet::new();
    let mut stop = std::pin::pin!(stop);
    let result = loop {
        let connection = tokio::select! {
            () = &mut stop => break Ok(()),
            c = listener.accept() => match c {
                Ok(c) => c,
                Err(e) => break Err(e),
            },
        };
        let source = source.clone();
        while clients.try_join_next().is_some() {}
        clients.spawn(async move {
            if let Err(e) = serve_client(connection, source).await {
                tracing::debug!(error = %e, "camera client ended");
            }
        });
    };
    clients.shutdown().await;
    drop(listener);
    result
}

async fn serve_client(
    mut connection: BoxedConnection,
    source: Arc<dyn FrameSource>,
) -> Result<(), ProtocolError> {
    let mut buf = Vec::new();
    let (pid, format) =
        match tokio::time::timeout(HELLO_TIMEOUT, read(&mut connection, &mut buf)).await {
            Ok(Ok(Owned::Hello { pid, format })) => (pid, format),
            Ok(Ok(_)) => return Err(ProtocolError::Invalid("expected Hello".into())),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(ProtocolError::Invalid("no Hello in time".into())),
        };
    tracing::info!(pid, %format, "camera client connected");
    source.client_connected(format);
    // Also when the server stops and cancels this task mid-stream.
    let _connected = Connected {
        source: &*source,
        pid,
    };
    stream_to(connection, format, &*source).await
}

/// Tells the source a client left when dropped.
struct Connected<'a> {
    source: &'a dyn FrameSource,
    pid: u32,
}

impl Drop for Connected<'_> {
    fn drop(&mut self) {
        self.source.client_disconnected();
        tracing::info!(pid = self.pid, "camera client disconnected");
    }
}

async fn stream_to(
    connection: BoxedConnection,
    format: VideoFormat,
    source: &dyn FrameSource,
) -> Result<(), ProtocolError> {
    let (mut rd, wr) = tokio::io::split(connection);
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
    mut wr: tokio::io::WriteHalf<BoxedConnection>,
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
        let streaming = status.0 == StreamStatus::Streaming;
        if last_status.as_ref() != Some(&status) || now >= status_due {
            let msg = Message::Status {
                status: status.0,
                message: status.1.clone(),
            };
            wr.write_all(&encode(&msg)).await?;
            last_status = Some(status);
            status_due = now + STATUS_INTERVAL;
        }
        // Pictures don't arrive exactly once per slot (hardware decoding adds jitter). One
        // that is a little late still goes out in this slot instead of waiting a whole slot,
        // by when the next one has replaced it.
        // Waits on the source's signal where it has one: a short sleep lasts a whole timer tick
        // on Windows (about 15.6 ms), longer than the half slot at high frame rates. The
        // signal is only taken once a picture is late, and is taken before looking again, so a
        // picture published in between still ends the wait.
        let late_until = now + frame_interval(format) / 2;
        let mut next = source.next_frame(format, last_seq, &mut data);
        while next.is_none() && streaming && tokio::time::Instant::now() < late_until {
            match source.picture_signal() {
                Some(signal) => {
                    next = source.next_frame(format, last_seq, &mut data);
                    if next.is_some() || tokio::time::timeout_at(late_until, signal).await.is_err()
                    {
                        break;
                    }
                }
                None => tokio::time::sleep(LATE_POLL).await,
            }
            next = source.next_frame(format, last_seq, &mut data);
        }
        if let Some(seq) = next {
            if data.len() != format.frame_len() {
                tracing::error!(len = data.len(), %format, "frame source returned a wrong-sized frame");
                continue;
            }
            let header = FrameHeader {
                seq,
                timestamp: 0,
                format,
            };
            // Includes waiting for the client to read (the pipe's buffer is small).
            let started = std::time::Instant::now();
            wr.write_all(&frame_prefix(&header, data.len())).await?;
            wr.write_all(&data).await?;
            profile::record(Stage::Deliver, started.elapsed());
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
