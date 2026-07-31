//! Dot-command dispatcher and interaction handler.
//!
//! Handles three kinds of player interactions:
//!
//! - **Dot-commands** — player types `.command` in chat.
//! - **Target cursor responses** — player clicks on an object after a
//!   target request.
//! - **Gump responses** — player clicks a button in a server-sent dialog.
//!
//! # Architecture
//!
//! [`DotCommands`] is a state machine that owns all pending interaction
//! state (at most one pending target cursor and any number of registered
//! gumps).  A single method [`DotCommands::handle_packet`] is called for
//! every incoming C→S packet in both the playback and free-move loops.
//!
//! # Command types
//!
//! | Type | How it works |
//! |------|-------------|
//! | Immediate | Executed inline when text is received |
//! | Targeted  | Sends `TargetCursor`, waits for response |
//! | Gump      | Sends `SendGumpDialog`, waits for `GumpMenuSelection` |
//!
//! # Adding a new command
//!
//! - **Immediate**: add a branch in `DotCommands::dispatch_text`.
//! - **Targeted**: add a variant to `PendingTarget`, a branch in
//!   `dispatch_text`, and a branch in `handle_target_response`.
//! - **Gump**: add a variant to `GumpKind`, use `DotCommands::send_gump`
//!   to show it, and add a branch in `handle_gump_response`.

use std::collections::HashMap;

use framework::diorama::ObserverPipeline;
use log::{debug, info, trace};
use common::dot_commands::{
    self as common_cmd, CMD_CURSOR_BASE, CMD_GUMP_BASE, PacketSink,
};

use network::error as fw_error;
use protocol::RawPacket;

use packets::character::UpdateMobile;
use packets::gump::{GumpMenuSelection, GumpTextLine};
use packets::interaction::{DeleteObject, TargetCursor};
use packets::mobile_flags::MobileFlags;
use packets::movement::Notoriety;
use packets::system::GeneralInfo;
use packets::traits::{ManualPacket, BasicPacket};

use crate::replay_handler::ReplayCommand;
use crate::replay_session::ShadowTx;
use common::uo_engine::rpc::EngineProxy;
use common::uo_engine::handler::EngineCommand;

// ── Public result type ────────────────────────────────────────────────────

/// Result of [`DotCommands::handle_packet`].
pub enum Handled {
    /// The packet was consumed — the caller should `continue` the loop.
    Yes,
    /// Not a command packet — the caller should process it normally.
    No,
    /// The stop-replay gump was confirmed — the caller should exit playback.
    StopPlayback,
    /// A targeted action finished — caller should re-show the action menu gump.
    ReshowActionMenu,
    /// User requested a seek in the replay by `delta_ms` milliseconds
    /// (negative = rewind, positive = fast-forward).
    SeekPlayback(i64),
    /// User requested to restart replay from the beginning.
    RestartReplay,
    /// Toggle pause/resume of playback.
    TogglePause,
    /// Step to the previous/next packet (negative = prev, positive = next).
    StepPacket(i32),
    /// Step to the previous/next client (C→S) packet.
    StepClientPacket(i32),
    /// Step to the previous/next server (S→C) packet.
    StepServerPacket(i32),
    /// Fast-forward: accelerated playback by `delta_ms` (always positive).
    FastForward(i64),
}

// ── Pending target variants ───────────────────────────────────────────────

/// Which command is waiting for a target-cursor response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingTarget {
    /// `.remove` — delete the targeted object from the world.
    Remove,
    /// `.tele` — teleport the player to the targeted location (once).
    Teleport,
    /// `.mtele` — teleport the player repeatedly until explicitly cancelled.
    MultiTeleport,
}

impl PendingTarget {
    fn cursor_id(self) -> u32 {
        CMD_CURSOR_BASE
            | match self {
                Self::Remove => 0x01,
                Self::Teleport => 0x02,
                Self::MultiTeleport => 0x03,
            }
    }
}

// ── Gump variants ─────────────────────────────────────────────────────────

/// Registered gump types that the command system knows how to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GumpKind {
    /// `.info` — world/session info dialog.
    Info,
    /// Free-move action menu (Remove / Teleport / Replay-from-start buttons).
    ActionMenu,
    /// Playback control bar — rewind/fast-forward + stop.
    PlaybackControl,
}

