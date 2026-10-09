//! The JSON configuration model.
//!
//! The whole configuration is one JSON object stored at `%APPDATA%\RtspCam\config.json`
//! (see [`ConfigStore`]). Every field has a default, so older or hand-written files load, and
//! unknown fields are kept in `extra` and written back unchanged.

mod migrate;
mod picture;
mod protocol;
mod store;
mod templates;
mod transfer;
mod url;
mod validate;
#[cfg(feature = "watch")]
mod watch;

use std::net::Ipv6Addr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

pub use migrate::CURRENT_VERSION;
pub use picture::{Crop, MAX_CROP_PERCENT, OnDisconnect, Picture, Rotation};
pub use protocol::Protocol;
pub use store::ConfigStore;
pub use templates::{BRAND_TEMPLATES, BrandTemplate, find_template};
pub use transfer::{ExportError, ImportReport, export_streams, import_streams, unique_stream_name};
pub use url::{StreamUrl, UrlError, parse_stream_url};
pub use validate::{Field, Problem, ValidationIssue};
#[cfg(feature = "watch")]
pub use watch::ConfigWatcher;

use crate::secret::Secret;

/// Root of `config.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version, used for migrations. See [`CURRENT_VERSION`].
    pub version: u64,
    pub settings: AppSettings,
    pub streams: Vec<StreamConfig>,
    /// Fields this build doesn't know about, kept so saving doesn't drop them.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            settings: AppSettings::default(),
            streams: Vec::new(),
            extra: Map::new(),
        }
    }
}

impl Config {
    pub fn stream(&self, id: Uuid) -> Option<&StreamConfig> {
        self.streams.iter().find(|s| s.id == id)
    }

    pub fn stream_mut(&mut self, id: Uuid) -> Option<&mut StreamConfig> {
        self.streams.iter_mut().find(|s| s.id == id)
    }
}

/// App-wide settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Launch at sign-in (HKCU `Run` key), so cameras exist before other apps start.
    pub start_with_windows: bool,
    /// Minimizing hides the window and leaves only the tray icon.
    pub minimize_to_tray: bool,
    pub log_level: LogLevel,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            minimize_to_tray: false,
            log_level: LogLevel::Info,
            extra: Map::new(),
        }
    }
}

/// One RTSP source, exposed as one virtual webcam.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamConfig {
    /// Stable identity: names the IPC pipe and the virtual camera. Generated if missing.
    pub id: Uuid,
    /// Shown as the webcam name ("<name> – Windows Virtual Camera"). Must be unique.
    pub name: String,
    pub enabled: bool,
    pub protocol: Protocol,
    /// IPv4, IPv6 or hostname.
    pub host: String,
    pub port: u16,
    /// For example `/Streaming/Channels/101`. May be empty.
    pub path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<Secret>,
    pub transport: Transport,
    pub output: OutputFormat,
    pub fit_mode: FitMode,
    /// Rotate, flip, crop, text overlay, and what to show when the stream drops.
    #[serde(skip_serializing_if = "Picture::is_default")]
    pub picture: Picture,
    /// Only connect while an app is using the webcam or the preview is open.
    pub on_demand: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        let protocol = Protocol::default();
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            enabled: true,
            port: protocol.default_port().unwrap_or_default(),
            protocol,
            host: String::new(),
            path: String::new(),
            username: String::new(),
            password: None,
            transport: Transport::default(),
            output: OutputFormat::default(),
            fit_mode: FitMode::default(),
            picture: Picture::default(),
            on_demand: true,
            extra: Map::new(),
        }
    }
}

impl StreamConfig {
    /// A new enabled stream with default settings and a fresh id.
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            ..Self::default()
        }
    }

    /// The stream URL, `rtsp://host:port/path`.
    ///
    /// Credentials are never included; pass them to the RTSP client separately so they can't
    /// leak into logs. IPv6 addresses are bracketed and a zone id's `%` is percent-encoded.
    pub fn url(&self) -> String {
        format!(
            "{}://{}:{}{}",
            self.protocol.scheme(),
            url_host(&self.host),
            self.port,
            normalize_path(&self.path)
        )
    }

    /// Username and password, if a username is set and the password could be decrypted.
    pub fn credentials(&self) -> Option<(&str, &str)> {
        let user = self.username.trim();
        if user.is_empty() {
            return None;
        }
        let pass = match &self.password {
            Some(secret) => secret.expose()?,
            None => "",
        };
        Some((user, pass))
    }
}

