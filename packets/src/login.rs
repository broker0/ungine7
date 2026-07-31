//! Login flow packets.
//!
//! Covers both the login phase (account authentication, server selection)
//! and the game phase entry (game login, character selection).

use std::net::Ipv4Addr;

use u_io::{FixedString, ListU16, ListU8, RawBytes};
use macros::{Decode, Encode, Packet, WireEnum};

use crate::traits::BasicPacket;

// ── 0x80 AccountLogin (62 bytes, C→S) ──────────────────────────────────────

/// Packet 0x80 — AccountLogin (62 bytes, fixed, C→S)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x80, size = fixed(62), endian = "be")]
pub struct AccountLogin {
    pub id: u8,
    pub account: FixedString<30>,
    pub password: FixedString<30>,
    /// Next-login key sent by the client (0xFF on OSI; used as a seed hint).
    pub next_login_key: u8,
}

impl AccountLogin {
    pub fn new(account: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            id: Self::ID,
            account: FixedString::new(account),
            password: FixedString::new(password),
            next_login_key: 0xFF,
        }
    }
}

// ── 0xA0 SelectServer (3 bytes, C→S) ───────────────────────────────────────

/// Packet 0xA0 — SelectServer (3 bytes, fixed, C→S)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xA0, size = fixed(3), endian = "be")]
pub struct SelectServer {
    pub id: u8,
    pub index: u16,
}

impl SelectServer {
    pub fn new(index: u16) -> Self {
        Self { id: Self::ID, index }
    }
}

// ── 0x91 GameLogin (65 bytes, C→S) ─────────────────────────────────────────

/// Packet 0x91 — GameLogin (65 bytes, fixed, C→S)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x91, size = fixed(65), endian = "be")]
pub struct GameLogin {
    pub id: u8,
    pub auth_key: u32,
    pub account: FixedString<30>,
    pub password: FixedString<30>,
}

impl GameLogin {
    pub fn new(auth_key: u32, account: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            id: Self::ID,
            auth_key,
            account: FixedString::new(account),
            password: FixedString::new(password),
        }
    }
}

// ── 0x5D LoginCharacter (73 bytes, C→S) ────────────────────────────────────

/// Packet 0x5D — LoginCharacter (73 bytes, fixed, C→S)
///
/// Sent by the client to select and log in as a character from the
/// character list.
///
/// `client_flags` indicates which expansions the client supports:
///
/// | Value  | Expansion      |
/// |--------|----------------|
/// | 0x00   | T2A            |
/// | 0x01   | Renaissance    |
/// | 0x02   | Third Dawn     |
/// | 0x04   | LBR            |
/// | 0x08   | AOS            |
/// | 0x10   | SE             |
/// | 0x20   | SA             |
/// | 0x40   | UO3D           |
/// | 0x80   | Reserved       |
/// | 0x100  | 3D Client      |
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x5D, size = fixed(73), endian = "be")]
pub struct LoginCharacter {
    pub id: u8,
    #[binary(const_value = 0xEDED_EDEDu32)]
    pub _const0: u32,
    pub name: FixedString<30>,
    #[binary(pad = 2)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    pub client_flags: u32,
    pub unknown1: u32,
    pub login_count: u32,
    #[binary(pad = 16)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad1: (),
    pub slot: u32,
    pub client_ip: u32,
}

impl LoginCharacter {
    pub fn new(name: impl Into<String>, slot: u32) -> Self {
        Self {
            id: Self::ID,
            _const0: 0xEDED_EDED,
            name: FixedString::new(name),
            _pad0: (),
            client_flags: 0x0000_001F,
            unknown1: 1,
            login_count: 1,
            _pad1: (),
            slot,
            client_ip: 0xC0A8_0101, // 192.168.1.1
        }
    }
}

// ── 0x00 CreateCharacter (104 bytes, C→S) ──────────────────────────────────

/// Packet 0x00 — CreateCharacter (104 bytes, fixed, C→S)
///
/// Sent by the client when the player finishes the character-creation
/// screen.  This is the classic layout used by clients prior to the
/// Stygian Abyss expansion (the framing for opcode `0x00` is 104 bytes in
/// the legacy packet table).
///
/// Three starting skills are chosen by the player (`skillN_id` +
/// `skillN_value`).  `city_index` references an entry in the
/// [`StartingLocation`] list previously sent in the [`CharacterList`]
/// (0xA9) packet.  `slot` is the character slot the new character is
/// created in.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x00, size = fixed(104), endian = "be")]
pub struct CreateCharacter {
    pub id: u8,
    #[binary(const_value = 0xEDED_EDEDu32)]
    pub _pattern1: u32,
    #[binary(const_value = 0xFFFF_FFFFu32)]
    pub _pattern2: u32,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pattern3: (),
    pub name: FixedString<30>,
    #[binary(pad = 2)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _unknown0: (),
    pub client_flags: u32,
    pub unknown1: u32,
    pub login_count: u32,
    pub profession: u8,
    #[binary(pad = 15)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _unknown2: (),
    /// Low bit = female (0 = male, 1 = female); higher bits encode race
    /// on SA clients.
    pub sex: u8,
    pub strength: u8,
    pub dexterity: u8,
    pub intelligence: u8,
    pub skill1_id: u8,
    pub skill1_value: u8,
    pub skill2_id: u8,
    pub skill2_value: u8,
    pub skill3_id: u8,
    pub skill3_value: u8,
    pub skin_hue: u16,
    pub hair_graphic: u16,
    pub hair_hue: u16,
    pub beard_graphic: u16,
    pub beard_hue: u16,
    /// Index into the starting-locations list sent in [`CharacterList`].
    pub city_index: u16,
    pub slot: u32,
    pub client_ip: u32,
    pub shirt_hue: u16,
    pub pants_hue: u16,
}