impl GumpKind {
    fn gump_id(self) -> u32 {
        CMD_GUMP_BASE
            | match self {
                Self::Info => 0x701,
                Self::ActionMenu => 0x702,
                Self::PlaybackControl => 0x703,
            }
    }
}

// ── DotCommands ───────────────────────────────────────────────────────────

/// Command dispatcher and interaction state machine.
pub struct DotCommands {
    /// Currently pending target-cursor request, if any.
    pending_target: Option<PendingTarget>,
    /// Active gumps by gump_id → kind.
    active_gumps: HashMap<u32, GumpKind>,
}

impl DotCommands {
    pub fn new() -> Self {
        Self {
            pending_target: None,
            active_gumps: HashMap::new(),
        }
    }

    // ── Main entry point ──────────────────────────────────────────────

    /// Call this for every incoming C→S packet.
    ///
    /// Returns:
    /// - [`Handled::Yes`] — packet consumed, caller should `continue`.
    /// - [`Handled::No`] — not ours, caller processes normally.
    /// - [`Handled::StopPlayback`] — stop-replay gump confirmed.
    pub async fn handle_packet(
        &mut self,
        packet: &RawPacket,
        client: &mut impl PacketSink,
        observer: &mut ObserverPipeline,
        shadow_tx: &ShadowTx,
    ) -> fw_error::Result<Handled> {
        // ── Speech → dispatch dot-command ─────────────────────────────────
        if let Some(text) = common_cmd::extract_speech_text(packet) {
            if let Some(cmd) = text.strip_prefix('.') {
                return self
                    .dispatch_text(cmd, client, observer, shadow_tx)
                    .await;
            }
        }

        // ── TargetCursor response ────────────────────────────────────
        if packet.id() == TargetCursor::ID {
            if let Ok(tc) = TargetCursor::from_bytes(&packet.data) {
                if let Some(pending) = self.pending_target.take() {
                    if tc.cursor_id == pending.cursor_id() {
                        let reshow = self
                            .handle_target_response(pending, &tc, client, observer, shadow_tx)
                            .await?;
                        return Ok(if reshow {
                            Handled::ReshowActionMenu
                        } else {
                            Handled::Yes
                        });
                    }
                    self.pending_target = Some(pending);
                }
            }
            return Ok(Handled::Yes);
        }

        // ── GumpMenuSelection response ───────────────────────────────
        if packet.id() == GumpMenuSelection::ID {
            if let Ok(resp) = GumpMenuSelection::from_bytes(&packet.data) {
                if let Some(kind) = self.active_gumps.remove(&resp.gump_id) {
                    return self
                        .handle_gump_response(kind, &resp, client)
                        .await;
                }
            }
            return Ok(Handled::Yes);
        }

        Ok(Handled::No)
    }

    // ── Text dispatch ─────────────────────────────────────────────────

