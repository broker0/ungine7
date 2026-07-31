//! Generic pending-move queue with UO sequence matching.
//!
//! [`PendingQueue<T>`] implements the core ack/reject/drain protocol shared
//! by all movement components:
//!
//! - [`MovementTracker`](super::MovementTracker) and
 //!   [`ObserverPipeline`](crate::diorama::ObserverPipeline) use
//!   `PendingQueue<Facing>` for passive observation (sequence comes from
//!   client packets).
//! - [`ActiveMover`](super::ActiveMover) uses `PendingQueue<PendingStep>`
//!   for active movement (sequence is generated internally).
//!
//! The queue stores `(sequence: u8, payload: T)` tuples and provides
//! matching against server `MoveAck` / `MoveReject` sequence numbers.

use std::collections::VecDeque;

// ── AckOutcome ────────────────────────────────────────────────────────────

/// Outcome of matching a server `MoveAck` sequence against the pending queue.
#[derive(Debug)]
pub enum AckOutcome<T> {
    /// Head of queue matched `server_seq` — the step is confirmed.
    Matched(T),

    /// Sequence mismatch or empty queue — all pending entries were drained
    /// to resynchronise.  The vector may be empty (if the queue was already
    /// empty when the ack arrived).
    Desync(Vec<(u8, T)>),
}

// ── PendingQueue ──────────────────────────────────────────────────────────

/// Generic pending-move queue implementing UO sequence matching.
///
/// Each entry is a `(sequence: u8, payload: T)` pair.  Entries are pushed
/// at the back and matched from the front, preserving FIFO order.
#[derive(Debug, Clone)]
pub struct PendingQueue<T> {
    queue: VecDeque<(u8, T)>,
}

impl<T> Default for PendingQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> PendingQueue<T> {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Enqueue a step with the given sequence number and payload.
    pub fn push(&mut self, seq: u8, payload: T) {
        self.queue.push_back((seq, payload));
    }

    /// Number of pending (unacknowledged) entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the queue is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Peek at the front entry without removing it.
    #[inline]
    pub fn front(&self) -> Option<&(u8, T)> {
        self.queue.front()
    }

    /// Match a server `MoveAck` sequence against the head of the queue.
    ///
    /// - If the queue is empty, returns [`AckOutcome::Desync`] with an
    ///   empty vector.
    /// - If `server_seq` matches the front entry's sequence, that entry is
    ///   popped and its payload returned as [`AckOutcome::Matched`].
    /// - Otherwise all entries are drained and returned as
    ///   [`AckOutcome::Desync`].
    pub fn on_ack(&mut self, server_seq: u8) -> AckOutcome<T> {
        if self.queue.is_empty() {
            return AckOutcome::Desync(vec![]);
        }
        if self.queue[0].0 == server_seq {
            let (_, payload) = self.queue.pop_front().unwrap();
            AckOutcome::Matched(payload)
        } else {
            AckOutcome::Desync(self.queue.drain(..).collect())
        }
    }

    /// Handle a server `MoveReject`.
    ///
    /// Pops the front entry (the rejected step) and drains all remaining
    /// entries (they are implicitly invalidated by the rejection).
    ///
    /// Returns `(rejected_entry, remaining_drained)`.  If the queue was
    /// empty, returns `(None, vec![])`.
    pub fn on_reject(&mut self, _server_seq: u8) -> (Option<(u8, T)>, Vec<(u8, T)>) {
        if self.queue.is_empty() {
            return (None, vec![]);
        }
        let first = self.queue.pop_front();
        let rest: Vec<_> = self.queue.drain(..).collect();
        (first, rest)
    }

    /// Drain all entries, returning them in FIFO order.
    pub fn drain_all(&mut self) -> Vec<(u8, T)> {
        self.queue.drain(..).collect()
    }

    /// Clear the queue without returning entries.
    pub fn clear(&mut self) {
        self.queue.clear();
    }

    /// Iterate over pending entries in FIFO order (for diagnostics).
    pub fn iter(&self) -> impl Iterator<Item = &(u8, T)> {
        self.queue.iter()
    }
}
