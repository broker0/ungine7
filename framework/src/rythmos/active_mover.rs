//! Active movement queue for bot and multi-client scenarios.
//!
//! [`ActiveMover`] manages a pending-move queue where **this process** is the
//! one generating `MoveRequest` packets (as opposed to the passive observation
//! in [`MovementTracker`](super::MovementTracker) /
 //! [`ObserverPipeline`](crate::diorama::ObserverPipeline) where requests come from an
//! external UO client).
//!
//! Each enqueued step is assigned a sequence number (1–255, wrapping) and
//! tagged with a [`StepOrigin`] so that the caller (typically
//! [`MoveArbiter`](super::MoveArbiter)) can route acknowledgements and
//! rejections back to the correct source.
//!
//! # Example
//!
//! ```ignore
//! let mut mover = ActiveMover::new(2);
//!
//! // Bot wants to step north
//! let facing = Facing::from_heading(Heading::North);
//! match mover.try_enqueue(facing, StepOrigin::Bot) {
//!     Ok(req) => send_to_server(req.to_bytes()),
//!     Err(_origin) => { /* queue full, skip */ }
//! }
//!
//! // Server acknowledges
//! match mover.on_ack(server_seq) {
//!     AckOutcome::Matched(step) => { /* apply step */ }
//!     AckOutcome::Desync(drained) => { /* handle desync */ }
//! }
//! ```

use u_core::Facing;
use packets::movement::MoveRequest;
use packets::traits::BasicPacket;

use super::pending_queue::{AckOutcome, PendingQueue};

// ── Types ─────────────────────────────────────────────────────────────────

/// Identifier for a connected client (assigned by the caller).
pub type ClientId = u32;

/// Who initiated a particular movement step.
#[derive(Debug, Clone)]
pub enum StepOrigin {
    /// Step initiated by the bot / AI logic.
    Internal,

    /// Step initiated by a connected UO client.
    External {
        /// Client identifier.
        id: ClientId,
        /// The sequence number from the client's own `MoveRequest` packet.
        /// Used to send the corresponding `MoveAck` / `MoveReject` back.
        their_seq: u8,
    },
}

/// A step waiting for server acknowledgement.
#[derive(Debug, Clone)]
pub struct PendingStep {
    /// The direction requested for this step.
    pub facing: Facing,
    /// Who initiated this step.
    pub origin: StepOrigin,
}

// ── ActiveMover ───────────────────────────────────────────────────────────

/// Active movement queue that generates `MoveRequest` packets and tracks
/// pending acknowledgements.
///
/// The queue enforces a configurable maximum depth (`max_pending`, default 2,
/// range 1–4).  Steps beyond the limit are rejected locally without sending
/// anything to the server.
#[derive(Debug, Clone)]
pub struct ActiveMover {
    /// Pending steps awaiting server ack/reject.
    pending: PendingQueue<PendingStep>,

    /// Maximum number of unacknowledged steps allowed.
    max_pending: usize,

    /// Next sequence number to assign (wraps 1..=255, skipping 0 except after reset).
    next_seq: u8,

    /// When `true`, the next step uses seq=0 (after login / snap / reject).
    /// Cleared after the first step is enqueued.
    reset_pending: bool,

    /// Reserved for future fastwalk stack support.  Always 0 for now.
    fastwalk_key: u32,
}

impl Default for ActiveMover {
    fn default() -> Self {
        Self::new(2)
    }
}

impl ActiveMover {
    /// Create a new `ActiveMover` with the given maximum pending depth.
    ///
    /// # Panics
    ///
    /// Panics if `max_pending` is 0 or greater than 4.
    pub fn new(max_pending: usize) -> Self {
        assert!(
            (1..=4).contains(&max_pending),
            "max_pending must be 1–4, got {max_pending}"
        );
        Self {
            pending: PendingQueue::new(),
            max_pending,
            next_seq: 0,
            reset_pending: true,
            fastwalk_key: 0,
        }
    }

    /// Whether the queue has room for another step.
    #[inline]
    pub fn can_enqueue(&self) -> bool {
        self.pending.len() < self.max_pending
    }

    /// Number of steps currently awaiting acknowledgement.
    #[inline]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Read-only access to the pending queue (for diagnostics).
    #[inline]
    pub fn pending(&self) -> &PendingQueue<PendingStep> {
        &self.pending
    }

    /// Attempt to enqueue a new movement step.
    ///
    /// On success, returns `Ok(MoveRequest)` — the packet that the caller
    /// must send to the server.
    ///
    /// If the queue is full, returns `Err(origin)` so the caller can
    /// generate a local reject for client-origin steps.
    pub fn try_enqueue(
        &mut self,
        facing: Facing,
        origin: StepOrigin,
    ) -> Result<MoveRequest, StepOrigin> {
        if self.pending.len() >= self.max_pending {
            return Err(origin);
        }

        // Determine next sequence number.
        // After a reset (login / snap / reject) the first step uses seq=0;
        // subsequent steps wrap 1..=255, skipping 0.
        let seq = if self.reset_pending {
            self.reset_pending = false;
            self.next_seq = 0;
            0
        } else {
            self.next_seq = self.next_seq.wrapping_add(1);
            if self.next_seq == 0 {
                self.next_seq = 1;
            }
            self.next_seq
        };

        self.pending.push(seq, PendingStep { facing, origin });

        Ok(MoveRequest {
            id: MoveRequest::ID,
            direction: facing.raw(),
            sequence: seq,
            fastwalk_key: self.fastwalk_key,
        })
    }

    /// Handle a server `MoveAck`.
    ///
    /// - [`AckOutcome::Matched`] — the front step matched; the caller
    ///   should apply the step to `PositionTracker`.
    /// - [`AckOutcome::Desync`] — sequence mismatch; all pending steps
    ///   were drained.  The caller should reject client-origin steps and
    ///   snap position.
    pub fn on_ack(&mut self, server_seq: u8) -> AckOutcome<PendingStep> {
        self.pending.on_ack(server_seq)
    }

    /// Handle a server `MoveReject`.
    ///
    /// Returns `(rejected, drained_rest)`:
    /// - `rejected` — the front step (the one the server rejected), or
    ///   `None` if the queue was empty.
    /// - `drained_rest` — all remaining steps that were implicitly
    ///   invalidated.
    ///
    /// Resets the sequence counter so the next step starts at seq=0.
    pub fn on_reject(
        &mut self,
        server_seq: u8,
    ) -> (Option<(u8, PendingStep)>, Vec<(u8, PendingStep)>) {
        self.reset_pending = true;
        self.pending.on_reject(server_seq)
    }

    /// Drain all pending steps and return them (e.g. on world change / position snap).
    ///
    /// Resets the sequence counter so the next step sent to the server starts at seq=0.
    pub fn clear(&mut self) -> Vec<(u8, PendingStep)> {
        self.reset_pending = true;
        self.pending.drain_all()
    }
}
