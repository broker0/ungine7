//! Action / text command packets (0x12).
//!
//! Packet 0x12 is a multipurpose "text command" sent by the client to
//! request skill usage, macro'd spell casting, door opening, or emote
//! actions (bow, salute).
//!
//! The `type` byte discriminates the command kind:
//!
//! | Type   | Meaning       | Payload                                  |
//! |--------|---------------|------------------------------------------|
//! | `0x24` | Use Skill     | `"<skill_id> 0\0"` e.g. `"1 0"` = Anatomy |
//! | `0x56` | Cast Spell    | `"<spell_id>\0"` e.g. `"2"` = Create Food  |
//! | `0x58` | Open Door     | `"\0"` (just null terminator)            |
//! | `0xC7` | Action        | `"<action>\0"` e.g. `"bow"`, `"salute"`  |

use u_io::{BE, BinaryWriter, Decode, Encode, NullString, packet_reader};

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── 0x12 TextCommand (variable, C→S) ──────────────────────────────────────

/// Packet 0x12 — Text Command / Request Skill etc. use (variable, C→S)
///
/// Sent by the client when the player activates a skill, casts a spell
/// via macro, opens a door, or performs an emote action.
///
/// # Wire format
///
/// ```text
/// BYTE    cmd           (0x12)
/// BYTE[2] blockSize     (total packet length)
/// BYTE    type          (0x24 / 0x56 / 0x58 / 0xC7)
/// BYTE[]  payload       (null-terminated ASCII string)
/// ```
///
/// # Examples
///
/// ```
/// use packets::action::TextCommand;
/// use packets::traits::ManualPacket;
///
/// let cmd = TextCommand::use_skill(1);   // Anatomy
/// let bytes = cmd.to_bytes();
/// let decoded = TextCommand::from_bytes(&bytes).unwrap();
/// assert_eq!(cmd, decoded);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TextCommand {
    /// Type `0x24` (`$`) — Use Skill.
    ///
    /// Payload is `"<skill_id> 0"`, for example `"1 0"` for Anatomy.
    UseSkill {
        /// The raw null-terminated skill string (e.g. `"1 0"`).
        skill: NullString,
    },
    /// Type `0x56` (`V`) — Cast Spell (via macro).
    ///
    /// Payload is `"<spell_id>"`, for example `"2"` for Create Food.
    CastSpell {
        /// The raw null-terminated spell string (e.g. `"2"`).
        spell: NullString,
    },
    /// Type `0x58` (`X`) — Open Door.
    ///
    /// No meaningful payload (just a null terminator).
    OpenDoor,
    /// Type `0xC7` — Action / Emote.
    ///
    /// Payload is the action name, e.g. `"bow"` or `"salute"`.
    Action {
        /// The raw null-terminated action string (e.g. `"bow"`).
        action: NullString,
    },
}

/// Type byte constants for the command discriminator.
impl TextCommand {
    /// Type byte for Use Skill.
    pub const TYPE_SKILL: u8 = 0x24;
    /// Type byte for Cast Spell.
    pub const TYPE_SPELL: u8 = 0x56;
    /// Type byte for Open Door.
    pub const TYPE_OPEN_DOOR: u8 = 0x58;
    /// Type byte for Action.
    pub const TYPE_ACTION: u8 = 0xC7;
}

impl ManualPacket for TextCommand {
    const ID: u8 = 0x12;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 5, true)?;
        let type_byte: u8 = Decode::decode(&mut r)?;

        match type_byte {
            Self::TYPE_SKILL => {
                let skill: NullString = Decode::decode(&mut r)?;
                Ok(Self::UseSkill { skill })
            }
            Self::TYPE_SPELL => {
                let spell: NullString = Decode::decode(&mut r)?;
                Ok(Self::CastSpell { spell })
            }
            Self::TYPE_OPEN_DOOR => {
                // Consume the null terminator.
                let _null: u8 = Decode::decode(&mut r)?;
                Ok(Self::OpenDoor)
            }
            Self::TYPE_ACTION => {
                let action: NullString = Decode::decode(&mut r)?;
                Ok(Self::Action { action })
            }
            other => Err(u_io::DecodeError::Other(format!(
                "unknown TextCommand type: 0x{other:02X}"
            ))
            .into()),
        }
    }
}

// ── Factory methods ───────────────────────────────────────────────────────

impl TextCommand {
    /// Create a "Use Skill" command from a skill ID.
    ///
    /// The skill string is formatted as `"<skill_id> 0"` per the UO
    /// protocol convention (the trailing ` 0` is always present).
    pub fn use_skill(skill_id: u16) -> Self {
        Self::UseSkill {
            skill: NullString(format!("{skill_id} 0")),
        }
    }

    /// Create a "Cast Spell" command from a spell ID.
    pub fn cast_spell(spell_id: u16) -> Self {
        Self::CastSpell {
            spell: NullString(format!("{spell_id}")),
        }
    }

    /// Create an "Open Door" command.
    pub fn open_door() -> Self {
        Self::OpenDoor
    }

    /// Create a "Bow" action command.
    pub fn bow() -> Self {
        Self::Action {
            action: NullString("bow".to_string()),
        }
    }

    /// Create a "Salute" action command.
    pub fn salute() -> Self {
        Self::Action {
            action: NullString("salute".to_string()),
        }
    }

    /// Create an action command with a custom action string.
    pub fn action(name: impl Into<String>) -> Self {
        Self::Action {
            action: NullString(name.into()),
        }
    }
}

// ── Encoding ──────────────────────────────────────────────────────────────

impl Encode<BE> for TextCommand {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder

        match self {
            Self::UseSkill { skill } => {
                w.put_u8(Self::TYPE_SKILL);
                skill.encode(w);
            }
            Self::CastSpell { spell } => {
                w.put_u8(Self::TYPE_SPELL);
                spell.encode(w);
            }
            Self::OpenDoor => {
                w.put_u8(Self::TYPE_OPEN_DOOR);
                w.put_u8(0x00); // null terminator
            }
            Self::Action { action } => {
                w.put_u8(Self::TYPE_ACTION);
                action.encode(w);
            }
        }
    }
}
