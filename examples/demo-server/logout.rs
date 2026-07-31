//! Offline-character logout system.
//!
//! When a real-account player disconnects, their character is not removed
//! immediately.  Instead, a 20-second grace timer is armed.  On expiry the
//! character is atomically transferred into the **logout storage zone**
//! (`LOGOUT_STORAGE_MAP`), where it is:
//!
//! - **Fully isolated** from the live world (combat, AI, collision — all
//!   zone-scoped, so the storage zone is never scanned by game logic).
//! - **Preserved with full state** — `transfer_entity` carries the entity,
//!   all container contents (backpack + nested), item properties, and any
//!   attached AI controller.
//! - **Automatically persisted** — the storage zone is saved by the ordinary
//!   world snapshot (`.save` / `--load`), just like any other zone.
//!
//! On re-login (before or after the timer fires) the character is transferred
//! back to its original world at the position stored in the entity's
//! `item_props.meta` under [`META_LOGOUT_RETURN`].
//!
//! # Future extension: Camping skill
//!
//! [`logout_delay`] currently returns a fixed [`DEFAULT_LOGOUT_DELAY`].
//! When the Camping skill is implemented, change the body to read
//! `entity.mobile().skills[CAMPING_SKILL_ID]` and compute a scaled delay.

use std::collections::HashMap;
use std::time::Duration;

use log::{info, warn};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use common::uo_engine::entity::DemoEntity;

use crate::game_util::engine_for;
use crate::DemoWorkerTx;

// ── Constants ─────────────────────────────────────────────────────────────

/// Map id of the offline-character storage zone.
///
/// This is a virtual zone that never maps to any UO facet; the UO wire
/// protocol never sends this id to clients.  The zone is auto-created by
/// the worker's zone factory on first use.
///
/// Re-exported from `common` where it is also used by `RestoreSnapshot`
/// to exclude the storage zone from crash-recovery orphan collection.
pub use common::uo_engine::handler::LOGOUT_STORAGE_MAP;

/// Default logout delay (20 seconds).
pub const DEFAULT_LOGOUT_DELAY: Duration = Duration::from_secs(20);

/// `item_props.meta` key that stores the return coordinates/world on a
/// character that is currently sitting in the storage zone.
///
/// Format: `"world|x|y|z|dir"` (pipe-separated integers).
pub const META_LOGOUT_RETURN: &str = "logout_return";

/// `item_props.meta` key that marks a character as "pending logout" —
/// the grace timer has been armed but the transfer to storage has not yet
/// occurred.
///
/// Value format: same `"world|x|y|z|dir"` string as [`META_LOGOUT_RETURN`],
/// so the restore path can read the return address without a separate key.
///
/// **Lifecycle:**
/// - Set in `cleanup_session` when the reaper timer is armed.
/// - Cleared in the reaper sub-task after a successful transfer to storage
///   (replaced by [`META_LOGOUT_RETURN`] on the storage-zone entity).
/// - Cleared in `resolve_normal_account_spawn` (Case 1) when the player
///   reconnects before the timer fires.
/// - Survives a world snapshot: `RestoreSnapshot` includes it in the
///   `logout_pending` list of `WorldEvent::SnapshotRestored`, and the
///   restore task arms the reaper with `delay = Duration::ZERO` so the
///   transfer happens immediately on the next server start.
pub const META_LOGOUT_PENDING: &str = "logout_pending";

// ── Delay calculation ─────────────────────────────────────────────────────

/// Return the logout grace period for the given entity.
///
/// Currently returns [`DEFAULT_LOGOUT_DELAY`] for all characters.
///
/// **TODO – Camping skill**: when the Camping skill is implemented, replace
/// the body with:
/// ```ignore
/// let camping = entity.mobile()
///     .and_then(|m| m.skills.get(&CAMPING_SKILL_ID))
///     .map(|sv| sv.value)
///     .unwrap_or(0);
/// // e.g. 20s base + up to 40s at 100 skill → clamp to [20s, 60s]
/// Duration::from_secs(20 + (camping as u64 * 40 / 100).min(40))
/// ```
pub fn logout_delay(_entity: &DemoEntity) -> Duration {
    DEFAULT_LOGOUT_DELAY
}

