//! Encrypted strings: the stream passwords in `config.json`.
//!
//! In the file a secret is stored encrypted, behind a prefix that says how:
//!
//! - `dpapi:<base64>`: Windows DPAPI, current user. Only the same Windows user on the same
//!   machine can decrypt it.
//! - `keyring:<base64>` and `keyfile:<base64>`: Linux and macOS, encrypted with a key kept in
//!   the OS keyring (or, without one, in a file only the user can read).
//!
//! This crate has no OS code: the encryption is done by the [`SecretStore`] that the program
//! installs at startup with [`install_store`] (`rtspcam_platform::install` does it). A value
//! without one of the [`SEALED_PREFIXES`] is plain text (for hand-edited files) and is encrypted
//! the next time the config is saved.
//!
//! A value that can't be decrypted (another user or machine, a `dpapi:` value on Linux, a lost
//! keyring key) never fails the load: it becomes a *locked* secret, which is written back
//! unchanged and reported by validation so the user can enter the password again.
//!
//! A [`Secret`] never prints its contents: `Debug` is redacted, so it is safe to log a
//! [`StreamConfig`](crate::StreamConfig).

use std::fmt;
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

use crate::error::SecretError;

/// Prefix of a value encrypted with Windows DPAPI.
pub const DPAPI_PREFIX: &str = "dpapi:";
/// Prefix of a value encrypted with a key kept in the Linux or macOS keyring.
pub const KEYRING_PREFIX: &str = "keyring:";
/// Prefix of a value encrypted with a key kept in a file (no keyring was available).
pub const KEYFILE_PREFIX: &str = "keyfile:";

/// Every prefix that marks an encrypted value. Anything else is plain text.
pub const SEALED_PREFIXES: [&str; 3] = [DPAPI_PREFIX, KEYRING_PREFIX, KEYFILE_PREFIX];

/// Encrypts and decrypts stored secrets. One is installed per process with
/// [`install_store`]; the implementations live in `rtspcam-platform`.
pub trait SecretStore: Send + Sync + 'static {
    /// Encrypts `plain` and returns the value to store, including its prefix.
    fn seal(&self, plain: &str) -> Result<String, SecretError>;

    /// Decrypts a stored value that starts with one of [`SEALED_PREFIXES`]. A prefix this store
    /// doesn't handle (a `dpapi:` value on Linux, say) is an error, not a panic.
    fn unseal(&self, stored: &str) -> Result<Zeroizing<String>, SecretError>;
}

static STORE: OnceLock<Box<dyn SecretStore>> = OnceLock::new();

/// Sets the store used to encrypt and decrypt every [`Secret`] in this process. Call it once at
/// startup, before loading the config. Returns `false` (and keeps the first store) if one was
/// already installed.
pub fn install_store(store: Box<dyn SecretStore>) -> bool {
    STORE.set(store).is_ok()
}

fn installed() -> Option<&'static dyn SecretStore> {
    STORE.get().map(Box::as_ref)
}

