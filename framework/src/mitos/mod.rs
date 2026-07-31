//! Lua scripting infrastructure — **mitos**.
//!
//! Provides shared runtime, event buffering, script lifecycle management,
//! and common parameter types for Lua scripting across different backends
//! (rpc-proxy client, demo-server async, demo-server controller).
//!
//! # Architecture
//!
//! The module is split into layers:
//!
//! - **`backend`** — [`ScriptingBackend`] trait that backends implement.
//! - **`runtime`** — Shared async Lua VM setup, base globals, script
//!   execution with cancellation.
//! - **`manager`** — [`ScriptManager`](manager::run_script_manager) for
//!   script lifecycle, hot-reload via file watcher, command handling.
//! - **`event_buffer`** — [`BroadcastEventBuffer`]
//!   for async mode, [`SimpleEventBuffer`]
//!   for controller mode.
//! - **`types`** — Shared parameter types ([`EffectParams`],
//!   [`AnimateOpts`], [`SayOpts`]).
//!
//! # Usage
//!
//! Each backend (rpc-proxy, demo-server) implements [`ScriptingBackend`] and
//! defines its own `World`, `Mobile`, `Item` userdata types.  The framework
//! provides the runtime infrastructure; the backend provides the game logic.
//!
//! Enabled via the `lua` cargo feature.

pub mod backend;
pub mod event_buffer;
pub mod manager;
pub mod runtime;
pub mod types;

// Re-exports for convenience.
pub use backend::ScriptingBackend;
pub use event_buffer::{BroadcastEventBuffer, SimpleEventBuffer, SpatialFilter, EventPosition};
pub use manager::{ScriptCommand, ManagerCallbacks, NoCallbacks, run_script_manager};
pub use runtime::{run_lua_script_file, run_lua_source};
pub use types::{EffectParams, AnimateOpts, SayOpts};
