//! `RustGameLogicHandler` — implements `GameLogicHandler` for the Rust
//! game-logic mode.
//!
//! Wraps `SessionContext` (which owns all game-logic state: action slots,
//! combat, regen, pending cursors) and the associated timers.

use std::pin::Pin;
use std::sync::Arc;

use protocol::RawPacket;
use packets::traits::BasicPacket;
use tokio::time::{Duration, Instant, Sleep};

use network::error;
use network::session::Session;

use framework::continuum::WorldEvent;

use crate::actions::ActionPayload;
use crate::DemoWorkerTx;

use super::{
    bandage, crafting, gather, mount, potions, scrolls, spells, treasure, util,
    parsed_packet::ParsedPacket,
    pending_cursor::CursorKind,
    game_logic::{GameLogicHandler, TimerEvent},
    session_state::{SessionContext, far_future_instant, FAR_FUTURE},
};

// ── RustGameLogicHandler ──────────────────────────────────────────────────

/// Rust implementation of `GameLogicHandler`.
///
/// Owns all game-logic state (via `SessionContext`) and the five timers
/// used by the Rust session: cast, skill, bandage, regen, swing.
pub(super) struct RustGameLogicHandler {
    pub(super) ctx: SessionContext,
    cast_timer: Pin<Box<Sleep>>,
    skill_timer: Pin<Box<Sleep>>,
    bandage_timer: Pin<Box<Sleep>>,
    regen_timer: tokio::time::Interval,
    swing_timer: Pin<Box<Sleep>>,
    /// Set by `handle_world_events` when a target moved and charge is ready.
    pub(super) pending_charge_check: bool,
}

impl RustGameLogicHandler {
    pub fn new(ctx: SessionContext) -> Self {
        let swing_next = ctx.combat_state.next_swing;
        Self {
            ctx,
            cast_timer: Box::pin(tokio::time::sleep_until(far_future_instant())),
            skill_timer: Box::pin(tokio::time::sleep_until(far_future_instant())),
            bandage_timer: Box::pin(tokio::time::sleep_until(far_future_instant())),
            regen_timer: tokio::time::interval(Duration::from_millis(
                crate::constants::regen::TICK_INTERVAL_MS,
            )),
            swing_timer: Box::pin(tokio::time::sleep_until(swing_next)),
            pending_charge_check: false,
        }
    }

    /// Process a pending charge check (after world events).
    pub async fn flush_charge_check(
        &mut self,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        if self.pending_charge_check {
            self.pending_charge_check = false;
            self.ctx.try_charge_and_handle(
                &mut self.swing_timer, session, worker_tx,
            ).await?;
        }
        Ok(())
    }

    /// Process a charge check after movement / position change.
    pub async fn try_charge_after_move(
        &mut self,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        self.ctx.try_charge_and_handle(
            &mut self.swing_timer, session, worker_tx,
        ).await
    }
}