/// Whether `stored` is an encrypted value (as opposed to plain text typed into the file).
pub fn is_sealed(stored: &str) -> bool {
    SEALED_PREFIXES.iter().any(|p| stored.starts_with(p))
}

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
        Self::from_stored_with(stored, installed())
    }

    fn from_stored_with(stored: &str, store: Option<&dyn SecretStore>) -> Self {
        if !is_sealed(stored) {
            return Self::new(stored);
        }
        match store
            .ok_or(SecretError::NoStore)
            .and_then(|s| s.unseal(stored))
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

    /// The value to write to the config file, for example `dpapi:<base64>`.
    pub fn to_stored(&self) -> Result<String, SecretError> {
        self.to_stored_with(installed())
    }

    fn to_stored_with(&self, store: Option<&dyn SecretStore>) -> Result<String, SecretError> {
        match &self.0 {
            Inner::Plain(plain) => store.ok_or(SecretError::NoStore)?.seal(plain),
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

/// Compares the decrypted values. Encryption is randomized, so the stored form can't be
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

/// A stand-in store for this crate's tests: reversible, randomized, no OS involved.
#[cfg(test)]
pub(crate) mod test_store {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use zeroize::Zeroizing;

    use super::{KEYFILE_PREFIX, SecretStore};
    use crate::error::SecretError;

    /// Stores `keyfile:<base64(salt + reversed text)>`.
    pub(crate) struct Reversing;

    impl SecretStore for Reversing {
        fn seal(&self, plain: &str) -> Result<String, SecretError> {
            let mut bytes = vec![rand::random::<u8>()];
            bytes.extend(plain.bytes().rev());
            Ok(format!("{KEYFILE_PREFIX}{}", BASE64.encode(bytes)))
        }

        fn unseal(&self, stored: &str) -> Result<Zeroizing<String>, SecretError> {
            let encoded = stored
                .strip_prefix(KEYFILE_PREFIX)
                .ok_or_else(|| SecretError::Unreadable("not a test value".into()))?;
            let bytes = BASE64.decode(encoded)?;
            let text: Vec<u8> = bytes.iter().skip(1).rev().copied().collect();
            Ok(Zeroizing::new(
                String::from_utf8(text).map_err(|_| SecretError::Utf8)?,
            ))
        }
    }

    /// Installs [`Reversing`] for the whole test binary (the first call wins).
    pub(crate) fn install() {
        super::install_store(Box::new(Reversing));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        test_store::install();
        let secret = Secret::new("hunter2 – ünïcödé");
        let stored = secret.to_stored().unwrap();
        assert!(is_sealed(&stored));
        assert!(!stored.contains("hunter2"));

        let back = Secret::from_stored(&stored);
        assert_eq!(back.expose(), Some("hunter2 – ünïcödé"));
        assert_eq!(back, secret);
    }

    #[test]
    fn plain_text_is_accepted_and_encrypted_on_save() {
        test_store::install();
        let secret = Secret::from_stored("typed-by-hand");
        assert_eq!(secret.expose(), Some("typed-by-hand"));
        assert!(is_sealed(&secret.to_stored().unwrap()));
    }

    #[test]
    fn garbage_becomes_locked_and_is_preserved() {
        test_store::install();
        let stored = "keyfile:!!!not base64";
        let secret = Secret::from_stored(stored);
        assert_eq!(secret.expose(), None);
        assert!(secret.locked_reason().is_some());
        assert_eq!(secret.to_stored().unwrap(), stored);
    }

    #[test]
    fn other_platforms_values_are_locked_not_plain() {
        test_store::install();
        // A DPAPI value read where only the test store exists: kept, not used as a password.
        let secret = Secret::from_stored("dpapi:AQAAANCMnd8BFdERjHoAwE/Cl+sBAAAA");
        assert_eq!(secret.expose(), None);
        assert_eq!(
            secret.to_stored().unwrap(),
            "dpapi:AQAAANCMnd8BFdERjHoAwE/Cl+sBAAAA"
        );
    }

    #[test]
    fn without_a_store_secrets_lock_and_saving_fails_clearly() {
        let secret = Secret::from_stored_with("keyring:AAAA", None);
        assert!(secret.locked_reason().unwrap().contains("no secret store"));
        let err = Secret::new("pw").to_stored_with(None).unwrap_err();
        assert!(matches!(err, SecretError::NoStore), "{err}");
        // Plain text needs no store to read.
        assert_eq!(
            Secret::from_stored_with("typed", None).expose(),
            Some("typed")
        );
    }

    #[test]
    fn debug_is_redacted() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    }

    #[test]
    fn serde_round_trip() {
        test_store::install();
        let json = serde_json::to_string(&Secret::new("pw")).unwrap();
        assert!(json.starts_with("\"keyfile:"));
        let back: Secret = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expose(), Some("pw"));
    }
}
