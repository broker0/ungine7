//! Smoke tests: port several existing UO packets to derive macros and verify
//! encode/decode roundtrips.

use std::net::Ipv4Addr;
use u_io::{BE, BinaryReader, BinaryWriter, Decode, Encode};
use packets::{WireEnum, Decode as DecodeMacro, Encode as EncodeMacro, Packet as PacketMacro};
use packets::traits::{ManualPacket, BasicPacket, encode_packet};
use u_io::{FixedString, ListU16, NullString};
use packets::layer::Layer;

// ── Helper ─────────────────────────────────────────────────────────────────

/// Encode a value, then decode it back and compare.
fn roundtrip<T>(value: &T) -> T
where
    T: Encode<BE> + Decode<BE> + std::fmt::Debug,
{
    let mut writer = BinaryWriter::<BE>::new();
    value.encode(&mut writer);
    let bytes = writer.finish();
    let mut reader = BinaryReader::<BE>::new(&bytes);
    T::decode(&mut reader).expect("decode failed")
}

/// Encode a framed packet, then decode it back via `BasicPacket::from_bytes`.
fn roundtrip_packet<T>(value: &T) -> T
where
    T: BasicPacket + std::fmt::Debug,
{
    let bytes = encode_packet(value);
    T::from_bytes(&bytes).expect("packet decode failed")
}

// ── Simple packet: Ping (0x73) ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PacketMacro)]
#[packet(id = 0x73, size = fixed(2), endian = "be")]
pub struct Ping {
    pub id: u8,
    pub sequence: u8,
}

#[test]
fn ping_roundtrip() {
    let ping = Ping { id: 0x73, sequence: 42 };
    let decoded = roundtrip_packet(&ping);
    assert_eq!(ping, decoded);
}

#[test]
fn ping_wire_format() {
    let ping = Ping { id: 0x73, sequence: 0xFF };
    let mut writer = BinaryWriter::<BE>::new();
    ping.encode(&mut writer);
    let bytes = writer.finish();
    assert_eq!(&bytes[..], &[0x73, 0xFF]);
}

// ── Packet with Ipv4Addr: ServerRedirect (0x8C) ───────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PacketMacro)]
#[packet(id = 0x8C, size = fixed(11), endian = "be")]
pub struct ServerRedirect {
    pub id: u8,
    pub ip: Ipv4Addr,
    pub port: u16,
    pub auth_key: u32,
}

#[test]
fn server_redirect_roundtrip() {
    let pkt = ServerRedirect {
        id: 0x8C,
        ip: Ipv4Addr::new(192, 168, 1, 100),
        port: 2593,
        auth_key: 0xDEADBEEF,
    };
    assert_eq!(pkt, roundtrip_packet(&pkt));
}

#[test]
fn server_redirect_wire_format() {
    let pkt = ServerRedirect {
        id: 0x8C,
        ip: Ipv4Addr::new(10, 0, 0, 1),
        port: 2593,
        auth_key: 0x12345678,
    };
    let mut writer = BinaryWriter::<BE>::new();
    pkt.encode(&mut writer);
    let bytes = writer.finish();
    assert_eq!(
        &bytes[..],
        &[
            0x8C,                   // id
            10, 0, 0, 1,            // ip
            0x0A, 0x21,             // port 2593
            0x12, 0x34, 0x56, 0x78, // auth_key
        ]
    );
}

// ── Packet with const_value and pad: LoginCharacter (0x5D) ─────────────────

#[derive(Debug, Clone, PartialEq, Eq, PacketMacro)]
#[packet(id = 0x5D, size = fixed(73), endian = "be")]
pub struct LoginCharacter {
    pub id: u8,

    #[binary(const_value = 0xEDED_EDEDu32)]
    _marker1: u32,

    pub name: FixedString<30>,

    #[binary(pad = 2)]
    _pad1: (),

    pub client_flags: u32,

    #[binary(const_value = 0x0000_0001u32)]
    _marker2: u32,

    #[binary(const_value = 0x0000_0001u32)]
    _marker3: u32,

    #[binary(pad = 16)]
    _pad2: (),

    pub slot: u32,
    pub client_ip: u32,
}

#[test]
fn login_character_roundtrip() {
    let pkt = LoginCharacter {
        id: 0x5D,
        _marker1: 0xEDED_EDED,
        name: FixedString::new("TestChar"),
        _pad1: (),
        client_flags: 0x0000_001F,
        _marker2: 0x0000_0001,
        _marker3: 0x0000_0001,
        _pad2: (),
        slot: 0,
        client_ip: 0xC0A8_0101,
    };
    let decoded = roundtrip_packet(&pkt);
    assert_eq!(pkt.id, decoded.id);
    assert_eq!(pkt.name, decoded.name);
    assert_eq!(pkt.client_flags, decoded.client_flags);
    assert_eq!(pkt.slot, decoded.slot);
    assert_eq!(pkt.client_ip, decoded.client_ip);
}

#[test]
fn login_character_wire_size() {
    let pkt = LoginCharacter {
        id: 0x5D,
        _marker1: 0xEDED_EDED,
        name: FixedString::new("X"),
        _pad1: (),
        client_flags: 0,
        _marker2: 1,
        _marker3: 1,
        _pad2: (),
        slot: 0,
        client_ip: 0,
    };
    let mut writer = BinaryWriter::<BE>::new();
    pkt.encode(&mut writer);
    assert_eq!(writer.len(), 73); // known packet size
}

#[test]
fn login_character_bad_constant_rejected() {
    // Build valid packet bytes, then corrupt the first constant.
    let pkt = LoginCharacter {
        id: 0x5D,
        _marker1: 0xEDED_EDED,
        name: FixedString::new("X"),
        _pad1: (),
        client_flags: 0,
        _marker2: 1,
        _marker3: 1,
        _pad2: (),
        slot: 0,
        client_ip: 0,
    };
    let mut writer = BinaryWriter::<BE>::new();
    pkt.encode(&mut writer);
    let mut bytes = writer.finish().to_vec();

    // Corrupt the first u32 constant at offset 1 (after id byte).
    bytes[1] = 0x00;
    bytes[2] = 0x00;
    bytes[3] = 0x00;
    bytes[4] = 0x00;

    let result = LoginCharacter::from_bytes(&bytes);
    assert!(result.is_err());
}

// ── Sub-struct without packet id ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[binary(endian = "be")]
pub struct GameServerEntry {
    pub index: u16,
    pub name: FixedString<32>,
    pub full_percent: u8,
    pub timezone: u8,
    pub ip: Ipv4Addr,
}

#[test]
fn game_server_entry_roundtrip() {
    let entry = GameServerEntry {
        index: 1,
        name: FixedString::new("Test Shard"),
        full_percent: 50,
        timezone: 5,
        ip: Ipv4Addr::new(127, 0, 0, 1),
    };
    assert_eq!(entry, roundtrip(&entry));
}

// ── Dynamic packet with ListU16: GameServerList (0xA8) ─────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PacketMacro)]
#[packet(id = 0xA8, size = dynamic, endian = "be")]
pub struct GameServerList {
    pub id: u8,
    pub len: u16,
    pub system_info_flag: u8,
    pub servers: ListU16<GameServerEntry>,
}

#[test]
fn game_server_list_roundtrip() {
    let pkt = GameServerList {
        id: 0xA8,
        len: 0,
        system_info_flag: 0x5D,
        servers: ListU16::new(vec![
            GameServerEntry {
                index: 0,
                name: FixedString::new("Shard A"),
                full_percent: 10,
                timezone: 0,
                ip: Ipv4Addr::new(10, 0, 0, 1),
            },
            GameServerEntry {
                index: 1,
                name: FixedString::new("Shard B"),
                full_percent: 80,
                timezone: 3,
                ip: Ipv4Addr::new(10, 0, 0, 2),
            },
        ]),
    };
    let bytes = encode_packet(&pkt);
    let mut expected = pkt.clone();
    expected.len = bytes.len() as u16;
    let decoded = GameServerList::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, expected);
}

// ── NullString packet: SendSpeechPacketTest (0x1C) ───────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PacketMacro)]
#[packet(id = 0x1C, size = dynamic, endian = "be")]
pub struct SendSpeechPacketTest {
    pub id: u8,
    pub len: u16,
    pub item_id: u32,
    pub model: u16,
    pub text_type: u8,
    pub color: u16,
    pub font: u16,
    pub name: FixedString<30>,
    pub message: NullString,
}

#[test]
fn send_speech_roundtrip() {
    let pkt = SendSpeechPacketTest {
        id: 0x1C,
        len: 0,
        item_id: 0xFFFF_FFFF,
        model: 0xFFFF,
        text_type: 6,
        color: 0x0035,
        font: 3,
        name: FixedString::new("System"),
        message: NullString::new("Hello, world!"),
    };
    let bytes = encode_packet(&pkt);
    let mut expected = pkt.clone();
    expected.len = bytes.len() as u16;
    let decoded = SendSpeechPacketTest::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, expected);
}

// ── WireEnum with Unknown ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[repr(u8)]
pub enum LoginDeniedReason {
    #[wire_enum(0x00, "incorrect name/password")]
    IncorrectCredentials,
    #[wire_enum(0x01, "someone is already using this account")]
    AccountInUse,
    #[wire_enum(0x02, "your account has been blocked")]
    AccountBlocked,
    #[wire_enum(unknown)]
    Unknown(u8),
}

#[test]
fn byte_enum_with_unknown_known_value() {
    let reason = LoginDeniedReason::from_wire(0x01);
    assert_eq!(reason, LoginDeniedReason::AccountInUse);
    assert_eq!(reason.to_wire(), 0x01);
    assert_eq!(format!("{reason}"), "someone is already using this account");
}

#[test]
fn byte_enum_with_unknown_unknown_value() {
    let reason = LoginDeniedReason::from_wire(0xFE);
    assert_eq!(reason, LoginDeniedReason::Unknown(0xFE));
    assert_eq!(reason.to_wire(), 0xFE);
    assert_eq!(format!("{reason}"), "unknown (0xFE)");
}

#[test]
fn byte_enum_with_unknown_roundtrip() {
    let reason = LoginDeniedReason::AccountBlocked;
    let mut writer = BinaryWriter::<BE>::new();
    reason.encode(&mut writer);
    let bytes = writer.finish();
    assert_eq!(&bytes[..], &[0x02]);

    let mut reader = BinaryReader::<BE>::new(&bytes);
    let decoded = LoginDeniedReason::decode(&mut reader).unwrap();
    assert_eq!(decoded, reason);
}

// ── WireEnum without Unknown (strict) ────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[repr(u8)]
pub enum SkillLock {
    #[wire_enum(0x00, "up")]
    Up,
    #[wire_enum(0x01, "down")]
    Down,
    #[wire_enum(0x02, "locked")]
    Locked,
}

#[test]
fn byte_enum_strict_known_value() {
    let lock = SkillLock::from_wire(0x02).unwrap();
    assert_eq!(lock, SkillLock::Locked);
    assert_eq!(lock.to_wire(), 0x02);
}

#[test]
fn byte_enum_strict_unknown_value_errors() {
    let result = SkillLock::from_wire(0xFF);
    assert!(result.is_err());
}

#[test]
fn byte_enum_strict_decode_unknown_errors() {
    let data = [0x99u8];
    let mut reader = BinaryReader::<BE>::new(&data);
    let result = SkillLock::decode(&mut reader);
    assert!(result.is_err());
}

#[test]
fn byte_enum_strict_roundtrip() {
    let lock = SkillLock::Down;
    let mut writer = BinaryWriter::<BE>::new();
    lock.encode(&mut writer);
    let bytes = writer.finish();
    let mut reader = BinaryReader::<BE>::new(&bytes);
    let decoded = SkillLock::decode(&mut reader).unwrap();
    assert_eq!(decoded, lock);
}

// ── Custom with = "path" ───────────────────────────────────────────────────

/// Custom decode/encode for a `Vec<u8>` field that is prefixed by a u32
/// length on the wire, where the length **includes** its own 4 bytes.
///
/// This is the pattern from Compressed Gump (0xDD) `CompressedBlock`.
mod inclusive_blob {
    use u_io::{ByteOrder, BinaryWriter, DecodeError, ReadPrimitives};

    pub fn decode<E: ByteOrder, R: ReadPrimitives<E>>(
        reader: &mut R,
    ) -> Result<Vec<u8>, DecodeError> {
        let total_len = reader.read_u32()?;
        let data_len = total_len
            .checked_sub(4)
            .ok_or_else(|| DecodeError::Other("inclusive blob length < 4".into()))?
            as usize;
        let mut buf = vec![0u8; data_len];
        reader.read_bytes(&mut buf)?;
        Ok(buf)
    }

    pub fn encode<E: ByteOrder>(data: &Vec<u8>, writer: &mut BinaryWriter<E>) {
        let total_len = data.len() as u32 + 4;
        writer.put_u32(total_len);
        writer.put_slice(data);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
#[binary(endian = "be")]
pub struct CompressedBlock {
    pub decompressed_len: u32,

    #[binary(with = "inclusive_blob")]
    pub data: Vec<u8>,
}

#[test]
fn custom_with_roundtrip() {
    let block = CompressedBlock {
        decompressed_len: 1024,
        data: vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE],
    };
    assert_eq!(block, roundtrip(&block));
}

#[test]
fn custom_with_wire_format() {
    let block = CompressedBlock {
        decompressed_len: 256,
        data: vec![0x01, 0x02, 0x03],
    };
    let mut writer = BinaryWriter::<BE>::new();
    block.encode(&mut writer);
    let bytes = writer.finish();
    assert_eq!(
        &bytes[..],
        &[
            0x00, 0x00, 0x01, 0x00, // decompressed_len = 256
            0x00, 0x00, 0x00, 0x07, // inclusive len = 3 + 4 = 7
            0x01, 0x02, 0x03,       // data
        ]
    );
}

#[test]
fn custom_with_decode_from_wire() {
    let wire: &[u8] = &[
        0x00, 0x00, 0x02, 0x00, // decompressed_len = 512
        0x00, 0x00, 0x00, 0x06, // inclusive len = 6 => 2 bytes of data
        0xFF, 0xFE,             // data
    ];
    let mut reader = BinaryReader::<BE>::new(wire);
    let block = CompressedBlock::decode(&mut reader).unwrap();
    assert_eq!(block.decompressed_len, 512);
    assert_eq!(block.data, vec![0xFF, 0xFE]);
}

#[test]
fn custom_with_bad_length_errors() {
    // inclusive len = 2, which is < 4 — should error
    let wire: &[u8] = &[
        0x00, 0x00, 0x01, 0x00, // decompressed_len
        0x00, 0x00, 0x00, 0x02, // inclusive len = 2 (invalid)
    ];
    let mut reader = BinaryReader::<BE>::new(wire);
    let result = CompressedBlock::decode(&mut reader);
    assert!(result.is_err());
}

// ── Generic endian (default) ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, DecodeMacro, EncodeMacro)]
pub struct GenericStruct {
    pub a: u16,
    pub b: u32,
}

#[test]
fn generic_endian_be() {
    let val = GenericStruct { a: 0x0102, b: 0x03040506 };
    let mut writer = BinaryWriter::<BE>::new();
    val.encode(&mut writer);
    let bytes = writer.finish();
    assert_eq!(&bytes[..], &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);

    let mut reader = BinaryReader::<BE>::new(&bytes);
    let decoded = GenericStruct::decode(&mut reader).unwrap();
    assert_eq!(val, decoded);
}

#[test]
fn generic_endian_le() {
    use u_io::LE;
    let val = GenericStruct { a: 0x0102, b: 0x03040506 };
    let mut writer = BinaryWriter::<LE>::new();
    val.encode(&mut writer);
    let bytes = writer.finish();
    // LE: u16 0x0102 => [0x02, 0x01], u32 0x03040506 => [0x06, 0x05, 0x04, 0x03]
    assert_eq!(&bytes[..], &[0x02, 0x01, 0x06, 0x05, 0x04, 0x03]);

    let mut reader = BinaryReader::<LE>::new(&bytes);
    let decoded = GenericStruct::decode(&mut reader).unwrap();
    assert_eq!(val, decoded);
}

// ── TextCommand (0x12) ────────────────────────────────────────────────────

use packets::action::TextCommand;

#[test]
fn text_command_use_skill_roundtrip() {
    let cmd = TextCommand::use_skill(1); // Anatomy
    let bytes = cmd.to_bytes();
    let decoded = TextCommand::from_bytes(&bytes).unwrap();
    assert_eq!(cmd, decoded);
}

#[test]
fn text_command_use_skill_wire_format() {
    let cmd = TextCommand::use_skill(1);
    let bytes = cmd.to_bytes();
    // 0x12, len(2), type=0x24, "1 0", 0x00
    assert_eq!(bytes[0], 0x12);
    assert_eq!(bytes[3], 0x24); // type = skill
    assert_eq!(&bytes[4..], b"1 0\0");
    // Total length: 1 + 2 + 1 + 4 = 8
    assert_eq!(bytes.len(), 8);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 8);
}

