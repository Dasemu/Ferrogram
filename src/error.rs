//! Crate-wide error type, user-facing messages and redaction helpers.

use std::fmt;

/// Result alias used by every library-style module (`net`, `store`, `auth`).
pub type Result<T> = std::result::Result<T, Error>;

/// Authentication specific failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The phone number was rejected by the server.
    InvalidPhone,
    /// The login code was wrong or expired.
    InvalidCode,
    /// The 2FA (cloud) password was wrong.
    InvalidPassword,
    /// The phone number has no Telegram account yet (sign-up is not supported).
    SignUpRequired,
    /// The server asked us to wait before retrying.
    FloodWait(u32),
    /// The stored session was revoked server-side.
    SessionRevoked,
    /// `FERROGRAM_API_ID` / `FERROGRAM_API_HASH` are missing.
    MissingApiCredentials,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPhone => write!(f, "invalid phone number"),
            Self::InvalidCode => write!(f, "invalid code"),
            Self::InvalidPassword => write!(f, "invalid password"),
            Self::SignUpRequired => write!(f, "sign-up required"),
            Self::FloodWait(s) => write!(f, "flood wait {s}s"),
            Self::SessionRevoked => write!(f, "session revoked"),
            Self::MissingApiCredentials => write!(f, "missing api credentials"),
        }
    }
}

/// Errors grouped by origin (architecture §7). Cloneable so it can travel
/// inside iced messages; payloads are already-redacted strings.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Net(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("authentication error: {0}")]
    Auth(AuthError),
    #[error("storage error: {0}")]
    Store(String),
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("i/o error: {0}")]
    Io(String),
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// A short message that is safe to show in the UI (no secrets, no ids).
    pub fn user_message(&self) -> String {
        match self {
            Self::Net(_) => "Connection problem. Retrying…".into(),
            Self::Protocol(_) => "Telegram returned an unexpected response.".into(),
            Self::Auth(a) => match a {
                AuthError::InvalidPhone => "Invalid phone number. Please try again.".into(),
                AuthError::InvalidCode => "Invalid code. Please try again.".into(),
                AuthError::InvalidPassword => "Invalid password. Please try again.".into(),
                AuthError::SignUpRequired => {
                    "This number is not registered. Sign up with an official app first.".into()
                }
                AuthError::FloodWait(s) => format!("Too many attempts. Try again in {s} s."),
                AuthError::SessionRevoked => "Your session ended. Please log in again.".into(),
                AuthError::MissingApiCredentials => {
                    "Set FERROGRAM_API_ID and FERROGRAM_API_HASH (see README).".into()
                }
            },
            Self::Store(_) => "Local cache error.".into(),
            Self::Keyring(_) => {
                "The system keyring (Secret Service) is not available. Ferrogram never stores \
                 your session in plain text, so it cannot log you in without it."
                    .into()
            }
            Self::Io(_) => "File access failed.".into(),
            Self::Cancelled => "Cancelled.".into(),
        }
    }

    /// True when the error means the session must be dropped.
    pub fn is_session_revoked(&self) -> bool {
        matches!(self, Self::Auth(AuthError::SessionRevoked))
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.kind().to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Protocol(format!("serialization: {}", e.classify() as u8))
    }
}

/// Wrapper for tracing fields that must never be printed verbatim
/// (message bodies, file names, codes). Prints only the length.
pub struct Redacted<T>(pub T);

impl<T: AsRef<str>> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[redacted len={}]", self.0.as_ref().chars().count())
    }
}

impl<T: AsRef<str>> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A phone number. `Debug`/`Display` mask everything but the country prefix
/// and the last two digits: `+34•••••••12`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Phone(String);

impl Phone {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Digits and leading `+` only, as Telegram expects.
    pub fn normalized(&self) -> String {
        let mut out = String::new();
        for (i, c) in self.0.trim().chars().enumerate() {
            if c.is_ascii_digit() || (i == 0 && c == '+') {
                out.push(c);
            }
        }
        out
    }

    /// The raw value. Only for the protocol layer; never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Plausibility check: 7..=15 digits (E.164).
    pub fn is_plausible(&self) -> bool {
        let digits = self
            .normalized()
            .chars()
            .filter(char::is_ascii_digit)
            .count();
        (7..=15).contains(&digits)
    }

    pub fn masked(&self) -> String {
        let n = self.normalized();
        let digits: Vec<char> = n.chars().filter(char::is_ascii_digit).collect();
        if digits.len() < 5 {
            return "•••".into();
        }
        let head: String = digits[..2].iter().collect();
        let tail: String = digits[digits.len() - 2..].iter().collect();
        let dots = "•".repeat(digits.len() - 4);
        format!("+{head}{dots}{tail}")
    }
}

impl fmt::Debug for Phone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Phone({})", self.masked())
    }
}

impl fmt::Display for Phone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.masked())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_is_masked_in_debug_and_display() {
        let p = Phone::new("+34 600 123 412");
        assert_eq!(format!("{p}"), "+34•••••••12");
        assert_eq!(format!("{p:?}"), "Phone(+34•••••••12)");
        assert!(!format!("{p:?}").contains("600123"));
    }

    #[test]
    fn phone_normalization_and_plausibility() {
        let p = Phone::new(" +1 (555) 010-9999 ");
        assert_eq!(p.normalized(), "+15550109999");
        assert!(p.is_plausible());
        assert!(!Phone::new("123").is_plausible());
    }

    #[test]
    fn redacted_hides_content() {
        let s = format!("{}", Redacted("secret body"));
        assert_eq!(s, "[redacted len=11]");
    }

    #[test]
    fn user_messages_are_secret_free() {
        let e = Error::Net("tcp reset 149.154.167.50".into());
        assert!(!e.user_message().contains("149."));
        assert!(Error::Auth(AuthError::SessionRevoked).is_session_revoked());
    }
}
