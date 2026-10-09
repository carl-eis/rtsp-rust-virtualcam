//! Helpers for the "Find cameras" dialog. The ONVIF work itself is in `rtspcam-onvif`.

use url::Url;

/// How long a network scan listens for answers.
pub const SCAN_FOR: std::time::Duration = std::time::Duration::from_secs(4);

/// The ONVIF device service address for what the user typed: a full URL, or `host[:port]`.
pub fn device_url(typed: &str) -> Option<Url> {
    let text = typed.trim();
    if text.is_empty() {
        return None;
    }
    let full = if text.contains("://") {
        text.to_owned()
    } else {
        format!("http://{text}/onvif/device_service")
    };
    Url::parse(&full).ok()
}

/// "Found 2 cameras. Pick one." and the like.
pub fn found_text(count: usize) -> String {
    if count == 0 {
        "No cameras answered. Type the camera's address below, or check that ONVIF is on and \
         that the firewall allows RTSP Cam."
            .to_owned()
    } else {
        format!(
            "Found {count} camera{}. Pick one.",
            if count == 1 { "" } else { "s" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_become_device_service_urls() {
        assert_eq!(
            device_url(" 192.168.1.50 ").unwrap().as_str(),
            "http://192.168.1.50/onvif/device_service"
        );
        assert_eq!(
            device_url("cam.local:2020").unwrap().as_str(),
            "http://cam.local:2020/onvif/device_service"
        );
        assert_eq!(
            device_url("http://10.0.0.2:8000/onvif/device_service")
                .unwrap()
                .port(),
            Some(8000)
        );
        assert!(device_url("  ").is_none());
    }

    #[test]
    fn found_text_counts() {
        assert_eq!(found_text(1), "Found 1 camera. Pick one.");
        assert!(found_text(0).starts_with("No cameras"));
    }
}
