//! Dot-command dispatcher for rpc-proxy.
//!
//! Handles three kinds of player interactions:
//!
//! - **Dot-commands** — player types `.command` in chat.
//! - **Target cursor responses** — player clicks on an object/tile after a
//!   target request issued by a dot-command.
//! - **Gump responses** — player clicks a button in a command gump.
//!
//! # Architecture
//!
//! [`DotCommands`] is a per-client state machine.  A single method
//! [`DotCommands::handle_packet`] is called for every incoming C→S packet
//! in [`dispatch_c2s`](super::headless).
//!
//! # Adding a new command
//!
//! - **Immediate**: add a branch in `DotCommands::dispatch_text`.
//! - **Targeted**: add a variant to `PendingTarget`, a branch in
//!   `dispatch_text` to send the cursor, and a branch in
//!   `handle_target_response` to process the reply.
//! - **Gump**: add a variant to `GumpKind`, use `DotCommands::send_gump`
//!   to show it, and add a branch in `handle_gump_response`.

use std::collections::HashMap;
#[cfg(feature = "lua")]
use std::path::PathBuf;

use log::{debug, info};
use tokio::sync::mpsc;
use u_core::position::Heading;
use common::dot_commands::{
    self as common_cmd, CMD_CURSOR_BASE, CMD_GUMP_BASE,
};
use framework::diorama::ObserverPipeline;
use packets::gump::{GumpMenuSelection, GumpTextLine};
use packets::interaction::TargetCursor;
use packets::traits::{ManualPacket, BasicPacket};
use protocol::RawPacket;

// ── Cursor IDs ────────────────────────────────────────────────────────────

/// Cursor ID for `.inspect`.
const INSPECT_CURSOR: u32 = CMD_CURSOR_BASE | 0x10;

/// Cursor ID for `.step`.
const STEP_CURSOR: u32 = CMD_CURSOR_BASE | 0x11;

/// Cursor ID for `.mstep` (multi-step).
const MSTEP_CURSOR: u32 = CMD_CURSOR_BASE | 0x12;

/// Cursor ID for `.raw_step`.
const RAW_STEP_CURSOR: u32 = CMD_CURSOR_BASE | 0x13;

// ── Public result type ────────────────────────────────────────────────────

/// Result of [`DotCommands::handle_packet`].
pub enum Handled {
    /// The packet was consumed — the caller should skip normal processing.
    Yes,
    /// Not a command packet — the caller should process it normally.
    No,
    /// The user picked a tile and wants to step toward it (validated).
    Step { heading: Heading },
    /// The user picked a tile and wants to step toward it (no validation).
    RawStep { heading: Heading },
}

// ── Pending target variants ───────────────────────────────────────────────

/// Which command is waiting for a target-cursor response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingTarget {
    /// `.inspect` — show information about the targeted object.
    Inspect,
    /// `.step` — step in the direction of the targeted tile.
    Step,
    /// `.mstep` — step repeatedly until explicitly cancelled.
    MultiStep,
    /// `.raw_step` — step without passability validation.
    RawStep,
}

impl PendingTarget {
    fn cursor_id(self) -> u32 {
        match self {
            Self::Inspect => INSPECT_CURSOR,
            Self::Step => STEP_CURSOR,
            Self::MultiStep => MSTEP_CURSOR,
            Self::RawStep => RAW_STEP_CURSOR,
        }
    }
}

// ── Gump variants ─────────────────────────────────────────────────────────

/// Registered gump types that the command system knows how to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GumpKind {
    /// `.menu` — action menu with available commands.
    ActionMenu,
}

impl GumpKind {
    fn gump_id(self) -> u32 {
        CMD_GUMP_BASE
            | match self {
                Self::ActionMenu => 0x702,
            }
    }
}

// ── DotCommands ───────────────────────────────────────────────────────────

/// Per-client dot-command dispatcher and interaction state machine.
pub struct DotCommands {
    /// Currently pending target-cursor request, if any.
    pending_target: Option<PendingTarget>,
    /// Active gumps by gump_id -> kind.
    active_gumps: HashMap<u32, GumpKind>,
    /// Lua script manager command channel (if lua feature enabled).
    #[cfg(feature = "lua")]
    lua_cmd_tx: Option<mpsc::Sender<crate::lua_script::LuaCommand>>,
}

