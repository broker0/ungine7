//! Status bar info packet (0x11) and mobile status packets.
//!
//! Packet 0x11 is sent server → client with the player's (or another
//! mobile's) stats. The amount of data depends on the **status flag**:
//!
//! | Flag | Era           |
//! |------|---------------|
//! | 0x00 | No more data following (end of packet) |
//! | 0x01 | T2A           |
//! | 0x03 | UOR           |
//! | 0x04 | AOS (4.0+)    |
//! | 0x05 | UOML (5.0+)   |
//! | 0x06 | UOKR (KR+)    |

use u_io::{BE, BinaryWriter, Decode, Encode, FixedString, packet_reader};
use macros::{Decode as DecodeMacro, Encode as EncodeMacro, Packet, WireEnum};

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── Sub-structures for conditional field groups ───────────────────────────

/// T2A+ base stats (status flag >= 1).
///
/// Present whenever the status flag is non-zero **and** the packet
/// belongs to the local player (non-player mobiles only send
/// `is_female` and nothing further). All fields may be absent if the
/// server sends a truncated packet.
///
/// Note: the `is_female` byte that precedes this block on the wire is
/// stored separately in [`StatusBarInfo::is_female`].
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct BaseStats {
    pub strength: u16,
    pub dexterity: u16,
    pub intelligence: u16,
    pub stamina: u16,
    pub max_stamina: u16,
    pub mana: u16,
    pub max_mana: u16,
    pub gold: u32,
    /// Physical Resist (AOS) or old Armor Rating, depending on client.
    pub armor_rating: u16,
    pub weight: u16,
}

/// UOML+ extended fields (status flag >= 5).
///
/// Appears **before** the UOR block on the wire.
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct UomlStats {
    pub max_weight: u16,
    /// Race: 1 = Human, 2 = Elf, 3 = Gargoyle
    pub race: u8,
}

/// UOR+ extended fields (status flag >= 3).
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct UorStats {
    pub stats_cap: u16,
    pub followers: u8,
    pub max_followers: u8,
}

/// AOS+ extended fields (status flag >= 4).
///
/// Resistances can be negative — encoded as `0x10000 + value` on the wire
/// (i.e. two's complement u16).
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct AosStats {
    pub fire_resist: u16,
    pub cold_resist: u16,
    pub poison_resist: u16,
    pub energy_resist: u16,
    pub luck: u16,
    pub damage_min: u16,
    pub damage_max: u16,
    pub tithing_points: u32,
}

/// UOKR+ extended fields (status flag >= 6).
#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct UokrStats {
    pub hit_chance_increase: u16,
    pub swing_speed_increase: u16,
    pub damage_chance_increase: u16,
    pub lower_reagent_cost: u16,
    pub hp_regen: u16,
    pub stamina_regen: u16,
    pub mana_regen: u16,
    pub reflect_physical_damage: u16,
    pub enhance_potions: u16,
    pub defense_chance_increase: u16,
    pub spell_damage_increase: u16,
    pub faster_cast_recovery: u16,
    pub faster_casting: u16,
    pub lower_mana_cost: u16,
    pub strength_increase: u16,
    pub dexterity_increase: u16,
    pub intelligence_increase: u16,
    pub hit_points_increase: u16,
    pub stamina_increase: u16,
    pub mana_increase: u16,
    pub max_hit_points_increase: u16,
    pub max_stamina_increase: u16,
    pub max_mana_increase: u16,
}

// ── 0x11 StatusBarInfo (dynamic, S→C) ─────────────────────────────────────

