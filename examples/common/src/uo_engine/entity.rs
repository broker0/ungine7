//! Concrete [`DemoEntity`] implementation for the shadow continuum.
//!
//! Provides a three-variant enum (`Item`, `Multi`, `Mobile`) that implements
//! the [`framework::ecumene::Entity`] trait and can be stored in a
//! [`super::store::DemoStore`].

use framework::ecumene::{StaticDataProvider, Entity, TileShape};
use framework::continuum::EntitySnapshot;
use bytes::Bytes;
use u_core::Pos3D;
use packets::mobile_flags::MobileFlags;
use packets::movement::Notoriety;
use packets::world::{EquippedItem, ObjectInfo, ObjectInfoFlags, ObjectInfoSA};
use packets::traits::{ManualPacket, BasicPacket};

// ── Skills ──────────────────────────────────────────────────────────────

/// Lock state of a skill, mirroring the UO 0x3A protocol values but defined
/// locally so the entity layer does not depend on the `packets` crate.
///
/// Maps to `packets::skills::SkillLock` at send time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SkillLock {
    /// Skill may rise (default).
    Up,
    /// Skill may fall.
    Down,
    /// Skill is locked (no change).
    Locked,
}

impl Default for SkillLock {
    fn default() -> Self {
        SkillLock::Up
    }
}

/// A single skill's state on a mobile.
///
/// `value` and `cap` are in **tenths** (e.g. `500` = 50.0, `1000` = 100.0),
/// matching the UO 0x3A wire format.
///
/// In this demo there is no skill gain, so `value` is fixed at spawn; `cap`
/// is the per-skill maximum and `lock` is set by the client via 0x3A.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillValue {
    /// Current skill value, in tenths.
    pub value: u16,
    /// Per-skill cap, in tenths.
    pub cap: u16,
    /// Lock state (client-controlled).
    #[serde(default)]
    pub lock: SkillLock,
}

impl SkillValue {
    /// Create a skill value with the default cap (100.0) and `Up` lock.
    pub fn new(value: u16) -> Self {
        Self { value, cap: 1000, lock: SkillLock::Up }
    }

    /// Create a skill value with an explicit cap and `Up` lock.
    pub fn with_cap(value: u16, cap: u16) -> Self {
        Self { value, cap, lock: SkillLock::Up }
    }
}

fn notoriety_to_u8(n: Notoriety) -> u8 {
    match n {
        Notoriety::Invalid => 0,
        Notoriety::Innocent => 1,
        Notoriety::Ally => 2,
        Notoriety::Attackable => 3,
        Notoriety::Criminal => 4,
        Notoriety::Enemy => 5,
        Notoriety::Murderer => 6,
        Notoriety::Translucent => 7,
        Notoriety::Unknown(v) => v,
    }
}

// ── MobileData ──────────────────────────────────────────────────────────

