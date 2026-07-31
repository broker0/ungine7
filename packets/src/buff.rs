//! Buff / Debuff system packet (0xDF).
//!
//! Sent by the server to add or remove status icons (buffs/debuffs) from the
//! player's buff bar.
//!
//! # Wire format variants
//!
//! The packet has two distinct wire formats differentiated by client version:
//!
//! ## Pre-5.0.2b format
//!
//! Carries the full buff metadata including cliloc IDs, duration, and an
//! optional argument string.
//!
//! **Add / Show** (`action == 0x0001`):
//! ```text
//! BYTE[1]  0xDF
//! BYTE[2]  total length
//! BYTE[4]  serial
//! BYTE[2]  icon            — buff/debuff icon number
//! BYTE[2]  0x0001          — show
//! BYTE[4]  0x00000000
//! BYTE[2]  icon            — repeated
//! BYTE[2]  0x0001          — repeated
//! BYTE[4]  0x00000000
//! BYTE[2]  duration_secs   — countdown in seconds (0 = no timer)
//! BYTE[2]  0x0000
//! BYTE[1]  0x00
//! BYTE[4]  cliloc_id1      — title cliloc
//! BYTE[4]  cliloc_id2      — body cliloc
//! BYTE[4]  0x00000000
//! BYTE[2]  0x0001
//! BYTE[?]  args            — little-endian UTF-16, leading " ", entries sep by " "
//! BYTE[2]  0x0000          — null terminator
//! ```
//!
//! **Remove** (`action == 0x0000`):
//! ```text
//! BYTE[1]  0xDF
//! BYTE[2]  total length
//! BYTE[4]  serial
//! BYTE[2]  icon
//! BYTE[2]  0x0000          — remove; packet ends here
//! ```
//!
//! ## Post-5.0.2b format
//!
//! A shorter packet used by newer clients — just serial + icon, no metadata.
//! Detected by the total packet length being too short for the pre-5.0.2b
//! add layout.
//!
//! ```text
//! BYTE[1]  0xDF
//! BYTE[2]  total length
//! BYTE[4]  serial
//! BYTE[2]  icon
//! ```

use u_io::{BE, BinaryWriter, Decode, Encode, packet_reader, encode_le_utf16_str, decode_le_utf16_str};
use macros::WireEnum;

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── BuffIcon ───────────────────────────────────────────────────────────────

