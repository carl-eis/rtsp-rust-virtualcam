//! Passwords encrypted with Windows DPAPI (current-user scope), stored as `dpapi:<base64>`.
//! Only the same Windows user on the same machine can decrypt them.

use std::io;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rtspcam_core::SecretError;
use rtspcam_core::secret::{DPAPI_PREFIX, SecretStore};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;
use zeroize::Zeroizing;

/// Extra entropy mixed into every DPAPI call, so other apps running as the same user can't
/// decrypt our blobs by accident.
const ENTROPY: &[u8] = b"RtspCam.secret.v1";

/// The Windows [`SecretStore`].
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DpapiStore;

impl SecretStore for DpapiStore {
    fn seal(&self, plain: &str) -> Result<String, SecretError> {
        let blob = protect(plain.as_bytes())?;
        Ok(format!("{DPAPI_PREFIX}{}", BASE64.encode(blob)))
    }

    fn unseal(&self, stored: &str) -> Result<Zeroizing<String>, SecretError> {
        let encoded = stored.strip_prefix(DPAPI_PREFIX).ok_or_else(|| {
            SecretError::Unreadable(
                "the password was saved on another operating system; enter it again".to_owned(),
            )
        })?;
        unprotect(&BASE64.decode(encoded.trim())?)
    }
}

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

fn protect(plain: &[u8]) -> Result<Vec<u8>, SecretError> {
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

fn unprotect(encrypted: &[u8]) -> Result<Zeroizing<String>, SecretError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let stored = DpapiStore.seal("hunter2 – ünïcödé").unwrap();
        assert!(stored.starts_with(DPAPI_PREFIX));
        assert!(!stored.contains("hunter2"));
        assert_eq!(
            DpapiStore.unseal(&stored).unwrap().as_str(),
            "hunter2 – ünïcödé"
        );
    }

    #[test]
    fn encryption_is_randomized() {
        assert_ne!(
            DpapiStore.seal("same").unwrap(),
            DpapiStore.seal("same").unwrap()
        );
    }

    #[test]
    fn garbage_and_foreign_values_are_errors() {
        assert!(DpapiStore.unseal("dpapi:AAAAAAAA").is_err());
        assert!(DpapiStore.unseal("dpapi:not base64!").is_err());
        let err = DpapiStore.unseal("keyring:AAAA").unwrap_err();
        assert!(err.to_string().contains("enter it again"), "{err}");
    }
}
