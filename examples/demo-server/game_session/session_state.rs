//! Session state container — groups all mutable per-session state into a
//! single struct to reduce parameter counts and improve cohesion.
//!
//! `SessionContext` owns the player state, action slots, pending cursors,
//! combat/regen state, held item, and open containers.  Methods on the
//! struct replace the scattered helper functions that previously required
//! 10+ parameters.
//!
//! The `tokio::select!` timers (`cast_timer`, `skill_timer`, etc.) are
//! **not** stored here — they must be pinned and polled directly by
//! `select!`.  Instead, `SessionContext` exposes `reset_*_timer` methods
//! that return the `Instant` the caller should use to reset the pin.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use protocol::RawPacket;
use packets::traits::BasicPacket;
use tokio::time::{Instant, Sleep};

use network::error;
use network::session::Session;

use framework::continuum::WorldEvent;
use framework::diorama::ObserverPipeline;
use framework::ecumene::StaticDataProvider;

use common::uo_engine::serial_alloc::SerialAllocator;

use crate::actions::ActiveAction;
use crate::buffs::BuffState;
use crate::combat::{self, CombatState, ChargeResult};
use crate::regen::RegenState;
use crate::DemoWorkerTx;

use super::game_logic::InfraState;

// ── Far-future sentinel ──────────────────────────────────────────────────

/// Used to park timers that shouldn't fire.
pub(super) const FAR_FUTURE: Duration = Duration::from_secs(86400);

pub(super) fn far_future_instant() -> Instant {
    Instant::now() + FAR_FUTURE
}

// ── SessionContext ────────────────────────────────────────────────────────

/// All mutable per-session game state, grouped for easy passing.
pub(super) struct SessionContext {
    // ── Infrastructure state (shared with session loop) ──────────────
    pub infra: InfraState,

    // ── Serial allocator (shared across the server) ─────────────────
    pub serial_alloc: Arc<SerialAllocator>,

    // ── Action slots ─────────────────────────────────────────────────
    pub active_cast: Option<ActiveAction>,
    pub active_skill: Option<ActiveAction>,
    pub active_bandage: Option<ActiveAction>,

    // ── Combat ───────────────────────────────────────────────────────
    pub combat_state: CombatState,

    // ── Regen ────────────────────────────────────────────────────────
    pub regen_state: RegenState,

    // ── Potions ──────────────────────────────────────────────────────
    /// Global potion cooldown — `Some(instant)` if on cooldown.
    pub potion_cooldown_until: Option<Instant>,

    // ── Buffs ────────────────────────────────────────────────────────
    /// Active timed buffs (strength, agility, etc.).
    pub buff_state: BuffState,

    /// When `Some`, a ghost-visibility update is pending and should be applied
    /// to the engine after the current world-event batch (true = visible to
    /// other observers, false = hidden).
    pub pending_ghost_visibility: Option<bool>,
}

impl SessionContext {
    /// Create a new session context with default state.
    pub fn new(
        observer: Option<ObserverPipeline>,
        serial_alloc: Arc<SerialAllocator>,
        worker_tx: &DemoWorkerTx,
        static_data: Option<Arc<dyn StaticDataProvider>>,
        client_version: u_core::ProtocolVersion,
    ) -> Self {
        Self {
            infra: InfraState::new(observer, worker_tx, static_data, client_version),
            serial_alloc,
            active_cast: None,
            active_skill: None,
            active_bandage: None,
            combat_state: CombatState::new(),
            regen_state: RegenState::new(),
            potion_cooldown_until: None,
            buff_state: BuffState::new(),
            pending_ghost_visibility: None,
        }
    }

    // ── Convenience queries ──────────────────────────────────────────

    /// Returns `true` if a game-logic target cursor (spell, skill, bandage)
    /// is currently pending.
    ///
    /// DotCommand cursors are excluded — they should not block game actions.
    pub fn has_pending_cursor(&self) -> bool {
        self.infra.pending_cursor.as_ref().map_or(false, |c| c.is_game_cursor())
    }

    /// Returns `true` if a blocking gump is currently open (e.g. travel
    /// stone menu).  Blocks spells and skills; bandages remain allowed.
    pub fn has_blocking_gump(&self) -> bool {
        self.infra.blocking_gump.is_some()
    }

    /// Returns the held item as `(serial, graphic, amount)` for engine
    /// weight queries, or `None`.
    pub fn held_item_info(&self) -> Option<(u32, u16, u16)> {
        self.infra.held_item.as_ref().map(|h| (h.serial, h.graphic, h.amount))
    }

    // ── Action slot helpers ──────────────────────────────────────────

    /// Complete a timed action and start weapon recovery.
    pub fn complete_action_recovery(&mut self) {
        self.combat_state.start_weapon_recovery(
            Duration::from_millis(crate::constants::melee::ACTION_RECOVERY_DELAY_MS),
        );
    }

