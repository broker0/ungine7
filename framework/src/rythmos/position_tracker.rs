//! Player position tracking.
//!
//! [`PositionTracker`] maintains the authoritative position of the player
//! character during both log pre-processing and live playback.  It is the
//! single source of truth for the synthesised `DrawGamePlayer (0x20)` packets
//! that replace raw `MoveAck (0x22)` packets during replay.
//!
//! # Packets that update position
//!
//! | ID   | Name                   | What is updated                        |
//! |------|------------------------|----------------------------------------|
//! | 0x1B | CharacterLocaleAndBody | full reset: serial, coords, dir, body  |
//! | 0x20 | DrawGamePlayer         | full update: coords, dir, body, hue, flags |
//! | 0x77 | UpdateMobile (self)    | coords + direction (matched by serial) |
//! | 0x78 | DrawMobile (self)      | coords + direction + body + hue + flags (matched by serial) |
//!
//! # Movement stepping
//!
//! [`PositionTracker::step`] applies a one-tile delta for a given direction
//! byte from a `MoveRequest (0x02)`.  The running bit (`0x80`) is preserved in
//! the stored direction so that `DrawGamePlayer` reflects whether the character
//! was walking or running at that moment.

use log::trace;

use u_core::Facing;
use packets::character::{CharacterLocaleAndBody, DrawGamePlayer, UpdateMobile};
use packets::mobile_flags::MobileFlags;
use packets::traits::{ManualPacket, BasicPacket};
use packets::world::DrawMobile;

// ── PositionTracker ───────────────────────────────────────────────────────

/// Tracks the position and appearance of the player character.
#[derive(Debug, Clone, Copy)]
pub struct PositionTracker {
    /// Serial of the player character.  `0` means not yet initialised.
    pub serial: u32,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    /// Direction: compass heading + running flag.
    pub facing: Facing,
    pub body_type: u16,
    pub hue: u16,
    pub flags: u8,
}

impl Default for PositionTracker {
    fn default() -> Self {
        Self {
            serial: 0,
            x: 0,
            y: 0,
            z: 0,
            facing: Facing::new(0),
            body_type: 0,
            hue: 0,
            flags: 0,
        }
    }
}

impl PositionTracker {
    /// Returns `true` if the tracker has been initialised (serial ≠ 0).
    pub fn is_ready(&self) -> bool {
        self.serial != 0
    }

    /// Apply a `MoveRequest` direction byte: either a step or a turn-in-place.
    ///
    /// In UO, if the requested heading differs from the current facing the
    /// server acknowledges the packet but only rotates the character — no tile
    /// is crossed.  Coordinates are updated only when the heading already
    /// matches (i.e. the second consecutive request in the same direction).
    ///
    /// Returns `true` if a tile was actually crossed, `false` if it was a turn.
    pub fn step(&mut self, move_direction: Facing) -> bool {
        let new_heading = move_direction.heading();
        let cur_heading = self.facing.heading();

        // Always update facing (heading + running flag).
        self.facing = move_direction;

        if new_heading != cur_heading {
            // Turn only — no coordinate change.
            trace!(
                "[pos] turn: heading {} → {} at ({},{},{})",
                cur_heading, new_heading, self.x, self.y, self.z
            );
            return false;
        }
        let (dx, dy) = new_heading.delta();
        self.x = (self.x as i32 + dx).clamp(0, 0x1FFF) as u16;
        self.y = (self.y as i32 + dy).clamp(0, 0x1FFF) as u16;
        true
    }

    /// Build a `DrawGamePlayer (0x20)` packet from the current state.
    ///
    /// Panics if `self.serial == 0`; call [`is_ready`](Self::is_ready) first.
    pub fn to_draw_game_player(&self) -> DrawGamePlayer {
        DrawGamePlayer {
            id: DrawGamePlayer::ID,
            serial: self.serial,
            body_type: self.body_type,
            _pad0: (),
            hue: self.hue,
            flags: MobileFlags(self.flags),
            x: self.x,
            y: self.y,
            _pad1: (),
            direction: self.facing.raw(),
            z: self.z,
        }
    }

    // ── Typed apply methods ───────────────────────────────────────────

