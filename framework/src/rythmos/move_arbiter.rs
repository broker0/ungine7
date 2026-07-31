//! Movement arbiter for multi-client scenarios.
//!
//! [`MoveArbiter`] sits between one or more connected UO clients (and/or a
//! bot) and a single server connection.  It multiplexes movement requests
//! through [`ActiveMover`], maintains the authoritative position via
//! [`PositionTracker`], and produces per-client response instructions that
//! the caller translates into wire packets.
//!
//! # Design
//!
//! `MoveArbiter` is **synchronous and pure** -- it does not own any network
//! connections or depend on an async runtime.  The caller is responsible for:
//!
//! 1. Sending `MoveRequest` packets to the server (when `to_server` is `Some`).
//! 2. Translating [`ClientResponse`] values into wire packets and sending
//!    them to the appropriate clients.
//! 3. Calling the appropriate `on_server_*` method when server packets arrive.
//!
//! # Packets not handled
//!
//! Position-carrying packets other than `DrawGamePlayer (0x20)` and
//! `MoveReject (0x21)` -- specifically `CharacterLocaleAndBody (0x1B)`,
//! `UpdateMobile (0x77)`, and `DrawMobile (0x78)` -- should be applied by
//! the caller via [`PositionTracker`]'s typed `apply_*` methods before or
//! after calling arbiter methods, depending on the packet flow.

use std::collections::HashSet;

use log::warn;

use u_core::Facing;
use packets::character::DrawGamePlayer;
use packets::movement::{MoveAck, MoveReject, MoveRequest, Notoriety};

use super::active_mover::{ActiveMover, ClientId, PendingStep, StepOrigin};
use super::pending_queue::AckOutcome;
use super::position_tracker::PositionTracker;
use super::z_resolver::ZResolver;

// -- Response types -----------------------------------------------------------

/// Instruction for what to send to a specific client.
///
/// The caller is responsible for encoding these into wire packets.
#[derive(Debug, Clone)]
pub enum ClientResponse {
    /// Step confirmed -- send `MoveAck` to the originating client.
    Ack {
        /// The client's original sequence number (from their `MoveRequest`).
        their_seq: u8,
        /// Notoriety value from the server's `MoveAck`.
        notoriety: Notoriety,
    },

    /// Step rejected -- send `MoveReject` + `DrawGamePlayer` to the client.
    Reject {
        /// The client's original sequence number.
        their_seq: u8,
        /// Authoritative `DrawGamePlayer` packet reflecting current position.
        draw: DrawGamePlayer,
    },

    /// Position updated -- send `DrawGamePlayer` to clients that are not the
    /// step's originator (or to all clients on snap/world-change).
    Draw {
        /// Authoritative `DrawGamePlayer` packet reflecting current position.
        draw: DrawGamePlayer,
    },
}

/// Result of a `client_step` or `bot_step` call.
#[derive(Debug)]
pub struct ArbiterResult {
    /// Packet to send to the server.  `None` if the step was locally rejected
    /// (queue full).
    pub to_server: Option<MoveRequest>,

    /// Immediate response to the step's originator (only set when the queue
    /// is full and the origin is a client -- bots silently skip).
    pub immediate: Option<(ClientId, ClientResponse)>,
}

// -- MoveArbiter --------------------------------------------------------------

/// Multiplexes movement from multiple sources through a single server
/// connection.
#[derive(Debug, Clone)]
pub struct MoveArbiter {
    /// Active movement queue (sequence generation + pending tracking).
    mover: ActiveMover,

    /// Authoritative player position.
    pos: PositionTracker,

    /// Connected client identifiers.
    clients: Vec<ClientId>,
}

impl MoveArbiter {
    /// Create a new arbiter with the given maximum pending depth (1-4).
    pub fn new(max_pending: usize) -> Self {
        Self {
            mover: ActiveMover::new(max_pending),
            pos: PositionTracker::default(),
            clients: Vec::new(),
        }
    }

    // -- Accessors ------------------------------------------------------------

    /// Read-only access to the active mover (for diagnostics).
    #[inline]
    pub fn mover(&self) -> &ActiveMover {
        &self.mover
    }

    /// Read-only access to the position tracker.
    #[inline]
    pub fn pos(&self) -> &PositionTracker {
        &self.pos
    }

    /// Mutable access to the position tracker.
    ///
    /// Use this to apply position-carrying packets (`0x1B`, `0x77`, `0x78`)
    /// that are not directly handled by the arbiter.
    #[inline]
    pub fn pos_mut(&mut self) -> &mut PositionTracker {
        &mut self.pos
    }

