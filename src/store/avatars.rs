//! On-disk cache of profile photos.
//!
//! Files are named `<chat id>_<photo id>.img` (numbers only, so a hostile
//! value can never form a path). A new photo id for a chat replaces its old
//! file, and the directory is pruned to a file-count and byte budget by
//! least-recent modification, so it stays bounded however long the app runs.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::model::ChatId;

/// Largest avatar we accept or keep (a 160 px JPEG is ~10 KB).
pub const MAX_AVATAR_BYTES: usize = 512 * 1024;
/// Prune above this many files...
pub const MAX_FILES: usize = 1500;
/// ...or this many bytes.
pub const MAX_TOTAL_BYTES: u64 = 48 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct AvatarDisk {
    dir: PathBuf,
}

impl AvatarDisk {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn file_name(chat: ChatId, photo_id: i64) -> String {
        format!("{chat}_{photo_id}.img")
    }

    fn path(&self, chat: ChatId, photo_id: i64) -> PathBuf {
        self.dir.join(Self::file_name(chat, photo_id))
    }

    /// Cached bytes, or `None` (missing, unreadable or oversized).
    pub fn read(&self, chat: ChatId, photo_id: i64) -> Option<Vec<u8>> {
        let p = self.path(chat, photo_id);
        let len = std::fs::metadata(&p).ok()?.len();
        if len == 0 || len > MAX_AVATAR_BYTES as u64 {
            return None;
        }
        std::fs::read(&p).ok()
    }

    /// Store `bytes`, dropping older photos of the same chat, then prune.
    pub fn write(&self, chat: ChatId, photo_id: i64, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
            return Err(std::io::Error::other("avatar size out of range"));
        }
        crate::config::ensure_private_dir(&self.dir)?;
        let prefix = format!("{chat}_");
        let keep = Self::file_name(chat, photo_id);
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&prefix) && name != keep {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let tmp = self.dir.join(format!("{keep}.tmp"));
        {
            use std::io::Write;
            let mut f = crate::config::create_private_file(&tmp)?;
            f.write_all(bytes)?;
        }
        std::fs::rename(&tmp, self.path(chat, photo_id))?;
        self.prune(MAX_FILES, MAX_TOTAL_BYTES);
        Ok(())
    }

    /// Delete the oldest files until both budgets hold. Returns how many
    /// files were removed.
    pub fn prune(&self, max_files: usize, max_bytes: u64) -> usize {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let md = e.metadata().ok()?;
                md.is_file().then(|| {
                    (
                        md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        md.len(),
                        e.path(),
                    )
                })
            })
            .collect();
        files.sort_by_key(|(t, _, _)| *t);
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        let mut count = files.len();
        let mut removed = 0;
        for (_, len, path) in files {
            if count <= max_files && total <= max_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                count -= 1;
                total = total.saturating_sub(len);
                removed += 1;
            }
        }
        removed
    }

    /// Remove everything (logout).
    pub fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn roundtrip_and_replace_old_photo() {
        let d = tempfile::tempdir().unwrap();
        let c = AvatarDisk::new(d.path().join("av"));
        assert!(c.read(5, 1).is_none());
        c.write(5, 1, b"one").unwrap();
        assert_eq!(c.read(5, 1).unwrap(), b"one");
        c.write(5, 2, b"two").unwrap();
        assert!(c.read(5, 1).is_none(), "stale photo removed");
        assert_eq!(c.read(5, 2).unwrap(), b"two");
        c.write(6, 9, b"other").unwrap();
        assert_eq!(c.read(5, 2).unwrap(), b"two", "other chats untouched");
    }

    #[test]
    fn rejects_empty_and_oversized() {
        let d = tempfile::tempdir().unwrap();
        let c = AvatarDisk::new(d.path().to_path_buf());
        assert!(c.write(1, 1, b"").is_err());
        assert!(c.write(1, 1, &vec![0; MAX_AVATAR_BYTES + 1]).is_err());
        // An oversized file planted on disk is ignored on read.
        std::fs::write(c.path(2, 2), vec![0; MAX_AVATAR_BYTES + 1]).unwrap();
        assert!(c.read(2, 2).is_none());
    }

    #[test]
    fn prune_keeps_budgets_dropping_oldest() {
        let d = tempfile::tempdir().unwrap();
        let c = AvatarDisk::new(d.path().to_path_buf());
        for i in 0..5 {
            c.write(i, 1, &[7u8; 100]).unwrap();
            // Distinct mtimes so "oldest" is well defined.
            let t = SystemTime::now() - std::time::Duration::from_secs(100 - i as u64);
            let f = std::fs::File::options()
                .write(true)
                .open(c.path(i, 1))
                .unwrap();
            f.set_modified(t).unwrap();
        }
        assert_eq!(c.prune(3, u64::MAX), 2);
        assert!(c.read(0, 1).is_none() && c.read(1, 1).is_none());
        assert!(c.read(4, 1).is_some());
        assert_eq!(c.prune(10, 250), 1, "byte budget");
        assert_eq!(c.prune(10, 1000), 0);
    }

    #[test]
    fn file_names_are_numeric_only() {
        let n = AvatarDisk::file_name(-1_000_000_000_042, 77);
        assert!(
            n.chars()
                .all(|ch| ch.is_ascii_digit() || "-_.img".contains(ch))
        );
    }
}