/// Packet 0x11 — Status Bar Info (variable, S→C)
///
/// Contains the stat block for a mobile. The extent of the data depends
/// on the `status_flag` field:
///
/// - `0x00`: no data after the flag — all optional fields are `None`
/// - `>= 1` (T2A): `is_female` byte is present; [`BaseStats`]
///   (str/dex/int, stam/mana, gold, AR, weight) follows **only for the
///   local player** — for other mobiles the packet ends after `is_female`
/// - `>= 3` (UOR): adds [`UorStats`] (stats cap, followers)
/// - `>= 4` (AOS): adds [`AosStats`] (resistances, luck, damage, tithing)
/// - `>= 5` (UOML): adds [`UomlStats`] (max weight, race) — inserted
///   **before** [`UorStats`] on the wire
/// - `>= 6` (UOKR): adds [`UokrStats`] (combat/regen properties);
///   individual fields may be absent and default to `0`
///
/// All optional field groups are parsed defensively: if the remaining
/// bytes are insufficient the group is stored as `None` rather than
/// returning an error.
///
/// For non-player mobiles hit-point values are percentage-based (not real
/// values) to prevent injection tools from reading them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StatusBarInfo {
    // ── Header fields (always present) ─────────────────────────────────
    pub serial: u32,
    pub name: FixedString<30>,
    pub hit_points: u16,
    pub max_hit_points: u16,
    /// 1 = allowed to change name (e.g. pets), 0 = not allowed.
    pub name_change_flag: u8,
    pub status_flag: u8,

    // ── flag >= 1 ──────────────────────────────────────────────────────
    /// `true` = female, `false` = male.
    ///
    /// Present (1 byte on the wire) whenever `status_flag >= 1`.
    /// `None` when the packet contains no extended data at all.
    pub is_female: Option<bool>,

    /// Core player stats — `None` when the packet is truncated or when
    /// the target is not the local player (servers omit this block for
    /// other mobiles).
    pub stats: Option<BaseStats>,

    // ── Conditional groups ─────────────────────────────────────────────
    /// UOML+ (flag >= 5): max weight and race.
    pub uoml: Option<UomlStats>,
    /// UOR+ (flag >= 3): stats cap and followers.
    pub uor: Option<UorStats>,
    /// AOS+ (flag >= 4): resistances, luck, damage, tithing.
    pub aos: Option<AosStats>,
    /// UOKR+ (flag >= 6): extended combat/regen properties.
    ///
    /// Stored as a raw byte vector to preserve the exact wire length —
    /// OSI servers send a variable number of u16 fields (up to 23 × 2 = 46
    /// bytes) and not all fields may be present. Use [`uokr_parsed`] to
    /// obtain a fully decoded [`UokrStats`] with missing fields defaulting to 0.
    ///
    /// [`uokr_parsed`]: StatusBarInfo::uokr_parsed
    pub uokr: Option<Vec<u8>>,
}

impl StatusBarInfo {
    /// Decode the raw UOKR bytes into a [`UokrStats`], filling missing
    /// fields with `0`. Returns `None` if `uokr` is `None`.
    pub fn uokr_parsed(&self) -> Option<UokrStats> {
        let bytes = self.uokr.as_ref()?;
        let mut r = u_io::BinaryReader::<BE>::new(bytes);
        fn read_u16_or_zero(r: &mut u_io::BinaryReader<BE>) -> u16 {
            if r.remaining_len() >= 2 { Decode::decode(r).unwrap_or(0) } else { 0 }
        }
        Some(UokrStats {
            hit_chance_increase:       read_u16_or_zero(&mut r),
            swing_speed_increase:      read_u16_or_zero(&mut r),
            damage_chance_increase:    read_u16_or_zero(&mut r),
            lower_reagent_cost:        read_u16_or_zero(&mut r),
            hp_regen:                  read_u16_or_zero(&mut r),
            stamina_regen:             read_u16_or_zero(&mut r),
            mana_regen:                read_u16_or_zero(&mut r),
            reflect_physical_damage:   read_u16_or_zero(&mut r),
            enhance_potions:           read_u16_or_zero(&mut r),
            defense_chance_increase:   read_u16_or_zero(&mut r),
            spell_damage_increase:     read_u16_or_zero(&mut r),
            faster_cast_recovery:      read_u16_or_zero(&mut r),
            faster_casting:            read_u16_or_zero(&mut r),
            lower_mana_cost:           read_u16_or_zero(&mut r),
            strength_increase:         read_u16_or_zero(&mut r),
            dexterity_increase:        read_u16_or_zero(&mut r),
            intelligence_increase:     read_u16_or_zero(&mut r),
            hit_points_increase:       read_u16_or_zero(&mut r),
            stamina_increase:          read_u16_or_zero(&mut r),
            mana_increase:             read_u16_or_zero(&mut r),
            max_hit_points_increase:   read_u16_or_zero(&mut r),
            max_stamina_increase:      read_u16_or_zero(&mut r),
            max_mana_increase:         read_u16_or_zero(&mut r),
        })
    }
}

impl ManualPacket for StatusBarInfo {
    const ID: u8 = 0x11;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum valid packet: id(1) + len(2) + serial(4) + name(30)
        //   + hp(2) + max_hp(2) + name_change_flag(1) + status_flag(1) = 43 bytes
        let mut r = packet_reader(data, Self::ID, 43, true)?;

