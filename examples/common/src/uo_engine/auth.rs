//! Concrete account, role and permission types for UO.
//!
//! Implements abstractions from [`framework::moira`]:
//! - [`DemoAccount`] — account with UO-specific fields
//! - [`AccessLevel`] — role hierarchy (Player → Developer)
//! - [`Permission`] — granular permissions
//! - [`Credential`] — plain-text password (for demo)
//!
//! And ready-to-use service implementations for demos/examples:
//! - [`MemoryAccountStore`] — in-memory HashMap, auto-creates accounts
//! - [`PlainAuthenticator`] — plain-text password check (demo only!)
//! - [`SimpleSessionManager`] — auth_key→account HashMap
//! - [`AuthKey`] — `u32` session token (wraps UO auth_key)

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::RwLock;
use std::time::Instant;

use framework::moira::{
    Account, AccountStatus, AccountStore, AccessPolicy, AuthError, Authenticator,
    SessionError, SessionManager, SessionToken,
};

// ── AccessLevel ────────────────────────────────────────────────────────

/// Hierarchical access level, as in classic UO.
///
/// Each level includes all permissions of lower levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessLevel {
    /// Regular player.
    Player      = 0,
    /// Counselor — sees complaints, can assist.
    Counselor   = 1,
    /// Seer — event management.
    Seer        = 2,
    /// Game master — full control over the world.
    GameMaster  = 3,
    /// Administrator — server management.
    Administrator = 4,
    /// Developer — maximum privileges.
    Developer   = 5,
}

impl fmt::Display for AccessLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Player        => write!(f, "Player"),
            Self::Counselor     => write!(f, "Counselor"),
            Self::Seer          => write!(f, "Seer"),
            Self::GameMaster    => write!(f, "GameMaster"),
            Self::Administrator => write!(f, "Administrator"),
            Self::Developer     => write!(f, "Developer"),
        }
    }
}

// ── DemoPermission ─────────────────────────────────────────────────────────

/// Granular permissions for a UO server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Login to the server.
    Login,
    /// Create a character.
    CreateCharacter,
    /// Delete a character.
    DeleteCharacter,
    /// Use GM commands (.tele, .kill, etc.).
    UseGMCommands,
    /// Teleport (.tele).
    Teleport,
    /// Spawn entities (.spawn).
    SpawnEntities,
    /// Ban accounts.
    BanAccounts,
    /// Server management (shutdown, restart).
    ManageServer,
}

// ── Credential ─────────────────────────────────────────────────────────

/// Credentials for UO — plain-text password.
///
/// A real server would use a hash or a (password, OTP) pair here,
/// but the UO protocol transmits the password in plain text (packet 0x80).
#[derive(Debug, Clone)]
pub struct Credential {
    pub password: String,
}

// ── DemoAccount ────────────────────────────────────────────────────────────

/// Concrete account type for a UO server.
#[derive(Debug, Clone)]
pub struct DemoAccount {
    /// Unique account ID.
    pub id: u32,
    /// Username (login).
    pub username: String,
    /// Password hash (or plain-text for demo).
    pub password: String,
    /// Current status.
    pub status: AccountStatus,
    /// Access level.
    pub access_level: AccessLevel,
}

impl Account for DemoAccount {
    type Id = u32;

    fn id(&self) -> &u32 {
        &self.id
    }

    fn username(&self) -> &str {
        &self.username
    }

    fn status(&self) -> AccountStatus {
        self.status
    }
}

// ── DemoAccessPolicy ───────────────────────────────────────────────────────

/// Role-based access policy for UO.
///
/// Mapping `AccessLevel` → set of `Permission` by hierarchy:
/// each level includes all permissions of lower levels.
pub struct DemoAccessPolicy;

impl AccessPolicy for DemoAccessPolicy {
    type Account = DemoAccount;
    type Permission = Permission;

    fn has_permission(&self, account: &DemoAccount, permission: &Permission) -> bool {
        let level = account.access_level;
        match permission {
            Permission::Login           => true, // everyone can log in (status checked separately)
            Permission::CreateCharacter => true,
            Permission::DeleteCharacter => true,
            Permission::UseGMCommands   => level >= AccessLevel::GameMaster,
            Permission::Teleport        => level >= AccessLevel::Seer,
            Permission::SpawnEntities   => level >= AccessLevel::GameMaster,
            Permission::BanAccounts     => level >= AccessLevel::Administrator,
            Permission::ManageServer    => level >= AccessLevel::Administrator,
        }
    }

    fn permissions(&self, account: &DemoAccount) -> Vec<Permission> {
        let all = [
            Permission::Login,
            Permission::CreateCharacter,
            Permission::DeleteCharacter,
            Permission::UseGMCommands,
            Permission::Teleport,
            Permission::SpawnEntities,
            Permission::BanAccounts,
            Permission::ManageServer,
        ];
        all.iter()
            .filter(|p| self.has_permission(account, p))
            .copied()
            .collect()
    }
}

// ── AuthKey (SessionToken) ───────────────────────────────────────────────

/// Session token for UO — wraps the `u32` auth_key used in the
/// login→game server handoff (packet 0x8C / 0x91).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuthKey(pub u32);

impl SessionToken for AuthKey {}

// ── MemoryAccountStore ───────────────────────────────────────────────────

/// In-memory account store.  Thread-safe via `RwLock`.
///
/// When `auto_create` is true (the default for the demo server),
/// unknown accounts are automatically created on first login.
pub struct MemoryAccountStore {
    accounts: RwLock<HashMap<String, DemoAccount>>,
    next_id: AtomicU32,
    /// If true, `find_by_username` will NOT auto-create.
    /// Auto-creation happens only in `PlainAuthenticator`.
    pub auto_create: bool,
    /// Default access level for newly created accounts.
    pub default_access_level: AccessLevel,
}

