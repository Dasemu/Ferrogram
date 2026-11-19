//! Encrypted storage for large session blobs (the MTProto auth key and
//! update state). The random 256-bit key lives in the keyring; only the
//! ChaCha20-Poly1305 ciphertext is written to the data directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

use super::secrets::{Secret, SecretStore};
use crate::error::{Error, Result};

/// Keyring account name holding the vault key.
pub const VAULT_KEY: &str = "session-key";
const MAGIC: &[u8; 4] = b"FGV1";

#[derive(Clone)]
pub struct Vault {
    secrets: Arc<dyn SecretStore>,
    path: PathBuf,
}

impl Vault {
    pub fn new(secrets: Arc<dyn SecretStore>, path: impl Into<PathBuf>) -> Self {
        Self {
            secrets,
            path: path.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn key(&self, create: bool) -> Result<Option<Key>> {
        if let Some(k) = self.secrets.get(VAULT_KEY)?
            && k.expose().len() == 32
        {
            return Ok(Some(*Key::from_slice(k.expose())));
        }
        if !create {
            return Ok(None);
        }
        let key = ChaCha20Poly1305::generate_key(&mut OsRng);
        self.secrets.set(VAULT_KEY, &Secret::new(key.to_vec()))?;
        Ok(Some(key))
    }

    /// Encrypt and atomically write `plain`.
    pub fn save(&self, plain: &[u8]) -> Result<()> {
        let key = self
            .key(true)?
            .ok_or_else(|| Error::Keyring("no key".into()))?;
        let cipher = ChaCha20Poly1305::new(&key);
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(&nonce, plain)
            .map_err(|_| Error::Protocol("encrypt failed".into()))?;
        let mut out = Vec::with_capacity(4 + 12 + ct.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        if let Some(dir) = self.path.parent() {
            crate::config::ensure_private_dir(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        {
            use std::io::Write;
            // Created 0600 from the start: no window where it is world-readable.
            let mut f = crate::config::create_private_file(&tmp)?;
            f.write_all(&out)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Load and decrypt. `Ok(None)` when there is no blob or no key.
    pub fn load(&self) -> Result<Option<Secret>> {
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let Some(key) = self.key(false)? else {
            return Ok(None);
        };
        if data.len() < 16 || &data[..4] != MAGIC {
            return Err(Error::Protocol("vault: bad header".into()));
        }
        let nonce = Nonce::from_slice(&data[4..16]);
        let plain = ChaCha20Poly1305::new(&key)
            .decrypt(nonce, &data[16..])
            .map_err(|_| Error::Protocol("vault: decrypt failed".into()))?;
        Ok(Some(Secret::new(plain)))
    }

    /// Remove both the ciphertext and the key.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.secrets.delete(VAULT_KEY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::secrets::MemoryStore;

    #[test]
    fn roundtrip_and_ciphertext_is_not_plain() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let v = Vault::new(store.clone(), dir.path().join("s.enc"));
        assert!(v.load().unwrap().is_none());
        v.save(b"AUTH-KEY-BYTES-0123456789").unwrap();
        let raw = std::fs::read(v.path()).unwrap();
        assert!(!raw.windows(8).any(|w| w == b"AUTH-KEY"));
        assert_eq!(
            v.load().unwrap().unwrap().expose(),
            b"AUTH-KEY-BYTES-0123456789"
        );
        assert_eq!(store.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(v.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        v.clear().unwrap();
        assert!(v.load().unwrap().is_none());
        assert!(store.is_empty());
    }

    #[test]
    fn wrong_key_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.enc");
        Vault::new(Arc::new(MemoryStore::new()), &path)
            .save(b"x")
            .unwrap();
        let other = Arc::new(MemoryStore::new());
        other.set(VAULT_KEY, &Secret::new(vec![7u8; 32])).unwrap();
        assert!(Vault::new(other, &path).load().is_err());
    }
}
