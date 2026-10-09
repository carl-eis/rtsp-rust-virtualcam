//! Named pipes `\\.\pipe\rtspcam\<id>`: how the virtual camera DLL inside Frame Server reaches
//! the app.

use std::fs::OpenOptions;
use std::io;

use rtspcam_core::constants::frame_pipe_name;
use rtspcam_ipc::server::{Accept, BoxedConnection, FrameListener};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use uuid::Uuid;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, GetLastError};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::core::HSTRING;

use crate::transport::{FrameTransport, ReadWrite};

/// The Windows [`FrameTransport`].
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct NamedPipes;

impl FrameTransport for NamedPipes {
    fn listen(&self, id: Uuid) -> io::Result<Box<dyn FrameListener>> {
        let name = frame_pipe_name(id);
        // `first_pipe_instance` makes this fail if another process serves the camera.
        let next = create_instance(&name, true)?;
        Ok(Box::new(PipeListener { name, next }))
    }

    fn connect(&self, id: Uuid) -> io::Result<Box<dyn ReadWrite>> {
        let pipe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(frame_pipe_name(id))?;
        Ok(Box::new(pipe))
    }

    /// Asks Windows without connecting: opening the pipe (which `Path::exists` does) would
    /// take a server instance away from a real client.
    fn is_served(&self, id: Uuid) -> bool {
        let name = HSTRING::from(frame_pipe_name(id));
        // SAFETY: a valid null-terminated pipe name; the call only queries.
        let found = unsafe { WaitNamedPipeW(&name, 1) }.as_bool();
        // SAFETY: reads the calling thread's last error.
        found || unsafe { GetLastError() } != ERROR_FILE_NOT_FOUND
    }
}

/// One camera's pipe. There is always one instance waiting for the next client.
struct PipeListener {
    name: String,
    next: NamedPipeServer,
}

impl FrameListener for PipeListener {
    fn accept(&mut self) -> Accept<'_> {
        Box::pin(async move {
            self.next.connect().await?;
            // Create the next instance before serving, so new clients never see "no pipe".
            let next = create_instance(&self.name, false)?;
            let connected = std::mem::replace(&mut self.next, next);
            Ok(Box::new(connected) as BoxedConnection)
        })
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
