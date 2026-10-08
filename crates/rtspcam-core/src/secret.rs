//! Strings protected with Windows DPAPI (current-user scope).
//!
//! In `config.json` a secret is stored as `"dpapi:<base64>"`. Only the same Windows user on the
//! same machine can decrypt it. A value without the prefix is treated as plain text (for
//! hand-edited files) and is encrypted the next time the config is saved.
//!
//! A [`Secret`] never prints its contents: `Debug` is redacted, so it is safe to log a
//! [`StreamConfig`](crate::StreamConfig).

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

use crate::error::SecretError;

/// Prefix that marks a DPAPI-encrypted value in the config file.
pub const DPAPI_PREFIX: &str = "dpapi:";

/// Extra entropy mixed into every DPAPI call, so other apps running as the same user can't
/// decrypt our blobs by accident.
const ENTROPY: &[u8] = b"RtspCam.secret.v1";

/// A password or other credential.
#[derive(Clone)]
pub struct Secret(Inner);

#[derive(Clone)]
enum Inner {
    /// Decrypted value, wiped from memory on drop.
    Plain(Zeroizing<String>),
    /// A stored value that could not be decrypted (for example a config copied from another
    /// user or machine). It is written back unchanged so nothing is lost.
    Locked { stored: String, reason: String },
}

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Inner::Plain(Zeroizing::new(value.into())))
    }

    /// The plain-text value, or `None` if the stored value could not be decrypted.
    pub fn expose(&self) -> Option<&str> {
        match &self.0 {
            Inner::Plain(s) => Some(s),
            Inner::Locked { .. } => None,
        }
    }

    /// Why the stored value could not be decrypted, if it couldn't.
    pub fn locked_reason(&self) -> Option<&str> {
        match &self.0 {
            Inner::Plain(_) => None,
            Inner::Locked { reason, .. } => Some(reason),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.expose().is_some_and(str::is_empty)
    }

    /// Parses a value as stored in the config file. Never fails: an undecryptable value becomes
    /// a locked secret (see [`Secret::locked_reason`]).
    pub fn from_stored(stored: &str) -> Self {
        let Some(encoded) = stored.strip_prefix(DPAPI_PREFIX) else {
            return Self::new(stored);
        };
        match BASE64
            .decode(encoded.trim())
            .map_err(SecretError::from)
            .and_then(|blob| dpapi::unprotect(&blob))
        {
            Ok(plain) => Self(Inner::Plain(plain)),
            Err(err) => {
                tracing::warn!(error = %err, "could not decrypt a stored secret");
                Self(Inner::Locked {
                    stored: stored.to_owned(),
                    reason: err.to_string(),
                })
            }
        }
    }

    /// The value to write to the config file: `dpapi:<base64>`.
    pub fn to_stored(&self) -> Result<String, SecretError> {
        match &self.0 {
            Inner::Plain(plain) => {
                let blob = dpapi::protect(plain.as_bytes())?;
                Ok(format!("{DPAPI_PREFIX}{}", BASE64.encode(blob)))
            }
            Inner::Locked { stored, .. } => Ok(stored.clone()),
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Inner::Plain(_) => f.write_str("Secret(<redacted>)"),
            Inner::Locked { .. } => f.write_str("Secret(<locked>)"),
        }
    }
}

/// Compares the decrypted values. DPAPI output is randomized, so the stored form can't be
/// compared.
impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Inner::Plain(a), Inner::Plain(b)) => a == b,
            (Inner::Locked { stored: a, .. }, Inner::Locked { stored: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Eq for Secret {}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let stored = self.to_stored().map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&stored)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = Zeroizing::new(String::deserialize(deserializer)?);
        Ok(Self::from_stored(&stored))
    }
}

#[cfg(windows)]
mod dpapi {
    use std::io;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;
    use zeroize::Zeroizing;

    use super::ENTROPY;
    use crate::error::SecretError;

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr().cast_mut(),
        }
    }

    /// Copies a DPAPI output blob into a `Vec` and frees the original with `LocalFree`.
    ///
    /// # Safety
    /// `out` must have been filled in by a successful `CryptProtectData`/`CryptUnprotectData`.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: on success DPAPI hands back a valid buffer of `cbData` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        // SAFETY: the buffer was allocated by DPAPI with LocalAlloc and is not used again.
        unsafe { LocalFree(Some(HLOCAL(out.pbData.cast()))) };
        bytes
    }

    fn dpapi_err(op: &'static str, err: windows::core::Error) -> SecretError {
        SecretError::Dpapi {
            op,
            source: io::Error::from_raw_os_error(err.code().0),
        }
    }

    pub(super) fn protect(plain: &[u8]) -> Result<Vec<u8>, SecretError> {
        let input = blob(plain);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: all pointers are valid for the duration of the call; `out` is freed in `take`.
        unsafe {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| dpapi_err("encrypt", e))?;
            Ok(take(out))
        }
    }

    pub(super) fn unprotect(encrypted: &[u8]) -> Result<Zeroizing<String>, SecretError> {
        let input = blob(encrypted);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as in `protect`.
        let plain = Zeroizing::new(unsafe {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
            .map_err(|e| dpapi_err("decrypt", e))?;
            take(out)
        });
        let text = std::str::from_utf8(&plain).map_err(|_| SecretError::Utf8)?;
        Ok(Zeroizing::new(text.to_owned()))
    }
}

#[cfg(not(windows))]
mod dpapi {
    use zeroize::Zeroizing;

    use crate::error::SecretError;

    pub(super) fn protect(_plain: &[u8]) -> Result<Vec<u8>, SecretError> {
        Err(SecretError::Unsupported)
    }

    pub(super) fn unprotect(_encrypted: &[u8]) -> Result<Zeroizing<String>, SecretError> {
        Err(SecretError::Unsupported)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip() {
        let secret = Secret::new("hunter2 – ünïcödé");
        let stored = secret.to_stored().unwrap();
        assert!(stored.starts_with(DPAPI_PREFIX));
        assert!(!stored.contains("hunter2"));

        let back = Secret::from_stored(&stored);
        assert_eq!(back.expose(), Some("hunter2 – ünïcödé"));
        assert_eq!(back, secret);
    }

    #[test]
    fn encryption_is_randomized() {
        let secret = Secret::new("same");
        assert_ne!(secret.to_stored().unwrap(), secret.to_stored().unwrap());
    }

    #[test]
    fn plain_text_is_accepted_and_encrypted_on_save() {
        let secret = Secret::from_stored("typed-by-hand");
        assert_eq!(secret.expose(), Some("typed-by-hand"));
        assert!(secret.to_stored().unwrap().starts_with(DPAPI_PREFIX));
    }

    #[test]
    fn garbage_becomes_locked_and_is_preserved() {
        let stored = "dpapi:AAAAAAAA";
        let secret = Secret::from_stored(stored);
        assert_eq!(secret.expose(), None);
        assert!(secret.locked_reason().is_some());
        assert_eq!(secret.to_stored().unwrap(), stored);
    }

    #[test]
    fn debug_is_redacted() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    }

    #[test]
    fn serde_round_trip() {
        let json = serde_json::to_string(&Secret::new("pw")).unwrap();
        assert!(json.starts_with("\"dpapi:"));
        let back: Secret = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expose(), Some("pw"));
    }
}
