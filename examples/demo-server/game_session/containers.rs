//! Session-side tracking of open container gumps.
//!
//! Tracks which containers the player currently has open so we can:
//! - Auto-close containers when the player walks out of range
//! - Close bank/vendor containers on any movement
//! - Filter container-interior events to only forward relevant ones
//! - Clean up on disconnect

use std::collections::{HashMap, HashSet};

use log::{debug, info, trace, warn};

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};
use packets::system::GeneralInfo;

use framework::continuum::{ContainerContentChange, WorldEvent};

use u_core::ProtocolVersion;

// ── Container classification ─────────────────────────────────────────────

/// How the container should behave with respect to range and movement.
#[derive(Debug, Clone)]
pub(super) enum ContainerKind {
    /// Player's own backpack — never auto-closed by range.
    OwnBackpack,

    /// Bank box — closes when the player moves away from the position
    /// where the bank was opened (Chebyshev distance > 0).
    Bank { x: u16, y: u16 },

    /// Vendor buy/sell container — closes on **any** movement.
    Vendor,

    /// A container on the ground at a fixed world position.
    /// Closed when Chebyshev distance from player exceeds `max_range`.
    /// Also used for containers equipped on another mobile (e.g. looting
    /// someone's backpack), recorded at the mobile's open-time position.
    Ground { x: u16, y: u16 },

    /// Inside another container (nested).  Lifetime is tied to the
    /// parent — if the parent is closed, all children close too.
    Nested { parent_serial: u32 },
}

/// Maximum Chebyshev distance (tiles) before a ground container is
/// auto-closed.  The UO client auto-closes container gumps at distance 4+,
/// so we close at > 3 to stay in sync.  Interaction (pick up / drop) is
/// still limited to distance ≤ 2 by the engine.
const CONTAINER_MAX_RANGE: u16 = 3;

// ── OpenContainers ───────────────────────────────────────────────────────

/// Tracks which container gumps the player currently has open.
#[derive(Debug, Default)]
pub(super) struct OpenContainers {
    containers: HashMap<u32, ContainerKind>,
}