#[async_trait::async_trait]
impl GameLogicHandler for RustGameLogicHandler {
    async fn handle_packet(
        &mut self,
        parsed: &ParsedPacket,
        raw: &RawPacket,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<bool> {
        let ctx = &mut self.ctx;

        // ── Interrupt meditation on action packets ───────────────
        if let Some(msg_pkt) = ctx.maybe_interrupt_meditation(raw) {
            session.send(msg_pkt).await?;
        }

        match parsed {
            // ── TargetCursor (0x6C) — unified dispatch via PendingCursor ──
            ParsedPacket::TargetCursor(_) => {
                if let Some(pending) = ctx.infra.pending_cursor.take() {
                    let cursor_id = pending.cursor_id;
                    // Validate cursor ID before dispatching.
                    let cursor_id_matches = if let Ok(tc) = packets::interaction::TargetCursor::from_bytes(&raw.data) {
                        tc.cursor_id == pending.cursor_id
                    } else {
                        false
                    };

                    if cursor_id_matches {
                        if ctx.infra.dead && matches!(
                            pending.kind,
                            CursorKind::Spell { .. } | CursorKind::Skill { .. } | CursorKind::Bandage { .. } | CursorKind::GatherTarget { .. }
                            | CursorKind::TreasureSelectMap { .. } | CursorKind::TreasureDigTile { .. }
                        ) {
                            // Ghosts cannot complete spell/skill/bandage/gather targets.
                            return Ok(true);
                        }
                        match pending.kind {
                            CursorKind::Spell { .. } => {
                                let was_casting = ctx.active_cast.is_some();
                                let is_wall = matches!(
                                    pending.kind,
                                    CursorKind::Spell { spell, .. }
                                        if spell.id == crate::magic::spell_id::WALL_OF_STONE
                                );
                                if is_wall {
                                    spells::handle_wall_target(
                                        raw, pending, ctx, &mut self.cast_timer, session, worker_tx,
                                    ).await?;
                                } else {
                                    spells::handle_spell_target(
                                        raw, pending, ctx, &mut self.cast_timer, session, worker_tx,
                                    ).await?;
                                }
                                ctx.maybe_set_weapon_away(was_casting, ctx.active_cast.is_some());
                            }
                            CursorKind::Skill { .. } => {
                                let was_using = ctx.active_skill.is_some();
                                spells::handle_skill_target(
                                    raw, pending, ctx, &mut self.skill_timer, session,
                                ).await?;
                                ctx.maybe_set_weapon_away(was_using, ctx.active_skill.is_some());
                            }
                            CursorKind::Bandage { .. } => {
                                let was_bandaging = ctx.active_bandage.is_some();
                                bandage::handle_bandage_target(
                                    raw, pending, ctx, &mut self.bandage_timer, session, worker_tx,
                                ).await?;
                                ctx.maybe_set_weapon_away(was_bandaging, ctx.active_bandage.is_some());
                            }
                            CursorKind::DotCommand(_) => {
                                // DotCommand cursors are handled earlier in session_loop
                                // (before the game-logic handler). This should not happen,
                                // but if it does, just drop the cursor.
                            }
                            CursorKind::SpawnerPlacement { .. } => {
                                // `.spawner` placement cursors are handled earlier in
                                // session_loop (in handle_dot_commands).  Drop here.
                            }
                            CursorKind::HousePlacement { multi_id, deed_serial } => {
                                super::housing::handle_placement_cursor(
                                    raw, cursor_id, multi_id, deed_serial,
                                    &mut ctx.infra, session, worker_tx,
                                ).await?;
                            }
                            CursorKind::ShipPlacement { multi_id, deed_serial } => {
                                super::shipping::handle_placement_cursor(
                                    raw, cursor_id, multi_id, deed_serial,
                                    &mut ctx.infra, session, worker_tx,
                                ).await?;
                            }
                            CursorKind::PoisonSelectBottle { .. } => {
                                super::poison::handle_poison_bottle_target(
                                    raw, pending, ctx, session, worker_tx,
                                ).await?;
                            }
                            CursorKind::PoisonSelectWeapon { .. } => {
                                let was_using = ctx.active_skill.is_some();
                                super::poison::handle_poison_weapon_target(
                                    raw, pending, ctx, &mut self.skill_timer, session, worker_tx,
                                ).await?;
                                ctx.maybe_set_weapon_away(was_using, ctx.active_skill.is_some());
                            }
                            CursorKind::GatherTarget { .. } => {
                                let was_using = ctx.active_skill.is_some();
                                gather::handle_gather_target(
                                    raw, pending, ctx, &mut self.skill_timer, session, worker_tx,
                                ).await?;
                                ctx.maybe_set_weapon_away(was_using, ctx.active_skill.is_some());
                            }
                            CursorKind::ShrinkSelectAnimal { .. } => {
                                super::shrink::handle_shrink_target(
                                    raw, pending, ctx, session, worker_tx,
                                ).await?;
                            }
                            CursorKind::TreasureSelectMap { .. } => {
                                treasure::handle_treasure_select_map_target(
                                    raw, pending, ctx, session, worker_tx,
                                ).await?;
                            }
                            CursorKind::TreasureDigTile { .. } => {
                                let was_using = ctx.active_skill.is_some();
                                treasure::handle_treasure_dig_tile_target(
                                    raw, pending, ctx, &mut self.skill_timer, session, worker_tx,
                                ).await?;
                                ctx.maybe_set_weapon_away(was_using, ctx.active_skill.is_some());
                            }
                            CursorKind::Controller => {
                                // Controller cursors are only used in controller-session
                                // mode. If one appears here, just drop it.
                            }
                        }
                        return Ok(true);
                    } else {
                        // Cursor ID mismatch — stale response, drop it.
                        // Don't restore: the client has already moved on.
                        return Ok(true);
                    }
                }
                Ok(false)
            }

            // ── TextCommand (0x12) — spells, skills, emotes ──────────
            ParsedPacket::TextCommand(_) => {
                if ctx.infra.dead {
                    session.send(crate::game_util::system_message(
                        "You are dead and cannot do that.",
                    )).await?;
                    return Ok(true);
                }
                let was_casting = ctx.active_cast.is_some();
                let was_using_skill = ctx.active_skill.is_some();
                if spells::handle_text_command(
                    raw, ctx, &mut self.cast_timer, &mut self.skill_timer,
                    session, worker_tx,
                ).await? {
                    if (!was_casting && ctx.active_cast.is_some())
                        || (!was_using_skill && ctx.active_skill.is_some())
                    {
                        ctx.combat_state.set_weapon_away();
                    }
                    return Ok(true);
                }
                Ok(false)
            }

            // ── CastTargetedSpell (0xBF:0x002D) ─────────────────────
            ParsedPacket::CastTargetedSpell { .. } => {
                if ctx.infra.dead {
                    session.send(crate::game_util::system_message(
                        "You are dead and cannot do that.",
                    )).await?;
                    return Ok(true);
                }
                let was_casting = ctx.active_cast.is_some();
                if spells::handle_general_info_spell(
                    raw, ctx, &mut self.cast_timer, session, worker_tx,
                ).await? {
                    ctx.maybe_set_weapon_away(was_casting, ctx.active_cast.is_some());
                    return Ok(true);
                }
                Ok(false)
            }

            // ── DoubleClick (0x06) — chain: mount → scroll → bandage ─
            ParsedPacket::DoubleClick { paperdoll: false, .. } => {
                // Ghosts may still open containers (e.g. their own corpse),
                // but cannot mount, use scrolls, or apply bandages.
                if ctx.infra.dead {
                    return Ok(false); // fall through to infra (containers)
                }
                if mount::handle_mount_double_click(
                    raw, &ctx.infra.player, session, worker_tx, &ctx.serial_alloc,
                ).await? {
                    return Ok(true);
                }
                {
                    let was_pending = ctx.has_pending_cursor();
                    if scrolls::handle_scroll_double_click(
                        raw, ctx, session, worker_tx,
                    ).await? {
                        if !was_pending && ctx.has_pending_cursor() {
                            ctx.combat_state.set_weapon_away();
                        }
                        return Ok(true);
                    }
                }
                if super::spellbook::handle_spellbook_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                if bandage::handle_bandage_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                if potions::handle_potion_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                if super::shrink::handle_statue_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                if gather::handle_tool_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                // Treasure hunting: decode tattered maps, open treasure maps,
                // dig with the digging tool.
                if treasure::handle_tattered_map_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                if treasure::handle_treasure_map_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                if treasure::handle_digging_tool_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                // Smelting: double-click ore near a forge.
                if crafting::handle_ore_double_click(
                    raw, ctx, &mut self.skill_timer, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                // Blacksmithing: double-click a smith's hammer near an anvil.
                if crafting::handle_hammer_double_click(
                    raw, ctx, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                // House interactions: deed (place), sign (manage), door (toggle).
                if let ParsedPacket::DoubleClick { serial, .. } = parsed {
                    if super::housing::handle_deed_double_click(
                        *serial, &mut ctx.infra, session, worker_tx,
                    ).await? {
                        return Ok(true);
                    }
                    if super::housing::handle_sign_double_click(
                        *serial, &mut ctx.infra, session, worker_tx,
                    ).await? {
                        return Ok(true);
                    }
                    if super::housing::handle_door_double_click(
                        *serial, &ctx.infra, session, worker_tx,
                    ).await? {
                        return Ok(true);
                    }
                    // Ship interactions: deed (place on water), ship (re-deed).
                    if super::shipping::handle_deed_double_click(
                        *serial, &mut ctx.infra, session, worker_tx,
                    ).await? {
                        return Ok(true);
                    }
                    // Ship components: plank toggle / tillerman (hold falls
                    // through to the container-open path in infra).
                    if super::shipping::handle_component_double_click(
                        *serial, &ctx.infra, session, worker_tx,
                    ).await? {
                        return Ok(true);
                    }
                    if super::shipping::handle_ship_double_click(
                        *serial, &ctx.infra, session, worker_tx,
                    ).await? {
                        return Ok(true);
                    }
                }
                Ok(false) // falls through to infra (containers)
            }

            // ── WarMode (0x72) ───────────────────────────────────────
            ParsedPacket::WarMode { .. } => {
                ctx.handle_war_mode(raw, &mut self.swing_timer, session).await
            }

            // ── AttackRequest (0x05) ─────────────────────────────────
            ParsedPacket::AttackRequest { .. } => {
                ctx.handle_attack_request(raw, &mut self.swing_timer, session).await
            }

            // ── BuyItems (0x3B) — vendor purchase ────────────────────
            ParsedPacket::BuyItems { vendor_id, items } => {
                let infra = &mut ctx.infra;
                if let Some(player) = infra.player.as_ref() {
                    super::vendor_session::handle_buy(
                        *vendor_id, items, player,
                        &mut infra.open_vendor,
                        session, worker_tx,
                    ).await?;
                }
                Ok(true)
            }

            // ── SellListReply (0x9F) — vendor sale ───────────────────
            ParsedPacket::SellReply { shopkeeper_id, items } => {
                let infra = &ctx.infra;
                if let Some(player) = infra.player.as_ref() {
                    super::vendor_session::handle_sell(
                        *shopkeeper_id, items, player,
                        session, worker_tx,
                    ).await?;
                }
                Ok(true)
            }

            // ── GumpMenuSelection (0xB1) — house / craft gumps ──────
            ParsedPacket::GumpMenuSelection { gump_id, button_id, .. } => {
                if crafting::handle_craft_gump(
                    *gump_id, *button_id, ctx, &mut self.skill_timer, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                if super::housing::handle_house_gump(
                    *gump_id, *button_id, &mut ctx.infra, session, worker_tx,
                ).await? {
                    return Ok(true);
                }
                Ok(false) // not ours — let infra forward to controller
            }

            // Not game-logic — let infra handle it.
            _ => Ok(false),
        }
    }

    fn handle_world_events(
        &mut self,
        events: &[Arc<WorldEvent>],
        out: &mut Vec<RawPacket>,
    ) {
        let mut trigger_charge_check = false;

        for event in events {
            let had_cast = self.ctx.active_cast.is_some();

            self.ctx.check_action_interrupt(
                event, &mut self.swing_timer, &mut trigger_charge_check, out,
            );

            if had_cast && self.ctx.active_cast.is_none() {
                self.cast_timer.as_mut().reset(far_future_instant());
            }
        }

        // Store pending charge check for the caller to process
        // (via try_charge_and_handle after sending the batch).
        if trigger_charge_check {
            self.pending_charge_check = true;
        }
    }

    async fn poll_timer(&mut self) -> TimerEvent {
        let ctx = &self.ctx;

        tokio::select! {
            biased;

            _ = &mut self.cast_timer, if ctx.active_cast.is_some() => {
                TimerEvent::CastComplete
            }

            _ = &mut self.skill_timer, if ctx.active_skill.is_some() => {
                TimerEvent::SkillComplete
            }

            _ = &mut self.bandage_timer, if ctx.active_bandage.is_some() => {
                TimerEvent::BandageComplete
            }

            _ = self.regen_timer.tick() => {
                TimerEvent::RegenTick
            }

            _ = &mut self.swing_timer, if ctx.combat_state.has_targets() && !ctx.combat_state.charged => {
                TimerEvent::SwingCharged
            }

            _ = std::future::pending::<()>() => {
                unreachable!()
            }
        }
    }

    async fn handle_timer_event(
        &mut self,
        event: TimerEvent,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        match event {
            TimerEvent::CastComplete => {
                let action = self.ctx.active_cast.take().unwrap();
                self.cast_timer.as_mut().reset(far_future_instant());

                if let ActionPayload::WallOfStone {
                    caster_serial, target_x, target_y, target_z, world,
                } = action.payload {
                    let serial_alloc = self.ctx.serial_alloc.clone();
                    crate::magic::complete_wall_of_stone(
                        caster_serial, target_x, target_y, target_z, world,
                        &serial_alloc, session, worker_tx,
                    ).await?;
                    self.ctx.complete_action_recovery();
                    return Ok(());
                }

                if let ActionPayload::SpellCast {
                    spell, caster_serial, target_serial, world, scroll_item_serial,
                } = action.payload {
                    // Mark / Recall operate on a rune item in the caster's
                    // backpack, not a world entity — they resolve outside the
                    // generic `complete_cast` path.
                    use crate::magic::spell_id;
                    if spell.id == spell_id::MARK {
                        super::recall::complete_mark(
                            spell, caster_serial, target_serial, world, session, worker_tx,
                        ).await?;
                        self.ctx.complete_action_recovery();
                        return Ok(());
                    }
                    if spell.id == spell_id::RECALL {
                        let pending = if let Some(player) = self.ctx.infra.player.as_mut() {
                            super::recall::complete_recall(
                                spell, target_serial, player, session, worker_tx,
                            ).await?
                        } else {
                            None
                        };
                        if pending.is_some() {
                            self.ctx.infra.pending_teleport = pending;
                        }
                        self.ctx.complete_action_recovery();
                        return Ok(());
                    }

                    let result = crate::magic::complete_cast(
                        spell, caster_serial, target_serial, worker_tx, world,
                        scroll_item_serial,
                    ).await;
                    for pkt in result.packets {
                        session.send(pkt).await?;
                    }
                    // Apply stat buff/debuff if the spell has one (Bless, Curse).
                    if let Some(eff) = result.stat_effect {
                        // Revert existing buff of the same kind, then apply new.
                        if let Some(old_delta) = self.ctx.buff_state.add_buff(
                            eff.buff_kind, eff.delta, eff.duration_ms,
                        ) {
                            crate::buffs::revert_buff_stat(
                                worker_tx, world, eff.target_serial,
                                eff.buff_kind, old_delta,
                            ).await;
                        }
                        crate::buffs::apply_buff_stat(
                            worker_tx, world, eff.target_serial,
                            eff.buff_kind, eff.delta,
                        ).await;
                    }
                }
                self.ctx.complete_action_recovery();
            }

            TimerEvent::SkillComplete => {
                let action = self.ctx.active_skill.take().unwrap();
                self.skill_timer.as_mut().reset(far_future_instant());

                match action.payload {
                    ActionPayload::SkillUse {
                        skill_id, user_serial, target_serial, world,
                    } => {
                        spells::complete_skill_use(
                            skill_id, user_serial, target_serial, world, session, worker_tx,
                        ).await?;
                    }
                    ActionPayload::Poisoning {
                        user_serial, weapon_serial, potion_serial, level, world,
                    } => {
                        super::poison::apply_poison_to_weapon(
                            user_serial, weapon_serial, potion_serial, level, world,
                            session, worker_tx,
                        ).await?;
                    }
                    ActionPayload::Gather {
                        user_serial, tool_graphic, target_x, target_y, target_z,
                        source_graphic, source_serial, world,
                    } => {
                        let serial_alloc = self.ctx.serial_alloc.clone();
                        gather::complete_gather(
                            user_serial, tool_graphic, target_x, target_y, target_z,
                            source_graphic, source_serial, world,
                            &serial_alloc, session, worker_tx,
                        ).await?;
                    }
                    ActionPayload::Smelt { user_serial, ore_serial, world } => {
                        let serial_alloc = self.ctx.serial_alloc.clone();
                        crafting::complete_smelt(
                            user_serial, ore_serial, world,
                            &serial_alloc, session, worker_tx,
                        ).await?;
                    }
                    ActionPayload::Craft { user_serial, recipe_key, world } => {
                        let serial_alloc = self.ctx.serial_alloc.clone();
                        crafting::complete_craft(
                            user_serial, recipe_key, world,
                            &serial_alloc, session, worker_tx,
                        ).await?;
                    }
                    ActionPayload::TreasureDig {
                        user_serial, tool_serial, map_serial, level,
                        target_x, target_y, target_z, world,
                    } => {
                        let serial_alloc = self.ctx.serial_alloc.clone();
                        treasure::complete_treasure_dig(
                            user_serial, tool_serial, map_serial, level,
                            target_x, target_y, target_z, world,
                            &serial_alloc, session, worker_tx,
                        ).await?;
                    }
                    _ => {}
                }
                self.ctx.complete_action_recovery();
            }

            TimerEvent::BandageComplete => {
                let action = self.ctx.active_bandage.take().unwrap();
                self.bandage_timer.as_mut().reset(far_future_instant());

                if let ActionPayload::Bandage {
                    healer_serial, target_serial, bandage_item_serial, world,
                } = action.payload {
                    bandage::complete_bandage(
                        healer_serial, target_serial, bandage_item_serial,
                        world, session, worker_tx,
                    ).await?;

                    if let Some(p) = &self.ctx.infra.player {
                        let held_info = self.ctx.held_item_info();
                        util::send_weight_update(p, held_info, session, worker_tx).await?;
                    }
                }
                self.ctx.complete_action_recovery();
            }

            TimerEvent::RegenTick => {
                // Ghosts do not regenerate stats until resurrected.
                if !self.ctx.infra.dead {
                    if let Some(p) = &self.ctx.infra.player {
                        let serial = p.serial;
                        let world = p.world;
                        self.ctx.regen_state.tick(serial, world, worker_tx).await;
                        // Expire any stale buffs (strength, agility, etc.).
                        crate::buffs::tick_buffs(
                            &mut self.ctx.buff_state, worker_tx, world, serial,
                        ).await;
                    }
                }
            }

            TimerEvent::SwingCharged => {
                self.ctx.combat_state.charged = true;

                if self.ctx.infra.player.is_some() {
                    self.ctx.try_charge_and_handle(
                        &mut self.swing_timer, session, worker_tx,
                    ).await?;
                }

                if self.ctx.combat_state.charged {
                    self.swing_timer.as_mut().reset(Instant::now() + FAR_FUTURE);
                } else {
                    self.ctx.sync_swing_timer(&mut self.swing_timer);
                }
            }

            TimerEvent::LuaAction => {} // Not used in Rust handler.
        }
        Ok(())
    }

    async fn shutdown(&mut self) {
        // Nothing to clean up for Rust handler.
    }

    // ── Infrastructure state access ────────────────────────────────

    fn infra(&self) -> &super::game_logic::InfraState { &self.ctx.infra }
    fn infra_mut(&mut self) -> &mut super::game_logic::InfraState { &mut self.ctx.infra }

    // ── Hooks ────────────────────────────────────────────────────────

    async fn post_world_events(
        &mut self,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        self.ctx.apply_pending_ghost_visibility(worker_tx).await;
        self.flush_charge_check(session, worker_tx).await
    }

    async fn post_packet(
        &mut self,
        session: &mut Session,
        worker_tx: &DemoWorkerTx,
    ) -> error::Result<()> {
        self.ctx.apply_pending_ghost_visibility(worker_tx).await;
        // Close the vendor buy/sell window if the player walked away.
        // The buy/sell lists are not container gumps, so we only drop the
        // server-side state — the client closes them on its own.
        if let Some(vs) = self.ctx.infra.open_vendor.as_ref() {
            if let Some(p) = self.ctx.infra.player.as_ref() {
                if p.x != vs.open_x || p.y != vs.open_y {
                    self.ctx.infra.open_vendor = None;
                }
            }
        }
        self.try_charge_after_move(session, worker_tx).await
    }
}
