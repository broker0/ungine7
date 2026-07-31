//! [`DiffOverlay`] — per-session mutable storage for map & statics diffs.
//!
//! When the server sends `EnableMapDiff` (0xBF sub 0x0018), the client
//! loads diff files from disk and stores the overrides here.  The overlay
//! is consulted by [`DiffAwareDataProvider`](super::diff_provider::DiffAwareDataProvider)
//! before falling back to the base [`StaticDataProvider`](super::StaticDataProvider).
//!
//! # Lifecycle
//!
//! - Created once per session.
//! - Updated via [`apply`](DiffOverlay::apply) each time the server
//!   sends `EnableMapDiff` (e.g. on zone/world change).
//! - Cleared for a world via [`clear_world`](DiffOverlay::clear_world)
//!   or entirely via [`clear`](DiffOverlay::clear).

use std::io;
use std::path::Path;

use log::{debug, warn};

use files::map_diff::{self, MapDiffData, StaticDiffData};

/// Number of worlds supported (Felucca, Trammel, Ilshenar, Malas, Tokuno, Ter Mur).
const MAX_WORLDS: usize = 6;

/// Per-session mutable storage for map & statics diffs.
///
/// Each world slot holds an optional pair of `(MapDiffData, StaticDiffData)`.
/// When a diff is applied, the old data for that world is replaced entirely.
#[derive(Clone)]
pub struct DiffOverlay {
    map_diffs: [Option<MapDiffData>; MAX_WORLDS],
    static_diffs: [Option<StaticDiffData>; MAX_WORLDS],
}

impl DiffOverlay {
    /// Create an empty overlay (no diffs for any world).
    pub fn new() -> Self {
        Self {
            map_diffs: [const { None }; MAX_WORLDS],
            static_diffs: [const { None }; MAX_WORLDS],
        }
    }

    /// Apply map and statics diffs for a single world.
    ///
    /// Replaces any previously loaded diffs for this world.
    pub fn apply(&mut self, world: u8, map_diff: MapDiffData, static_diff: StaticDiffData) {
        let idx = world as usize;
        if idx >= MAX_WORLDS {
            warn!("DiffOverlay::apply: world {world} out of range, ignoring");
            return;
        }

        debug!(
            "DiffOverlay: world {world}: {} map overrides, {} static overrides",
            map_diff.len(),
            static_diff.len(),
        );

        self.map_diffs[idx] = if map_diff.is_empty() { None } else { Some(map_diff) };
        self.static_diffs[idx] = if static_diff.is_empty() { None } else { Some(static_diff) };
    }

    /// Load and apply diffs from disk for all worlds described in the
    /// `EnableMapDiff` packet entries.
    ///
    /// `entries` is a slice of `(map_patches, static_patches)` per world,
    /// as received from the server.  Worlds with zero patches are cleared.
    ///
    /// Errors from individual world loads are logged as warnings and
    /// the world is silently cleared (no diff applied).
    pub fn load_and_apply(&mut self, data_dir: &Path, entries: &[(u32, u32)]) {
        debug!(
            "DiffOverlay::load_and_apply: {} world entries from {}",
            entries.len(),
            data_dir.display(),
        );

        for (world, &(map_patches, static_patches)) in entries.iter().enumerate() {
            let world = world as u8;

            if map_patches == 0 && static_patches == 0 {
                debug!(
                    "DiffOverlay: world {world}: 0 map + 0 static patches, clearing"
                );
                self.clear_world(world);
                continue;
            }

            debug!(
                "DiffOverlay: world {world}: loading {map_patches} map + \
                 {static_patches} static patches"
            );

            match Self::load_world(data_dir, world, map_patches, static_patches) {
                Ok((map_diff, static_diff)) => {
                    self.apply(world, map_diff, static_diff);
                }
                Err(e) => {
                    warn!(
                        "DiffOverlay: failed to load diffs for world {world}: {e}"
                    );
                    self.clear_world(world);
                }
            }
        }
    }

    fn load_world(
        data_dir: &Path,
        world: u8,
        map_patches: u32,
        static_patches: u32,
    ) -> io::Result<(MapDiffData, StaticDiffData)> {
        let map_diff = map_diff::read_map_diff(data_dir, world, map_patches)?;
        let static_diff = map_diff::read_static_diff(data_dir, world, static_patches)?;
        Ok((map_diff, static_diff))
    }

    /// Clear diffs for a single world.
    pub fn clear_world(&mut self, world: u8) {
        let idx = world as usize;
        if idx < MAX_WORLDS {
            let had_map = self.map_diffs[idx].is_some();
            let had_static = self.static_diffs[idx].is_some();
            self.map_diffs[idx] = None;
            self.static_diffs[idx] = None;
            if had_map || had_static {
                debug!("DiffOverlay: cleared diffs for world {world}");
            }
        }
    }

    /// Clear all diffs for all worlds.
    pub fn clear(&mut self) {
        let had_any = !self.is_empty();
        self.map_diffs = [const { None }; MAX_WORLDS];
        self.static_diffs = [const { None }; MAX_WORLDS];
        if had_any {
            debug!("DiffOverlay: cleared all diffs for all worlds");
        }
    }

    /// Whether there are any diffs loaded for any world.
    pub fn is_empty(&self) -> bool {
        self.map_diffs.iter().all(|d| d.is_none())
            && self.static_diffs.iter().all(|d| d.is_none())
    }

    // ── Accessors (used by DiffAwareDataProvider) ─────────────────────

    /// Get the map diff data for a world, if any.
    pub fn map_diff(&self, world: u8) -> Option<&MapDiffData> {
        self.map_diffs.get(world as usize).and_then(Option::as_ref)
    }

    /// Get the static diff data for a world, if any.
    pub fn static_diff(&self, world: u8) -> Option<&StaticDiffData> {
        self.static_diffs.get(world as usize).and_then(Option::as_ref)
    }
}

impl Default for DiffOverlay {
    fn default() -> Self {
        Self::new()
    }
}
