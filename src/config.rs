//! Paths (XDG via `directories`), feature flags and env overrides.

use std::path::{Path, PathBuf};

use secrecy::SecretString;

use crate::auth::secrets::{Secret, SecretStore};
use crate::error::{AuthError, Error, Result};

pub const APP_ID: &str = "dev.ferro.ferrogram";
pub const APP_NAME: &str = "Ferrogram";

/// Which screen `--screenshot` should render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotTarget {
    LoginPhone,
    LoginCode,
    LoginPassword,
    Chats,
    Search,
    Reply,
    Media,
}

impl ScreenshotTarget {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "login-phone" => Self::LoginPhone,
            "login-code" => Self::LoginCode,
            "login-password" => Self::LoginPassword,
            "chats" => Self::Chats,
            "search" => Self::Search,
            "reply" => Self::Reply,
            "media" => Self::Media,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub config_dir: PathBuf,
    pub mock: bool,
    pub screenshot: Option<(ScreenshotTarget, PathBuf)>,
    pub light_theme: bool,
    pub trace_startup: bool,
    /// Notification privacy: when false only the sender is shown.
    pub notification_preview: bool,
    /// Desktop notifications on/off (`FERROGRAM_NO_NOTIFY=1` disables).
    pub notifications: bool,
}

impl Config {
    /// Build the config from CLI args and env vars.
    pub fn from_env(args: &[String]) -> Self {
        let dirs = directories::ProjectDirs::from("dev", "ferro", "ferrogram");
        let fallback = std::env::temp_dir().join("ferrogram");
        let data_dir = dirs
            .as_ref()
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| fallback.clone());
        let state_dir = dirs
            .as_ref()
            .and_then(|d| d.state_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| data_dir.join("state"));
        let config_dir = dirs
            .as_ref()
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or(fallback);

        let mock = args.iter().any(|a| a == "--mock");
        let screenshot = args.iter().position(|a| a == "--screenshot").and_then(|i| {
            let target = ScreenshotTarget::parse(args.get(i + 1)?)?;
            let out = PathBuf::from(args.get(i + 2)?);
            Some((target, out))
        });
        let env_flag = |k: &str| std::env::var(k).is_ok_and(|v| v == "1" || v == "true");
        Self {
            data_dir,
            state_dir,
            config_dir,
            mock: mock || screenshot.is_some(),
            screenshot: screenshot.clone(),
            light_theme: std::env::var("FERRO_THEME").is_ok_and(|v| v == "light"),
            trace_startup: env_flag("FERRO_TRACE_STARTUP"),
            notification_preview: !env_flag("FERROGRAM_NOTIFY_SENDER_ONLY"),
            notifications: !env_flag("FERROGRAM_NO_NOTIFY") && screenshot.is_none(),
        }
    }

    pub fn cache_db_path(&self) -> PathBuf {
        self.data_dir.join("cache.sqlite3")
    }

    pub fn session_blob_path(&self) -> PathBuf {
        self.data_dir.join("session.enc")
    }

    pub fn log_path(&self) -> PathBuf {
        self.state_dir.join("log")
    }

    fn config_ini(&self) -> PathBuf {
        self.config_dir.join("config.ini")
    }

    /// Telegram API credentials (blocking: touches the keyring, call from a
    /// blocking thread).
    ///
    /// * `api_id` (not secret): `FERROGRAM_API_ID`, else `config.ini`.
    /// * `api_hash` (secret): `FERROGRAM_API_HASH`, else the OS keyring
    ///   (`dev.ferro.ferrogram` / `api-hash`). It is never read from or
    ///   written to a plain file. A legacy `api_hash` line found in
    ///   `config.ini`, or a hash given via the env var, is imported into the
    ///   keyring; the legacy line is then removed from the file.
    pub fn load_api_credentials(&self, secrets: &dyn SecretStore) -> Result<ApiCredentials> {
        let path = self.config_ini();
        let file = std::fs::read_to_string(&path).unwrap_or_default();
        let env_hash = std::env::var("FERROGRAM_API_HASH")
            .ok()
            .filter(|h| !h.trim().is_empty());
        if let Some(legacy) = ini_value(&file, "api_hash")
            && secrets
                .set(API_HASH_KEY, &Secret::new(legacy.into_bytes()))
                .is_ok()
        {
            let cleaned: String = file
                .lines()
                .filter(|l| {
                    l.split_once('=')
                        .is_none_or(|(k, _)| k.trim() != "api_hash")
                })
                .map(|l| format!("{l}\n"))
                .collect();
            let _ = write_private(&path, &cleaned);
            tracing::info!("migrated api_hash from config.ini into the keyring");
        }
        if let Some(h) = &env_hash {
            // Import so the env var is only needed once.
            let _ = secrets.set(API_HASH_KEY, &Secret::new(h.trim().as_bytes().to_vec()));
        }
        let keyring_hash = match env_hash {
            Some(_) => None,
            None => secrets
                .get(API_HASH_KEY)?
                .and_then(|s| String::from_utf8(s.expose().to_vec()).ok()),
        };
        let file = std::fs::read_to_string(&path).unwrap_or_default();
        ApiCredentials::resolve(
            std::env::var("FERROGRAM_API_ID").ok(),
            std::env::var("FERROGRAM_API_HASH").ok(),
            &file,
            keyring_hash,
        )
    }

    /// First-run setup: `api_id` to `config.ini` (0600), `api_hash` to the
    /// keyring. Blocking.
    pub fn save_api_credentials(
        &self,
        secrets: &dyn SecretStore,
        api_id: i32,
        api_hash: &str,
    ) -> Result<()> {
        secrets.set(
            API_HASH_KEY,
            &Secret::new(api_hash.trim().as_bytes().to_vec()),
        )?;
        ensure_private_dir(&self.config_dir)?;
        write_private(&self.config_ini(), &format!("api_id = {api_id}\n"))?;
        Ok(())
    }
}

