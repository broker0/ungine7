//! Small system/utility packets: ping, login complete, feature flags,
//! weather, seasons, sound effects, client version.

use u_core::ProtocolVersion;
use u_io::{BE, BinaryReader, BinaryWriter, Decode, Encode, FixedString, NullString, packet_reader};
use macros::{Packet, WireEnum};

use crate::traits::{ManualPacket, PacketError, PacketSize, BasicPacket};

// ── 0x33 PauseClient (2 bytes, fixed, S→C) ────────────────────────────────

/// Packet 0x33 — Pause Client (2 bytes, fixed, S→C)
///
/// Sent by the server to pause or resume client input processing.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x33, size = fixed(2), endian = "be")]
pub struct PauseClient {
    pub id: u8,
    /// `1` = pause, `0` = resume.
    pub pause: u8,
}

impl PauseClient {
    pub fn pause() -> Self {
        Self { id: Self::ID, pause: 1 }
    }

    pub fn resume() -> Self {
        Self { id: Self::ID, pause: 0 }
    }
}

// ── 0x73 Ping (2 bytes, fixed, bidirectional) ──────────────────────────────

/// Packet 0x73 — Ping (2 bytes, fixed, bidirectional)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x73, size = fixed(2), endian = "be")]
pub struct Ping {
    pub id: u8,
    pub sequence: u8,
}

impl Ping {
    pub fn new(sequence: u8) -> Self {
        Self { id: Self::ID, sequence }
    }
}

// ── 0x55 LoginComplete (1 byte, fixed, S→C) ───────────────────────────────

/// Packet 0x55 — LoginComplete (1 byte, fixed, S→C)
///
/// Sent by the game server after character login to signal that the
/// client has fully entered the world. All initial world state packets
/// (0x1B, 0x20, etc.) arrive before this marker.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x55, size = fixed(1), endian = "be")]
pub struct LoginComplete {
    pub id: u8,
}

impl LoginComplete {
    pub fn new() -> Self {
        Self { id: Self::ID }
    }
}

// ── 0xB9 EnableFeatures (version-dependent, S→C) ──────────────────────────
//
// Format depends on client version:
// - Clients < 6.0.14.2: 3 bytes (u8 id + u16 flags)
// - Clients >= 6.0.14.2: 5 bytes (u8 id + u32 flags)

/// Packet 0xB9 — Legacy format (3 bytes, fixed, S→C)
///
/// Used by clients before 6.0.14.2. Feature flags are u16.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xB9, size = fixed(3), endian = "be")]
pub struct EnableFeaturesLegacy {
    pub id: u8,
    pub flags: u16,
}

impl EnableFeaturesLegacy {
    pub fn new(flags: u16) -> Self {
        Self { id: Self::ID, flags }
    }
}

/// Packet 0xB9 — Extended format (5 bytes, fixed, S→C)
///
/// Used by clients from 6.0.14.2 onwards. Feature flags are u32.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xB9, size = fixed(5), endian = "be")]
pub struct EnableFeaturesExtended {
    pub id: u8,
    pub flags: u32,
}

impl EnableFeaturesExtended {
    pub fn new(flags: u32) -> Self {
        Self { id: Self::ID, flags }
    }
}

/// Packet 0xB9 — Enable Locked Client Features (S→C)
///
/// Version-dependent enum wrapper. Wraps [`EnableFeaturesLegacy`] (u16 flags)
/// and [`EnableFeaturesExtended`] (u32 flags) as variants.
///
/// # Reading
///
/// Use [`from_data`](Self::from_bytes) — detects format by data length.
///
/// # Writing
///
/// Use [`new_legacy`](Self::new_legacy) or [`new_extended`](Self::new_extended),
/// then encode via the [`Encode`] trait or [`to_bytes`](Self::to_bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EnableFeatures {
    Legacy(EnableFeaturesLegacy),
    Extended(EnableFeaturesExtended),
}

impl ManualPacket for EnableFeatures {
    const ID: u8 = 0xB9;
    const SIZE: PacketSize = PacketSize::Fixed(5);

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        if data.is_empty() {
            return Err(u_io::DecodeError::Truncated.into());
        }
        if data[0] != Self::ID {
            return Err(PacketError::BadId { expected: Self::ID, actual: data[0] });
        }
        match data.len() {
            3 => {
                let mut reader = BinaryReader::<BE>::new(data);
                Ok(Self::Legacy(u_io::Decode::decode(&mut reader)?))
            }
            5 => {
                let mut reader = BinaryReader::<BE>::new(data);
                Ok(Self::Extended(u_io::Decode::decode(&mut reader)?))
            }
            n => Err(u_io::DecodeError::Other(
                format!("unexpected 0xB9 length: {n}, expected 3 or 5"),
            ).into()),
        }
    }
}

impl EnableFeatures {
    /// Create with the correct format for the given client version.
    ///
    /// Clients before 6.0.14.2 use the legacy (u16) format;
    /// clients from 6.0.14.2 onwards use the extended (u32) format.
    pub fn new(flags: u32, version: ProtocolVersion) -> Self {
        if version >= ProtocolVersion::EXT_FEATURES_CLIENT {
            Self::Extended(EnableFeaturesExtended::new(flags))
        } else {
            Self::Legacy(EnableFeaturesLegacy::new(flags as u16))
        }
    }

    /// Create the legacy (u16) variant.
    pub fn new_legacy(flags: u16) -> Self {
        Self::Legacy(EnableFeaturesLegacy::new(flags))
    }

    /// Create the extended (u32) variant.
    pub fn new_extended(flags: u32) -> Self {
        Self::Extended(EnableFeaturesExtended::new(flags))
    }

    /// Get flags as u32 regardless of format.
    pub fn flags(&self) -> u32 {
        match self {
            Self::Legacy(p) => p.flags as u32,
            Self::Extended(p) => p.flags,
        }
    }
}

impl Encode<BE> for EnableFeatures {
    fn encode(&self, writer: &mut BinaryWriter<BE>) {
        match self {
            Self::Legacy(p) => p.encode(writer),
            Self::Extended(p) => p.encode(writer),
        }
    }
}

// ── 0x72 WarMode (5 bytes, fixed, bidirectional) ──────────────────────────

/// Packet 0x72 — Request / Set War Mode (5 bytes, fixed, bidirectional)
///
/// Sent by the client to toggle war mode, and echoed back by the server
/// to confirm the current state.
///
/// - `flag`: 0x00 = normal, 0x01 = fighting
/// - `unknown1`: always 0x00
/// - `unknown2`: 0x32 (50) on OSI; sometimes 0x00 on free shards
/// - `unknown3`: always 0x00
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x72, size = fixed(5), endian = "be")]
pub struct WarMode {
    pub id: u8,
    pub flag: u8,
    pub unknown1: u8,
    pub unknown2: u8,
    pub unknown3: u8,
}

impl WarMode {
    pub fn new(fighting: bool) -> Self {
        Self {
            id: Self::ID,
            flag: if fighting { 0x01 } else { 0x00 },
            unknown1: 0x00,
            unknown2: 0x32,
            unknown3: 0x00,
        }
    }

    /// Whether the character is in war/combat mode.
    pub fn is_fighting(&self) -> bool {
        self.flag != 0
    }
}

// ── 0x4F OverallLightLevel (2 bytes, fixed, S→C) ──────────────────────────

/// Packet 0x4F — Overall Light Level (2 bytes, fixed, S→C)
///
/// Sets the global light level for the client.
///
/// - `0x00`: day
/// - `0x09`: OSI night
/// - `0x1F`: black (max normal value)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x4F, size = fixed(2), endian = "be")]
pub struct OverallLightLevel {
    pub id: u8,
    pub level: u8,
}