#[test]
fn text_command_cast_spell_roundtrip() {
    let cmd = TextCommand::cast_spell(58); // Energy Vortex
    let bytes = cmd.to_bytes();
    let decoded = TextCommand::from_bytes(&bytes).unwrap();
    assert_eq!(cmd, decoded);
}

#[test]
fn text_command_cast_spell_wire_format() {
    let cmd = TextCommand::cast_spell(2); // Create Food
    let bytes = cmd.to_bytes();
    assert_eq!(bytes[0], 0x12);
    assert_eq!(bytes[3], 0x56); // type = spell
    assert_eq!(&bytes[4..], b"2\0");
}

#[test]
fn text_command_open_door_roundtrip() {
    let cmd = TextCommand::open_door();
    let bytes = cmd.to_bytes();
    let decoded = TextCommand::from_bytes(&bytes).unwrap();
    assert_eq!(cmd, decoded);
}

#[test]
fn text_command_open_door_wire_format() {
    let cmd = TextCommand::open_door();
    let bytes = cmd.to_bytes();
    // 0x12, len(2), type=0x58, 0x00
    assert_eq!(bytes[0], 0x12);
    assert_eq!(bytes[3], 0x58);
    assert_eq!(bytes[4], 0x00); // null terminator
    assert_eq!(bytes.len(), 5);
}

#[test]
fn text_command_action_bow_roundtrip() {
    let cmd = TextCommand::bow();
    let bytes = cmd.to_bytes();
    let decoded = TextCommand::from_bytes(&bytes).unwrap();
    assert_eq!(cmd, decoded);
}

#[test]
fn text_command_action_salute_roundtrip() {
    let cmd = TextCommand::salute();
    let bytes = cmd.to_bytes();
    let decoded = TextCommand::from_bytes(&bytes).unwrap();
    assert_eq!(cmd, decoded);
}

#[test]
fn text_command_action_wire_format() {
    let cmd = TextCommand::bow();
    let bytes = cmd.to_bytes();
    assert_eq!(bytes[0], 0x12);
    assert_eq!(bytes[3], 0xC7); // type = action
    assert_eq!(&bytes[4..], b"bow\0");
}

#[test]
fn text_command_unknown_type_errors() {
    // Manually build a packet with an unknown type byte
    let data: &[u8] = &[0x12, 0x00, 0x05, 0xFF, 0x00];
    let result = TextCommand::from_bytes(data);
    assert!(result.is_err());
}

#[test]
fn text_command_bad_id_errors() {
    let data: &[u8] = &[0x99, 0x00, 0x05, 0x24, 0x00];
    let result = TextCommand::from_bytes(data);
    assert!(result.is_err());
}

#[test]
fn text_command_truncated_errors() {
    let data: &[u8] = &[0x12, 0x00];
    let result = TextCommand::from_bytes(data);
    assert!(result.is_err());
}

// ── CharacterAnimation (0x6E) ─────────────────────────────────────────────

use packets::character::CharacterAnimation;

#[test]
fn character_animation_roundtrip() {
    let pkt = CharacterAnimation {
        id: 0x6E,
        serial: 0x00012345,
        action: 0x0B,   // swing overhand sword
        _pad0: (),
        frame_count: 7,
        repeat_count: 1, // once
        direction: 0x00, // forward
        repeat_flag: 0,
        frame_delay: 0,
    };
    let decoded = roundtrip_packet(&pkt);
    assert_eq!(pkt, decoded);
}

#[test]
fn character_animation_wire_format() {
    let pkt = CharacterAnimation::once(0xAABBCCDD, 0x10, 5); // magic cast
    let mut writer = BinaryWriter::<BE>::new();
    pkt.encode(&mut writer);
    let bytes = writer.finish();

    assert_eq!(bytes.len(), 14);
    assert_eq!(bytes[0], 0x6E);
    // serial
    assert_eq!(&bytes[1..5], &[0xAA, 0xBB, 0xCC, 0xDD]);
    // action = 0x0010
    assert_eq!(&bytes[5..7], &[0x00, 0x10]);
    // pad
    assert_eq!(bytes[7], 0x00);
    // frame_count = 5
    assert_eq!(bytes[8], 0x05);
    // repeat_count = 1 (once)
    assert_eq!(&bytes[9..11], &[0x00, 0x01]);
    // direction = 0 (forward)
    assert_eq!(bytes[11], 0x00);
    // repeat_flag = 0
    assert_eq!(bytes[12], 0x00);
    // frame_delay = 0
    assert_eq!(bytes[13], 0x00);
}

#[test]
fn character_animation_looping_factory() {
    let pkt = CharacterAnimation::looping(0x00000001, 0x04, 10); // stand, loop
    assert_eq!(pkt.repeat_count, 0);
    assert_eq!(pkt.repeat_flag, 1);
    assert_eq!(pkt.direction, 0x00);
    let decoded = roundtrip_packet(&pkt);
    assert_eq!(pkt, decoded);
}

// ── GraphicalEffect (0x70) ────────────────────────────────────────────────

use packets::world::GraphicalEffect;

#[test]
fn graphical_effect_roundtrip() {
    let pkt = GraphicalEffect {
        id: 0x70,
        direction_type: 0x00,   // projectile
        source_serial: 0x00001000,
        target_serial: 0x00002000,
        model: 0x36D4,
        x: 1500,
        y: 1200,
        z: 10,
        target_x: 1510,
        target_y: 1210,
        target_z: 12,
        speed: 7,
        duration: 10,
        _pad0: (),
        fixed_direction: 0,
        explode: 1,
    };
    let decoded = roundtrip_packet(&pkt);
    assert_eq!(pkt, decoded);
}

#[test]
fn graphical_effect_wire_format() {
    let pkt = GraphicalEffect::lightning(0xDEADBEEF, 100, 200, -5);
    let mut writer = BinaryWriter::<BE>::new();
    pkt.encode(&mut writer);
    let bytes = writer.finish();

    assert_eq!(bytes.len(), 28);
    assert_eq!(bytes[0], 0x70);
    // direction_type = 0x01 (lightning)
    assert_eq!(bytes[1], 0x01);
    // source_serial = 0xDEADBEEF
    assert_eq!(&bytes[2..6], &[0xDE, 0xAD, 0xBE, 0xEF]);
    // target_serial = 0
    assert_eq!(&bytes[6..10], &[0x00, 0x00, 0x00, 0x00]);
    // model = 0
    assert_eq!(&bytes[10..12], &[0x00, 0x00]);
    // x = 100
    assert_eq!(&bytes[12..14], &[0x00, 0x64]);
    // y = 200
    assert_eq!(&bytes[14..16], &[0x00, 0xC8]);
    // z = -5 (0xFB as i8)
    assert_eq!(bytes[16], 0xFB);
}

#[test]
fn graphical_effect_projectile_factory() {
    let pkt = GraphicalEffect::projectile(
        0x01, 0x02, 0x36D4,
        100, 200, 10,
        110, 210, 15,
        5, 10,
    );
    assert_eq!(pkt.direction_type, 0x00);
    assert_eq!(pkt.explode, 0);
    let decoded = roundtrip_packet(&pkt);
    assert_eq!(pkt, decoded);
}

#[test]
fn graphical_effect_stationary_factory() {
    let pkt = GraphicalEffect::stationary(0x3728, 500, 600, 0, 3, 15);
    assert_eq!(pkt.direction_type, 0x02);
    assert_eq!(pkt.target_x, 500);
    assert_eq!(pkt.target_y, 600);
    assert_eq!(pkt.target_z, 0);
    let decoded = roundtrip_packet(&pkt);
    assert_eq!(pkt, decoded);
}

// ── CorpseClothing (0x89) ─────────────────────────────────────────────────

use packets::interaction::{CorpseClothing, CorpseClothingEntry};

#[test]
fn corpse_clothing_roundtrip() {
    let pkt = CorpseClothing {
        corpse_id: 0x4000_1234,
        items: vec![
            CorpseClothingEntry { layer: Layer::RightHand,  item_id: 0x4000_0001 },
            CorpseClothingEntry { layer: Layer::Torso,      item_id: 0x4000_0002 },
            CorpseClothingEntry { layer: Layer::Mount,      item_id: 0x4000_0003 },
        ],
    };
    let bytes = pkt.to_bytes();
    let decoded = CorpseClothing::from_bytes(&bytes).unwrap();
    assert_eq!(pkt, decoded);
}

#[test]
fn corpse_clothing_wire_format() {
    let pkt = CorpseClothing {
        corpse_id: 0x4000_0001,
        items: vec![
            CorpseClothingEntry { layer: Layer::Shirt, item_id: 0x4000_AABB },
        ],
    };
    let bytes = pkt.to_bytes();
    // id(1) + len(2) + corpse_id(4) + [layer(1) + item_id(4)] + terminator(1) = 13
    assert_eq!(bytes.len(), 13);
    assert_eq!(bytes[0], 0x89);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 13);
    assert_eq!(u32::from_be_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]), 0x4000_0001);
    assert_eq!(bytes[7], 0x05);
    assert_eq!(u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]), 0x4000_AABB);
    assert_eq!(bytes[12], 0x00);
}

#[test]
fn corpse_clothing_empty_roundtrip() {
    let pkt = CorpseClothing {
        corpse_id: 0x4000_0099,
        items: vec![],
    };
    let bytes = pkt.to_bytes();
    // id(1) + len(2) + corpse_id(4) + terminator(1) = 8
    assert_eq!(bytes.len(), 8);
    let decoded = CorpseClothing::from_bytes(&bytes).unwrap();
    assert_eq!(pkt, decoded);
}

#[test]
fn corpse_clothing_bad_id_errors() {
    let data = [0x88, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00];
    assert!(CorpseClothing::from_bytes(&data).is_err());
}

#[test]
fn corpse_clothing_truncated_errors() {
    let data = [0x89, 0x00];
    assert!(CorpseClothing::from_bytes(&data).is_err());
}

// ── MobileFlags ───────────────────────────────────────────────────────────

#[test]
fn mobile_flags_none_is_zero() {
    use packets::mobile_flags::MobileFlags;
    assert_eq!(MobileFlags::NONE.to_raw(), 0x00);
}

#[test]
fn mobile_flags_accessors() {
    use packets::mobile_flags::MobileFlags;

    let f = MobileFlags(0x00);
    assert!(!f.war_mode_aos());
    assert!(!f.can_alter_paperdoll());
    assert!(!f.poisoned());
    assert!(!f.golden_health());
    assert!(!f.war_mode_legacy());
    assert!(!f.hidden());

    let f = MobileFlags(0xFF);
    assert!(f.war_mode_aos());
    assert!(f.can_alter_paperdoll());
    assert!(f.poisoned());
    assert!(f.golden_health());
    assert!(f.war_mode_legacy());
    assert!(f.hidden());
}

#[test]
fn mobile_flags_individual_bits() {
    use packets::mobile_flags::MobileFlags;

    assert!(MobileFlags(0x01).war_mode_aos());
    assert!(MobileFlags(0x02).can_alter_paperdoll());
    assert!(MobileFlags(0x04).poisoned());
    assert!(MobileFlags(0x08).golden_health());
    assert!(MobileFlags(0x40).war_mode_legacy());
    assert!(MobileFlags(0x80).hidden());
}

#[test]
fn mobile_flags_builder_helpers() {
    use packets::mobile_flags::MobileFlags;

    let f = MobileFlags::NONE.with_poisoned(true).with_war_mode(true);
    assert!(f.poisoned());
    assert!(f.war_mode_legacy());
    assert!(!f.hidden());
    assert_eq!(f.to_raw(), 0x04 | 0x40);

    let f2 = f.with_poisoned(false);
    assert!(!f2.poisoned());
    assert!(f2.war_mode_legacy());

    let f3 = MobileFlags::NONE.with_hidden(true);
    assert!(f3.hidden());
    assert_eq!(f3.to_raw(), 0x80);
}

#[test]
fn mobile_flags_from_into_u8() {
    use packets::mobile_flags::MobileFlags;

    let f: MobileFlags = 0x46u8.into();
    assert_eq!(u8::from(f), 0x46);
    assert!(f.war_mode_legacy());
    assert!(f.can_alter_paperdoll());
    assert!(f.poisoned());
}

#[test]
fn mobile_flags_encode_decode_roundtrip() {
    use u_io::{BE, BinaryReader, BinaryWriter, Decode, Encode};
    use packets::mobile_flags::MobileFlags;

    for raw in [0x00u8, 0x02, 0x04, 0x06, 0x40, 0x46, 0x80, 0xFF] {
        let flags = MobileFlags(raw);
        let mut writer = BinaryWriter::<BE>::new();
        flags.encode(&mut writer);
        let bytes = writer.finish();
        assert_eq!(bytes.len(), 1);
        assert_eq!(bytes[0], raw);

        let mut reader = BinaryReader::<BE>::new(&bytes);
        let decoded: MobileFlags = Decode::decode(&mut reader).unwrap();
        assert_eq!(decoded, flags);
    }
}

