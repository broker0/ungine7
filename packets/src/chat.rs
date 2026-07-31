//! Chat system packets (0xB2 server→client, 0xB3 client→server, 0xB5 client→server).
//!
//! The UO chat system uses a single packet ID with a `message_type` / `cmd_type`
//! discriminator that selects the wire layout of the payload.  All Unicode
//! strings are big-endian UTF-16, null-terminated (`NullUnicodeString`).
//!
//! # 0xB2 ChatMessage — message type groups
//!
//! | Range            | Variant                | Description                        |
//! |------------------|------------------------|------------------------------------|
//! | 0x0001 – 0x0024  | [`ChatMessage::SystemNotice`] | System status/error messages  |
//! | 0x0025           | [`ChatMessage::Message`]      | Regular chat message          |
//! | 0x0026           | [`ChatMessage::Emote`]        | Emote message                 |
//! | 0x0027           | [`ChatMessage::Ooc`]          | Out-of-character message      |
//! | 0x0028 – 0x002C  | [`ChatMessage::SystemNotice`] | More system status messages   |
//! | 0x03E8           | [`ChatMessage::CreateConference`]   | Create a chat channel   |
//! | 0x03E9           | [`ChatMessage::DestroyConference`]  | Destroy a chat channel  |
//! | 0x03EB           | [`ChatMessage::DisplayUsernameWindow`] | Show username entry  |
//! | 0x03EC           | [`ChatMessage::CloseChat`]          | Close the chat window   |
//! | 0x03ED           | [`ChatMessage::UsernameAccepted`]   | Enter chat with username|
//! | 0x03EE           | [`ChatMessage::AddUser`]            | User joined channel     |
//! | 0x03EF           | [`ChatMessage::RemoveUser`]         | User left channel       |
//! | 0x03F0           | [`ChatMessage::ClearAllPlayers`]    | Clear player list       |
//! | 0x03F1           | [`ChatMessage::JoinedConference`]   | Joined a conference     |

use u_io::{BE, BinaryWriter, Decode, Encode, FixedString, NullUnicodeString, BasicPacket, packet_reader};
use macros::{Packet, WireEnum};

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── ChatSpeakerType ────────────────────────────────────────────────────────

/// Speaker/sender type for chat messages (0x0025, 0x0026, 0x0027).
///
/// | Wire value | Meaning   |
/// |------------|-----------|
/// | 0x0030     | User      |
/// | 0x0031     | Moderator |
/// | 0x0032     | Muted     |
/// | 0x0034     | Me (self) |
/// | 0x0035     | System    |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u16)]
pub enum ChatSpeakerType {
    #[wire_enum(0x0030, "User")]
    User,
    #[wire_enum(0x0031, "Moderator")]
    Moderator,
    #[wire_enum(0x0032, "Muted")]
    Muted,
    #[wire_enum(0x0034, "Me")]
    Me,
    #[wire_enum(0x0035, "System")]
    System,
    #[wire_enum(unknown)]
    Unknown(u16),
}

// ── ChatPasswordSetting ────────────────────────────────────────────────────

/// Password requirement for a conference (0x03E8 CreateConference).
///
/// | Wire value | Meaning           |
/// |------------|-------------------|
/// | 0x0030     | No password       |
/// | 0x0031     | Password required |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u16)]
pub enum ChatPasswordSetting {
    /// Conference has no password.
    #[wire_enum(0x0030, "no password")]
    NoPassword,
    /// Conference requires a password to join.
    #[wire_enum(0x0031, "password required")]
    PasswordRequired,
    #[wire_enum(unknown)]
    Unknown(u16),
}

// ── ChatUserType ───────────────────────────────────────────────────────────

