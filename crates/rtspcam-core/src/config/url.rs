//! Splitting a pasted stream URL into the fields of a [`StreamConfig`](super::StreamConfig).
//!
//! The Add/Edit dialog's "paste full URL" box and the developer CLI accept URLs such as
//! `rtsp://admin:secret@192.168.1.50:554/Streaming/Channels/101`. Credentials are split out so
//! they can be stored encrypted and never appear in the URL that is logged.

use percent_encoding::percent_decode_str;
use url::{Host, Url};

use super::Protocol;

/// The parts of a stream URL, ready to copy into a [`StreamConfig`](super::StreamConfig).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamUrl {
    pub protocol: Protocol,
    /// Host name or IP address. IPv6 addresses are returned without brackets.
    pub host: String,
    /// The explicit port, or the protocol's default port.
    pub port: u16,
    /// Path including any query string (some cameras use `?channel=1&subtype=0`), or empty.
    pub path: String,
    /// Percent-decoded user name, or empty.
    pub username: String,
    /// Percent-decoded password, if the URL had one.
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("not a valid URL: {0}")]
    Invalid(String),
    #[error("unsupported scheme \"{0}\"; only rtsp:// URLs are supported")]
    UnsupportedScheme(String),
    #[error("the URL has no host")]
    NoHost,
    #[error("the user name or password is not valid UTF-8 after percent-decoding")]
    BadCredentials,
}

/// Parses `rtsp://[user[:password]@]host[:port][/path][?query]`.
pub fn parse_stream_url(input: &str) -> Result<StreamUrl, UrlError> {
    let url = Url::parse(input.trim()).map_err(|e| UrlError::Invalid(e.to_string()))?;
    let protocol: Protocol = url.scheme().parse().unwrap_or_else(|never| match never {});
    if !protocol.is_supported() {
        return Err(UrlError::UnsupportedScheme(url.scheme().to_owned()));
    }
    let host = match url.host() {
        // Non-special schemes keep IPv4 addresses as opaque "domains"; both are fine as-is.
        Some(Host::Domain(d)) if !d.is_empty() => d.to_owned(),
        Some(Host::Ipv4(ip)) => ip.to_string(),
        Some(Host::Ipv6(ip)) => ip.to_string(),
        _ => return Err(UrlError::NoHost),
    };
    let port = url
        .port()
        .or_else(|| protocol.default_port())
        .unwrap_or_default();
    let mut path = url.path().to_owned();
    if path == "/" {
        path.clear();
    }
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let decode = |s: &str| {
        percent_decode_str(s)
            .decode_utf8()
            .map(|s| s.into_owned())
            .map_err(|_| UrlError::BadCredentials)
    };
    Ok(StreamUrl {
        protocol,
        host,
        port,
        path,
        username: decode(url.username())?,
        password: url.password().map(decode).transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_url_with_credentials() {
        let u =
            parse_stream_url("rtsp://admin:p%40ss%3Aword@192.168.1.50:8554/Streaming/Channels/101")
                .unwrap();
        assert_eq!(u.protocol, Protocol::Rtsp);
        assert_eq!(u.host, "192.168.1.50");
        assert_eq!(u.port, 8554);
        assert_eq!(u.path, "/Streaming/Channels/101");
        assert_eq!(u.username, "admin");
        assert_eq!(u.password.as_deref(), Some("p@ss:word"));
    }

    #[test]
    fn defaults_and_query() {
        let u = parse_stream_url(" RTSP://Cam.local/cam/realmonitor?channel=1&subtype=0 ").unwrap();
        assert_eq!(u.host, "Cam.local");
        assert_eq!(u.port, 554);
        assert_eq!(u.path, "/cam/realmonitor?channel=1&subtype=0");
        assert_eq!(u.username, "");
        assert_eq!(u.password, None);

        let u = parse_stream_url("rtsp://10.0.0.2").unwrap();
        assert_eq!(u.path, "");
        let u = parse_stream_url("rtsp://10.0.0.2/").unwrap();
        assert_eq!(u.path, "");
    }

    #[test]
    fn ipv6_host() {
        let u = parse_stream_url("rtsp://[fe80::1]:554/live").unwrap();
        assert_eq!(u.host, "fe80::1");
        assert_eq!(u.port, 554);
    }

    #[test]
    fn user_without_password() {
        let u = parse_stream_url("rtsp://viewer@cam/live").unwrap();
        assert_eq!(u.username, "viewer");
        assert_eq!(u.password, None);
    }

    #[test]
    fn rejects_other_schemes_and_garbage() {
        assert_eq!(
            parse_stream_url("http://cam/video.mjpg"),
            Err(UrlError::UnsupportedScheme("http".into()))
        );
        assert!(matches!(
            parse_stream_url("192.168.1.50"),
            Err(UrlError::Invalid(_))
        ));
        assert_eq!(parse_stream_url("rtsp:///live"), Err(UrlError::NoHost));
    }

    #[test]
    fn round_trips_through_stream_config_url() {
        let u = parse_stream_url("rtsp://u:p@[::1]:8554/a?b=c").unwrap();
        let s = super::super::StreamConfig {
            port: u.port,
            path: u.path,
            ..super::super::StreamConfig::new("x", u.host)
        };
        assert_eq!(s.url(), "rtsp://[::1]:8554/a?b=c");
    }
}