/// Formats a host for a URL: IPv6 literals are bracketed (`[fe80::1%25eth0]`).
fn url_host(host: &str) -> String {
    let host = host.trim();
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let (addr, zone) = match bare.split_once('%') {
        Some((addr, zone)) => (addr, Some(zone.strip_prefix("25").unwrap_or(zone))),
        None => (bare, None),
    };
    if addr.parse::<Ipv6Addr>().is_err() {
        return host.to_owned();
    }
    match zone {
        Some(zone) => format!("[{addr}%25{zone}]"),
        None => format!("[{addr}]"),
    }
}

/// Makes sure a non-empty path starts with `/`.
fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() || path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

/// RTP transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// RTP interleaved in the RTSP TCP connection. Works through NAT and firewalls.
    #[default]
    Tcp,
    Udp,
}

/// How the source picture is fitted to the advertised output size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FitMode {
    /// Scale to fit and pad with black bars.
    #[default]
    Letterbox,
    /// Scale to fill and cut off the overflow.
    Crop,
    /// Scale to fill, ignoring the aspect ratio.
    Stretch,
}

/// Size and frame rate the virtual camera advertises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl OutputFormat {
    /// Resolutions the virtual camera offers, largest first.
    pub const RESOLUTIONS: [(u32, u32); 3] = [(1920, 1080), (1280, 720), (640, 480)];
    /// Frame rates the virtual camera offers.
    pub const FRAME_RATES: [u32; 2] = [30, 15];

    pub fn is_supported(&self) -> bool {
        Self::RESOLUTIONS.contains(&(self.width, self.height))
            && Self::FRAME_RATES.contains(&self.fps)
    }
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
        }
    }
}

/// Log verbosity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

