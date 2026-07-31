//! File watcher, hot-reload manager, and command handler for Lua scripts.
//!
//! `ScriptManager` owns the current script task and handles
//! start/restart/stop.  It accepts commands via a [`tokio::sync::mpsc`]
//! channel so that dot-commands, the CLI, and WebSocket handlers can all
//! control it.

use std::path::PathBuf;
use std::time::Duration;

use log::{error, info, warn};
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::backend::ScriptingBackend;
use super::runtime;

// ── Commands ──────────────────────────────────────────────────────────────

/// Commands that can be sent to the script manager.
pub enum ScriptCommand {
    /// Load and run a script from a file (stops any current script).
    /// Also starts a file watcher for hot-reload.
    RunFile(PathBuf),
    /// Run Lua source code directly (e.g. from WebSocket eval).
    /// No file watcher.
    RunCode {
        code: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Reload the current file-based script from disk.
    Reload,
    /// Stop the current script and file watcher.
    Stop,
}

// ── Callbacks ─────────────────────────────────────────────────────────────

/// Optional hooks for application-specific lifecycle events.
///
/// The script manager calls these at the appropriate points.
/// All callbacks are called from the manager's async task.
pub trait ManagerCallbacks: Send + 'static {
    /// Called just before a new script is spawned.
    /// Can be used to take an allocator snapshot.
    fn on_before_spawn(&mut self) {}