/// All fields for a mobile entity, extracted from the `DemoEntity::Mobile`
/// variant to reduce destructuring boilerplate.
///
/// With this struct, `entity.mobile()` returns `Option<&MobileData>` and
/// callers access fields directly (e.g. `m.x`, `m.hits`, `m.items`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MobileData {
    pub serial: u32,
    pub graphic: u16,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub direction: u8,
    pub color: u16,
    pub status: MobileFlags,
    pub notoriety: Notoriety,
    pub items: Vec<EquippedItem>,
    /// Name extracted from StatusBarInfo (0x11) or ClilocMessage (0xC1).
    pub name: String,
    /// Current / max hit points (percentage-based for non-player mobs).
    pub hits: u16,
    pub hits_max: u16,
    /// Current / max mana.
    pub mana: u16,
    pub mana_max: u16,
    /// Current / max stamina.
    pub stamina: u16,
    pub stamina_max: u16,
    /// Base stats.
    pub str_: u16,
    pub dex: u16,
    pub int: u16,
    /// `true` if this mobile is a player character (not an NPC).
    ///
    /// Players are not removed from the world when killed — instead they
    /// become a ghost (see [`MobileData::dead`]).  NPCs are removed and replaced by a
    /// corpse item.
    #[serde(default)]
    pub is_player: bool,
    /// `true` if this mobile is currently dead (a ghost).
    ///
    /// Only meaningful for players.  A dead player keeps existing in the
    /// world with the ghost body graphic and cannot act until resurrected.
    #[serde(default)]
    pub dead: bool,
    /// The living body graphic, saved when the player dies so it can be
    /// restored on resurrection.  `0` when alive.
    #[serde(default)]
    pub living_graphic: u16,

    // ── Reputation / notoriety (classic T2A) ─────────────────────────────
    /// Intrinsic notoriety class (innocent / criminal / murderer / neutral).
    ///
    /// For NPCs this is set at spawn (usually `Neutral`); for players it is
    /// derived from murder counts and the criminal flag (see
    /// [`MobileData::effective_notoriety_class`]).
    #[serde(default)]
    pub noto_class: crate::uo_engine::notoriety::NotorietyClass,
    /// Guild id; two mobiles with the same id are rendered as allies (green).
    #[serde(default)]
    pub guild_id: Option<u32>,
    /// Long-term murder count.  At/above
    /// [`MURDERER_THRESHOLD`](crate::uo_engine::notoriety::MURDERER_THRESHOLD)
    /// the mobile is a Murderer (red).
    #[serde(default)]
    pub murders: u16,
    /// Karma (-10000..=10000).  Cosmetic in this demo (titles not shown).
    #[serde(default)]
    pub karma: i32,
    /// Fame (0..=10000).  Cosmetic in this demo.
    #[serde(default)]
    pub fame: i32,
    /// Absolute expiry (Unix epoch ms) of the criminal flag.  `0` = not a
    /// criminal.  Stored as epoch ms so it survives snapshot save/load.
    #[serde(default)]
    pub criminal_until_ms: u64,
    /// Active aggressor relationships: `(other_serial, expiry_epoch_ms)`.
    ///
    /// An entry `(v, t)` means *this* mobile aggressed `v` (or vice versa);
    /// during the window `v` may attack this mobile without becoming a
    /// criminal, and this mobile is shown gray to `v`.
    #[serde(default)]
    pub aggressors: Vec<(u32, u64)>,

    // ── Poison ────────────────────────────────────────────────────────────
    /// Poison level: `0` = not poisoned, `1..=4` = Lesser..Deadly.
    #[serde(default)]
    pub poison_level: u8,
    /// Absolute expiry (Unix epoch ms) of the poison.  `0` = not poisoned.
    #[serde(default)]
    pub poison_until_ms: u64,
    /// Absolute time (Unix epoch ms) of the next poison damage tick.
    #[serde(default)]
    pub poison_next_tick_ms: u64,
    /// Damage applied per poison tick (resolved at application time).
    #[serde(default)]
    pub poison_damage_per_tick: u16,
    /// Interval (ms) between poison ticks (resolved at application time).
    #[serde(default)]
    pub poison_tick_interval_ms: u64,
    /// Who applied the poison (for aggression / kill attribution).  `0` if
    /// ambient or unknown.
    #[serde(default)]
    pub poison_source: u32,

    // ── Ship binding ──────────────────────────────────────────────────────
    /// Serial of the ship (multi) this mobile is currently standing on, if
    /// any.  `None` when on dry land / not aboard.
    ///
    /// Set when the mobile steps onto a ship deck and cleared when it steps
    /// off.  Used so the sailing tick carries exactly the right passengers
    /// (rather than guessing from the footprint bbox) and so a step taken
    /// while the ship is mid-move can be validated relative to the deck.
    #[serde(default)]
    pub ship_serial: Option<u32>,

    // ── Skills ────────────────────────────────────────────────────────────
    /// Skill values keyed by UO skill id.  Values/caps are in tenths.
    ///
    /// In this demo there is no skill gain; values are seeded at character
    /// creation (see `common::spawn::new_player_entity`) and sent to the
    /// client via packet 0x3A.  `#[serde(default)]` keeps older snapshots /
    /// `.uolog` entities (which lack this field) loadable — they simply have
    /// no skills.
    #[serde(default)]
    pub skills: std::collections::BTreeMap<u16, SkillValue>,
}

