//! Paced movement sender — rate-limits `MoveRequest` packets sent to the
//! upstream server and buffers client steps that exceed the in-flight queue.
//!
//! [`PacedSender`] sits between [`framework::rythmos::MoveArbiter`] and the server send path.
//! It solves two problems:
//!
//! 1. **Rate limiting** — UO servers expect minimum delays between
//!    consecutive `MoveRequest` packets (100–400 ms depending on speed
//!    tier).  Sending multiple steps with zero inter-packet delay can
//!    trigger FastWalk detection or desynchronise the server's movement
//!    timer.
//!
//! 2. **Input buffering** — when [`framework::rythmos::MoveArbiter`] rejects a step because
//!    the 4-slot in-flight queue is full, the step is buffered here
//!    instead of being immediately rejected to the client.  As server
//!    ack's arrive and free in-flight slots, buffered steps are replayed
//!    through the arbiter.
//!
//! # Architecture
//!
//! ```text
//!   client_step / bot_step
//!          │
//!          ▼
//!   ┌──────────────┐
//!   │ MoveArbiter  │── to_server ──► outbound queue  ──► (paced) server.send()
//!   │              │── Err ────────► waiting queue    ──► (on ack) replay
//!   └──────────────┘                                      through arbiter
//! ```
//!
//! The `outbound` queue holds steps that have been accepted by the arbiter
//! (have a server-side sequence number) and are waiting for the pacer timer
//! to allow sending.
//!
//! The `waiting` queue holds steps from clients that did not fit into the
//! arbiter's in-flight queue.  They are replayed through `client_step()`
//! as ack's free up slots.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use log::{debug, warn};

use u_core::position::Facing;
use framework::rythmos::{ClientId, MoveSpeed, MovePacer, PositionTracker};
use protocol::RawPacket;

// ── Queued entry types ───────────────────────────────────────────────────

/// A step accepted by the arbiter, waiting to be sent to the server.
#[derive(Debug)]
struct OutboundStep {
    /// The wire-encoded `MoveRequest` packet ready to send.
    raw: RawPacket,
    /// The `MoveSpeed` tier used for pacing this step.
    speed: MoveSpeed,
    /// The direction of this step (used to advance predicted position on send).
    facing: Facing,
}

/// A client step that could not be enqueued into the arbiter (queue full).
/// Will be replayed through `MoveArbiter::client_step()` when a slot opens.
#[derive(Debug, Clone)]
pub struct WaitingStep {
    /// Which client sent this step.
    pub client_id: ClientId,
    /// The direction requested.
    pub facing: Facing,
    /// The client's own sequence number (for their MoveAck/Reject).
    pub their_seq: u8,
}

// ── PacedSender ──────────────────────────────────────────────────────────

/// Rate-limited movement sender with input buffering and predicted position.
#[derive(Debug)]
pub struct PacedSender {
    /// Pacer that tracks inter-step timing.
    pacer: MovePacer,

    /// Steps accepted by the arbiter, waiting for the pacer to allow sending.
    outbound: VecDeque<OutboundStep>,

    /// Client steps that did not fit into the arbiter's in-flight queue.
    waiting: VecDeque<WaitingStep>,

    /// Whether the player character is currently mounted.
    mounted: bool,

    /// Maximum number of entries in the waiting queue.
    max_waiting: usize,

    /// Predicted position: confirmed position + steps already sent to server.
    ///
    /// Advanced each time a step is flushed from the outbound queue and
    /// actually sent to the server.  Reset on reject/snap/resync to match
    /// the server's authoritative position.
    predicted: PositionTracker,
}

impl PacedSender {
    /// Create a new `PacedSender`.
    ///
    /// `max_waiting` is the capacity of the input buffer for steps that
    /// didn't fit into the arbiter.  12–16 is a good default (matches
    /// Stealth's `msWaitSent` zone).
    pub fn new(max_waiting: usize) -> Self {
        Self {
            pacer: MovePacer::new(),
            outbound: VecDeque::new(),
            waiting: VecDeque::new(),
            mounted: false,
            max_waiting,
            predicted: PositionTracker::default(),
        }
    }

    // ── Outbound (rate-limited send queue) ───────────────────────────

    /// Enqueue a step that was accepted by the arbiter for paced sending.
    ///
    /// `facing` is used to determine the speed tier (walk/run/turn).
    /// `is_turn` indicates whether this step is a turn-in-place (no tile
    /// movement).
    pub fn enqueue_outbound(&mut self, raw: RawPacket, facing: Facing, is_turn: bool) {
        let speed = if is_turn {
            MoveSpeed::TurnOnly
        } else {
            MoveSpeed::from_facing(facing, self.mounted)
        };
        self.outbound.push_back(OutboundStep { raw, speed, facing });
    }