#[test]
fn mobile_flags_used_in_update_mobile() {
    use packets::character::UpdateMobile;
    use packets::mobile_flags::MobileFlags;
    use packets::traits::BasicPacket;

    let pkt = UpdateMobile {
        id: UpdateMobile::ID,
        serial: 0x0000_0001,
        model: 0x0190,
        x: 100, y: 200, z: 10,
        direction: 2,
        hue: 0x083F,
        status_flags: MobileFlags(0x02),
        notoriety: packets::movement::Notoriety::Innocent,
    };
    let bytes = pkt.to_bytes();
    assert_eq!(bytes.len(), 17);
    assert_eq!(bytes[0], 0x77);
    // status_flags byte is at offset 15 (after id+serial+model+x+y+z+direction+hue)
    assert_eq!(bytes[15], 0x02);
    let decoded = UpdateMobile::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.status_flags, MobileFlags(0x02));
    assert!(decoded.status_flags.can_alter_paperdoll());
}

#[test]
fn mobile_flags_used_in_draw_game_player() {
    use packets::character::DrawGamePlayer;
    use packets::mobile_flags::MobileFlags;
    use packets::traits::BasicPacket;

    let pkt = DrawGamePlayer {
        id: DrawGamePlayer::ID,
        serial: 0x0000_0001,
        body_type: 0x0190,
        _pad0: (),
        hue: 0x083F,
        flags: MobileFlags(0x04),
        x: 100, y: 200,
        _pad1: (),
        direction: 2,
        z: 0,
    };
    let bytes = pkt.to_bytes();
    assert_eq!(bytes.len(), 19);
    assert_eq!(bytes[0], 0x20);
    let decoded = DrawGamePlayer::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.flags, MobileFlags(0x04));
    assert!(decoded.flags.poisoned());
}

#[test]
fn mobile_flags_used_in_draw_mobile() {
    use packets::mobile_flags::MobileFlags;
    use packets::world::DrawMobile;

    // Use an existing real-packet test: flags byte = 0x02
    let data = [
        0x78u8, 0x00, 0x17,          // id, len=23
        0x00, 0x00, 0x00, 0x01,      // serial
        0x01, 0x90,                  // graphic
        0x00, 0x64,                  // x=100
        0x00, 0xC8,                  // y=200
        0x0A,                        // z=10
        0x02,                        // direction
        0x08, 0x3F,                  // color
        0x06,                        // status = WAR_MODE_LEGACY | CAN_ALTER_PAPERDOLL
        0x01,                        // notoriety = Innocent
        0x00, 0x00, 0x00, 0x00,      // terminator
    ];
    let pkt = DrawMobile::from_bytes(&data).unwrap();
    assert_eq!(pkt.status, MobileFlags(0x06));
    assert!(pkt.status.can_alter_paperdoll());
    assert!(pkt.status.poisoned());
    assert!(!pkt.status.war_mode_legacy());
}

// ── FightOccurring (0x2F) ─────────────────────────────────────────────────

use packets::interaction::FightOccurring;
#[test]
fn fight_occurring_roundtrip() {
    let pkt = FightOccurring::new(0x0000_1234, 0x0000_5678);
    let bytes = encode_packet(&pkt);
    let decoded = FightOccurring::from_bytes(&bytes).unwrap();
    assert_eq!(pkt, decoded);
}

#[test]
fn fight_occurring_wire_format() {
    let pkt = FightOccurring::new(0x0000_0001, 0x0000_0002);
    let bytes = encode_packet(&pkt);
    assert_eq!(bytes.len(), 10);
    assert_eq!(bytes[0], 0x2F);
    assert_eq!(bytes[1], 0x00);
    assert_eq!(u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]), 0x0000_0001);
    assert_eq!(u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]), 0x0000_0002);
}

// ── GeneralInfo (0xBF) ────────────────────────────────────────────────────

use packets::system::{GeneralInfo, MapDiffEntry};

fn roundtrip_general_info(pkt: &GeneralInfo) -> GeneralInfo {
    let bytes = pkt.to_bytes();
    GeneralInfo::from_bytes(&bytes).expect("GeneralInfo decode failed")
}

// — 0x0005 ScreenSize —

#[test]
fn general_info_screen_size_roundtrip() {
    let pkt = GeneralInfo::ScreenSize { unk1: 0, x: 1280, y: 720, unk2: 0 };
    assert_eq!(pkt, roundtrip_general_info(&pkt));
}

#[test]
fn general_info_screen_size_wire_format() {
    let pkt = GeneralInfo::ScreenSize { unk1: 0x0000, x: 0x0500, y: 0x02D0, unk2: 0x0000 };
    let bytes = pkt.to_bytes();
    // id(1) + len(2) + sub_cmd(2) + unk1(2) + x(2) + y(2) + unk2(2) = 13
    assert_eq!(bytes.len(), 13);
    assert_eq!(bytes[0], 0xBF);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 13);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0x0005); // sub_cmd
    assert_eq!(u16::from_be_bytes([bytes[5], bytes[6]]), 0x0000); // unk1
    assert_eq!(u16::from_be_bytes([bytes[7], bytes[8]]), 0x0500); // x = 1280
    assert_eq!(u16::from_be_bytes([bytes[9], bytes[10]]), 0x02D0); // y = 720
    assert_eq!(u16::from_be_bytes([bytes[11], bytes[12]]), 0x0000); // unk2
}

// — 0x0008 SetCursorHue —

#[test]
fn general_info_set_cursor_hue_roundtrip() {
    for hue in [0u8, 1, 2] {
        let pkt = GeneralInfo::SetMap { world: hue };
        assert_eq!(pkt, roundtrip_general_info(&pkt));
    }
}

#[test]
fn general_info_set_cursor_hue_wire_format() {
    let pkt = GeneralInfo::SetMap { world: 1 };
    let bytes = pkt.to_bytes();
    // id(1) + len(2) + sub_cmd(2) + hue(1) = 6
    assert_eq!(bytes.len(), 6);
    assert_eq!(bytes[0], 0xBF);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 6);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0x0008); // sub_cmd
    assert_eq!(bytes[5], 1);                                      // hue = Trammel
}

// — 0x000B ClientLanguage —

#[test]
fn general_info_client_language_roundtrip() {
    let pkt = GeneralInfo::ClientLanguage { language: FixedString::new("ENU") };
    assert_eq!(pkt, roundtrip_general_info(&pkt));
}

#[test]
fn general_info_client_language_wire_format() {
    let pkt = GeneralInfo::ClientLanguage { language: FixedString::new("ENU") };
    let bytes = pkt.to_bytes();
    // id(1) + len(2) + sub_cmd(2) + language(4 = 3 chars + null) = 9
    assert_eq!(bytes.len(), 9);
    assert_eq!(bytes[0], 0xBF);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 9);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0x000B); // sub_cmd
    assert_eq!(&bytes[5..8], b"ENU");
    assert_eq!(bytes[8], 0x00); // null terminator
}

#[test]
fn general_info_client_language_real_packet_rus() {
    // Regression: FixedString<3> dropped the trailing null, re-encode was 8 bytes not 9.
    let raw: &[u8] = &[0xBF, 0x00, 0x09, 0x00, 0x0B, 0x52, 0x55, 0x53, 0x00];
    let pkt = GeneralInfo::from_bytes(raw).unwrap();
    match &pkt {
        GeneralInfo::ClientLanguage { language } => {
            assert_eq!(&**language, "RUS");
        }
        other => panic!("unexpected variant: {:?}", other),
    }
    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], raw);
}

// — 0x000F ClientType —

#[test]
fn general_info_client_type_roundtrip() {
    let pkt = GeneralInfo::ClientType { unk1: 0x0A, flags: 0x0000_0003 };
    assert_eq!(pkt, roundtrip_general_info(&pkt));
}

#[test]
fn general_info_client_type_wire_format() {
    let pkt = GeneralInfo::ClientType { unk1: 0x0A, flags: 0x0000_0003 };
    let bytes = pkt.to_bytes();
    // id(1) + len(2) + sub_cmd(2) + unk1(1) + flags(4) = 10
    assert_eq!(bytes.len(), 10);
    assert_eq!(bytes[0], 0xBF);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 10);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0x000F); // sub_cmd
    assert_eq!(bytes[5], 0x0A);                                   // unk1
    assert_eq!(u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]), 0x0000_0003); // flags
}

// — 0x0018 EnableMapDiff —

#[test]
fn general_info_enable_map_diff_roundtrip() {
    let pkt = GeneralInfo::EnableMapDiff {
        maps: vec![
            MapDiffEntry { map_patches: 10, static_patches: 20 },
            MapDiffEntry { map_patches: 0,  static_patches: 5  },
        ],
    };
    assert_eq!(pkt, roundtrip_general_info(&pkt));
}

#[test]
fn general_info_enable_map_diff_wire_format() {
    let pkt = GeneralInfo::EnableMapDiff {
        maps: vec![
            MapDiffEntry { map_patches: 0x0000_000A, static_patches: 0x0000_0014 },
        ],
    };
    let bytes = pkt.to_bytes();
    // id(1) + len(2) + sub_cmd(2) + num_maps(4) + 1×[map_patches(4)+static_patches(4)] = 17
    assert_eq!(bytes.len(), 17);
    assert_eq!(bytes[0], 0xBF);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 17);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0x0018);       // sub_cmd
    assert_eq!(u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]), 1); // num_maps
    assert_eq!(u32::from_be_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]), 0x0000_000A); // map_patches
    assert_eq!(u32::from_be_bytes([bytes[13], bytes[14], bytes[15], bytes[16]]), 0x0000_0014); // static_patches
}

#[test]
fn general_info_enable_map_diff_empty_roundtrip() {
    let pkt = GeneralInfo::EnableMapDiff { maps: vec![] };
    let bytes = pkt.to_bytes();
    // id(1) + len(2) + sub_cmd(2) + num_maps(4) = 9
    assert_eq!(bytes.len(), 9);
    let decoded = GeneralInfo::from_bytes(&bytes).unwrap();
    assert_eq!(pkt, decoded);
}

#[test]
fn general_info_unknown_roundtrip() {
    let pkt = GeneralInfo::Unknown { sub_cmd: 0x00FF, data: vec![0x01, 0x02, 0x03] };
    assert_eq!(pkt, roundtrip_general_info(&pkt));
}

#[test]
fn general_info_bad_id_errors() {
    let data = [0xBE, 0x00, 0x07, 0x00, 0x05, 0x00, 0x00];
    assert!(GeneralInfo::from_bytes(&data).is_err());
}

#[test]
fn general_info_truncated_errors() {
    // Only 4 bytes — minimum is 5 (need sub_cmd u16 after id+len)
    let data = [0xBF, 0x00, 0x04, 0x00];
    assert!(GeneralInfo::from_bytes(&data).is_err());
}

// ── DrawMobile (0x78) autodetect ──────────────────────────────────────────

use packets::world::DrawMobile;

/// Build a raw 0x78 packet from parts without going through Encode,
/// so we can control exactly what goes on the wire.
fn build_draw_mobile_raw(
    serial: u32, graphic: u16, x: u16, y: u16, z: i8,
    direction: u8, color: u16, flags: u8, notoriety: u8,
    items: &[(u32, u16, u8, Option<u16>)], // (serial, graphic, layer, hue)
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(0x78u8);
    buf.extend_from_slice(&[0x00, 0x00]); // length placeholder
    buf.extend_from_slice(&serial.to_be_bytes());
    buf.extend_from_slice(&graphic.to_be_bytes());
    buf.extend_from_slice(&x.to_be_bytes());
    buf.extend_from_slice(&y.to_be_bytes());
    buf.push(z as u8);
    buf.push(direction);
    buf.extend_from_slice(&color.to_be_bytes());
    buf.push(flags);
    buf.push(notoriety);
    for &(s, g, l, hue) in items {
        buf.extend_from_slice(&s.to_be_bytes());
        let raw_g = if let Some(_) = hue { g | 0x8000 } else { g & 0x7FFF };
        buf.extend_from_slice(&raw_g.to_be_bytes());
        buf.push(l);
        if let Some(h) = hue {
            buf.extend_from_slice(&h.to_be_bytes());
        }
    }
    // terminator
    buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    let len = buf.len() as u16;
    buf[1] = (len >> 8) as u8;
    buf[2] = (len & 0xFF) as u8;
    buf
}

// ── 0xD8 SendCustomHouse ──────────────────────────────────────────────────

#[test]
fn send_custom_house_roundtrip() {
    use packets::house::{HousePlane, SendCustomHouse};
    use packets::traits::ManualPacket;

    // Two fake planes with pre-"compressed" data (no real zlib here).
    let pkt = SendCustomHouse {
        compression_type: 0,
        enable_response: false,        house_serial: 0x4000_0001,
        revision: 42,
        num_tiles: 3,
        buffer_length: 15,
        planes: vec![
            HousePlane {
                mode: 0,
                plane_z: 0,
                uncompressed_len: 15, // 3 × 5-byte mode-0 records
                data: vec![0x01, 0x02, 0x03, 0x04, 0x05], // fake compressed blob
            },
            HousePlane {
                mode: 1,
                plane_z: 2,
                uncompressed_len: 8, // 2 × 4-byte mode-1 records
                data: vec![0xAA, 0xBB],
            },
        ],
    };

    let bytes = pkt.to_bytes();
    let decoded = SendCustomHouse::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, pkt);
}

#[test]
fn send_custom_house_wire_format() {
    use packets::house::{HousePlane, SendCustomHouse};
    use packets::traits::ManualPacket;

    let pkt = SendCustomHouse {
        compression_type: 0,
        enable_response: false,        house_serial: 0x1234_5678,
        revision: 7,
        num_tiles: 0,
        buffer_length: 0,
        planes: vec![
            HousePlane {
                mode: 2,
                plane_z: 1,
                uncompressed_len: 4,
                data: vec![0x00, 0x01, 0x00, 0x02], // 2 × mode-2 (u16 id)
            },
        ],
    };

    let bytes = pkt.to_bytes();

    // cmd
    assert_eq!(bytes[0], 0xD8);
    // length = 17 header + 1 plane_count + 4 header + 4 data = 26
    let total_len = u16::from_be_bytes([bytes[1], bytes[2]]);
    assert_eq!(total_len as usize, bytes.len());
    // compression_type
    assert_eq!(bytes[3], 0x00);
    // house_serial
    assert_eq!(&bytes[5..9], &[0x12, 0x34, 0x56, 0x78]);
    // plane_count byte (offset 17)
    assert_eq!(bytes[17], 1);
}

#[test]
fn send_custom_house_bitpack_roundtrip() {
    use packets::house::HousePlane;

    // Verify that encode_header is the exact inverse of decode_header
    // for a variety of (mode, plane_z, unc_len, cmp_len) combinations.
    let cases: &[(u8, u8, u16, u16)] = &[
        (0, 0, 0, 0),
        (0, 0, 15, 10),
        (1, 3, 100, 80),
        (2, 7, 4095, 4095), // max 12-bit values
        (2, 15, 512, 300),
    ];

    for &(mode, plane_z, unc, cmp) in cases {
        let h = HousePlane::encode_header(mode, plane_z, unc, cmp);
        let (dm, dz, du, dc) = HousePlane::decode_header(h);
        assert_eq!((dm, dz, du, dc), (mode, plane_z, unc, cmp),
            "bitpack roundtrip failed for ({mode}, {plane_z}, {unc}, {cmp})");
    }
}