/// Keyring account holding the Telegram `api_hash`.
pub const API_HASH_KEY: &str = "api-hash";

fn ini_value(file: &str, key: &str) -> Option<String> {
    file.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        let v = v.trim().trim_matches('"');
        (k.trim() == key && !v.is_empty()).then(|| v.to_owned())
    })
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = create_private_file(path)?;
    f.write_all(contents.as_bytes())
}

pub struct ApiCredentials {
    pub api_id: i32,
    pub api_hash: SecretString,
}

impl ApiCredentials {
    /// Pure resolution: `api_id` from env or file, `api_hash` from env or
    /// keyring. A hash in the file is deliberately ignored.
    pub fn resolve(
        env_id: Option<String>,
        env_hash: Option<String>,
        file: &str,
        keyring_hash: Option<String>,
    ) -> Result<Self> {
        let id = env_id.or_else(|| ini_value(file, "api_id"));
        let hash = env_hash.filter(|h| !h.trim().is_empty()).or(keyring_hash);
        match (id.and_then(|i| i.trim().parse::<i32>().ok()), hash) {
            (Some(api_id), Some(h)) if !h.trim().is_empty() => Ok(Self {
                api_id,
                api_hash: SecretString::from(h.trim().to_owned()),
            }),
            _ => Err(Error::Auth(AuthError::MissingApiCredentials)),
        }
    }
}

/// Create `dir` (and parents) and make it owner-only (0700) on Unix.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Make an existing file owner-only (0600) on Unix. Missing files are ignored.
pub fn restrict_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Open a file for writing that is created owner-only (0600) from the start.
pub fn create_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn credentials_env_wins_over_file() {
        let c = ApiCredentials::resolve(
            Some("123".into()),
            Some("abc".into()),
            "api_id=9\napi_hash=zzz",
            Some("keyring".into()),
        )
        .unwrap();
        assert_eq!(c.api_id, 123);
        assert_eq!(c.api_hash.expose_secret(), "abc");
    }

    #[test]
    fn credentials_id_from_file_hash_from_keyring_only() {
        let file = "# comment\napi_id = 42\napi_hash = \"h\"\n";
        let c = ApiCredentials::resolve(None, None, file, Some("kh".into())).unwrap();
        assert_eq!(c.api_id, 42);
        assert_eq!(c.api_hash.expose_secret(), "kh");
        // A hash in the plain file is never used.
        assert!(ApiCredentials::resolve(None, None, file, None).is_err());
    }

    #[test]
    fn save_and_migrate_credentials() {
        use crate::auth::secrets::MemoryStore;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::from_env(&["x".into(), "--mock".into()]);
        cfg.config_dir = dir.path().join("cfg");
        let secrets = MemoryStore::new();
        cfg.save_api_credentials(&secrets, 7, "secret-hash")
            .unwrap();
        let ini = std::fs::read_to_string(cfg.config_ini()).unwrap();
        assert!(ini.contains("api_id = 7") && !ini.contains("secret-hash"));
        // Legacy file with a plain-text hash is migrated and scrubbed.
        std::fs::write(cfg.config_ini(), "api_id = 8\napi_hash = legacy\n").unwrap();
        let secrets = MemoryStore::new();
        if std::env::var_os("FERROGRAM_API_HASH").is_none()
            && std::env::var_os("FERROGRAM_API_ID").is_none()
        {
            let c = cfg.load_api_credentials(&secrets).unwrap();
            assert_eq!((c.api_id, c.api_hash.expose_secret()), (8, "legacy"));
            assert!(
                !std::fs::read_to_string(cfg.config_ini())
                    .unwrap()
                    .contains("legacy")
            );
        }
    }

    #[test]
    fn missing_credentials_is_an_auth_error() {
        assert!(matches!(
            ApiCredentials::resolve(None, None, "", None),
            Err(Error::Auth(AuthError::MissingApiCredentials))
        ));
    }

    #[test]
    fn screenshot_args_imply_mock() {
        let args: Vec<String> = ["x", "--screenshot", "chats", "/tmp/a.png"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let c = Config::from_env(&args);
        assert!(c.mock);
        assert_eq!(c.screenshot.unwrap().0, ScreenshotTarget::Chats);
    }
}
