//! Stat regeneration and meditation system.
//!
//! Provides periodic HP / Mana / Stamina regeneration for player sessions.
//! Meditation is an active skill that boosts mana regen while active and
//! is interrupted by damage or actions.
//!
//! ## Usage
//!
//! The session loop calls [`RegenState::tick`] on a periodic timer
//! (every [`regen::TICK_INTERVAL_MS`]).
//! Meditation is toggled via [`RegenState::start_meditation`] /
//! [`RegenState::stop_meditation`].

use crate::constants::regen;
use crate::DemoWorkerTx;

// ── MeditationState ───────────────────────────────────────────────────────

/// Whether the player is actively meditating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeditationState {
    /// Not meditating.
    Inactive,
    /// Actively meditating (bonus mana regen).
    Active,
}

// ── RegenState ────────────────────────────────────────────────────────────

/// Per-session regeneration state.
pub struct RegenState {
    pub meditation: MeditationState,
}

impl RegenState {
    pub fn new() -> Self {
        Self {
            meditation: MeditationState::Inactive,
        }
    }

    /// Returns `true` if the player is actively meditating.
    pub fn is_meditating(&self) -> bool {
        self.meditation == MeditationState::Active
    }

    /// Begin meditation. Returns a system message to send to the client.
    pub fn start_meditation(&mut self) -> &'static str {
        self.meditation = MeditationState::Active;
        "You enter a meditative trance."
    }

    /// End meditation (e.g. due to damage, action, or manual toggle).
    /// Returns `Some(message)` if meditation was actually active.
    pub fn stop_meditation(&mut self) -> Option<&'static str> {
        if self.meditation == MeditationState::Active {
            self.meditation = MeditationState::Inactive;
            Some("You stop meditating.")
        } else {
            None
        }
    }

    /// Perform one regen tick. Sends engine commands to heal/restore stats.
    ///
    /// `serial` is the player entity serial, `world` is the map ID.
    /// Returns packets to send to the session (currently none, but
    /// reserved for future "mana full" notifications).
    pub async fn tick(
        &self,
        serial: u32,
        world: u8,
        worker_tx: &DemoWorkerTx,
    ) {
        let engine = crate::game_util::engine_for(worker_tx, world);

        // HP regen
        let _ = engine.heal(serial, regen::HP_PER_TICK).await;

        // Mana regen (boosted by meditation)
        let mana_amount = if self.is_meditating() {
            regen::MANA_PER_TICK + regen::MANA_MEDITATION_BONUS
        } else {
            regen::MANA_PER_TICK
        };
        let _ = engine.modify_mana(serial, mana_amount as i32).await;

        // Stamina regen
        let _ = engine.modify_stamina(serial, regen::STAM_PER_TICK as i32).await;
    }
}
