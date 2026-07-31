//! Script manager adapter for rpc-proxy.
//!
//! Delegates lifecycle management to [`framework::mitos::run_script_manager`],
//! wrapping the proxy's [`ProxyBackend`].

use std::path::PathBuf;

use tokio::sync::{broadcast, mpsc};

use framework::diorama::ObserverEvent;
use framework::mitos;

use crate::session::commands::ClientCommand;

use super::runtime::ProxyBackend;

// ── Re-export command type ────────────────────────────────────────────────

/// Commands that can be sent to the Lua script manager.
///
/// This is a re-export of [`mitos::ScriptCommand`] for backward
/// compatibility with existing dot-command and WS handler code.
pub type LuaCommand = mitos::ScriptCommand;

// ── Command loop ──────────────────────────────────────────────────────────

/// Run the Lua script manager command loop.
///
/// Wraps [`framework::mitos::run_script_manager`] with the proxy backend.
pub async fn run_lua_manager(
    command_tx: mpsc::Sender<ClientCommand>,
    event_tx: broadcast::Sender<ObserverEvent>,
    cmd_rx: mpsc::Receiver<LuaCommand>,
    initial_script: Option<PathBuf>,
) {
    let backend = ProxyBackend { command_tx };

    mitos::run_script_manager(
        backend,
        event_tx,
        cmd_rx,
        initial_script,
        None, // no scripts_dir for proxy
        mitos::NoCallbacks,
    ).await;
}