    /// Called after a script task has been stopped and awaited.
    /// Can be used to clean up entities, free serials, etc.
    /// This is an async method because cleanup may need to send
    /// commands to the game engine.
    fn on_after_stop(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

/// No-op callbacks for backends that don't need lifecycle hooks.
pub struct NoCallbacks;
impl ManagerCallbacks for NoCallbacks {}

// ── ScriptManager ─────────────────────────────────────────────────────────

/// Manages the lifecycle of a Lua script task and its file watcher.
///
/// Generic over:
/// - `B`: the [`ScriptingBackend`] that creates World objects and converts events.
/// - `C`: the [`ManagerCallbacks`] for application-specific hooks.
struct ScriptManager<B: ScriptingBackend, C: ManagerCallbacks> {
    backend: B,
    event_tx: broadcast::Sender<B::Event>,
    self_tx: mpsc::Sender<ScriptCommand>,
    scripts_dir: Option<PathBuf>,
    script_path: Option<PathBuf>,
    current_task: Option<(JoinHandle<()>, CancellationToken)>,
    watcher_task: Option<JoinHandle<()>>,
    callbacks: C,
}

impl<B: ScriptingBackend, C: ManagerCallbacks> ScriptManager<B, C> {
    fn new(
        backend: B,
        event_tx: broadcast::Sender<B::Event>,
        self_tx: mpsc::Sender<ScriptCommand>,
        scripts_dir: Option<PathBuf>,
        callbacks: C,
    ) -> Self {
        Self {
            backend,
            event_tx,
            self_tx,
            scripts_dir,
            script_path: None,
            current_task: None,
            watcher_task: None,
            callbacks,
        }
    }

    /// Start or restart the Lua script from the given file path.
    async fn run_file(&mut self, path: PathBuf) {
        self.stop_current_task().await;
        self.stop_watcher();

        info!("[{}] loading script: {}", self.backend.log_prefix(), path.display());
        self.script_path = Some(path);
        self.spawn_file_script();
        self.start_watcher();
    }

    /// Run Lua source code directly (no file watcher).
    async fn run_code(&mut self, code: String) -> Result<(), String> {
        self.stop_current_task().await;
        self.stop_watcher();
        self.script_path = None;

        info!(
            "[{}] running eval code ({} bytes)",
            self.backend.log_prefix(),
            code.len()
        );
        self.spawn_code_script(code);
        Ok(())
    }

    /// Reload the current file-based script from disk.
    async fn reload_script(&mut self) {
        if self.script_path.is_some() {
            info!("[{}] reloading script", self.backend.log_prefix());
            self.stop_current_task().await;
            self.spawn_file_script();
        } else {
            warn!("[{}] no script loaded, nothing to reload", self.backend.log_prefix());
        }
    }

    /// Stop the current script and watcher.
    async fn stop_script(&mut self) {
        self.stop_current_task().await;
        self.stop_watcher();
        if self.script_path.take().is_some() {
            info!("[{}] script stopped", self.backend.log_prefix());
        }
    }

    /// Cancel the running script task, wait for it to finish, then
    /// run application-specific cleanup.
    async fn stop_current_task(&mut self) {
        if let Some((handle, cancel)) = self.current_task.take() {
            cancel.cancel();
            let _ = handle.await;
            self.callbacks.on_after_stop().await;
        }
    }

    fn stop_watcher(&mut self) {
        if let Some(handle) = self.watcher_task.take() {
            handle.abort();
        }
    }

    fn spawn_file_script(&mut self) {
        let Some(path) = &self.script_path else { return };

        self.callbacks.on_before_spawn();

        let cancel = CancellationToken::new();
        let script_path = path.clone();
        let backend = self.backend.clone();
        let event_rx = self.event_tx.subscribe();
        let cancel2 = cancel.clone();
        let scripts_dir = self.scripts_dir.clone();
        let prefix = backend.log_prefix().to_string();

        let handle = tokio::spawn(async move {
            if let Err(e) = runtime::run_lua_script_file(
                &script_path,
                &backend,
                event_rx,
                cancel2,
                scripts_dir.as_deref(),
            ).await {
                error!("[{}] script error: {}", prefix, e);
            }
        });

        self.current_task = Some((handle, cancel));
    }

    fn spawn_code_script(&mut self, code: String) {
        self.callbacks.on_before_spawn();

        let cancel = CancellationToken::new();
        let backend = self.backend.clone();
        let event_rx = self.event_tx.subscribe();
        let cancel2 = cancel.clone();
        let scripts_dir = self.scripts_dir.clone();
        let prefix = backend.log_prefix().to_string();

        let handle = tokio::spawn(async move {
            if let Err(e) = runtime::run_lua_source(
                &code,
                "ws-eval",
                &backend,
                event_rx,
                cancel2,
                scripts_dir.as_deref(),
            ).await {
                error!("[{}] eval error: {}", prefix, e);
            }
        });

        self.current_task = Some((handle, cancel));
    }

    fn start_watcher(&mut self) {
        let Some(path) = self.script_path.clone() else { return };
        let watch_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let (notify_tx, mut notify_rx) = mpsc::channel::<()>(16);
        let cmd_tx = self.self_tx.clone();
        let prefix = self.backend.log_prefix().to_string();

        let watcher_handle = tokio::spawn(async move {
            let _watcher = match notify::recommended_watcher(
                move |res: Result<notify::Event, notify::Error>| {
                    if let Ok(event) = res {
                        if matches!(
                            event.kind,
                            EventKind::Modify(_) | EventKind::Create(_)
                        ) {
                            let _ = notify_tx.blocking_send(());
                        }
                    }
                },
            ) {
                Ok(mut w) => {
                    if let Err(e) = w.watch(&watch_dir, RecursiveMode::Recursive) {
                        error!(
                            "[{}] failed to watch {}: {}",
                            prefix,
                            watch_dir.display(),
                            e
                        );
                        return;
                    }
                    info!(
                        "[{}] watching {} for changes",
                        prefix,
                        watch_dir.display()
                    );
                    w
                }
                Err(e) => {
                    error!("[{}] failed to create file watcher: {}", prefix, e);
                    return;
                }
            };

            // Debounced reload loop.
            loop {
                if notify_rx.recv().await.is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                while notify_rx.try_recv().is_ok() {}

                info!("[{}] file changed, reloading", prefix);
                if cmd_tx.send(ScriptCommand::Reload).await.is_err() {
                    break;
                }
            }
        });

        self.watcher_task = Some(watcher_handle);
    }
}

impl<B: ScriptingBackend, C: ManagerCallbacks> Drop for ScriptManager<B, C> {
    fn drop(&mut self) {
        if let Some((handle, cancel)) = self.current_task.take() {
            cancel.cancel();
            handle.abort();
        }
        self.stop_watcher();
    }
}

// ── Command loop ──────────────────────────────────────────────────────────

/// Run the script manager command loop.
///
/// Receives [`ScriptCommand`]s and manages the script lifecycle accordingly.
/// Optionally starts with an initial script if `initial_script` is provided.
pub async fn run_script_manager<B, C>(
    backend: B,
    event_tx: broadcast::Sender<B::Event>,
    mut cmd_rx: mpsc::Receiver<ScriptCommand>,
    initial_script: Option<PathBuf>,
    scripts_dir: Option<PathBuf>,
    callbacks: C,
)
where
    B: ScriptingBackend,
    C: ManagerCallbacks,
{
    let (self_tx, mut internal_rx) = mpsc::channel::<ScriptCommand>(16);
    let mut manager = ScriptManager::new(backend, event_tx, self_tx, scripts_dir, callbacks);

    if let Some(path) = initial_script {
        manager.run_file(path).await;
    }

    loop {
        tokio::select! {
            biased;
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(ScriptCommand::RunFile(path)) => manager.run_file(path).await,
                    Some(ScriptCommand::RunCode { code, reply }) => {
                        let result = manager.run_code(code).await;
                        let _ = reply.send(result);
                    }
                    Some(ScriptCommand::Reload) => manager.reload_script().await,
                    Some(ScriptCommand::Stop) => manager.stop_script().await,
                    None => break,
                }
            }
            cmd = internal_rx.recv() => {
                match cmd {
                    Some(ScriptCommand::Reload) => manager.reload_script().await,
                    Some(ScriptCommand::RunFile(path)) => manager.run_file(path).await,
                    Some(ScriptCommand::RunCode { code, reply }) => {
                        let result = manager.run_code(code).await;
                        let _ = reply.send(result);
                    }
                    Some(ScriptCommand::Stop) => manager.stop_script().await,
                    None => {}
                }
            }
        }
    }
}
