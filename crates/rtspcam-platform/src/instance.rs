//! One running copy of the app per user session.

use std::fmt;
use std::io;

/// Claims "the running copy" for a name, or tells the copy that holds it to show itself.
///
/// - **Windows**: the named mutex `Local\<name>.SingleInstance`; a second copy signals the
///   named event `Local\<name>.Show`.
/// - **Linux and macOS**: an exclusive lock on `<name>.lock` in the runtime folder (the OS drops
///   it if the process dies); a second copy connects to the Unix socket `<name>.sock` next to it.
pub trait SingleInstance: Send + Sync {
    fn acquire(&self) -> io::Result<Instance>;
}

/// The outcome of [`SingleInstance::acquire`].
#[derive(Debug)]
pub enum Instance {
    /// This is the only copy. Keep the lock alive for the life of the process.
    First(Box<dyn InstanceLock>),
    /// Another copy runs; it has been asked to show its window.
    AlreadyRunning,
}

/// Held by the first copy. Dropping it releases the name.
pub trait InstanceLock: Send + fmt::Debug {
    /// Calls `on_show` (on a background thread) each time another copy is started.
    fn on_show(&mut self, on_show: Box<dyn Fn() + Send>);
}
