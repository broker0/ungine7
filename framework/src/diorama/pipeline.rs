//! [`ObserverPipeline`] — unified single-pass S→C / C→S packet processor.
//!
//! Combines [`SessionView`] (visible set, multi registry, world tracking)
//! with [`PositionTracker`] and movement prediction into a single struct.
//! Each incoming S→C packet is delegated to `SessionView::ingest_packet`
//! for visible/multi/world handling, then movement and position logic is
//! applied on top — no duplicated dispatch.
//!
//! # Usage
//!
//! ```ignore
 //! let mut observer = ObserverPipeline::new(Some(static_data));
 //!
 //! // C→S: queue pending move requests
 //! observer.ingest_c2s(move_request_bytes);
 //!
 //! // S→C: update all state in one pass
 //! observer.ingest_s2c(server_packet_bytes);
//! ```
//!
//! # Relationship to standalone components
//!
//! [`SessionView`] retains its own public `ingest_packet` method and can
 //! be accessed directly via `observer.session` for use cases where only
 //! session-level tracking is needed (e.g. standalone playback).
 //! [`PositionTracker`] is accessible via `observer.pos`.

use std::path::PathBuf;
use std::sync::Arc;

use log::{debug, warn};

use u_core::Facing;
use packets::character::{CharacterAnimation, CharacterLocaleAndBody, DrawGamePlayer, UpdateMobile};
use packets::interaction::DeleteObject;
use packets::movement::{MoveAck, MoveReject, MoveRequest};
use packets::speech::{ClilocMessage, SendSpeech, UnicodeSpeech};
use packets::status::{UpdateHealth, UpdateMana, UpdateStamina};
use packets::system::{
    ClientViewRange, GeneralInfo, OverallLightLevel, PlayMidiMusic, PlaySoundEffect,
    SeasonalInformation, SetWeather,
};
use packets::traits::{ManualPacket, BasicPacket};
use packets::world::{DrawMobile, GraphicalEffect, ObjectInfo, ObjectInfoSA};

use crate::ecumene::{MovementValidator, StaticDataProvider, TileRect};
use crate::rythmos::pending_queue::{AckOutcome, PendingQueue};
use crate::rythmos::position_tracker::PositionTracker;
use super::observer_event::ObserverEvent;
use super::session_view::SessionView;
use super::composite_tiles::CompositeTileProvider;

// ── DrainReason ──────────────────────────────────────────────────────────────

/// Why the pending-move queue was last drained/cleared.
///
/// Stored after every queue-clearing event so that downstream diagnostics
/// (e.g. the replay preprocessor) can report *why* a subsequent `MoveAck`
/// found the queue empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainReason {
    /// No drain has occurred yet (or the queue was never populated).
    None,
    /// `DrawGamePlayer (0x20)` — server sent an authoritative position update.
    DrawGamePlayer,
    /// `MoveReject (0x21)` — server rejected a pending move.
    MoveReject,
    /// `MoveAck (0x22)` sequence mismatch — all pending entries drained to
    /// resynchronise.
    MoveAckDesync,
    /// `SetMap (0xBF sub 0x0008)` — world/facet change.
    SetMap,
    /// Explicit `reset()` call.
    Reset,
}

// ── ObserverPipeline ─────────────────────────────────────────────────────────

/// Unified packet-driven world observer.
///
/// Contains a [`SessionView`] for visible/multi/world tracking and adds
/// position tracking + movement prediction on top.  After each
/// [`ingest_s2c`](Self::ingest_s2c) call, typed [`ObserverEvent`]s are
/// accumulated in an internal buffer and can be retrieved via
/// [`drain_events`](Self::drain_events).
#[derive(Clone)]
pub struct ObserverPipeline {
    // ── Position & movement ──────────────────────────────────────────
    /// Player position, appearance, and serial.
    pub pos: PositionTracker,

    /// Queue of `(sequence, facing)` from C→S `MoveRequest` packets that
    /// have not yet been matched with a S→C `MoveAck`.
    pending_moves: PendingQueue<Facing>,

