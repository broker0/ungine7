//! Abstract account and its status.
//!
//! [`Account`] is the central trait of the `moira` module. The framework does not know
//! how the account is stored (file, HashMap, SQL) — this is decided by the implementation.
//! The only requirements: identifier, username and status.

use std::fmt::{Debug, Display};
use std::hash::Hash;

// ── AccountStatus ────────────────────────────────────────────────────────

/// Account status in the system.
///
/// Minimal set of states sufficient for making decisions
/// about authentication and access. Implementation may extend semantics
/// through its own fields in the concrete account type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountStatus {
    /// Account active, login allowed.
    Active,

    /// Temporary lock (moderation, suspicious activity).
    Suspended,

    /// Permanent lock.
    Banned,

    /// Not activated / sleeping / awaiting confirmation.
    Inactive,
}

impl AccountStatus {
    /// `true` if the account can pass authentication.
    pub fn can_login(&self) -> bool {
        matches!(self, AccountStatus::Active)
    }
}

// ── Account trait ────────────────────────────────────────────────────────

/// Abstract user account.
///
/// The framework operates only with this trait, without knowing the concrete type.
/// The implementation defines:
/// - identifier type (`Id`) — `u32`, `Uuid`, `String`, …
/// - fields and storage — plain struct, ORM model, protobuf, etc.
///
/// `Clone` is required for passing the account between components
/// (session manager, access policy, etc.) without extra `Arc`.
///
/// # Example
///
/// ```ignore
/// #[derive(Debug, Clone)]
/// pub struct MyAccount {
///     pub id: u32,
///     pub username: String,
///     pub status: AccountStatus,
/// }
///
/// impl Account for MyAccount {
///     type Id = u32;
///     fn id(&self) -> &u32 { &self.id }
///     fn username(&self) -> &str { &self.username }
///     fn status(&self) -> AccountStatus { self.status }
/// }
/// ```
pub trait Account: Send + Sync + Clone + Debug {
    /// Account identifier type.
    ///
    /// Must be cheap to copy (or `Clone`), usable in `HashMap` and logs.
    type Id: Clone + Eq + Hash + Send + Sync + Debug + Display;

    /// Unique account identifier.
    fn id(&self) -> &Self::Id;

    /// Username (login).
    fn username(&self) -> &str;

    /// Current account status.
    fn status(&self) -> AccountStatus;
}
