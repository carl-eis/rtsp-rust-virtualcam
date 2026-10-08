//! Per-camera "latest frame" bus.
//!
//! The decoder publishes every picture; readers (the virtual camera's IPC server, the UI
//! preview) only ever see the newest one. Publishing never waits for readers, so a slow reader
//! can't hold up decoding: it just skips frames.

use std::sync::Arc;

use tokio::sync::watch;

use crate::Frame;

/// Sending side, owned by the pipeline. Cheap to clone.
#[derive(Debug, Clone)]
pub struct FrameBus {
    tx: Arc<watch::Sender<Option<Arc<Frame>>>>,
}

impl Default for FrameBus {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameBus {
    pub fn new() -> Self {
        Self {
            tx: Arc::new(watch::Sender::new(None)),
        }
    }

    /// Replaces the latest frame and wakes waiting readers.
    pub fn publish(&self, frame: Frame) {
        self.tx.send_replace(Some(Arc::new(frame)));
    }

    /// Forgets the latest frame (readers then see `None` until the next one).
    pub fn clear(&self) {
        self.tx.send_replace(None);
    }

    /// The newest frame, if any.
    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> FrameReceiver {
        FrameReceiver {
            rx: self.tx.subscribe(),
        }
    }
}

/// Reading side. Each receiver tracks which frame it has seen.
#[derive(Debug, Clone)]
pub struct FrameReceiver {
    rx: watch::Receiver<Option<Arc<Frame>>>,
}

impl FrameReceiver {
    /// The newest frame, marking it as seen.
    pub fn latest(&mut self) -> Option<Arc<Frame>> {
        self.rx.borrow_and_update().clone()
    }

    /// Whether a frame newer than the last one returned by [`latest`](Self::latest) or
    /// [`changed`](Self::changed) is available.
    pub fn has_new(&self) -> bool {
        self.rx.has_changed().unwrap_or(false)
    }

    /// Waits for the next frame. Returns `None` once the bus is gone.
    pub async fn changed(&mut self) -> Option<Option<Arc<Frame>>> {
        self.rx.changed().await.ok()?;
        Some(self.rx.borrow_and_update().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_see_only_the_latest_frame() {
        let bus = FrameBus::new();
        let mut rx = bus.subscribe();
        assert!(rx.latest().is_none());
        assert!(!rx.has_new());

        bus.publish(Frame::black(2, 2));
        bus.publish(Frame::black(4, 2));
        assert!(rx.has_new());
        assert_eq!(rx.latest().unwrap().width(), 4);
        assert!(!rx.has_new());

        bus.clear();
        assert!(rx.has_new());
        assert!(rx.latest().is_none());
    }

    #[tokio::test]
    async fn changed_wakes_and_ends_with_the_bus() {
        let bus = FrameBus::new();
        let mut rx = bus.subscribe();
        let publisher = bus.clone();
        tokio::spawn(async move { publisher.publish(Frame::black(2, 2)) });
        let got = rx.changed().await.unwrap().unwrap();
        assert_eq!(got.width(), 2);
        drop(bus);
        assert!(rx.changed().await.is_none());
    }
}
