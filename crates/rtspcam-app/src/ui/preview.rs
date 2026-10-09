//! The live preview: a worker thread takes the selected stream's newest picture, scales it to
//! the preview box and converts it to RGBA, then hands the finished image to the UI thread.
//!
//! The UI thread never touches the pipeline or does pixel work: it only swaps in an image that
//! is ready. At most one image is on its way to the UI at a time, so a busy UI is never queued
//! up with stale pictures, and pictures arrive at most at [`FRAME_INTERVAL`].

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtspcam_core::FitMode;
use rtspcam_engine::{Activity, Preview};
use rtspcam_pipeline::scale::nv12_to_rgba;
use rtspcam_pipeline::{Frame, Matrix, Scaler};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use super::generated::MainWindow;

/// How often the worker looks for a new picture (about the display rate of a camera).
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Default)]
struct Shared {
    /// The stream being previewed. Dropping it releases an on-demand stream.
    current: Mutex<Option<Arc<Preview>>>,
    /// The preview box in physical pixels.
    target: Mutex<(u32, u32)>,
    /// Bumped whenever the previewed stream changes, so late pictures of the old one are
    /// thrown away.
    generation: AtomicU64,
    /// An image is on its way to the UI thread.
    pending: AtomicBool,
    /// The last thing handed to the UI was a picture (not "nothing").
    has_picture: AtomicBool,
    stop: AtomicBool,
}

/// Owns the preview worker. Dropping it stops the worker.
pub(crate) struct PreviewFeed {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl PreviewFeed {
    pub(crate) fn start(ui: slint::Weak<MainWindow>) -> Self {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let thread = thread::Builder::new()
            .name("preview".into())
            .spawn(move || run(&worker, &ui))
            .map_err(|e| tracing::error!(error = %e, "could not start the preview"))
            .ok();
        Self { shared, thread }
    }

    /// Previews `preview` instead of whatever was shown (`None`: nothing).
    pub(crate) fn set(&self, preview: Option<Preview>) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.has_picture.store(false, Ordering::SeqCst);
        *self
            .shared
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = preview.map(Arc::new);
    }

    pub(crate) fn is_set(&self) -> bool {
        self.current().is_some()
    }

    /// The previewed stream's state, for the message shown when there is no picture.
    pub(crate) fn activity(&self) -> Option<Activity> {
        self.current().map(|p| p.activity())
    }

    /// Whether a picture is showing.
    pub(crate) fn has_picture(&self) -> bool {
        self.shared.has_picture.load(Ordering::SeqCst)
    }

    /// The preview box's size in physical pixels.
    pub(crate) fn set_target(&self, width: u32, height: u32) {
        *self
            .shared
            .target
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = (width, height);
    }

    fn current(&self) -> Option<Arc<Preview>> {
        self.shared
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for PreviewFeed {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.set(None);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Arc<Shared>, ui: &slint::Weak<MainWindow>) {
    let mut scaler = Scaler::new();
    let mut rgba = Vec::new();
    // The picture last converted, and for which stream and size.
    let mut last: Option<(Arc<Frame>, u64, (u32, u32))> = None;
    while !shared.stop.load(Ordering::SeqCst) {
        thread::sleep(FRAME_INTERVAL);
        if shared.pending.load(Ordering::SeqCst) {
            continue; // the UI hasn't taken the last one yet
        }
        let generation = shared.generation.load(Ordering::SeqCst);
        let preview = shared
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let target = *shared.target.lock().unwrap_or_else(PoisonError::into_inner);
        let frame = preview.as_ref().and_then(|p| p.latest());
        drop(preview);

        let Some(frame) = frame else {
            // Nothing to show: clear the picture once.
            if last.take().is_some() || shared.has_picture.swap(false, Ordering::SeqCst) {
                deliver(shared, ui, generation, None);
            }
            continue;
        };
        if last
            .as_ref()
            .is_some_and(|(f, g, t)| Arc::ptr_eq(f, &frame) && *g == generation && *t == target)
        {
            continue;
        }
        let Some((w, h)) = fit_within(frame.width(), frame.height(), target) else {
            continue; // the preview box isn't laid out yet
        };
        let scaled = scaler.scale(&frame, w, h, FitMode::Stretch);
        nv12_to_rgba(&scaled, Matrix::for_height(frame.height()), &mut rgba);
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
        buffer.make_mut_bytes().copy_from_slice(&rgba);
        last = Some((frame, generation, target));
        deliver(shared, ui, generation, Some(buffer));
    }
}

/// Hands a picture (or "no picture") to the UI thread, unless the previewed stream changed in
/// the meantime.
fn deliver(
    shared: &Arc<Shared>,
    ui: &slint::Weak<MainWindow>,
    generation: u64,
    picture: Option<SharedPixelBuffer<Rgba8Pixel>>,
) {
    shared.pending.store(true, Ordering::SeqCst);
    let (shared, ui) = (shared.clone(), ui.clone());
    let sent = slint::invoke_from_event_loop(move || {
        shared.pending.store(false, Ordering::SeqCst);
        if shared.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let Some(ui) = ui.upgrade() else { return };
        shared
            .has_picture
            .store(picture.is_some(), Ordering::SeqCst);
        ui.set_preview_image(picture.map(Image::from_rgba8).unwrap_or_default());
    });
    if sent.is_err() {
        // The event loop has ended: the app is quitting, and `pending` stays set.
        tracing::debug!("the UI has gone; the preview stops delivering");
    }
}

/// The largest even size with the picture's shape that fits in `target`, or `None` while the
/// target is empty.
pub(crate) fn fit_within(width: u32, height: u32, target: (u32, u32)) -> Option<(u32, u32)> {
    let (tw, th) = target;
    if width == 0 || height == 0 || tw < 2 || th < 2 {
        return None;
    }
    let scale = f64::min(
        f64::from(tw) / f64::from(width),
        f64::from(th) / f64::from(height),
    );
    let w = ((f64::from(width) * scale) as u32).max(2) & !1;
    let h = ((f64::from(height) * scale) as u32).max(2) & !1;
    Some((w, h))
}

/// A frame as an image at most `max` pixels in size (for thumbnails). Any thread.
pub(crate) fn thumbnail(frame: &Frame, max: (u32, u32)) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let (w, h) = fit_within(frame.width(), frame.height(), max)?;
    let scaled = Scaler::new().scale(frame, w, h, FitMode::Stretch);
    let mut rgba = Vec::new();
    nv12_to_rgba(&scaled, Matrix::for_height(frame.height()), &mut rgba);
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    buffer.make_mut_bytes().copy_from_slice(&rgba);
    Some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_keep_their_shape_inside_the_box() {
        assert_eq!(fit_within(1920, 1080, (480, 270)), Some((480, 270)));
        assert_eq!(fit_within(2304, 1296, (960, 600)), Some((960, 540)));
        // Portrait (a rotated camera) is pillarboxed.
        assert_eq!(fit_within(720, 1280, (480, 270)), Some((150, 270)));
        assert_eq!(fit_within(1280, 720, (0, 0)), None);
        assert_eq!(fit_within(0, 720, (480, 270)), None);
    }

    #[test]
    fn thumbnails_are_rgba_of_the_right_size() {
        let buffer = thumbnail(&Frame::black(640, 480), (112, 63)).unwrap();
        assert_eq!((buffer.width(), buffer.height()), (84, 62));
        assert!(buffer.as_slice().iter().all(|p| p.r < 5 && p.a == 255));
    }
}