// ── 0x54 PlaySoundEffect (12 bytes, fixed, S→C) ──────────────────────────

/// Packet 0x54 — Play Sound Effect (12 bytes, fixed, S→C)
///
/// Plays a sound effect at a specific location.
///
/// - `mode`: 0x00 = quiet/repeating, 0x01 = single normal sound effect
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x54, size = fixed(12), endian = "be")]
pub struct PlaySoundEffect {
    pub id: u8,
    pub mode: u8,
    pub sound_model: u16,
    pub unknown: u16,
    pub x: u16,
    pub y: u16,
    pub z: i16,
}



// ── 0x6D PlaySoundEffect (12 bytes, fixed, S→C) ──────────────────────────

/// Packet 0x6D —  Play Midi Music (3 bytes, fixed, S→C)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x6D, size = fixed(3), endian = "be")]
pub struct PlayMidiMusic {
    pub id: u8,
    pub music_id: u16,
}


// ── 0x65 SetWeather (4 bytes, fixed, S→C) ─────────────────────────────────

/// Packet 0x65 — Set Weather (4 bytes, fixed, S→C)
///
/// Sets the weather effect for the client.
///
/// Types:
/// - `0x00`: "It starts to rain"
/// - `0x01`: "A fierce storm approaches."
/// - `0x02`: "It begins to snow"
/// - `0x03`: "A storm is brewing."
/// - `0xFE`: no effect (set temperature?)
/// - `0xFF`: none (turns off sound effects)
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x65, size = fixed(4), endian = "be")]
pub struct SetWeather {
    pub id: u8,
    pub weather_type: u8,
    pub num_effects: u8,
    pub temperature: u8,
}

// ── Season ────────────────────────────────────────────────────────────────

/// Season flag used in [`SeasonalInformation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum Season {
    #[wire_enum(0x00, "spring")]
    Spring,
    #[wire_enum(0x01, "summer")]
    Summer,
    #[wire_enum(0x02, "fall")]
    Fall,
    #[wire_enum(0x03, "winter")]
    Winter,
    #[wire_enum(0x04, "desolation")]
    Desolation,
    #[wire_enum(unknown)]
    Unknown(u8),
}

// ── 0xBC SeasonalInformation (3 bytes, fixed, S→C) ────────────────────────

/// Packet 0xBC — Seasonal Information (3 bytes, fixed, S→C)
///
/// Sent by the server to change the visual season and optionally play
/// a transition sound effect.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xBC, size = fixed(3), endian = "be")]
pub struct SeasonalInformation {
    pub id: u8,
    pub season: Season,
    pub play_sound: u8,
}


// ── 0x76 NewSubserver (16 bytes, fixed, S→C) ─────────────────────────────

/// Packet 0x76 — New Subserver (16 bytes, fixed, S→C)
///
/// Sent by the server when the player crosses a server boundary,
/// indicating the new subserver coordinates and boundary rectangle.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x76, size = fixed(16), endian = "be")]
pub struct NewSubserver {
    pub id: u8,
    pub x: u16,
    pub y: u16,
    pub z: u16,
    #[binary(pad = 1)]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _pad0: (),
    pub boundary_x: u16,
    pub boundary_y: u16,
    pub boundary_width: u16,
    pub boundary_height: u16,
}


// ── 0xBD ClientVersion (dynamic, bidirectional) ───────────────────────────

/// Packet 0xBD — Client Version Request (3 bytes, S→C)
///
/// Sent by the server to prompt the client to reply with its version
/// string. The packet body is empty (length field = 3).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xBD, size = dynamic, endian = "be")]
pub struct ClientVersionRequest {
    pub id: u8,
    pub len: u16,
}

impl ClientVersionRequest {
    pub fn new() -> Self {
        Self { id: Self::ID, len: 3 }
    }
}

/// Packet 0xBD — Client Version Response (variable, C→S)
///
/// Sent by the client in reply to [`ClientVersionRequest`]. Contains
/// the version string (e.g. `"3.0.8j"`), null-terminated.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xBD, size = dynamic, endian = "be")]
pub struct ClientVersionResponse {
    pub id: u8,
    pub len: u16,
    pub version: NullString,
}

impl ClientVersionResponse {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            id: Self::ID,
            len: 0, // back-patched by encode_packet
            version: NullString::new(version),
        }
    }
}

// ── 0xBF GeneralInfo (dynamic, bidirectional) ─────────────────────────────

/// Sub-entry for [`GeneralInfo::EnableMapDiff`].
///
/// Contains the number of map and static patches for a single map file.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MapDiffEntry {
    pub map_patches: u32,
    pub static_patches: u32,
}

// ── Party sub-commands (0xBF sub-command 0x0006) ──────────────────────────

/// Party system sub-commands carried inside [`GeneralInfo::Party`].
///
/// Sub-command 0x0006 is bidirectional; the client and server use different
/// payloads for sub-sub-commands 1, 2, and 4.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PartyCommand {
    // ── Client → Server ───────────────────────────────────────────────────

    /// Sub-sub 0x01 C→S — Request to add a member (0 = show targeting cursor).
    AddMemberRequest { id: u32 },

    /// Sub-sub 0x02 C→S — Request to remove a member (0 = show targeting cursor).
    RemoveMemberRequest { id: u32 },

    /// Sub-sub 0x03 C→S — Send private message to a specific member.
    PrivateMessageToMember { target_id: u32, message: String },

    /// Sub-sub 0x04 C→S — Broadcast message to the whole party.
    BroadcastMessage { message: String },

    /// Sub-sub 0x06 C→S — Toggle whether others can loot the player.
    SetCanLoot { can_loot: bool },

    /// Sub-sub 0x08 C→S — Accept a party invitation.
    AcceptInvitation { leader_serial: u32 },

    /// Sub-sub 0x09 C→S — Decline a party invitation.
    DeclineInvitation { leader_serial: u32 },

    // ── Server → Client ───────────────────────────────────────────────────

    /// Sub-sub 0x01 S→C — Updated member list (sent after any member change).
    MemberList { members: Vec<u32> },

    /// Sub-sub 0x02 S→C — A member was removed; remaining member list follows.
    MemberRemoved { removed_id: u32, members: Vec<u32> },

    /// Sub-sub 0x03 S→C — Private message from a party member.
    PrivateMessageFromMember { source_id: u32, message: String },

    /// Sub-sub 0x04 S→C — Broadcast message from a party member.
    BroadcastMessageFromMember { source_id: u32, message: String },

    /// Sub-sub 0x07 S→C — Party invitation from the named leader.
    Invitation { leader_serial: u32 },
}

// ── Context-menu entry for 0x14 ───────────────────────────────────────────

/// A single 2D-client context-menu entry in [`GeneralInfo::DisplayPopupMenu`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PopupEntry2D {
    pub unique_id: u16,
    /// Cliloc ID − 3 000 000.
    pub cliloc_offset: u16,
    /// Flags: 0x00=enabled, 0x01=disabled, 0x02=arrow, 0x20=has_color.
    pub flags: u16,
    /// RGB 1555 color.  Present only when `flags & 0x20 != 0`.
    pub color: Option<u16>,
}

/// A single KR-client context-menu entry in [`GeneralInfo::DisplayPopupMenu`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PopupEntryKR {
    pub text_id: u32,
    pub index: u16,
    /// Flags: 0x00=enabled, 0x01=disabled, 0x04=highlighted.
    pub flags: u16,
}