        // Always-present fields
        let serial: u32 = Decode::decode(&mut r)?;
        let name: FixedString<30> = Decode::decode(&mut r)?;
        let hit_points: u16 = Decode::decode(&mut r)?;
        let max_hit_points: u16 = Decode::decode(&mut r)?;
        let name_change_flag: u8 = Decode::decode(&mut r)?;
        let status_flag: u8 = Decode::decode(&mut r)?;

        // When status_flag == 0 no more data follows.
        if status_flag == 0 {
            return Ok(Self {
                serial,
                name,
                hit_points,
                max_hit_points,
                name_change_flag,
                status_flag,
                is_female: None,
                stats: None,
                uoml: None,
                uor: None,
                aos: None,
                uokr: None,
            });
        }

        // flag >= 1: is_female byte is always the first extended field.
        // For non-player mobiles the packet may end right here.
        let is_female: Option<bool> = if r.remaining_len() >= 1 {
            let raw: u8 = Decode::decode(&mut r)?;
            Some(raw != 0)
        } else {
            None
        };

        // BaseStats: str(2)+dex(2)+int(2)+stam(2)+max_stam(2)+mana(2)+max_mana(2)
        //            +gold(4)+ar(2)+weight(2) = 22 bytes.
        // Only present for the local player — other mobiles stop after is_female.
        const BASE_STATS_SIZE: usize = 22;
        let stats: Option<BaseStats> = if r.remaining_len() >= BASE_STATS_SIZE {
            Some(BaseStats::decode(&mut r)?)
        } else {
            None
        };

        // UOML+ (flag >= 5): max_weight(2) + race(1) = 3 bytes.
        // Appears on the wire *before* UOR.
        const UOML_SIZE: usize = 3;
        let uoml: Option<UomlStats> = if status_flag >= 5 && r.remaining_len() >= UOML_SIZE {
            Some(UomlStats::decode(&mut r)?)
        } else {
            None
        };

        // UOR+ (flag >= 3): stats_cap(2) + followers(1) + max_followers(1) = 4 bytes.
        const UOR_SIZE: usize = 4;
        let uor: Option<UorStats> = if status_flag >= 3 && r.remaining_len() >= UOR_SIZE {
            Some(UorStats::decode(&mut r)?)
        } else {
            None
        };

        // AOS+ (flag >= 4): fire(2)+cold(2)+poison(2)+energy(2)+luck(2)
        //                   +dmg_min(2)+dmg_max(2)+tithing(4) = 18 bytes.
        const AOS_SIZE: usize = 18;
        let aos: Option<AosStats> = if status_flag >= 4 && r.remaining_len() >= AOS_SIZE {
            Some(AosStats::decode(&mut r)?)
        } else {
            None
        };

        // UOKR+ (flag >= 6): read all remaining bytes verbatim to preserve
        // the exact wire length (servers send a variable number of u16 fields).
        let uokr: Option<Vec<u8>> = if status_flag >= 6 {
            Some(r.read_slice(r.remaining_len()).unwrap_or(&[]).to_vec())
        } else {
            None
        };

        Ok(Self {
            serial,
            name,
            hit_points,
            max_hit_points,
            name_change_flag,
            status_flag,
            is_female,
            stats,
            uoml,
            uor,
            aos,
            uokr,
        })
    }
}

impl Encode<BE> for StatusBarInfo {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        // Header
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder

        // Always-present fields
        w.put_u32(self.serial);
        self.name.encode(w);
        w.put_u16(self.hit_points);
        w.put_u16(self.max_hit_points);
        w.put_u8(self.name_change_flag);
        w.put_u8(self.status_flag);

        // flag >= 1: is_female byte
        if let Some(is_female) = self.is_female {
            w.put_u8(u8::from(is_female));
        }

        // BaseStats (player only)
        if let Some(ref stats) = self.stats {
            stats.encode(w);
        }

        // Conditional groups (wire order: uoml → uor → aos → uokr)
        if let Some(ref uoml) = self.uoml {
            uoml.encode(w);
        }
        if let Some(ref uor) = self.uor {
            uor.encode(w);
        }
        if let Some(ref aos) = self.aos {
            aos.encode(w);
        }
        if let Some(ref uokr) = self.uokr {
            w.put_slice(uokr);
        }
    }
}

// ── 0x2D MobAttributes (17 bytes, fixed, S→C) ─────────────────────────────