impl MemoryAccountStore {
    pub fn new() -> Self {
        Self {
            accounts: RwLock::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            auto_create: true,
            default_access_level: AccessLevel::Player,
        }
    }

    /// Insert an account into the store.  Used for pre-seeded accounts.
    #[allow(dead_code)]
    pub fn insert(&self, account: DemoAccount) {
        let mut map = self.accounts.write().unwrap();
        // Ensure next_id stays ahead of inserted IDs.
        let current = self.next_id.load(Ordering::Relaxed);
        if account.id >= current {
            self.next_id.store(account.id + 1, Ordering::Relaxed);
        }
        map.insert(account.username.clone(), account);
    }

    /// Create a new account with the given username and password.
    /// Returns the created account.
    pub fn create(
        &self,
        username: &str,
        password: &str,
        access_level: AccessLevel,
    ) -> DemoAccount {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let account = DemoAccount {
            id,
            username: username.to_string(),
            password: password.to_string(),
            status: AccountStatus::Active,
            access_level,
        };
        self.accounts.write().unwrap().insert(username.to_string(), account.clone());
        account
    }
}

impl AccountStore for MemoryAccountStore {
    type Account = DemoAccount;
    type Error = Infallible;

    fn find_by_username(&self, username: &str) -> Result<Option<DemoAccount>, Infallible> {
        let map = self.accounts.read().unwrap();
        Ok(map.get(username).cloned())
    }

    fn find_by_id(&self, id: &u32) -> Result<Option<DemoAccount>, Infallible> {
        let map = self.accounts.read().unwrap();
        Ok(map.values().find(|a| a.id == *id).cloned())
    }
}

// ── PlainAuthenticator ───────────────────────────────────────────────────

/// Plain-text authenticator for the demo server.
///
/// If the account doesn't exist and the store has `auto_create = true`,
/// a new account is created automatically.  **Not for production use!**
pub struct PlainAuthenticator {
    pub store: std::sync::Arc<MemoryAccountStore>,
    /// Usernames that receive [`AccessLevel::Developer`] on auto-creation.
    /// All other accounts receive the store's `default_access_level`.
    pub admin_usernames: Vec<String>,
}

impl Authenticator for PlainAuthenticator {
    type Account = DemoAccount;
    type Credential = Credential;
    type Error = Infallible;

    fn authenticate(
        &self,
        username: &str,
        credential: &Credential,
    ) -> Result<DemoAccount, AuthError<Infallible>> {
        // Look up existing account.
        let existing = self.store.find_by_username(username).unwrap();

        if let Some(account) = existing {
            // Check status first.
            match account.status {
                AccountStatus::Suspended => return Err(AuthError::AccountSuspended),
                AccountStatus::Banned    => return Err(AuthError::AccountBanned),
                AccountStatus::Inactive  => return Err(AuthError::AccountSuspended),
                AccountStatus::Active    => {}
            }
            // Check password.
            if account.password != credential.password {
                return Err(AuthError::InvalidCredentials);
            }
            Ok(account)
        } else if self.store.auto_create {
            // Auto-create on first login (demo behavior).
            let access_level = if self.admin_usernames.iter().any(|n| n == username) {
                AccessLevel::Developer
            } else {
                self.store.default_access_level
            };
            let account = self.store.create(
                username,
                &credential.password,
                access_level,
            );
            log::info!(
                "auto-created account '{}' (id={}, level={})",
                username, account.id, account.access_level,
            );
            Ok(account)
        } else {
            Err(AuthError::AccountNotFound)
        }
    }
}

// ── SimpleSessionManager ─────────────────────────────────────────────────

/// Entry in the session store.
struct SessionEntry {
    account: DemoAccount,
    created_at: Instant,
}

/// In-memory session manager using `u32` auth keys.
///
/// Generates incrementing auth keys (matching the existing demo-server
/// `auth_counter` pattern) and stores the associated account.
pub struct SimpleSessionManager {
    sessions: RwLock<HashMap<u32, SessionEntry>>,
    counter: AtomicU32,
    /// Session TTL.  After this duration, `validate_session` returns `Expired`.
    /// Default: 60 seconds (UO clients reconnect within a few seconds).
    pub ttl: std::time::Duration,
}

impl SimpleSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            counter: AtomicU32::new(0x1000_0000),
            ttl: std::time::Duration::from_secs(60),
        }
    }
}

impl SessionManager for SimpleSessionManager {
    type Account = DemoAccount;
    type Token = AuthKey;
    type Error = Infallible;

    fn create_session(
        &self,
        account: &DemoAccount,
    ) -> Result<AuthKey, Infallible> {
        let key = self.counter.fetch_add(1, Ordering::Relaxed);
        let entry = SessionEntry {
            account: account.clone(),
            created_at: Instant::now(),
        };
        self.sessions.write().unwrap().insert(key, entry);
        Ok(AuthKey(key))
    }

    fn validate_session(
        &self,
        token: &AuthKey,
    ) -> Result<DemoAccount, SessionError<Infallible>> {
        let map = self.sessions.read().unwrap();
        match map.get(&token.0) {
            None => Err(SessionError::InvalidToken),
            Some(entry) => {
                if entry.created_at.elapsed() > self.ttl {
                    return Err(SessionError::Expired);
                }
                if !entry.account.status().can_login() {
                    return Err(SessionError::AccountSuspended);
                }
                Ok(entry.account.clone())
            }
        }
    }

    fn destroy_session(
        &self,
        token: &AuthKey,
    ) -> Result<(), Infallible> {
        self.sessions.write().unwrap().remove(&token.0);
        Ok(())
    }
}
