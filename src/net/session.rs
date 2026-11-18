//! grammers `Session` implementation whose persistent form is an encrypted
//! blob ([`crate::auth::vault::Vault`]); the vault key lives in the keyring.
//! The MTProto auth key is therefore never written to disk in plain text.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use grammers_session::types::{
    ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState,
};
use grammers_session::{BoxFuture, Session, SessionData};
use serde::{Deserialize, Serialize};

use crate::auth::vault::Vault;
use crate::error::{Error, Result};

#[derive(Debug)]
pub struct SessionError;

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session lock poisoned")
    }
}

impl std::error::Error for SessionError {}

/// Serialisable mirror of [`SessionData`].
#[derive(Serialize, Deserialize)]
struct Persisted {
    home_dc: i32,
    dc_options: Vec<DcOption>,
    peers: Vec<PeerInfo>,
    updates: UpdatesState,
}

pub struct EncryptedSession {
    data: Mutex<SessionData>,
    vault: Vault,
    dirty: AtomicBool,
}

impl EncryptedSession {
    /// Load from the vault, or start a fresh session.
    pub fn load(vault: Vault) -> Result<Self> {
        let data = match vault.load()? {
            Some(plain) => from_bytes(plain.expose())?,
            None => SessionData::default(),
        };
        Ok(Self {
            data: Mutex::new(data),
            vault,
            dirty: AtomicBool::new(false),
        })
    }

    fn data(&self) -> std::result::Result<MutexGuard<'_, SessionData>, SessionError> {
        self.data.lock().map_err(|_| SessionError)
    }

    /// Encrypt and write the session now (blocking: call from a blocking thread).
    pub fn save(&self) -> Result<()> {
        let bytes = {
            let d = self
                .data
                .lock()
                .map_err(|_| Error::Store("session poisoned".into()))?;
            to_bytes(&d)?
        };
        self.dirty.store(false, Ordering::Release);
        self.vault.save(&bytes)
    }

    pub fn save_if_dirty(&self) -> Result<()> {
        if self.dirty.load(Ordering::Acquire) {
            self.save()?;
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        if let Ok(mut d) = self.data.lock() {
            *d = SessionData::default();
        }
        self.vault.clear()
    }

    fn mark(&self) {
        self.dirty.store(true, Ordering::Release);
    }
}

fn to_bytes(d: &SessionData) -> Result<Vec<u8>> {
    let p = Persisted {
        home_dc: d.home_dc,
        dc_options: d.dc_options.values().cloned().collect(),
        peers: d.peer_infos.values().cloned().collect(),
        updates: d.updates_state.clone(),
    };
    Ok(serde_json::to_vec(&p)?)
}

fn from_bytes(b: &[u8]) -> Result<SessionData> {
    let p: Persisted = serde_json::from_slice(b)?;
    let mut d = SessionData {
        home_dc: p.home_dc,
        ..SessionData::default()
    };
    for o in p.dc_options {
        d.dc_options.insert(o.id, o);
    }
    for peer in p.peers {
        d.peer_infos.insert(peer.id(), peer);
    }
    d.updates_state = p.updates;
    Ok(d)
}

impl Session for EncryptedSession {
    type Error = SessionError;

    fn home_dc_id(&self) -> std::result::Result<i32, SessionError> {
        Ok(self.data()?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, std::result::Result<(), SessionError>> {
        Box::pin(async move {
            self.data()?.home_dc = dc_id;
            self.mark();
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> std::result::Result<Option<DcOption>, SessionError> {
        Ok(self.data()?.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(
        &self,
        dc_option: &DcOption,
    ) -> BoxFuture<'_, std::result::Result<(), SessionError>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            self.data()?.dc_options.insert(dc_option.id, dc_option);
            self.mark();
            Ok(())
        })
    }

    fn peer(
        &self,
        peer: PeerId,
    ) -> BoxFuture<'_, std::result::Result<Option<PeerInfo>, SessionError>> {
        Box::pin(async move { Ok(self.data()?.peer_infos.get(&peer).cloned()) })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, std::result::Result<(), SessionError>> {
        Box::pin(async move {
            self.data()?
                .peer_infos
                .entry(peer.id())
                .or_insert_with(|| peer.clone())
                .extend_info(&peer);
            self.mark();
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, std::result::Result<UpdatesState, SessionError>> {
        Box::pin(async move { Ok(self.data()?.updates_state.clone()) })
    }

    fn set_update_state(
        &self,
        update: UpdateState,
    ) -> BoxFuture<'_, std::result::Result<(), SessionError>> {
        Box::pin(async move {
            let mut data = self.data()?;
            match update {
                UpdateState::All(s) => data.updates_state = s,
                UpdateState::Primary { pts, date, seq } => {
                    data.updates_state.pts = pts;
                    data.updates_state.date = date;
                    data.updates_state.seq = seq;
                }
                UpdateState::Secondary { qts } => data.updates_state.qts = qts,
                UpdateState::Channel { id, pts } => {
                    data.updates_state.channels.retain(|c| c.id != id);
                    data.updates_state.channels.push(ChannelState { id, pts });
                }
            }
            drop(data);
            self.mark();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::secrets::MemoryStore;
    use std::sync::Arc;

    #[tokio::test]
    async fn session_roundtrips_through_vault() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(Arc::new(MemoryStore::new()), dir.path().join("s.enc"));
        let s = EncryptedSession::load(vault.clone()).unwrap();
        s.set_home_dc_id(4).await.unwrap();
        s.set_update_state(UpdateState::Primary {
            pts: 10,
            date: 20,
            seq: 30,
        })
        .await
        .unwrap();
        s.set_update_state(UpdateState::Channel { id: 5, pts: 7 })
            .await
            .unwrap();
        assert!(s.dirty.load(Ordering::Acquire));
        s.save_if_dirty().unwrap();
        assert!(!s.dirty.load(Ordering::Acquire));

        let again = EncryptedSession::load(vault).unwrap();
        assert_eq!(again.home_dc_id().unwrap(), 4);
        let st = again.updates_state().await.unwrap();
        assert_eq!((st.pts, st.date, st.seq), (10, 20, 30));
        assert_eq!(st.channels.len(), 1);
        assert_eq!(again.dc_option(2).unwrap().map(|o| o.id), Some(2));
    }

    #[tokio::test]
    async fn clear_removes_blob() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(Arc::new(MemoryStore::new()), dir.path().join("s.enc"));
        let s = EncryptedSession::load(vault.clone()).unwrap();
        s.save().unwrap();
        assert!(vault.path().exists());
        s.clear().unwrap();
        assert!(!vault.path().exists());
    }
}