/// User role type for AddUser (0x03EE).
///
/// | Wire value | Meaning   |
/// |------------|-----------|
/// | 0x0030     | User      |
/// | 0x0031     | Moderator |
/// | 0x0032     | Muted     |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u16)]
pub enum ChatUserType {
    #[wire_enum(0x0030, "User")]
    User,
    #[wire_enum(0x0031, "Moderator")]
    Moderator,
    #[wire_enum(0x0032, "Muted")]
    Muted,
    #[wire_enum(unknown)]
    Unknown(u16),
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Decode a null-terminated UTF-16 BE string from the reader.
///
/// Reads `u16` code units until `0x0000`, then converts to `String`.
fn decode_ustr<R: u_io::ReadPrimitives<BE>>(
    r: &mut R,
) -> Result<String, PacketError> {
    let s: NullUnicodeString = Decode::decode(r)?;
    Ok(s.0)
}

/// Encode a `String` as null-terminated UTF-16 BE into `w`.
fn encode_ustr(s: &str, w: &mut BinaryWriter<BE>) {
    let nus = NullUnicodeString(s.to_owned());
    nus.encode(w);
}

// ── 0xB2 ChatMessage (dynamic, S→C) ───────────────────────────────────────

/// Packet 0xB2 — Chat Message (dynamic, S→C)
///
/// All Unicode strings are big-endian UTF-16, null-terminated.
///
/// System notice message types (0x0001–0x0024, 0x0028–0x002C) come directly
/// from the `Chat.enu` localisation file.  They share a common wire layout
/// but differ in which `%1` / `%2` argument placeholders are present.  Both
/// arguments are decoded unconditionally — a missing argument simply produces
/// an empty string.
///
/// See the module-level documentation for the full type table.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ChatMessage {
    // ── System notices 0x0001 – 0x0024 and 0x0028 – 0x002C ───────────────
    //
    /// A system status or error notice from `Chat.enu`.
    ///
    /// The `msg_type` identifies which string to look up.  `arg1` and `arg2`
    /// replace `%1` and `%2` in the localised string; they are empty strings
    /// when the notice has no corresponding placeholder.
    ///
    /// # Message type reference
    ///
    /// | `msg_type` | `Chat.enu` text (abbreviated)                            |
    /// |------------|----------------------------------------------------------|
    /// | 0x0001     | You are already ignoring the maximum number of people.   |
    /// | 0x0002     | You are already ignoring %1.                             |
    /// | 0x0003     | You are now ignoring %1.                                 |
    /// | 0x0004     | You are no longer ignoring %1.                           |
    /// | 0x0005     | You are not ignoring %1.                                 |
    /// | 0x0006     | You are no longer ignoring anyone.                       |
    /// | 0x0007     | That is not a valid conference name.                     |
    /// | 0x0008     | There is already a conference of that name.              |
    /// | 0x0009     | You must have operator status to do this.                |
    /// | 0x000A     | Conference %1 renamed to %2.                             |
    /// | 0x000B     | You must be in a conference to do this.                  |
    /// | 0x000C     | There is no player named '%1'.                           |
    /// | 0x000D     | There is no conference named '%1'.                       |
    /// | 0x000E     | That is not the correct password.                        |
    /// | 0x000F     | %1 has chosen to ignore you.                             |
    /// | 0x0010     | The moderator has not given you speaking privileges.     |
    /// | 0x0011     | You can now receive private messages.                    |
    /// | 0x0012     | You will no longer receive private messages.             |
    /// | 0x0013     | You are now showing your character name.                 |
    /// | 0x0014     | You are no longer showing your character name.           |
    /// | 0x0015     | %1 is remaining anonymous.                               |
    /// | 0x0016     | %1 has chosen to not receive private messages.           |
    /// | 0x0017     | %1 is known in the lands of Britannia as %2.            |
    /// | 0x0018     | %1 has been kicked out of the conference.                |
    /// | 0x0019     | %1, a conference moderator, has kicked you out.          |
    /// | 0x001A     | You are already in the conference '%1'.                  |
    /// | 0x001B     | %1 is no longer a conference moderator.                  |
    /// | 0x001C     | %1 is now a conference moderator.                        |
    /// | 0x001D     | %1 has removed you from the list of moderators.          |
    /// | 0x001E     | %1 has made you a conference moderator.                  |
    /// | 0x001F     | %1 no longer has speaking privileges.                    |
    /// | 0x0020     | %1 now has speaking privileges.                          |
    /// | 0x0021     | %1, a moderator, has removed your speaking privileges.   |
    /// | 0x0022     | %1, a moderator, has granted you speaking privileges.    |
    /// | 0x0023     | Everyone in the conference has speaking privileges.      |
    /// | 0x0024     | Only moderators have speaking privileges by default.     |
    /// | 0x0028     | The password to the conference has been changed.         |
    /// | 0x0029     | Conference named '%1' is full.                           |
    /// | 0x002A     | You are banning %1 from this conference.                 |
    /// | 0x002B     | %1, a moderator, has banned you from the conference.     |
    /// | 0x002C     | You have been banned from this conference.               |
    SystemNotice {
        /// Raw message type number (`0x0001`–`0x0024` or `0x0028`–`0x002C`).
        msg_type: u16,
        /// Value substituted for `%1` in the localised string.  Empty when
        /// the message type has no `%1` placeholder.
        arg1: String,
        /// Value substituted for `%2` in the localised string.  Empty when
        /// the message type has no `%2` placeholder.
        arg2: String,
    },

    // ── Chat text messages 0x0025, 0x0026, 0x0027 ────────────────────────

    /// MessageType 0x0025 — Regular chat message from a player.
    Message {
        /// 3-letter ISO language code, e.g. `"ENU"`.
        language: FixedString<4>,
        /// How the sender appears (user, moderator, muted, self, system).
        speaker_type: ChatSpeakerType,
        /// Display name of the sender.
        username: String,
        /// The message text.
        text: String,
    },

    /// MessageType 0x0026 — Emote from a player.
    Emote {
        language: FixedString<4>,
        speaker_type: ChatSpeakerType,
        username: String,
        text: String,
    },

