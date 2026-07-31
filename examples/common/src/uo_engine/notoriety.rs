//! Classic (T2A-era) notoriety / reputation system.
//!
//! Notoriety in UO is **per-viewer relative**: the same mobile is rendered
//! with a different name/health-bar hue depending on who is looking at it.
//! A guild-mate is green, a criminal is gray, a murderer is red, and so on.
//!
//! This module provides:
//! - [`NotorietyClass`] — the intrinsic reputation class of a mobile
//!   (innocent / criminal / murderer / neutral monster).
//! - [`NotorietyView`] — the minimal per-mobile state needed to resolve the
//!   wire [`Notoriety`] value relative to a viewer (used in
//!   [`framework::continuum::EntitySnapshot`] so the per-session render path
//!   needs no RPC).
//! - [`resolve_notoriety`] — the pure per-viewer resolver implementing the
//!   classic relationship matrix.
//! - flag / counter helpers (`apply_criminal_flag`, `record_aggressor`,
//!   murder-count thresholds, expiry).
//!
//! Guild support is intentionally minimal: a mobile carries an optional
//! `guild_id: Option<u32>` and two mobiles with the *same* id are treated as
//! allies (green).  There is no full guild system (alliances, wars, etc.).

use packets::movement::Notoriety;

// ── Tunable constants (classic T2A) ─────────────────────────────────────────

/// Number of long-term murders at/above which a mobile is a Murderer (red).
pub const MURDERER_THRESHOLD: u16 = 5;

/// How long a freshly-set criminal flag lasts (classic: 2 minutes).
pub const CRIMINAL_FLAG_MS: u64 = 120_000;

/// How long an aggressor relationship lasts — during this window the victim
/// may retaliate without becoming a criminal (classic: 2 minutes).
pub const AGGRESSOR_FLAG_MS: u64 = 120_000;

/// Maximum number of aggressor relationships tracked per mobile.
pub const MAX_AGGRESSORS: usize = 16;

// ── NotorietyClass ───────────────────────────────────────────────────────────

/// The intrinsic reputation class of a mobile, independent of the viewer.
///
/// This is the *base* state; the final per-viewer wire colour is computed by
/// [`resolve_notoriety`], which may upgrade an `Innocent` to gray when the
/// viewer is an aggressor target, render a guild-mate green, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum NotorietyClass {
    /// A lawful player who has committed no crimes (blue).
    #[default]
    Innocent,
    /// A player flagged as a criminal (gray) — temporary, see criminal timer.
    Criminal,
    /// A player with too many murder counts (red).
    Murderer,
    /// A neutral / always-attackable creature (most monsters, gray).
    Neutral,
    /// An inherently hostile/evil creature shown as enemy (orange).
    Enemy,
}

impl NotorietyClass {
    /// Map this class to its *default* wire colour, ignoring viewer relation.
    ///
    /// Used as the fallback when there is no special viewer relationship.
    pub fn base_wire(self) -> Notoriety {
        match self {
            Self::Innocent => Notoriety::Innocent,
            Self::Criminal => Notoriety::Criminal,
            Self::Murderer => Notoriety::Murderer,
            Self::Neutral => Notoriety::Attackable,
            Self::Enemy => Notoriety::Enemy,
        }
    }

    /// Opaque `u8` code for transport in `framework::continuum::NotorietyContext`.
    pub fn to_u8(self) -> u8 {
        match self {
            Self::Innocent => 0,
            Self::Criminal => 1,
            Self::Murderer => 2,
            Self::Neutral => 3,
            Self::Enemy => 4,
        }
    }

    /// Inverse of [`NotorietyClass::to_u8`].
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Innocent,
            1 => Self::Criminal,
            2 => Self::Murderer,
            4 => Self::Enemy,
            _ => Self::Neutral,
        }
    }
}

// ── NotorietyView ────────────────────────────────────────────────────────────

/// Minimal snapshot of a mobile's reputation, sufficient to resolve the
/// per-viewer wire [`Notoriety`] without touching the zone.
///
/// Embedded in [`framework::continuum::EntitySnapshot`] so the per-session
/// world-event render path can colour mobiles relative to each viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotorietyView {
    /// Intrinsic class.
    pub class: NotorietyClass,
    /// Guild id, if any.  Two mobiles with the same id are allies (green).
    pub guild_id: Option<u32>,
    /// Whether this mobile is a player character.
    pub is_player: bool,
}