#[test]
fn send_custom_house_compression_type_3_roundtrip() {
    use packets::house::{HousePlane, SendCustomHouse};
    use packets::traits::ManualPacket;

    // compression_type=3 is an informational field that does NOT trigger
    // outer zlib — it is simply stored and round-tripped.
    let pkt = SendCustomHouse {
        compression_type: 3,
        enable_response: true,
        house_serial: 0x4000_0002,
        revision: 99,
        num_tiles: 5,
        buffer_length: 25,
        planes: vec![
            HousePlane {
                mode: 0,
                plane_z: 0,
                uncompressed_len: 15,
                data: vec![0x01, 0x02, 0x03, 0x04, 0x05],
            },
            HousePlane {
                mode: 1,
                plane_z: 3,
                uncompressed_len: 8,
                data: vec![0xAA, 0xBB, 0xCC],
            },
        ],
    };

    let bytes = pkt.to_bytes();
    assert_eq!(bytes[0], 0xD8);
    assert_eq!(bytes[3], 0x03); // compression_type preserved

    let decoded = SendCustomHouse::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, pkt);
}

#[test]
fn send_custom_house_compression_type_3_empty_payload() {
    use packets::house::SendCustomHouse;
    use packets::traits::ManualPacket;

    // Minimal packet with compression_type=3 and no plane data —
    // should parse successfully as 0 planes.
    let mut data = vec![0u8; 17];
    data[0] = 0xD8;
    data[1] = 0x00;
    data[2] = 0x11; // len = 17
    data[3] = 0x03; // compression_type = 3

    let pkt = SendCustomHouse::from_bytes(&data).unwrap();
    assert_eq!(pkt.compression_type, 3);
    assert!(pkt.planes.is_empty());
}

#[test]
fn send_custom_house_plane_z_translation() {
    use packets::house::HousePlane;

    assert_eq!(HousePlane::actual_z(0), 0);
    assert_eq!(HousePlane::actual_z(1), 7);
    assert_eq!(HousePlane::actual_z(2), 27);
    assert_eq!(HousePlane::actual_z(3), 47);
    assert_eq!(HousePlane::actual_z(4), 67);
    // wraps at mod 4: plane_z=5 → (4%4)*20+7 = 7
    assert_eq!(HousePlane::actual_z(5), 7);
}

#[test]
fn send_custom_house_empty_plane_skipped() {
    use packets::house::{HousePlane, SendCustomHouse};
    use packets::traits::ManualPacket;

    // A packet with two planes: first has cmp_len=0 (must be skipped),
    // second has real data.
    let pkt = SendCustomHouse {
        compression_type: 0,
        enable_response: false,
        house_serial: 0x1,
        revision: 1,
        num_tiles: 0,
        buffer_length: 0,
        planes: vec![
            HousePlane { mode: 0, plane_z: 0, uncompressed_len: 10, data: vec![0xAB, 0xCD] },
        ],
    };

    // Build wire bytes manually inserting a zero-cmp_len plane before the real one.
    let real_bytes = pkt.to_bytes();

    // Insert a plane header with cmp_len=0 right after the plane_count byte (offset 18).
    // plane_count byte is at offset 17; plane headers start at 18.
    let mut wire = real_bytes.to_vec();
    // Increment plane_count from 1 to 2.
    wire[17] = 2;
    // Insert 4-byte header with cmp_len=0 at offset 18 (before existing plane).
    let zero_header = HousePlane::encode_header(0, 0, 0, 0).to_be_bytes();
    wire.splice(18..18, zero_header.iter().copied());
    // Fix total length field.
    let new_len = wire.len() as u16;
    wire[1] = (new_len >> 8) as u8;
    wire[2] = (new_len & 0xFF) as u8;

    let decoded = SendCustomHouse::from_bytes(&wire).unwrap();
    // Zero-cmp_len plane must be absent; only the real plane remains.
    assert_eq!(decoded.planes.len(), 1);
    assert_eq!(decoded.planes[0].data, vec![0xAB, 0xCD]);
}

// ── 0xDD SendCompressedGump ──────────────────────────────────────────────

#[test]
fn send_compressed_gump_roundtrip() {
    use packets::gump::{GumpTextLine, SendCompressedGump};
    use packets::traits::ManualPacket;

    let pkt = SendCompressedGump {
        serial: 0x0000_0001,
        gump_id: 0x1234_5678,
        x: 100,
        y: 200,
        layout: "{ button 10 20 30 40 1 0 1 }".to_string(),
        text_lines: vec![
            GumpTextLine("Hello".to_string()),
            GumpTextLine("World".to_string()),
        ],
    };

    let bytes = pkt.to_bytes();
    assert_eq!(bytes[0], 0xDD);

    let decoded = SendCompressedGump::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, pkt);
}

#[test]
fn send_compressed_gump_empty_layout_and_text_roundtrip() {
    use packets::gump::SendCompressedGump;
    use packets::traits::ManualPacket;

    let pkt = SendCompressedGump {
        serial: 0,
        gump_id: 0,
        x: 0,
        y: 0,
        layout: String::new(),
        text_lines: Vec::new(),
    };

    let bytes = pkt.to_bytes();
    let decoded = SendCompressedGump::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, pkt);
}

#[test]
fn send_compressed_gump_unicode_text_roundtrip() {
    use packets::gump::{GumpTextLine, SendCompressedGump};
    use packets::traits::ManualPacket;

    let pkt = SendCompressedGump {
        serial: 42,
        gump_id: 100,
        x: 0,
        y: 0,
        layout: "{ text 0 0 1000 0 }".to_string(),
        text_lines: vec![
            GumpTextLine("\u{0410}\u{0411}\u{0412}".to_string()), // Cyrillic
        ],
    };

    let bytes = pkt.to_bytes();
    let decoded = SendCompressedGump::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, pkt);
}

#[test]
fn send_compressed_gump_from_send_gump_dialog() {
    use packets::gump::{GumpTextLine, SendCompressedGump, SendGumpDialog};

    let dialog = SendGumpDialog {
        serial: 1,
        gump_id: 2,
        x: 10,
        y: 20,
        layout: "{ page 0 }".to_string(),
        text_lines: vec![GumpTextLine("Test".to_string())],
        trailing_pad: vec![0x00],
    };

    let compressed: SendCompressedGump = (&dialog).into();
    assert_eq!(compressed.serial, dialog.serial);
    assert_eq!(compressed.gump_id, dialog.gump_id);
    assert_eq!(compressed.layout, dialog.layout);
    assert_eq!(compressed.text_lines, dialog.text_lines);

    // And back.
    let back: SendGumpDialog = (&compressed).into();
    assert_eq!(back.serial, dialog.serial);
    assert_eq!(back.layout, dialog.layout);
    assert_eq!(back.text_lines, dialog.text_lines);
    assert!(back.trailing_pad.is_empty(), "trailing_pad should be empty after conversion");
}

/// Same but modern mode: hue word always present regardless of graphic bit.
fn build_draw_mobile_modern_raw(
    serial: u32, graphic: u16, x: u16, y: u16, z: i8,
    direction: u8, color: u16, flags: u8, notoriety: u8,
    items: &[(u32, u16, u8, u16)], // (serial, graphic, layer, hue) — always
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(0x78u8);
    buf.extend_from_slice(&[0x00, 0x00]);
    buf.extend_from_slice(&serial.to_be_bytes());
    buf.extend_from_slice(&graphic.to_be_bytes());
    buf.extend_from_slice(&x.to_be_bytes());
    buf.extend_from_slice(&y.to_be_bytes());
    buf.push(z as u8);
    buf.push(direction);
    buf.extend_from_slice(&color.to_be_bytes());
    buf.push(flags);
    buf.push(notoriety);
    for &(s, g, l, h) in items {
        buf.extend_from_slice(&s.to_be_bytes());
        // modern: graphic stored raw (no bit15 flag needed)
        buf.extend_from_slice(&(g & 0x7FFF).to_be_bytes());
        buf.push(l);
        buf.extend_from_slice(&h.to_be_bytes());
    }
    buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    let len = buf.len() as u16;
    buf[1] = (len >> 8) as u8;
    buf[2] = (len & 0xFF) as u8;
    buf
}

#[test]
fn draw_mobile_legacy_no_hue_autodetect() {
    // Legacy: no hue on any item → 7 bytes per item → not divisible by 9
    let items = [
        (0x1234_5678u32, 0x0190u16, 1u8, None),
        (0x1234_5679u32, 0x03E4u16, 5u8, None),
    ];
    let data = build_draw_mobile_raw(
        0x0000_0001, 0x0191, 100, 200, 10, 2, 0x0000, 0x02, 0x01, &items,
    );
    let pkt = DrawMobile::from_bytes(&data).expect("should parse (legacy no-hue)");
    assert_eq!(pkt.items.len(), 2);
    assert_eq!(pkt.items[0].serial, 0x1234_5678);
    assert_eq!(pkt.items[0].graphic, 0x0190);
    assert_eq!(pkt.items[0].layer, Layer::RightHand);
    assert_eq!(pkt.items[0].color, None);
    assert_eq!(pkt.items[1].color, None);
}

#[test]
fn draw_mobile_legacy_mixed_hue_autodetect() {
    // Legacy: mixed — one item has hue (graphic bit15 set), one doesn't.
    // Total item bytes: 9 + 7 + 4 (term) = 20, payload = 16, not % 9 → legacy.
    let items = [
        (0xAAAA_0001u32, 0x1234u16, 2u8, Some(0x0500u16)),
        (0xAAAA_0002u32, 0x0567u16, 3u8, None),
    ];
    let data = build_draw_mobile_raw(
        0x0000_0002, 0x0190, 50, 60, 0, 0, 0x083F, 0x00, 0x01, &items,
    );
    let pkt = DrawMobile::from_bytes(&data).expect("should parse (legacy mixed)");
    assert_eq!(pkt.items.len(), 2);
    assert_eq!(pkt.items[0].color, Some(0x0500));
    assert_eq!(pkt.items[1].color, None);
}

#[test]
fn draw_mobile_modern_autodetect() {
    // Modern: hue always present → 9 bytes per item.
    // Reproduces the real packet from the bug report (shard with modern encoding).
    let items = [
        (0x4732_9B23u32, 0x170Du16, 3u8, 0x08FDu16),
        (0x4732_9B25u32, 0x204Bu16, 11u8, 0x0475u16),
        (0x4732_9B22u32, 0x0E75u16, 21u8, 0x0000u16),
        (0x4732_9B24u32, 0x1EFFu16, 22u8, 0x08FDu16),
    ];
    let data = build_draw_mobile_modern_raw(
        0x0732_9B26, 0x0191, 2420, 446, 15, 6, 0x83F6, 0x02, 0x01, &items,
    );
    let pkt = DrawMobile::from_bytes(&data).expect("should parse (modern autodetect)");
    assert_eq!(pkt.serial, 0x0732_9B26);
    assert_eq!(pkt.items.len(), 4);
    assert_eq!(pkt.items[0].serial, 0x4732_9B23);
    assert_eq!(pkt.items[0].graphic, 0x170D);
    assert_eq!(pkt.items[0].layer, Layer::Shoes);
    assert_eq!(pkt.items[0].color, Some(0x08FD));
    assert_eq!(pkt.items[2].color, Some(0x0000));
}

#[test]
fn draw_mobile_real_packet_from_bug_report() {
    // Verbatim bytes from the bug report that previously caused
    // "buffer truncated".
    let data = [
        0x78u8, 0x00, 0x3B,
        0x07, 0x32, 0x9B, 0x26,  // serial
        0x01, 0x91,              // graphic
        0x09, 0x74,              // x=2420
        0x01, 0xBE,              // y=446
        0x0F,                    // z=15
        0x06,                    // direction
        0x83, 0xF6,              // color
        0x02,                    // flags
        0x01,                    // notoriety
        // items (modern: 4×9 bytes)
        0x47, 0x32, 0x9B, 0x23,  0x17, 0x0D,  0x03,  0x08, 0xFD,
        0x47, 0x32, 0x9B, 0x25,  0x20, 0x47,  0x0B,  0x04, 0x75,
        0x47, 0x32, 0x9B, 0x22,  0x0E, 0x75,  0x15,  0x00, 0x00,
        0x47, 0x32, 0x9B, 0x24,  0x1E, 0xFF,  0x16,  0x08, 0xFD,
        // terminator
        0x00, 0x00, 0x00, 0x00,
    ];
    let pkt = DrawMobile::from_bytes(&data).expect("should parse real packet");
    assert_eq!(pkt.serial, 0x0732_9B26);
    assert_eq!(pkt.graphic, 0x0191);
    assert_eq!(pkt.x, 2420);
    assert_eq!(pkt.y, 446);
    assert_eq!(pkt.items.len(), 4);
    assert_eq!(pkt.items[0].serial, 0x4732_9B23);
    assert_eq!(pkt.items[0].graphic, 0x170D);
    assert_eq!(pkt.items[0].color, Some(0x08FD));
    assert_eq!(pkt.items[3].serial, 0x4732_9B24);
    assert_eq!(pkt.items[3].color, Some(0x08FD));
}

#[test]
fn draw_mobile_no_items_autodetect() {
    // No items at all — only terminator.
    let data = build_draw_mobile_raw(
        0x0000_0001, 0x0191, 10, 20, 0, 0, 0x0000, 0x00, 0x01, &[],
    );
    let pkt = DrawMobile::from_bytes(&data).expect("should parse (no items)");
    assert!(pkt.items.is_empty());
}

// ── SendSpeech (0x1C) ─────────────────────────────────────────────────────

use packets::speech::{SendSpeech, TalkRequest};

#[test]
fn send_speech_standard_roundtrip() {
    // Standard layout: name in 30-byte field, message after header.
    let pkt = SendSpeech {
        serial: 0x0000_0001,
        model: 0x0190,
        speech_type: packets::speech::SpeechType::Normal,
        color: 0x0048,
        font: 0x0003,
        name: "Bob".to_string(),
        message: "Hello world".to_string(),
    };
    let bytes = pkt.to_bytes();
    let decoded = SendSpeech::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.name, "Bob");
    assert_eq!(decoded.message, "Hello world");
    assert_eq!(decoded.serial, pkt.serial);
}

#[test]
fn send_speech_system_message_no_trailing_message() {
    // Non-standard shard layout: message packed inside name[30], no bytes after.
    // Verbatim bytes from the bug report.
    let data = [
        0x1Cu8, 0x00, 0x2C,           // id + len=44
        0x00, 0x00, 0x00, 0x00,       // serial=0
        0x00, 0x00,                   // model=0
        0x00,                         // type=Normal
        0xFF, 0xFF,                   // color=0xFFFF
        0xFF, 0xFF,                   // font=0xFFFF
        // name[30]: "SYSTEM\0updateAccountCurrency\0u"
        b'S', b'Y', b'S', b'T', b'E', b'M', 0x00,
        b'u', b'p', b'd', b'a', b't', b'e', b'A', b'c', b'c',
        b'o', b'u', b'n', b't', b'C', b'u', b'r', b'r', b'e',
        b'n', b'c', b'y', 0x00, b'u',
    ];
    assert_eq!(data.len(), 44);
    let pkt = SendSpeech::from_bytes(&data).expect("should parse packed-name packet");
    assert_eq!(pkt.serial, 0);
    assert_eq!(pkt.name, "SYSTEM");
    assert_eq!(pkt.message, "updateAccountCurrency");
}