    /// MessageType 0x0027 — Out-of-character message from a player.
    Ooc {
        language: FixedString<4>,
        speaker_type: ChatSpeakerType,
        username: String,
        text: String,
    },

    // ── Conference management 0x03E8 – 0x03F1 ────────────────────────────

    /// MessageType 0x03E8 — Create a new conference / channel.
    CreateConference {
        /// Name of the new conference.
        channel_name: String,
        /// Whether the conference requires a password to join.
        password_setting: ChatPasswordSetting,
    },

    /// MessageType 0x03E9 — Destroy (close) a conference.
    DestroyConference {
        /// Name of the conference being destroyed.
        channel_name: String,
    },

    /// MessageType 0x03EB — Display the "enter username" dialog.
    DisplayUsernameWindow,

    /// MessageType 0x03EC — Close the chat window.
    CloseChat,

    /// MessageType 0x03ED — Username accepted; enter the chat with this name.
    UsernameAccepted {
        /// The accepted username.
        username: String,
    },

    /// MessageType 0x03EE — A user has joined the current channel.
    AddUser {
        /// Role of the joining user.
        user_type: ChatUserType,
        /// Username of the joining player.
        username: String,
    },

    /// MessageType 0x03EF — A user has left the current channel.
    RemoveUser {
        /// Username of the departing player.
        username: String,
    },

    /// MessageType 0x03F0 — Clear the entire player list in the chat window.
    ClearAllPlayers,

    /// MessageType 0x03F1 — You have joined the named conference.
    JoinedConference {
        /// Name of the conference joined.
        conference_name: String,
    },

    /// Unrecognised message type — raw payload preserved for forward compat.
    Unknown {
        msg_type: u16,
        data: Vec<u8>,
    },
}

impl ManualPacket for ChatMessage {
    const ID: u8 = 0xB2;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) + msg_type(2) = 5 bytes
        let mut r = packet_reader(data, Self::ID, 5, true)?;

        let msg_type: u16 = Decode::decode(&mut r)?;

        // Helper range checks
        let is_system_notice = matches!(msg_type, 0x0001..=0x0024 | 0x0028..=0x002C);

        if is_system_notice {
            return Self::decode_system_notice(msg_type, &mut r);
        }

        match msg_type {
            0x0025 => Self::decode_chat_text(msg_type, &mut r),
            0x0026 => Self::decode_chat_text(msg_type, &mut r),
            0x0027 => Self::decode_chat_text(msg_type, &mut r),
            0x03E8 => Self::decode_create_conference(&mut r),
            0x03E9 => Self::decode_destroy_conference(&mut r),
            0x03EB => Self::decode_fixed8_only(data, msg_type),
            0x03EC => Self::decode_fixed8_only(data, msg_type),
            0x03ED => Self::decode_username_accepted(&mut r),
            0x03EE => Self::decode_add_user(&mut r),
            0x03EF => Self::decode_remove_user(&mut r),
            0x03F0 => Self::decode_fixed8_only(data, msg_type),
            0x03F1 => Self::decode_joined_conference(&mut r),
            _ => {
                // Preserve unknown payload (everything after msg_type)
                let remaining = r.remaining_len();
                let raw = r.read_slice(remaining)?;
                Ok(Self::Unknown { msg_type, data: raw.to_vec() })
            }
        }
    }
}

// ── Decoding helpers ───────────────────────────────────────────────────────

impl ChatMessage {
    fn decode_system_notice(
        msg_type: u16,
        r: &mut u_io::BinaryReader<'_, BE>,
    ) -> Result<Self, PacketError> {
        // BYTE[4] unknown (always 00000000)
        let _unknown: u32 = Decode::decode(r)?;

        // arg1 and arg2 are both null-terminated UTF-16 BE strings.
        // Remaining bytes after the 4 unknown = both strings concatenated.
        // We always read two strings; if only one is present the second will
        // be an empty string (immediate null terminator).
        let arg1 = decode_ustr(r)?;
        // After arg1's null terminator: if more data remains, read arg2.
        let arg2 = if r.remaining_len() >= 2 {
            decode_ustr(r)?
        } else {
            String::new()
        };

        Ok(Self::SystemNotice { msg_type, arg1, arg2 })
    }

    fn decode_chat_text(
        msg_type: u16,
        r: &mut u_io::BinaryReader<'_, BE>,
    ) -> Result<Self, PacketError> {
        // BYTE[3] language code + BYTE[1] null = FixedString<4>
        let language: FixedString<4> = Decode::decode(r)?;
        // BYTE[2] speaker type
        let speaker_raw: u16 = Decode::decode(r)?;
        let speaker_type = ChatSpeakerType::from_wire(speaker_raw);
        // BYTE[?] username (null-terminated UTF-16 BE)
        let username = decode_ustr(r)?;
        // BYTE[?] message text (null-terminated UTF-16 BE)
        let text = decode_ustr(r)?;

        Ok(match msg_type {
            0x0025 => Self::Message { language, speaker_type, username, text },
            0x0026 => Self::Emote { language, speaker_type, username, text },
            _ => Self::Ooc { language, speaker_type, username, text },
        })
    }