impl MobileData {
    /// Current wall-clock time as Unix epoch milliseconds.
    pub fn now_epoch_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Resolve the *effective* notoriety class right now, taking the murder
    /// count and the (possibly expired) criminal flag into account.
    pub fn effective_notoriety_class(&self) -> crate::uo_engine::notoriety::NotorietyClass {
        use crate::uo_engine::notoriety::{NotorietyClass, MURDERER_THRESHOLD};

        // Non-players keep their spawn-assigned class verbatim.
        if !self.is_player {
            return self.noto_class;
        }
        if self.murders >= MURDERER_THRESHOLD {
            return NotorietyClass::Murderer;
        }
        if self.criminal_until_ms > Self::now_epoch_ms() {
            return NotorietyClass::Criminal;
        }
        NotorietyClass::Innocent
    }

    /// Build the per-viewer [`NotorietyView`](crate::uo_engine::notoriety::NotorietyView)
    /// for this mobile.
    pub fn notoriety_view(&self) -> crate::uo_engine::notoriety::NotorietyView {
        crate::uo_engine::notoriety::NotorietyView {
            class: self.effective_notoriety_class(),
            guild_id: self.guild_id,
            is_player: self.is_player,
        }
    }

    /// Whether `viewer_serial` is currently in this mobile's aggressor list
    /// (relationship not yet expired).
    pub fn is_aggressor_to(&self, viewer_serial: u32) -> bool {
        let now = Self::now_epoch_ms();
        self.aggressors
            .iter()
            .any(|(s, until)| *s == viewer_serial && *until > now)
    }

    /// Whether this mobile is currently poisoned (poison not yet expired).
    pub fn is_poisoned(&self) -> bool {
        self.poison_level > 0 && self.poison_until_ms > Self::now_epoch_ms()
    }
}

impl Default for MobileData {
    fn default() -> Self {
        Self {
            serial: 0,
            graphic: 0,
            x: 0,
            y: 0,
            z: 0,
            direction: 0,
            color: 0,
            status: MobileFlags(0),
            notoriety: Notoriety::Innocent,
            items: Vec::new(),
            name: String::new(),
            hits: 0,
            hits_max: 0,
            mana: 0,
            mana_max: 0,
            stamina: 0,
            stamina_max: 0,
            str_: 0,
            dex: 0,
            int: 0,
            is_player: false,
            dead: false,
            living_graphic: 0,
            noto_class: crate::uo_engine::notoriety::NotorietyClass::default(),
            guild_id: None,
            murders: 0,
            karma: 0,
            fame: 0,
            criminal_until_ms: 0,
            aggressors: Vec::new(),
            poison_level: 0,
            poison_until_ms: 0,
            poison_next_tick_ms: 0,
            poison_damage_per_tick: 0,
            poison_tick_interval_ms: 0,
            poison_source: 0,
            ship_serial: None,
            skills: std::collections::BTreeMap::new(),
        }
    }
}

