//! Skill packets (0x3A).
//!
//! Packet 0x3A is used in both directions:
//!
//! - **Server → Client**: skill list or single skill update ([`SendSkills`])
//! - **Client → Server**: set skill lock state ([`SetSkillLock`])
//!
//! The server format has multiple sub-types controlled by a `type` byte:
//!
//! | Type | Meaning                           |
//! |------|-----------------------------------|
//! | 0x00 | Full list (no cap), null-terminated|
//! | 0x02 | Full list with skill cap          |
//! | 0xFF | Single skill update (no cap)      |
//! | 0xDF | Single skill update with cap      |

use u_io::{BE, BinaryWriter, Decode, Encode, ReadPrimitives, packet_reader};
use macros::{Decode, Encode, Packet, WireEnum};

use crate::traits::{ManualPacket, PacketError, PacketSize, BasicPacket};

// ── SkillLock enum ─────────────────────────────────────────────────────────

/// Skill lock state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum SkillLock {
    #[wire_enum(0x00, "up")]
    Up,
    #[wire_enum(0x01, "down")]
    Down,
    #[wire_enum(0x02, "locked")]
    Locked,
    #[wire_enum(unknown)]
    Unknown(u8),
}

// ── Sub-structs ────────────────────────────────────────────────────────────

/// A single skill entry without cap (used with type 0x00 / 0xFF).
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct SkillEntry {
    pub skill_id: u16,
    pub value: u16,
    pub unmodified_value: u16,
    pub lock: SkillLock,
}

/// A single skill entry with cap (used with type 0x02 / 0xDF).
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct SkillEntryWithCap {
    pub skill_id: u16,
    pub value: u16,
    pub unmodified_value: u16,
    pub lock: SkillLock,
    pub cap: u16,
}

// ── Client → Server: SetSkillLock ──────────────────────────────────────────

/// Packet 0x3A — Set Skill Lock (variable, C→S)
///
/// Sent by the client to change the lock state of a single skill.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x3A, size = dynamic, endian = "be")]
pub struct SetSkillLock {
    pub id: u8,
    pub len: u16,
    pub skill_id: u16,
    pub lock: SkillLock,
}

impl SetSkillLock {
    pub fn new(skill_id: u16, lock: SkillLock) -> Self {
        Self {
            id: Self::ID,
            len: 0,
            skill_id,
            lock,
        }
    }
}

// ── Server → Client: SendSkills ────────────────────────────────────────────

/// Packet 0x3A — Send Skills (variable, S→C)
///
/// Server-to-client skill data. The format depends on the `type` byte:
///
/// - `FullList` (type 0x00): all skills without cap, null-terminated
/// - `FullListWithCap` (type 0x02): all skills with cap per skill
/// - `SingleUpdate` (type 0xFF): single skill without cap
/// - `SingleUpdateWithCap` (type 0xDF): single skill with cap
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SendSkills {
    /// Type 0x00 — full skill list without cap, null-terminated.
    FullList {
        skills: Vec<SkillEntry>,
    },
    /// Type 0x02 — full skill list with skill cap per entry.
    FullListWithCap {
        skills: Vec<SkillEntryWithCap>,
    },
    /// Type 0xFF — single skill update without cap.
    SingleUpdate(SkillEntry),
    /// Type 0xDF — single skill update with cap.
    SingleUpdateWithCap(SkillEntryWithCap),
}

impl ManualPacket for SendSkills {
    const ID: u8 = 0x3A;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut reader = packet_reader(data, Self::ID, 4, true)?;
        let skill_type: u8 = Decode::decode(&mut reader)?;

        match skill_type {
            0x00 => {
                // Full list without cap, terminated by skill_id == 0x0000.
                let mut skills = Vec::new();
                loop {
                    let skill_id: u16 = Decode::decode(&mut reader)?;
                    if skill_id == 0x0000 {
                        break;
                    }
                    let value: u16 = Decode::decode(&mut reader)?;
                    let unmodified_value: u16 = Decode::decode(&mut reader)?;
                    let lock: SkillLock = Decode::decode(&mut reader)?;
                    skills.push(SkillEntry {
                        skill_id,
                        value,
                        unmodified_value,
                        lock,
                    });
                }
                Ok(SendSkills::FullList { skills })
            }
            0x02 => {
                // Full list with cap, read until end of packet data.
                let mut skills = Vec::new();
                while reader.remaining().unwrap_or(0) >= 9 {
                    skills.push(SkillEntryWithCap::decode(&mut reader)?);
                }
                Ok(SendSkills::FullListWithCap { skills })
            }
            0xFF => {
                let entry = SkillEntry::decode(&mut reader)?;
                Ok(SendSkills::SingleUpdate(entry))
            }
            0xDF => {
                let entry = SkillEntryWithCap::decode(&mut reader)?;
                Ok(SendSkills::SingleUpdateWithCap(entry))
            }
            other => Err(u_io::DecodeError::Other(format!(
                "unknown SendSkills type: 0x{other:02X}"
            ))
            .into()),
        }
    }
}

impl Encode<BE> for SendSkills {
    fn encode(&self, writer: &mut BinaryWriter<BE>) {
        // id
        writer.put_u8(Self::ID);
        // len placeholder
        writer.put_u16(0);

        match self {
            SendSkills::FullList { skills } => {
                writer.put_u8(0x00);
                for entry in skills {
                    entry.encode(writer);
                }
                // Null terminator
                writer.put_u16(0x0000);
            }
            SendSkills::FullListWithCap { skills } => {
                writer.put_u8(0x02);
                for entry in skills {
                    entry.encode(writer);
                }
            }
            SendSkills::SingleUpdate(entry) => {
                writer.put_u8(0xFF);
                entry.encode(writer);
            }
            SendSkills::SingleUpdateWithCap(entry) => {
                writer.put_u8(0xDF);
                entry.encode(writer);
            }
        }
    }
}
