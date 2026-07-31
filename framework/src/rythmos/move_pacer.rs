//! Movement pacing — walk / run speed control.
//!
//! [`MovePacer`] enforces the correct delay between consecutive movement
//! steps based on the current [`MoveSpeed`] (walk or run) and whether the
//! character is mounted.
//!
//! # UO movement timing
//!
//! The classic UO client uses fixed per-step delays:
//!
//! | Speed | Delay |
//! |-------|-------|
//! | Walk  | ~400 ms (unmounted) |
//! | Run   | ~200 ms (unmounted) |
//! | Walk (mounted) | ~200 ms |
//! | Run (mounted)  | ~100 ms |
//!
//! [`MovePacer`] does **not** enforce these exact values — it is
//! configurable via [`MoveSpeed::delay`].

use std::time::Duration;

use tokio::time::Instant;

use u_core::Facing;

// -- MoveSpeed ----------------------------------------------------------------

/// Movement speed tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveSpeed {
    /// Walking pace (unmounted).
    Walk,
    /// Running pace (unmounted).
    Run,
    /// Walking pace (mounted).
    MountedWalk,
    /// Running pace (mounted).
    MountedRun,
    /// Turn in place (no tile movement).
    TurnOnly,
}

impl MoveSpeed {
    /// Per-step delay for this speed tier.
    pub fn delay(self) -> Duration {
        match self {
            Self::Walk => Duration::from_millis(400),
            Self::Run => Duration::from_millis(200),
            Self::MountedWalk => Duration::from_millis(200),
            Self::MountedRun => Duration::from_millis(100),
            Self::TurnOnly => Duration::from_millis(100),
        }
    }

    /// Determine the speed tier from a [`Facing`] byte (running flag)
    /// and whether the character is mounted.
    pub fn from_facing(facing: Facing, mounted: bool) -> Self {
        let running = facing.is_running();
        match (running, mounted) {
            (false, false) => Self::Walk,
            (true, false) => Self::Run,
            (false, true) => Self::MountedWalk,
            (true, true) => Self::MountedRun,
        }
    }
}

// -- MovePacer ----------------------------------------------------------------

/// Enforces minimum delay between consecutive movement steps.
///
/// Call [`can_move`](Self::can_move) before sending a `MoveRequest` to the
/// server.  If it returns `true`, proceed and call
/// [`record_move`](Self::record_move) afterwards.
#[derive(Debug, Clone)]
pub struct MovePacer {
    /// Instant of the last accepted movement step.
    last_move: Option<Instant>,
}

impl Default for MovePacer {
    fn default() -> Self {
        Self::new()
    }
}

impl MovePacer {
    /// Create a new pacer with no movement history.
    pub fn new() -> Self {
        Self { last_move: None }
    }

    /// Whether enough time has elapsed since the last step for the given
    /// speed tier.
    pub fn can_move(&self, speed: MoveSpeed) -> bool {
        match self.last_move {
            None => true,
            Some(last) => last.elapsed() >= speed.delay(),
        }
    }

    /// Record that a movement step was accepted at the current instant.
    pub fn record_move(&mut self) {
        self.last_move = Some(Instant::now());
    }

    /// Time remaining until the next step is allowed at the given speed.
    /// Returns `Duration::ZERO` if a step is already allowed.
    pub fn time_until_ready(&self, speed: MoveSpeed) -> Duration {
        match self.last_move {
            None => Duration::ZERO,
            Some(last) => {
                let elapsed = last.elapsed();
                let delay = speed.delay();
                delay.saturating_sub(elapsed)
            }
        }
    }

    /// Reset the pacer (e.g. after a teleport or world change).
    pub fn reset(&mut self) {
        self.last_move = None;
    }
}