// ── DemoEntity ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum DemoEntity {
    Item {
        serial: u32,
        graphic: u16,
        color: u16,
        amount: u16,
        x: u16,
        y: u16,
        z: i8,
        /// `true` if a S->C 0x24 `DrawContainer` was seen for this serial.
        is_container: bool,
        /// When `true`, only GM+ observers see this item.  The UO client
        /// receives `ObjectInfoFlags(0x80)` which renders it semi-transparent
        /// for staff and invisible for regular players.
        #[serde(default)]
        hidden: bool,
        /// Optional facing direction for the item.
        ///
        /// Used by corpse items (graphic `0x2006`) to control which
        /// direction the corpse sprite faces.  `None` for regular items.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        facing: Option<u8>,
    },
    Multi {
        serial: u32,
        graphic: u16,
        x: u16,
        y: u16,
        z: i8,
        /// Serial of the owning player (0 = unowned, e.g. world/static multis).
        #[serde(default)]
        owner: u32,
        /// Serials of the house's door items (spawned alongside the multi).
        #[serde(default)]
        door_serials: Vec<u32>,
        /// Serial of the house sign item (0 = none).
        #[serde(default)]
        sign_serial: u32,
    },
    Mobile(MobileData),
}

// ── Accessor view types ─────────────────────────────────────────────────
//
// `ItemRef` and `MultiRef` remain lightweight borrow-views for the inline
// `Item` and `Multi` variants.  For `Mobile`, callers now use
// `&MobileData` / `&mut MobileData` directly — no more `MobileRef` /
// `MobileMut` wrappers.

/// Borrowed view into a [`DemoEntity::Item`] variant.
pub struct ItemRef<'a> {
    pub serial: u32,
    pub graphic: u16,
    pub color: u16,
    pub amount: u16,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub is_container: bool,
    pub hidden: bool,
    pub facing: Option<u8>,
    _phantom: std::marker::PhantomData<&'a ()>,
}

