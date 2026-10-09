//! Passwords for config.json on Linux and macOS.
//!
//! One random 256-bit key encrypts every password (ChaCha20-Poly1305, a fresh nonce each time).
//! The key lives in the OS keyring: Secret Service (GNOME Keyring, KWallet, ...) on Linux, the
//! login Keychain on macOS, as the item service `RtspCam`, account `config-key`. Values are
//! stored as `keyring:<base64(nonce + ciphertext)>`, so config.json stays self-contained like
//! the `dpapi:` values on Windows, and deleting a stream leaves nothing behind in the keyring.
//!
//! Without a reachable keyring (a minimal or headless Linux), the key goes to `secret.key`
//! next to config.json, readable only by the user, and values are stored as `keyfile:...`. That
//! keeps passwords out of exports and copies of the config, but not from other programs running
//! as the same user; a warning is logged when it happens.
//!
//! Anything that can't be decrypted (a `dpapi:` value from Windows, a lost key) is an error,
//! which the config turns into a "locked" password the user is asked to enter again.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore as _;
use rtspcam_core::SecretError;
use rtspcam_core::secret::{DPAPI_PREFIX, KEYFILE_PREFIX, KEYRING_PREFIX, SecretStore};
use zeroize::Zeroizing;

/// Mixed into every encryption, so a blob can't be reused for another purpose.
const AAD: &[u8] = b"RtspCam.secret.v1";
const NONCE_LEN: usize = 12;

type SecretKey = Zeroizing<[u8; 32]>;

/// Where a key is kept.
pub(crate) trait KeySource: Send + Sync {
    /// The stored-value prefix for secrets encrypted with this key.
    fn prefix(&self) -> &'static str;

    /// The key, or `None` if there is none yet. An error means the place can't be reached.
    fn load(&self) -> Result<Option<SecretKey>, String>;

    fn save(&self, key: &SecretKey) -> Result<(), String>;
}

/// The key in the OS keyring, through `keyring-core`.
pub(crate) struct Keyring {
    /// Opens the OS's credential store (it differs per OS).
    open: fn() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>>,
}

impl Keyring {
    const SERVICE: &'static str = "RtspCam";
    const ACCOUNT: &'static str = "config-key";

    pub(crate) fn new(
        open: fn() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>>,
    ) -> Self {
        Self { open }
    }

    fn entry(&self) -> Result<keyring_core::Entry, String> {
        let store = (self.open)().map_err(|e| format!("the keyring is not available: {e}"))?;
        store
            .build(Self::SERVICE, Self::ACCOUNT, None)
            .map_err(|e| format!("the keyring is not available: {e}"))
    }
}

impl KeySource for Keyring {
    fn prefix(&self) -> &'static str {
        KEYRING_PREFIX
    }

    fn load(&self) -> Result<Option<SecretKey>, String> {
        match self.entry()?.get_secret() {
            Ok(bytes) => to_key(&bytes).map(Some),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("could not read the key from the keyring: {e}")),
        }
    }

    fn save(&self, key: &SecretKey) -> Result<(), String> {
        self.entry()?
            .set_secret(key.as_slice())
            .map_err(|e| format!("could not store the key in the keyring: {e}"))
    }
}

/// The key in a file only the user can read.
pub(crate) struct KeyFile {
    pub(crate) path: PathBuf,
}

impl KeySource for KeyFile {
    fn prefix(&self) -> &'static str {
        KEYFILE_PREFIX
    }

    fn load(&self) -> Result<Option<SecretKey>, String> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                to_key(&bytes).map(Some)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("could not read {}: {e}", self.path.display())),
        }
    }

    fn save(&self, key: &SecretKey) -> Result<(), String> {
        let write = || -> io::Result<()> {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&self.path)?;
            file.write_all(key.as_slice())?;
            file.sync_all()
        };
        write().map_err(|e| format!("could not write {}: {e}", self.path.display()))
    }
}