impl DotCommands {
    pub fn new() -> Self {
        Self {
            pending_target: None,
            active_gumps: HashMap::new(),
            #[cfg(feature = "lua")]
            lua_cmd_tx: None,
        }
    }

    /// Set the Lua command sender (called when the session has a Lua manager).
    #[cfg(feature = "lua")]
    pub fn set_lua_cmd_tx(&mut self, tx: mpsc::Sender<crate::lua_script::LuaCommand>) {
        self.lua_cmd_tx = Some(tx);
    }

    // ── Main entry point ──────────────────────────────────────────────

    /// Call this for every incoming C→S packet.
    ///
    /// - [`Handled::Yes`] — packet consumed, caller should skip it.
    /// - [`Handled::No`] — not ours, caller processes normally.
    /// - [`Handled::Step`] — caller should execute a bot_step.
    pub async fn handle_packet(
        &mut self,
        packet: &RawPacket,
        sink: &mpsc::Sender<RawPacket>,
        observer: &ObserverPipeline,
    ) -> Handled {
        // ── Speech -> dispatch dot-command ────────────────────────────
        if let Some(text) = common_cmd::extract_speech_text(packet) {
            if let Some(cmd) = text.strip_prefix('.') {
                return self.dispatch_text(cmd, sink, observer).await;
            }
        }

        // ── TargetCursor response ────────────────────────────────────
        if packet.id() == TargetCursor::ID {
            if let Some(pending) = self.pending_target.take() {
                if let Ok(tc) = TargetCursor::from_bytes(&packet.data) {
                    if tc.cursor_id == pending.cursor_id() {
                        let reshow_menu = pending != PendingTarget::MultiStep;
                        let result = self
                            .handle_target_response(pending, &tc, sink, observer)
                            .await;
                        // Re-show action menu after single-shot targeted actions.
                        // MultiStep manages its own cursor chain (re-shows menu
                        // only on cancel, inside handle_target_response).
                        if reshow_menu {
                            self.send_action_menu_gump(observer.pos.serial, sink).await;
                        }
                        return result;
                    }
                }
                // Not our cursor — restore pending and fall through.
                self.pending_target = Some(pending);
            }
        }

        // ── GumpMenuSelection response ───────────────────────────────
        if packet.id() == GumpMenuSelection::ID {
            if let Ok(resp) = GumpMenuSelection::from_bytes(&packet.data) {
                if let Some(kind) = self.active_gumps.remove(&resp.gump_id) {
                    return self
                        .handle_gump_response(kind, &resp, sink, observer)
                        .await;
                }
            }
        }

        Handled::No
    }

    /// Whether the given cursor_id belongs to our reserved range.
    #[allow(dead_code)]
    fn is_our_cursor(cursor_id: u32) -> bool {
        cursor_id & 0xFFFF_F000 == CMD_CURSOR_BASE
    }

    // ── Text dispatch ─────────────────────────────────────────────────

    async fn dispatch_text(
        &mut self,
        cmd: &str,
        sink: &mpsc::Sender<RawPacket>,
        observer: &ObserverPipeline,
    ) -> Handled {
        let (name, _args) = cmd.split_once(' ').unwrap_or((cmd, ""));

        match name.to_ascii_lowercase().as_str() {
            // ── Immediate ─────────────────────────────────────────────
            "info" => {
                let msg = format!(
                    "Visible: {}, World: {}, Pos: ({},{},{}), Range: {}",
                    observer.session.visible.len(),
                    observer.session.current_world,
                    observer.pos.x,
                    observer.pos.y,
                    observer.pos.z,
                    observer.session.view_range(),
                );
                send_system_message(sink, &msg).await;
            }

            "where" => {
                let msg = format!(
                    "Position: ({},{},{}) facing={}",
                    observer.pos.x, observer.pos.y, observer.pos.z, observer.pos.facing,
                );
                send_system_message(sink, &msg).await;
            }

            // ── Targeted ──────────────────────────────────────────────
            "inspect" => {
                info!("[cmd] .inspect — sending target cursor");
                self.send_target(PendingTarget::Inspect, 0, sink).await;
            }

            "step" => {
                info!("[cmd] .step — sending target cursor (ground)");
                self.send_target(PendingTarget::Step, 1, sink).await;
            }

            "mstep" => {
                info!("[cmd] .mstep — sending target cursor (ground, repeating)");
                self.send_target(PendingTarget::MultiStep, 1, sink).await;
            }

            "raw_step" => {
                info!("[cmd] .raw_step — sending target cursor (ground, no validation)");
                self.send_target(PendingTarget::RawStep, 1, sink).await;
            }

            // ── Gump ──────────────────────────────────────────────────
            "menu" => {
                self.send_action_menu_gump(observer.pos.serial, sink).await;
            }

            // ── Lua scripting ─────────────────────────────────────────────
            #[cfg(feature = "lua")]
            "lua" => {
                self.handle_lua_command(_args, sink).await;
            }

            _ => {
                let msg = format!("Unknown command: .{name}");
                send_system_message(sink, &msg).await;
            }
        }

        Handled::Yes
    }

