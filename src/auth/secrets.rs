//! The ONLY module allowed to call the `keyring` crate (architecture §5).

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretBox};

use crate::error::{Error, Result};

pub const SERVICE: &str = crate::config::APP_ID;

/// A secret value, zeroised on drop, `Debug` prints `[REDACTED]`.
pub struct Secret(SecretBox<Vec<u8>>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(SecretBox::new(Box::new(bytes)))
    }

    pub fn expose(&self) -> &[u8] {
        self.0.expose_secret()
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self::new(self.expose().to_vec())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Secret>>;
    fn set(&self, key: &str, value: &Secret) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

/// OS keyring (Secret Service on Linux, Keychain, Credential Manager).
pub struct KeyringStore;

impl KeyringStore {
    fn entry(key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, key).map_err(map_keyring)
    }

    /// Whether a default credential store could be initialised.
    pub fn available() -> bool {
        keyring::Entry::store_status().is_ok()
    }
}

fn map_keyring(e: keyring::Error) -> Error {
    // Platform errors can contain object paths but never secret values.
    Error::Keyring(match e {
        keyring::Error::NoStorageAccess(_) => "no storage access".into(),
        keyring::Error::PlatformFailure(_) => "platform failure".into(),
        keyring::Error::NoDefaultStore => "no default store".into(),
        other => format!("{other:?}").chars().take(60).collect(),
    })
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        match Self::entry(key)?.get_secret() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_keyring(e)),
        }
    }

    fn set(&self, key: &str, value: &Secret) -> Result<()> {
        Self::entry(key)?
            .set_secret(value.expose())
            .map_err(map_keyring)
    }

    fn delete(&self, key: &str) -> Result<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_keyring(e)),
        }
    }
}

/// In-memory store for tests and `--mock` mode.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, Vec<u8>>>);

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn map(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Vec<u8>>>> {
        self.0
            .lock()
            .map_err(|_| Error::Keyring("memory store poisoned".into()))
    }

    pub fn len(&self) -> usize {
        self.map().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        Ok(self.map()?.get(key).cloned().map(Secret::new))
    }

    fn set(&self, key: &str, value: &Secret) -> Result<()> {
        self.map()?.insert(key.to_owned(), value.expose().to_vec());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.map()?.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::new();
        assert!(s.get("k").unwrap().is_none());
        s.set("k", &Secret::new(b"v".to_vec())).unwrap();
        assert_eq!(s.get("k").unwrap().unwrap().expose(), b"v");
        s.delete("k").unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new(b"hunter2".to_vec());
        assert_eq!(format!("{s:?}"), "[REDACTED]");
    }
}
