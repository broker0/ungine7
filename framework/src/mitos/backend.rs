//! Scripting backend trait.
//!
//! [`ScriptingBackend`] abstracts the differences between rpc-proxy and
//! demo-server so that the shared runtime (`runtime.rs`) can set up
//! the Lua VM, register base globals, and run scripts without knowing
//! the concrete event type or world context.

use mlua::prelude::*;

/// Backend abstraction for the async Lua scripting runtime.
///
/// Each application (rpc-proxy, demo-server) provides its own
/// implementation of this trait.  The runtime uses it to:
///
/// 1. Convert events to Lua values via [`event_to_lua()`](Self::event_to_lua).
/// 2. Create the root `World()` object for scripts.
/// 3. Run application-specific cleanup after a script finishes.
pub trait ScriptingBackend: Clone + Send + Sync + 'static {
    /// The event type delivered to Lua scripts via `poll_event()` /
    /// `wait_event()`.
    type Event: Clone + Send + 'static;

    /// Convert an event to a Lua value.
    ///
    /// Called by `poll_event()` and `wait_event()` when delivering an
    /// event to the script.  The implementation should create a Lua
    /// table (or other value) representing the event.
    fn event_to_lua(&self, lua: &Lua, event: &Self::Event) -> LuaResult<LuaValue>;

    /// Create the root `World(...)` Lua constructor function.
    ///
    /// The returned Lua function will be registered as the global `World`.
    /// It receives whatever arguments the script passes (e.g. `World(0)`
    /// for map_id on the server, or `World()` on the proxy).
    fn create_world_constructor(&self, lua: &Lua) -> LuaResult<LuaFunction>;

    /// Optional: provide a log prefix for messages from this backend.
    /// Defaults to `"lua"`.
    fn log_prefix(&self) -> &str {
        "lua"
    }

    /// Optional: called after a script finishes (both success and error).
    /// Can be used for application-specific cleanup.
    fn on_script_finished(&self) {}
}
