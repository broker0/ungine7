//! Shared parameter parsing for Lua script bindings.
//!
//! Re-exports the common parameter types from [`framework::mitos::types`]
//! so that existing code using `super::params::EffectParams` etc.
//! continues to work.

pub(crate) use framework::mitos::types::{EffectParams, AnimateOpts, SayOpts};