    // ── Action menu gump ──────────────────────────────────────────────

    /// Send the action menu gump.
    ///
    /// Layout (200 x 265 resizable background):
    /// - Button 1: Info
    /// - Button 2: Where
    /// - Button 3: Inspect (targeted)
    /// - Button 4: Step (targeted, validated)
    /// - Button 5: Multi-step (targeted, repeating)
    /// - Button 6: Raw step (targeted, no validation)
    /// - Button 7 / 0: Close (no action)
    pub async fn send_action_menu_gump(
        &mut self,
        serial: u32,
        sink: &mpsc::Sender<RawPacket>,
    ) {
        self.send_gump(
            GumpKind::ActionMenu,
            serial,
            "{ page 0 }{ nodispose }{ noclose }\
             { resizepic 0 0 2620 200 265 }\
             { text 10 10 2100 0 }\
             { button 10  35 2117 2118 1 0 1 }{ text 45  35 2100 1 }\
             { button 10  65 2117 2118 1 0 2 }{ text 45  65 2100 2 }\
             { button 10  95 2117 2118 1 0 3 }{ text 45  95 2100 3 }\
             { button 10 125 2117 2118 1 0 4 }{ text 45 125 2100 4 }\
             { button 10 155 2117 2118 1 0 5 }{ text 45 155 2100 5 }\
             { button 10 185 2117 2118 1 0 6 }{ text 45 185 2100 6 }\
             { button 10 215 2117 2118 1 0 7 }{ text 45 215 2100 7 }",
            &[
                GumpTextLine("Commands".to_string()),
                GumpTextLine("Info".to_string()),
                GumpTextLine("Where".to_string()),
                GumpTextLine("Inspect".to_string()),
                GumpTextLine("Step".to_string()),
                GumpTextLine("Multi-step".to_string()),
                GumpTextLine("Raw step".to_string()),
                GumpTextLine("Close".to_string()),
            ],
            sink,
        )
        .await;
    }

    // ── Target cursor helpers ─────────────────────────────────────────

    async fn send_target(
        &mut self,
        target: PendingTarget,
        cursor_target: u8,
        sink: &mpsc::Sender<RawPacket>,
    ) {
        let mut sink_mut = sink.clone();
        // Errors are silently ignored (fire-and-forget).
        let _ = common_cmd::send_target_cursor(
            target.cursor_id(),
            cursor_target,
            &mut sink_mut,
        ).await;
        self.pending_target = Some(target);
    }

