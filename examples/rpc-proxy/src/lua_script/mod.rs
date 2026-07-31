//! Lua scripting subsystem for rpc-proxy.
//!
//! Provides async-mode Lua script execution with two launch mechanisms:
//!
//! ## Local mode (file + hot-reload)
//!
//! Scripts are loaded from disk and executed in their own [`tokio::spawn`]ed
//! tasks.  A file watcher (via [`notify`]) monitors the script directory
//! and automatically reloads on change.  Initiated via `--lua-script` CLI
//! argument or the `.lua` dot-command.
//!
//! ## WebSocket mode (eval)
//!
//! External clients send Lua source code over WebSocket.  The code is
//! executed in the same async runtime with the same `World` API.
//! No file watcher — the external client manages the lifecycle.
//!
//! ## Architecture
//!
//! The shared runtime infrastructure (VM setup, base globals, event
//! buffering, script lifecycle, hot-reload) is provided by
//! [`framework::mitos`].  This module supplies only the proxy-specific
//! `World` userdata and RPC helpers.
//!
//! Enabled via the `lua` cargo feature.

mod runtime;
mod watcher;

pub use watcher::{LuaCommand, run_lua_manager};