fn to_key(bytes: &[u8]) -> Result<SecretKey, String> {
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "the stored key is damaged".to_owned())?;
    Ok(Zeroizing::new(key))
}

/// The Linux and macOS [`SecretStore`]: the keyring first, the key file as a fallback.
pub(crate) struct SealedStore {
    keyring: Box<dyn KeySource>,
    file: Box<dyn KeySource>,
    /// Keys already loaded, by prefix, so the keyring is asked once per run.
    cache: Mutex<HashMap<&'static str, SecretKey>>,
}

impl SealedStore {
    pub(crate) fn new(keyring: Box<dyn KeySource>, file: Box<dyn KeySource>) -> Self {
        Self {
            keyring,
            file,
            cache: Mutex::default(),
        }
    }

    fn cached(&self, prefix: &'static str) -> Option<SecretKey> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(prefix)
            .cloned()
    }

    fn remember(&self, prefix: &'static str, key: &SecretKey) {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(prefix, key.clone());
    }

    /// The key for `source`, made (and saved) if there is none yet.
    fn key_for_writing(&self, source: &dyn KeySource) -> Result<SecretKey, String> {
        if let Some(key) = self.cached(source.prefix()) {
            return Ok(key);
        }
        let key = match source.load()? {
            Some(key) => key,
            None => {
                let mut key = Zeroizing::new([0u8; 32]);
                rand::rng().fill_bytes(key.as_mut_slice());
                source.save(&key)?;
                key
            }
        };
        self.remember(source.prefix(), &key);
        Ok(key)
    }

    fn key_for_reading(&self, source: &dyn KeySource) -> Result<SecretKey, SecretError> {
        if let Some(key) = self.cached(source.prefix()) {
            return Ok(key);
        }
        match source.load() {
            Ok(Some(key)) => {
                self.remember(source.prefix(), &key);
                Ok(key)
            }
            Ok(None) => Err(SecretError::Unreadable(
                "the key for saved passwords is missing (was the config copied from another \
                 computer?); enter the password again"
                    .to_owned(),
            )),
            Err(e) => Err(SecretError::Unreadable(format!(
                "{e}; enter the password again"
            ))),
        }
    }
}

