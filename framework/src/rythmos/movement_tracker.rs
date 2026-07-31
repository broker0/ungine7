//! Minimal player-position tracker.
//!
//! [`MovementTracker`] processes raw S->C and C->S packet bytes and maintains
//! the authoritative position of the player character.  It handles:
//!
//! - `MoveRequest (0x02)` C->S -- queued for later matching with `MoveAck`.
//! - `MoveAck (0x22)` -- matched with the pending queue; applies a one-tile
//!   step and resolves Z via [`ZResolver`] when available.
//! - `MoveReject (0x21)` -- snaps position back to server-provided coords.
//! - `DrawGamePlayer (0x20)` -- drains the pending move queue.
//! - Position-carrying packets (`0x1B`, `0x20`, `0x77`, `0x78`) -- forwarded
//!   to [`PositionTracker::update_from_packet`].
//! - `SetMap (0xBF sub 0x0008)` -- updates `current_world` and drains
//!   pending moves.
//!
//! The tracker is deliberately minimal: no phase machine, no entity maps,
//! no owned world data.  It is designed to be embedded in higher-level
//! constructs such as `LogPlayer` or used standalone by analysis tools.
//!
//! # Z resolution
//!
//! When a [`ZResolver`] is passed to [`process_s2c`](MovementTracker::process_s2c),
//! Z resolution uses the resolver's implementation (e.g. terrain + visible
//! items + multi shapes).  Without a resolver, Z is left unchanged after
//! stepping (only X/Y move).
//!
//! **Important:** when using with a resolver backed by a `SessionView`,
//! make sure to call `session.ingest_packet(data)` **before**
//! `tracker.process_s2c(data, ...)` so that visible items and multi shapes
//! are up-to-date at the destination tile.

use log::{debug, warn};

use u_core::Facing;
use super::pending_queue::{AckOutcome, PendingQueue};
use packets::character::DrawGamePlayer;
use packets::movement::{MoveAck, MoveReject, MoveRequest};
use packets::system::GeneralInfo;
use packets::traits::{ManualPacket, BasicPacket};

use super::position_tracker::PositionTracker;
use super::z_resolver::ZResolver;

// -- MovementTracker ----------------------------------------------------------

/// Minimal player-position tracker.
///
/// Processes raw packet bytes and maintains the player's position,
/// pending-move queue, and current world index.
#[derive(Clone)]
pub struct MovementTracker {
    /// Player position, appearance, and serial.
    pub pos: PositionTracker,

    /// Current UO map index (0 = Felucca, 1 = Trammel, ...).
    ///
    /// Updated when a `0xBF SetMap` packet is processed.
    current_world: u8,

    /// Queue of `(sequence, facing)` from C->S `MoveRequest` packets that
    /// have not yet been matched with a S->C `MoveAck`.
    pending_moves: PendingQueue<Facing>,

    /// Set to `true` after a `MoveAck` successfully matched the head of
    /// `pending_moves` and `pos.step()` was called.  Reset to `false`
    /// at the start of every [`process_s2c`](Self::process_s2c) call.
    pub last_move_accepted: bool,
}

impl MovementTracker {
    /// Create a new tracker in the initial (empty) state.
    pub fn new() -> Self {
        Self {
            pos: PositionTracker::default(),
            current_world: 0,
            pending_moves: PendingQueue::new(),
            last_move_accepted: false,
        }
    }

    /// Reset all state (position, pending moves, world) back to defaults.
    pub fn reset(&mut self) {
        self.pos = PositionTracker::default();
        self.current_world = 0;
        self.pending_moves.clear();
        self.last_move_accepted = false;
    }

    /// Current world / map index.
    #[inline]
    pub fn current_world(&self) -> u8 {
        self.current_world
    }

    /// Read-only access to the pending move queue (for diagnostics).
    #[inline]
    pub fn pending_moves(&self) -> &PendingQueue<Facing> {
        &self.pending_moves
    }

    /// Drain the pending-move queue.
    ///
    /// Called by `LogPlayer` when the world changes via `SessionView` or
    /// when other events require a queue reset.
    pub fn clear_pending_moves(&mut self) {
        self.pending_moves.clear();
    }

    // -- C->S processing ------------------------------------------------------