/// Packet 0x2D — Mob Attributes (17 bytes, fixed, S→C)
///
/// Sent by the server to provide a mobile's full attribute set in one
/// packet — hit points, mana, and stamina (both current and max).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x2D, size = fixed(17), endian = "be")]
pub struct MobAttributes {
    pub id: u8,
    pub serial: u32,
    pub hits_max: u16,
    pub hits_current: u16,
    pub mana_max: u16,
    pub mana_current: u16,
    pub stam_max: u16,
    pub stam_current: u16,
}

// ── 0xA1 UpdateHealth (9 bytes, fixed, S→C) ───────────────────────────────

/// Packet 0xA1 — Update Current Health (9 bytes, fixed, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xA1, size = fixed(9), endian = "be")]
pub struct UpdateHealth {
    pub id: u8,
    pub serial: u32,
    pub max_health: u16,
    pub current_health: u16,
}

// ── 0xA2 UpdateMana (9 bytes, fixed, S→C) ─────────────────────────────────

/// Packet 0xA2 — Update Current Mana (9 bytes, fixed, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xA2, size = fixed(9), endian = "be")]
pub struct UpdateMana {
    pub id: u8,
    pub serial: u32,
    pub max_mana: u16,
    pub current_mana: u16,
}

// ── 0xA3 UpdateStamina (9 bytes, fixed, S→C) ──────────────────────────────

/// Packet 0xA3 — Update Current Stamina (9 bytes, fixed, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xA3, size = fixed(9), endian = "be")]
pub struct UpdateStamina {
    pub id: u8,
    pub serial: u32,
    pub max_stamina: u16,
    pub current_stamina: u16,
}

// ── 0xDE UpdateMobileStatus (variable, S→C) ───────────────────────────────

/// Mobile status code carried in [`UpdateMobileStatus`].
///
/// | Value | Meaning        |
/// |-------|----------------|
/// | 0     | Normal         |
/// | 1     | Being attacked |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum MobileStatusCode {
    /// The mobile is in a normal, unaffected state.
    Normal = 0,
    /// The mobile is currently being attacked.
    BeingAttacked = 1,
}

/// Packet 0xDE — Update Mobile Status (variable, S→C)
///
/// Sent since the buff/debuff system was introduced.  Notifies the client
/// of a change in a mobile's combat status.  When `status` is
/// [`BeingAttacked`](MobileStatusCode::BeingAttacked) an additional
/// `attacker_serial` field follows.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0xDE
/// BYTE[2]  total packet length
/// BYTE[4]  player_serial
/// BYTE[1]  status           — 0 = Normal, 1 = Being Attacked
/// If status == 1:
///   BYTE[4]  attacker_serial
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UpdateMobileStatus {
    /// The mobile has returned to a normal state.
    Normal {
        /// Serial of the mobile whose status is being updated.
        player_serial: u32,
    },
    /// The mobile is being attacked.
    BeingAttacked {
        /// Serial of the mobile whose status is being updated.
        player_serial: u32,
        /// Serial of the attacker.
        attacker_serial: u32,
    },
}

impl UpdateMobileStatus {
    /// Return the serial of the mobile whose status is described.
    pub fn player_serial(&self) -> u32 {
        match self {
            Self::Normal { player_serial } => *player_serial,
            Self::BeingAttacked { player_serial, .. } => *player_serial,
        }
    }
}

impl ManualPacket for UpdateMobileStatus {
    const ID: u8 = 0xDE;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) + player_serial(4) + status(1) = 8 bytes
        let mut r = packet_reader(data, Self::ID, 8, true)?;

        let player_serial: u32 = Decode::decode(&mut r)?;
        let status: u8 = Decode::decode(&mut r)?;

        match status {
            0 => Ok(Self::Normal { player_serial }),
            1 => {
                let attacker_serial: u32 = Decode::decode(&mut r)?;
                Ok(Self::BeingAttacked { player_serial, attacker_serial })
            }
            _ => Err(u_io::DecodeError::Other(
                format!("unknown UpdateMobileStatus code: 0x{status:02X}"),
            )
            .into()),
        }
    }
}

impl Encode<BE> for UpdateMobileStatus {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()

        match self {
            Self::Normal { player_serial } => {
                w.put_u32(*player_serial);
                w.put_u8(0x00);
            }
            Self::BeingAttacked { player_serial, attacker_serial } => {
                w.put_u32(*player_serial);
                w.put_u8(0x01);
                w.put_u32(*attacker_serial);
            }
        }
    }
}

// ── 0x16 NewHealthBarStatus (dynamic, S→C) ────────────────────────────────

