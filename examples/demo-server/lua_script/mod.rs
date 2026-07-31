//! Lua scripting subsystem for the demo server.
//!
//! Provides two modes of Lua script execution:
//!
//! ## Async mode (original)
//!
//! Scripts run in their own [`tokio::spawn`]ed tasks and communicate with
//! the game world via RPC over [`DemoWorkerTx`](crate::DemoWorkerTx).
//! Managed by [`LuaScriptManager`](watcher) with hot-reload support.
//! Suitable for standalone scripts, WebSocket bridges, and tools that
//! need true async I/O.
//!
//! ### Globals
//!
//! | Global | Description |
//! |---|---|
//! | `World(map_id)` | Create a world context object |
//! | `sleep(ms)` | Async sleep |
//! | `log(msg)` | Logging |
//! | `clock()` | Monotonic time in seconds |
//! | `poll_event()` | Non-blocking event poll |
//! | `wait_event(timeout_ms)` | Blocking event wait |
//! | `set_event_filter(x1,y1,x2,y2)` | Restrict events to a tile rectangle |
//! | `clear_event_filter()` | Receive all events again (default) |
//!
//! ## Controller mode (new)
//!
//! Scripts run as Lua coroutines inside [`LuaController`], which
//! implements [`EntityController`](framework::anima::EntityController).
//! All world access is synchronous through [`ControlContext`](framework::anima::ControlContext)
//! — no channels or RPC.  The script participates fully in the anima
//! framework: access levels, scheduler timers, event routing, commands.
//!
//! ## Architecture
//!
//! The shared runtime infrastructure (VM setup, base globals, event
//! buffering, script lifecycle, hot-reload) is provided by
//! [`framework::mitos`].  This module supplies the demo-server specific
//! `World` userdata, RPC wrappers, and entity conversions.
//!
//! Enabled via the `lua` cargo feature.

pub mod lua_controller;
pub(crate) mod params;
pub(crate) mod runtime;
mod watcher;

pub use lua_controller::LuaController;
pub use watcher::{LuaCommand, run_lua_manager};
