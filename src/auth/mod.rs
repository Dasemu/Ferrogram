//! Login flow: phone → code → 2FA password (SRP), as an explicit state
//! machine. `reduce` is pure; the app turns the returned [`AuthCommand`]
//! into a network `Task` and feeds the result back as an [`AuthEvent`].

pub mod secrets;
pub mod vault;

use secrecy::SecretString;

use crate::error::{AuthError, Error, Phone};
use crate::model::User;

/// Outcome of `sign_in` / `check_password`.
#[derive(Debug, Clone)]
pub enum SignInOutcome {
    SignedIn(User),
    PasswordRequired { hint: Option<String> },
}

/// Text typed into a login field (phone, code or password). `Debug` never
/// prints it, so it is safe inside messages/events that may be logged.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SecretInput(pub String);

impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretInput(len={})", self.0.chars().count())
    }
}

impl From<String> for SecretInput {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SecretInput {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

#[derive(Clone, PartialEq, Default)]
pub enum AuthState {
    /// Checking the keyring for a stored session.
    #[default]
    Restoring,
    Phone {
        input: String,
        error: Option<String>,
        busy: bool,
    },
    Code {
        phone: Phone,
        input: String,
        error: Option<String>,
        busy: bool,
    },
    Password {
        hint: Option<String>,
        input: String,
        error: Option<String>,
        busy: bool,
    },
    SignedIn(UserSummary),
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = |s: &String| s.chars().count();
        match self {
            Self::Restoring => f.write_str("Restoring"),
            Self::Phone { input, error, busy } => f
                .debug_struct("Phone")
                .field("input_len", &len(input))
                .field("error", error)
                .field("busy", busy)
                .finish(),
            Self::Code {
                phone,
                input,
                error,
                busy,
            } => f
                .debug_struct("Code")
                .field("phone", phone)
                .field("input_len", &len(input))
                .field("error", error)
                .field("busy", busy)
                .finish(),
            Self::Password {
                hint,
                input,
                error,
                busy,
            } => f
                .debug_struct("Password")
                .field("has_hint", &hint.is_some())
                .field("input_len", &len(input))
                .field("error", error)
                .field("busy", busy)
                .finish(),
            Self::SignedIn(u) => f.debug_tuple("SignedIn").field(&u.id).finish(),
        }
    }
}

impl std::fmt::Debug for UserSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserSummary")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Non-sensitive part of the logged-in user kept in the auth state.
#[derive(Clone, PartialEq)]
pub struct UserSummary {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum AuthEvent {
    /// Result of trying to restore a stored session. `Ok(Some(user))` = logged in.
    Restored(Result<Option<User>, Error>),
    Input(SecretInput),
    Submit,
    Back,
    CodeRequested(Result<(), Error>),
    SignIn(Result<SignInOutcome, Error>),
    /// The session was revoked while running; go back to phone entry.
    LoggedOut,
}

/// Side effect requested by the state machine.
#[derive(Debug)]
pub enum AuthCommand {
    RequestCode(Phone),
    SignIn {
        code: String,
    },
    CheckPassword(SecretString),
    /// Persist the session (keyring + encrypted blob).
    PersistSession,
}

fn phone_state(input: String, error: Option<String>) -> AuthState {
    AuthState::Phone {
        input,
        error,
        busy: false,
    }
}

/// Pure transition function.
pub fn reduce(state: &mut AuthState, event: AuthEvent) -> Option<AuthCommand> {
    use AuthEvent as E;
    use AuthState as S;

    match (std::mem::take(state), event) {
        (S::Restoring, E::Restored(Ok(Some(user)))) => {
            *state = S::SignedIn(summary(&user));
            None
        }
        (S::Restoring, E::Restored(Ok(None))) => {
            *state = phone_state(String::new(), None);
            None
        }
        (S::Restoring, E::Restored(Err(e))) => {
            *state = phone_state(String::new(), Some(e.user_message()));
            None
        }

        // ---- phone entry
        (S::Phone { error, busy, .. }, E::Input(SecretInput(s))) => {
            let input: String = s
                .chars()
                .filter(|c| c.is_ascii_digit() || " +-()".contains(*c))
                .take(24)
                .collect();
            *state = S::Phone { input, error, busy };
            None
        }
        (
            S::Phone {
                input, busy: false, ..
            },
            E::Submit,
        ) => {
            let phone = Phone::new(input.clone());
            if !phone.is_plausible() {
                *state = phone_state(
                    input,
                    Some(Error::Auth(AuthError::InvalidPhone).user_message()),
                );
                return None;
            }
            *state = S::Phone {
                input,
                error: None,
                busy: true,
            };
            Some(AuthCommand::RequestCode(phone))
        }
        (S::Phone { input, .. }, E::CodeRequested(Ok(()))) => {
            *state = S::Code {
                phone: Phone::new(input),
                input: String::new(),
                error: None,
                busy: false,
            };
            None
        }
        (S::Phone { input, .. }, E::CodeRequested(Err(e))) => {
            *state = phone_state(input, Some(e.user_message()));
            None
        }

        // ---- code entry
        (
            S::Code {
                phone, error, busy, ..
            },
            E::Input(SecretInput(s)),
        ) => {
            let input: String = s.chars().filter(char::is_ascii_digit).take(6).collect();
            *state = S::Code {
                phone,
                input,
                error,
                busy,
            };
            None
        }
        (
            S::Code {
                phone,
                input,
                busy: false,
                ..
            },
            E::Submit,
        ) => {
            if input.len() < 5 {
                *state = S::Code {
                    phone,
                    input,
                    error: Some(Error::Auth(AuthError::InvalidCode).user_message()),
                    busy: false,
                };
                return None;
            }
            let code = input.clone();
            *state = S::Code {
                phone,
                input,
                error: None,
                busy: true,
            };
            Some(AuthCommand::SignIn { code })
        }
        (S::Code { phone, .. }, E::Back) => {
            *state = phone_state(phone.expose().to_owned(), None);
            None
        }
        (S::Code { .. }, E::SignIn(Ok(SignInOutcome::PasswordRequired { hint }))) => {
            *state = S::Password {
                hint,
                input: String::new(),
                error: None,
                busy: false,
            };
            None
        }
        (S::Code { phone, input, .. }, E::SignIn(Err(e))) => {
            *state = S::Code {
                phone,
                input: if matches!(e, Error::Auth(AuthError::InvalidCode)) {
                    String::new()
                } else {
                    input
                },
                error: Some(e.user_message()),
                busy: false,
            };
            None
        }

        // ---- 2FA password
        (
            S::Password {
                hint, error, busy, ..
            },
            E::Input(SecretInput(s)),
        ) => {
            *state = S::Password {
                hint,
                input: s,
                error,
                busy,
            };
            None
        }
        (
            S::Password {
                hint,
                input,
                busy: false,
                ..
            },
            E::Submit,
        ) => {
            if input.is_empty() {
                *state = S::Password {
                    hint,
                    input,
                    error: None,
                    busy: false,
                };
                return None;
            }
            let pw = SecretString::from(input);
            *state = S::Password {
                hint,
                input: String::new(),
                error: None,
                busy: true,
            };
            Some(AuthCommand::CheckPassword(pw))
        }
        (S::Password { .. }, E::Back) => {
            *state = phone_state(String::new(), None);
            None
        }
        (S::Password { hint, .. }, E::SignIn(Err(e))) => {
            *state = S::Password {
                hint,
                input: String::new(),
                error: Some(e.user_message()),
                busy: false,
            };
            None
        }

        // ---- success from either code or password
        (S::Code { .. } | S::Password { .. }, E::SignIn(Ok(SignInOutcome::SignedIn(user)))) => {
            *state = S::SignedIn(summary(&user));
            Some(AuthCommand::PersistSession)
        }

        (_, E::LoggedOut) => {
            *state = phone_state(
                String::new(),
                Some(Error::Auth(AuthError::SessionRevoked).user_message()),
            );
            None
        }

        // Anything else (e.g. a late network result, input while busy) is ignored.
        (prev, _) => {
            *state = prev;
            None
        }
    }
}

fn summary(u: &User) -> UserSummary {
    UserSummary {
        id: u.id,
        name: u.name.clone(),
    }
}

impl AuthState {
    pub fn is_signed_in(&self) -> bool {
        matches!(self, Self::SignedIn(_))
    }