    fn decode_create_conference(
        r: &mut u_io::BinaryReader<'_, BE>,
    ) -> Result<Self, PacketError> {
        let _unknown: u32 = Decode::decode(r)?;
        let channel_name = decode_ustr(r)?;
        let pw_raw: u16 = Decode::decode(r)?;
        let password_setting = ChatPasswordSetting::from_wire(pw_raw);
        // BYTE[2] null terminator for packet — consumed by the next read or ignored
        Ok(Self::CreateConference { channel_name, password_setting })
    }

    fn decode_destroy_conference(
        r: &mut u_io::BinaryReader<'_, BE>,
    ) -> Result<Self, PacketError> {
        let _unknown: u32 = Decode::decode(r)?;
        let channel_name = decode_ustr(r)?;
        // BYTE[4] unknown at end — ignored
        Ok(Self::DestroyConference { channel_name })
    }

    /// For message types whose payload is just 8 unknown bytes.
    fn decode_fixed8_only(_data: &[u8], msg_type: u16) -> Result<Self, PacketError> {
        Ok(match msg_type {
            0x03EB => Self::DisplayUsernameWindow,
            0x03EC => Self::CloseChat,
            _ => Self::ClearAllPlayers,
        })
    }

    fn decode_username_accepted(
        r: &mut u_io::BinaryReader<'_, BE>,
    ) -> Result<Self, PacketError> {
        let _unknown: u32 = Decode::decode(r)?;
        let username = decode_ustr(r)?;
        // BYTE[4] null terminator at end — ignored
        Ok(Self::UsernameAccepted { username })
    }

    fn decode_add_user(
        r: &mut u_io::BinaryReader<'_, BE>,
    ) -> Result<Self, PacketError> {
        let _unknown: u32 = Decode::decode(r)?;
        let type_raw: u16 = Decode::decode(r)?;
        let user_type = ChatUserType::from_wire(type_raw);
        let username = decode_ustr(r)?;
        Ok(Self::AddUser { user_type, username })
    }

    fn decode_remove_user(
        r: &mut u_io::BinaryReader<'_, BE>,
    ) -> Result<Self, PacketError> {
        let _unknown: u32 = Decode::decode(r)?;
        let username = decode_ustr(r)?;
        Ok(Self::RemoveUser { username })
    }

    fn decode_joined_conference(
        r: &mut u_io::BinaryReader<'_, BE>,
    ) -> Result<Self, PacketError> {
        let _unknown: u32 = Decode::decode(r)?;
        let conference_name = decode_ustr(r)?;
        // BYTE[4] unknown at end — ignored
        Ok(Self::JoinedConference { conference_name })
    }
}

// ── Encode ─────────────────────────────────────────────────────────────────

impl Encode<BE> for ChatMessage {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()

        match self {
            Self::SystemNotice { msg_type, arg1, arg2 } => {
                w.put_u16(*msg_type);
                w.put_u32(0x00000000); // unknown
                encode_ustr(arg1, w);
                encode_ustr(arg2, w);
            }

            Self::Message { language, speaker_type, username, text }
            | Self::Emote { language, speaker_type, username, text }
            | Self::Ooc { language, speaker_type, username, text } => {
                let msg_type = match self {
                    Self::Message { .. } => 0x0025u16,
                    Self::Emote { .. } => 0x0026u16,
                    _ => 0x0027u16,
                };
                w.put_u16(msg_type);
                language.encode(w);
                w.put_u16(speaker_type.to_wire());
                encode_ustr(username, w);
                encode_ustr(text, w);
            }

            Self::CreateConference { channel_name, password_setting } => {
                w.put_u16(0x03E8);
                w.put_u32(0x00000000);
                encode_ustr(channel_name, w);
                w.put_u16(password_setting.to_wire());
                w.put_u16(0x0000); // packet null terminator
            }

            Self::DestroyConference { channel_name } => {
                w.put_u16(0x03E9);
                w.put_u32(0x00000000);
                encode_ustr(channel_name, w);
                w.put_u32(0x00000000); // trailing unknown
            }

            Self::DisplayUsernameWindow => {
                w.put_u16(0x03EB);
                w.put_u64(0x0000000000000000);
            }

            Self::CloseChat => {
                w.put_u16(0x03EC);
                w.put_u64(0x0000000000000000);
            }

            Self::UsernameAccepted { username } => {
                w.put_u16(0x03ED);
                w.put_u32(0x00000000);
                encode_ustr(username, w);
                w.put_u32(0x00000000); // trailing null terminator
            }

            Self::AddUser { user_type, username } => {
                w.put_u16(0x03EE);
                w.put_u32(0x00000000);
                w.put_u16(user_type.to_wire());
                encode_ustr(username, w);
            }

            Self::RemoveUser { username } => {
                w.put_u16(0x03EF);
                w.put_u32(0x00000000);
                encode_ustr(username, w);
            }

            Self::ClearAllPlayers => {
                w.put_u16(0x03F0);
                w.put_u64(0x0000000000000000);
            }

            Self::JoinedConference { conference_name } => {
                w.put_u16(0x03F1);
                w.put_u32(0x00000000);
                encode_ustr(conference_name, w);
                w.put_u32(0x00000000); // trailing unknown
            }

            Self::Unknown { msg_type, data } => {
                w.put_u16(*msg_type);
                w.put_slice(data);
            }
        }
    }
}

