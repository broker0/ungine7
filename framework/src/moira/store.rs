//! Account storage — lookup and CRUD.
//!
//! [`AccountStore`] provides a minimal set of read operations.
//! Account creation is intentionally not included — each implementation
//! has its own set of required fields (password, email, role, etc.),
//! and the framework cannot define a universal signature.
//! Implementations add their own creation methods on the concrete type.

use std::fmt;

use super::account::Account;

// ── AccountStoreError ────────────────────────────────────────────────────

/// Account storage error.
#[derive(Debug)]
pub enum AccountStoreError<E> {
    /// Account not found.
    NotFound,

    /// Account with this name/id already exists.
    AlreadyExists,

    /// Backend error.
    Backend(E),
}

impl<E: fmt::Display> fmt::Display for AccountStoreError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound      => write!(f, "account not found"),
            Self::AlreadyExists => write!(f, "account already exists"),
            Self::Backend(e)    => write!(f, "backend error: {e}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for AccountStoreError<E> {}

// ── AccountStore trait ───────────────────────────────────────────────────

/// Account store — lookup by name and identifier.
///
/// Implementation determines backend: `HashMap`, file, SQL, Redis, etc.
/// Account creation is a method of the concrete type, not of the trait.
///
/// # Example
///
/// ```ignore
/// pub struct MemoryStore {
///     accounts: RwLock<HashMap<String, MyAccount>>,
/// }
///
/// impl AccountStore for MemoryStore {
///     type Account = MyAccount;
///     type Error = Infallible;
///
///     fn find_by_username(&self, username: &str) -> Result<Option<MyAccount>, Infallible> {
///         Ok(self.accounts.read().get(username).cloned())
///     }
///
///     fn find_by_id(&self, id: &u32) -> Result<Option<MyAccount>, Infallible> {
///         Ok(self.accounts.read().values().find(|a| a.id == *id).cloned())
///     }
/// }
/// ```
pub trait AccountStore: Send + Sync {
    /// Account type.
    type Account: Account;

    /// Backend-specific error.
    type Error: std::error::Error + Send + Sync;

    /// Find account by username (login).
    fn find_by_username(
        &self,
        username: &str,
    ) -> Result<Option<Self::Account>, Self::Error>;

    /// Find account by identifier.
    fn find_by_id(
        &self,
        id: &<Self::Account as Account>::Id,
    ) -> Result<Option<Self::Account>, Self::Error>;
}
