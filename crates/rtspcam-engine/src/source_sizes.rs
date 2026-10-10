//! The picture size each "Source" stream's camera last sent, remembered across runs.
//!
//! A virtual camera lists its formats when it is created, but the camera's own size is only
//! known once the stream connects. Remembering it means the camera offers the right size from
//! the start next time, and is re-created only when the size changes (OBS keeps its virtual
//! camera's size in a file the same way). Until a stream's first connection the camera offers
//! the stream's configured size.
//!
//! The file is a JSON object of stream id to `[width, height]`, next to `config.json`. It is
//! a cache: a missing or unreadable file only means the sizes are learned again.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use uuid::Uuid;

/// File name of the cache in the config folder.
pub const FILE_NAME: &str = "source-sizes.json";

#[derive(Debug, Default)]
pub(crate) struct SourceSizes {
    /// `None`: kept in memory only.
    path: Option<PathBuf>,
    sizes: Mutex<BTreeMap<Uuid, (u32, u32)>>,
}

impl SourceSizes {
    /// Loads the sizes saved in `path`, if any.
    pub(crate) fn load(path: Option<PathBuf>) -> Self {
        let sizes = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| match serde_json::from_slice(&bytes) {
                Ok(sizes) => Some(sizes),
                Err(e) => {
                    tracing::debug!(error = %e, "ignoring the saved camera picture sizes");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            path,
            sizes: Mutex::new(sizes),
        }
    }

    pub(crate) fn get(&self, id: Uuid) -> Option<(u32, u32)> {
        self.lock().get(&id).copied()
    }

    /// Remembers stream `id`'s size; saves the file if it changed.
    pub(crate) fn set(&self, id: Uuid, size: (u32, u32)) {
        let mut sizes = self.lock();
        if sizes.insert(id, size) != Some(size) {
            self.save(&sizes);
        }
    }

    /// Forgets the streams not in `keep` (removed, or no longer "Source").
    pub(crate) fn retain(&self, keep: impl Fn(Uuid) -> bool) {
        let mut sizes = self.lock();
        let before = sizes.len();
        sizes.retain(|id, _| keep(*id));
        if sizes.len() != before {
            self.save(&sizes);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<Uuid, (u32, u32)>> {
        self.sizes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes a temporary file and renames it over the old one, so a crash can't leave half a
    /// file. A few bytes, written only when a camera's size changes.
    fn save(&self, sizes: &BTreeMap<Uuid, (u32, u32)>) {
        let Some(path) = &self.path else { return };
        let tmp = path.with_extension("json.tmp");
        let result = serde_json::to_vec_pretty(sizes)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = result {
            tracing::warn!(path = %path.display(), error = %e, "could not save the camera picture sizes");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let sizes = SourceSizes::load(Some(path.clone()));
        assert_eq!(sizes.get(a), None);
        sizes.set(a, (2560, 1440));
        sizes.set(b, (640, 480));
        let again = SourceSizes::load(Some(path.clone()));
        assert_eq!(again.get(a), Some((2560, 1440)));
        assert_eq!(again.get(b), Some((640, 480)));
        again.retain(|id| id == a);
        let again = SourceSizes::load(Some(path));
        assert_eq!(again.get(a), Some((2560, 1440)));
        assert_eq!(again.get(b), None);
    }

    #[test]
    fn a_broken_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "not json").unwrap();
        let sizes = SourceSizes::load(Some(path.clone()));
        let id = Uuid::new_v4();
        assert_eq!(sizes.get(id), None);
        sizes.set(id, (1280, 720));
        assert_eq!(SourceSizes::load(Some(path)).get(id), Some((1280, 720)));
        // In memory only.
        let sizes = SourceSizes::load(None);
        sizes.set(id, (1280, 720));
        assert_eq!(sizes.get(id), Some((1280, 720)));
    }
}
