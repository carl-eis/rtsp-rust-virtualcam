//! Where a camera's frames come from.

use crate::protocol::{StreamStatus, VideoFormat};

/// A camera's pictures and state, as the app offers them to whatever delivers them to apps.
///
/// Implemented by the app's camera manager. A virtual camera backend uses it in one of two
/// ways: a *serving* backend (the Windows DLL, a future macOS extension) hands it to
/// [`serve`](crate::server::serve) and the consumer pulls frames over a connection; a *push*
/// backend (a future v4l2loopback device) calls the methods itself: `client_connected` when it
/// starts, then `next_frame` at the device's frame rate.
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