    async fn handle_target_response(
        &mut self,
        target: PendingTarget,
        tc: &TargetCursor,
        sink: &mpsc::Sender<RawPacket>,
        observer: &ObserverPipeline,
    ) -> Handled {
        match target {
            PendingTarget::Inspect => {
                let serial = tc.target_serial;
                if tc.cursor_type == 3 || serial == 0 {
                    debug!("[cmd] .inspect cancelled");
                    return Handled::Yes;
                }

                if let Some(entity) = observer.session.visible.get(serial) {
                    let kind = entity.kind();
                    let msg = format!(
                        "Inspect: serial={:#010X} graphic={:#06X} kind={:?} pos=({},{},{})",
                        serial,
                        entity.graphic(),
                        kind,
                        entity.x(),
                        entity.y(),
                        entity.z(),
                    );
                    info!("[cmd] {}", msg);
                    send_system_message(sink, &msg).await;
                } else if let Some(owner) = observer.session.visible.lookup_serial(serial) {
                    let msg = format!(
                        "Inspect: serial={:#010X} (equipped on {:#010X})",
                        serial, owner,
                    );
                    info!("[cmd] {}", msg);
                    send_system_message(sink, &msg).await;
                } else {
                    let msg = format!(
                        "Inspect: serial={:#010X} — not in visible set",
                        serial,
                    );
                    debug!("[cmd] {}", msg);
                    send_system_message(sink, &msg).await;
                }
                Handled::Yes
            }

            PendingTarget::Step => {
                if tc.cursor_type == 3 || (tc.x == 0xFFFF && tc.y == 0xFFFF) {
                    debug!("[cmd] .step cancelled");
                    return Handled::Yes;
                }

                let dx = tc.x as i32 - observer.pos.x as i32;
                let dy = tc.y as i32 - observer.pos.y as i32;

                match Heading::from_delta(dx, dy) {
                    Some(heading) => {
                        info!(
                            "[cmd] .step toward ({},{}) heading={}",
                            tc.x, tc.y, heading,
                        );
                        let msg = format!("Step: heading={heading}");
                        send_system_message(sink, &msg).await;
                        Handled::Step { heading }
                    }
                    None => {
                        send_system_message(sink, "Already at target").await;
                        Handled::Yes
                    }
                }
            }

            PendingTarget::MultiStep => {
                if tc.cursor_type == 3 || (tc.x == 0xFFFF && tc.y == 0xFFFF) {
                    // Explicit cancel — stop the chain, re-show menu.
                    debug!("[cmd] .mstep cancelled");
                    self.send_action_menu_gump(observer.pos.serial, sink).await;
                    return Handled::Yes;
                }

                let dx = tc.x as i32 - observer.pos.x as i32;
                let dy = tc.y as i32 - observer.pos.y as i32;

                match Heading::from_delta(dx, dy) {
                    Some(heading) => {
                        info!(
                            "[cmd] .mstep toward ({},{}) heading={}",
                            tc.x, tc.y, heading,
                        );
                        let msg = format!("Multi-step: heading={heading}");
                        send_system_message(sink, &msg).await;
                        // Send cursor again to continue the chain.
                        self.send_target(PendingTarget::MultiStep, 1, sink).await;
                        Handled::Step { heading }
                    }
                    None => {
                        send_system_message(sink, "Already at target").await;
                        // Re-send cursor — let the user pick again.
                        self.send_target(PendingTarget::MultiStep, 1, sink).await;
                        Handled::Yes
                    }
                }
            }

            PendingTarget::RawStep => {
                if tc.cursor_type == 3 || (tc.x == 0xFFFF && tc.y == 0xFFFF) {
                    debug!("[cmd] .raw_step cancelled");
                    return Handled::Yes;
                }

                let dx = tc.x as i32 - observer.pos.x as i32;
                let dy = tc.y as i32 - observer.pos.y as i32;

                match Heading::from_delta(dx, dy) {
                    Some(heading) => {
                        info!(
                            "[cmd] .raw_step toward ({},{}) heading={}",
                            tc.x, tc.y, heading,
                        );
                        let msg = format!("Raw step: heading={heading}");
                        send_system_message(sink, &msg).await;
                        Handled::RawStep { heading }
                    }
                    None => {
                        send_system_message(sink, "Already at target").await;
                        Handled::Yes
                    }
                }
            }
        }
    }

    // ── Lua command helper ──────────────────────────────────────────

    #[cfg(feature = "lua")]
    async fn handle_lua_command(
        &self,
        args: &str,
        sink: &mpsc::Sender<RawPacket>,
    ) {
        let Some(tx) = &self.lua_cmd_tx else {
            send_system_message(sink, "Lua scripting not available for this session").await;
            return;
        };

        let args = args.trim();
        if args.is_empty() {
            send_system_message(sink, "Usage: .lua <path> | .lua reload | .lua stop").await;
            return;
        }

        let result = match args {
            "stop" => {
                tx.send(crate::lua_script::LuaCommand::Stop).await
            }
            "reload" => {
                tx.send(crate::lua_script::LuaCommand::Reload).await
            }
            path => {
                info!("[cmd] .lua loading: {}", path);
                tx.send(crate::lua_script::LuaCommand::RunFile(PathBuf::from(path))).await
            }
        };

        match result {
            Ok(()) => send_system_message(sink, &format!("Lua: {}", args)).await,
            Err(_) => send_system_message(sink, "Lua: command channel closed").await,
        }
    }

