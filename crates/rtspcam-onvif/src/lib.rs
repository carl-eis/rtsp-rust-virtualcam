//! ONVIF support: finding cameras on the local network and asking them for their RTSP
//! addresses.
//!
//! - [`discover`]: WS-Discovery over UDP multicast (`239.255.255.250:3702`), which makes ONVIF
//!   cameras answer with their name and device-service address.
//! - [`query_camera`]: SOAP calls to a camera (`GetDeviceInformation`, `GetProfiles`,
//!   `GetStreamUri`), signed with a WS-Security UsernameToken when credentials are given.
//!
//! Only plain-HTTP device services are supported (that is what almost every camera offers).
//! The code is small on purpose: the SOAP is written by hand and the HTTP client is a few dozen
//! lines over a tokio TCP stream, so the app does not pull in a TLS stack for this.

mod client;
mod discovery;
mod soap;
mod time;
mod xml;

pub use client::{CameraInfo, Credentials, Profile, query_camera};
pub use discovery::{DiscoveredDevice, discover, parse_probe_match, probe_message};

/// Why an ONVIF request failed. Each variant is something the user can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OnvifError {
    #[error("could not reach the camera: {0}")]
    Network(String),
    #[error("the camera did not answer in time")]
    Timeout,
    #[error("the camera rejected the user name or password")]
    Unauthorized,
    #[error("the camera reported an error: {0}")]
    Fault(String),
    #[error("the camera sent a reply that could not be understood: {0}")]
    Protocol(String),
    #[error("only plain http:// ONVIF addresses are supported (got {0})")]
    UnsupportedScheme(String),
}

impl OnvifError {
    /// What to try next, if there is something.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Unauthorized => Some(
                "Check the camera's ONVIF account. Many cameras use a separate ONVIF user, \
                 and need the camera's clock to be close to this PC's.",
            ),
            Self::Timeout | Self::Network(_) => {
                Some("Check that the camera is on, on the same network, and that ONVIF is enabled.")
            }
            Self::Protocol(_) | Self::Fault(_) => Some(
                "This camera may not fully support ONVIF. Use a brand preset or type the address.",
            ),
            Self::UnsupportedScheme(_) => None,
        }
    }
}