    /// Process a C->S packet.
    ///
    /// Recognises `MoveRequest (0x02)` and queues the sequence + direction
    /// for later matching.  All other packets are ignored.
    pub fn process_c2s(&mut self, data: &[u8]) {
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

    // -- S->C processing ------------------------------------------------------

    /// Process an S->C packet.
    ///
    /// Updates position from any position-carrying packet (`0x1B`, `0x20`,
    /// `0x77`, `0x78`), handles `MoveAck`/`MoveReject`/`DrawGamePlayer`
    /// movement logic, and tracks world changes from `0xBF SetMap`.
    ///
    /// If `z_resolver` is provided, Z resolution after `MoveAck` steps uses
    /// the resolver's implementation (e.g. terrain + visible items + multi
    /// shapes).  Without a resolver, Z is left unchanged after stepping.
    ///
    /// **Call order:** when the resolver is backed by a `SessionView`, call
    /// `session.ingest_packet(data)` **before** this method so that the
    /// visible set and multi registry are up-to-date.
    pub fn process_s2c(&mut self, data: &[u8], z_resolver: Option<&dyn ZResolver>) {
        if data.is_empty() {
            return;
        }

        self.last_move_accepted = false;

        // -- SetMap (0xBF sub 0x0008) -----------------------------------------
        if data[0] == 0xBF {
            if let Ok(GeneralInfo::SetMap { world }) = GeneralInfo::from_bytes(data) {
                if world != self.current_world {
                    debug!(
                        "[movement] SetMap: world {} -> {}, draining {} pending moves",
                        self.current_world, world, self.pending_moves.len(),
                    );
                    self.current_world = world;
                    self.pending_moves.clear();
                }
            }
        }

        // -- Position update from any position-carrying packet ----------------
        self.pos.update_from_packet(data);

        // -- Movement-specific handling ---------------------------------------

        // DrawGamePlayer (0x20) -- drains the pending move queue.
        if data[0] == DrawGamePlayer::ID {
            if !self.pending_moves.is_empty() {
                debug!(
                    "[movement] 0x20 DrawGamePlayer -- draining {} pending moves",
                    self.pending_moves.len(),
                );
                self.pending_moves.clear();
            }
            return;
        }

        // MoveReject (0x21) -- server rejected a move; snap to provided pos.
        if data[0] == MoveReject::ID {
            if let Ok(rej) = MoveReject::from_bytes(data) {
                if !self.pending_moves.is_empty() {
                    debug!(
                        "[movement] 0x21 MoveReject seq={} ({},{},{}) -- draining {} pending moves",
                        rej.sequence, rej.x, rej.y, rej.z,
                        self.pending_moves.len(),
                    );
                    self.pending_moves.clear();
                }
                self.pos.x = rej.x;
                self.pos.y = rej.y;
                self.pos.z = rej.z;
                self.pos.facing = Facing::new(rej.direction);
            }
            return;
        }

        // MoveAck (0x22) -- match with pending queue.
        if data[0] == MoveAck::ID {
            if let Ok(ack) = MoveAck::from_bytes(data) {
                match self.pending_moves.on_ack(ack.sequence) {
                    AckOutcome::Matched(direction) => {
                        let stepped = self.pos.step(direction);
                        if stepped {
                            self.resolve_z(z_resolver);
                        }
                        self.last_move_accepted = true;
                    }
                    AckOutcome::Desync(drained) => {
                        if drained.is_empty() {
                            warn!(
                                "[movement] MoveAck seq={} -- queue empty, ignoring",
                                ack.sequence,
                            );
                        } else {
                            warn!(
                                "[movement] MoveAck seq={} -- desync, \
                                 drained {} pending moves",
                                ack.sequence, drained.len(),
                            );
                        }
                    }
                }
            }
        }
    }

    // -- Z resolution ---------------------------------------------------------

    /// Resolve standing Z at the current position using the provided
    /// [`ZResolver`].
    ///
    /// Does nothing if `z_resolver` is `None`.
    fn resolve_z(&mut self, z_resolver: Option<&dyn ZResolver>) {
        let Some(resolver) = z_resolver else { return };

        if let Some(new_z) = resolver.resolve_standing_z(
            self.pos.x, self.pos.y, self.pos.z, self.pos.facing.heading(),
        ) {
            self.pos.z = new_z;
        }
    }
}
