//! `rtspcam_vcam.dll`: the Media Foundation custom media source behind every RTSP Cam virtual
//! camera. Windows' Frame Server service loads it; it forwards frames received from the app
//! over IPC.
//!
//! Implemented in Phase 3 (after the Phase 0 spike). Every exported function and COM method
//! must catch panics and return an `HRESULT`: a panic here would take down the Frame Server.