/// Context-menu entries for sub-command 0x0014.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PopupEntries {
    /// 2D-client format (sub-sub 0x01).
    TwoD { serial: u32, entries: Vec<PopupEntry2D> },
    /// KR-client format (sub-sub 0x02).
    Kr { serial: u32, entries: Vec<PopupEntryKR> },
}

// ── Close UI window IDs for 0x16 ─────────────────────────────────────────

/// UI window identifiers for [`GeneralInfo::CloseUiWindow`].
///
/// | Wire value | Window       |
/// |------------|--------------|
/// | 0x01  | Paperdoll     |
/// | 0x02  | Status        |
/// | 0x08  | Char profile  |
/// | 0x0C  | Container     |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u32)]
pub enum UiWindow {
    #[wire_enum(0x01, "Paperdoll")]
    Paperdoll,
    #[wire_enum(0x02, "Status")]
    Status,
    #[wire_enum(0x08, "CharProfile")]
    CharProfile,
    #[wire_enum(0x0C, "Container")]
    Container,
    #[wire_enum(unknown)]
    Unknown(u32),
}

// ── Custom Housing tile update for 0x20 ─────────────────────────────────

/// Tile placement information in [`GeneralInfo::CustomHousing`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HousingTile {
    pub graphic: u16,
    pub x: u16,
    pub y: u16,
    pub z: u8,
}

/// Custom housing action type for sub-command 0x0020.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CustomHousingAction {
    /// Type 0x01 — place/update a tile.
    Update(HousingTile),
    /// Type 0x04 — begin custom housing session.
    Begin,
    /// Type 0x05 — end custom housing session.
    End,
}

// ── Extended stat sub-sub-commands for 0x19 ─────────────────────────────

/// Sub-sub-command data for [`GeneralInfo::ExtendedStats`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ExtendedStatPayload {
    /// Sub-sub 0x02 — 2D client: stat lock flags.
    ///
    /// `lock_flags` bits: `00SSDDII` (00=up, 01=down, 10=locked) for
    /// STR (SS), DEX (DD), INT (II).
    TwoD {
        serial: u32,
        lock_flags: u8,
    },
    /// Sub-sub 0x05 — KR client.
    Kr {
        serial: u32,
        lock_flags: u8,
        /// Present when `lock_flags == 0xFF` (update mobile status animation).
        animation: Option<KrAnimationData>,
        /// Any bytes that follow the parsed fields (server-dependent padding).
        extra: Vec<u8>,
    },
}

/// Animation data present in KR extended stats when `lock_flags == 0xFF`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KrAnimationData {
    pub status: u8,
    pub animation: u8,
    pub frame: u8,
}

// ── Spellbook content for 0x1B ───────────────────────────────────────────

/// New spellbook data for sub-command 0x001B.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpellbookData {
    pub serial: u32,
    pub item_id: u16,
    /// Scroll offset (1=regular, 101=necro, 201=paladin, 401=bushido,
    /// 501=ninjitsu, 601=spellweaving).
    pub scroll_offset: u16,
    /// 8-byte bitmask; bit N = spell N+1 present.
    pub content: [u8; 8],
}

// ── Change Race payload for 0x2A ─────────────────────────────────────────

/// Sub-command 0x002A payload.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ChangeRace {
    /// S→C — prompt to choose a new race/gender.
    Request { female: bool, race: u8 },
    /// C→S — response with chosen appearance.
    Response {
        skin_color: u16,
        hair_style: u16,
        hair_color: u16,
        beard_style: u16,
        beard_color: u16,
    },
}

// ── GeneralInfo enum ─────────────────────────────────────────────────────

/// Packet 0xBF — General Information Packet (variable, bidirectional)
///
/// A container packet dispatched by a `u16` sub-command at bytes 3–4.
/// Unknown sub-commands are captured as raw bytes.
///
/// # Wire format
///
/// ```text
/// [0xBF][len_hi][len_lo][sub_cmd_hi][sub_cmd_lo][payload...]
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GeneralInfo {
    /// Sub-command not yet implemented — carries the raw sub-data.
    Unknown { sub_cmd: u16, data: Vec<u8> },

    // ── 0x0001 S→C — Initialize Fast Walk Prevention ─────────────────────
    /// Six 32-bit keys seeded into the client's fast-walk stack.
    FastWalkInit { keys: [u32; 6] },

    // ── 0x0002 S→C — Add key to Fast Walk Stack ──────────────────────────
    /// Push one new key onto the top of the fast-walk stack.
    FastWalkAddKey { key: u32 },

    // ── 0x0004 S→C — Close Generic Gump ──────────────────────────────────
    /// Close a gump and simulate a button-press response.
    CloseGump { dialog_id: u32, button_id: u32 },

    // ── 0x0005 C→S — Screen Size ─────────────────────────────────────────
    /// Client screen dimensions.
    ScreenSize { unk1: u16, x: u16, y: u16, unk2: u16 },

    // ── 0x0006 — Party System ────────────────────────────────────────────
    /// Bidirectional party system sub-commands.
    Party(PartyCommand),

    // ── 0x0008 S→C — Set Cursor Hue / Set Map ────────────────────────────
    /// Controls cursor colour and active map.
    SetMap { world: u8 },

    // ── 0x000A C→S — Wrestling Stun (obsolete) ───────────────────────────
    /// Obsolete since AoS; no payload.
    WrestlingStun,

    // ── 0x000B C→S — Client Language ─────────────────────────────────────
    /// 3-character ASCII language code, null-terminated on the wire (4 bytes).
    /// e.g. `"ENU"` for English, `"RUS"` for Russian.
    ClientLanguage { language: FixedString<4> },

    // ── 0x000C C→S — Closed Status Gump ──────────────────────────────────
    /// Client reports it closed the status bar for this character.
    ClosedStatusGump { character_id: u32 },

    // ── 0x000E C→S — 3D Client Action ────────────────────────────────────
    /// Trigger a social animation (bow, wave, dance, etc.).
    ///
    /// Known IDs: 0x06=Yawn, 0x15=Faint, 0x20=Bow, 0x21=Salute, …
    ClientAction { animation_id: u32 },

    // ── 0x000F C→S — Client Type ─────────────────────────────────────────
    /// Sent once at login; identifies client type and feature flags.
    ClientType { unk1: u8, flags: u32 },

    // ── 0x0010 — Mega Cliloc related ─────────────────────────────────────
    /// Relationship to 0xD6 Mega Cliloc not fully documented.
    MegaClilocRelated { item_id: u32, unknown: u32 },

    // ── 0x0013 C→S — Request Popup Menu ──────────────────────────────────
    /// Request the context menu for a character.
    RequestPopupMenu { character_id: u32 },

    // ── 0x0014 S→C — Display Popup / Context Menu ────────────────────────
    /// Context menu entries for 2D or KR clients.
    DisplayPopupMenu(PopupEntries),

    // ── 0x0015 C→S — Popup Entry Selection ───────────────────────────────
    /// Player selected an entry from the context menu.
    PopupMenuSelection { character_id: u32, entry_tag: u16 },

    // ── 0x0016 S→C — Close User Interface Windows ────────────────────────
    /// Close a specific UI window.
    CloseUiWindow { window: UiWindow, serial: u32 },

    // ── 0x0017 S→C — Codex of Wisdom ─────────────────────────────────────
    /// Display a Codex of Wisdom entry.
    CodexOfWisdom {
        /// Always 1; if not 1, packet has no effect.
        unk: u8,
        msg_number: u32,
        /// 0 = flashing, 1 = directly opening.
        presentation: u8,
    },

    // ── 0x0018 S→C — Enable Map Diffs ────────────────────────────────────
    EnableMapDiff { maps: Vec<MapDiffEntry> },

    // ── 0x0019 S→C — Extended Stats ──────────────────────────────────────
    ExtendedStats(ExtendedStatPayload),

    // ── 0x001A C→S — Stat Lock Change ────────────────────────────────────
    /// Client changed the lock state of a stat on the status bar.
    StatLockChange {
        /// 0 = STR, 1 = DEX, 2 = INT.
        stat: u8,
        /// 0 = up, 1 = down, 2 = locked.
        status: u8,
    },

    // ── 0x001B S→C — New Spellbook ───────────────────────────────────────
    NewSpellbook(SpellbookData),

    // ── 0x001C C→S — Spell Selected ──────────────────────────────────────
    /// Client indicates which spell is currently selected.
    SpellSelected {
        /// Always 2 on the wire.
        unk: u16,
        /// Selected spell index + scroll offset from 0x1B.
        spell_index: u16,
    },

    // ── 0x001D S→C — Send House Revision State ───────────────────────────
    HouseRevisionState { house_serial: u32, revision: u32 },

    // ── 0x001E C→S — Request House State ─────────────────────────────────
    RequestHouseState { house_serial: u32 },

    // ── 0x0020 S→C — Custom Housing ──────────────────────────────────────
    CustomHousing { house_serial: u32, action: CustomHousingAction },

    // ── 0x0021 S→C — Ability Icon Confirm ────────────────────────────────
    /// Resets ability icon colour; also sent when ability attempt is denied.
    AbilityIconConfirm,

    // ── 0x0022 S→C — Damage ──────────────────────────────────────────────
    /// Display damage dealt above a mobile's head.
    Damage { serial: u32, damage: u8 },

    // ── 0x0024 C→S — Unknown (UOSE) ──────────────────────────────────────
    UnknownUose { unknown: u8 },

    // ── 0x0025 S→C — SE Ability Change ───────────────────────────────────
    SeAbilityChange { ability_id: u8, enabled: bool },

    // ── 0x0026 S→C — Mount Speed ─────────────────────────────────────────
    /// 0=normal, 1=fast, 2=slow, >2=hybrid movement.
    MountSpeed { speed: u8 },

    // ── 0x002A — Change Race ─────────────────────────────────────────────
    ChangeRace(ChangeRace),

    // ── 0x002C C→S — Use Targeted Item ───────────────────────────────────
    UseTargetedItem { item_serial: u32, target_serial: u32 },

    // ── 0x002D C→S — Cast Targeted Spell ─────────────────────────────────
    CastTargetedSpell { spell_id: u16, target_serial: u32 },

    // ── 0x002E C→S — Use Targeted Skill ──────────────────────────────────
    UseTargetedSkill { skill_id: u16, target_serial: u32 },

    // ── 0x0032 C→S — Toggle Gargoyle Flying ──────────────────────────────
    ToggleGargoyleFlying { unk1: u32, unk2: u16 },
}

