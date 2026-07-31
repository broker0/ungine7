//! Re-export from `common::uo_engine::handler`.
//!
//! The engine command handler now lives in `examples/common`.  This module
//! provides backward-compatible type aliases so that existing code using
//! `crate::replay_handler::ReplayCommand` continues to compile.

pub use common::uo_engine::handler::{EngineCommand, EngineHandler, MobileStepResult};

/// Backward-compatible alias — new code should use [`EngineCommand`].
pub type ReplayCommand = EngineCommand;

/// Backward-compatible alias — new code should use [`EngineHandler`].
pub type ReplayHandler = EngineHandler;