    /// Currently connected client IDs.
    #[inline]
    pub fn clients(&self) -> &[ClientId] {
        &self.clients
    }

    // -- Client management ----------------------------------------------------

    /// Register a client.  Duplicates are silently ignored.
    pub fn attach_client(&mut self, id: ClientId) {
        if !self.clients.contains(&id) {
            self.clients.push(id);
        }
    }

    /// Unregister a client.
    pub fn detach_client(&mut self, id: ClientId) {
        self.clients.retain(|&c| c != id);
    }

    // -- Step submission ------------------------------------------------------

    /// A connected client submitted a `MoveRequest`.
    ///
    /// The arbiter translates it into a server-bound request (with its own
    /// sequence numbering) or rejects it locally if the queue is full.
    pub fn client_step(&mut self, id: ClientId, req: &MoveRequest) -> ArbiterResult {
        let facing = Facing::new(req.direction);
        let origin = StepOrigin::External {
            id,
            their_seq: req.sequence,
        };

        match self.mover.try_enqueue(facing, origin) {
            Ok(server_req) => ArbiterResult {
                to_server: Some(server_req),
                immediate: None,
            },
            Err(origin) => {
                // Extract client info from the returned origin.
                let (cid, their_seq) = match origin {
                    StepOrigin::External { id, their_seq } => (id, their_seq),
                    StepOrigin::Internal => unreachable!("client_step always uses Client origin"),
                };
                ArbiterResult {
                    to_server: None,
                    immediate: Some((
                        cid,
                        ClientResponse::Reject {
                            their_seq,
                            draw: self.pos.to_draw_game_player(),
                        },
                    )),
                }
            }
        }
    }

    /// The bot / AI logic wants to take a step.
    ///
    /// If the queue is full the step is silently dropped (no reject is
    /// generated for bot-origin steps).
    pub fn bot_step(&mut self, facing: Facing) -> ArbiterResult {
        match self.mover.try_enqueue(facing, StepOrigin::Internal) {
            Ok(server_req) => ArbiterResult {
                to_server: Some(server_req),
                immediate: None,
            },
            Err(_) => ArbiterResult {
                to_server: None,
                immediate: None,
            },
        }
    }

    // -- Server responses -----------------------------------------------------

    /// The server acknowledged a step (`MoveAck`).
    ///
    /// Updates position and produces per-client responses:
    /// - The originating client gets [`ClientResponse::Ack`].
    /// - All other clients get [`ClientResponse::Draw`].
    /// - On desync, client-origin steps get [`ClientResponse::Reject`]
    ///   and remaining clients get [`ClientResponse::Draw`].
    ///
    /// If `z_resolver` is provided, Z is resolved after stepping.
    pub fn on_server_ack(
        &mut self,
        ack: &MoveAck,
        z_resolver: Option<&dyn ZResolver>,
    ) -> Vec<(ClientId, ClientResponse)> {
        match self.mover.on_ack(ack.sequence) {
            AckOutcome::Matched(PendingStep { facing, origin }) => {
                // Apply the step to position.
                let stepped = self.pos.step(facing);
                if stepped {
                    self.resolve_z(z_resolver);
                }

                let draw = self.pos.to_draw_game_player();
                let mut responses = Vec::new();

                match origin {
                    StepOrigin::External { id, their_seq } => {
                        // Ack to the originating client.
                        responses.push((
                            id,
                            ClientResponse::Ack {
                                their_seq,
                                notoriety: ack.notoriety,
                            },
                        ));
                        // Draw to all other clients.
                        for &cid in &self.clients {
                            if cid != id {
                                responses.push((
                                    cid,
                                    ClientResponse::Draw { draw: draw.clone() },
                                ));
                            }
                        }
                    }
                    StepOrigin::Internal => {
                        // Draw to all clients.
                        for &cid in &self.clients {
                            responses.push((
                                cid,
                                ClientResponse::Draw { draw: draw.clone() },
                            ));
                        }
                    }
                }
                responses
            }

            AckOutcome::Desync(drained) => {
                // Position does NOT move -- we're out of sync.
                warn!(
                    "[arbiter] MoveAck seq={} desync -- drained {} pending steps",
                    ack.sequence,
                    drained.len(),
                );

                let draw = self.pos.to_draw_game_player();
                let mut responses = Vec::new();
                let mut clients_with_reject = HashSet::new();

                // Reject each drained client-origin step.
                for (_seq, step) in drained {
                    if let StepOrigin::External { id, their_seq } = step.origin {
                        responses.push((
                            id,
                            ClientResponse::Reject {
                                their_seq,
                                draw: draw.clone(),
                            },
                        ));
                        clients_with_reject.insert(id);
                    }
                }

                // Draw to all clients that didn't get a reject.
                for &cid in &self.clients {
                    if !clients_with_reject.contains(&cid) {
                        responses.push((
                            cid,
                            ClientResponse::Draw { draw: draw.clone() },
                        ));
                    }
                }

                responses
            }
        }
    }