/// Borrowed view into a [`DemoEntity::Multi`] variant.
pub struct MultiRef<'a> {
    pub serial: u32,
    pub graphic: u16,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub owner: u32,
    pub sign_serial: u32,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl DemoEntity {
    // ── Type-safe accessors ─────────────────────────────────────────────

    /// Returns a reference to [`MobileData`] if this is a `Mobile` variant.
    pub fn mobile(&self) -> Option<&MobileData> {
        if let Self::Mobile(m) = self { Some(m) } else { None }
    }

    /// Returns a mutable reference to [`MobileData`] if this is a `Mobile` variant.
    pub fn mobile_mut(&mut self) -> Option<&mut MobileData> {
        if let Self::Mobile(m) = self { Some(m) } else { None }
    }

    /// Returns a borrowed view if this is an `Item` variant.
    pub fn item(&self) -> Option<ItemRef<'_>> {
        if let Self::Item {
            serial, graphic, color, amount, x, y, z,
            is_container, hidden, facing,
        } = self {
            Some(ItemRef {
                serial: *serial, graphic: *graphic, color: *color,
                amount: *amount, x: *x, y: *y, z: *z,
                is_container: *is_container, hidden: *hidden,
                facing: *facing,
                _phantom: std::marker::PhantomData,
            })
        } else { None }
    }

    /// Returns a borrowed view if this is a `Multi` variant.
    pub fn multi(&self) -> Option<MultiRef<'_>> {
        if let Self::Multi { serial, graphic, x, y, z, owner, sign_serial, .. } = self {
            Some(MultiRef {
                serial: *serial, graphic: *graphic,
                x: *x, y: *y, z: *z,
                owner: *owner, sign_serial: *sign_serial,
                _phantom: std::marker::PhantomData,
            })
        } else { None }
    }

    /// Returns the owner serial if this is an owned `Multi` (house), else `None`.
    pub fn house_owner(&self) -> Option<u32> {
        match self {
            Self::Multi { owner, .. } if *owner != 0 => Some(*owner),
            _ => None,
        }
    }

    /// Returns `true` if this is a `Multi` owned by `serial`.
    pub fn is_house_owner(&self, serial: u32) -> bool {
        matches!(self, Self::Multi { owner, .. } if *owner != 0 && *owner == serial)
    }

    /// Returns `(x, y, z)` regardless of variant.
    pub fn xyz(&self) -> (u16, u16, i8) {
        match self {
            Self::Item { x, y, z, .. } => (*x, *y, *z),
            Self::Multi { x, y, z, .. } => (*x, *y, *z),
            Self::Mobile(m) => (m.x, m.y, m.z),
        }
    }

    /// Returns the equipment list if this is a `Mobile`, or an empty slice.
    pub fn equipment(&self) -> &[EquippedItem] {
        if let Self::Mobile(m) = self { &m.items } else { &[] }
    }

    /// Returns a mutable reference to the equipment list if this is a `Mobile`.
    pub fn equipment_mut(&mut self) -> Option<&mut Vec<EquippedItem>> {
        if let Self::Mobile(m) = self { Some(&mut m.items) } else { None }
    }

    pub fn to_raw_bytes(&self) -> Bytes {
        match self {
            Self::Item {
                serial, graphic, color, amount,
                x, y, z, hidden, facing, ..
            } => {
                let flags_byte = if *hidden { 0x80 } else { 0 };
                if *graphic >= 0x4000 {
                    // Items with graphic >= 0x4000 (SA+ / High Seas range)
                    // cannot be sent via 0x1A ObjectInfo — the client would
                    // misinterpret them as multi-objects (graphic >= 0x4000 is
                    // the multi detection threshold in 0x1A).  Use 0xF3
                    // ObjectInfoSA which has an explicit data_type field.
                    ObjectInfoSA::item(
                        *serial, *graphic, 0,
                        (*amount).max(1),
                        *x, *y, *z,
                        flags_byte, *color, 0, 0,
                    ).to_bytes()
                } else {
                    // Corpse items (graphic 0x2006) always need the amount
                    // field (carries the body graphic) and facing.
                    let is_corpse = *graphic == 0x2006;
                    ObjectInfo {
                        object_id: *serial,
                        graphic: *graphic,
                        amount: if is_corpse || *amount > 1 { Some(*amount) } else { None },
                        graphic_increment: None,
                        x: *x,
                        y: *y,
                        facing: *facing,
                        z: *z,
                        dye: if *color != 0 { Some(*color) } else { None },
                        flags: Some(ObjectInfoFlags(flags_byte)),
                    }.to_bytes()
                }
            }
            Self::Multi {
                serial, graphic, x, y, z, ..
            } => {
                // Multi uses ObjectInfo (0x1A) with graphic + 0x4000 offset.
                ObjectInfo {
                    object_id: *serial,
                    graphic: *graphic + 0x4000,
                    amount: None,
                    graphic_increment: None,
                    x: *x,
                    y: *y,
                    facing: None,
                    z: *z,
                    dye: None,
                    flags: None,
                }.to_bytes()
            }
            Self::Mobile(m) => {
                let draw_mobile = packets::world::DrawMobile {
                    serial: m.serial,
                    graphic: m.graphic,
                    x: m.x,
                    y: m.y,
                    z: m.z,
                    direction: m.direction,
                    color: m.color,
                    status: m.status,
                    notoriety: m.notoriety,
                    items: m.items.clone(),
                };
                packets::traits::ManualPacket::to_bytes(&draw_mobile)
            }
        }
    }
}

impl Entity for DemoEntity {
    fn serial(&self) -> u32 {
        match self {
            Self::Item { serial, .. } => *serial,
            Self::Multi { serial, .. } => *serial,
            Self::Mobile(m) => m.serial,
        }
    }

    fn pos(&self) -> Pos3D {
        match self {
            Self::Item { x, y, z, .. } => Pos3D::new(*x, *y, *z),
            Self::Multi { x, y, z, .. } => Pos3D::new(*x, *y, *z),
            Self::Mobile(m) => Pos3D::new(m.x, m.y, m.z),
        }
    }

    fn graphic(&self) -> u16 {
        match self {
            Self::Item { graphic, .. } => *graphic,
            Self::Multi { graphic, .. } => *graphic,
            Self::Mobile(m) => m.graphic,
        }
    }

