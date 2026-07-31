//! Universal door mechanics for the path server.
//!
//! Ported from the demo server.  Understands **any** UO door graphic by
//! decoding the structure of the graphic id itself.
//!
//! # Door graphic layout
//!
//! UO doors are laid out in blocks of **16 graphics** per material (wood,
//! metal, barred, …).  Within a block the low nibble (`graphic & 0xF`)
//! encodes both the **hinge/facing** of the door and its **open/closed**
//! state:
//!
//! - **State** is the parity of the offset from the block base: an *even*
//!   offset is **closed**, an *odd* offset is **open**.
//! - **Facing** is `(offset & 0xF) >> 1` → one of 8 variants, each with its
//!   own offset applied to the leaf when it swings open.
//!
//! The known door block spans `0x0675..=0x06F4` (the [`DOOR_BLOCK_MIN`] /
//! [`DOOR_BLOCK_MAX`] range).  Beyond this purely arithmetic test, callers
//! should still confirm the `TileFlags::DOOR` flag via tiledata where
//! available.

// ── Auto-close timing ────────────────────────────────────────────────────────

/// Delay before an opened door automatically closes (milliseconds).
pub const DOOR_AUTO_CLOSE_MS: i64 = 10_000;

/// Re-check interval when a close is blocked by a mobile in the doorway
/// (milliseconds).  Once a door wants to close but the closed tile is
/// occupied, it polls at this short cadence so it shuts promptly after the
/// blocker steps away, rather than waiting a full [`DOOR_AUTO_CLOSE_MS`].
pub const DOOR_RETRY_CLOSE_MS: i64 = 1_000;

// ── Door graphic block ───────────────────────────────────────────────────────

/// First graphic of the contiguous door range.
pub const DOOR_BLOCK_MIN: u16 = 0x0675;
/// Last graphic (inclusive) of the contiguous door range.
pub const DOOR_BLOCK_MAX: u16 = 0x06F4;

// ── Facing offsets ───────────────────────────────────────────────────────────
//
// Indexed by the pair index within a 16-block: `(rel & 0xF) >> 1` (0..=7),
// where `rel` is the offset from the block base.  Each entry is the `(dx, dy)`
// applied to the door leaf when it opens; closing applies `(-dx, -dy)`.

/// Per-facing `(dx, dy)` shift applied to a door leaf when it opens.
const DOOR_OFFSETS: [(i16, i16); 8] = [
    (-1, 1),  // 0 — west, CW   (verified)
    (1, 1),   // 1 — east, CW   (unverified)
    (-1, 0),  // 2 — west, CCW  (verified)
    (1, 0),   // 3 — east, CCW  (unverified)
    (1, 1),   // 4 — east double, south leaf (verified: 0x06AD)
    (1, -1),  // 5 — east double, north leaf (verified: 0x06AF)
    (-1, -1), // 6 — south, CCW (unverified)
    (0, 0),   // 7 — north, CW  (unverified)
];

// ── DoorState ──────────────────────────────────────────────────────────────

/// Decoded state of a door graphic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorState {
    /// Closed graphic for this door's block + facing.
    pub closed: u16,
    /// Open graphic for this door's block + facing.
    pub open: u16,
    /// Whether the supplied graphic is currently in the *open* state.
    pub is_open: bool,
    /// Tile shift applied when the door opens (closing applies the inverse).
    pub open_dx: i16,
    /// Tile shift applied when the door opens (closing applies the inverse).
    pub open_dy: i16,
}

/// Decode a door graphic into its [`DoorState`].
///
/// This is pure arithmetic and makes no validity check — call
/// [`is_door_graphic`] (or a tiledata `TileFlags::DOOR` test) first to be
/// sure `graphic` actually is a door.
pub fn classify(graphic: u16) -> DoorState {
    let base = DOOR_BLOCK_MIN;
    let rel = graphic.saturating_sub(base);
    let is_open = (rel & 1) == 1;
    let closed_rel = rel & !1;
    let closed = base + closed_rel;
    let open = closed + 1;
    let facing = ((rel & 0xF) >> 1) as usize;
    let (open_dx, open_dy) = DOOR_OFFSETS[facing & 0x7];
    DoorState { closed, open, is_open, open_dx, open_dy }
}

