//! Sessions — tracking active connections between login phases.
//!
//! In the UO protocol, login is two-phase: login-server → game-server.
//! [`SessionManager`] creates tokens upon successful authentication,
//! and [`SessionToken`] is an opaque identifier for passing
//! between phases.

use std::fmt::{self, Debug};
use std::hash::Hash;

use super::account::Account;

// ── SessionError ─────────────────────────────────────────────────────────

/// Session validation error.
#[derive(Debug)]
pub enum SessionError<E> {
    /// Token not found or corrupted.
    InvalidToken,

    /// Session expired.
    Expired,

    /// Account blocked after session creation.
    AccountSuspended,

    /// Backend error.
    Backend(E),
}

impl<E: fmt::Display> fmt::Display for SessionError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken      => write!(f, "invalid session token"),
            Self::Expired           => write!(f, "session expired"),
            Self::AccountSuspended  => write!(f, "account suspended"),
            Self::Backend(e)        => write!(f, "backend error: {e}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for SessionError<E> {}

// ── SessionToken trait ───────────────────────────────────────────────────

/// Opaque session token.
///
/// Implementation determines format: `u32` auth-key, UUID, JWT, etc.
/// The framework requires only basic trait bounds for storage
/// in collections and logging.
pub trait SessionToken: Clone + Eq + Hash + Send + Sync + Debug {}

// ── SessionManager trait ─────────────────────────────────────────────────

/// Session manager — creates, validates and destroys tokens.
///
/// Lifecycle:
/// 1. `create_session()` — after successful authentication (login phase)
/// 2. `validate_session()` — when connecting to the game server
/// 3. `destroy_session()` — upon disconnect / logout
///
/// # Example
///
/// ```ignore
/// pub struct MemorySessionManager {
///     sessions: RwLock<HashMap<u32, (MyAccount, Instant)>>,
///     counter: AtomicU32,
/// }
///
/// impl SessionManager for MemorySessionManager {
///     type Account = MyAccount;
///     type Token = AuthKey;        // wrapper over u32
///     type Error = Infallible;
///
///     fn create_session(&self, account: &MyAccount) -> Result<AuthKey, Infallible> {
///         let key = self.counter.fetch_add(1, Ordering::Relaxed);
///         self.sessions.write().insert(key, (account.clone(), Instant::now()));
///         Ok(AuthKey(key))
///     }
///     // ...
/// }
/// ```
pub trait SessionManager: Send + Sync {
    /// Type of account associated with the session.
    type Account: Account;

    /// Session token type.
    type Token: SessionToken;

    /// Backend-specific error.
    type Error: std::error::Error + Send + Sync;

    /// Create a new session for an authenticated account.
    fn create_session(
        &self,
        account: &Self::Account,
    ) -> Result<Self::Token, Self::Error>;

    /// Validate token and return the associated account.
    ///
    /// Implementation **may** check TTL, account status, etc.
    fn validate_session(
        &self,
        token: &Self::Token,
    ) -> Result<Self::Account, SessionError<Self::Error>>;

    /// Destroy the session (logout, disconnect, kick).
    fn destroy_session(
        &self,
        token: &Self::Token,
    ) -> Result<(), Self::Error>;
}