    /// The server rejected a step (`MoveReject`).
    ///
    /// Snaps position to the server-provided coordinates, drains the
    /// pending queue, and produces per-client responses.
    pub fn on_server_reject(
        &mut self,
        reject: &MoveReject,
    ) -> Vec<(ClientId, ClientResponse)> {
        // Snap position to server-provided coordinates.
        self.pos.x = reject.x;
        self.pos.y = reject.y;
        self.pos.z = reject.z;
        self.pos.facing = Facing::new(reject.direction);

        let (first, drained) = self.mover.on_reject(reject.sequence);

        let draw = self.pos.to_draw_game_player();
        let mut responses = Vec::new();
        let mut clients_with_reject = HashSet::new();

        // Reject the front step (if it was client-originated).
        if let Some((_seq, step)) = first {
            if let StepOrigin::External { id, their_seq } = step.origin {
                responses.push((
                    id,
                    ClientResponse::Reject {
                        their_seq,
                        draw: draw.clone(),
                    },
                ));
                clients_with_reject.insert(id);
            }
        }

        // Reject each drained client-origin step.
        for (_seq, step) in drained {
            if let StepOrigin::External { id, their_seq } = step.origin {
                responses.push((
                    id,
                    ClientResponse::Reject {
                        their_seq,
                        draw: draw.clone(),
                    },
                ));
                clients_with_reject.insert(id);
            }
        }

        // Draw to all clients that didn't get a reject.
        for &cid in &self.clients {
            if !clients_with_reject.contains(&cid) {
                responses.push((
                    cid,
                    ClientResponse::Draw { draw: draw.clone() },
                ));
            }
        }

        responses
    }

    /// The server sent a `DrawGamePlayer (0x20)` -- authoritative position snap.
    ///
    /// Updates position, drains the pending queue, and notifies all clients.
    pub fn on_position_snap(
        &mut self,
        p: &DrawGamePlayer,
    ) -> Vec<(ClientId, ClientResponse)> {
        self.pos.apply_draw_game_player(p);

        let drained = self.mover.clear();

        if !drained.is_empty() {
            warn!(
                "[arbiter] DrawGamePlayer snap -- drained {} pending steps",
                drained.len(),
            );
        }

        let draw = self.pos.to_draw_game_player();
        let mut responses = Vec::new();
        let mut clients_with_reject = HashSet::new();

        // Reject drained client-origin steps.
        for (_seq, step) in drained {
            if let StepOrigin::External { id, their_seq } = step.origin {
                responses.push((
                    id,
                    ClientResponse::Reject {
                        their_seq,
                        draw: draw.clone(),
                    },
                ));
                clients_with_reject.insert(id);
            }
        }

        // Draw to all clients that didn't get a reject.
        for &cid in &self.clients {
            if !clients_with_reject.contains(&cid) {
                responses.push((
                    cid,
                    ClientResponse::Draw { draw: draw.clone() },
                ));
            }
        }

        responses
    }

    /// World changed (e.g. `SetMap`) -- drain all pending steps and notify
    /// all clients with a `DrawGamePlayer`.
    ///
    /// Unlike reject, drained client-origin steps receive `Draw` (not
    /// `Reject`) because the world change supersedes individual step
    /// outcomes.
    pub fn on_world_change(&mut self) -> Vec<(ClientId, ClientResponse)> {
        let _drained = self.mover.clear();

        let draw = self.pos.to_draw_game_player();
        self.clients
            .iter()
            .map(|&cid| (cid, ClientResponse::Draw { draw: draw.clone() }))
            .collect()
    }

    // -- Z resolution ---------------------------------------------------------

    /// Resolve standing Z at the current position using the provided
    /// [`ZResolver`].
    fn resolve_z(&mut self, z_resolver: Option<&dyn ZResolver>) {
        let Some(resolver) = z_resolver else { return };

        if let Some(new_z) = resolver.resolve_standing_z(
            self.pos.x,
            self.pos.y,
            self.pos.z,
            self.pos.facing.heading(),
        ) {
            self.pos.z = new_z;
        }
    }
}