#[test]
fn send_speech_lossy_message() {
    // Message contains invalid UTF-8 — should decode lossy.
    let mut data = Vec::new();
    data.push(0x1Cu8);
    data.extend_from_slice(&[0x00, 0x00]); // len placeholder
    data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // serial
    data.extend_from_slice(&[0x01, 0x90]); // model
    data.push(0x00); // type
    data.extend_from_slice(&[0x00, 0x48]); // color
    data.extend_from_slice(&[0x00, 0x03]); // font
    // name[30]: "Npc\0" + padding
    data.extend_from_slice(b"Npc\x00");
    data.extend_from_slice(&[0u8; 26]);
    // message: invalid UTF-8 + null
    data.extend_from_slice(&[0xFF, 0xFE, 0x41, 0x00]);
    let total = data.len() as u16;
    data[1] = (total >> 8) as u8;
    data[2] = (total & 0xFF) as u8;

    let pkt = SendSpeech::from_bytes(&data).expect("should parse lossy message");
    assert_eq!(pkt.name, "Npc");
    // Message should contain U+FFFD replacements, not panic
    assert!(pkt.message.contains('\u{FFFD}') || !pkt.message.is_empty());
}

// ── TalkRequest (0x03) ───────────────────────────────────────────────────

#[test]
fn talk_request_standard() {
    let pkt = TalkRequest::new(
        packets::speech::SpeechType::Normal,
        0x0048, 0x0003, "Hi there",
    );
    let bytes = pkt.to_bytes();
    let decoded = TalkRequest::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.message, "Hi there");
    assert_eq!(decoded.color, 0x0048);
}

#[test]
fn talk_request_binary_garbage_lossy() {
    // Verbatim bytes from the bug report that previously failed with
    // "invalid string data".
    let data = [
        0x03u8, 0x00, 0x28,           // id + len=40
        0x20,                         // type=0x20 (Unknown)
        0x00, 0x34,                   // color
        0x00, 0x03,                   // font
        // message: binary garbage ending with 0x00
        0xDB, 0x13, 0x14, 0x3F, 0x45, 0x2C, 0x69, 0x38,
        0x79, 0x69, 0x0D, 0x61, 0x39, 0xE9, 0x75, 0x7C,
        0xC0, 0x1E, 0x71, 0x4F, 0x31, 0x34, 0x09, 0x41,
        0x1E, 0x18, 0x78, 0x1B, 0x59, 0x80, 0x5B, 0x00,
    ];
    let pkt = TalkRequest::from_bytes(&data).expect("should parse binary-garbage message");
    assert_eq!(pkt.color, 0x0034);
    assert_eq!(pkt.font, 0x0003);
    // Message should be non-empty (lossy decoded) and not panic.
    assert!(!pkt.message.is_empty());
}

// ── 0xB3 ChatText: JoinConference with corrupt surrogate after closing " ──

#[test]
fn chat_text_join_conference_corrupt_surrogate_lossy() {
    use packets::chat::ChatText;
    // Verbatim bytes from bug report:
    // B3 00 3F 52 55 53 00  00 62
    // 00 22 00 48 00 65 00 6C 00 70 00 22   — "Help"
    // 45 76 A9 DF 00 00                     — corrupt surrogate pair instead of 0x0020 + 0x0000
    // 6F 00 70 00 F0 F0 00 00 42 00 72 00   — further garbage
    // 6F 00 6B 00 65 00 72 00 02 02 00 00
    // 6F 00 2E 76 A9 DF 00 00 67 00 00
    let data: &[u8] = &[
        0xB3, 0x00, 0x3F,
        0x52, 0x55, 0x53, 0x00,   // language = "RUS\0"
        0x00, 0x62,               // cmd_type = JoinConference
        0x00, 0x22,               // opening "
        0x00, 0x48, 0x00, 0x65, 0x00, 0x6C, 0x00, 0x70,  // "Help"
        0x00, 0x22,               // closing "
        0x45, 0x76, 0xA9, 0xDF, 0x00, 0x00,  // corrupt surrogate instead of space+null
        0x6F, 0x00, 0x70, 0x00, 0xF0, 0xF0, 0x00, 0x00,
        0x42, 0x00, 0x72, 0x00, 0x6F, 0x00, 0x6B, 0x00,
        0x65, 0x00, 0x72, 0x00, 0x02, 0x02, 0x00, 0x00,
        0x6F, 0x00, 0x2E, 0x76, 0xA9, 0xDF, 0x00, 0x00,
        0x67, 0x00, 0x00,
    ];
    let pkt = ChatText::from_bytes(data)
        .expect("should parse despite corrupt surrogate");
    match pkt {
        ChatText::JoinConference { language, name, .. } => {
            assert_eq!(language, 0x52555300);
            assert_eq!(name, "Help");
        }
        other => panic!("expected JoinConference, got {:?}", other),
    }
}

// ── 0xA5 OpenWebBrowser ───────────────────────────────────────────────────

#[test]
fn open_web_browser_roundtrip() {
    use packets::system::OpenWebBrowser;
    let pkt = OpenWebBrowser::new("https://example.com");
    let bytes = encode_packet(&pkt);
    let mut expected = pkt.clone();
    expected.len = bytes.len() as u16;
    let decoded = OpenWebBrowser::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, expected);
}

#[test]
fn open_web_browser_wire_format() {
    use packets::system::OpenWebBrowser;
    let pkt = OpenWebBrowser::new("hi");
    let bytes = encode_packet(&pkt);
    // id(1) + len(2) + "hi"(2) + null(1) = 6
    assert_eq!(bytes.len(), 6);
    assert_eq!(bytes[0], 0xA5);
    assert_eq!(bytes[1], 0x00);
    assert_eq!(bytes[2], 0x06);
    assert_eq!(&bytes[3..], b"hi\0");
}

#[test]
fn chat_text_join_conference_no_password() {
    use packets::chat::ChatText;
    // Verbatim bytes from bug report:
    // B3 00 17 52 55 53 00  00 62
    // 00 22 00 48 00 65 00 6C 00 70 00 22  — "Help"
    // 00 00                                — no space, no password, just null terminator
    let data: &[u8] = &[
        0xB3, 0x00, 0x17,
        0x52, 0x55, 0x53, 0x00,   // language = "RUS\0"
        0x00, 0x62,               // cmd_type = JoinConference
        0x00, 0x22,               // opening "
        0x00, 0x48, 0x00, 0x65, 0x00, 0x6C, 0x00, 0x70,  // "Help"
        0x00, 0x22,               // closing "
        0x00, 0x00,               // null terminator (no password)
    ];
    let pkt = ChatText::from_bytes(data)
        .expect("should parse JoinConference without password");
    match pkt {
        ChatText::JoinConference { language, name, password } => {
            assert_eq!(language, 0x52555300);
            assert_eq!(name, "Help");
            assert_eq!(password, "");
        }
        other => panic!("expected JoinConference, got {:?}", other),
    }
}

// ── 0x34 GetMobileStatus ──────────────────────────────────────────────────

#[test]
fn get_mobile_status_status_factory() {
    use packets::interaction::{GetMobileStatus, MobileStatusRequest};

    let pkt = GetMobileStatus::status(0xDEADBEEF);
    assert_eq!(pkt.pattern, 0xEDEDEDED);
    assert_eq!(pkt.request_type, MobileStatusRequest::Status);
    assert_eq!(pkt.serial, 0xDEADBEEF);
}

#[test]
fn get_mobile_status_skills_factory() {
    use packets::interaction::{GetMobileStatus, MobileStatusRequest};

    let pkt = GetMobileStatus::skills(0x00000001);
    assert_eq!(pkt.pattern, 0xEDEDEDED);
    assert_eq!(pkt.request_type, MobileStatusRequest::Skills);
    assert_eq!(pkt.serial, 0x00000001);
}

#[test]
fn get_mobile_status_roundtrip() {
    use packets::interaction::{GetMobileStatus, MobileStatusRequest};
    use packets::traits::BasicPacket;

    let orig = GetMobileStatus::status(0x1234_5678);
    let bytes = orig.to_bytes();
    let decoded = GetMobileStatus::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.request_type, MobileStatusRequest::Status);
    assert_eq!(decoded.serial, 0x1234_5678);
    assert_eq!(decoded.pattern, 0xEDEDEDED);
}

#[test]
fn get_mobile_status_wire_format() {
    use packets::interaction::GetMobileStatus;
    use packets::traits::BasicPacket;

    let pkt = GetMobileStatus::skills(0x0000_0042);
    let bytes = pkt.to_bytes();
    assert_eq!(bytes.len(), 10);
    assert_eq!(bytes[0], 0x34);
    assert_eq!(&bytes[1..5], &[0xED, 0xED, 0xED, 0xED]);
    assert_eq!(bytes[5], 0x05);
    assert_eq!(&bytes[6..10], &[0x00, 0x00, 0x00, 0x42]);
}

#[test]
fn get_mobile_status_unknown_request_type() {
    use packets::interaction::{GetMobileStatus, MobileStatusRequest};
    use packets::traits::BasicPacket;

    let raw: &[u8] = &[
        0x34,
        0xED, 0xED, 0xED, 0xED,
        0xFF,
        0x00, 0x00, 0x00, 0x01,
    ];
    let pkt = GetMobileStatus::from_bytes(raw).unwrap();
    assert_eq!(pkt.request_type, MobileStatusRequest::Unknown(0xFF));
    assert_eq!(pkt.serial, 0x00000001);
}

// ── 0xF5 NewMapMessage ────────────────────────────────────────────────────

#[test]
fn new_map_message_roundtrip() {
    use packets::map::NewMapMessage;
    use packets::traits::BasicPacket;

    let orig = NewMapMessage {
        id: 0xF5,
        map_serial: 0x4000_0001,
        gump_art: 0x139D,
        upper_left_x: 10,
        upper_left_y: 20,
        lower_right_x: 630,
        lower_right_y: 400,
        gump_width: 620,
        gump_height: 380,
        facet_id: 1,
    };
    let bytes = orig.to_bytes();
    let decoded = NewMapMessage::from_bytes(&bytes).unwrap();
    assert_eq!(orig, decoded);
}

#[test]
fn new_map_message_wire_format() {
    use packets::map::NewMapMessage;
    use packets::traits::BasicPacket;

    let pkt = NewMapMessage {
        id: 0xF5,
        map_serial: 0x4000_0001,
        gump_art: 0x139D,
        upper_left_x: 0x0000,
        upper_left_y: 0x0000,
        lower_right_x: 0x0276,
        lower_right_y: 0x01F4,
        gump_width: 0x0270,
        gump_height: 0x01F0,
        facet_id: 0x0002,
    };
    let bytes = pkt.to_bytes();

    assert_eq!(bytes.len(), 21);
    assert_eq!(bytes[0], 0xF5);
    assert_eq!(&bytes[1..5],   &[0x40, 0x00, 0x00, 0x01]); // map_serial
    assert_eq!(&bytes[5..7],   &[0x13, 0x9D]);              // gump_art
    assert_eq!(&bytes[7..9],   &[0x00, 0x00]);              // upper_left_x
    assert_eq!(&bytes[9..11],  &[0x00, 0x00]);              // upper_left_y
    assert_eq!(&bytes[11..13], &[0x02, 0x76]);              // lower_right_x
    assert_eq!(&bytes[13..15], &[0x01, 0xF4]);              // lower_right_y
    assert_eq!(&bytes[15..17], &[0x02, 0x70]);              // gump_width
    assert_eq!(&bytes[17..19], &[0x01, 0xF0]);              // gump_height
    assert_eq!(&bytes[19..21], &[0x00, 0x02]);              // facet_id
}

// ── 0x7C OpenDialogBox / 0x7D ResponseToDialogBox ────────────────────────

#[test]
fn open_dialog_box_roundtrip() {
    use packets::gump::{DialogEntry, OpenDialogBox};
    use packets::traits::ManualPacket;

    let orig = OpenDialogBox {
        dialog_id: 0xDEAD_BEEF,
        menu_id: 0x0042,
        question: "Choose wisely".to_string(),
        entries: vec![
            DialogEntry { model_id: 0x139D, color: 0x0000, text: "Option A".to_string() },
            DialogEntry { model_id: 0x0000, color: 0x0035, text: "Option B".to_string() },
        ],
    };
    let bytes = orig.to_bytes();
    let decoded = OpenDialogBox::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.dialog_id, orig.dialog_id);
    assert_eq!(decoded.menu_id, orig.menu_id);
    assert_eq!(decoded.question, orig.question);
    assert_eq!(decoded.entries.len(), 2);
    assert_eq!(decoded.entries[0].model_id, 0x139D);
    assert_eq!(decoded.entries[0].text, "Option A");
    assert_eq!(decoded.entries[1].color, 0x0035);
    assert_eq!(decoded.entries[1].text, "Option B");
}

#[test]
fn open_dialog_box_wire_format() {
    use packets::gump::{DialogEntry, OpenDialogBox};
    use packets::traits::ManualPacket;

    let pkt = OpenDialogBox {
        dialog_id: 0x0000_0001,
        menu_id: 0x0002,
        question: "Hi".to_string(),
        entries: vec![
            DialogEntry { model_id: 0x0010, color: 0x0020, text: "Yes".to_string() },
        ],
    };
    let bytes = pkt.to_bytes();

    // id
    assert_eq!(bytes[0], 0x7C);
    // total length (big-endian u16): 1+2+4+2+1+2+1+2+2+1+3 = 21
    let total_len = u16::from_be_bytes([bytes[1], bytes[2]]);
    assert_eq!(total_len as usize, bytes.len());
    // dialog_id
    assert_eq!(&bytes[3..7], &[0x00, 0x00, 0x00, 0x01]);
    // menu_id
    assert_eq!(&bytes[7..9], &[0x00, 0x02]);
    // question length + "Hi"
    assert_eq!(bytes[9], 2);
    assert_eq!(&bytes[10..12], b"Hi");
    // entry count
    assert_eq!(bytes[12], 1);
    // entry: model_id, color, text_len, text
    assert_eq!(&bytes[13..15], &[0x00, 0x10]);
    assert_eq!(&bytes[15..17], &[0x00, 0x20]);
    assert_eq!(bytes[17], 3);
    assert_eq!(&bytes[18..21], b"Yes");
}

#[test]
fn open_dialog_box_empty_entries() {
    use packets::gump::OpenDialogBox;
    use packets::traits::ManualPacket;

    let pkt = OpenDialogBox {
        dialog_id: 0,
        menu_id: 0,
        question: String::new(),
        entries: vec![],
    };
    let bytes = pkt.to_bytes();
    let decoded = OpenDialogBox::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.entries.len(), 0);
    assert_eq!(decoded.question, "");
}