    /// Set to `true` after a `MoveAck` successfully matched the head of
    /// `pending_moves` and `pos.step()` was called.  Reset to `false`
    /// at the start of every [`ingest_s2c`](Self::ingest_s2c) call.
    pub last_move_accepted: bool,

    /// Why the pending-move queue was last drained.  Updated whenever
    /// the queue is cleared (by `DrawGamePlayer`, `MoveReject`, `SetMap`,
    /// or a `MoveAck` desync).  Useful for diagnosing "queue empty" warnings
    /// in downstream consumers (e.g. the replay preprocessor).
    pub last_drain_reason: DrainReason,

    /// Number of entries that were in the queue at the moment of the last
    /// drain.  Zero means the queue was already empty.
    pub last_drain_count: usize,

    // ── Session-level observation ────────────────────────────────────
    /// Current world, visible objects, multi registry, feature flags.
    pub session: SessionView,

    // ── Event buffer ─────────────────────────────────────────────────
    /// Typed events emitted during [`ingest_s2c`](Self::ingest_s2c).
    /// Consumers retrieve them via [`drain_events`](Self::drain_events).
    pending_events: Vec<ObserverEvent>,
}

// ── Constructors ──────────────────────────────────────────────────────────

impl ObserverPipeline {
    /// Create a new pipeline in the initial (empty) state.
    pub fn new(static_data: Option<Arc<dyn StaticDataProvider>>) -> Self {
        let session = match static_data {
            Some(sd) => SessionView::with_static_data(
                0, 0, ClientViewRange::DEFAULT as u16, sd,
            ),
            None => SessionView::new(0, 0, ClientViewRange::DEFAULT as u16),
        };
        Self {
            pos: PositionTracker::default(),
            pending_moves: PendingQueue::new(),
            last_move_accepted: false,
            last_drain_reason: DrainReason::None,
            last_drain_count: 0,
            session,
            pending_events: Vec::with_capacity(4),
        }
    }

    /// Create a new pipeline with a data directory for loading diff files.
    pub fn with_data_dir(
        static_data: Arc<dyn StaticDataProvider>,
        data_dir: PathBuf,
    ) -> Self {
        let session = SessionView::with_data_dir(
            0, 0, ClientViewRange::DEFAULT as u16, static_data, data_dir,
        );
        Self {
            pos: PositionTracker::default(),
            pending_moves: PendingQueue::new(),
            last_move_accepted: false,
            last_drain_reason: DrainReason::None,
            last_drain_count: 0,
            session,
            pending_events: Vec::with_capacity(4),
        }
    }

    /// Reset all state back to initial (empty) values.
    ///
    /// The session's static data reference is preserved.
    pub fn reset(&mut self) {
        let sd = self.session.registry.static_data().cloned();
        *self = Self::new(sd);
    }

    // ── Accessors ─────────────────────────────────────────────────────

    /// Read-only access to the pending move queue (for diagnostics).
    #[inline]
    pub fn pending_moves(&self) -> &PendingQueue<Facing> {
        &self.pending_moves
    }

    /// Drain the pending-move queue.
    pub fn clear_pending_moves(&mut self) {
        self.pending_moves.clear();
    }

    /// Current view rectangle (delegates to session).
    pub fn view_rect(&self) -> &TileRect {
        self.session.view_rect()
    }

    /// Current view range (delegates to session).
    pub fn view_range(&self) -> u16 {
        self.session.view_range()
    }