impl SecretStore for SealedStore {
    fn seal(&self, plain: &str) -> Result<String, SecretError> {
        let (prefix, key) = match self.key_for_writing(&*self.keyring) {
            Ok(key) => (self.keyring.prefix(), key),
            Err(why) => {
                tracing::warn!(
                    reason = %why,
                    "no keyring for the password key; keeping it in a file only you can read"
                );
                let key = self
                    .key_for_writing(&*self.file)
                    .map_err(SecretError::Unreadable)?;
                (self.file.prefix(), key)
            }
        };
        let mut nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_slice()));
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain.as_bytes(),
                    aad: AAD,
                },
            )
            .map_err(|_| SecretError::Unreadable("encryption failed".to_owned()))?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&sealed);
        Ok(format!("{prefix}{}", BASE64.encode(blob)))
    }

    fn unseal(&self, stored: &str) -> Result<Zeroizing<String>, SecretError> {
        let (source, encoded) = if let Some(rest) = stored.strip_prefix(self.keyring.prefix()) {
            (&*self.keyring, rest)
        } else if let Some(rest) = stored.strip_prefix(self.file.prefix()) {
            (&*self.file, rest)
        } else if stored.starts_with(DPAPI_PREFIX) {
            return Err(SecretError::Unreadable(
                "the password was saved by RTSP Cam on Windows; enter it again".to_owned(),
            ));
        } else {
            return Err(SecretError::Unreadable(
                "the saved password is in an unknown format; enter it again".to_owned(),
            ));
        };
        let blob = BASE64.decode(encoded.trim())?;
        if blob.len() < NONCE_LEN {
            return Err(SecretError::Unreadable(
                "the saved password is damaged; enter it again".to_owned(),
            ));
        }
        let key = self.key_for_reading(source)?;
        let (nonce, sealed) = blob.split_at(NONCE_LEN);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_slice()));
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(nonce),
                    Payload {
                        msg: sealed,
                        aad: AAD,
                    },
                )
                .map_err(|_| {
                    SecretError::Unreadable(
                        "the saved password doesn't match this computer's key; enter it again"
                            .to_owned(),
                    )
                })?,
        );
        let text = std::str::from_utf8(&plain).map_err(|_| SecretError::Utf8)?;
        Ok(Zeroizing::new(text.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// An in-memory keyring that can be switched off.
    #[derive(Default)]
    struct FakeKeyring {
        key: Mutex<Option<SecretKey>>,
        down: bool,
        loads: Arc<AtomicUsize>,
    }

    impl KeySource for FakeKeyring {
        fn prefix(&self) -> &'static str {
            KEYRING_PREFIX
        }

        fn load(&self) -> Result<Option<SecretKey>, String> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if self.down {
                return Err("the keyring is not available".to_owned());
            }
            Ok(self.key.lock().unwrap().clone())
        }

        fn save(&self, key: &SecretKey) -> Result<(), String> {
            if self.down {
                return Err("the keyring is not available".to_owned());
            }
            *self.key.lock().unwrap() = Some(key.clone());
            Ok(())
        }
    }

    fn store(keyring: FakeKeyring, dir: &tempfile::TempDir) -> SealedStore {
        SealedStore::new(
            Box::new(keyring),
            Box::new(KeyFile {
                path: dir.path().join("secret.key"),
            }),
        )
    }

    #[test]
    fn round_trip_through_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let loads = Arc::new(AtomicUsize::new(0));
        let s = store(
            FakeKeyring {
                loads: loads.clone(),
                ..FakeKeyring::default()
            },
            &dir,
        );
        let stored = s.seal("hunter2 – ünïcödé").unwrap();
        assert!(stored.starts_with(KEYRING_PREFIX), "{stored}");
        assert!(!stored.contains("hunter2"));
        assert_eq!(s.unseal(&stored).unwrap().as_str(), "hunter2 – ünïcödé");
        // Randomized, and the keyring is asked once.
        assert_ne!(s.seal("same").unwrap(), s.seal("same").unwrap());
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert!(!dir.path().join("secret.key").exists());
    }

    #[test]
    fn without_a_keyring_the_key_goes_to_a_private_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let s = store(
            FakeKeyring {
                down: true,
                ..FakeKeyring::default()
            },
            &dir,
        );
        let stored = s.seal("pw").unwrap();
        assert!(stored.starts_with(KEYFILE_PREFIX), "{stored}");
        let mode = fs::metadata(dir.path().join("secret.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        // A fresh process (no cache) reads it back from the file.
        let again = store(
            FakeKeyring {
                down: true,
                ..FakeKeyring::default()
            },
            &dir,
        );
        assert_eq!(again.unseal(&stored).unwrap().as_str(), "pw");
    }

    #[test]
    fn unreadable_values_are_errors_that_ask_for_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(FakeKeyring::default(), &dir);
        for stored in [
            "dpapi:AQAAANCMnd8BFdERjHoAwE/Cl+sBAAAA",
            "keyring:!!!",
            "keyring:AAAA",
        ] {
            let err = s.unseal(stored).unwrap_err().to_string();
            assert!(
                err.contains("enter") || err.contains("base64"),
                "{stored}: {err}"
            );
        }
        // Encrypted with a key this computer doesn't have.
        let other_dir = tempfile::tempdir().unwrap();
        let other = store(FakeKeyring::default(), &other_dir);
        let foreign = other.seal("pw").unwrap();
        let err = s.unseal(&foreign).unwrap_err().to_string();
        assert!(err.contains("enter the password again"), "{err}");
    }
}