impl ManualPacket for GeneralInfo {
    const ID: u8 = 0xBF;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        let mut r = packet_reader(data, Self::ID, 5, true)?;
        let sub_cmd: u16 = Decode::decode(&mut r)?;

        match sub_cmd {
            // 0x0001 — FastWalkInit: 6 × u32 = 24 bytes payload
            0x0001 => {
                let mut keys = [0u32; 6];
                for k in &mut keys { *k = Decode::decode(&mut r)?; }
                Ok(Self::FastWalkInit { keys })
            }

            // 0x0002 — FastWalkAddKey: 4 bytes
            0x0002 => {
                let key: u32 = Decode::decode(&mut r)?;
                Ok(Self::FastWalkAddKey { key })
            }

            // 0x0004 — CloseGump: 4 + 4 bytes
            0x0004 => {
                let dialog_id: u32 = Decode::decode(&mut r)?;
                let button_id: u32 = Decode::decode(&mut r)?;
                Ok(Self::CloseGump { dialog_id, button_id })
            }

            // 0x0005 — ScreenSize
            0x0005 => {
                let unk1: u16 = Decode::decode(&mut r)?;
                let x: u16 = Decode::decode(&mut r)?;
                let y: u16 = Decode::decode(&mut r)?;
                let unk2: u16 = Decode::decode(&mut r)?;
                Ok(Self::ScreenSize { unk1, x, y, unk2 })
            }

            // 0x0006 — Party System
            0x0006 => {
                let sub_sub: u8 = Decode::decode(&mut r)?;
                decode_party(sub_sub, &mut r, data)
            }

            // 0x0008 — SetMap
            0x0008 => {
                let world: u8 = Decode::decode(&mut r)?;
                Ok(Self::SetMap { world })
            }

            // 0x000A — WrestlingStun: no payload
            0x000A => Ok(Self::WrestlingStun),

            // 0x000B — ClientLanguage
            0x000B => {
                let language: FixedString<4> = Decode::decode(&mut r)?;
                Ok(Self::ClientLanguage { language })
            }

            // 0x000C — ClosedStatusGump
            0x000C => {
                let character_id: u32 = Decode::decode(&mut r)?;
                Ok(Self::ClosedStatusGump { character_id })
            }

            // 0x000E — ClientAction
            0x000E => {
                let animation_id: u32 = Decode::decode(&mut r)?;
                Ok(Self::ClientAction { animation_id })
            }

            // 0x000F — ClientType
            0x000F => {
                let unk1: u8 = Decode::decode(&mut r)?;
                let flags: u32 = Decode::decode(&mut r)?;
                Ok(Self::ClientType { unk1, flags })
            }

            // 0x0010 — MegaClilocRelated
            0x0010 => {
                let item_id: u32 = Decode::decode(&mut r)?;
                let unknown: u32 = Decode::decode(&mut r)?;
                Ok(Self::MegaClilocRelated { item_id, unknown })
            }

            // 0x0013 — RequestPopupMenu
            0x0013 => {
                let character_id: u32 = Decode::decode(&mut r)?;
                Ok(Self::RequestPopupMenu { character_id })
            }

            // 0x0014 — DisplayPopupMenu
            0x0014 => {
                let _unk: u8 = Decode::decode(&mut r)?;
                let sub_sub: u8 = Decode::decode(&mut r)?;
                let serial: u32 = Decode::decode(&mut r)?;
                let num_entries: u8 = Decode::decode(&mut r)?;
                match sub_sub {
                    0x01 => {
                        let mut entries = Vec::with_capacity(num_entries as usize);
                        for _ in 0..num_entries {
                            let unique_id: u16 = Decode::decode(&mut r)?;
                            let cliloc_offset: u16 = Decode::decode(&mut r)?;
                            let flags: u16 = Decode::decode(&mut r)?;
                            let color = if flags & 0x20 != 0 {
                                Some(<u16 as Decode<BE>>::decode(&mut r)?)
                            } else {
                                None
                            };
                            entries.push(PopupEntry2D { unique_id, cliloc_offset, flags, color });
                        }
                        Ok(Self::DisplayPopupMenu(PopupEntries::TwoD { serial, entries }))
                    }
                    _ => {
                        // KR format
                        let mut entries = Vec::with_capacity(num_entries as usize);
                        for _ in 0..num_entries {
                            let text_id: u32 = Decode::decode(&mut r)?;
                            let index: u16 = Decode::decode(&mut r)?;
                            let flags: u16 = Decode::decode(&mut r)?;
                            entries.push(PopupEntryKR { text_id, index, flags });
                        }
                        Ok(Self::DisplayPopupMenu(PopupEntries::Kr { serial, entries }))
                    }
                }
            }

            // 0x0015 — PopupMenuSelection
            0x0015 => {
                let character_id: u32 = Decode::decode(&mut r)?;
                let entry_tag: u16 = Decode::decode(&mut r)?;
                Ok(Self::PopupMenuSelection { character_id, entry_tag })
            }

            // 0x0016 — CloseUiWindow
            0x0016 => {
                let window_raw: u32 = Decode::decode(&mut r)?;
                let serial: u32 = Decode::decode(&mut r)?;
                Ok(Self::CloseUiWindow { window: UiWindow::from_wire(window_raw), serial })
            }

            // 0x0017 — CodexOfWisdom
            0x0017 => {
                let unk: u8 = Decode::decode(&mut r)?;
                let msg_number: u32 = Decode::decode(&mut r)?;
                let presentation: u8 = Decode::decode(&mut r)?;
                Ok(Self::CodexOfWisdom { unk, msg_number, presentation })
            }

            // 0x0018 — EnableMapDiff
            0x0018 => {
                let num_maps: u32 = Decode::decode(&mut r)?;
                let mut maps = Vec::with_capacity(num_maps as usize);
                for _ in 0..num_maps {
                    let map_patches: u32 = Decode::decode(&mut r)?;
                    let static_patches: u32 = Decode::decode(&mut r)?;
                    maps.push(MapDiffEntry { map_patches, static_patches });
                }
                Ok(Self::EnableMapDiff { maps })
            }

            // 0x0019 — ExtendedStats
            0x0019 => {
                let sub_sub: u8 = Decode::decode(&mut r)?;
                let serial: u32 = Decode::decode(&mut r)?;
                let _unk: u8 = Decode::decode(&mut r)?;
                let lock_flags: u8 = Decode::decode(&mut r)?;
                let payload = if sub_sub == 0x05 {
                    let animation = if lock_flags == 0xFF {
                        let status: u8 = Decode::decode(&mut r)?;
                        let _unk2: u8 = Decode::decode(&mut r)?;
                        let animation: u8 = Decode::decode(&mut r)?;
                        let _unk3: u8 = Decode::decode(&mut r)?;
                        let frame: u8 = Decode::decode(&mut r)?;
                        Some(KrAnimationData { status, animation, frame })
                    } else {
                        // skip BYTE[1] + BYTE[4]
                        let _: u8 = Decode::decode(&mut r)?;
                        let _: u32 = Decode::decode(&mut r)?;
                        None
                    };
                    let extra = r.read_slice(r.remaining_len())
                        .unwrap_or(&[])
                        .to_vec();
                    ExtendedStatPayload::Kr { serial, lock_flags, animation, extra }
                } else {
                    ExtendedStatPayload::TwoD { serial, lock_flags }
                };
                Ok(Self::ExtendedStats(payload))
            }

            // 0x001A — StatLockChange
            0x001A => {
                let stat: u8 = Decode::decode(&mut r)?;
                let status: u8 = Decode::decode(&mut r)?;
                Ok(Self::StatLockChange { stat, status })
            }

            // 0x001B — NewSpellbook
            0x001B => {
                let _unk: u16 = Decode::decode(&mut r)?; // always 1
                let serial: u32 = Decode::decode(&mut r)?;
                let item_id: u16 = Decode::decode(&mut r)?;
                let scroll_offset: u16 = Decode::decode(&mut r)?;
                let raw = r.read_slice(8)?;
                let mut content = [0u8; 8];
                content.copy_from_slice(raw);
                Ok(Self::NewSpellbook(SpellbookData { serial, item_id, scroll_offset, content }))
            }

            // 0x001C — SpellSelected
            0x001C => {
                let unk: u16 = Decode::decode(&mut r)?;
                let spell_index: u16 = Decode::decode(&mut r)?;
                Ok(Self::SpellSelected { unk, spell_index })
            }

            // 0x001D — HouseRevisionState
            0x001D => {
                let house_serial: u32 = Decode::decode(&mut r)?;
                let revision: u32 = Decode::decode(&mut r)?;
                Ok(Self::HouseRevisionState { house_serial, revision })
            }

            // 0x001E — RequestHouseState
            0x001E => {
                let house_serial: u32 = Decode::decode(&mut r)?;
                Ok(Self::RequestHouseState { house_serial })
            }

            // 0x0020 — CustomHousing
            0x0020 => {
                let house_serial: u32 = Decode::decode(&mut r)?;
                let action_type: u8 = Decode::decode(&mut r)?;
                let action = match action_type {
                    0x01 => {
                        let graphic: u16 = Decode::decode(&mut r)?;
                        let x: u16 = Decode::decode(&mut r)?;
                        let y: u16 = Decode::decode(&mut r)?;
                        let z: u8 = Decode::decode(&mut r)?;
                        CustomHousingAction::Update(HousingTile { graphic, x, y, z })
                    }
                    0x04 => {
                        // skip 2+4+1 padding bytes
                        let _: u16 = Decode::decode(&mut r)?;
                        let _: u32 = Decode::decode(&mut r)?;
                        let _: u8 = Decode::decode(&mut r)?;
                        CustomHousingAction::Begin
                    }
                    _ => {
                        let _: u16 = Decode::decode(&mut r)?;
                        let _: u32 = Decode::decode(&mut r)?;
                        let _: u8 = Decode::decode(&mut r)?;
                        CustomHousingAction::End
                    }
                };
                Ok(Self::CustomHousing { house_serial, action })
            }

            // 0x0021 — AbilityIconConfirm: no payload (total 5 bytes)
            0x0021 => Ok(Self::AbilityIconConfirm),

            // 0x0022 — Damage
            0x0022 => {
                let _unk: u16 = Decode::decode(&mut r)?; // always 1
                let serial: u32 = Decode::decode(&mut r)?;
                let damage: u8 = Decode::decode(&mut r)?;
                Ok(Self::Damage { serial, damage })
            }

            // 0x0024 — UnknownUose
            0x0024 => {
                let unknown: u8 = Decode::decode(&mut r)?;
                Ok(Self::UnknownUose { unknown })
            }

            // 0x0025 — SeAbilityChange
            0x0025 => {
                let ability_id: u8 = Decode::decode(&mut r)?;
                let on_off: u8 = Decode::decode(&mut r)?;
                Ok(Self::SeAbilityChange { ability_id, enabled: on_off != 0 })
            }

            // 0x0026 — MountSpeed
            0x0026 => {
                let speed: u8 = Decode::decode(&mut r)?;
                Ok(Self::MountSpeed { speed })
            }

            // 0x002A — ChangeRace (direction determined by payload size)
            0x002A => {
                // S→C: 2 bytes (female + race)
                // C→S: 10 bytes (5 × u16)
                if r.remaining_len() == 2 {
                    let female: u8 = Decode::decode(&mut r)?;
                    let race: u8 = Decode::decode(&mut r)?;
                    Ok(Self::ChangeRace(ChangeRace::Request { female: female != 0, race }))
                } else {
                    let skin_color: u16 = Decode::decode(&mut r)?;
                    let hair_style: u16 = Decode::decode(&mut r)?;
                    let hair_color: u16 = Decode::decode(&mut r)?;
                    let beard_style: u16 = Decode::decode(&mut r)?;
                    let beard_color: u16 = Decode::decode(&mut r)?;
                    Ok(Self::ChangeRace(ChangeRace::Response {
                        skin_color, hair_style, hair_color, beard_style, beard_color,
                    }))
                }
            }

            // 0x002C — UseTargetedItem
            0x002C => {
                let item_serial: u32 = Decode::decode(&mut r)?;
                let target_serial: u32 = Decode::decode(&mut r)?;
                Ok(Self::UseTargetedItem { item_serial, target_serial })
            }

            // 0x002D — CastTargetedSpell
            0x002D => {
                let spell_id: u16 = Decode::decode(&mut r)?;
                let target_serial: u32 = Decode::decode(&mut r)?;
                Ok(Self::CastTargetedSpell { spell_id, target_serial })
            }

            // 0x002E — UseTargetedSkill
            0x002E => {
                let skill_id: u16 = Decode::decode(&mut r)?;
                let target_serial: u32 = Decode::decode(&mut r)?;
                Ok(Self::UseTargetedSkill { skill_id, target_serial })
            }

            // 0x0032 — ToggleGargoyleFlying
            0x0032 => {
                let unk1: u32 = Decode::decode(&mut r)?;
                let unk2: u16 = Decode::decode(&mut r)?;
                Ok(Self::ToggleGargoyleFlying { unk1, unk2 })
            }

            _ => {
                let payload = data[5..].to_vec();
                Ok(Self::Unknown { sub_cmd, data: payload })
            }
        }
    }
}

