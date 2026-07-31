//! Ship gangplank (boarding plank) mechanics for the demo server.
//!
//! A plank is a ship component (see [`crate::ships`]) that behaves like a
//! door: double-clicking it toggles between a **closed** and an **open**
//! graphic.  Unlike a house door it does **not** shift position when it
//! opens — the leaf is drawn open on the same tile.
//!
//! # Graphic layout
//!
//! Classic UO planks come as **even/odd pairs**: the closed graphic is even,
//! the open graphic is the next id (`closed | 1`).  Each ship heading has its
//! own closed graphic, recorded per-component in [`crate::ships::ComponentDef`]
//! and copied onto the plank item's `ItemProps.meta` so the engine can swap
//! the art on a ship turn.
//!
//! Boarding/disembarking (teleport between deck and shore) is **not**
//! implemented in this step — only open/close and carry-with-ship.

use crate::ships;

/// Decoded state of a plank graphic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlankState {
    /// Closed graphic for this plank.
    pub closed: u16,
    /// Open graphic for this plank.
    pub open: u16,
    /// Whether the supplied graphic is currently in the *open* state.
    pub is_open: bool,
}

/// Decode a plank graphic into its [`PlankState`].
///
/// Planks are even/odd pairs: `closed = graphic & !1`, `open = closed | 1`,
/// and the parity of the graphic decides the state.
pub fn classify(graphic: u16) -> PlankState {
    let is_open = (graphic & 1) == 1;
    let closed = graphic & !1;
    let open = closed | 1;
    PlankState { closed, open, is_open }
}

/// Compute the graphic to toggle a plank to.
///
/// Returns the new graphic: closed → open, open → closed.  Unlike a door the
/// plank does not move, so no position delta is returned.
pub fn toggle_target(graphic: u16) -> u16 {
    let s = classify(graphic);
    if s.is_open { s.closed } else { s.open }
}

/// Compute the closed graphic for any plank graphic in the pair.
#[allow(dead_code)]
pub fn close_target(graphic: u16) -> u16 {
    classify(graphic).closed
}

/// Return `true` if the given role string marks a ship plank (port or
/// starboard).
pub fn is_plank_role(role: Option<&str>) -> bool {
    matches!(role, Some(ships::ROLE_PLANK_PORT) | Some(ships::ROLE_PLANK_STAR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity_decides_state() {
        assert!(!classify(0x3ED4).is_open); // even = closed
        assert!(classify(0x3ED4 | 1).is_open); // odd = open
    }

    #[test]
    fn toggle_is_reversible() {
        let closed = 0x3ED4u16; // even
        let opened = toggle_target(closed);
        assert_eq!(opened, 0x3ED4 | 1);
        let reclosed = toggle_target(opened);
        assert_eq!(reclosed, closed);
    }

    #[test]
    fn close_target_idempotent() {
        assert_eq!(close_target(0x3ED4), 0x3ED4);
        assert_eq!(close_target(0x3ED4 | 1), 0x3ED4);
    }
}
