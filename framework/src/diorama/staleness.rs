//! [`StalenessTracker`] — generic tracker for confirming object presence.
//!
//! When armed, the tracker collects serial numbers that have been
//! "confirmed" (i.e. the server sent us fresh data for them).  After a
//! configurable quiet period (no new confirmations), a sweep can be
//! performed: any serial from a caller-supplied candidate list that was
//! **not** confirmed is reported as stale and should be removed.
//!
//! This is intentionally decoupled from spatial indices, view rects, and
//! world IDs — the caller decides which candidates to check and what to
//! do with the stale results.

use std::collections::HashSet;
use std::time::Duration;

use tokio::time::Instant;

/// Default quiet period before a sweep is considered due.
pub const DEFAULT_STALENESS_THRESHOLD: Duration = Duration::from_millis(100);

/// Tracks which object serials have been confirmed since the tracker was
/// armed, and decides when a staleness sweep should run.
#[derive(Clone, Debug)]
pub struct StalenessTracker {
    /// Serials confirmed (via `ObjectInfo` etc.) since last arm/sweep.
    confirmed: HashSet<u32>,
    /// Whether the tracker is currently armed (expecting confirmations).
    armed: bool,
    /// Timestamp of the last confirmation (or arm, whichever is later).
    /// Used to detect the quiet period.
    last_activity: Instant,
    /// How long after the last confirmation we wait before sweeping.
    threshold: Duration,
}

impl StalenessTracker {
    /// Create a new tracker with the given quiet-period threshold.
    pub fn new(threshold: Duration) -> Self {
        Self {
            confirmed: HashSet::new(),
            armed: false,
            last_activity: Instant::now(),
            threshold,
        }
    }

    /// Arm the tracker — start collecting confirmations.
    ///
    /// Typically called on `SetMap` or when re-entering an area where
    /// stale objects may exist.
    pub fn arm(&mut self) {
        self.armed = true;
        self.confirmed.clear();
        self.last_activity = Instant::now();
    }

    /// Record a serial as confirmed (the server sent fresh data for it).
    ///
    /// Also resets the quiet-period timer so that a burst of confirmations
    /// delays the sweep until the burst is over.
    pub fn confirm(&mut self, serial: u32) {
        if self.armed {
            self.confirmed.insert(serial);
            self.last_activity = Instant::now();
        }
    }

    /// Whether the tracker is currently armed.
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Whether the quiet period has elapsed and a sweep should run.
    pub fn should_sweep(&self) -> bool {
        self.armed && self.last_activity.elapsed() >= self.threshold
    }

    /// Perform the sweep: from `candidates`, return those that were **not**
    /// confirmed.  Disarms the tracker and clears the confirmed set.
    ///
    /// The caller provides candidate serials (e.g. all multi-objects of the
    /// current world inside the view rect).  The returned `Vec` contains
    /// serials that should be removed as stale.
    pub fn sweep(&mut self, candidates: &[u32]) -> Vec<u32> {
        let stale: Vec<u32> = candidates
            .iter()
            .copied()
            .filter(|s| !self.confirmed.contains(s))
            .collect();
        self.confirmed.clear();
        self.armed = false;
        stale
    }

    /// Disarm without sweeping (e.g. on resync where we rebuild from
    /// scratch anyway).
    pub fn disarm(&mut self) {
        self.armed = false;
        self.confirmed.clear();
    }
}