// ── Party decode helper ───────────────────────────────────────────────────

fn decode_party(
    sub_sub: u8,
    r: &mut u_io::BinaryReader<'_, BE>,
    _data: &[u8],
) -> Result<GeneralInfo, PacketError> {
    use GeneralInfo::Party;

    /// Decode null-terminated UTF-16 BE string.
    fn read_ustr(r: &mut u_io::BinaryReader<'_, BE>) -> Result<String, PacketError> {
        let mut units = Vec::new();
        loop {
            let u: u16 = Decode::decode(r)?;
            if u == 0 { break; }
            units.push(u);
        }
        Ok(String::from_utf16_lossy(&units).to_owned())
    }

    match sub_sub {
        // The client always sends a single u32; the server sends a list.
        // We detect by remaining length: if exactly 4 bytes remain → C→S.
        0x01 => {
            if r.remaining_len() == 4 {
                let id: u32 = Decode::decode(r)?;
                Ok(Party(PartyCommand::AddMemberRequest { id }))
            } else {
                let num: u8 = Decode::decode(r)?;
                let mut members = Vec::with_capacity(num as usize);
                for _ in 0..num {
                    members.push(<u32 as Decode<BE>>::decode(r)?);
                }
                Ok(Party(PartyCommand::MemberList { members }))
            }
        }
        0x02 => {
            if r.remaining_len() == 4 {
                let id: u32 = Decode::decode(r)?;
                Ok(Party(PartyCommand::RemoveMemberRequest { id }))
            } else {
                let num: u8 = Decode::decode(r)?;
                let removed_id: u32 = Decode::decode(r)?;
                let mut members = Vec::with_capacity(num as usize);
                for _ in 0..num {
                    members.push(<u32 as Decode<BE>>::decode(r)?);
                }
                Ok(Party(PartyCommand::MemberRemoved { removed_id, members }))
            }
        }
        0x03 => {
            let id: u32 = Decode::decode(r)?;
            let message = read_ustr(r)?;
            // Ambiguous: same wire for C→S (target) and S→C (source).
            // We decode as PrivateMessageFromMember (server convention).
            Ok(Party(PartyCommand::PrivateMessageFromMember { source_id: id, message }))
        }
        0x04 => {
            if r.remaining_len() > 0 {
                // Check if first bytes could be a u32 serial (S→C) or direct string (C→S)
                // C→S: starts directly with the null-terminated unicode string
                // S→C: starts with a u32 source id, then string
                // Heuristic: if remaining >= 4, assume S→C with source id
                if r.remaining_len() >= 4 {
                    let source_id: u32 = Decode::decode(r)?;
                    let message = read_ustr(r)?;
                    Ok(Party(PartyCommand::BroadcastMessageFromMember { source_id, message }))
                } else {
                    let message = read_ustr(r)?;
                    Ok(Party(PartyCommand::BroadcastMessage { message }))
                }
            } else {
                Ok(Party(PartyCommand::BroadcastMessage { message: String::new() }))
            }
        }
        0x06 => {
            let can_loot: u8 = Decode::decode(r)?;
            Ok(Party(PartyCommand::SetCanLoot { can_loot: can_loot != 0 }))
        }
        0x07 => {
            let leader_serial: u32 = Decode::decode(r)?;
            Ok(Party(PartyCommand::Invitation { leader_serial }))
        }
        0x08 => {
            let leader_serial: u32 = Decode::decode(r)?;
            Ok(Party(PartyCommand::AcceptInvitation { leader_serial }))
        }
        0x09 => {
            let leader_serial: u32 = Decode::decode(r)?;
            Ok(Party(PartyCommand::DeclineInvitation { leader_serial }))
        }
        _ => {
            // Unknown party sub-sub-command — fall through to Unknown
            let remaining = r.remaining_len();
            let raw = r.read_slice(remaining)?;
            Ok(GeneralInfo::Unknown {
                sub_cmd: 0x0006,
                data: std::iter::once(sub_sub).chain(raw.iter().copied()).collect(),
            })
        }
    }
}