/// Compute the result of toggling a door.
///
/// Returns `(new_graphic, dx, dy)` where `(dx, dy)` is the tile shift to
/// apply to the door's current position:
///
/// - currently **closed** → opens: `new_graphic = open`, shift `+(dx, dy)`.
/// - currently **open**   → closes: `new_graphic = closed`, shift `-(dx, dy)`.
pub fn toggle_target(graphic: u16) -> (u16, i16, i16) {
    let s = classify(graphic);
    if s.is_open {
        (s.closed, -s.open_dx, -s.open_dy)
    } else {
        (s.open, s.open_dx, s.open_dy)
    }
}

/// Compute the closed state of a door, given any graphic in its family.
///
/// Returns `(closed_graphic, dx, dy)` where `(dx, dy)` is the shift to apply
/// to the *current* (open) position to return the leaf to its closed tile.
/// If the door is already closed, `(dx, dy)` is `(0, 0)`.
pub fn close_target(graphic: u16) -> (u16, i16, i16) {
    let s = classify(graphic);
    if s.is_open {
        (s.closed, -s.open_dx, -s.open_dy)
    } else {
        (s.closed, 0, 0)
    }
}

/// Cheap arithmetic test: is `graphic` within the known door block?
///
/// This is a fallback used when tiledata is unavailable (no `--data`).
/// When tiledata *is* loaded, prefer the authoritative `TileFlags::DOOR`
/// flag, which covers door families outside this block too.
pub fn is_door_graphic(graphic: u16) -> bool {
    (DOOR_BLOCK_MIN..=DOOR_BLOCK_MAX).contains(&graphic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity_decides_state() {
        assert!(!classify(0x0675).is_open);
        assert!(classify(0x0676).is_open);
        assert!(!classify(0x06A5).is_open);
        assert!(classify(0x06A6).is_open);
    }

    #[test]
    fn closed_open_pairing() {
        let s = classify(0x0676); // open
        assert_eq!(s.closed, 0x0675);
        assert_eq!(s.open, 0x0676);
        let c = classify(0x0675);
        assert_eq!((c.closed, c.open), (0x0675, 0x0676));
    }

    #[test]
    fn toggle_is_reversible() {
        let closed = 0x0675u16;
        let (opened, dx1, dy1) = toggle_target(closed);
        assert_eq!(opened, 0x0676);
        let (reclosed, dx2, dy2) = toggle_target(opened);
        assert_eq!(reclosed, closed);
        assert_eq!(dx1 + dx2, 0);
        assert_eq!(dy1 + dy2, 0);
    }

    #[test]
    fn block_bounds() {
        assert!(is_door_graphic(DOOR_BLOCK_MIN));
        assert!(is_door_graphic(DOOR_BLOCK_MAX));
        assert!(!is_door_graphic(DOOR_BLOCK_MIN - 1));
        assert!(!is_door_graphic(DOOR_BLOCK_MAX + 1));
    }

    #[test]
    fn close_target_idempotent_when_closed() {
        let (g, dx, dy) = close_target(0x0675);
        assert_eq!(g, 0x0675);
        assert_eq!((dx, dy), (0, 0));
    }

    #[test]
    fn east_double_door_leaves_open_to_own_tiles() {
        let south = classify(0x06AD);
        assert!(!south.is_open);
        assert_eq!((south.open_dx, south.open_dy), (1, 1));
        let (_g, dx, dy) = toggle_target(0x06AD);
        assert_eq!((1439 + dx, 1613 + dy), (1440, 1614));

        let north = classify(0x06AF);
        assert!(!north.is_open);
        assert_eq!((north.open_dx, north.open_dy), (1, -1));
        let (_g, dx, dy) = toggle_target(0x06AF);
        assert_eq!((1439 + dx, 1612 + dy), (1440, 1611));
    }

    #[test]
    fn east_double_door_reversible() {
        for closed in [0x06ADu16, 0x06AF] {
            let (opened, dx1, dy1) = toggle_target(closed);
            let (reclosed, dx2, dy2) = toggle_target(opened);
            assert_eq!(reclosed, closed);
            assert_eq!(dx1 + dx2, 0);
            assert_eq!(dy1 + dy2, 0);
        }
    }
}
