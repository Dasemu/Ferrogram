//! Paths (XDG via `directories`), feature flags and env overrides.

use std::path::{Path, PathBuf};

use secrecy::SecretString;

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
            screenshot,
            light_theme: std::env::var("FERRO_THEME").is_ok_and(|v| v == "light"),
            trace_startup: env_flag("FERRO_TRACE_STARTUP"),
            notification_preview: !env_flag("FERROGRAM_NOTIFY_SENDER_ONLY"),
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

    /// Telegram API credentials: env vars first, then `config.ini` in the
    /// config dir (`api_id = …`, `api_hash = …`). Never compiled in.
    pub fn api_credentials(&self) -> Result<ApiCredentials> {
        let file = std::fs::read_to_string(self.config_dir.join("config.ini")).unwrap_or_default();
        ApiCredentials::resolve(
            std::env::var("FERROGRAM_API_ID").ok(),
            std::env::var("FERROGRAM_API_HASH").ok(),
            &file,
        )
    }
}

pub struct ApiCredentials {
    pub api_id: i32,
    pub api_hash: SecretString,
}

impl ApiCredentials {
    pub fn resolve(env_id: Option<String>, env_hash: Option<String>, file: &str) -> Result<Self> {
        let from_file = |key: &str| {
            file.lines().find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
            })
        };
        let id = env_id.or_else(|| from_file("api_id"));
        let hash = env_hash.or_else(|| from_file("api_hash"));
        match (id.and_then(|i| i.trim().parse::<i32>().ok()), hash) {
            (Some(api_id), Some(h)) if !h.trim().is_empty() => Ok(Self {
                api_id,
                api_hash: SecretString::from(h.trim().to_owned()),
            }),
            _ => Err(Error::Auth(AuthError::MissingApiCredentials)),
        }
    }
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
        )
        .unwrap();
        assert_eq!(c.api_id, 123);
        assert_eq!(c.api_hash.expose_secret(), "abc");
    }

    #[test]
    fn credentials_from_file() {
        let c = ApiCredentials::resolve(None, None, "# comment\napi_id = 42\napi_hash = \"h\"\n")
            .unwrap();
        assert_eq!(c.api_id, 42);
    }

    #[test]
    fn missing_credentials_is_an_auth_error() {
        assert!(matches!(
            ApiCredentials::resolve(None, None, ""),
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