// ── sub_cmd() helper ──────────────────────────────────────────────────────

impl GeneralInfo {
    /// Return the sub-command id.
    pub fn sub_cmd(&self) -> u16 {
        match self {
            Self::Unknown { sub_cmd, .. }        => *sub_cmd,
            Self::FastWalkInit { .. }            => 0x0001,
            Self::FastWalkAddKey { .. }          => 0x0002,
            Self::CloseGump { .. }              => 0x0004,
            Self::ScreenSize { .. }             => 0x0005,
            Self::Party(_)                       => 0x0006,
            Self::SetMap { .. }                 => 0x0008,
            Self::WrestlingStun                 => 0x000A,
            Self::ClientLanguage { .. }         => 0x000B,
            Self::ClosedStatusGump { .. }       => 0x000C,
            Self::ClientAction { .. }           => 0x000E,
            Self::ClientType { .. }             => 0x000F,
            Self::MegaClilocRelated { .. }      => 0x0010,
            Self::RequestPopupMenu { .. }       => 0x0013,
            Self::DisplayPopupMenu(_)            => 0x0014,
            Self::PopupMenuSelection { .. }     => 0x0015,
            Self::CloseUiWindow { .. }          => 0x0016,
            Self::CodexOfWisdom { .. }          => 0x0017,
            Self::EnableMapDiff { .. }          => 0x0018,
            Self::ExtendedStats(_)               => 0x0019,
            Self::StatLockChange { .. }         => 0x001A,
            Self::NewSpellbook(_)                => 0x001B,
            Self::SpellSelected { .. }          => 0x001C,
            Self::HouseRevisionState { .. }     => 0x001D,
            Self::RequestHouseState { .. }      => 0x001E,
            Self::CustomHousing { .. }          => 0x0020,
            Self::AbilityIconConfirm            => 0x0021,
            Self::Damage { .. }                 => 0x0022,
            Self::UnknownUose { .. }            => 0x0024,
            Self::SeAbilityChange { .. }        => 0x0025,
            Self::MountSpeed { .. }             => 0x0026,
            Self::ChangeRace(_)                  => 0x002A,
            Self::UseTargetedItem { .. }        => 0x002C,
            Self::CastTargetedSpell { .. }      => 0x002D,
            Self::UseTargetedSkill { .. }       => 0x002E,
            Self::ToggleGargoyleFlying { .. }   => 0x0032,
        }
    }
}

