//! Lua script runtime: VM setup, base globals, and async execution.
//!
//! This module provides the shared runtime that initialises a Lua VM,
//! registers base globals (`sleep`, `log`, `clock`, `poll_event`,
//! `wait_event`, `register_cleanup`), and executes a script with
//! cancellation support.
//!
//! The backend-specific `World()` constructor and event conversion
//! are provided by the [`ScriptingBackend`] trait.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{error, info, warn};
use mlua::prelude::*;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::backend::ScriptingBackend;
use super::event_buffer::BroadcastEventBuffer;

// ── Public API ────────────────────────────────────────────────────────────

/// Run a Lua script from a file path.
///
/// Reads the file, creates a fresh Lua VM, registers all globals
/// (both base globals and the backend's `World()` constructor), and
/// executes the script.  Returns when the script finishes or the
/// `cancel` token is triggered.
pub async fn run_lua_script_file<B: ScriptingBackend>(
    path: &Path,
    backend: &B,
    event_rx: broadcast::Receiver<B::Event>,
    cancel: CancellationToken,
    scripts_dir: Option<&Path>,
) -> Result<(), LuaError> {
    let script_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());

    info!("[{}:{}] starting script", backend.log_prefix(), script_name);

    let source = std::fs::read_to_string(path)
        .map_err(|e| LuaError::external(format!("failed to read {}: {e}", path.display())))?;

    run_lua_source(&source, &script_name, backend, event_rx, cancel, scripts_dir).await
}

/// Run Lua source code from a string.
///
/// `name` is used for error messages (e.g. file path or "ws-eval").
pub async fn run_lua_source<B: ScriptingBackend>(
    source: &str,
    name: &str,
    backend: &B,
    event_rx: broadcast::Receiver<B::Event>,
    cancel: CancellationToken,
    scripts_dir: Option<&Path>,
) -> Result<(), LuaError> {
    let prefix = backend.log_prefix();
    let lua = Lua::new();

    // Set up package.path for require() if scripts_dir is provided.
    if let Some(sd) = scripts_dir {
        let sd_str = sd.to_string_lossy();
        let sd_lua = sd_str.replace('\\', "/");
        lua.load(format!(
            r#"
            package.path = "{0}/?.lua;{0}/?/init.lua;" .. package.path
            SCRIPTS_DIR = "{0}"
            "#,
            sd_lua,
        )).exec().map_err(|e| {
            LuaError::external(format!("failed to set package.path: {e}"))
        })?;
    }

    // Register globals.
    register_globals(&lua, name, backend, event_rx, cancel.clone())?;

    // Run the script.
    let chunk = lua.load(source).set_name(name);

    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            info!("[{}:{}] cancelled", prefix, name);
            // On cancel, skip cleanup hooks — the VM may be in an
            // inconsistent state.
            backend.on_script_finished();
            return Ok(());
        }
        r = chunk.exec_async() => r,
    };

    // Run registered cleanup hooks.
    run_cleanup_hooks(&lua, prefix, name).await;

    backend.on_script_finished();

    match &result {
        Ok(()) => info!("[{}:{}] script finished", prefix, name),
        Err(e) => error!("[{}:{}] script error: {}", prefix, name, e),
    }

    result
}

// ── Globals registration ──────────────────────────────────────────────────

/// Type-erased event converter stored in Lua app_data.
///
/// Wraps the backend's `event_to_lua` in a type-erased closure so that
/// the Lua closures for `poll_event` / `wait_event` don't need to be
/// generic over `B`.
struct EventConverter<B: ScriptingBackend> {
    backend: B,
    state: Mutex<BroadcastEventBuffer<B::Event>>,
}