    fn is_mobile(&self) -> bool {
        matches!(self, Self::Mobile(_))
    }

    fn is_multi(&self) -> bool {
        matches!(self, Self::Multi { .. })
    }

    fn is_container(&self) -> bool {
        matches!(self, Self::Item { is_container: true, .. })
    }

    fn set_pos(&mut self, pos: Pos3D) {
        match self {
            Self::Item { x, y, z, .. }
            | Self::Multi { x, y, z, .. } => {
                *x = pos.x;
                *y = pos.y;
                *z = pos.z;
            }
            Self::Mobile(m) => {
                m.x = pos.x;
                m.y = pos.y;
                m.z = pos.z;
            }
        }
    }

    fn set_direction(&mut self, dir: u8) {
        if let Self::Mobile(m) = self {
            m.direction = dir;
        }
    }

    fn snapshot(&self) -> Option<EntitySnapshot> {
        let (graphic, hue, status_flags, notoriety) = match self {
            Self::Mobile(m) => (m.graphic, m.color, m.status.0, notoriety_to_u8(m.notoriety)),
            Self::Item { graphic, color, hidden, .. } => {
                (*graphic, *color, if *hidden { 0x80 } else { 0 }, 0)
            }
            Self::Multi { graphic, .. } => (*graphic, 0, 0, 0),
        };
        let notoriety_ctx = match self {
            Self::Mobile(m) => Some(framework::continuum::NotorietyContext {
                class: m.effective_notoriety_class().to_u8(),
                guild_id: m.guild_id,
                is_player: m.is_player,
                aggressors: m.aggressors.clone(),
            }),
            _ => None,
        };
        Some(EntitySnapshot {
            graphic,
            hue,
            status_flags,
            notoriety,
            raw: self.to_raw_bytes(),
            notoriety_ctx,
        })
    }

    fn extract_shapes(&self, static_data: &(impl StaticDataProvider + ?Sized)) -> Vec<(u16, u16, TileShape)> {
        if self.is_mobile() {
            return vec![];
        }

        let mut result = Vec::new();
        let pos = self.pos();

        if let Self::Multi { graphic, .. } = self {
            let parts = static_data.multi_parts(*graphic);
            for part in parts {
                if part.flags == 0 {
                    continue;
                }

                if let Some(def) = static_data.static_tile_def(part.tile_id) {
                    let px = (pos.x as i32 + part.x as i32).clamp(0, u16::MAX as i32) as u16;
                    let py = (pos.y as i32 + part.y as i32).clamp(0, u16::MAX as i32) as u16;
                    let pz = pos.z.saturating_add(part.z.clamp(i8::MIN as i16, i8::MAX as i16) as i8);

                    let shape = TileShape::from_static(pz, def);
                    result.push((px, py, shape));
                }
            }
        } else if let Self::Item { graphic, hidden, .. } = self {
            // Hidden items (GM-only markers, e.g. spawner objects) are
            // invisible to regular players and must not block movement for
            // anyone — server-side collision is observer-agnostic, so a
            // hidden item that blocked would block players who can't even
            // see it. Emit no collision shape for hidden items.
            if !*hidden {
                if let Some(def) = static_data.static_tile_def(*graphic) {
                    let shape = TileShape::from_static(pos.z, def);
                    result.push((pos.x, pos.y, shape));
                }
            }
        }

        result
    }

    fn hits(&self) -> Option<(u16, u16)> {
        let m = self.mobile()?;
        Some((m.hits, m.hits_max))
    }

    fn apply_damage(&mut self, amount: u16) -> u16 {
        if let Self::Mobile(m) = self {
            m.hits = m.hits.saturating_sub(amount);
            m.hits
        } else {
            0
        }
    }

    fn apply_heal(&mut self, amount: u16) -> u16 {
        if let Self::Mobile(m) = self {
            // A dead player (ghost) cannot be healed by normal means — only
            // resurrection restores HP.
            if m.dead {
                return m.hits;
            }
            m.hits = (m.hits + amount).min(m.hits_max);
            m.hits
        } else {
            0
        }
    }