    // ── Gump helpers ──────────────────────────────────────────────────

    async fn send_gump(
        &mut self,
        kind: GumpKind,
        serial: u32,
        commands: &str,
        text_lines: &[GumpTextLine],
        sink: &mpsc::Sender<RawPacket>,
    ) {
        let gump_id = kind.gump_id();
        info!("[cmd] sending gump {:?} (id={:#010X})", kind, gump_id);
        let mut sink_mut = sink.clone();
        // Errors are silently ignored (fire-and-forget).
        let _ = common_cmd::send_gump(
            gump_id, serial, commands, text_lines, &mut sink_mut,
        ).await;
        self.active_gumps.insert(gump_id, kind);
    }

    async fn handle_gump_response(
        &mut self,
        kind: GumpKind,
        resp: &GumpMenuSelection,
        sink: &mpsc::Sender<RawPacket>,
        observer: &ObserverPipeline,
    ) -> Handled {
        debug!(
            "[cmd] gump {:?} response: button={}, switches={:?}",
            kind, resp.button_id, resp.switches
        );

        match kind {
            GumpKind::ActionMenu => {
                match resp.button_id {
                    0 | 7 => {
                        // Close — no action.
                        Handled::Yes
                    }
                    1 => {
                        // Info
                        info!("[cmd] action menu: Info selected");
                        let msg = format!(
                            "Visible: {}, World: {}, Pos: ({},{},{}), Range: {}",
                            observer.session.visible.len(),
                            observer.session.current_world,
                            observer.pos.x,
                            observer.pos.y,
                            observer.pos.z,
                            observer.session.view_range(),
                        );
                        send_system_message(sink, &msg).await;
                        // Re-show menu.
                        self.send_action_menu_gump(observer.pos.serial, sink).await;
                        Handled::Yes
                    }
                    2 => {
                        // Where
                        info!("[cmd] action menu: Where selected");
                        let msg = format!(
                            "Position: ({},{},{}) facing={}",
                            observer.pos.x, observer.pos.y, observer.pos.z, observer.pos.facing,
                        );
                        send_system_message(sink, &msg).await;
                        self.send_action_menu_gump(observer.pos.serial, sink).await;
                        Handled::Yes
                    }
                    3 => {
                        // Inspect (targeted)
                        info!("[cmd] action menu: Inspect selected");
                        self.send_target(PendingTarget::Inspect, 0, sink).await;
                        Handled::Yes
                    }
                    4 => {
                        // Step (targeted, validated)
                        info!("[cmd] action menu: Step selected");
                        self.send_target(PendingTarget::Step, 1, sink).await;
                        Handled::Yes
                    }
                    5 => {
                        // Multi-step (targeted, repeating)
                        info!("[cmd] action menu: Multi-step selected");
                        self.send_target(PendingTarget::MultiStep, 1, sink).await;
                        Handled::Yes
                    }
                    6 => {
                        // Raw step (targeted, no validation)
                        info!("[cmd] action menu: Raw step selected");
                        self.send_target(PendingTarget::RawStep, 1, sink).await;
                        Handled::Yes
                    }
                    other => {
                        debug!("[cmd] action menu: unknown button_id={}", other);
                        Handled::Yes
                    }
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Send a system message (lower-left corner) to the client via its sink.
async fn send_system_message(sink: &mpsc::Sender<RawPacket>, text: &str) {
    let _ = sink.send(common_cmd::system_message_packet(text)).await;
}

/// Build a system-message packet (lower-left corner text).
///
/// Re-exported from common for callers outside this module (e.g.
/// `headless.rs`).
pub fn system_message_packet(text: &str) -> RawPacket {
    common_cmd::system_message_packet(text)
}
