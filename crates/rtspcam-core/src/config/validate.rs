use std::collections::HashSet;
use std::net::{IpAddr, Ipv6Addr};

use uuid::Uuid;

use super::{Config, OutputFormat, StreamConfig};

/// The stream field a [`ValidationIssue`] refers to, so the UI can show the error next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Id,
    Name,
    Protocol,
    Host,
    Port,
    Path,
    Password,
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Problem {
    #[error("a name is required")]
    NameRequired,
    #[error("another stream is already called \"{0}\"")]
    NameNotUnique(String),
    #[error("another stream has the same id")]
    DuplicateId,
    #[error("unsupported protocol \"{0}\"")]
    UnsupportedProtocol(String),
    #[error("an IP address or host name is required")]
    HostRequired,
    #[error("\"{0}\" is not a valid IP address or host name")]
    InvalidHost(String),
    #[error("port must be between 1 and 65535")]
    InvalidPort,
    #[error("path must not contain spaces or control characters")]
    InvalidPath,
    #[error("the saved password can't be decrypted (was the config copied from another user or PC?); enter it again")]
    PasswordLocked,
    #[error("unsupported output format {width}x{height} @ {fps} fps")]
    UnsupportedOutput { width: u32, height: u32, fps: u32 },
}

/// One problem with one stream.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("stream {stream}: {problem}")]
pub struct ValidationIssue {
    pub stream: Uuid,
    pub field: Field,
    pub problem: Problem,
}

impl Config {
    /// Checks every stream, plus that names and ids are unique. An empty result means valid.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        for stream in &self.streams {
            issues.extend(stream.validate());
            if !ids.insert(stream.id) {
                issues.push(issue(stream, Field::Id, Problem::DuplicateId));
            }
            let key = stream.name.trim().to_lowercase();
            if !key.is_empty() && !names.insert(key) {
                let name = stream.name.trim().to_owned();
                issues.push(issue(stream, Field::Name, Problem::NameNotUnique(name)));
            }
        }
        issues
    }
}

impl StreamConfig {
    /// Checks this stream on its own. Uniqueness is checked by [`Config::validate`].
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let mut push = |field, problem| issues.push(issue(self, field, problem));

        if self.name.trim().is_empty() {
            push(Field::Name, Problem::NameRequired);
        }
        if !self.protocol.is_supported() {
            push(Field::Protocol, Problem::UnsupportedProtocol(self.protocol.scheme().to_owned()));
        }
        let host = self.host.trim();
        if host.is_empty() {
            push(Field::Host, Problem::HostRequired);
        } else if !is_valid_host(host) {
            push(Field::Host, Problem::InvalidHost(host.to_owned()));
        }
        if self.port == 0 {
            push(Field::Port, Problem::InvalidPort);
        }
        if self.path.trim().chars().any(|c| c.is_whitespace() || c.is_control()) {
            push(Field::Path, Problem::InvalidPath);
        }
        if self.password.as_ref().is_some_and(|p| p.expose().is_none()) {
            push(Field::Password, Problem::PasswordLocked);
        }
        if !self.output.is_supported() {
            let OutputFormat { width, height, fps } = self.output;
            push(Field::Output, Problem::UnsupportedOutput { width, height, fps });
        }
        issues
    }
}

fn issue(stream: &StreamConfig, field: Field, problem: Problem) -> ValidationIssue {
    ValidationIssue { stream: stream.id, field, problem }
}

/// IPv4, IPv6 (optionally bracketed and/or with a `%zone`), or an RFC 1123 host name.
pub(crate) fn is_valid_host(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    let bare = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if let Some((addr, zone)) = bare.split_once('%') {
        return !zone.is_empty()
            && zone.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            && addr.parse::<Ipv6Addr>().is_ok();
    }
    if bare.parse::<Ipv6Addr>().is_ok() {
        return true;
    }
    is_valid_hostname(host)
}

fn is_valid_hostname(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        (1..=63).contains(&l.len())
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    // A numeric last label means a mistyped IPv4 address such as "192.168.1.300".
    let last_is_numeric = labels.last().is_some_and(|l| l.chars().all(|c| c.is_ascii_digit()));
    labels.iter().all(label_ok) && !last_is_numeric
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Protocol, Secret};

    #[test]
    fn hosts() {
        for ok in [
            "192.168.1.50",
            "10.0.0.1",
            "::1",
            "[::1]",
            "fe80::1%eth0",
            "cam",
            "cam-1.local",
            "nvr.example.com.",
        ] {
            assert!(is_valid_host(ok), "{ok} should be valid");
        }
        for bad in [
            "",
            "192.168.1.300",
            "192.168.1",
            "rtsp://cam",
            "admin@cam",
            "cam:554",
            "-cam",
            "cam-",
            "has space",
            "a..b",
            "fe80::1%",
            &"a".repeat(64),
        ] {
            assert!(!is_valid_host(bad), "{bad} should be invalid");
        }
    }

    #[test]
    fn valid_stream_has_no_issues() {
        let s = StreamConfig::new("Front Door", "192.168.1.50");
        assert_eq!(s.validate(), vec![]);
    }

    #[test]
    fn reports_each_bad_field() {
        let mut s = StreamConfig::new("  ", "300.1.1.1");
        s.protocol = Protocol::Unsupported("http".into());
        s.port = 0;
        s.path = "/a b".into();
        s.password = Some(Secret::from_stored("dpapi:AAAA"));
        s.output.fps = 60;
        let fields: Vec<Field> = s.validate().into_iter().map(|i| i.field).collect();
        assert_eq!(
            fields,
            [
                Field::Name,
                Field::Protocol,
                Field::Host,
                Field::Port,
                Field::Path,
                Field::Password,
                Field::Output
            ]
        );
    }

    #[test]
    fn names_must_be_unique_ignoring_case_and_spaces() {
        let mut config = Config::default();
        config.streams.push(StreamConfig::new("Front Door", "10.0.0.1"));
        config.streams.push(StreamConfig::new(" front door ", "10.0.0.2"));
        let issues = config.validate();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].field, Field::Name);
        assert_eq!(issues[0].stream, config.streams[1].id);
    }

    #[test]
    fn ids_must_be_unique() {
        let mut config = Config::default();
        let a = StreamConfig::new("A", "10.0.0.1");
        let b = StreamConfig { id: a.id, ..StreamConfig::new("B", "10.0.0.2") };
        config.streams = vec![a, b];
        assert_eq!(config.validate()[0].problem, Problem::DuplicateId);
    }
}
