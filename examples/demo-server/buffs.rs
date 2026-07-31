//! Timed buff system — tracks active stat modifications with expiration.
//!
//! Potions (and potentially other sources) can grant temporary stat boosts.
//! `BuffState` holds all active buffs for a session; its
//! [`expire_buffs`](BuffState::expire_buffs) method
//! checks for expired buffs and reverts the stat changes via engine commands.
//!
//! ## Design
//!
//! - Each buff records the `delta` applied and the `expires_at` instant.
//! - When a new buff of the same [`BuffKind`] is applied while one is still
//!   active, the old buff is reverted first (no stacking).
//! - The session's `RustGameLogicHandler` calls `tick()` on every regen
//!   tick (~2 s) to expire stale buffs cheaply.

use log::info;
use tokio::time::Instant;

use crate::DemoWorkerTx;

// ── BuffKind ─────────────────────────────────────────────────────────────

/// Kinds of timed buffs the system supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuffKind {
    /// Strength boost (applied via `engine.modify_str`).
    Strength,
    /// Agility / dexterity boost (applied via `engine.modify_dex`).
    Agility,
    /// Intelligence boost (applied via `engine.modify_int`).
    ///
    /// Reserved: the full apply/revert/name handling is wired up, but no
    /// source constructs it yet (no Cunning potion or INT spell). Kept for
    /// STR/DEX/INT symmetry until such a source is added.
    #[allow(dead_code)]
    Intelligence,
    /// Bless — boosts all three stats (STR + DEX + INT).
    Bless,
    /// Curse — reduces all three stats (STR + DEX + INT).
    Curse,
}

impl BuffKind {
    pub fn name(self) -> &'static str {
        match self {
            BuffKind::Strength     => "Strength",
            BuffKind::Agility      => "Agility",
            BuffKind::Intelligence => "Intelligence",
            BuffKind::Bless        => "Bless",
            BuffKind::Curse        => "Curse",
        }
    }
}

// ── ActiveBuff ───────────────────────────────────────────────────────────

/// A single active buff on the player.
#[derive(Debug, Clone)]
pub struct ActiveBuff {
    pub kind: BuffKind,
    /// When the buff expires (wall-clock `Instant`).
    pub expires_at: Instant,
    /// Signed delta that was applied to the stat.  On expiry, `−delta` is
    /// applied to revert the change.
    pub delta: i16,
}

// ── BuffState ────────────────────────────────────────────────────────────

/// Per-session buff tracker.
pub struct BuffState {
    /// Active buffs (small vec — typically 0–2 entries).
    active: Vec<ActiveBuff>,
}

impl BuffState {
    pub fn new() -> Self {
        Self { active: Vec::new() }
    }

    /// Add (or replace) a buff.
    ///
    /// If a buff of the same kind is already active, it is reverted first
    /// (the revert engine call is returned so the caller can `await` it).
    /// Then the new buff is recorded and applied.
    ///
    /// Returns the old delta to revert (if any).
    pub fn add_buff(&mut self, kind: BuffKind, delta: i16, duration_ms: u64) -> Option<i16> {
        let old_delta = self.remove_buff(kind);
        self.active.push(ActiveBuff {
            kind,
            expires_at: Instant::now() + std::time::Duration::from_millis(duration_ms),
            delta,
        });
        old_delta
    }

    /// Remove a buff of the given kind (if present) and return its delta
    /// so the caller can revert the stat modification.
    pub fn remove_buff(&mut self, kind: BuffKind) -> Option<i16> {
        if let Some(idx) = self.active.iter().position(|b| b.kind == kind) {
            let removed = self.active.swap_remove(idx);
            Some(removed.delta)
        } else {
            None
        }
    }

    /// Check all active buffs and expire any that have passed their
    /// deadline.  Returns a list of `(kind, delta_to_revert)` pairs.
    ///
    /// Call this periodically (e.g. on every regen tick).
    pub fn expire_buffs(&mut self) -> Vec<(BuffKind, i16)> {
        let now = Instant::now();
        let mut expired = Vec::new();
        self.active.retain(|b| {
            if now >= b.expires_at {
                expired.push((b.kind, b.delta));
                false // remove
            } else {
                true // keep
            }
        });
        expired
    }

    /// Returns `true` if any buffs are currently active.
    #[allow(dead_code)]
    pub fn has_active_buffs(&self) -> bool {
        !self.active.is_empty()
    }

    /// Return a read-only view of the active buffs.
    #[allow(dead_code)]
    pub fn active_buffs(&self) -> &[ActiveBuff] {
        &self.active
    }
}

// ── Engine helpers ───────────────────────────────────────────────────────

/// Apply a buff: modify the stat(s) on the engine side.
pub async fn apply_buff_stat(
    worker_tx: &DemoWorkerTx,
    world: u8,
    serial: u32,
    kind: BuffKind,
    delta: i16,
) {
    let engine = crate::game_util::engine_for(worker_tx, world);
    let d = delta as i32;
    match kind {
        BuffKind::Strength => {
            engine.modify_str(serial, d).await;
        }
        BuffKind::Agility => {
            engine.modify_dex(serial, d).await;
        }
        BuffKind::Intelligence => {
            engine.modify_int(serial, d).await;
        }
        BuffKind::Bless | BuffKind::Curse => {
            // Compound: modify all three stats.
            engine.modify_str(serial, d).await;
            engine.modify_dex(serial, d).await;
            engine.modify_int(serial, d).await;
        }
    }
    info!(
        "[buff] applied {} {:+} to 0x{:08X}",
        kind.name(), delta, serial,
    );
}

/// Revert a buff: apply the opposite delta.
pub async fn revert_buff_stat(
    worker_tx: &DemoWorkerTx,
    world: u8,
    serial: u32,
    kind: BuffKind,
    delta: i16,
) {
    let engine = crate::game_util::engine_for(worker_tx, world);
    let revert = -(delta as i32);
    match kind {
        BuffKind::Strength => {
            engine.modify_str(serial, revert).await;
        }
        BuffKind::Agility => {
            engine.modify_dex(serial, revert).await;
        }
        BuffKind::Intelligence => {
            engine.modify_int(serial, revert).await;
        }
        BuffKind::Bless | BuffKind::Curse => {
            // Compound: revert all three stats.
            engine.modify_str(serial, revert).await;
            engine.modify_dex(serial, revert).await;
            engine.modify_int(serial, revert).await;
        }
    }
    info!(
        "[buff] expired {} {:+} on 0x{:08X} (reverted {})",
        kind.name(), delta, serial, revert,
    );
}

/// Expire all stale buffs and revert their stat changes.
///
/// Called from the session's regen tick.
pub async fn tick_buffs(
    buff_state: &mut BuffState,
    worker_tx: &DemoWorkerTx,
    world: u8,
    serial: u32,
) {
    let expired = buff_state.expire_buffs();
    for (kind, delta) in expired {
        revert_buff_stat(worker_tx, world, serial, kind, delta).await;
    }
}