impl CreateCharacter {
    /// Returns `true` if the female sex flag is set.
    pub fn is_female(&self) -> bool {
        self.sex & 0x01 != 0
    }
}

// ── 0xA8 GameServerList (dynamic, S→C) ─────────────────────────────────────

/// A single game server entry in the server list.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct GameServerEntry {
    pub index: u16,
    /// Server name, stored verbatim as 32 raw bytes (null-padded ASCII).
    ///
    /// Use [`RawBytes::as_str_lossy`] to obtain the human-readable name.
    /// Raw storage is required because some servers write non-zero bytes
    /// in the padding region of this field.
    pub name: RawBytes<32>,
    pub full_percent: u8,
    pub timezone: u8,
    pub ip: Ipv4Addr,
}

/// Packet 0xA8 — GameServerList (dynamic, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xA8, size = dynamic, endian = "be")]
pub struct GameServerList {
    pub id: u8,
    pub len: u16,
    pub system_info_flag: u8,
    pub servers: ListU16<GameServerEntry>,
}

impl GameServerList {
    pub fn new(system_info_flag: u8, servers: Vec<GameServerEntry>) -> Self {
        Self {
            id: Self::ID,
            len: 0,
            system_info_flag,
            servers: ListU16::new(servers),
        }
    }
}

// ── 0xA9 CharacterList (dynamic, S→C) ──────────────────────────────────────

/// A single character slot from the character list.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct CharacterSlot {
    pub name: FixedString<30>,
    #[binary(pad = 30)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
}

impl CharacterSlot {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: FixedString::new(name),
            _pad0: (),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

/// A starting location entry.
#[derive(Debug, Clone, PartialEq, Eq, Decode, Encode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[binary(endian = "be")]
pub struct StartingLocation {
    pub index: u8,
    pub city_name: FixedString<31>,
    pub area_name: FixedString<31>,
}

/// Packet 0xA9 — Characters / Starting Locations (dynamic, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xA9, size = dynamic, endian = "be")]
pub struct CharacterList {
    pub id: u8,
    pub len: u16,
    pub characters: ListU8<CharacterSlot>,
    pub starting_locations: ListU8<StartingLocation>,
    pub flags: u32,
}

impl CharacterList {
    pub fn new(
        characters: Vec<CharacterSlot>,
        starting_locations: Vec<StartingLocation>,
        flags: u32,
    ) -> Self {
        Self {
            id: Self::ID,
            len: 0,
            characters: ListU8::new(characters),
            starting_locations: ListU8::new(starting_locations),
            flags,
        }
    }

    /// Find the first non-empty character slot.
    pub fn first_character(&self) -> Option<(usize, &CharacterSlot)> {
        self.characters
            .iter()
            .enumerate()
            .find(|(_, c)| !c.is_empty())
    }
}

// ── 0x82 LoginDenied (2 bytes, S→C) ───────────────────────────────────────

/// Reason sent in a [`LoginDenied`] packet (0x82).
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum LoginDeniedReason {
    #[wire_enum(0x00, "incorrect name/password")]
    IncorrectCredentials,
    #[wire_enum(0x01, "someone is already using this account")]
    AccountInUse,
    #[wire_enum(0x02, "your account has been blocked")]
    AccountBlocked,
    #[wire_enum(0x03, "your account credentials are invalid")]
    InvalidCredentials,
    #[wire_enum(0x04, "communication problem")]
    CommunicationProblem,
    #[wire_enum(0x05, "the IGR concurrency limit has been met")]
    IgrConcurrencyLimit,
    #[wire_enum(0x06, "the IGR time limit has been met")]
    IgrTimeLimit,
    #[wire_enum(0x07, "general IGR authentication failure")]
    IgrAuthFailure,
    #[wire_enum(unknown)]
    Unknown(u8),
}

/// Packet 0x82 — LoginDenied (2 bytes, fixed, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x82, size = fixed(2), endian = "be")]
pub struct LoginDenied {
    pub id: u8,
    pub reason: LoginDeniedReason,
}

impl LoginDenied {
    pub fn new(reason: LoginDeniedReason) -> Self {
        Self { id: Self::ID, reason }
    }
}

// ── 0x53 LoginRejected (2 bytes, S→C) ─────────────────────────────────────

/// Reason sent in a [`LoginRejected`] packet (0x53).
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum LoginRejectedReason {
    #[wire_enum(0x00, "incorrect password")]
    IncorrectPassword,
    #[wire_enum(0x01, "this character does not exist")]
    CharacterNotFound,
    #[wire_enum(0x02, "this character already exists")]
    CharacterAlreadyExist,
    #[wire_enum(0x03, "client could not attach to the game server")]
    ServerAttachError1,
    #[wire_enum(0x04, "client could not attach to the game server")]
    ServerAttachError2,
    #[wire_enum(0x05, "another character is logged in")]
    AnotherCharLogged,
    #[wire_enum(0x06, "synchronization Error")]
    SynchronizationError,
    #[wire_enum(0x07, "idle too long")]
    TooLongIdle,
    #[wire_enum(0x08, "client could not attach to the game server")]
    ServerAttachError3,
    #[wire_enum(0x09, "character transfer")]
    CharacterTransfer,
    #[wire_enum(unknown)]
    Unknown(u8),
}

/// Packet 0x53 — LoginRejected / ClientMessage (2 bytes, fixed, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x53, size = fixed(2), endian = "be")]
pub struct LoginRejected {
    pub id: u8,
    pub reason: LoginRejectedReason,
}

impl LoginRejected {
    pub fn new(reason: LoginRejectedReason) -> Self {
        Self { id: Self::ID, reason }
    }
}