// ── Encode ────────────────────────────────────────────────────────────────

impl Encode<BE> for GeneralInfo {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder

        /// Write null-terminated UTF-16 BE string.
        fn write_ustr(w: &mut BinaryWriter<BE>, s: &str) {
            for unit in s.encode_utf16() { w.put_u16(unit); }
            w.put_u16(0);
        }

        match self {
            Self::Unknown { sub_cmd, data } => {
                w.put_u16(*sub_cmd);
                w.put_slice(data);
            }

            Self::FastWalkInit { keys } => {
                w.put_u16(0x0001);
                for k in keys { w.put_u32(*k); }
            }

            Self::FastWalkAddKey { key } => {
                w.put_u16(0x0002);
                w.put_u32(*key);
            }

            Self::CloseGump { dialog_id, button_id } => {
                w.put_u16(0x0004);
                w.put_u32(*dialog_id);
                w.put_u32(*button_id);
            }

            Self::ScreenSize { unk1, x, y, unk2 } => {
                w.put_u16(0x0005);
                w.put_u16(*unk1);
                w.put_u16(*x);
                w.put_u16(*y);
                w.put_u16(*unk2);
            }

            Self::Party(cmd) => {
                w.put_u16(0x0006);
                match cmd {
                    PartyCommand::AddMemberRequest { id } => {
                        w.put_u8(0x01); w.put_u32(*id);
                    }
                    PartyCommand::RemoveMemberRequest { id } => {
                        w.put_u8(0x02); w.put_u32(*id);
                    }
                    PartyCommand::PrivateMessageToMember { target_id, message } |
                    PartyCommand::PrivateMessageFromMember { source_id: target_id, message } => {
                        w.put_u8(0x03); w.put_u32(*target_id); write_ustr(w, message);
                    }
                    PartyCommand::BroadcastMessage { message } => {
                        w.put_u8(0x04); write_ustr(w, message);
                    }
                    PartyCommand::BroadcastMessageFromMember { source_id, message } => {
                        w.put_u8(0x04); w.put_u32(*source_id); write_ustr(w, message);
                    }
                    PartyCommand::SetCanLoot { can_loot } => {
                        w.put_u8(0x06); w.put_u8(if *can_loot { 1 } else { 0 });
                    }
                    PartyCommand::Invitation { leader_serial } => {
                        w.put_u8(0x07); w.put_u32(*leader_serial);
                    }
                    PartyCommand::AcceptInvitation { leader_serial } => {
                        w.put_u8(0x08); w.put_u32(*leader_serial);
                    }
                    PartyCommand::DeclineInvitation { leader_serial } => {
                        w.put_u8(0x09); w.put_u32(*leader_serial);
                    }
                    PartyCommand::MemberList { members } => {
                        w.put_u8(0x01);
                        w.put_u8(members.len() as u8);
                        for id in members { w.put_u32(*id); }
                    }
                    PartyCommand::MemberRemoved { removed_id, members } => {
                        w.put_u8(0x02);
                        w.put_u8(members.len() as u8);
                        w.put_u32(*removed_id);
                        for id in members { w.put_u32(*id); }
                    }
                }
            }

            Self::SetMap { world: hue } => {
                w.put_u16(0x0008);
                w.put_u8(*hue);
            }

            Self::WrestlingStun => { w.put_u16(0x000A); }

            Self::ClientLanguage { language } => {
                w.put_u16(0x000B);
                language.encode(w);
            }

            Self::ClosedStatusGump { character_id } => {
                w.put_u16(0x000C);
                w.put_u32(*character_id);
            }

            Self::ClientAction { animation_id } => {
                w.put_u16(0x000E);
                w.put_u32(*animation_id);
            }

            Self::ClientType { unk1, flags } => {
                w.put_u16(0x000F);
                w.put_u8(*unk1);
                w.put_u32(*flags);
            }

            Self::MegaClilocRelated { item_id, unknown } => {
                w.put_u16(0x0010);
                w.put_u32(*item_id);
                w.put_u32(*unknown);
            }

            Self::RequestPopupMenu { character_id } => {
                w.put_u16(0x0013);
                w.put_u32(*character_id);
            }

            Self::DisplayPopupMenu(entries) => {
                w.put_u16(0x0014);
                w.put_u8(0x00); // unknown
                match entries {
                    PopupEntries::TwoD { serial, entries } => {
                        w.put_u8(0x01);
                        w.put_u32(*serial);
                        w.put_u8(entries.len() as u8);
                        for e in entries {
                            w.put_u16(e.unique_id);
                            w.put_u16(e.cliloc_offset);
                            w.put_u16(e.flags);
                            if let Some(c) = e.color { w.put_u16(c); }
                        }
                    }
                    PopupEntries::Kr { serial, entries } => {
                        w.put_u8(0x02);
                        w.put_u32(*serial);
                        w.put_u8(entries.len() as u8);
                        for e in entries {
                            w.put_u32(e.text_id);
                            w.put_u16(e.index);
                            w.put_u16(e.flags);
                        }
                    }
                }
            }

            Self::PopupMenuSelection { character_id, entry_tag } => {
                w.put_u16(0x0015);
                w.put_u32(*character_id);
                w.put_u16(*entry_tag);
            }

            Self::CloseUiWindow { window, serial } => {
                w.put_u16(0x0016);
                w.put_u32(window.to_wire());
                w.put_u32(*serial);
            }

            Self::CodexOfWisdom { unk, msg_number, presentation } => {
                w.put_u16(0x0017);
                w.put_u8(*unk);
                w.put_u32(*msg_number);
                w.put_u8(*presentation);
            }

            Self::EnableMapDiff { maps } => {
                w.put_u16(0x0018);
                w.put_u32(maps.len() as u32);
                for entry in maps {
                    w.put_u32(entry.map_patches);
                    w.put_u32(entry.static_patches);
                }
            }

            Self::ExtendedStats(payload) => {
                w.put_u16(0x0019);
                match payload {
                    ExtendedStatPayload::TwoD { serial, lock_flags } => {
                        w.put_u8(0x02);
                        w.put_u32(*serial);
                        w.put_u8(0x00);
                        w.put_u8(*lock_flags);
                    }
                    ExtendedStatPayload::Kr { serial, lock_flags, animation, extra } => {
                        w.put_u8(0x05);
                        w.put_u32(*serial);
                        w.put_u8(0x00);
                        w.put_u8(*lock_flags);
                        if let Some(a) = animation {
                            w.put_u8(a.status);
                            w.put_u8(0x00);
                            w.put_u8(a.animation);
                            w.put_u8(0x00);
                            w.put_u8(a.frame);
                        } else {
                            w.put_u8(0x00);
                            w.put_u32(0x00000000);
                        }
                        if !extra.is_empty() {
                            w.put_slice(extra);
                        }
                    }
                }
            }

            Self::StatLockChange { stat, status } => {
                w.put_u16(0x001A);
                w.put_u8(*stat);
                w.put_u8(*status);
            }

            Self::NewSpellbook(sb) => {
                w.put_u16(0x001B);
                w.put_u16(0x0001); // always 1
                w.put_u32(sb.serial);
                w.put_u16(sb.item_id);
                w.put_u16(sb.scroll_offset);
                w.put_slice(&sb.content);
            }

            Self::SpellSelected { unk, spell_index } => {
                w.put_u16(0x001C);
                w.put_u16(*unk);
                w.put_u16(*spell_index);
            }

            Self::HouseRevisionState { house_serial, revision } => {
                w.put_u16(0x001D);
                w.put_u32(*house_serial);
                w.put_u32(*revision);
            }

            Self::RequestHouseState { house_serial } => {
                w.put_u16(0x001E);
                w.put_u32(*house_serial);
            }

            Self::CustomHousing { house_serial, action } => {
                w.put_u16(0x0020);
                w.put_u32(*house_serial);
                match action {
                    CustomHousingAction::Update(tile) => {
                        w.put_u8(0x01);
                        w.put_u16(tile.graphic);
                        w.put_u16(tile.x);
                        w.put_u16(tile.y);
                        w.put_u8(tile.z);
                    }
                    CustomHousingAction::Begin => {
                        w.put_u8(0x04);
                        w.put_u16(0x0000);
                        w.put_u32(0xFFFFFFFF);
                        w.put_u8(0xFF);
                    }
                    CustomHousingAction::End => {
                        w.put_u8(0x05);
                        w.put_u16(0x0000);
                        w.put_u32(0xFFFFFFFF);
                        w.put_u8(0xFF);
                    }
                }
            }

            Self::AbilityIconConfirm => { w.put_u16(0x0021); }

            Self::Damage { serial, damage } => {
                w.put_u16(0x0022);
                w.put_u16(0x0001); // always 1
                w.put_u32(*serial);
                w.put_u8(*damage);
            }

            Self::UnknownUose { unknown } => {
                w.put_u16(0x0024);
                w.put_u8(*unknown);
            }

            Self::SeAbilityChange { ability_id, enabled } => {
                w.put_u16(0x0025);
                w.put_u8(*ability_id);
                w.put_u8(if *enabled { 1 } else { 0 });
            }

            Self::MountSpeed { speed } => {
                w.put_u16(0x0026);
                w.put_u8(*speed);
            }

            Self::ChangeRace(cr) => {
                w.put_u16(0x002A);
                match cr {
                    ChangeRace::Request { female, race } => {
                        w.put_u8(if *female { 1 } else { 0 });
                        w.put_u8(*race);
                    }
                    ChangeRace::Response { skin_color, hair_style, hair_color, beard_style, beard_color } => {
                        w.put_u16(*skin_color);
                        w.put_u16(*hair_style);
                        w.put_u16(*hair_color);
                        w.put_u16(*beard_style);
                        w.put_u16(*beard_color);
                    }
                }
            }

            Self::UseTargetedItem { item_serial, target_serial } => {
                w.put_u16(0x002C);
                w.put_u32(*item_serial);
                w.put_u32(*target_serial);
            }

            Self::CastTargetedSpell { spell_id, target_serial } => {
                w.put_u16(0x002D);
                w.put_u16(*spell_id);
                w.put_u32(*target_serial);
            }

            Self::UseTargetedSkill { skill_id, target_serial } => {
                w.put_u16(0x002E);
                w.put_u16(*skill_id);
                w.put_u32(*target_serial);
            }

            Self::ToggleGargoyleFlying { unk1, unk2 } => {
                w.put_u16(0x0032);
                w.put_u32(*unk1);
                w.put_u16(*unk2);
            }
        }
    }
}