    fn modify_mana(&mut self, delta: i32) -> u16 {
        if let Self::Mobile(m) = self {
            let new = (m.mana as i32 + delta).clamp(0, m.mana_max as i32) as u16;
            m.mana = new;
            new
        } else {
            0
        }
    }

    fn mana(&self) -> Option<(u16, u16)> {
        let m = self.mobile()?;
        Some((m.mana, m.mana_max))
    }

    fn modify_stamina(&mut self, delta: i32) -> u16 {
        if let Self::Mobile(m) = self {
            let new = (m.stamina as i32 + delta).clamp(0, m.stamina_max as i32) as u16;
            m.stamina = new;
            new
        } else {
            0
        }
    }

    fn stamina(&self) -> Option<(u16, u16)> {
        let m = self.mobile()?;
        Some((m.stamina, m.stamina_max))
    }

    fn modify_str(&mut self, delta: i32) -> u16 {
        if let Self::Mobile(m) = self {
            let new = (m.str_ as i32 + delta).clamp(1, u16::MAX as i32) as u16;
            m.str_ = new;
            // Strict invariant: max hit points always equal STR (classic UO).
            // Trim current hits if the cap dropped below it.
            m.hits_max = new;
            if m.hits > m.hits_max {
                m.hits = m.hits_max;
            }
            new
        } else {
            0
        }
    }

    fn str_val(&self) -> Option<u16> {
        let m = self.mobile()?;
        Some(m.str_)
    }

    fn modify_dex(&mut self, delta: i32) -> u16 {
        if let Self::Mobile(m) = self {
            let new = (m.dex as i32 + delta).clamp(1, u16::MAX as i32) as u16;
            m.dex = new;
            // Strict invariant: max stamina always equals DEX (classic UO).
            m.stamina_max = new;
            if m.stamina > m.stamina_max {
                m.stamina = m.stamina_max;
            }
            new
        } else {
            0
        }
    }

    fn dex_val(&self) -> Option<u16> {
        let m = self.mobile()?;
        Some(m.dex)
    }

    fn modify_int(&mut self, delta: i32) -> u16 {
        if let Self::Mobile(m) = self {
            let new = (m.int as i32 + delta).clamp(1, u16::MAX as i32) as u16;
            m.int = new;
            // Strict invariant: max mana always equals INT (classic UO).
            m.mana_max = new;
            if m.mana > m.mana_max {
                m.mana = m.mana_max;
            }
            new
        } else {
            0
        }
    }

    fn int_val(&self) -> Option<u16> {
        let m = self.mobile()?;
        Some(m.int)
    }

    fn notoriety(&self) -> Option<u8> {
        let m = self.mobile()?;
        Some(notoriety_to_u8(m.notoriety))
    }

    fn name(&self) -> Option<String> {
        let m = self.mobile()?;
        Some(m.name.clone())
    }

    fn direction(&self) -> Option<u8> {
        let m = self.mobile()?;
        Some(m.direction)
    }

    fn backpack_serial(&self) -> Option<u32> {
        let m = self.mobile()?;
        m.items.iter()
            .find(|eq| eq.layer == packets::layer::Layer::Backpack)
            .map(|eq| eq.serial)
    }

    fn equipment_serials(&self) -> Vec<u32> {
        if let Self::Mobile(m) = self {
            m.items.iter().map(|eq| eq.serial).collect()
        } else {
            Vec::new()
        }
    }

    fn is_mounted(&self) -> bool {
        if let Self::Mobile(m) = self {
            m.items.iter().any(|eq| eq.layer == packets::layer::Layer::Mount)
        } else {
            false
        }
    }

    fn is_player(&self) -> bool {
        matches!(self, Self::Mobile(m) if m.is_player)
    }
}