/// Known buff/debuff icon numbers.
///
/// Icons not listed here are captured as [`Unknown(u16)`](Self::Unknown).
///
/// Cliloc numbers in parentheses are `(title_cliloc, body_cliloc)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u16)]
pub enum BuffIcon {
    /// 1001 — Dismount (1075635, 1075636)
    #[wire_enum(1001, "Dismount")]
    Dismount,
    /// 1002 — Disarm (1075637, 1075638)
    #[wire_enum(1002, "Disarm")]
    Disarm,
    /// 1005 — Night Sight (1075643, 1075644)
    #[wire_enum(1005, "NightSight")]
    NightSight,
    /// 1006 — Death Strike
    #[wire_enum(1006, "DeathStrike")]
    DeathStrike,
    /// 1007 — Evil Omen
    #[wire_enum(1007, "EvilOmen")]
    EvilOmen,
    /// 1008 — Unknown (GumpID 0x7556)
    #[wire_enum(1008, "Unknown1008")]
    Unknown1008,
    /// 1009 — Regeneration (1044106, 1075106)
    #[wire_enum(1009, "Regeneration")]
    Regeneration,
    /// 1010 — Divine Fury
    #[wire_enum(1010, "DivineFury")]
    DivineFury,
    /// 1011 — Enemy of One
    #[wire_enum(1011, "EnemyOfOne")]
    EnemyOfOne,
    /// 1012 — Stealth (1044107, 1075655)
    #[wire_enum(1012, "Stealth")]
    Stealth,
    /// 1013 — Active Meditation (1044106, 1075106)
    #[wire_enum(1013, "ActiveMeditation")]
    ActiveMeditation,
    /// 1014 — Blood Oath (caster)
    #[wire_enum(1014, "BloodOathCaster")]
    BloodOathCaster,
    /// 1015 — Blood Oath (curse)
    #[wire_enum(1015, "BloodOathCurse")]
    BloodOathCurse,
    /// 1016 — Corpse Skin
    #[wire_enum(1016, "CorpseSkin")]
    CorpseSkin,
    /// 1017 — Mind Rot
    #[wire_enum(1017, "MindRot")]
    MindRot,
    /// 1018 — Pain Spike
    #[wire_enum(1018, "PainSpike")]
    PainSpike,
    /// 1019 — Strangle
    #[wire_enum(1019, "Strangle")]
    Strangle,
    /// 1020 — Gift of Renewal
    #[wire_enum(1020, "GiftOfRenewal")]
    GiftOfRenewal,
    /// 1021 — Attune Weapon
    #[wire_enum(1021, "AttuneWeapon")]
    AttuneWeapon,
    /// 1022 — Thunderstorm
    #[wire_enum(1022, "Thunderstorm")]
    Thunderstorm,
    /// 1023 — Essence of Wind
    #[wire_enum(1023, "EssenceOfWind")]
    EssenceOfWind,
    /// 1024 — Ethereal Voyage
    #[wire_enum(1024, "EtherealVoyage")]
    EtherealVoyage,
    /// 1025 — Gift of Life
    #[wire_enum(1025, "GiftOfLife")]
    GiftOfLife,
    /// 1026 — Arcane Empowerment
    #[wire_enum(1026, "ArcaneEmpowerment")]
    ArcaneEmpowerment,
    /// 1027 — Mortal Strike
    #[wire_enum(1027, "MortalStrike")]
    MortalStrike,
    /// 1028 — Reactive Armor (1075812, 1075813)
    #[wire_enum(1028, "ReactiveArmor")]
    ReactiveArmor,
    /// 1029 — Protection (1075814, 1075815)
    #[wire_enum(1029, "Protection")]
    Protection,
    /// 1030 — Arch Protection (1075816, 1075816)
    #[wire_enum(1030, "ArchProtection")]
    ArchProtection,
    /// 1031 — Magic Reflection (1075817, 1075818)
    #[wire_enum(1031, "MagicReflection")]
    MagicReflection,
    /// 1032 — Incognito (1075819, 1075820)
    #[wire_enum(1032, "Incognito")]
    Incognito,
    /// 1033 — Disguised
    #[wire_enum(1033, "Disguised")]
    Disguised,
    /// 1034 — Animal Form
    #[wire_enum(1034, "AnimalForm")]
    AnimalForm,
    /// 1035 — Polymorph (1075824, 1075820)
    #[wire_enum(1035, "Polymorph")]
    Polymorph,
    /// 1036 — Invisibility (1075825, 1075826)
    #[wire_enum(1036, "Invisibility")]
    Invisibility,
    /// 1037 — Paralyze (1075827, 1075828)
    #[wire_enum(1037, "Paralyze")]
    Paralyze,
    /// 1038 — Poison (1042011, 1069489)
    #[wire_enum(1038, "Poison")]
    Poison,
    /// 1039 — Bleed (1075893, 1075894)
    #[wire_enum(1039, "Bleed")]
    Bleed,
    /// 1040 — Clumsy (1075895, 1075896)
    #[wire_enum(1040, "Clumsy")]
    Clumsy,
    /// 1041 — Feeble Mind (1075897, 1075898)
    #[wire_enum(1041, "FeebileMind")]
    FeebileMind,
    /// 1042 — Weaken (1075837, 1075838)
    #[wire_enum(1042, "Weaken")]
    Weaken,
    /// 1043 — Curse (1075835, 1075836)
    #[wire_enum(1043, "Curse")]
    Curse,
    /// 1044 — Mass Curse (1075903, 1075904)
    #[wire_enum(1044, "MassCurse")]
    MassCurse,
    /// 1045 — Agility (1075905, 1075906)
    #[wire_enum(1045, "Agility")]
    Agility,
    /// 1046 — Cunning (1075907, 1075908)
    #[wire_enum(1046, "Cunning")]
    Cunning,
    /// 1047 — Strength (1075909, 1075910)
    #[wire_enum(1047, "Strength")]
    Strength,
    /// 1048 — Bless (1075911, 1075912)
    #[wire_enum(1048, "Bless")]
    Bless,
    /// Unknown buff icon number.
    #[wire_enum(unknown)]
    Unknown(u16),
}

// ── 0xDF BuffDebuff (dynamic, S→C) ────────────────────────────────────────

/// Packet 0xDF — Buff / Debuff System (dynamic, S→C)
///
/// Adds or removes a buff/debuff status icon from the player's buff bar.
///
/// See the module-level documentation for the full wire format description.
///
/// > **Note:** The packet format is partially unverified.  The `args` field
/// > uses little-endian UTF-16 ("Flipped Unicode String") with a leading
/// > space; multiple entries are separated by additional spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BuffDebuff {
    /// Pre-5.0.2b: add / show a buff icon with full metadata.
    Add {
        /// Serial of the player receiving the buff.
        serial: u32,
        /// Buff/debuff icon identifier.
        icon: u16,
        /// Countdown duration in seconds.  `0` = no countdown timer.
        duration_secs: u16,
        /// Title cliloc string number.
        cliloc_id1: u32,
        /// Body / description cliloc string number.
        cliloc_id2: u32,
        /// Argument string for cliloc interpolation, encoded as little-endian
        /// UTF-16 on the wire.  Entries are space-separated; the whole string
        /// has a leading space on the wire.
        args: String,
    },

    /// Pre-5.0.2b: remove a buff icon.
    Remove {
        /// Serial of the player losing the buff.
        serial: u32,
        /// Buff/debuff icon identifier.
        icon: u16,
    },

    /// Post-5.0.2b: compact notification carrying only serial + icon.
    ///
    /// Used by newer clients.  Whether this represents add or remove is
    /// determined by higher-level game logic, not the packet itself.
    Compact {
        /// Serial of the affected player.
        serial: u32,
        /// Buff/debuff icon identifier.
        icon: u16,
    },
}

