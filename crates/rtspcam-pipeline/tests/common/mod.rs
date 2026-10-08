//! Helpers for the integration tests that run against the `tools/test-rtsp` server.
//!
//! They only run when `RTSPCAM_TEST_SERVER` is set (for example `127.0.0.1:8554`), so a plain
//! `cargo test` without the server stays green:
//!
//! ```powershell
//! ./tools/test-rtsp/start.ps1 -Docker -Detach
//! $env:RTSPCAM_TEST_SERVER = '127.0.0.1:8554'; cargo test -p rtspcam-pipeline
//! ```

#![allow(dead_code, unreachable_pub)]

pub const TEST_USER: &str = "rtspcam";
pub const TEST_PASSWORD: &str = "rtspcam-test";

/// `host:port` of the test server, or `None` (test skipped) if it isn't configured.
pub fn server() -> Option<String> {
    match std::env::var("RTSPCAM_TEST_SERVER") {
        Ok(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => {
            eprintln!("RTSPCAM_TEST_SERVER not set; skipping");
            None
        }
    }
}

pub fn url(server: &str, path: &str) -> String {
    format!("rtsp://{server}/{path}")
}

pub fn secure_url(server: &str) -> String {
    format!("rtsp://{TEST_USER}:{TEST_PASSWORD}@{server}/secure")
}
