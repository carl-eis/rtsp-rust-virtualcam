use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Streaming protocol of a source.
///
/// Only RTSP is supported for now, but the field exists so more protocols can be added without
/// changing the config schema. An unknown value doesn't stop the config from loading: it becomes
/// [`Protocol::Unsupported`], is reported by validation, and is written back unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Protocol {
    #[default]
    Rtsp,
    /// A value this build doesn't understand, as written in the file.
    Unsupported(String),
}

impl Protocol {
    /// Protocols this build can stream from, in the order the UI lists them.
    pub const SUPPORTED: &[Protocol] = &[Protocol::Rtsp];

    /// Value used in `config.json` and as the URL scheme.
    pub fn scheme(&self) -> &str {
        match self {
            Self::Rtsp => "rtsp",
            Self::Unsupported(s) => s,
        }
    }

    /// Name shown in the UI.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Rtsp => "RTSP",
            Self::Unsupported(s) => s,
        }
    }

    pub fn default_port(&self) -> Option<u16> {
        match self {
            Self::Rtsp => Some(554),
            Self::Unsupported(_) => None,
        }
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::Unsupported(_))
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

impl Serialize for Protocol {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.scheme())
    }
}

impl FromStr for Protocol {
    type Err = Infallible;

    /// Case-insensitive. Unknown values become [`Protocol::Unsupported`] with the original text.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value.trim().to_ascii_lowercase().as_str() {
            "rtsp" => Self::Rtsp,
            _ => Self::Unsupported(value.to_owned()),
        })
    }
}

impl<'de> Deserialize<'de> for Protocol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(value.parse().unwrap_or_else(|never| match never {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtsp_defaults() {
        assert_eq!(Protocol::default(), Protocol::Rtsp);
        assert_eq!(Protocol::Rtsp.default_port(), Some(554));
        assert_eq!(Protocol::SUPPORTED, &[Protocol::Rtsp]);
    }

    #[test]
    fn parse_is_case_insensitive() {
        let p: Protocol = serde_json::from_str("\"RTSP\"").unwrap();
        assert_eq!(p, Protocol::Rtsp);
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"rtsp\"");
    }

    #[test]
    fn unknown_value_round_trips() {
        let p: Protocol = serde_json::from_str("\"srt\"").unwrap();
        assert!(!p.is_supported());
        assert_eq!(p.default_port(), None);
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"srt\"");
    }
}