// ── 0xB3 ChatText (dynamic, C→S) ──────────────────────────────────────────

/// Packet 0xB3 — Chat Text (dynamic, C→S)
///
/// Sent by the client for all chat system interactions: sending messages,
/// conference management, moderator actions, and preference toggles.
///
/// All Unicode strings are big-endian UTF-16, null-terminated.
/// The `language` field is a 4-byte raw big-endian value (e.g. `0x454E5500`
/// for `"ENU\0"`).
///
/// # Command type reference
///
/// | `cmd_type` | Variant                    | Payload                          |
/// |------------|----------------------------|----------------------------------|
/// | 0x41       | `ChangePassword`           | Unicode password                 |
/// | 0x58       | `Close`                    | (empty)                          |
/// | 0x61       | `Message`                  | Unicode message text             |
/// | 0x62       | `JoinConference`           | name + optional password         |
/// | 0x63       | `CreateConference`         | name + optional password         |
/// | 0x64       | `RenameConference`         | Unicode new name                 |
/// | 0x65       | `PrivateMessage`           | Unicode target name              |
/// | 0x66       | `Ignore`                   | Unicode target name              |
/// | 0x67       | `StopIgnoring`             | Unicode target name              |
/// | 0x68       | `ToggleIgnore`             | Unicode target name              |
/// | 0x69       | `GrantVoice`               | Unicode target name              |
/// | 0x6A       | `RemoveVoice`              | Unicode target name              |
/// | 0x6B       | `ToggleVoice`              | Unicode target name              |
/// | 0x6C       | `GrantModerator`           | Unicode target name              |
/// | 0x6D       | `RemoveModerator`          | Unicode target name              |
/// | 0x6E       | `ToggleModerator`          | Unicode target name              |
/// | 0x6F       | `BlockPrivateMessages`     | (empty)                          |
/// | 0x70       | `ReceivePrivateMessages`   | (empty)                          |
/// | 0x71       | `TogglePrivateMessages`    | (empty)                          |
/// | 0x72       | `ShowCharacterName`        | (empty)                          |
/// | 0x73       | `HideCharacterName`        | (empty)                          |
/// | 0x74       | `ToggleCharacterName`      | (empty)                          |
/// | 0x75       | `Whois`                    | Unicode target name              |
/// | 0x76       | `Kick`                     | Unicode target name              |
/// | 0x77       | `ModeratorSpeakOnly`       | (empty)                          |
/// | 0x78       | `AllCanSpeak`              | (empty)                          |
/// | 0x79       | `ToggleSpeakPrivileges`    | (empty)                          |
/// | 0x7A       | `Emote`                    | Unicode emote text               |
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ChatText {
    /// 0x41 — Change conference password.
    ChangePassword { language: u32, password: String },

    /// 0x58 — Close the chat window.
    Close { language: u32 },

    /// 0x61 — Send a chat message.
    Message { language: u32, text: String },

    /// 0x62 — Join an existing conference.
    ///
    /// The conference name is surrounded by `"` holders on the wire; the
    /// optional password follows a space holder.
    JoinConference {
        language: u32,
        /// Name of the conference to join.
        name: String,
        /// Password, if the conference is password-protected.
        password: String,
    },

    /// 0x63 — Create a new conference.
    ///
    /// If `password` is non-empty it is enclosed in `{` `}` holders on the
    /// wire.
    CreateConference {
        language: u32,
        /// Name for the new conference.
        name: String,
        /// Optional password for the conference.
        password: String,
    },

    /// 0x64 — Rename the current conference (`/rename <name>`).
    RenameConference { language: u32, name: String },

    /// 0x65 — Send a private message to a player (`/msg <name>`).
    PrivateMessage { language: u32, name: String },

    /// 0x66 — Ignore a player (`+ignore <name>`).
    Ignore { language: u32, name: String },

    /// 0x67 — Stop ignoring a player (`-ignore <name>`).
    StopIgnoring { language: u32, name: String },

    /// 0x68 — Toggle ignore for a player (`/ignore <name>`).
    ToggleIgnore { language: u32, name: String },

    /// 0x69 — Grant speaking privileges to a player (`+voice <name>`).
    GrantVoice { language: u32, name: String },

    /// 0x6A — Remove speaking privileges from a player (`-voice <name>`).
    RemoveVoice { language: u32, name: String },

    /// 0x6B — Toggle speaking privileges for a player (`/voice <name>`).
    ToggleVoice { language: u32, name: String },

    /// 0x6C — Grant moderator status to a player (`+ops <name>`).
    GrantModerator { language: u32, name: String },

    /// 0x6D — Remove moderator status from a player (`-ops <name>`).
    RemoveModerator { language: u32, name: String },

    /// 0x6E — Toggle moderator status for a player (`/ops <name>`).
    ToggleModerator { language: u32, name: String },

    /// 0x6F — Stop receiving private messages.
    BlockPrivateMessages { language: u32 },

    /// 0x70 — Start receiving private messages.
    ReceivePrivateMessages { language: u32 },

    /// 0x71 — Toggle receiving private messages.
    TogglePrivateMessages { language: u32 },

    /// 0x72 — Show own character name to others.
    ShowCharacterName { language: u32 },

    /// 0x73 — Hide own character name from others.
    HideCharacterName { language: u32 },

    /// 0x74 — Toggle showing own character name.
    ToggleCharacterName { language: u32 },

    /// 0x75 — Query info on a player (`/whois <name>`).
    Whois { language: u32, name: String },

    /// 0x76 — Kick a player from the conference (`/kick <name>`).
    Kick { language: u32, name: String },

    /// 0x77 — Set default: only moderators can speak.
    ModeratorSpeakOnly { language: u32 },

    /// 0x78 — Set default: everyone can speak.
    AllCanSpeak { language: u32 },

    /// 0x79 — Toggle default speaking privileges.
    ToggleSpeakPrivileges { language: u32 },

    /// 0x7A — Send an emote (`/emote` or `/em <text>`).
    Emote { language: u32, text: String },

    /// Unrecognised command type — raw payload preserved for forward compat.
    Unknown { language: u32, cmd_type: u16, data: Vec<u8> },
}