    /// If a new action was started (cast/skill), mark weapon as away.
    pub fn maybe_set_weapon_away(&mut self, was_active: bool, is_now_active: bool) {
        if !was_active && is_now_active {
            self.combat_state.set_weapon_away();
        }
    }

    // ── Combat helpers ───────────────────────────────────────────────

    /// Remove a target from the aggro list, sync the swing timer, and
    /// return an `AttackResponse(0)` packet if no targets remain.
    ///
    /// Use this whenever a target becomes invalid (killed, removed from
    /// the world, manually deselected, etc.).
    pub fn disengage_target(
        &mut self,
        serial: u32,
        swing_timer: &mut Pin<Box<Sleep>>,
    ) -> Option<RawPacket> {
        self.combat_state.remove_target(serial);
        swing_timer.as_mut().reset(self.combat_state.next_swing);
        if !self.combat_state.has_targets() {
            Some(combat::attack_response(0))
        } else {
            None
        }
    }

    /// Sync the swing timer to `combat_state.next_swing`.
    ///
    /// Call after any operation that changes the charge cycle timing
    /// (adding/removing targets, consuming a charge, toggling war mode).
    pub fn sync_swing_timer(&self, swing_timer: &mut Pin<Box<Sleep>>) {
        swing_timer.as_mut().reset(self.combat_state.next_swing);
    }

    /// Handle `ChargeResult` — send packets, update state.
    pub async fn handle_charge_result(
        &mut self,
        result: ChargeResult,
        swing_timer: &mut Pin<Box<Sleep>>,
        session: &mut Session,
    ) -> error::Result<()> {
        match result {
            ChargeResult::Idle => {}
            ChargeResult::Consumed { packets } => {
                for pkt in packets {
                    session.send(pkt).await?;
                }
            }
            ChargeResult::Disengaged { serials } => {
                for serial in serials {
                    self.combat_state.remove_target(serial);
                }
                self.sync_swing_timer(swing_timer);
                if !self.combat_state.has_targets() {
                    session.send(combat::attack_response(0)).await?;
                }
            }
        }
        Ok(())
    }

    /// Try to consume a melee charge (if charged and targets exist),
    /// then handle the result.
    pub async fn try_charge_and_handle(
        &mut self,
        swing_timer: &mut Pin<Box<Sleep>>,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        if let Some(p) = &self.infra.player {
            if self.combat_state.charged && self.combat_state.has_targets() {
                let result = combat::try_consume_charge(
                    p.serial, p.world, &mut self.combat_state, worker_tx,
                ).await;
                self.handle_charge_result(result, swing_timer, session).await?;
                if !self.combat_state.charged {
                    self.sync_swing_timer(swing_timer);
                }
            }
        }
        Ok(())
    }

    // ── Action interrupt on world events ─────────────────────────────

    /// Apply a pending ghost-visibility update to the engine, if any.
    ///
    /// Call after a world-event batch.  Sends a `SetGhostVisible` engine
    /// command so other observers draw or remove the ghost.
    pub async fn apply_pending_ghost_visibility(&mut self, worker_tx: &DemoWorkerTx) {
        let Some(visible) = self.pending_ghost_visibility.take() else { return };
        let Some(p) = &self.infra.player else { return };
        let engine = crate::game_util::engine_for(worker_tx, p.world);
        engine.set_ghost_visible(p.serial, visible).await;
    }