impl std::fmt::Display for GeneralInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GeneralInfo(sub=0x{:04X})", self.sub_cmd())
    }
}

// ── 0x4E PersonalLightLevel (6 bytes, fixed, S→C) ─────────────────────────

/// Packet 0x4E — Personal Light Level (6 bytes, fixed, S→C)
///
/// Sets the personal light level for a specific creature/player.
/// This overrides the global light level set by [`OverallLightLevel`]
/// (0x4F) for that creature on the client.
///
/// # Light level values
///
/// `0x00` = fully lit (daytime), higher values = progressively darker.
/// The maximum useful value is `0x1F` (pitch black).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x4E, size = fixed(6), endian = "be")]
pub struct PersonalLightLevel {
    pub id: u8,
    /// Serial of the creature whose personal light level is being set.
    pub serial: u32,
    /// Light level: `0x00` = fully lit, `0x1F` = pitch black.
    pub level: u8,
}

impl PersonalLightLevel {
    /// Create a new personal light level packet.
    pub fn new(serial: u32, level: u8) -> Self {
        Self { id: Self::ID, serial, level }
    }
}

// ── 0xC8 ClientViewRange (2 bytes, fixed, bidirectional) ──────────────────

/// Packet 0xC8 — Client View Range (2 bytes, fixed, bidirectional)
///
/// Controls how far the client can see items and NPCs.  Active since
/// client 3.0.8o: the client sends this packet when the player uses the
/// increase/decrease view range macro, and the server must **echo the
/// packet back** for the change to take effect on the client.
///
/// # Valid range
///
/// Minimum: `5`, maximum: `18`.  Values outside this range are clamped
/// by the client.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xC8, size = fixed(2), endian = "be")]
pub struct ClientViewRange {
    pub id: u8,
    /// Requested view range in tiles (`5`–`18`).
    pub range: u8,
}

impl ClientViewRange {
    /// Minimum valid view range.
    pub const MIN: u8 = 5;
    /// Maximum valid view range.
    ///
    /// Classic clients support up to 18; modern clients (Enhanced Client,
    /// ClassicUO) may negotiate up to 24 via a C→S 0xC8 exchange after
    /// login.
    pub const MAX: u8 = 24;
    /// Default view range used when no negotiation has taken place.
    pub const DEFAULT: u8 = 18;

    /// Create a new view range packet, clamping to the valid range.
    pub fn new(range: u8) -> Self {
        Self { id: Self::ID, range: range.clamp(Self::MIN, Self::MAX) }
    }

    /// Create without clamping — use when relaying the exact client value.
    pub fn raw(range: u8) -> Self {
        Self { id: Self::ID, range }
    }
}

// ── 0x5B Time (4 bytes, fixed, S→C) ───────────────────────────────────────

/// Packet 0x5B — Time (4 bytes, fixed, S→C)
///
/// Sent by the server to synchronise the in-game clock displayed in the
/// client UI.  Hours are in 24-hour format.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0x5B, size = fixed(4), endian = "be")]
pub struct Time {
    pub id: u8,
    /// Hour of the day (0–23).
    pub hour: u8,
    /// Minute of the hour (0–59).
    pub minute: u8,
    /// Second of the minute (0–59).
    pub second: u8,
}

impl Time {
    /// Create a new time packet.
    pub fn new(hour: u8, minute: u8, second: u8) -> Self {
        Self { id: Self::ID, hour, minute, second }
    }
}

// ── 0xA5 OpenWebBrowser (dynamic, S→C) ────────────────────────────────────

/// Packet 0xA5 — Open Web Browser (dynamic, S→C)
///
/// Instructs the client to open the given URL in a web browser.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]        0xA5
/// BYTE[2]        blockSize (total packet length)
/// BYTE[n]        url — null-terminated ASCII/UTF-8 string
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xA5, size = dynamic, endian = "be")]
pub struct OpenWebBrowser {
    pub id: u8,
    pub len: u16,
    /// Null-terminated URL to open, e.g. `"https://example.com"`.
    pub url: NullString,
}

impl OpenWebBrowser {
    /// Create an `OpenWebBrowser` packet for the given URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self { id: Self::ID, len: 0, url: NullString::new(url.into()) }
    }
}