/// Health bar color code in a [`NewHealthBarEntry`].
///
/// | Wire value | Meaning                      |
/// |------------|------------------------------|
/// | 1          | Green (poison / health bar)  |
/// | 2          | Yellow (damaged)             |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u16)]
pub enum HealthBarColor {
    /// Green health bar — used for poisoning.
    #[wire_enum(1, "green")]
    Green,
    /// Yellow health bar — used for damage indication.
    #[wire_enum(2, "yellow")]
    Yellow,
    #[wire_enum(unknown)]
    Unknown(u16),
}

/// A single health bar color entry in [`NewHealthBarStatus`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NewHealthBarEntry {
    /// Which health bar color this entry controls.
    pub color: HealthBarColor,
    /// `0` = remove / disable this color, `1` = enable.
    ///
    /// For poison (green): `0` = remove poison, non-zero = poison level.
    pub flag: u8,
}

/// Packet 0x16 — New Health Bar Status Update / SA (dynamic, S→C)
///
/// Shared handler for packets `0x16` (SA+) and `0x17` (KR era).
/// The `0x17` variant always has exactly one entry (`count = 1`).
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x16 or 0x17
/// BYTE[2]  total packet length
/// BYTE[4]  mobile serial
/// BYTE[2]  count              — number of entries that follow
/// For each entry:
///   BYTE[2]  type/color       — 1 = green (poison), 2 = yellow, 3 = unknown
///   BYTE[1]  flag             — 0 = remove / disable, 1 = enable
/// ```
///
/// Note: ClassicUO uses this same parser for both `0x16` and `0x17`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NewHealthBarStatus {
    /// Serial of the mobile whose health bar is being updated.
    pub serial: u32,
    /// One entry per health bar color being set.
    pub entries: Vec<NewHealthBarEntry>,
}

impl NewHealthBarStatus {
    /// Create a status update with a single entry.
    pub fn single(serial: u32, color: HealthBarColor, flag: u8) -> Self {
        Self {
            serial,
            entries: vec![NewHealthBarEntry { color, flag }],
        }
    }
}

impl ManualPacket for NewHealthBarStatus {
    const ID: u8 = 0x16;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) + serial(4) + count(2) = 9 bytes
        let mut r = packet_reader(data, Self::ID, 9, true)?;

        let serial: u32 = Decode::decode(&mut r)?;
        let count: u16 = Decode::decode(&mut r)?;

        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let color_raw: u16 = Decode::decode(&mut r)?;
            let flag: u8 = Decode::decode(&mut r)?;
            entries.push(NewHealthBarEntry {
                color: HealthBarColor::from_wire(color_raw),
                flag,
            });
        }

        Ok(Self { serial, entries })
    }
}

impl Encode<BE> for NewHealthBarStatus {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()
        w.put_u32(self.serial);
        w.put_u16(self.entries.len() as u16); // count
        for entry in &self.entries {
            w.put_u16(entry.color.to_wire());
            w.put_u8(entry.flag);
        }
    }
}

// ── 0x17 OldHealthBarStatus (12 bytes, fixed, S→C) ───────────────────────

/// Packet 0x17 — Health Bar Status Update / KR (12 bytes, fixed, S→C)
///
/// Pre-SA version of the health bar color update, sent by the KR (Kingdom
/// Reborn) client era server. Contains exactly one status entry (`count = 1`).
///
/// Parsed identically to [`NewHealthBarStatus`] (`0x16`) — same `count + entries`
/// structure, just always fixed at 12 bytes with a single entry.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]  0x17
/// BYTE[2]  length         (always 12)
/// BYTE[4]  mobile_serial
/// BYTE[2]  count          (always 0x0001)
/// BYTE[2]  color          — 1 = green (poison), 2 = yellow, >2 = unknown
/// BYTE[1]  flag           — 0 = remove / disable, 1 = enable
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x17, size = fixed(12), endian = "be")]
pub struct OldHealthBarStatus {
    pub id: u8,
    pub len: u16,
    pub serial: u32,
    /// Always `1` — number of entries in this packet (KR always sends exactly one).
    #[binary(const_value = 0x0001u16)]
    pub count: u16,
    pub color: HealthBarColor,
    pub flag: u8,
}

impl OldHealthBarStatus {
    pub fn new(serial: u32, color: HealthBarColor, flag: u8) -> Self {
        Self {
            id: 0x17,
            len: 12,
            serial,
            count: 0x0001,
            color,
            flag,
        }
    }
}
