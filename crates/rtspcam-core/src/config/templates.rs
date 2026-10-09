//! RTSP address templates for common camera brands, for when ONVIF discovery isn't available.
//!
//! These are the factory-default paths for channel 1. Cameras can be configured differently, so
//! the add dialog only uses them to fill in the port and path; the user can edit both.

/// One brand's address for one stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrandTemplate {
    pub brand: &'static str,
    /// "main stream", "sub stream", ...
    pub stream: &'static str,
    pub port: u16,
    pub path: &'static str,
    /// Something the user must know to finish the setup.
    pub note: &'static str,
}

impl BrandTemplate {
    /// Text for a combo box, for example `Hikvision - main stream`.
    pub fn label(&self) -> String {
        format!("{} - {}", self.brand, self.stream)
    }
}

const fn t(
    brand: &'static str,
    stream: &'static str,
    port: u16,
    path: &'static str,
    note: &'static str,
) -> BrandTemplate {
    BrandTemplate {
        brand,
        stream,
        port,
        path,
        note,
    }
}

const NO_NOTE: &str = "";

/// Every template, grouped by brand, main stream first.
pub const BRAND_TEMPLATES: &[BrandTemplate] = &[
    t(
        "Hikvision",
        "main stream",
        554,
        "/Streaming/Channels/101",
        NO_NOTE,
    ),
    t(
        "Hikvision",
        "sub stream",
        554,
        "/Streaming/Channels/102",
        NO_NOTE,
    ),
    t(
        "Dahua",
        "main stream",
        554,
        "/cam/realmonitor?channel=1&subtype=0",
        NO_NOTE,
    ),
    t(
        "Dahua",
        "sub stream",
        554,
        "/cam/realmonitor?channel=1&subtype=1",
        NO_NOTE,
    ),
    t(
        "Amcrest",
        "main stream",
        554,
        "/cam/realmonitor?channel=1&subtype=0",
        NO_NOTE,
    ),
    t(
        "Amcrest",
        "sub stream",
        554,
        "/cam/realmonitor?channel=1&subtype=1",
        NO_NOTE,
    ),
    t(
        "Reolink",
        "main stream",
        554,
        "/h264Preview_01_main",
        "Reolink cameras need RTSP turned on in their settings.",
    ),
    t(
        "Reolink",
        "sub stream",
        554,
        "/h264Preview_01_sub",
        "Reolink cameras need RTSP turned on in their settings.",
    ),
    t(
        "TP-Link Tapo",
        "main stream",
        554,
        "/stream1",
        "Create a camera account in the Tapo app (Advanced settings) and use it here.",
    ),
    t(
        "TP-Link Tapo",
        "sub stream",
        554,
        "/stream2",
        "Create a camera account in the Tapo app (Advanced settings) and use it here.",
    ),
    t(
        "UniFi Protect",
        "camera alias",
        7447,
        "/",
        "Turn on RTSP for the camera in UniFi Protect, then replace the path with the alias \
         from its RTSP address (the part after the port).",
    ),
    t(
        "Axis",
        "default stream",
        554,
        "/axis-media/media.amp",
        NO_NOTE,
    ),
    t("Foscam", "main stream", 88, "/videoMain", NO_NOTE),
    t("Foscam", "sub stream", 88, "/videoSub", NO_NOTE),
];

/// The template whose port and path match, if any.
pub fn find_template(port: u16, path: &str) -> Option<&'static BrandTemplate> {
    BRAND_TEMPLATES
        .iter()
        .find(|t| t.port == port && t.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StreamConfig;

    #[test]
    fn labels_are_unique() {
        let mut labels: Vec<_> = BRAND_TEMPLATES.iter().map(BrandTemplate::label).collect();
        let n = labels.len();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), n);
    }

    #[test]
    fn every_template_makes_a_valid_stream() {
        for t in BRAND_TEMPLATES {
            let mut s = StreamConfig::new("cam", "192.168.1.10");
            s.port = t.port;
            s.path = t.path.to_owned();
            assert!(s.validate().is_empty(), "{}: {:?}", t.label(), s.validate());
            assert!(s.url().starts_with("rtsp://192.168.1.10:"));
        }
    }

    #[test]
    fn finds_by_port_and_path() {
        assert_eq!(
            find_template(554, "/Streaming/Channels/102")
                .unwrap()
                .stream,
            "sub stream"
        );
        assert!(find_template(554, "/nope").is_none());
    }
}