impl BuffDebuff {
    /// Return the serial of the affected player.
    pub fn serial(&self) -> u32 {
        match self {
            Self::Add { serial, .. } => *serial,
            Self::Remove { serial, .. } => *serial,
            Self::Compact { serial, .. } => *serial,
        }
    }

    /// Return the raw icon number.
    pub fn icon(&self) -> u16 {
        match self {
            Self::Add { icon, .. } => *icon,
            Self::Remove { icon, .. } => *icon,
            Self::Compact { icon, .. } => *icon,
        }
    }

    /// Resolve the icon number to a [`BuffIcon`].
    pub fn buff_icon(&self) -> BuffIcon {
        BuffIcon::from_wire(self.icon())
    }
}

impl ManualPacket for BuffDebuff {
    const ID: u8 = 0xDF;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) + serial(4) + icon(2) = 9 bytes
        let mut r = packet_reader(data, Self::ID, 9, true)?;

        let serial: u32 = Decode::decode(&mut r)?;
        let icon: u16 = Decode::decode(&mut r)?;

        // Determine format by how much data remains after serial + icon.
        // Post-5.0.2b: no further bytes (remaining == 0).
        // Pre-5.0.2b remove: exactly 2 bytes remain (the action word 0x0000).
        // Pre-5.0.2b add: action word 0x0001 followed by the full payload.
        if r.remaining_len() == 0 {
            return Ok(Self::Compact { serial, icon });
        }

        let action: u16 = Decode::decode(&mut r)?;
        if action == 0x0000 {
            return Ok(Self::Remove { serial, icon });
        }

        // action == 0x0001 — full add payload
        // BYTE[4] 0x00000000
        let _pad0: u32 = Decode::decode(&mut r)?;
        // BYTE[2] icon (repeated)
        let _icon2: u16 = Decode::decode(&mut r)?;
        // BYTE[2] 0x0001 (repeated)
        let _action2: u16 = Decode::decode(&mut r)?;
        // BYTE[4] 0x00000000
        let _pad1: u32 = Decode::decode(&mut r)?;
        // BYTE[2] duration in seconds
        let duration_secs: u16 = Decode::decode(&mut r)?;
        // BYTE[2] 0x0000
        let _pad2: u16 = Decode::decode(&mut r)?;
        // BYTE[1] 0x00
        let _pad3: u8 = Decode::decode(&mut r)?;
        // BYTE[4] cliloc_id1
        let cliloc_id1: u32 = Decode::decode(&mut r)?;
        // BYTE[4] cliloc_id2
        let cliloc_id2: u32 = Decode::decode(&mut r)?;
        // BYTE[4] 0x00000000
        let _pad4: u32 = Decode::decode(&mut r)?;
        // BYTE[2] 0x0001
        let _str_flag: u16 = Decode::decode(&mut r)?;

        // Remaining bytes: little-endian UTF-16 string, null-terminated.
        let args = decode_le_utf16_str(&mut r)?;

        Ok(Self::Add {
            serial,
            icon,
            duration_secs,
            cliloc_id1,
            cliloc_id2,
            args,
        })
    }
}

// ── Encode ─────────────────────────────────────────────────────────────────

impl Encode<BE> for BuffDebuff {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder

        match self {
            Self::Remove { serial, icon } => {
                w.put_u32(*serial);
                w.put_u16(*icon);
                w.put_u16(0x0000); // action = remove
            }

            Self::Compact { serial, icon } => {
                w.put_u32(*serial);
                w.put_u16(*icon);
                // No further bytes — post-5.0.2b compact format
            }

            Self::Add {
                serial,
                icon,
                duration_secs,
                cliloc_id1,
                cliloc_id2,
                args,
            } => {
                w.put_u32(*serial);
                w.put_u16(*icon);
                w.put_u16(0x0001);         // action = show
                w.put_u32(0x00000000);     // pad
                w.put_u16(*icon);          // icon repeated
                w.put_u16(0x0001);         // action repeated
                w.put_u32(0x00000000);     // pad
                w.put_u16(*duration_secs);
                w.put_u16(0x0000);         // pad
                w.put_u8(0x00);            // pad
                w.put_u32(*cliloc_id1);
                w.put_u32(*cliloc_id2);
                w.put_u32(0x00000000);     // pad
                w.put_u16(0x0001);         // string flag

                // Little-endian UTF-16 string, null-terminated.
                encode_le_utf16_str(args, w);
            }
        }
    }
}