#[test]
fn response_to_dialog_box_roundtrip() {
    use packets::gump::ResponseToDialogBox;
    use packets::traits::ManualPacket;

    let orig = ResponseToDialogBox {
        dialog_id: 0xDEAD_BEEF,
        menu_id: 0x0042,
        index: 2,
        model_id: 0x139D,
        color: 0x0035,
    };
    let bytes = orig.to_bytes();
    assert_eq!(bytes.len(), 13);
    let decoded = ResponseToDialogBox::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
}

#[test]
fn response_to_dialog_box_wire_format() {
    use packets::gump::ResponseToDialogBox;
    use packets::traits::ManualPacket;

    let pkt = ResponseToDialogBox {
        dialog_id: 0x0000_0001,
        menu_id: 0x0002,
        index: 1,
        model_id: 0x0010,
        color: 0x0020,
    };
    let bytes = pkt.to_bytes();

    assert_eq!(bytes.len(), 13);
    assert_eq!(bytes[0], 0x7D);
    assert_eq!(&bytes[1..5],  &[0x00, 0x00, 0x00, 0x01]); // dialog_id
    assert_eq!(&bytes[5..7],  &[0x00, 0x02]);              // menu_id
    assert_eq!(&bytes[7..9],  &[0x00, 0x01]);              // index
    assert_eq!(&bytes[9..11], &[0x00, 0x10]);              // model_id
    assert_eq!(&bytes[11..13],&[0x00, 0x20]);              // color
}

#[test]
fn response_to_dialog_box_cancel() {
    use packets::gump::ResponseToDialogBox;
    use packets::traits::ManualPacket;

    // index=0 means cancelled
    let pkt = ResponseToDialogBox {
        dialog_id: 0xCAFE_BABE,
        menu_id: 1,
        index: 0,
        model_id: 0,
        color: 0,
    };
    let bytes = pkt.to_bytes();
    let decoded = ResponseToDialogBox::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.index, 0);
}

// ── 0x6F SecureTrading ────────────────────────────────────────────────────

#[test]
fn secure_trading_start_with_name_roundtrip() {
    use packets::trade::{SecureTrading, TradingAction};
    use packets::traits::ManualPacket;

    let orig = SecureTrading {
        action: TradingAction::Start,
        player_serial:     0x0000_0001,
        container1_serial: 0x4000_0010,
        container2_serial: 0x4000_0020,
        player_name: "PlayerOne".to_string(),
    };
    let bytes = orig.to_bytes();
    let decoded = SecureTrading::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
}

#[test]
fn secure_trading_start_without_name_roundtrip() {
    use packets::trade::{SecureTrading, TradingAction};
    use packets::traits::ManualPacket;

    let orig = SecureTrading {
        action: TradingAction::Start,
        player_serial:     0x0000_0002,
        container1_serial: 0x4000_0011,
        container2_serial: 0x4000_0021,
        player_name: String::new(),
    };
    let bytes = orig.to_bytes();
    let decoded = SecureTrading::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
}

#[test]
fn secure_trading_start_wire_format() {
    use packets::trade::{SecureTrading, TradingAction};
    use packets::traits::ManualPacket;

    let pkt = SecureTrading {
        action: TradingAction::Start,
        player_serial:     0x0000_0001,
        container1_serial: 0x4000_0010,
        container2_serial: 0x4000_0020,
        player_name: "Bob".to_string(),
    };
    let bytes = pkt.to_bytes();

    assert_eq!(bytes[0], 0x6F);
    let total_len = u16::from_be_bytes([bytes[1], bytes[2]]) as usize;
    assert_eq!(total_len, bytes.len());
    assert_eq!(bytes[3], 0x00);                              // action = Start
    assert_eq!(&bytes[4..8],   &[0x00, 0x00, 0x00, 0x01]);  // player_serial
    assert_eq!(&bytes[8..12],  &[0x40, 0x00, 0x00, 0x10]);  // container1_serial
    assert_eq!(&bytes[12..16], &[0x40, 0x00, 0x00, 0x20]);  // container2_serial
    assert_eq!(bytes[16], 1);                                // has_name = true
    assert_eq!(&bytes[17..21], b"Bob\0");                    // name + null
}

#[test]
fn secure_trading_cancel_roundtrip() {
    use packets::trade::{SecureTrading, TradingAction};
    use packets::traits::ManualPacket;

    let orig = SecureTrading {
        action: TradingAction::Cancel,
        player_serial:     0x0000_0003,
        container1_serial: 0x4000_0012,
        container2_serial: 0x4000_0022,
        player_name: String::new(),
    };
    let bytes = orig.to_bytes();
    // id(1)+len(2)+action(1)+serials(12)+has_name(1) = 17
    assert_eq!(bytes.len(), 17);
    assert_eq!(bytes[3], 0x01); // action = Cancel
    let decoded = SecureTrading::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
}

#[test]
fn secure_trading_update_roundtrip() {
    use packets::trade::{SecureTrading, TradingAction};
    use packets::traits::ManualPacket;

    let orig = SecureTrading {
        action: TradingAction::Update,
        player_serial:     0x0000_0004,
        container1_serial: 0x4000_0013,
        container2_serial: 0x4000_0023,
        player_name: String::new(),
    };
    let bytes = orig.to_bytes();
    assert_eq!(bytes[3], 0x02); // action = Update
    let decoded = SecureTrading::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
}

#[test]
fn secure_trading_unknown_action_preserved() {
    use packets::trade::{SecureTrading, TradingAction};
    use packets::traits::ManualPacket;

    let raw: &[u8] = &[
        0x6F,
        0x00, 0x11,              // length = 17
        0x99,                    // action = unknown
        0x00, 0x00, 0x00, 0x01,
        0x40, 0x00, 0x00, 0x10,
        0x40, 0x00, 0x00, 0x20,
        0x00,                    // has_name = false
    ];
    let pkt = SecureTrading::from_bytes(raw).unwrap();
    assert_eq!(pkt.action, TradingAction::Unknown(0x99));
    assert_eq!(pkt.player_serial, 0x0000_0001);
}

// ── 0x27 RejectMoveItem ───────────────────────────────────────────────────

#[test]
fn reject_move_item_roundtrip() {
    use packets::interaction::{RejectMoveItem, RejectMoveItemReason};
    use packets::traits::BasicPacket;

    for reason in [
        RejectMoveItemReason::CannotLift,
        RejectMoveItemReason::OutOfRange,
        RejectMoveItemReason::OutOfSight,
        RejectMoveItemReason::BelongsToAnother,
        RejectMoveItemReason::AlreadyHolding,
        RejectMoveItemReason::EmptyMessage,
    ] {
        let orig = RejectMoveItem::new(reason);
        let bytes = orig.to_bytes();
        let decoded = RejectMoveItem::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, orig);
    }
}

#[test]
fn reject_move_item_wire_format() {
    use packets::interaction::{RejectMoveItem, RejectMoveItemReason};
    use packets::traits::BasicPacket;

    let pkt = RejectMoveItem::new(RejectMoveItemReason::BelongsToAnother);
    let bytes = pkt.to_bytes();

    assert_eq!(bytes.len(), 2);
    assert_eq!(bytes[0], 0x27);
    assert_eq!(bytes[1], 0x03); // BelongsToAnother
}

#[test]
fn reject_move_item_unknown_reason() {
    use packets::interaction::{RejectMoveItem, RejectMoveItemReason};
    use packets::traits::BasicPacket;

    let raw: &[u8] = &[0x27, 0xFF];
    let pkt = RejectMoveItem::from_bytes(raw).unwrap();
    assert_eq!(pkt.reason, RejectMoveItemReason::Unknown(0xFF));
}

#[test]
fn reject_move_item_factory() {
    use packets::interaction::{RejectMoveItem, RejectMoveItemReason};

    let pkt = RejectMoveItem::new(RejectMoveItemReason::AlreadyHolding);
    assert_eq!(pkt.id, 0x27);
    assert_eq!(pkt.reason, RejectMoveItemReason::AlreadyHolding);
}

// ── 0x23 DraggingOfItem ───────────────────────────────────────────────────

#[test]
fn dragging_of_item_roundtrip() {
    use packets::interaction::DraggingOfItem;
    use packets::traits::BasicPacket;

    let orig = DraggingOfItem::serial_to_serial(
        0x0F3D, // model
        1,      // stack_count
        0x4000_0001, 10, 20, 0,  // source
        0x4000_0002, 30, 40, 5,  // target
    );
    let bytes = orig.to_bytes();
    let decoded = DraggingOfItem::from_bytes(&bytes).unwrap();
    assert_eq!(orig, decoded);
}

#[test]
fn dragging_of_item_wire_format() {
    use packets::interaction::DraggingOfItem;
    use packets::traits::BasicPacket;

    let pkt = DraggingOfItem::world_to_world(
        0x0F3D, // model
        5,      // stack_count
        0x0064, 0x00C8, 0x00,   // source x,y,z
        0x012C, 0x0190, 0x0A,   // target x,y,z
    );
    let bytes = pkt.to_bytes();

    // Fixed size: 26 bytes
    assert_eq!(bytes.len(), 26);

    // Byte 0: packet id
    assert_eq!(bytes[0], 0x23);

    // Bytes 1-2: model (big-endian)
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 0x0F3D);

    // Bytes 3-5: unknown1 (zeroed by default)
    assert_eq!(&bytes[3..6], &[0x00, 0x00, 0x00]);

    // Bytes 6-7: stack_count
    assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 5);

    // Bytes 8-11: source_id (0 = world)
    assert_eq!(u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]), 0);

    // Bytes 12-13: source_x
    assert_eq!(u16::from_be_bytes([bytes[12], bytes[13]]), 0x0064);

    // Bytes 14-15: source_y
    assert_eq!(u16::from_be_bytes([bytes[14], bytes[15]]), 0x00C8);

    // Byte 16: source_z
    assert_eq!(bytes[16], 0x00);

    // Bytes 17-20: target_id (0 = world)
    assert_eq!(u32::from_be_bytes([bytes[17], bytes[18], bytes[19], bytes[20]]), 0);

    // Bytes 21-22: target_x
    assert_eq!(u16::from_be_bytes([bytes[21], bytes[22]]), 0x012C);

    // Bytes 23-24: target_y
    assert_eq!(u16::from_be_bytes([bytes[23], bytes[24]]), 0x0190);

    // Byte 25: target_z
    assert_eq!(bytes[25], 0x0A);
}

#[test]
fn dragging_of_item_bad_id_rejected() {
    use packets::interaction::DraggingOfItem;
    use packets::traits::BasicPacket;

    let mut bytes = DraggingOfItem::world_to_world(
        0x0F3D, 1, 0, 0, 0, 0, 0, 0,
    ).to_bytes().to_vec();
    bytes[0] = 0xFF; // corrupt packet id
    assert!(DraggingOfItem::from_bytes(&bytes).is_err());
}

#[test]
fn dragging_of_item_truncated_rejected() {
    use packets::interaction::DraggingOfItem;
    use packets::traits::BasicPacket;

    let bytes: &[u8] = &[0x23, 0x0F, 0x3D]; // only 3 bytes — truncated
    assert!(DraggingOfItem::from_bytes(bytes).is_err());
}



#[test]
fn object_info_sa_item_roundtrip() {
    use packets::world::ObjectInfoSA;
    use packets::traits::BasicPacket;

    let orig = ObjectInfoSA::item(
        0x4000_0001, // serial
        0x0E21,      // graphic (keg)
        0,           // graphic_inc
        1,           // amount
        100, 200,    // x, y
        0,           // z
        0,           // direction
        0x0000,      // hue
        0x20,        // flags (movable)
        0x0000,      // highlight
    );
    let bytes = orig.to_bytes();
    assert_eq!(bytes.len(), 26);
    assert_eq!(bytes[0], 0xF3);
    let decoded = ObjectInfoSA::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
}

#[test]
fn object_info_sa_multi_roundtrip() {
    use packets::world::ObjectInfoSA;
    use packets::traits::BasicPacket;

    let orig = ObjectInfoSA::multi(0x4000_0042, 0x00CC, 512, 768, 0);
    let bytes = orig.to_bytes();
    assert_eq!(bytes.len(), 26);
    let decoded = ObjectInfoSA::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
    assert_eq!(decoded.direction, 0);
    assert_eq!(decoded.highlight, 0);
}

#[test]
fn object_info_sa_wire_layout() {
    use packets::world::ObjectInfoSA;
    use packets::traits::BasicPacket;

    let pkt = ObjectInfoSA::item(
        0x4000_0001, 0x0E21, 0x02, 5,
        0x0064, 0x00C8, -5, 0x01, 0x0ABC, 0x20, 0x0001,
    );
    let b = pkt.to_bytes();
    // id
    assert_eq!(b[0], 0xF3);
    // _header = 0x0001
    assert_eq!(b[1], 0x00); assert_eq!(b[2], 0x01);
    // data_type = Item (0x00)
    assert_eq!(b[3], 0x00);
    // serial = 0x40000001
    assert_eq!(&b[4..8], &[0x40, 0x00, 0x00, 0x01]);
    // graphic = 0x0E21
    assert_eq!(&b[8..10], &[0x0E, 0x21]);
    // graphic_inc = 0x02
    assert_eq!(b[10], 0x02);
    // amount = 5
    assert_eq!(&b[11..13], &[0x00, 0x05]);
    // amount2 = 5 (copy)
    assert_eq!(&b[13..15], &[0x00, 0x05]);
    // x = 0x0064
    assert_eq!(&b[15..17], &[0x00, 0x64]);
    // y = 0x00C8
    assert_eq!(&b[17..19], &[0x00, 0xC8]);
    // z = -5 = 0xFB
    assert_eq!(b[19], 0xFB);
    // direction = 0x01
    assert_eq!(b[20], 0x01);
    // hue = 0x0ABC
    assert_eq!(&b[21..23], &[0x0A, 0xBC]);
    // flags = 0x20
    assert_eq!(b[23], 0x20);
    // highlight = 0x0001
    assert_eq!(&b[24..26], &[0x00, 0x01]);
}

// ── 0xF7 PacketList ───────────────────────────────────────────────────────

#[test]
fn packet_list_roundtrip() {
    use packets::world::{ObjectInfoSA, PacketList};
    use packets::traits::ManualPacket;

    let items = vec![
        ObjectInfoSA::item(0x4000_0001, 0x0E21, 0, 1, 10, 20, 0, 0, 0, 0x20, 0),
        ObjectInfoSA::item(0x4000_0002, 0x0E22, 0, 3, 11, 21, 1, 0, 0, 0x00, 0),
    ];
    let orig = PacketList::new(items);
    let bytes = orig.to_bytes();

    // 5 header + 2 × 26
    assert_eq!(bytes.len(), 57);
    assert_eq!(bytes[0], 0xF7);
    // length field
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 57);
    // count = 2
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 2);
    // first sub-packet id
    assert_eq!(bytes[5], 0xF3);

    let decoded = PacketList::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.items.len(), 2);
    assert_eq!(decoded, orig);
}