    /// Check a world event for action interrupts (damage, kills, entity
    /// removal, movement) and produce response packets.
    ///
    /// Sets `trigger_charge_check` to `true` if a target moved and the
    /// charge is ready.
    pub fn check_action_interrupt(
        &mut self,
        event: &WorldEvent,
        swing_timer: &mut Pin<Box<Sleep>>,
        trigger_charge_check: &mut bool,
        out: &mut Vec<RawPacket>,
    ) {
        let Some(p) = &self.infra.player else { return };
        let player_serial = p.serial;
        let (player_x, player_y, player_z) = (p.x, p.y, p.z);

        if let WorldEvent::DamageDealt { serial, source_serial, .. } = event {
            if *serial == player_serial {
                // Interrupt spell cast on damage.
                if self.active_cast.is_some() {
                    self.active_cast = None;
                    out.extend(crate::game_util::fizzle_packets(
                        player_serial, player_x, player_y, player_z,
                        "The spell fizzles.",
                    ));
                }

                // Interrupt meditation on damage.
                if let Some(msg) = self.regen_state.stop_meditation() {
                    out.push(crate::game_util::system_message(msg));
                }

                // Auto-retaliate: add damage source to aggro list.
                if *source_serial != 0 && *source_serial != player_serial {
                    let was_empty = !self.combat_state.has_targets();
                    self.combat_state.add_aggro(*source_serial);
                    if was_empty && self.combat_state.has_targets() {
                        self.sync_swing_timer(swing_timer);
                    }
                }
            }
        }

        if let WorldEvent::MobileKilled { serial, .. } = event {
            if self.combat_state.targets.contains(serial) {
                if let Some(pkt) = self.disengage_target(*serial, swing_timer) {
                    out.push(pkt);
                }
            }
        }

        if let WorldEvent::EntityRemoved { serial, .. } = event {
            if self.combat_state.targets.contains(serial) {
                if let Some(pkt) = self.disengage_target(*serial, swing_timer) {
                    out.push(pkt);
                }
            }
        }

        if let WorldEvent::EntityMoved { serial, .. } = event {
            if self.combat_state.charged && self.combat_state.targets.contains(serial) {
                *trigger_charge_check = true;
            }
        }

        // A ship tick carries its passengers — treat each carried passenger as
        // a move for the combat charge-check (same as EntityMoved above).
        if let WorldEvent::ShipMoved { passengers, .. } = event {
            if self.combat_state.charged
                && passengers
                    .iter()
                    .any(|(s, ..)| self.combat_state.targets.contains(s))
            {
                *trigger_charge_check = true;
            }
        }

        // ── Player death / resurrection ───────────────────────────────
        if let WorldEvent::PlayerDied { serial, .. } = event {
            if *serial == player_serial {
                // Enter ghost state: drop combat, cancel timed actions.
                self.infra.dead = true;
                self.combat_state.clear_all();
                self.sync_swing_timer(swing_timer);
                self.active_cast = None;
                self.active_skill = None;
                self.active_bandage = None;
                if let Some(msg) = self.regen_state.stop_meditation() {
                    out.push(crate::game_util::system_message(msg));
                }
                // Ghost visible to others only while in war mode.
                self.pending_ghost_visibility = Some(self.combat_state.war_mode);
            }
        }

        if let WorldEvent::PlayerResurrected { serial, .. } = event {
            if *serial == player_serial {
                self.infra.dead = false;
                // A living player is always visible again.
                self.pending_ghost_visibility = Some(true);
            }
        }
    }

    // ── War mode toggle (0x72) ───────────────────────────────────────

    /// Handle a WarMode (0x72) packet.
    pub async fn handle_war_mode(
        &mut self,
        packet: &RawPacket,
        swing_timer: &mut Pin<Box<Sleep>>,
        session: &mut Session,
    ) -> error::Result<bool> {
        if packet.id() != 0x72 {
            return Ok(false);
        }

        if let Ok(pkt) = packets::system::WarMode::from_bytes(&packet.data) {
            self.combat_state.war_mode = pkt.is_fighting();

            if !self.combat_state.war_mode {
                self.combat_state.clear_all();
                self.sync_swing_timer(swing_timer);
                session.send(combat::attack_response(0)).await?;
            }

            // While dead (a ghost), war mode controls visibility to others:
            // visible as a ghost in war mode, fully invisible otherwise.
            if self.infra.dead {
                self.pending_ghost_visibility = Some(self.combat_state.war_mode);
            }

            session.send(combat::war_mode_response(self.combat_state.war_mode)).await?;
        }
        Ok(true)
    }

    // ── Attack request (0x05) ────────────────────────────────────────

    /// Handle an AttackRequest (0x05) packet.
    pub async fn handle_attack_request(
        &mut self,
        packet: &RawPacket,
        swing_timer: &mut Pin<Box<Sleep>>,
        session: &mut Session,
    ) -> error::Result<bool> {
        if packet.id() != 0x05 {
            return Ok(false);
        }

        // Ghosts cannot attack.
        if self.infra.dead {
            session.send(combat::attack_response(0)).await?;
            session.send(crate::game_util::system_message(
                "You are dead and cannot do that.",
            )).await?;
            return Ok(true);
        }

        if let Ok(pkt) = packets::interaction::RequestAttack::from_bytes(&packet.data) {
            if pkt.target_id == 0 {
                if let Some(primary) = self.combat_state.primary_target {
                    self.combat_state.remove_target(primary);
                }
                self.sync_swing_timer(swing_timer);
                session.send(combat::attack_response(0)).await?;
            } else if self.infra.player.as_ref().map_or(false, |p| pkt.target_id == p.serial) {
                // Ignore self-attack.
            } else {
                self.combat_state.add_target(pkt.target_id);
                self.sync_swing_timer(swing_timer);
                session.send(combat::attack_response(pkt.target_id)).await?;
            }
        }
        Ok(true)
    }

    // ── Meditation interrupt on action packets ───────────────────────

    /// Interrupt meditation if the packet is an action-type packet.
    /// Returns a system message packet to send if meditation was interrupted.
    pub fn maybe_interrupt_meditation(&mut self, packet: &RawPacket) -> Option<RawPacket> {
        let dominated_by_action = matches!(
            packet.id(),
            0x02 | 0x05 | 0x06 | 0x07 | 0x08
            | 0x12 | 0x13 | 0x6C | 0xBF
        );
        if dominated_by_action {
            self.regen_state.stop_meditation()
                .map(crate::game_util::system_message)
        } else {
            None
        }
    }
}
