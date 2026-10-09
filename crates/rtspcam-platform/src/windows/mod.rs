//! Windows 11: Media Foundation virtual cameras, DPAPI, the `Run` key, named pipes.
//!
//! Everything here was moved unchanged from the crates that used it before the
//! cross-platform split (see `documentation/09-cross-platform-plan.md`), so Windows behaves as
//! before.

mod autostart;
mod camera;
mod desktop;
mod dpapi;
mod files;
mod instance;
mod mf_decoder;
mod pipe;

use std::sync::Arc;

pub(crate) use desktop::{attach_parent_console, open_folder, process_memory};

use crate::{
    Autostart, FileReplace, FrameTransport, SecretStore, SingleInstance, VirtualCameraBackend,
};

pub(crate) fn install() {
    rtspcam_core::secret::install_store(secret_store());
    rtspcam_pipeline::decode::install_platform_decoders(Box::new(mf_decoder::MfDecoders));
}

pub(crate) fn camera_backend() -> Arc<dyn VirtualCameraBackend> {
    Arc::new(camera::VcamBackend::start())
}

pub(crate) fn secret_store() -> Box<dyn SecretStore> {
    Box::new(dpapi::DpapiStore)
}

pub(crate) fn autostart() -> Box<dyn Autostart> {
    Box::new(autostart::RunKey::default())
}

pub(crate) fn single_instance(name: &str) -> Box<dyn SingleInstance> {
    Box::new(instance::NamedMutex::new(name))
}

pub(crate) fn file_replacer() -> Arc<dyn FileReplace> {
    Arc::new(files::ReplaceFile)
}

pub(crate) fn frame_transport() -> Arc<dyn FrameTransport> {
    Arc::new(pipe::NamedPipes)
}