    pub fn is_busy(&self) -> bool {
        match self {
            Self::Restoring => true,
            Self::Phone { busy, .. } | Self::Code { busy, .. } | Self::Password { busy, .. } => {
                *busy
            }
            Self::SignedIn(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn user() -> User {
        User {
            id: 1,
            name: "Me".into(),
            username: None,
        }
    }

    fn at_phone() -> AuthState {
        let mut s = AuthState::Restoring;
        reduce(&mut s, AuthEvent::Restored(Ok(None)));
        s
    }

    #[test]
    fn restore_with_session_goes_straight_in() {
        let mut s = AuthState::Restoring;
        assert!(reduce(&mut s, AuthEvent::Restored(Ok(Some(user())))).is_none());
        assert!(s.is_signed_in());
    }

    #[test]
    fn restore_failure_shows_error_on_phone_screen() {
        let mut s = AuthState::Restoring;
        reduce(
            &mut s,
            AuthEvent::Restored(Err(Error::Keyring("no dbus".into()))),
        );
        match s {
            AuthState::Phone { error: Some(e), .. } => assert!(e.contains("keyring")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn full_flow_with_2fa() {
        let mut s = at_phone();
        reduce(&mut s, AuthEvent::Input("+34 600 000 001x".into()));
        let cmd = reduce(&mut s, AuthEvent::Submit);
        assert!(
            matches!(cmd, Some(AuthCommand::RequestCode(ref p)) if p.normalized() == "+34600000001")
        );
        assert!(s.is_busy());
        // Submitting again while busy does nothing.
        assert!(reduce(&mut s, AuthEvent::Submit).is_none());

        reduce(&mut s, AuthEvent::CodeRequested(Ok(())));
        assert!(matches!(s, AuthState::Code { .. }));
        reduce(&mut s, AuthEvent::Input("12a34-5".into()));
        let cmd = reduce(&mut s, AuthEvent::Submit);
        assert!(matches!(cmd, Some(AuthCommand::SignIn { ref code }) if code == "12345"));

        reduce(
            &mut s,
            AuthEvent::SignIn(Ok(SignInOutcome::PasswordRequired {
                hint: Some("pet".into()),
            })),
        );
        assert!(
            matches!(s, AuthState::Password { ref hint, .. } if hint.as_deref() == Some("pet"))
        );

        reduce(&mut s, AuthEvent::Input("hunter2".into()));
        let cmd = reduce(&mut s, AuthEvent::Submit);
        match cmd {
            Some(AuthCommand::CheckPassword(p)) => assert_eq!(p.expose_secret(), "hunter2"),
            other => panic!("unexpected {other:?}"),
        }
        // Password is cleared from state as soon as it is submitted.
        assert!(matches!(s, AuthState::Password { ref input, busy: true, .. } if input.is_empty()));

        let cmd = reduce(
            &mut s,
            AuthEvent::SignIn(Ok(SignInOutcome::SignedIn(user()))),
        );
        assert!(matches!(cmd, Some(AuthCommand::PersistSession)));
        assert!(s.is_signed_in());
    }

    #[test]
    fn implausible_phone_is_rejected_locally() {
        let mut s = at_phone();
        reduce(&mut s, AuthEvent::Input("123".into()));
        assert!(reduce(&mut s, AuthEvent::Submit).is_none());
        assert!(matches!(
            s,
            AuthState::Phone {
                error: Some(_),
                busy: false,
                ..
            }
        ));
    }

    #[test]
    fn wrong_code_clears_input_and_shows_error() {
        let mut s = at_phone();
        reduce(&mut s, AuthEvent::Input("+15550100000".into()));
        reduce(&mut s, AuthEvent::Submit);
        reduce(&mut s, AuthEvent::CodeRequested(Ok(())));
        reduce(&mut s, AuthEvent::Input("99999".into()));
        reduce(&mut s, AuthEvent::Submit);
        reduce(
            &mut s,
            AuthEvent::SignIn(Err(Error::Auth(AuthError::InvalidCode))),
        );
        match &s {
            AuthState::Code {
                input,
                error: Some(_),
                busy: false,
                ..
            } => assert!(input.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
        // Back returns to phone with the number kept.
        reduce(&mut s, AuthEvent::Back);
        assert!(matches!(s, AuthState::Phone { ref input, .. } if input == "+15550100000"));
    }

    #[test]
    fn wrong_password_keeps_password_screen() {
        let mut s = AuthState::Password {
            hint: None,
            input: "x".into(),
            error: None,
            busy: false,
        };
        reduce(&mut s, AuthEvent::Submit);
        reduce(
            &mut s,
            AuthEvent::SignIn(Err(Error::Auth(AuthError::InvalidPassword))),
        );
        assert!(matches!(
            s,
            AuthState::Password {
                error: Some(_),
                busy: false,
                ..
            }
        ));
    }

    #[test]
    fn short_code_is_rejected_locally() {
        let mut s = AuthState::Code {
            phone: Phone::new("+15550100000"),
            input: "12".into(),
            error: None,
            busy: false,
        };
        assert!(reduce(&mut s, AuthEvent::Submit).is_none());
    }

    #[test]
    fn logout_returns_to_phone_with_message() {
        let mut s = AuthState::SignedIn(UserSummary {
            id: 1,
            name: "a".into(),
        });
        reduce(&mut s, AuthEvent::LoggedOut);
        assert!(matches!(s, AuthState::Phone { error: Some(_), .. }));
    }

    #[test]
    fn debug_never_prints_typed_secrets() {
        let s = AuthState::Password {
            hint: Some("my dog".into()),
            input: "hunter2".into(),
            error: None,
            busy: false,
        };
        let code = AuthState::Code {
            phone: Phone::new("+34600123412"),
            input: "54321".into(),
            error: None,
            busy: false,
        };
        let ev = AuthEvent::Input("hunter2".into());
        let all = format!("{s:?} {code:?} {ev:?}");
        for needle in ["hunter2", "my dog", "54321", "600123"] {
            assert!(!all.contains(needle), "{needle} leaked in {all}");
        }
    }

    #[test]
    fn late_results_are_ignored() {
        let mut s = at_phone();
        let before = s.clone();
        reduce(
            &mut s,
            AuthEvent::SignIn(Ok(SignInOutcome::SignedIn(user()))),
        );
        assert_eq!(s, before);
    }
}