    /// Drain all pending [`ObserverEvent`]s accumulated since the last drain.
    ///
    /// Typically called after [`ingest_s2c`](Self::ingest_s2c) to retrieve
    /// events for broadcasting to scripts, WebSocket observers, etc.
    pub fn drain_events(&mut self) -> std::vec::Drain<'_, ObserverEvent> {
        self.pending_events.drain(..)
    }

    // ── C→S processing ───────────────────────────────────────────────

    /// Process a C→S packet.
    ///
    /// Recognises `MoveRequest (0x02)` and queues the sequence + direction
    /// for later matching.  All other packets are ignored.
    pub fn ingest_c2s(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if data[0] == MoveRequest::ID {
            if let Ok(req) = MoveRequest::from_bytes(data) {
                self.pending_moves
                    .push(req.sequence, Facing::new(req.direction));
            }
        }
    }

    // ── S→C processing ───────────────────────────────────────────────

    /// Process an S→C packet through all subsystems.
    ///
    /// 1. Delegates visible set, multi registry, world, view range, and
    ///    feature flags to [`SessionView::ingest_packet`] (single dispatch).
    /// 2. Applies position and movement logic on top (position-carrying
    ///    packets, `MoveAck`/`MoveReject`, `DrawGamePlayer` drain, `SetMap`
    ///    pending-move drain).
    /// 3. Emits typed [`ObserverEvent`]s into the internal buffer
    ///    (retrievable via [`drain_events`](Self::drain_events)).
    pub fn ingest_s2c(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        self.last_move_accepted = false;

        // ── Step 1: session handles visible + multi + world + view ────
        self.session.ingest_packet(data);

        // ── Step 2: position + movement ──────────────────────────────
        match data[0] {
            // ── CharacterLocaleAndBody (0x1B) ────────────────────────
            id if id == CharacterLocaleAndBody::ID => {
                if let Ok(p) = CharacterLocaleAndBody::from_bytes(data) {
                    self.pos.apply_character_locale(&p);
                }
            }

            // ── DrawGamePlayer (0x20) ────────────────────────────────
            id if id == DrawGamePlayer::ID => {
                if let Ok(p) = DrawGamePlayer::from_bytes(data) {
                    self.pos.apply_draw_game_player(&p);
                }
                if !self.pending_moves.is_empty() {
                    let count = self.pending_moves.len();
                    debug!(
                        "[pipeline] 0x20 DrawGamePlayer — draining {} pending moves",
                        count,
                    );
                    self.last_drain_reason = DrainReason::DrawGamePlayer;
                    self.last_drain_count = count;
                    self.pending_moves.clear();
                }
            }

            // ── MoveReject (0x21) ────────────────────────────────────
            id if id == MoveReject::ID => {
                if let Ok(rej) = MoveReject::from_bytes(data) {
                    if !self.pending_moves.is_empty() {
                        let count = self.pending_moves.len();
                        debug!(
                            "[pipeline] 0x21 MoveReject seq={} ({},{},{}) — draining {} pending moves",
                            rej.sequence, rej.x, rej.y, rej.z,
                            count,
                        );
                        self.last_drain_reason = DrainReason::MoveReject;
                        self.last_drain_count = count;
                        self.pending_moves.clear();
                    }
                    self.pos.x = rej.x;
                    self.pos.y = rej.y;
                    self.pos.z = rej.z;
                    self.pos.facing = Facing::new(rej.direction);
                }
            }

            // ── MoveAck (0x22) ───────────────────────────────────────
            id if id == MoveAck::ID => {
                if let Ok(ack) = MoveAck::from_bytes(data) {
                    match self.pending_moves.on_ack(ack.sequence) {
                        AckOutcome::Matched(direction) => {
                            let stepped = self.pos.step(direction);
                            if stepped {
                                self.resolve_z();
                            }
                            self.last_move_accepted = true;
                        }
                        AckOutcome::Desync(drained) => {
                            if drained.is_empty() {
                                warn!(
                                    "[pipeline] MoveAck seq={} — queue empty, ignoring \
                                     (last drain: {:?}, drained {} entries)",
                                    ack.sequence,
                                    self.last_drain_reason,
                                    self.last_drain_count,
                                );
                            } else {
                                let count = drained.len();
                                warn!(
                                    "[pipeline] MoveAck seq={} — desync, \
                                     drained {} pending moves",
                                    ack.sequence, count,
                                );
                                self.last_drain_reason = DrainReason::MoveAckDesync;
                                self.last_drain_count = count;
                            }
                        }
                    }
                }
            }

            // ── UpdateMobile (0x77) — position update for self ───────
            id if id == UpdateMobile::ID => {
                if let Ok(m) = UpdateMobile::from_bytes(data) {
                    self.pos.apply_update_mobile(&m);
                }
            }

            // ── DrawMobile (0x78) — position update for self ─────────
            // (visible set upsert already handled by session above)
            id if id == DrawMobile::ID => {
                if let Ok(mob) = DrawMobile::parse(data, false) {
                    self.pos.apply_draw_mobile(&mob);
                }
            }

            // ── SetMap (0xBF sub 0x0008) — drain pending moves on world change
            // (session already updated current_world, cleared visible/multi)
            //
            // IMPORTANT: only sub 0x0008 triggers a drain.  0xBF is a
            // container for dozens of subcommands (ExtendedStats, Party,
            // HouseRevision, ContextMenu, …) — draining on every 0xBF
            // would incorrectly discard pending moves whenever any of
            // those unrelated packets arrives between a MoveRequest and
            // its MoveAck.
            0xBF => {
                if data.len() >= 5 {
                    let sub = u16::from_be_bytes([data[3], data[4]]);
                    if sub == 0x0008 && !self.pending_moves.is_empty() {
                        let count = self.pending_moves.len();
                        debug!(
                            "[pipeline] 0xBF:0008 SetMap — draining {} pending moves",
                            count,
                        );
                        self.last_drain_reason = DrainReason::SetMap;
                        self.last_drain_count = count;
                        self.pending_moves.clear();
                    }
                }
            }

            _ => {}
        }

        // ── Step 3: emit typed events ────────────────────────────────
        self.emit_events(data);
    }

    // ── Event emission ─────────────────────────────────────────────

    /// Parse additional fields from the raw S→C packet and push typed
    /// [`ObserverEvent`]s into `pending_events`.
    ///
    /// Called at the end of [`ingest_s2c`](Self::ingest_s2c) after both
    /// session and position state have been updated.  Entity lifecycle
    /// packets (0x78, 0x77, 0x1D, 0x1A, 0xF3) are re-parsed from bytes
    /// rather than reading from `SessionView` — this avoids coupling to
    /// `WorldEntity` internals and keeps the code straightforward.
    /// Fire-and-forget packets (sounds, effects, speech, stats) are parsed
    /// here for the first time.
    fn emit_events(&mut self, data: &[u8]) {
        let pkt_id = data[0];

        match pkt_id {
            // ── 0x78 DrawMobile ───────────────────────────────────────
            id if id == DrawMobile::ID => {
                if let Ok(mob) = DrawMobile::parse(data, false) {
                    self.pending_events.push(ObserverEvent::MobileAppeared {
                        serial: mob.serial,
                        graphic: mob.graphic,
                        color: mob.color,
                        x: mob.x,
                        y: mob.y,
                        z: mob.z,
                        direction: mob.direction,
                        notoriety: mob.notoriety.to_wire(),
                    });
                }
            }

            // ── 0x77 UpdateMobile ─────────────────────────────────────
            id if id == UpdateMobile::ID => {
                if let Ok(mob) = UpdateMobile::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::MobileMoved {
                        serial: mob.serial,
                        graphic: mob.model,
                        color: mob.hue,
                        x: mob.x,
                        y: mob.y,
                        z: mob.z,
                        direction: mob.direction,
                        notoriety: mob.notoriety.to_wire(),
                    });
                }
            }

            // ── 0x1D DeleteObject ─────────────────────────────────────
            id if id == DeleteObject::ID => {
                if let Ok(del) = DeleteObject::from_bytes(data) {
                    let serial = del.serial;
                    if serial >= 0x40000000 {
                        self.pending_events.push(ObserverEvent::ItemRemoved { serial });
                    } else {
                        self.pending_events.push(ObserverEvent::MobileRemoved { serial });
                    }
                }
            }

            // ── 0x1A ObjectInfo (item) ────────────────────────────────
            id if id == ObjectInfo::ID => {
                if let Ok(obj) = ObjectInfo::from_bytes(data) {
                    let serial = obj.object_id & 0x7FFFFFFF;
                    self.pending_events.push(ObserverEvent::ItemAppeared {
                        serial,
                        graphic: obj.graphic & 0x7FFF,
                        color: obj.dye.unwrap_or(0),
                        x: obj.x & 0x7FFF,
                        y: obj.y & 0x3FFF,
                        z: obj.z,
                        count: obj.amount.unwrap_or(1),
                    });
                }
            }

            // ── 0xF3 ObjectInfoSA (item) ──────────────────────────────
            id if id == ObjectInfoSA::ID => {
                if let Ok(obj) = ObjectInfoSA::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::ItemAppeared {
                        serial: obj.serial,
                        graphic: obj.graphic,
                        color: obj.hue,
                        x: obj.x,
                        y: obj.y,
                        z: obj.z,
                        count: obj.amount,
                    });
                }
            }

            // ── 0x20 DrawGamePlayer (own position) ────────────────────
            id if id == DrawGamePlayer::ID => {
                if let Ok(dgp) = DrawGamePlayer::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::PositionChanged {
                        x: dgp.x,
                        y: dgp.y,
                        z: dgp.z,
                        direction: dgp.direction,
                    });
                }
            }

            // ── 0x54 PlaySoundEffect ──────────────────────────────────
            id if id == PlaySoundEffect::ID => {
                if let Ok(snd) = PlaySoundEffect::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::SoundPlayed {
                        sound_id: snd.sound_model,
                        x: snd.x,
                        y: snd.y,
                        z: snd.z,
                    });
                }
            }

            // ── 0x70 GraphicalEffect ──────────────────────────────────
            id if id == GraphicalEffect::ID => {
                if let Ok(eff) = GraphicalEffect::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::EffectPlayed {
                        direction_type: eff.direction_type,
                        source_serial: eff.source_serial,
                        target_serial: eff.target_serial,
                        graphic: eff.model,
                        x: eff.x,
                        y: eff.y,
                        z: eff.z,
                        target_x: eff.target_x,
                        target_y: eff.target_y,
                        target_z: eff.target_z,
                        speed: eff.speed,
                        duration: eff.duration,
                        fixed_direction: eff.fixed_direction != 0,
                        explode: eff.explode != 0,
                    });
                }
            }

            // ── 0x6E CharacterAnimation ───────────────────────────────
            id if id == CharacterAnimation::ID => {
                if let Ok(anim) = CharacterAnimation::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::AnimationPlayed {
                        serial: anim.serial,
                        action: anim.action,
                        frame_count: anim.frame_count,
                        repeat_count: anim.repeat_count,
                        reverse: anim.direction != 0,
                        repeat: anim.repeat_flag != 0,
                        frame_delay: anim.frame_delay,
                    });
                }
            }

            // ── 0x1C SendSpeech ───────────────────────────────────────
            id if id == SendSpeech::ID => {
                if let Ok(sp) = SendSpeech::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::Speech {
                        serial: sp.serial,
                        graphic: sp.model,
                        speech_type: sp.speech_type.to_wire(),
                        color: sp.color,
                        font: sp.font,
                        name: sp.name.clone(),
                        message: sp.message.clone(),
                    });
                }
            }

            // ── 0xAE UnicodeSpeech ────────────────────────────────────
            id if id == UnicodeSpeech::ID => {
                if let Ok(sp) = UnicodeSpeech::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::Speech {
                        serial: sp.serial,
                        graphic: sp.model,
                        speech_type: sp.speech_type.to_wire(),
                        color: sp.color,
                        font: sp.font,
                        name: sp.name.to_string(),
                        message: sp.message.to_string(),
                    });
                }
            }

            // ── 0xC1 ClilocMessage ────────────────────────────────────
            id if id == ClilocMessage::ID => {
                if let Ok(msg) = ClilocMessage::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::ClilocMessage {
                        serial: msg.serial,
                        cliloc_id: msg.message_number,
                        speech_type: msg.speech_type.to_wire(),
                        color: msg.hue,
                        font: msg.font,
                        name: msg.name.to_string(),
                        args: msg.arguments.clone(),
                    });
                }
            }

            // ── 0xBF GeneralInfo (Damage, CloseGump) ──────────────────
            0xBF => {
                if let Ok(gi) = GeneralInfo::from_bytes(data) {
                    match gi {
                        GeneralInfo::Damage { serial, damage } => {
                            self.pending_events.push(ObserverEvent::DamageDealt {
                                serial,
                                amount: damage,
                            });
                        }
                        GeneralInfo::CloseGump { dialog_id, .. } => {
                            self.pending_events.push(ObserverEvent::GumpClosed {
                                gump_id: dialog_id,
                            });
                        }
                        _ => {}
                    }
                }
            }

            // ── 0xA1 UpdateHealth ─────────────────────────────────────
            id if id == UpdateHealth::ID => {
                if let Ok(hp) = UpdateHealth::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::HpUpdated {
                        serial: hp.serial,
                        hits: hp.current_health,
                        max_hits: hp.max_health,
                    });
                }
            }

            // ── 0xA2 UpdateMana ───────────────────────────────────────
            id if id == UpdateMana::ID => {
                if let Ok(mp) = UpdateMana::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::ManaUpdated {
                        serial: mp.serial,
                        mana: mp.current_mana,
                        max_mana: mp.max_mana,
                    });
                }
            }

            // ── 0xA3 UpdateStamina ────────────────────────────────────
            id if id == UpdateStamina::ID => {
                if let Ok(sp) = UpdateStamina::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::StaminaUpdated {
                        serial: sp.serial,
                        stamina: sp.current_stamina,
                        max_stamina: sp.max_stamina,
                    });
                }
            }

            // ── 0x4F OverallLightLevel ────────────────────────────────
            id if id == OverallLightLevel::ID => {
                if let Ok(light) = OverallLightLevel::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::GlobalLight {
                        level: light.level,
                    });
                }
            }

            // ── 0x65 SetWeather ───────────────────────────────────────
            id if id == SetWeather::ID => {
                if let Ok(w) = SetWeather::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::Weather {
                        weather_type: w.weather_type,
                        num_effects: w.num_effects,
                        temperature: w.temperature,
                    });
                }
            }

            // ── 0xBC SeasonalInformation ──────────────────────────────
            id if id == SeasonalInformation::ID => {
                if let Ok(s) = SeasonalInformation::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::Season {
                        season: s.season.to_wire(),
                        play_sound: s.play_sound != 0,
                    });
                }
            }

            // ── 0x6D PlayMidiMusic ────────────────────────────────────
            id if id == PlayMidiMusic::ID => {
                if let Ok(m) = PlayMidiMusic::from_bytes(data) {
                    self.pending_events.push(ObserverEvent::Music {
                        music_id: m.music_id,
                    });
                }
            }

            _ => {}
        }
    }

    // ── Z resolution ─────────────────────────────────────────────────

    /// Resolve standing Z at the current position using static data +
    /// visible items + multi shapes from the session.
    ///
    /// If a [`DiffOverlay`](crate::ecumene::DiffOverlay) is active, diff-
    /// patched map/statics blocks are used transparently.
    fn resolve_z(&mut self) {
        let Some(sd) = self.session.registry.static_data() else {
            return;
        };

        let diff_overlay = if self.session.diff_overlay.is_empty() {
            None
        } else {
            debug!(
                "[pipeline] resolve_z: using diff overlay for world {}",
                self.session.current_world,
            );
            Some(&self.session.diff_overlay)
        };

        let provider = CompositeTileProvider::with_diff(
            sd.as_ref(),
            self.session.current_world,
            &self.session.visible,
            &self.session.registry,
            diff_overlay,
        );
        if let Some(new_z) = MovementValidator::new(&provider).resolve_standing_z(
            self.pos.x,
            self.pos.y,
            self.pos.z,
            self.pos.facing.heading(),
        ) {
            self.pos.z = new_z;
        }
    }
}