    async fn dispatch_text(
        &mut self,
        cmd: &str,
        client: &mut impl PacketSink,
        observer: &mut ObserverPipeline,
        shadow_tx: &ShadowTx,
    ) -> fw_error::Result<Handled> {
        let (name, _args) = cmd.split_once(' ').unwrap_or((cmd, ""));

        match name.to_ascii_lowercase().as_str() {
            // ── Targeted ──────────────────────────────────────────────
            "remove" => {
                self.send_target(PendingTarget::Remove, 0, client).await?;
            }
            "tele" => {
                self.send_target(PendingTarget::Teleport, 1, client).await?;
            }
            "mtele" => {
                self.send_target(PendingTarget::MultiTeleport, 1, client).await?;
            }

            // ── Gump ──────────────────────────────────────────────────
            "menu" => {
                self.send_action_menu_gump(observer.pos.serial, client).await?;
            }
            "info" => {
                let text = format!(
                    "Visible: {}\\nWorld: {}\\nPos: ({},{},{})\\nRange: {}",
                    observer.session.visible.len(),
                    observer.session.current_world,
                    observer.pos.x,
                    observer.pos.y,
                    observer.pos.z,
                    observer.view_range(),
                );
                self.send_gump(
                    GumpKind::Info,
                    observer.pos.serial,
                    &format!(
                        "{{ page 0 }}{{ resizepic 0 0 2620 300 250 }}\
                         {{ text 20 20 0 0 }}\
                         {{ button 120 210 2130 2129 1 0 1 }}",
                    ),
                    &[GumpTextLine(text)],
                    client,
                )
                .await?;
            }

            // ── Immediate ─────────────────────────────────────────────
            "where" => {
                let msg = format!(
                    "Position: ({},{},{}) facing={}",
                    observer.pos.x, observer.pos.y, observer.pos.z, observer.pos.facing
                );
                common_cmd::send_system_message(client, &msg).await?;
            }

            "world" => {
                let world_id: u8 = match _args.trim().parse() {
                    Ok(w) if w <= 5 => w,
                    _ => {
                        let msg = format!(
                            "Usage: .world <0-5> (current: {})",
                            observer.session.current_world
                        );
                        common_cmd::send_system_message(client, &msg).await?;
                        return Ok(Handled::Yes);
                    }
                };

                if world_id == observer.session.current_world {
                    let msg = format!("Already on world {world_id}");
                    common_cmd::send_system_message(client, &msg).await?;
                    return Ok(Handled::Yes);
                }

                let old_world = observer.session.current_world;
                info!(
                    "[cmd] .world {} → {} at ({},{},{})",
                    old_world, world_id, observer.pos.x, observer.pos.y, observer.pos.z
                );

                // 1. Send SetMap to client + ingest (updates session state,
                //    swaps multi hot/cold, arms staleness).
                let set_map = GeneralInfo::SetMap { world: world_id };
                let set_map_bytes = set_map.to_bytes();
                observer.session.ingest_packet(&set_map_bytes);
                client.send_packet(RawPacket::s2c(set_map_bytes)).await?;

                // 2. Sync player position in the new world's zone.
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), world_id);
                engine.teleport(
                    observer.pos.serial, observer.pos.x, observer.pos.y, observer.pos.z,
                    None,
                ).await;

                // 3. Send DrawGamePlayer so client shows the character.
                if observer.pos.is_ready() {
                    client
                        .send_packet(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
                        .await?;
                }

                // 4. Stream visible entities from the new world's zone.
                let view_rect = *observer.session.visible.view_rect();
                let items = engine.items_in_area(view_rect).await;
                let count = items.len();
                for raw in items {
                    observer.session.ingest_packet(&raw);
                    client.send_packet(RawPacket::s2c(raw)).await?;
                }

                let msg = format!(
                    "Switched to world {world_id} — {count} items loaded"
                );
                common_cmd::send_system_message(client, &msg).await?;
            }

            _ => {
                let msg = format!("Unknown command: .{name}");
                common_cmd::send_system_message(client, &msg).await?;
            }
        }