fn register_globals<B: ScriptingBackend>(
    lua: &Lua,
    script_name: &str,
    backend: &B,
    event_rx: broadcast::Receiver<B::Event>,
    cancel: CancellationToken,
) -> LuaResult<()> {
    let globals = lua.globals();

    // ── Cleanup hook registry ─────────────────────────────────────────
    lua.load(r#"
        _cleanup_hooks = {}
        function register_cleanup(fn)
            table.insert(_cleanup_hooks, fn)
        end
    "#).exec()?;

    // ── World() constructor (backend-specific) ────────────────────────
    let world_ctor = backend.create_world_constructor(lua)?;
    globals.set("World", world_ctor)?;

    // ── sleep(ms) — async cancellable sleep ───────────────────────────
    let cancel_sleep = cancel.clone();
    let sleep_fn = lua.create_async_function(move |_, ms: u64| {
        let cancel = cancel_sleep.clone();
        async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    Err(LuaError::external("script cancelled"))
                }
                _ = tokio::time::sleep(Duration::from_millis(ms)) => {
                    Ok(())
                }
            }
        }
    })?;
    globals.set("sleep", sleep_fn)?;

    // ── log(msg) ──────────────────────────────────────────────────────
    let prefix = backend.log_prefix().to_string();
    let name = script_name.to_string();
    let log_fn = lua.create_function(move |_, msg: String| {
        info!("[{}:{}] {}", prefix, name, msg);
        Ok(())
    })?;
    globals.set("log", log_fn)?;

    // ── clock() → number — monotonic time in seconds ──────────────────
    let epoch = tokio::time::Instant::now();
    let clock_fn = lua.create_function(move |_, ()| {
        Ok(epoch.elapsed().as_secs_f64())
    })?;
    globals.set("clock", clock_fn)?;

    // ── Event handling ────────────────────────────────────────────────
    //
    // We wrap the backend + event buffer in an Arc so that the Lua
    // closures can call backend.event_to_lua() for conversion.
    let converter = Arc::new(EventConverter {
        backend: backend.clone(),
        state: Mutex::new(BroadcastEventBuffer::new(event_rx)),
    });

    // poll_event() → table | nil (non-blocking)
    {
        let conv = converter.clone();
        let poll_fn = lua.create_function(move |lua, ()| {
            let mut state = conv.state.lock().map_err(|e| LuaError::external(e.to_string()))?;
            match state.try_recv() {
                Some(event) => conv.backend.event_to_lua(&lua, &event),
                None => Ok(LuaValue::Nil),
            }
        })?;
        globals.set("poll_event", poll_fn)?;
    }

    // wait_event(timeout_ms) → table | nil (async blocking)
    {
        let conv = converter.clone();
        let cancel_wait = cancel.clone();
        let wait_fn = lua.create_async_function(move |lua, timeout_ms: u64| {
            let conv = conv.clone();
            let cancel = cancel_wait.clone();
            async move {
                let deadline =
                    tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

                // Check buffer first.
                {
                    let mut s = conv.state
                        .lock()
                        .map_err(|e| LuaError::external(e.to_string()))?;
                    if let Some(event) = s.try_recv() {
                        return conv.backend.event_to_lua(&lua, &event);
                    }
                }

                // Poll loop with 50ms granularity.
                loop {
                    let remaining =
                        deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        return Ok(LuaValue::Nil);
                    }

                    {
                        let mut s = conv.state
                            .lock()
                            .map_err(|e| LuaError::external(e.to_string()))?;
                        s.drain_broadcast();
                        if let Some(event) = s.pop() {
                            return conv.backend.event_to_lua(&lua, &event);
                        }
                    }

                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            return Err(LuaError::external("script cancelled"));
                        }
                        _ = tokio::time::sleep_until(
                            deadline.min(
                                tokio::time::Instant::now() + Duration::from_millis(50)
                            )
                        ) => {}
                    }
                }
            }
        })?;
        globals.set("wait_event", wait_fn)?;
    }

    // set_event_filter(x1, y1, x2, y2) — limit events to a tile rectangle.
    {
        let conv = converter.clone();
        let filter_fn = lua.create_function(move |_, (x1, y1, x2, y2): (u16, u16, u16, u16)| {
            let mut s = conv.state.lock().map_err(|e| LuaError::external(e.to_string()))?;
            s.set_filter(super::event_buffer::SpatialFilter {
                x_min: x1,
                y_min: y1,
                x_max: x2,
                y_max: y2,
            });
            Ok(())
        })?;
        globals.set("set_event_filter", filter_fn)?;
    }

    // clear_event_filter() — remove the spatial filter.
    {
        let conv = converter.clone();
        let clear_fn = lua.create_function(move |_, ()| {
            let mut s = conv.state.lock().map_err(|e| LuaError::external(e.to_string()))?;
            s.clear_filter();
            Ok(())
        })?;
        globals.set("clear_event_filter", clear_fn)?;
    }

    Ok(())
}

// ── Cleanup hooks ────────────────────────────────────────────────────────

async fn run_cleanup_hooks(lua: &Lua, prefix: &str, name: &str) {
    if let Err(e) = lua.load(r#"
        for _, fn in ipairs(_cleanup_hooks or {}) do
            pcall(fn)
        end
    "#).exec_async().await {
        warn!("[{}:{}] error running cleanup hooks: {}", prefix, name, e);
    }
}