#[test]
fn packet_list_empty() {
    use packets::world::PacketList;
    use packets::traits::ManualPacket;

    let orig = PacketList::new(vec![]);
    let bytes = orig.to_bytes();
    assert_eq!(bytes.len(), 5);
    assert_eq!(bytes[0], 0xF7);
    assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 0);

    let decoded = PacketList::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.items.len(), 0);
}

#[test]
fn packet_list_unknown_sub_id_stops_early() {
    use packets::world::PacketList;
    use packets::traits::ManualPacket;

    // count=2 but first byte of first sub-packet is 0xAB (unknown)
    let raw: &[u8] = &[
        0xF7,
        0x00, 0x09,  // length = 9 (header only + 2 padding + count)
        0x00, 0x02,  // count = 2
        0xAB,        // unknown sub-id → stop
        0x00, 0x00, 0x00, // padding (won't be read)
    ];
    let pkt = PacketList::from_bytes(raw).unwrap();
    assert_eq!(pkt.items.len(), 0);
}

// ── RemoveWaypoint (0xE6) ─────────────────────────────────────────────────

#[test]
fn remove_waypoint_roundtrip() {
    use packets::world::RemoveWaypoint;

    let orig = RemoveWaypoint { id: 0xE6, serial: 0x0012_3456 };
    let bytes = encode_packet(&orig);
    assert_eq!(bytes.len(), 5);
    assert_eq!(bytes[0], 0xE6);
    let decoded = RemoveWaypoint::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
}

#[test]
fn remove_waypoint_wire_format() {
    use packets::world::RemoveWaypoint;

    let pkt = RemoveWaypoint { id: 0xE6, serial: 0xDEAD_BEEF };
    let bytes = encode_packet(&pkt);
    assert_eq!(bytes.as_ref(), &[0xE6, 0xDE, 0xAD, 0xBE, 0xEF]);
}

// ── DisplayWaypoint (0xE5) ────────────────────────────────────────────────

#[test]
fn display_waypoint_roundtrip() {
    use packets::world::{DisplayWaypoint, WaypointType};
    use packets::traits::ManualPacket;

    let orig = DisplayWaypoint {
        serial: 0x0000_0001,
        x: 1234,
        y: 5678,
        z: -5,
        map: 0,
        waypoint_type: WaypointType::Quest,
        ignore_object: true,
        cliloc: 0x0005_A6CF,
        name: "Treasure".to_string(),
    };
    let bytes = orig.to_bytes();
    let decoded = DisplayWaypoint::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.serial, orig.serial);
    assert_eq!(decoded.x, orig.x);
    assert_eq!(decoded.y, orig.y);
    assert_eq!(decoded.z, orig.z);
    assert_eq!(decoded.map, orig.map);
    assert_eq!(decoded.waypoint_type, orig.waypoint_type);
    assert_eq!(decoded.ignore_object, orig.ignore_object);
    assert_eq!(decoded.cliloc, orig.cliloc);
    assert_eq!(decoded.name, orig.name);
}

#[test]
fn display_waypoint_empty_name_roundtrip() {
    use packets::world::{DisplayWaypoint, WaypointType};
    use packets::traits::ManualPacket;

    let orig = DisplayWaypoint {
        serial: 0xAABB_CCDD,
        x: 0,
        y: 0,
        z: 0,
        map: 1,
        waypoint_type: WaypointType::Corpse,
        ignore_object: false,
        cliloc: 0,
        name: String::new(),
    };
    let bytes = orig.to_bytes();
    let decoded = DisplayWaypoint::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.name, "");
    assert_eq!(decoded.waypoint_type, WaypointType::Corpse);
    assert!(!decoded.ignore_object);
}

#[test]
fn display_waypoint_wire_format() {
    use packets::world::{DisplayWaypoint, WaypointType};
    use packets::traits::ManualPacket;

    // Empty name → wire should be exactly 21 bytes:
    // id(1) + len(2) + serial(4) + x(2) + y(2) + z(1) + map(1)
    // + type(2) + ignore(2) + cliloc(4) + null(2) = 23
    let pkt = DisplayWaypoint {
        serial: 0x0000_0001,
        x: 100,
        y: 200,
        z: 10,
        map: 0,
        waypoint_type: WaypointType::PartyMember,
        ignore_object: false,
        cliloc: 0,
        name: String::new(),
    };
    let bytes = pkt.to_bytes();
    assert_eq!(bytes[0], 0xE5);
    // length word (BE) at bytes[1..3]
    let len = u16::from_be_bytes([bytes[1], bytes[2]]) as usize;
    assert_eq!(len, bytes.len());
    // type field at offset 13..15 → 0x0001 (PartyMember)
    let ty = u16::from_be_bytes([bytes[13], bytes[14]]);
    assert_eq!(ty, 0x0001);
    // ignore field at offset 15..17 → 0x0000
    let ign = u16::from_be_bytes([bytes[15], bytes[16]]);
    assert_eq!(ign, 0);
    // last 2 bytes — null terminator (LE u16 = 0x0000)
    let last = &bytes[bytes.len() - 2..];
    assert_eq!(last, &[0x00, 0x00]);
}

// ── Particle3DEffect (0xC7) ────────────────────────────────────────────────

#[test]
fn particle_3d_effect_wire_size() {
    use packets::world::Particle3DEffect;

    let pkt = Particle3DEffect {
        id: 0xC7,
        direction_type: 0x00,
        source_serial: 0x0000_0001,
        target_serial: 0x0000_0002,
        model: 0x0373,
        x: 100, y: 200, z: 10,
        target_x: 110, target_y: 210, target_z: 10,
        speed: 5, duration: 10,
        unk: 0x0000,
        fixed_direction: 0,
        explode: 0,
        hue: 0,
        render_mode: 0,
        particle_effect: 0x0042,
        particle_explode: 0x0000,
        particle_move_effect: 0x0000,
        particle_item_id: 0,
        layer: Layer::MovingEffect,
        particle_unk_effect: 0x0000,
    };
    let bytes = encode_packet(&pkt);
    assert_eq!(bytes.len(), 49);
    assert_eq!(bytes[0], 0xC7);
}

#[test]
fn particle_3d_effect_roundtrip() {
    use packets::world::Particle3DEffect;

    let orig = Particle3DEffect {
        id: 0xC7,
        direction_type: 0x00,
        source_serial: 0xAABB_CCDD,
        target_serial: 0x1122_3344,
        model: 0x03B2,
        x: 1500, y: 1600, z: -5,
        target_x: 1510, target_y: 1610, target_z: -3,
        speed: 8, duration: 15,
        unk: 0x0100,
        fixed_direction: 1,
        explode: 1,
        hue: 0x0000_04B0,
        render_mode: 0x0000_0000,
        particle_effect: 0x1234,
        particle_explode: 0x5678,
        particle_move_effect: 0x0000,
        particle_item_id: 0,
        layer: Layer::MovingEffect,
        particle_unk_effect: 0x0000,
    };
    let bytes = encode_packet(&orig);
    let decoded = Particle3DEffect::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, orig);
}

#[test]
fn particle_3d_effect_wire_format() {
    use packets::world::Particle3DEffect;

    let pkt = Particle3DEffect {
        id: 0xC7,
        direction_type: 0x02,
        source_serial: 0x0000_0001,
        target_serial: 0x0000_0000,
        model: 0x0000,
        x: 0, y: 0, z: 0,
        target_x: 0, target_y: 0, target_z: 0,
        speed: 0, duration: 0,
        unk: 0x0000,
        fixed_direction: 0,
        explode: 0,
        hue: 0,
        render_mode: 0,
        particle_effect: 0x00AB,
        particle_explode: 0x00CD,
        particle_move_effect: 0x0000,
        particle_item_id: 0xDEAD_BEEF,
        layer: Layer::LeftHand,
        particle_unk_effect: 0x0000,
    };
    let bytes = encode_packet(&pkt);
    assert_eq!(bytes.len(), 49);
    // particle_effect at offset 36..38
    let pe = u16::from_be_bytes([bytes[36], bytes[37]]);
    assert_eq!(pe, 0x00AB);
    // particle_explode at offset 38..40
    let pex = u16::from_be_bytes([bytes[38], bytes[39]]);
    assert_eq!(pex, 0x00CD);
    // particle_item_id at offset 42..46
    let pid = u32::from_be_bytes([bytes[42], bytes[43], bytes[44], bytes[45]]);
    assert_eq!(pid, 0xDEAD_BEEF);
    // layer at offset 46
    assert_eq!(bytes[46], 0x02);
}

// ── ObjectInfo (0x1A) ─────────────────────────────────────────────────────

#[test]
fn object_info_wire_header() {
    // Regression: encode was missing the 0x1A id byte and the length word,
    // writing raw object_id bytes at offset 0 instead.
    use packets::world::ObjectInfo;
    use packets::traits::ManualPacket;

    let pkt = ObjectInfo {
        object_id: 0xCEDB_B7A6,
        graphic: 0x0823,
        amount: None,
        graphic_increment: None,
        x: 0x0752,
        y: 0x0345,
        facing: None,
        z: -1,
        dye: None,
        flags: None,
    };
    let bytes = pkt.to_bytes();
    // id must be 0x1A at offset 0
    assert_eq!(bytes[0], 0x1A, "id byte missing");
    // length word at offset 1-2 must equal total length
    let len = u16::from_be_bytes([bytes[1], bytes[2]]) as usize;
    assert_eq!(len, bytes.len(), "length field mismatch");
}

#[test]
fn object_info_roundtrip_minimal() {
    use packets::world::ObjectInfo;
    use packets::traits::ManualPacket;

    // Simple item: no amount, no increment, no facing, no dye, no flags.
    // object_id has bit 31 clear → no amount.
    // Wire: 1A len(2) serial(4) graphic(2) x(2) y(2) z(1) = 14 bytes
    let orig = ObjectInfo {
        object_id: 0x4000_1234,
        graphic: 0x0E21,
        amount: None,
        graphic_increment: None,
        x: 0x0752,
        y: 0x0345,
        facing: None,
        z: -1,
        dye: None,
        flags: None,
    };
    let bytes = orig.to_bytes();
    assert_eq!(bytes[0], 0x1A, "id byte wrong");
    let decoded = ObjectInfo::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.object_id, orig.object_id);
    assert_eq!(decoded.graphic,   orig.graphic);
    assert_eq!(decoded.x,         orig.x);
    assert_eq!(decoded.y,         orig.y);
    assert_eq!(decoded.z,         orig.z);
    assert_eq!(decoded.amount,    None);
    assert_eq!(decoded.dye,       None);
    assert_eq!(decoded.flags,     None);
}

#[test]
fn object_info_roundtrip_with_amount_and_dye() {
    use packets::world::{ObjectInfo, ObjectInfoFlags};
    use packets::traits::ManualPacket;

    let orig = ObjectInfo {
        object_id: 0x0012_3456,
        graphic: 0x0E21,
        amount: Some(5),
        graphic_increment: None,
        x: 100,
        y: 200,
        facing: None,
        z: 10,
        dye: Some(0x04B0),
        flags: Some(ObjectInfoFlags(0x20)),
    };
    let bytes = orig.to_bytes();
    assert_eq!(bytes[0], 0x1A);
    let decoded = ObjectInfo::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.object_id,           orig.object_id);
    assert_eq!(decoded.amount,              orig.amount);
    assert_eq!(decoded.graphic,             orig.graphic);
    assert_eq!(decoded.x,                   orig.x);
    assert_eq!(decoded.y,                   orig.y);
    assert_eq!(decoded.z,                   orig.z);
    assert_eq!(decoded.dye,                 orig.dye);
    assert_eq!(decoded.flags.map(|f| f.0), orig.flags.map(|f| f.0));
}

// ── CharacterLocaleAndBody (0x1B) ─────────────────────────────────────────

#[test]
fn char_locale_and_body_roundtrip_real_packet() {
    // Regression: unknown2/unknown3 were #[binary(pad=4)] and were zeroed
    // on re-encode.  Real OSI traffic has 7F 00 00 00 in unknown3.
    use packets::character::CharacterLocaleAndBody;
    use packets::traits::BasicPacket;

    let raw: &[u8] = &[
        0x1B,
        0x04, 0x25, 0x78, 0xE7, // serial
        0x00, 0x00, 0x00, 0x00, // unknown0
        0x01, 0x90,             // body_type
        0x07, 0x56,             // x
        0x06, 0x09,             // y
        0x00,                   // _pad1
        0x00,                   // z
        0x00,                   // facing
        0x00, 0x00, 0x00, 0x00, // unknown2
        0x7F, 0x00, 0x00, 0x00, // unknown3 = 127.0.0.1 LE
        0x00,                   // _pad4
        0x18, 0x00,             // map_width_minus8
        0x10, 0x00,             // map_height
        0x00, 0x00,             // _pad5
        0x00, 0x00, 0x00, 0x00, // unknown6
    ];
    assert_eq!(raw.len(), 37);

    let pkt = CharacterLocaleAndBody::from_bytes(raw).unwrap();
    assert_eq!(pkt.serial,           0x042578E7);
    assert_eq!(pkt.unknown0,         0x00000000);
    assert_eq!(pkt.body_type,        0x0190);
    assert_eq!(pkt.x,                0x0756);
    assert_eq!(pkt.y,                0x0609);
    assert_eq!(pkt.z,                0i8);
    assert_eq!(pkt.facing,           0u8);
    assert_eq!(pkt.unknown2,         0x00000000);
    assert_eq!(pkt.unknown3,         0x7F000000);  // key field: must survive roundtrip
    assert_eq!(pkt.map_width_minus8, 0x1800);      // 6144+8 = 6152 wide (Felucca)
    assert_eq!(pkt.map_height,       0x1000);      // 4096 tall
    assert_eq!(pkt.unknown6,         0x00000000);

    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], raw,
        "roundtrip mismatch at byte {}",
        raw.iter().zip(reencoded.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(raw.len()),
    );
}

// ── SendGumpDialog (0xB0) ─────────────────────────────────────────────────

#[test]
fn send_gump_dialog_trailing_pad_roundtrip() {
    // Regression: server appends a trailing 0x00 after the last text line.
    // Previously this byte was silently dropped on re-encode, producing a
    // packet 1 byte shorter than the original.
    use packets::gump::SendGumpDialog;

    // Minimal 0xB0 packet: no commands (just null), 1 text line "A", + trailing 0x00.
    // Layout: id(1)+len(2)+serial(4)+gump_id(4)+x(4)+y(4)+cmd_len(2)+null(1)
    //        +num_lines(2)+line_len(2)+codeunit(2)+trailing(1) = 29 bytes
    let raw: &[u8] = &[
        0xB0,
        0x00, 0x1D,             // len = 29
        0x00, 0x00, 0x00, 0x01, // serial
        0x00, 0x00, 0x00, 0x02, // gump_id
        0x00, 0x00, 0x00, 0x00, // x
        0x00, 0x00, 0x00, 0x00, // y
        0x00, 0x01,             // cmd_len = 1 (just the null terminator)
        0x00,                   // null terminator of commands
        0x00, 0x01,             // num_lines = 1
        0x00, 0x01,             // text_len = 1 (one u16 code unit)
        0x00, 0x41,             // U+0041 = 'A' in BE UTF-16
        0x00,                   // trailing pad byte (server quirk)
    ];
    assert_eq!(raw.len(), 29);

    let pkt = SendGumpDialog::from_bytes(raw).unwrap();
    assert_eq!(pkt.text_lines.len(), 1);
    assert_eq!(&*pkt.text_lines[0].0, "A");
    assert_eq!(pkt.trailing_pad, vec![0x00]);

    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], raw,
        "roundtrip mismatch: trailing pad byte was dropped");
}