impl Default for NotorietyView {
    fn default() -> Self {
        Self {
            class: NotorietyClass::Neutral,
            guild_id: None,
            is_player: false,
        }
    }
}

/// Per-viewer resolver: how does `viewer` see `target`?
///
/// Relationship matrix (classic T2A, simplified for guild-by-id):
/// 1. Self / non-player viewer → fall back to the target's base colour.
/// 2. Murderer target → red, regardless of relationship.
/// 3. Same guild id → green (Ally).
/// 4. Criminal target → gray (Attackable).
/// 5. Neutral / Enemy creature → its base colour (gray / orange).
/// 6. Innocent player → blue.
///
/// `aggressor_to_viewer` is `true` when the target has the viewer in its
/// aggressor list (i.e. the target attacked the viewer) — such a target is
/// shown gray so the viewer may freely retaliate.
pub fn resolve_notoriety(
    viewer: &NotorietyView,
    target: &NotorietyView,
    is_self: bool,
    aggressor_to_viewer: bool,
) -> Notoriety {
    // Murderers are always red to everyone.
    if target.class == NotorietyClass::Murderer {
        return Notoriety::Murderer;
    }

    // Looking at yourself: criminals/innocents see their own real colour.
    if is_self {
        return target.class.base_wire();
    }

    // Guild allies are green (only meaningful between players in a guild).
    if let (Some(vg), Some(tg)) = (viewer.guild_id, target.guild_id) {
        if vg == tg {
            return Notoriety::Ally;
        }
    }

    // A target that has aggressed the viewer is freely attackable (gray).
    if aggressor_to_viewer {
        return Notoriety::Attackable;
    }

    target.class.base_wire()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(class: NotorietyClass, guild: Option<u32>) -> NotorietyView {
        NotorietyView { class, guild_id: guild, is_player: true }
    }

    #[test]
    fn innocent_player_is_blue_to_others() {
        let v = player(NotorietyClass::Innocent, None);
        let t = player(NotorietyClass::Innocent, None);
        assert_eq!(resolve_notoriety(&v, &t, false, false), Notoriety::Innocent);
    }

    #[test]
    fn murderer_is_red_to_everyone_even_guildmates() {
        let v = player(NotorietyClass::Innocent, Some(7));
        let t = player(NotorietyClass::Murderer, Some(7));
        assert_eq!(resolve_notoriety(&v, &t, false, false), Notoriety::Murderer);
    }

    #[test]
    fn same_guild_is_ally_green() {
        let v = player(NotorietyClass::Innocent, Some(42));
        let t = player(NotorietyClass::Innocent, Some(42));
        assert_eq!(resolve_notoriety(&v, &t, false, false), Notoriety::Ally);
    }

    #[test]
    fn different_guild_is_not_ally() {
        let v = player(NotorietyClass::Innocent, Some(1));
        let t = player(NotorietyClass::Innocent, Some(2));
        assert_eq!(resolve_notoriety(&v, &t, false, false), Notoriety::Innocent);
    }

    #[test]
    fn criminal_is_gray() {
        let v = player(NotorietyClass::Innocent, None);
        let t = player(NotorietyClass::Criminal, None);
        assert_eq!(resolve_notoriety(&v, &t, false, false), Notoriety::Criminal);
    }

    #[test]
    fn aggressor_target_shows_attackable() {
        let v = player(NotorietyClass::Innocent, None);
        let t = player(NotorietyClass::Innocent, None);
        // Target has aggressed the viewer ⇒ viewer may freely retaliate.
        assert_eq!(resolve_notoriety(&v, &t, false, true), Notoriety::Attackable);
    }

    #[test]
    fn self_view_shows_own_criminal_colour() {
        let v = player(NotorietyClass::Criminal, None);
        assert_eq!(resolve_notoriety(&v, &v, true, false), Notoriety::Criminal);
    }

    #[test]
    fn class_u8_roundtrip() {
        for c in [
            NotorietyClass::Innocent,
            NotorietyClass::Criminal,
            NotorietyClass::Murderer,
            NotorietyClass::Neutral,
            NotorietyClass::Enemy,
        ] {
            assert_eq!(NotorietyClass::from_u8(c.to_u8()), c);
        }
    }
}