impl ManualPacket for ChatText {
    const ID: u8 = 0xB3;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1) + len(2) + language(4) + type(2) = 9 bytes
        let mut r = packet_reader(data, Self::ID, 9, true)?;

        let language: u32 = Decode::decode(&mut r)?;
        let cmd_type: u16 = Decode::decode(&mut r)?;

        match cmd_type {
            0x41 => {
                let password = decode_ustr(&mut r)?;
                Ok(Self::ChangePassword { language, password })
            }
            0x58 => Ok(Self::Close { language }),
            0x61 => {
                let text = decode_ustr(&mut r)?;
                Ok(Self::Message { language, text })
            }
            0x62 => {
                // Wire (with password):    0x0022 <name> 0x0022 0x0020 <password> 0x0000
                // Wire (without password): 0x0022 <name> 0x0022 0x0000
                let _open_quote: u16 = Decode::decode(&mut r)?;
                let name = decode_ustr_until(&mut r, 0x0022)?; // read until closing "
                // Peek at the next u16: 0x0020 = space (password follows),
                // 0x0000 = end of packet (no password), anything else treated as end.
                let password = if r.remaining_len() >= 4 {
                    let sep: u16 = Decode::decode(&mut r)?;
                    if sep == 0x0020 {
                        decode_ustr(&mut r)?
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                Ok(Self::JoinConference { language, name, password })
            }
            0x63 => {
                // <name> [{<password>}] \0
                // If password present: name ends at 0x007B, then password, then 0x007D, then 0x0000
                // If no password: name ends at 0x0000
                let (name, password) = decode_create_conference_args(&mut r)?;
                Ok(Self::CreateConference { language, name, password })
            }
            0x64 => { let name = decode_ustr(&mut r)?; Ok(Self::RenameConference { language, name }) }
            0x65 => { let name = decode_ustr(&mut r)?; Ok(Self::PrivateMessage { language, name }) }
            0x66 => { let name = decode_ustr(&mut r)?; Ok(Self::Ignore { language, name }) }
            0x67 => { let name = decode_ustr(&mut r)?; Ok(Self::StopIgnoring { language, name }) }
            0x68 => { let name = decode_ustr(&mut r)?; Ok(Self::ToggleIgnore { language, name }) }
            0x69 => { let name = decode_ustr(&mut r)?; Ok(Self::GrantVoice { language, name }) }
            0x6A => { let name = decode_ustr(&mut r)?; Ok(Self::RemoveVoice { language, name }) }
            0x6B => { let name = decode_ustr(&mut r)?; Ok(Self::ToggleVoice { language, name }) }
            0x6C => { let name = decode_ustr(&mut r)?; Ok(Self::GrantModerator { language, name }) }
            0x6D => { let name = decode_ustr(&mut r)?; Ok(Self::RemoveModerator { language, name }) }
            0x6E => { let name = decode_ustr(&mut r)?; Ok(Self::ToggleModerator { language, name }) }
            0x6F => Ok(Self::BlockPrivateMessages { language }),
            0x70 => Ok(Self::ReceivePrivateMessages { language }),
            0x71 => Ok(Self::TogglePrivateMessages { language }),
            0x72 => Ok(Self::ShowCharacterName { language }),
            0x73 => Ok(Self::HideCharacterName { language }),
            0x74 => Ok(Self::ToggleCharacterName { language }),
            0x75 => { let name = decode_ustr(&mut r)?; Ok(Self::Whois { language, name }) }
            0x76 => { let name = decode_ustr(&mut r)?; Ok(Self::Kick { language, name }) }
            0x77 => Ok(Self::ModeratorSpeakOnly { language }),
            0x78 => Ok(Self::AllCanSpeak { language }),
            0x79 => Ok(Self::ToggleSpeakPrivileges { language }),
            0x7A => { let text = decode_ustr(&mut r)?; Ok(Self::Emote { language, text }) }
            _ => {
                let remaining = r.remaining_len();
                let raw = r.read_slice(remaining)?;
                Ok(Self::Unknown { language, cmd_type, data: raw.to_vec() })
            }
        }
    }
}