impl From<LogLevel> for tracing::level_filters::LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Self::ERROR,
            LogLevel::Warn => Self::WARN,
            LogLevel::Info => Self::INFO,
            LogLevel::Debug => Self::DEBUG,
            LogLevel::Trace => Self::TRACE,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn stream(host: &str, port: u16, path: &str) -> StreamConfig {
        StreamConfig {
            port,
            path: path.into(),
            ..StreamConfig::new("cam", host)
        }
    }

    #[test]
    fn url_ipv4_and_hostname() {
        assert_eq!(
            stream("192.168.1.50", 554, "/Streaming/Channels/101").url(),
            "rtsp://192.168.1.50:554/Streaming/Channels/101"
        );
        assert_eq!(
            stream("cam.local", 8554, "live").url(),
            "rtsp://cam.local:8554/live"
        );
        assert_eq!(stream(" cam.local ", 554, "").url(), "rtsp://cam.local:554");
    }

    #[test]
    fn url_ipv6_is_bracketed() {
        assert_eq!(stream("::1", 554, "/a").url(), "rtsp://[::1]:554/a");
        assert_eq!(stream("[fe80::1]", 554, "").url(), "rtsp://[fe80::1]:554");
        assert_eq!(
            stream("fe80::1%eth0", 554, "").url(),
            "rtsp://[fe80::1%25eth0]:554"
        );
        assert_eq!(
            stream("[fe80::1%25eth0]", 554, "").url(),
            "rtsp://[fe80::1%25eth0]:554"
        );
    }

    #[test]
    fn url_never_contains_credentials() {
        let mut s = stream("10.0.0.2", 554, "/x");
        s.username = "admin".into();
        s.password = Some(Secret::new("hunter2"));
        let url = s.url();
        assert!(!url.contains("admin") && !url.contains("hunter2") && !url.contains('@'));
        assert_eq!(s.credentials(), Some(("admin", "hunter2")));
    }

    #[test]
    fn missing_fields_use_defaults() {
        let config: Config = serde_json::from_value(json!({
            "streams": [{ "name": "Front Door", "host": "192.168.1.50" }]
        }))
        .unwrap();

        assert_eq!(config.version, CURRENT_VERSION);
        assert_eq!(config.settings, AppSettings::default());
        assert!(!config.settings.minimize_to_tray);
        assert!(!config.settings.start_with_windows);

        let s = &config.streams[0];
        assert_eq!(s.protocol, Protocol::Rtsp);
        assert_eq!(s.port, 554);
        assert_eq!(s.transport, Transport::Tcp);
        assert_eq!(
            s.output,
            OutputFormat {
                width: 1280,
                height: 720,
                fps: 30
            }
        );
        assert_eq!(s.fit_mode, FitMode::Letterbox);
        assert!(s.enabled);
        assert!(s.password.is_none());
        assert!(!s.id.is_nil());
    }

    #[test]
    fn empty_object_is_default_config() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn unknown_fields_are_preserved() {
        let input = json!({
            "version": 1,
            "future_setting": { "a": 1 },
            "settings": { "minimize_to_tray": true, "theme": "dark" },
            "streams": [{ "name": "A", "host": "h", "ptz": true }]
        });
        let config: Config = serde_json::from_value(input).unwrap();
        let out = serde_json::to_value(&config).unwrap();
        assert_eq!(out["future_setting"], json!({ "a": 1 }));
        assert_eq!(out["settings"]["theme"], "dark");
        assert_eq!(out["streams"][0]["ptz"], true);
    }

    #[test]
    fn unsupported_protocol_still_loads() {
        let config: Config = serde_json::from_value(json!({
            "streams": [
                { "name": "A", "host": "h", "protocol": "http" },
                { "name": "B", "host": "h" }
            ]
        }))
        .unwrap();
        assert_eq!(
            config.streams[0].protocol,
            Protocol::Unsupported("http".into())
        );
        assert_eq!(config.streams[1].protocol, Protocol::Rtsp);
        let out = serde_json::to_value(&config).unwrap();
        assert_eq!(out["streams"][0]["protocol"], "http");
    }

    #[cfg(windows)]
    #[test]
    fn json_round_trip() {
        let mut config = Config::default();
        config.settings.minimize_to_tray = true;
        config.settings.log_level = LogLevel::Debug;
        let mut s = StreamConfig::new("Front Door", "192.168.1.50");
        s.path = "/Streaming/Channels/101".into();
        s.username = "admin".into();
        s.password = Some(Secret::new("hunter2"));
        s.transport = Transport::Udp;
        s.output = OutputFormat {
            width: 1920,
            height: 1080,
            fps: 15,
        };
        s.fit_mode = FitMode::Crop;
        s.on_demand = false;
        config.streams.push(s);

        let text = serde_json::to_string_pretty(&config).unwrap();
        assert!(!text.contains("hunter2"));
        assert!(text.contains("\"password\": \"dpapi:"));
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(back, config);
    }

    #[test]
    fn plan_example_parses() {
        let config: Config = serde_json::from_value(json!({
          "version": 1,
          "settings": { "start_with_windows": false, "minimize_to_tray": true, "log_level": "info" },
          "streams": [{
              "id": "6f1c2d3e-8a4b-4c5d-9e0f-112233445566",
              "name": "Front Door",
              "enabled": true,
              "protocol": "rtsp",
              "host": "192.168.1.50",
              "port": 554,
              "path": "/Streaming/Channels/101",
              "username": "admin",
              "password": "dpapi:AQAAANCMnd8BFdERjHoAwE/Cl+sBAAAA",
              "transport": "tcp",
              "output": { "width": 1280, "height": 720, "fps": 30 },
              "fit_mode": "letterbox",
              "on_demand": true
          }]
        }))
        .unwrap();
        let s = &config.streams[0];
        assert_eq!(s.url(), "rtsp://192.168.1.50:554/Streaming/Channels/101");
        // Encrypted by someone else: kept, but not usable.
        assert!(s.password.as_ref().unwrap().expose().is_none());
        assert!(s.extra.is_empty());
    }
}