    /// Try to dequeue the next outbound step if the pacer allows it.
    ///
    /// Returns `Some(raw_packet)` if a step is ready to send, `None` if
    /// the outbound queue is empty or the pacer hasn't elapsed yet.
    ///
    /// On success, advances the predicted position by the step's facing.
    pub fn try_flush(&mut self) -> Option<RawPacket> {
        let front = self.outbound.front()?;
        if self.pacer.can_move(front.speed) {
            let step = self.outbound.pop_front().unwrap();
            self.pacer.record_move();
            // Advance predicted position — this step is now sent to the server.
            self.predicted.step(step.facing);
            Some(step.raw)
        } else {
            None
        }
    }

    /// Duration until the next outbound step can be sent.
    ///
    /// Returns `None` if the outbound queue is empty.
    pub fn time_until_next(&self) -> Option<Duration> {
        let front = self.outbound.front()?;
        Some(self.pacer.time_until_ready(front.speed))
    }

    /// Compute the [`Instant`] at which the next outbound step can be sent.
    ///
    /// Returns `None` if the outbound queue is empty.  This is suitable
    /// for use with `tokio::time::sleep_until()`.
    pub fn next_send_instant(&self) -> Option<Instant> {
        let dur = self.time_until_next()?;
        Some(Instant::now() + dur)
    }

    /// Whether there are outbound steps waiting to be sent.
    #[inline]
    pub fn has_outbound(&self) -> bool {
        !self.outbound.is_empty()
    }

    // ── Waiting (input buffer for queue-full steps) ──────────────────

    /// Buffer a client step that the arbiter rejected (queue full).
    ///
    /// Returns `true` if buffered, `false` if the waiting queue is full
    /// (the caller should reject back to the client).
    pub fn enqueue_waiting(&mut self, step: WaitingStep) -> bool {
        if self.waiting.len() >= self.max_waiting {
            warn!(
                "[paced] waiting queue full ({}/{}), rejecting client_id={} seq={}",
                self.waiting.len(),
                self.max_waiting,
                step.client_id,
                step.their_seq,
            );
            return false;
        }
        debug!(
            "[paced] buffering client_id={} seq={} (waiting={}/{})",
            step.client_id,
            step.their_seq,
            self.waiting.len() + 1,
            self.max_waiting,
        );
        self.waiting.push_back(step);
        true
    }

    /// Dequeue the next waiting step for replay through the arbiter.
    ///
    /// Should be called after a server ack frees an in-flight slot.
    pub fn dequeue_waiting(&mut self) -> Option<WaitingStep> {
        self.waiting.pop_front()
    }

    /// Whether there are waiting steps to replay.
    #[inline]
    pub fn has_waiting(&self) -> bool {
        !self.waiting.is_empty()
    }

    // ── Drain / reset ────────────────────────────────────────────────

    /// Drain all waiting steps (e.g. on reject, position snap, world change).
    ///
    /// Returns the drained steps so the caller can reject them back to
    /// their originating clients.
    pub fn drain_waiting(&mut self) -> Vec<WaitingStep> {
        self.waiting.drain(..).collect()
    }

    /// Drain all outbound steps (e.g. on reject, position snap).
    ///
    /// Note: outbound steps already have server-side sequence numbers
    /// assigned by the arbiter — draining them means the server will never
    /// see those requests.  The arbiter's pending queue should also be
    /// cleared (which happens via `on_server_reject` / `on_position_snap`).
    pub fn drain_outbound(&mut self) -> Vec<RawPacket> {
        self.outbound.drain(..).map(|s| s.raw).collect()
    }

    /// Full reset: drain both queues, reset the pacer.
    ///
    /// The caller must follow up with [`sync_predicted`](Self::sync_predicted)
    /// to re-align the predicted position with the server's authoritative
    /// position.
    ///
    /// Returns drained waiting steps for client rejection.
    pub fn reset(&mut self) -> Vec<WaitingStep> {
        self.pacer.reset();
        self.outbound.clear();
        self.waiting.drain(..).collect()
    }

    // ── Predicted position ───────────────────────────────────────────

    /// Synchronise the predicted position with the confirmed position.
    ///
    /// Should be called:
    /// - After `reset()` with the server's authoritative position
    /// - On first login (when position becomes known)
    /// - After reject / snap / resync
    pub fn sync_predicted(&mut self, confirmed: &PositionTracker) {
        self.predicted = *confirmed;
    }

    /// Read-only access to the predicted position.
    ///
    /// This reflects the confirmed position plus all steps that have been
    /// sent to the server (pending ack).  Steps still in the outbound
    /// (pacing) or waiting (queue-full) queues are **not** included.
    #[inline]
    pub fn predicted_pos(&self) -> &PositionTracker {
        &self.predicted
    }

    // ── State ────────────────────────────────────────────────────────

    /// Update the mounted state (affects speed tier calculation).
    pub fn set_mounted(&mut self, mounted: bool) {
        if self.mounted != mounted {
            debug!("[paced] mounted state changed: {} -> {}", self.mounted, mounted);
            self.mounted = mounted;
        }
    }

    /// Current mounted state.
    #[inline]
    pub fn is_mounted(&self) -> bool {
        self.mounted
    }
}
