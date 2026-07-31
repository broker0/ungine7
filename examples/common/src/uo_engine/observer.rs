//! Backward-compatibility re-export.
//!
//! The `ObserverRegistry` has been moved into the `framework` crate at
//! [`framework::continuum::observer`].  This module re-exports the key
//! types so existing `use super::observer::*` imports keep working.

pub use framework::continuum::observer::{ObserverRegistry, ObserverId};

/// Legacy alias — use [`ObserverId`] instead.
pub type SessionId = ObserverId;