// ── ChatText decode helpers ────────────────────────────────────────────────

/// Read UTF-16 BE code units until the given sentinel value (exclusive),
/// consuming the sentinel but not including it in the result.
fn decode_ustr_until<R: u_io::ReadPrimitives<BE>>(
    r: &mut R,
    sentinel: u16,
) -> Result<String, PacketError> {
    let mut units = Vec::new();
    loop {
        let u: u16 = Decode::decode(r)?;
        if u == sentinel || u == 0 {
            break;
        }
        units.push(u);
    }
    Ok(String::from_utf16_lossy(&units).to_owned())
}

/// Decode the 0x63 CreateConference argument block.
///
/// Wire format:
/// - No password: `<name units> 0x0000`
/// - With password: `<name units> 0x007B <pass units> 0x007D 0x0000`
fn decode_create_conference_args<R: u_io::ReadPrimitives<BE>>(
    r: &mut R,
) -> Result<(String, String), PacketError> {
    let mut name_units = Vec::new();
    let mut password = String::new();

    loop {
        let u: u16 = Decode::decode(r)?;
        match u {
            0x0000 => break, // end of packet, no password
            0x007B => {
                // '{' — password follows, terminated by '}'
                let mut pass_units = Vec::new();
                loop {
                    let pu: u16 = Decode::decode(r)?;
                    if pu == 0x007D || pu == 0x0000 {
                        break;
                    }
                    pass_units.push(pu);
                }
                password = String::from_utf16_lossy(&pass_units).to_owned();
                // consume trailing 0x0000 if present
                break;
            }
            u => name_units.push(u),
        }
    }

    let name = String::from_utf16_lossy(&name_units).to_owned();
    Ok((name, password))
}

// ── ChatText encode ────────────────────────────────────────────────────────

impl Encode<BE> for ChatText {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()

        /// Write a null-terminated UTF-16 BE string.
        fn write_str(w: &mut BinaryWriter<BE>, s: &str) {
            encode_ustr(s, w);
        }

        /// Write a u16 BE code unit.
        fn put_u16c(w: &mut BinaryWriter<BE>, v: u16) {
            w.put_u16(v);
        }