// ── Reaper command ────────────────────────────────────────────────────────

/// Commands sent to the [`run_logout_reaper`] task.
pub enum ReaperCmd {
    /// Arm (or re-arm) the logout timer for a character.
    Arm {
        serial: u32,
        world: u8,
        x: u16,
        y: u16,
        z: i8,
        dir: u8,
        delay: Duration,
    },
    /// Cancel a pending logout timer (player reconnected in time).
    Cancel {
        serial: u32,
    },
}

// ── Reaper task ───────────────────────────────────────────────────────────

/// Run the logout reaper task.
///
/// This is a long-lived async task that manages per-character logout timers.
/// For each `Arm` command it spawns a short-lived sub-task that sleeps for
/// the requested duration and then performs the cross-zone transfer.  If the
/// player reconnects before the timer fires, `Cancel` aborts the sub-task.
///
/// The task exits when the command channel is closed (all senders dropped,
/// i.e. server shutdown).
pub async fn run_logout_reaper(worker_tx: DemoWorkerTx, mut rx: mpsc::Receiver<ReaperCmd>) {
    // Map from serial → handle of the active sleep sub-task (if any).
    let mut handles: HashMap<u32, JoinHandle<()>> = HashMap::new();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            ReaperCmd::Arm { serial, world, x, y, z, dir, delay } => {
                // Cancel any existing timer for this serial first.
                if let Some(old) = handles.remove(&serial) {
                    old.abort();
                }

                info!(
                    "[logout] armed timer for {:#010X} (delay={}s, world={}, pos=({},{},{}))",
                    serial, delay.as_secs(), world, x, y, z,
                );

                let tx_clone = worker_tx.clone();
                let handle = tokio::spawn(async move {
                    tokio::time::sleep(delay).await;

                    let return_meta = format!("{}|{}|{}|{}|{}", world, x, y, z, dir);
                    let engine = engine_for(&tx_clone, world);

                    // Atomically swap META_LOGOUT_PENDING → META_LOGOUT_RETURN
                    // in the entity's item_props before transferring.
                    //
                    // - Remove META_LOGOUT_PENDING (no longer needed once the
                    //   transfer happens; also prevents the restore task from
                    //   re-arming a timer after a subsequent .save / --load).
                    // - Set META_LOGOUT_RETURN so the re-login path knows
                    //   where to return the character.
                    {
                        let mut props = engine.get_item_props(serial).await.unwrap_or_default();
                        props.meta.remove(META_LOGOUT_PENDING);
                        props.set_meta(
                            META_LOGOUT_RETURN,
                            common::uo_engine::item_props::MetaValue::Str(return_meta),
                        );
                        engine.set_item_props(serial, Some(props)).await;
                    }

                    // Atomically transfer entity (+ containers + props + controller)
                    // from the live world into the storage zone.
                    // The storage zone position doesn't matter (0,0,0); only the
                    // return meta is used to restore the character later.
                    match engine.transfer_entity(world, LOGOUT_STORAGE_MAP, serial, 0, 0, 0, Some(dir)).await {
                        Ok(_) => {
                            info!(
                                "[logout] transferred {:#010X} to storage zone (was world={}, ({},{},{}))",
                                serial, world, x, y, z,
                            );
                        }
                        Err(e) => {
                            warn!(
                                "[logout] transfer failed for {:#010X}: {:?} (entity may have been removed already)",
                                serial, e,
                            );
                        }
                    }
                });

                handles.insert(serial, handle);
            }

            ReaperCmd::Cancel { serial } => {
                if let Some(handle) = handles.remove(&serial) {
                    handle.abort();
                    info!("[logout] cancelled timer for {:#010X} (player reconnected)", serial);
                }
            }
        }
    }

    info!("[logout] reaper task exiting (channel closed)");
}
