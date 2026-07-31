//! # Moira — Identity & Access.
//!
//! Module for framework authorization and authentication. Named after the
//! Moirai (Μοῖραι) — Greek goddesses of fate who determine everyone's portion.
//!
//! `moira` provides **only abstractions** (traits), leaving the concrete
//! implementation up to the consumer — as with all other framework modules.
//!
//! ## Architecture
//!
//! The module is divided into four responsibilities:
//!
//! | Component | Question | Trait |
//! |-----------|----------|-------|
//! | **account** | Who are you? (identity) | [`Account`] |
//! | **auth** | Prove it (authentication) | [`Authenticator`] |
//! | **session** | Thread of fate (session between phases) | [`SessionManager`], [`SessionToken`] |
//! | **access** | What are you allowed? (authorization) | [`AccessPolicy`] |
//! | **store** | Where are you recorded? (storage) | [`AccountStore`] |
//!
//! ## Sync by design
//!
//! All traits are synchronous — like `CommandHandler`, `EntityController`
//! and other framework traits. Asynchronicity is added at the worker level:
//! the implementation can use an in-memory cache,
//! `spawn_blocking` or RPC via channels.

pub mod account;
pub mod auth;
pub mod session;
pub mod access;
pub mod store;

// ── Re-exports ───────────────────────────────────────────────────────────

pub use account::{Account, AccountStatus};
pub use auth::{AuthError, Authenticator};
pub use session::{SessionError, SessionManager, SessionToken};
pub use access::AccessPolicy;
pub use store::{AccountStore, AccountStoreError};
