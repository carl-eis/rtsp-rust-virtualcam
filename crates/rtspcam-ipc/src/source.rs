//! Where a camera's frames come from.

use std::future::Future;
use std::pin::Pin;

use crate::protocol::{StreamStatus, VideoFormat};

/// What [`FrameSource::picture_signal`] returns.
pub type PictureSignal = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A camera's pictures and state, as the app offers them to whatever delivers them to apps.
///
/// Implemented by the app's camera manager. A virtual camera backend uses it in one of two
/// ways: a *serving* backend (the Windows DLL) hands it to [`serve`](crate::server::serve) and
/// the consumer pulls frames over a connection; a *push* backend (v4l2loopback, a CoreMediaIO
/// extension) hands it to `rtspcam_platform::push::Pusher`, which calls `client_connected`
/// while an app reads the camera and `next_frame` at the device's frame rate.
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

    /// Finishes when a picture newer than any published before this call may be ready, so a
    /// late picture can be waited for without polling. `None` (the default): the caller polls.
    fn picture_signal(&self) -> Option<PictureSignal> {
        None
    }
}
