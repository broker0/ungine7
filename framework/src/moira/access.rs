//! Access control — "what are you allowed to do?"
//!
//! [`AccessPolicy`] checks account permissions. `Permission` —
//! associated type: enum, string, bitflags — up to the implementation.
//!
//! Separation of authn/authz: [`Authenticator`](super::Authenticator)
//! handles "who you are", while `AccessPolicy` handles "what you may do".

use std::fmt::Debug;
use std::hash::Hash;

use super::account::Account;

// ── AccessPolicy trait ───────────────────────────────────────────────────

/// Access policy — checks whether an account has permission
/// for a specific action.
///
/// Implementation defines the `Permission` type and mapping logic.
/// Typical strategies:
/// - **Role-based** (RBAC): account has a role → role grants a set of permissions
/// - **Direct**: permissions stored directly in the account
/// - **Hierarchical**: access levels (Player < GM < Admin)
///
/// # Example
///
/// ```ignore
/// #[derive(Clone, Eq, PartialEq, Hash, Debug)]
/// pub enum Permission { Login, Teleport, Ban, ManageServer }
///
/// pub struct RolePolicy;
///
/// impl AccessPolicy for RolePolicy {
///     type Account = MyAccount;
///     type Permission = Permission;
///
///     fn has_permission(&self, acc: &MyAccount, perm: &Permission) -> bool {
///         match acc.role {
///             Role::Admin => true,
///             Role::GM    => matches!(perm, Permission::Login | Permission::Teleport),
///             Role::Player => matches!(perm, Permission::Login),
///         }
///     }
///
///     fn permissions(&self, acc: &MyAccount) -> Vec<Permission> {
///         // ...
///     }
/// }
/// ```
pub trait AccessPolicy: Send + Sync {
    /// Account type.
    type Account: Account;

    /// Permission type. Enum, string, bitflag — implementation decides.
    type Permission: Clone + Eq + Hash + Send + Sync + Debug;

    /// Check whether the account has a specific permission.
    fn has_permission(
        &self,
        account: &Self::Account,
        permission: &Self::Permission,
    ) -> bool;

    /// Get all permissions of the account.
    ///
    /// Useful for debugging, logging and UI (list of available commands).
    fn permissions(
        &self,
        account: &Self::Account,
    ) -> Vec<Self::Permission>;
}