        match self {
            Self::ChangePassword { language, password } => {
                w.put_u32(*language); w.put_u16(0x41);
                write_str(w, password);
            }
            Self::Close { language } => {
                w.put_u32(*language); w.put_u16(0x58);
                w.put_u16(0x0000); // null terminator
            }
            Self::Message { language, text } => {
                w.put_u32(*language); w.put_u16(0x61);
                write_str(w, text);
            }
            Self::JoinConference { language, name, password } => {
                w.put_u32(*language); w.put_u16(0x62);
                put_u16c(w, 0x0022); // opening "
                // write name without null-terminator, then closing "
                for unit in name.encode_utf16() { w.put_u16(unit); }
                put_u16c(w, 0x0022); // closing "
                put_u16c(w, 0x0020); // space holder
                write_str(w, password);
            }
            Self::CreateConference { language, name, password } => {
                w.put_u32(*language); w.put_u16(0x63);
                for unit in name.encode_utf16() { w.put_u16(unit); }
                if !password.is_empty() {
                    put_u16c(w, 0x007B); // '{'
                    for unit in password.encode_utf16() { w.put_u16(unit); }
                    put_u16c(w, 0x007D); // '}'
                }
                w.put_u16(0x0000); // null terminator
            }
            Self::RenameConference { language, name } => {
                w.put_u32(*language); w.put_u16(0x64); write_str(w, name);
            }
            Self::PrivateMessage { language, name } => {
                w.put_u32(*language); w.put_u16(0x65); write_str(w, name);
            }
            Self::Ignore { language, name } => {
                w.put_u32(*language); w.put_u16(0x66); write_str(w, name);
            }
            Self::StopIgnoring { language, name } => {
                w.put_u32(*language); w.put_u16(0x67); write_str(w, name);
            }
            Self::ToggleIgnore { language, name } => {
                w.put_u32(*language); w.put_u16(0x68); write_str(w, name);
            }
            Self::GrantVoice { language, name } => {
                w.put_u32(*language); w.put_u16(0x69); write_str(w, name);
            }
            Self::RemoveVoice { language, name } => {
                w.put_u32(*language); w.put_u16(0x6A); write_str(w, name);
            }
            Self::ToggleVoice { language, name } => {
                w.put_u32(*language); w.put_u16(0x6B); write_str(w, name);
            }
            Self::GrantModerator { language, name } => {
                w.put_u32(*language); w.put_u16(0x6C); write_str(w, name);
            }
            Self::RemoveModerator { language, name } => {
                w.put_u32(*language); w.put_u16(0x6D); write_str(w, name);
            }
            Self::ToggleModerator { language, name } => {
                w.put_u32(*language); w.put_u16(0x6E); write_str(w, name);
            }
            Self::BlockPrivateMessages { language } => {
                w.put_u32(*language); w.put_u16(0x6F); w.put_u16(0x0000);
            }
            Self::ReceivePrivateMessages { language } => {
                w.put_u32(*language); w.put_u16(0x70); w.put_u16(0x0000);
            }
            Self::TogglePrivateMessages { language } => {
                w.put_u32(*language); w.put_u16(0x71); w.put_u16(0x0000);
            }
            Self::ShowCharacterName { language } => {
                w.put_u32(*language); w.put_u16(0x72); w.put_u16(0x0000);
            }
            Self::HideCharacterName { language } => {
                w.put_u32(*language); w.put_u16(0x73); w.put_u16(0x0000);
            }
            Self::ToggleCharacterName { language } => {
                w.put_u32(*language); w.put_u16(0x74); w.put_u16(0x0000);
            }
            Self::Whois { language, name } => {
                w.put_u32(*language); w.put_u16(0x75); write_str(w, name);
            }
            Self::Kick { language, name } => {
                w.put_u32(*language); w.put_u16(0x76); write_str(w, name);
            }
            Self::ModeratorSpeakOnly { language } => {
                w.put_u32(*language); w.put_u16(0x77); w.put_u16(0x0000);
            }
            Self::AllCanSpeak { language } => {
                w.put_u32(*language); w.put_u16(0x78); w.put_u16(0x0000);
            }
            Self::ToggleSpeakPrivileges { language } => {
                w.put_u32(*language); w.put_u16(0x79); w.put_u16(0x0000);
            }
            Self::Emote { language, text } => {
                w.put_u32(*language); w.put_u16(0x7A); write_str(w, text);
            }
            Self::Unknown { language, cmd_type, data } => {
                w.put_u32(*language); w.put_u16(*cmd_type); w.put_slice(data);
            }
        }
    }
}

// ── 0xB5 OpenChatWindow (64 bytes, fixed, C→S) ────────────────────────────

/// Packet 0xB5 — Open Chat Window (64 bytes, fixed, C→S)
///
/// Sent by the client to open the chat window.  The `chat_name` field
/// contains the player's chat username if already known to the client,
/// or is all-zero bytes if the username has not yet been set.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]   0xB5
/// BYTE[63]  chat_name — null-padded Unicode (UTF-16 BE), up to 30 chars + null
/// ```
///
/// The 63-byte field holds up to 30 UTF-16 BE code units (60 bytes) plus a
/// 2-byte null terminator, leaving 1 byte unused at the end.  In practice
/// the entire field is treated as a null-padded block.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[packet(id = 0xB5, size = fixed(64), endian = "be")]
pub struct OpenChatWindow {
    pub id: u8,
    /// Null-padded Unicode chat name (63 bytes on the wire).
    ///
    /// All-zero when the client does not yet have a chat username.
    pub chat_name: FixedString<63>,
}

impl OpenChatWindow {
    /// Create a packet with an empty (all-zero) chat name.
    pub fn new() -> Self {
        Self { id: Self::ID, chat_name: FixedString::new("") }
    }

    /// Create a packet with the given chat username.
    pub fn with_name(name: impl Into<String>) -> Self {
        Self { id: Self::ID, chat_name: FixedString::new(name.into()) }
    }

    /// Returns `true` if the chat name is all-zero (unknown to the client).
    pub fn is_name_unknown(&self) -> bool {
        self.chat_name.is_empty() || self.chat_name.chars().all(|c| c == '\0')
    }
}
