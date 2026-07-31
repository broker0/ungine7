//! Authentication — verifying "are you who you claim to be?"
//!
//! [`Authenticator`] accepts a username and credential (password,
//! token, hash — anything), and returns an account or an error.
//!
//! The trait is synchronous, like all framework traits. If an implementation
//! needs I/O (DB query), it uses an in-memory cache,
//! `tokio::task::spawn_blocking`, or RPC via a channel — its choice.

use std::fmt;

use super::account::Account;

// ── AuthError ────────────────────────────────────────────────────────────

/// Authentication error.
///
/// `E` — backend-specific implementation error (I/O, DB, network, etc.).
/// Standard variants cover typical failure scenarios.
#[derive(Debug)]
pub enum AuthError<E> {
    /// Invalid credentials (password, token, etc.).
    InvalidCredentials,

    /// Account with this name not found.
    AccountNotFound,

    /// Account temporarily blocked.
    AccountSuspended,

    /// Account permanently blocked.
    AccountBanned,

    /// Too many attempts — rate limiting.
    RateLimited,

    /// Backend error (DB unavailable, I/O, etc.).
    Backend(E),
}

impl<E: fmt::Display> fmt::Display for AuthError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCredentials => write!(f, "invalid credentials"),
            Self::AccountNotFound    => write!(f, "account not found"),
            Self::AccountSuspended   => write!(f, "account suspended"),
            Self::AccountBanned      => write!(f, "account banned"),
            Self::RateLimited        => write!(f, "rate limited"),
            Self::Backend(e)         => write!(f, "backend error: {e}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for AuthError<E> {}

// ── Authenticator trait ──────────────────────────────────────────────────

/// Authenticator — validates user credentials.
///
/// `Credential` — associated type: password (`String`), hash (`[u8; 32]`),
/// OAuth token, pair (password, OTP) — implementation decides.
///
/// # Example
///
/// ```ignore
/// pub struct PlainAuth { store: Arc<MemoryAccountStore> }
///
/// impl Authenticator for PlainAuth {
///     type Account = MyAccount;
///     type Credential = String;          // plain-text password
///     type Error = std::io::Error;
///
///     fn authenticate(
///         &self, username: &str, credential: &String,
///     ) -> Result<MyAccount, AuthError<std::io::Error>> {
///         let acc = self.store.find(username)
///             .ok_or(AuthError::AccountNotFound)?;
///         if acc.password != *credential {
///             return Err(AuthError::InvalidCredentials);
///         }
///         Ok(acc)
///     }
/// }
/// ```
pub trait Authenticator: Send + Sync {
    /// Account type returned on successful authentication.
    type Account: Account;

    /// Credential type. The framework neither knows nor imposes the format.
    type Credential: Send;

    /// Backend-specific error (I/O, DB, etc.).
    type Error: std::error::Error + Send + Sync;

    /// Validate credentials, return account on success.
    ///
    /// Implementation **must** check [`AccountStatus`](super::AccountStatus):
    /// if the account is found but blocked — return
    /// [`AuthError::AccountSuspended`] or [`AuthError::AccountBanned`].
    fn authenticate(
        &self,
        username: &str,
        credential: &Self::Credential,
    ) -> Result<Self::Account, AuthError<Self::Error>>;
}
