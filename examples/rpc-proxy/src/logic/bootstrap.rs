//! Re-exports [`generate_bootstrap`] from [`framework::diorama::bootstrap`].
//!
//! The bootstrap logic has been moved to the `framework` crate so it can be
//! shared across examples (rpc-proxy, demo-server, etc.).  This module
//! preserves the original import path for backward compatibility.

pub use framework::diorama::bootstrap::generate_bootstrap;