impl OpenContainers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a container as open.
    pub fn open(&mut self, serial: u32, kind: ContainerKind) {
        let was_open = self.containers.contains_key(&serial);
        self.containers.insert(serial, kind);
        if was_open {
            debug!(
                "[open_containers] RE-OPEN container=0x{:08X} (was already open), \
                 {} open container(s)",
                serial, self.containers.len(),
            );
        } else {
            info!(
                "[open_containers] OPEN container=0x{:08X}, kind={:?}, \
                 now {} open container(s)",
                serial, self.containers.get(&serial).unwrap(),
                self.containers.len(),
            );
        }
    }

    /// Close a single container (unregister it).
    #[allow(dead_code)]
    pub fn close(&mut self, serial: u32) {
        if let Some(kind) = self.containers.remove(&serial) {
            info!(
                "[open_containers] CLOSE container=0x{:08X}, was kind={:?}, \
                 now {} open container(s)",
                serial, kind, self.containers.len(),
            );
        }
    }

    /// Check whether a container is currently open.
    #[allow(dead_code)]
    pub fn is_open(&self, serial: u32) -> bool {
        self.containers.contains_key(&serial)
    }

    /// Close all containers that should be closed due to player movement.
    ///
    /// - `Bank` containers are closed when the player moves away from
    ///   the position where the bank was opened (any step).
    /// - `Vendor` containers are closed on **any** step.
    /// - `Ground` containers are closed when the Chebyshev distance
    ///   exceeds [`CONTAINER_MAX_RANGE`].
    /// - `Nested` containers are closed if their root ancestor was closed.
    /// - `OwnBackpack` is never closed by movement.
    ///
    /// Returns the serials of all containers that were closed (for sending
    /// `CloseGump` packets to the client).
    pub fn close_on_move(&mut self, player_x: u16, player_y: u16) -> Vec<u32> {
        // Phase 1: identify containers to close (not nested).
        let mut to_close: Vec<u32> = Vec::new();
        for (&serial, kind) in &self.containers {
            match kind {
                ContainerKind::OwnBackpack => {}
                ContainerKind::Vendor => {
                    to_close.push(serial);
                }
                ContainerKind::Bank { x, y } => {
                    // Close only when the player has actually moved away
                    // from the position where the bank was opened.
                    if chebyshev(player_x, player_y, *x, *y) > 0 {
                        to_close.push(serial);
                    }
                }
                ContainerKind::Ground { x, y } => {
                    if chebyshev(player_x, player_y, *x, *y) > CONTAINER_MAX_RANGE {
                        to_close.push(serial);
                    }
                }
                ContainerKind::Nested { .. } => {
                    // Handled in phase 2.
                }
            }
        }

        // Phase 2: close nested containers whose parent was closed.
        // Repeat until no more orphans are found.
        loop {
            let mut more = Vec::new();
            for (&serial, kind) in &self.containers {
                if let ContainerKind::Nested { parent_serial } = kind {
                    if to_close.contains(parent_serial) && !to_close.contains(&serial) {
                        more.push(serial);
                    }
                }
            }
            if more.is_empty() {
                break;
            }
            to_close.extend(&more);
        }

        // Phase 3: actually remove them.
        if !to_close.is_empty() {
            for &serial in &to_close {
                let kind = self.containers.remove(&serial);
                warn!(
                    "[open_containers] close_on_move: CLOSING container=0x{:08X}, \
                     kind={:?}, player_pos=({},{})",
                    serial, kind, player_x, player_y,
                );
            }
        }

        to_close
    }

    /// Close a specific container and all containers nested inside it
    /// (recursively).
    ///
    /// Used when an entity is removed from the world (EntityRemoved) or
    /// when the player explicitly closes a container.
    ///
    /// Returns serials of all closed containers.
    pub fn close_with_children(&mut self, serial: u32) -> Vec<u32> {
        let mut closed = Vec::new();
        if let Some(kind) = self.containers.remove(&serial) {
            info!(
                "[open_containers] close_with_children: CLOSING root container=0x{:08X}, \
                 kind={:?}",
                serial, kind,
            );
            closed.push(serial);
        }
        // Cascade to children.
        loop {
            let mut more = Vec::new();
            for (&s, kind) in &self.containers {
                if let ContainerKind::Nested { parent_serial } = kind {
                    if closed.contains(parent_serial) {
                        more.push(s);
                    }
                }
            }
            if more.is_empty() {
                break;
            }
            for &s in &more {
                let kind = self.containers.remove(&s);
                info!(
                    "[open_containers] close_with_children: CLOSING nested container=0x{:08X}, \
                     kind={:?}",
                    s, kind,
                );
                closed.push(s);
            }
        }
        closed
    }

    /// Drain all entries — used on disconnect for cleanup.
    #[allow(dead_code)]
    pub fn drain(&mut self) -> Vec<u32> {
        self.containers.drain().map(|(serial, _)| serial).collect()
    }

    /// Return the set of all currently open container serials.
    ///
    /// Used to build the `accessible_containers` set for engine commands
    /// so that item pickup/drop operations are restricted to containers
    /// the player has legitimately opened (own backpack, double-clicked
    /// world containers, nested sub-containers, etc.).
    pub fn all_open_serials(&self) -> HashSet<u32> {
        self.containers.keys().copied().collect()
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Build a `CloseGump` (0xBF sub 0x04) raw packet for a container serial.
pub(super) fn close_container_gump_packet(container_serial: u32) -> RawPacket {
    let pkt = GeneralInfo::CloseGump {
        dialog_id: container_serial,
        button_id: 0,
    };
    RawPacket::s2c(pkt.to_bytes())
}

/// Chebyshev (king-move) distance between two tile positions.
fn chebyshev(x1: u16, y1: u16, x2: u16, y2: u16) -> u16 {
    crate::game_util::chebyshev(x1, y1, x2, y2)
}

// ── Container content synchronisation ────────────────────────────────────

/// Translate a [`WorldEvent::ContainerContentsUpdated`] into UO packets,
/// appending them to `out`.
///
/// Only produces packets if:
/// - The event is a `ContainerContentsUpdated`.
/// - The session currently has the affected container open.
///
/// This is called from the game session's world-event loop so that all
/// sessions viewing a container (including the initiator) see changes.
pub(super) fn collect_container_update_packets(
    event: &WorldEvent,
    open_containers: &OpenContainers,
    player_serial: u32,
    out: &mut Vec<RawPacket>,
    client_version: ProtocolVersion,
) {
    let (container_serial, changes) = match event {
        WorldEvent::ContainerContentsUpdated {
            container_serial, changes, ..
        } => (*container_serial, changes),
        _ => return,
    };

    let is_open = open_containers.is_open(container_serial);
    if is_open {
        trace!(
            "[container_update] player=0x{:08X}: ContainerContentsUpdated: \
             container=0x{:08X}, is_open=true, {} change(s) → generating packets",
            player_serial, container_serial, changes.len(),
        );
    } else {
        trace!(
            "[container_update] player=0x{:08X}: ContainerContentsUpdated: \
             container=0x{:08X}, is_open=false, {} change(s) → SKIPPING",
            player_serial, container_serial, changes.len(),
        );
    }

    if !is_open {
        return;
    }

    use packets::interaction::DeleteObject;

    for change in changes {
        match change {
            ContainerContentChange::ItemAdded {
                item_serial, graphic, amount, x, y, color,
            } => {
                trace!(
                    "[container_update] -> 0x25 ItemAdded: serial=0x{:08X}, \
                     graphic=0x{:04X}, amount={}, pos=({},{}), container=0x{:08X}",
                    item_serial, graphic, amount, x, y, container_serial,
                );
                out.push(common::spawn::build_add_item_to_container(
                    *item_serial, *graphic, *amount, *x, *y,
                    container_serial, *color, 0, client_version,
                ));
            }
            ContainerContentChange::ItemRemoved { item_serial } => {
                debug!(
                    "[container_update] -> 0x1D ItemRemoved: serial=0x{:08X}, \
                     container=0x{:08X}",
                    item_serial, container_serial,
                );
                out.push(RawPacket::s2c(encode_packet(&DeleteObject {
                    id: DeleteObject::ID,
                    serial: *item_serial,
                })));
            }
            ContainerContentChange::ItemUpdated {
                item_serial, graphic, amount, x, y, color,
            } => {
                debug!(
                    "[container_update] -> 0x25 ItemUpdated: serial=0x{:08X}, \
                     graphic=0x{:04X}, amount={}, pos=({},{}), container=0x{:08X}",
                    item_serial, graphic, amount, x, y, container_serial,
                );
                out.push(common::spawn::build_add_item_to_container(
                    *item_serial, *graphic, *amount, *x, *y,
                    container_serial, *color, 0, client_version,
                ));
            }
        }
    }
}
