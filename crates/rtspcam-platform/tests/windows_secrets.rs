//! Config passwords go through DPAPI once the platform is installed, exactly as before the
//! cross-platform split.
#![cfg(windows)]

use rtspcam_core::{Config, Secret, StreamConfig};

#[test]
fn passwords_are_stored_with_dpapi() {
    rtspcam_platform::install();
    let secret = Secret::new("hunter2");
    let stored = secret.to_stored().unwrap();
    assert!(stored.starts_with("dpapi:"), "{stored}");
    assert_eq!(Secret::from_stored(&stored).expose(), Some("hunter2"));

    let mut config = Config::default();
    let mut s = StreamConfig::new("Front Door", "192.168.1.50");
    s.username = "admin".into();
    s.password = Some(secret);
    config.streams.push(s);
    let text = serde_json::to_string_pretty(&config).unwrap();
    assert!(text.contains("\"password\": \"dpapi:") && !text.contains("hunter2"));
    let back: Config = serde_json::from_str(&text).unwrap();
    assert_eq!(back, config);
}

#[test]
fn a_value_from_another_user_is_locked_not_lost() {
    rtspcam_platform::install();
    let stored = "dpapi:AQAAANCMnd8BFdERjHoAwE/Cl+sBAAAA";
    let secret = Secret::from_stored(stored);
    assert_eq!(secret.expose(), None);
    assert_eq!(secret.to_stored().unwrap(), stored);
}