    /// Apply a pre-parsed `CharacterLocaleAndBody (0x1B)`.
    pub fn apply_character_locale(&mut self, p: &CharacterLocaleAndBody) {
        trace!(
            "[pos] 0x1B CharacterLocaleAndBody: serial={:#010X} ({},{},{}) dir={:#04X} body={:#06X}",
            p.serial, p.x, p.y, p.z, p.facing, p.body_type
        );
        self.serial = p.serial;
        self.x = p.x;
        self.y = p.y;
        self.z = p.z;
        self.facing = Facing::new(p.facing);
        self.body_type = p.body_type;
        // hue and flags are not carried by 0x1B; keep previous values
    }

    /// Apply a pre-parsed `DrawGamePlayer (0x20)`.
    pub fn apply_draw_game_player(&mut self, p: &DrawGamePlayer) {
        trace!(
            "[pos] 0x20 DrawGamePlayer: serial={:#010X} ({},{},{}) dir={:#04X} body={:#06X} hue={} flags={:#04X}",
            p.serial, p.x, p.y, p.z, p.direction, p.body_type, p.hue, p.flags.0
        );
        self.serial = p.serial;
        self.x = p.x;
        self.y = p.y;
        self.z = p.z;
        self.facing = Facing::new(p.direction);
        self.body_type = p.body_type;
        self.hue = p.hue;
        self.flags = p.flags.0;
    }

    /// Apply a pre-parsed `UpdateMobile (0x77)` if the serial matches.
    ///
    /// Returns `true` if the update was applied (serial matched).
    pub fn apply_update_mobile(&mut self, m: &UpdateMobile) -> bool {
        if m.serial != self.serial {
            trace!(
                "[pos] 0x77 UpdateMobile serial={:#010X} ≠ own {:#010X} — ignored",
                m.serial, self.serial
            );
            return false;
        }
        trace!(
            "[pos] 0x77 UpdateMobile (self): ({},{},{}) dir={:#04X}",
            m.x, m.y, m.z, m.direction
        );
        self.x = m.x;
        self.y = m.y;
        self.z = m.z;
        self.facing = Facing::new(m.direction);
        self.body_type = m.model;
        self.hue = m.hue;
        self.flags = m.status_flags.0;
        true
    }

    /// Apply a pre-parsed `DrawMobile (0x78)` if the serial matches.
    ///
    /// Returns `true` if the update was applied (serial matched).
    pub fn apply_draw_mobile(&mut self, m: &DrawMobile) -> bool {
        if m.serial != self.serial {
            trace!(
                "[pos] 0x78 DrawMobile serial={:#010X} ≠ own {:#010X} — ignored",
                m.serial, self.serial
            );
            return false;
        }
        trace!(
            "[pos] 0x78 DrawMobile (self): ({},{},{}) dir={:#04X} body={:#06X} hue={} flags={:#04X}",
            m.x, m.y, m.z, m.direction, m.graphic, m.color, m.status.0
        );
        self.x = m.x;
        self.y = m.y;
        self.z = m.z;
        self.facing = Facing::new(m.direction);
        self.body_type = m.graphic;
        self.hue = m.color;
        self.flags = m.status.0;
        true
    }

    // ── Convenience: raw-bytes ingestion ──────────────────────────────

    /// Update the tracker from a raw S→C packet.
    ///
    /// Recognises `0x1B`, `0x20`, `0x77`, `0x78`.  Silently ignores
    /// anything else or packets that fail to parse.
    ///
    /// Prefer the typed `apply_*` methods when the packet has already
    /// been parsed.
    pub fn update_from_packet(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        match data[0] {
            id if id == CharacterLocaleAndBody::ID => {
                if let Ok(p) = CharacterLocaleAndBody::from_bytes(data) {
                    self.apply_character_locale(&p);
                }
            }
            id if id == DrawGamePlayer::ID => {
                if let Ok(p) = DrawGamePlayer::from_bytes(data) {
                    self.apply_draw_game_player(&p);
                }
            }
            id if id == UpdateMobile::ID => {
                if let Ok(m) = UpdateMobile::from_bytes(data) {
                    self.apply_update_mobile(&m);
                }
            }
            id if id == DrawMobile::ID => {
                if let Ok(m) = DrawMobile::parse(data, false) {
                    self.apply_draw_mobile(&m);
                }
            }
            _ => {}
        }
    }
}