        Ok(Handled::Yes)
    }

    // ── Playback control gump ─────────────────────────────────────────────

    /// Send the playback control gump shown during replay.
    ///
    /// Layout — three rows + progress label:
    ///
    /// ```text
    ///  Row 1 (snapshot seek):
    ///  -30s   -10s    -1s   Stop    +1s   +10s   +30s
    /// [-30s] [-10s]  [-1s] [Stop]  [+1s] [+10s] [+30s]
    /// ─────────────────────────────────────────── separator 1
    ///  Row 2 (packet step + pause):
    /// [PktPr][CliPr][SrvPr][Pause][SrvNx][CliNx][PktNx]
    /// ─────────────────────────────────────────── separator 2
    ///  Row 3 (fast-forward — accelerated playback):
    ///                       FF+1s  FF+10s FF+30s
    /// ─────────────────────────────────────────────
    ///                    01:23 / 05:47
    /// ```
    ///
    /// Button IDs (row 1 — snapshot seek):
    /// - 1 = rewind 30s, 2 = rewind 10s, 3 = rewind 1s
    /// - 4 = Stop (exit playback → free-move)
    /// - 5 = forward 1s, 6 = forward 10s, 7 = forward 30s
    ///
    /// Button IDs (row 2 — packet step + pause):
    /// - 8  = prev packet (any), 9  = prev client, 10 = prev server
    /// - 11 = toggle pause/resume
    /// - 12 = next server, 13 = next client, 14 = next packet (any)
    ///
    /// Button IDs (row 3 — fast-forward):
    /// - 15 = FF +5s, 16 = FF +30s, 17 = FF +60s
    pub async fn send_playback_control_gump(
        &mut self,
        serial: u32,
        current_ms: u64,
        total_ms: u64,
        paused: bool,
        client: &mut impl PacketSink,
    ) -> fw_error::Result<()> {
        let fmt_ms = |ms: u64| -> String {
            let secs = ms / 1000;
            format!("{:02}:{:02}", secs / 60, secs % 60)
        };
        let progress = format!("{} / {}", fmt_ms(current_ms), fmt_ms(total_ms));

        // Row 1: labels y=5, buttons y=25  (4508/4502 arrows, ~45×45)
        // Separator 1: y=75
        // Row 2: buttons y=83, labels y=128 (4508/4502 arrows, ~45×45)
        // Separator 2: y=140
        // Row 3: buttons y=148, labels y=170 (2103/2104 gem, ~15×15)
        // Progress: y=190
        // resizepic: 470 × 215
        //
        // Row 1 & 2: arrow buttons 4508/4502, text color 2100
        // Row 3: alternate gem buttons 2103/2104, text color 53 (green-ish)
        // Pause/Resume button: playing → 2111/2112, paused → 2114/2115
        let (pause_up, pause_dn) = if paused {
            (2114, 2115)
        } else {
            (2111, 2112)
        };

        let commands = format!(
            "{{ page 0 }}{{ nodispose }}{{ noclose }}\
             {{ resizepic 0 0 2620 470 215 }}\
             {{ text  27  5 2100  0 }}{{ button  20 25 4508 4508 1 0  1 }}\
             {{ text  87  5 2100  1 }}{{ button  80 25 4508 4508 1 0  2 }}\
             {{ text 147  5 2100  2 }}{{ button 140 25 4508 4508 1 0  3 }}\
             {{ text 217  5 2100  3 }}{{ button 210 35 2119 2120 1 0  4 }}\
             {{ text 287  5 2100  4 }}{{ button 280 25 4502 4502 1 0  5 }}\
             {{ text 347  5 2100  5 }}{{ button 340 25 4502 4502 1 0  6 }}\
             {{ text 407  5 2100  6 }}{{ button 400 25 4502 4502 1 0  7 }}\
             {{ button  20 83 4508 4508 1 0  8 }}{{ text  27 128 2100  7 }}\
             {{ button  80 83 4508 4508 1 0  9 }}{{ text  87 128 2100  8 }}\
             {{ button 140 83 4508 4508 1 0 10 }}{{ text 147 128 2100  9 }}\
             {{ button 210 95 {pause_up} {pause_dn} 1 0 11 }}{{ text 217 128 2100 10 }}\
             {{ button 280 83 4502 4502 1 0 12 }}{{ text 287 128 2100 11 }}\
             {{ button 340 83 4502 4502 1 0 13 }}{{ text 347 128 2100 12 }}\
             {{ button 400 83 4502 4502 1 0 14 }}{{ text 407 128 2100 13 }}\
             {{ button 300 148 9903 9904 1 0 15 }}{{ text 287 170 53 14 }}\
             {{ button 360 148 9903 9904 1 0 16 }}{{ text 347 170 53 15 }}\
             {{ button 420 148 9903 9904 1 0 17 }}{{ text 407 170 53 16 }}\
             {{ text 185 190 2100 17 }}"
        );

        self.send_gump(
            GumpKind::PlaybackControl,
            serial,
             &commands,
            &[
                // Row 1 — snapshot seek (indices 0–6)
                GumpTextLine("-30s".to_string()),
                GumpTextLine("-10s".to_string()),
                GumpTextLine("-1s".to_string()),
                GumpTextLine("Stop".to_string()),
                GumpTextLine("+1s".to_string()),
                GumpTextLine("+10s".to_string()),
                GumpTextLine("+30s".to_string()),
                // Row 2 — packet step + pause (indices 7–13)
                GumpTextLine("PktPr".to_string()),
                GumpTextLine("CliPr".to_string()),
                GumpTextLine("SrvPr".to_string()),
                GumpTextLine(if paused { "Resume" } else { "Pause" }.to_string()),
                GumpTextLine("SrvNx".to_string()),
                GumpTextLine("CliNx".to_string()),
                GumpTextLine("PktNx".to_string()),
                // Row 3 — fast-forward (indices 14–16)
                GumpTextLine("FF+5s".to_string()),
                GumpTextLine("FF+30s".to_string()),
                GumpTextLine("FF+60s".to_string()),
                // Progress (index 17)
                GumpTextLine(progress),
            ],
            client,
        )
        .await
    }

    /// Send the free-move action menu gump.
    ///
    /// Layout (250 × 120 resizable background):
    /// - Button 1 (button_id=1): Remove object
    /// - Button 2 (button_id=2): Teleport
    /// - Button 0: Close (no action)
    pub async fn send_action_menu_gump(
        &mut self,
        serial: u32,
        client: &mut impl PacketSink,
    ) -> fw_error::Result<()> {
        self.send_gump(
            GumpKind::ActionMenu,
            serial,
            "{ page 0 }{ nodispose }{ noclose }\
             { resizepic 0 0 2620 200 175 }\
             { text 10 10 2100 0 }\
             { button 10  35 2117 2118 1 0 1 }{ text 45  35 2100 1 }\
             { button 10  65 2117 2118 1 0 2 }{ text 45  65 2100 2 }\
             { button 10  95 2117 2118 1 0 3 }{ text 45  95 2100 3 }\
             { button 10 125 2117 2118 1 0 4 }{ text 45 125 2100 4 }",
            &[
                GumpTextLine("Actions".to_string()),
                GumpTextLine("Remove object".to_string()),
                GumpTextLine("Teleport".to_string()),
                GumpTextLine("Multi-teleport".to_string()),
                GumpTextLine("Replay from start".to_string()),
            ],
            client,
        )
        .await
    }

    // ── Target cursor helpers ─────────────────────────────────────────

    async fn send_target(
        &mut self,
        target: PendingTarget,
        cursor_target: u8,
        client: &mut impl PacketSink,
    ) -> fw_error::Result<()> {
        info!("[cmd] .{:?} — sending target cursor", target);
        common_cmd::send_target_cursor(target.cursor_id(), cursor_target, client).await?;
        self.pending_target = Some(target);
        Ok(())
    }

    async fn handle_target_response(
        &mut self,
        target: PendingTarget,
        tc: &TargetCursor,
        client: &mut impl PacketSink,
        observer: &mut ObserverPipeline,
        shadow_tx: &ShadowTx,
    ) -> fw_error::Result<bool> {
        // Returns true when the action menu should be re-shown afterwards.
        match target {
            PendingTarget::Remove => {
                let serial = tc.target_serial;
                if tc.cursor_type == 3 || serial == 0 {
                    debug!("[cmd] .remove target cancelled");
                    return Ok(true);
                }
                info!(
                    "[cmd] .remove target serial={:#010X} at ({},{},{})",
                    serial, tc.x, tc.y, tc.z
                );
                observer.session.visible.remove(serial);

                let cmd = crate::continuum::WorkerCommand::MapCommand(
                    observer.session.current_world,
                    ReplayCommand::RemoveEntity {
                        entity_id: serial,
                    }
                );
                let _ = shadow_tx.send(cmd).await;

                let del = DeleteObject {
                    id: DeleteObject::ID,
                    serial,
                };
                client.send_packet(RawPacket::s2c(del.to_bytes())).await?;
                let msg = format!("Removed object {:#010X}", serial);
                common_cmd::send_system_message(client, &msg).await?;
                Ok(true)
            }

            PendingTarget::Teleport => {
                if tc.cursor_type == 3 || tc.x == 0xFFFF || tc.y == 0xFFFF {
                    debug!("[cmd] .tele target cancelled");
                    return Ok(true);
                }
                let (old_x, old_y, old_z) = (observer.pos.x, observer.pos.y, observer.pos.z);
                observer.pos.x = tc.x;
                observer.pos.y = tc.y;
                observer.pos.z = tc.z;
                info!(
                    "[cmd] .tele ({},{},{}) → ({},{},{})",
                    old_x, old_y, old_z, observer.pos.x, observer.pos.y, observer.pos.z
                );
                // Sync the shadow continuum entity position.
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                engine.teleport(
                    observer.pos.serial, observer.pos.x, observer.pos.y, observer.pos.z,
                    None,
                ).await;
                if observer.pos.is_ready() {
                    client
                        .send_packet(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
                        .await?;
                    // Send UpdateMobile as well — the client may ignore
                    // DrawGamePlayer while it has unacknowledged MoveRequests.
                    let upd = UpdateMobile {
                        id: UpdateMobile::ID,
                        serial: observer.pos.serial,
                        model: observer.pos.body_type,
                        x: observer.pos.x,
                        y: observer.pos.y,
                        z: observer.pos.z,
                        direction: observer.pos.facing.raw(),
                        hue: observer.pos.hue,
                        status_flags: MobileFlags(observer.pos.flags),
                        notoriety: Notoriety::Innocent,
                    };
                    client.send_packet(RawPacket::s2c(upd.to_bytes())).await?;
                }
                let new_strips = observer.session.visible.update_view(observer.pos.x, observer.pos.y);
                let mut count = 0usize;
                for strip in &new_strips {
                    let items = engine.items_in_area(*strip).await;
                    for raw in items {
                        observer.session.ingest_packet(&raw);
                        client.send_packet(RawPacket::s2c(raw)).await?;
                        count += 1;
                    }
                }
                let msg = format!(
                    "Teleported to ({},{},{}) - {} new items",
                    observer.pos.x, observer.pos.y, observer.pos.z, count
                );
                common_cmd::send_system_message(client, &msg).await?;
                Ok(true)
            }

            PendingTarget::MultiTeleport => {
                if tc.cursor_type == 3 || tc.x == 0xFFFF || tc.y == 0xFFFF {
                    // Explicit cancel — stop the chain and re-show the menu.
                    debug!("[cmd] .mtele cancelled");
                    return Ok(true);
                }
                let (old_x, old_y, old_z) = (observer.pos.x, observer.pos.y, observer.pos.z);
                observer.pos.x = tc.x;
                observer.pos.y = tc.y;
                observer.pos.z = tc.z;
                info!(
                    "[cmd] .mtele ({},{},{}) → ({},{},{})",
                    old_x, old_y, old_z, observer.pos.x, observer.pos.y, observer.pos.z
                );
                // Sync the shadow continuum entity position.
                let engine = EngineProxy::<EngineCommand>::new(shadow_tx.clone(), observer.session.current_world);
                engine.teleport(
                    observer.pos.serial, observer.pos.x, observer.pos.y, observer.pos.z,
                    None,
                ).await;
                if observer.pos.is_ready() {
                    client
                        .send_packet(RawPacket::s2c(observer.pos.to_draw_game_player().to_bytes()))
                        .await?;
                    // Send UpdateMobile as well — the client may ignore
                    // DrawGamePlayer while it has unacknowledged MoveRequests.
                    let upd = UpdateMobile {
                        id: UpdateMobile::ID,
                        serial: observer.pos.serial,
                        model: observer.pos.body_type,
                        x: observer.pos.x,
                        y: observer.pos.y,
                        z: observer.pos.z,
                        direction: observer.pos.facing.raw(),
                        hue: observer.pos.hue,
                        status_flags: MobileFlags(observer.pos.flags),
                        notoriety: Notoriety::Innocent,
                    };
                    client.send_packet(RawPacket::s2c(upd.to_bytes())).await?;
                }
                let new_strips = observer.session.visible.update_view(observer.pos.x, observer.pos.y);
                let mut count = 0usize;
                for strip in &new_strips {
                    let items = engine.items_in_area(*strip).await;
                    for raw in items {
                        observer.session.ingest_packet(&raw);
                        client.send_packet(RawPacket::s2c(raw)).await?;
                        count += 1;
                    }
                }
                let msg = format!(
                    "Teleported to ({},{},{}) - {} new items",
                    observer.pos.x, observer.pos.y, observer.pos.z, count
                );
                common_cmd::send_system_message(client, &msg).await?;
                // Send the cursor again to continue the chain.
                self.send_target(PendingTarget::MultiTeleport, 1, client).await?;
                Ok(false)
            }
        }
    }

    // ── Gump helpers ──────────────────────────────────────────────────

    /// Close the playback-control gump on the client side.
    ///
    /// Should be called when leaving the playback loop so the `{ noclose }`
    /// gump does not linger on the player's screen.
    pub async fn close_playback_gump(
        &mut self,
        client: &mut impl PacketSink,
    ) -> fw_error::Result<()> {
        let gump_id = GumpKind::PlaybackControl.gump_id();
        if self.active_gumps.remove(&gump_id).is_some() {
            common_cmd::close_gump(gump_id, client).await?;
        }
        Ok(())
    }

    async fn send_gump(
        &mut self,
        kind: GumpKind,
        serial: u32,
        commands: &str,
        text_lines: &[GumpTextLine],
        client: &mut impl PacketSink,
    ) -> fw_error::Result<()> {
        let gump_id = kind.gump_id();
        trace!("[cmd] sending gump {:?} (id={:#010X})", kind, gump_id);
        common_cmd::send_gump(gump_id, serial, commands, text_lines, client).await?;
        self.active_gumps.insert(gump_id, kind);
        Ok(())
    }

    async fn handle_gump_response(
        &mut self,
        kind: GumpKind,
        resp: &GumpMenuSelection,
        client: &mut impl PacketSink,
    ) -> fw_error::Result<Handled> {
        trace!(
            "[cmd] gump {:?} response: button={}, switches={:?}",
            kind, resp.button_id, resp.switches
        );

        match kind {
            GumpKind::PlaybackControl => {
                match resp.button_id {
                    0 => Ok(Handled::Yes), // blocked by noclose, but handle defensively
                    // Row 1 — time seek
                    1 => {
                        info!("[cmd] playback control: rewind 30s");
                        Ok(Handled::SeekPlayback(-30_000))
                    }
                    2 => {
                        info!("[cmd] playback control: rewind 10s");
                        Ok(Handled::SeekPlayback(-10_000))
                    }
                    3 => {
                        info!("[cmd] playback control: rewind 1s");
                        Ok(Handled::SeekPlayback(-1_000))
                    }
                    4 => {
                        info!("[cmd] playback control: stop");
                        Ok(Handled::StopPlayback)
                    }
                    5 => {
                        info!("[cmd] playback control: forward 1s");
                        Ok(Handled::SeekPlayback(1_000))
                    }
                    6 => {
                        info!("[cmd] playback control: forward 10s");
                        Ok(Handled::SeekPlayback(10_000))
                    }
                    7 => {
                        info!("[cmd] playback control: forward 30s");
                        Ok(Handled::SeekPlayback(30_000))
                    }
                    // Row 2 — packet step + pause
                    8 => {
                        trace!("[cmd] playback control: prev packet (any)");
                        Ok(Handled::StepPacket(-1))
                    }
                    9 => {
                        trace!("[cmd] playback control: prev client packet");
                        Ok(Handled::StepClientPacket(-1))
                    }
                    10 => {
                        trace!("[cmd] playback control: prev server packet");
                        Ok(Handled::StepServerPacket(-1))
                    }
                    11 => {
                        info!("[cmd] playback control: toggle pause");
                        Ok(Handled::TogglePause)
                    }
                    12 => {
                        trace!("[cmd] playback control: next server packet");
                        Ok(Handled::StepServerPacket(1))
                    }
                    13 => {
                        trace!("[cmd] playback control: next client packet");
                        Ok(Handled::StepClientPacket(1))
                    }
                    14 => {
                        trace!("[cmd] playback control: next packet (any)");
                        Ok(Handled::StepPacket(1))
                    }
                    // Row 3 — fast-forward (accelerated playback)
                    15 => {
                        info!("[cmd] playback control: fast-forward 5s");
                        Ok(Handled::FastForward(5_000))
                    }
                    16 => {
                        info!("[cmd] playback control: fast-forward 30s");
                        Ok(Handled::FastForward(30_000))
                    }
                    17 => {
                        info!("[cmd] playback control: fast-forward 60s");
                        Ok(Handled::FastForward(60_000))
                    }
                    other => {
                        debug!("[cmd] playback control: unknown button_id={}", other);
                        Ok(Handled::Yes)
                    }
                }
            }

            GumpKind::Info => {
                // Info gump has only an OK button — nothing to do.
                Ok(Handled::Yes)
            }

            GumpKind::ActionMenu => {
                match resp.button_id {
                    0 => {
                        // Closed without action — don't re-show.
                        Ok(Handled::Yes)
                    }
                    1 => {
                        // Remove object
                        info!("[cmd] action menu: Remove selected");
                        self.send_target(PendingTarget::Remove, 0, client).await?;
                        Ok(Handled::Yes)
                    }
                    2 => {
                        // Teleport
                        info!("[cmd] action menu: Teleport selected");
                        self.send_target(PendingTarget::Teleport, 1, client).await?;
                        Ok(Handled::Yes)
                    }
                    3 => {
                        // Multi-teleport
                        info!("[cmd] action menu: Multi-teleport selected");
                        self.send_target(PendingTarget::MultiTeleport, 1, client).await?;
                        Ok(Handled::Yes)
                    }
                    4 => {
                        // Replay from start
                        info!("[cmd] action menu: Replay from start selected");
                        Ok(Handled::RestartReplay)
                    }
                    other => {
                        debug!("[cmd] action menu: unknown button_id={}", other);
                        Ok(Handled::Yes)
                    }
                }
            }
        }
    }
}