// ── WarMode (0x72) ────────────────────────────────────────────────────────

#[test]
fn war_mode_real_packet_roundtrip() {
    // Regression: #[binary(pad = 3)] was zero-filling bytes 2-4; real OSI
    // traffic has 0x32 at offset 3 (unknown2). Must survive roundtrip.
    use packets::traits::BasicPacket;
    let raw: &[u8] = &[0x72, 0x01, 0x00, 0x32, 0x00];
    let pkt = packets::system::WarMode::from_bytes(raw).unwrap();
    assert!(pkt.is_fighting());
    assert_eq!(pkt.unknown2, 0x32);
    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], raw);
}

// ── GameServerList (0xA8) — name with non-zero padding byte ───────────────

#[test]
fn game_server_list_name_quirk_roundtrip() {
    // Regression: FixedString<32> stripped bytes past the first \0, so a
    // server writing a non-zero byte in the name padding region (byte 31)
    // caused a 1-byte mismatch on re-encode. RawBytes<32> preserves verbatim.
    use packets::login::GameServerList;
    use packets::traits::BasicPacket;

    // Build the exact 46-byte real packet from the proxy log.
    let raw: &[u8] = &[
        0xA8, 0x00, 0x2E, 0xFF, // id, len=46, flag=0xFF
        0x00, 0x01,             // count = 1
        0x00, 0x01,             // entry[0].index = 1
        // entry[0].name = "Utopia" + 25 × 0x00 + 0x01  (32 bytes)
        b'U', b't', b'o', b'p', b'i', b'a',
        0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
        0x01,
        0x00,                   // full_percent
        0x00,                   // timezone
        0x04, 0xFD, 0x57, 0xC2, // ip
    ];
    assert_eq!(raw.len(), 46);

    let pkt = GameServerList::from_bytes(raw).unwrap();
    assert_eq!(pkt.servers.len(), 1);
    let entry = &pkt.servers[0];
    assert_eq!(entry.name.as_str_lossy(), "Utopia");
    assert_eq!(entry.name.0[31], 0x01, "quirk byte at name[31] must be preserved");

    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], raw,
        "roundtrip mismatch: non-zero padding byte in name was lost");
}

// ── AccountLogin (0x80) ───────────────────────────────────────────────────

#[test]
fn account_login_next_login_key_roundtrip() {
    // Regression: last byte (offset 61) was #[binary(pad = 1)] → always 0x00.
    // Real client sends 0xFF there (next_login_key). Must survive roundtrip.
    use packets::login::AccountLogin;
    use packets::traits::BasicPacket;

    let mut raw = [0u8; 62];
    raw[0] = 0x80;
    // account = "teo" at bytes 1..4
    raw[1] = b't'; raw[2] = b'e'; raw[3] = b'o';
    // password = "kuckfuck" at bytes 31..39
    raw[31] = b'k'; raw[32] = b'u'; raw[33] = b'c'; raw[34] = b'k';
    raw[35] = b'f'; raw[36] = b'u'; raw[37] = b'c'; raw[38] = b'k';
    // next_login_key at byte 61
    raw[61] = 0xFF;

    let pkt = AccountLogin::from_bytes(&raw).unwrap();
    assert_eq!(&*pkt.account, "teo");
    assert_eq!(&*pkt.password, "kuckfuck");
    assert_eq!(pkt.next_login_key, 0xFF);

    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], &raw[..],
        "roundtrip mismatch: next_login_key was zeroed");
}

// ── GeneralInfo 0x0019 ExtendedStats/Kr extra bytes ──────────────────────

#[test]
fn general_info_extended_stats_kr_extra_roundtrip() {
    // Regression: Kr variant with lock_flags=0x00 (animation=None) consumed
    // BYTE[1]+BYTE[4]=5 bytes but OSI sends 7 bytes (2 extra), causing
    // orig_len=19 vs re_len=17.

    let raw: &[u8] = &[
        0xBF, 0x00, 0x13,           // id, len=19
        0x00, 0x19,                 // sub_cmd = 0x0019
        0x05,                       // sub_sub = Kr
        0x00, 0x45, 0x19, 0x58,     // serial
        0x00,                       // _unk
        0x00,                       // lock_flags = 0x00 (no animation)
        0x00, 0x00, 0x00, 0x00, 0x00, // BYTE[1]+BYTE[4] consumed by decode
        0x00, 0x00,                 // 2 extra bytes from server
    ];
    assert_eq!(raw.len(), 19);

    let pkt = GeneralInfo::from_bytes(raw).unwrap();
    match &pkt {
        GeneralInfo::ExtendedStats(packets::system::ExtendedStatPayload::Kr {
            serial, lock_flags, animation, extra
        }) => {
            assert_eq!(*serial, 0x00451958);
            assert_eq!(*lock_flags, 0x00);
            assert!(animation.is_none());
            assert_eq!(extra.len(), 2, "2 extra bytes must be captured");
        }
        other => panic!("unexpected variant: {:?}", other),
    }

    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], raw,
        "roundtrip mismatch: extra bytes after Kr payload were dropped");
}

// ── StatusBarInfo (0x11) — truncated UokrStats ───────────────────────────

#[test]
fn status_bar_info_truncated_uokr_roundtrip() {
    // Regression: UokrStats has 23 × u16 = 46 bytes, but OSI only sends
    // 15 × u16 = 30 bytes (flag=6 packet). decode() was zero-filling the
    // remaining 8 fields → encode() wrote 46 bytes → re_len=137 vs orig=121.
    use packets::status::StatusBarInfo;

    let raw: &[u8] = &[
        0x11, 0x00, 0x79,           // id, len=121
        0x00, 0x45, 0x19, 0x58,     // serial
        // name "Broker" = 6 bytes + 24 × 0x00 (30 total)
        b'B',b'r',b'o',b'k',b'e',b'r',
        0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
        0x00, 0x50,                 // hit_points = 80
        0x00, 0x50,                 // max_hit_points = 80
        0x06,                       // name_change_flag
        0x06,                       // status_flag = 6 (UOKR)
        // is_female = 0
        0x00,
        // BaseStats (22 bytes): str dex int stam max_stam mana max_mana gold(4) ar weight
        0x00, 0x3C, 0x00, 0x49, 0x00, 0x44, 0x00, 0x49, 0x00, 0x49,
        0x00, 0x44, 0x00, 0x44, 0x00, 0x00, 0x00, 0x35, 0x00, 0x02,
        0x00, 0x5D,
        // UomlStats (3 bytes): max_weight race
        0x01, 0x36, 0x01,
        // UorStats (4 bytes): stats_cap followers max_followers
        0x00, 0xE1, 0x04, 0x05,
        // AosStats (18 bytes): fire cold poison energy luck dmg_min dmg_max tithing(4)
        0x00, 0x09, 0x00, 0x0C, 0x00, 0x08, 0x00, 0x08,
        0x00, 0x00, 0x00, 0x16, 0x00, 0x1A, 0x00, 0x00, 0x00, 0x00,
        // UokrStats — only 15 × u16 = 30 bytes (not 23 × 2 = 46)
        0x00, 0x46, 0x00, 0x46, 0x00, 0x46, 0x00, 0x46, 0x00, 0x46,
        0x00, 0x00, 0x00, 0x2D, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(raw.len(), 121);

    let pkt = StatusBarInfo::from_bytes(raw).unwrap();
    assert_eq!(pkt.status_flag, 6);
    let uokr_bytes = pkt.uokr.as_ref().expect("uokr must be Some");
    assert_eq!(uokr_bytes.len(), 30, "must capture exactly the 30 bytes sent");

    // uokr_parsed() fills missing fields with 0
    let parsed = pkt.uokr_parsed().unwrap();
    assert_eq!(parsed.hit_chance_increase, 0x0046);
    assert_eq!(parsed.max_mana_increase, 0); // beyond the 15 sent fields

    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], raw,
        "roundtrip mismatch: encode wrote more uokr bytes than server sent");
}

// ── OldHealthBarStatus (0x17) ─────────────────────────────────────────────

#[test]
fn old_health_bar_status_roundtrip() {
    use packets::status::{HealthBarColor, OldHealthBarStatus};
    use packets::traits::BasicPacket;

    let pkt = OldHealthBarStatus::new(0x0451_9958, HealthBarColor::Green, 1);
    let bytes = pkt.to_bytes();
    assert_eq!(bytes.len(), 12);
    assert_eq!(bytes[0], 0x17);
    assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 12); // len
    assert_eq!(u32::from_be_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]), 0x04519958);
    assert_eq!(u16::from_be_bytes([bytes[7], bytes[8]]), 0x0001);  // count = 1
    assert_eq!(u16::from_be_bytes([bytes[9], bytes[10]]), 1);       // green
    assert_eq!(bytes[11], 1);                                        // flag

    let decoded = OldHealthBarStatus::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, pkt);
}

#[test]
fn old_health_bar_status_wire_bytes() {
    use packets::status::{HealthBarColor, OldHealthBarStatus};
    use packets::traits::BasicPacket;

    // Exact 12-byte wire packet: yellow color, disable (flag=0)
    let raw: &[u8] = &[
        0x17,
        0x00, 0x0C,             // len = 12
        0x00, 0x45, 0x19, 0x58, // serial
        0x00, 0x01,             // count = 0x0001
        0x00, 0x02,             // color = 2 (yellow)
        0x00,                   // flag = 0 (remove)
    ];
    let pkt = OldHealthBarStatus::from_bytes(raw).unwrap();
    assert_eq!(pkt.serial, 0x00451958);
    assert_eq!(pkt.color, HealthBarColor::Yellow);
    assert_eq!(pkt.flag, 0);
    assert_eq!(pkt.to_bytes().as_ref() as &[u8], raw);
}

#[test]
fn old_health_bar_status_bad_marker_errors() {
    use packets::status::OldHealthBarStatus;
    use packets::traits::BasicPacket;

    // count field != 0x0001 → const_value mismatch → decode error
    let raw: &[u8] = &[
        0x17, 0x00, 0x0C,
        0x00, 0x45, 0x19, 0x58,
        0x00, 0x02,             // marker = 0x0002 (wrong)
        0x00, 0x01,
        0x01,
    ];
    assert!(OldHealthBarStatus::from_bytes(raw).is_err());
}

// ── BuyItems (0x3B) ───────────────────────────────────────────────────────

#[test]
fn buy_items_with_items_roundtrip() {
    use packets::interaction::{BuyItemEntry, BuyItems};

    // flag=0x02, 2 items
    let raw: &[u8] = &[
        0x3B,
        0x00, 0x16,                         // len = 22
        0x00, 0x00, 0x04, 0x01,             // vendor_id
        0x02,                               // flag = items follow
        0x1A, 0x00, 0x11, 0x22, 0x33, 0x00, 0x03, // item 1: layer + id + qty=3
        0x1A, 0x00, 0x44, 0x55, 0x66, 0x00, 0x01, // item 2: qty=1
    ];
    assert_eq!(raw.len(), 22);

    let pkt = BuyItems::from_bytes(raw).unwrap();
    assert_eq!(pkt.vendor_id, 0x00000401);
    assert_eq!(pkt.items.len(), 2);
    assert_eq!(pkt.items[0], BuyItemEntry::new(0x00112233, 3));
    assert_eq!(pkt.items[1], BuyItemEntry::new(0x00445566, 1));

    let reencoded = pkt.to_bytes();
    assert_eq!(reencoded.as_ref() as &[u8], raw);
}

#[test]
fn buy_items_cancel_roundtrip() {
    use packets::interaction::BuyItems;

    // flag=0x00 — cancel, no items
    let raw: &[u8] = &[
        0x3B,
        0x00, 0x08,             // len = 8
        0x00, 0x00, 0x04, 0x01, // vendor_id
        0x00,                   // flag = cancel
    ];
    let pkt = BuyItems::from_bytes(raw).unwrap();
    assert!(pkt.items.is_empty());
    assert_eq!(pkt.to_bytes().as_ref() as &[u8], raw);
}

// ── SellListReply (0x9F) ──────────────────────────────────────────────────

#[test]
fn sell_list_reply_roundtrip() {
    use packets::interaction::{SellItemEntry, SellListReply};
    use packets::traits::BasicPacket;

    // 2 items
    let raw: &[u8] = &[
        0x9F,
        0x00, 0x15,                         // len = 21
        0x00, 0x00, 0x05, 0x02,             // shopkeeper_id
        0x00, 0x02,                         // item_count = 2
        0x00, 0xAA, 0xBB, 0xCC, 0x00, 0x05, // item 1: id + qty=5
        0x00, 0x11, 0x22, 0x33, 0x00, 0x01, // item 2: id + qty=1
    ];
    assert_eq!(raw.len(), 21);

    let pkt = SellListReply::from_bytes(raw).unwrap();
    assert_eq!(pkt.shopkeeper_id, 0x00000502);
    assert_eq!(pkt.items.len(), 2);
    assert_eq!(pkt.items[0], SellItemEntry { item_id: 0x00AABBCC, quantity: 5 });
    assert_eq!(pkt.items[1], SellItemEntry { item_id: 0x00112233, quantity: 1 });

    assert_eq!(pkt.to_bytes().as_ref() as &[u8], raw);
}

#[test]
fn sell_list_reply_empty_roundtrip() {
    use packets::interaction::SellListReply;
    use packets::traits::BasicPacket;

    let raw: &[u8] = &[
        0x9F,
        0x00, 0x09,             // len = 9
        0x00, 0x00, 0x05, 0x02, // shopkeeper_id
        0x00, 0x00,             // item_count = 0
    ];
    let pkt = SellListReply::from_bytes(raw).unwrap();
    assert!(pkt.items.is_empty());
    assert_eq!(pkt.to_bytes().as_ref() as &[u8], raw);
}

// -- MultiPlacement (0x99) ---------------------------------------------------

#[test]
fn multi_placement_server_request_wire() {
    use packets::interaction::MultiPlacement;

    // Example from a real server: deed_serial = 7, multi_model = 0x0BB8.
    let pkt = MultiPlacement::server_request(7, 0x0BB8);
    let bytes = encode_packet(&pkt);

    let expected: &[u8] = &[
        0x99, 0x01,
        0x00, 0x00, 0x00, 0x07,                         // deed serial
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,             // 12 bytes unknown
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0B, 0xB8,                                     // multi model
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,             // 6 bytes unknown
    ];
    assert_eq!(bytes.as_ref() as &[u8], expected);

    let decoded = MultiPlacement::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.request, 0x01);
    assert_eq!(decoded.deed_serial, 7);
    assert_eq!(decoded.multi_model, 0x0BB8);
}
