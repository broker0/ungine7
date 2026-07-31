//! Reusable spawn packet builders and helpers for server examples.
//!
//! ## Individual packet builders
//!
//! Low-level functions that build a single UO packet as a `RawPacket`:
//!
//! - [`build_character_locale_and_body`] — 0x1B CharacterLocaleAndBody
//! - [`build_draw_game_player`] — 0x20 DrawGamePlayer
//! - [`build_post_login_defaults`] — OverallLightLevel + WarMode + SetWeather
//! - [`build_welcome_message`] — SendSpeech system message
//! - [`build_status_bar`] — StatusBarInfo from a [`DemoEntity::Mobile`]
//!
//! ## Entity creation
//!
//! - [`new_player_entity`] — creates a `DemoEntity::Mobile` with a horse mount
//! - [`parse_test_account`] — parses `test\d+` account names into serial + name
//!
//! ## Legacy
//!
//! - [`build_world_spawn`] — self-contained static spawn for simple examples

use packets::character::{CharacterLocaleAndBody, DrawGamePlayer};
use packets::interaction::{
    AddItemToContainerLegacy, AddItemToContainerModern, AttackResponse,
    ContainerContent, ContainerItemLegacy, ContainerItemModern, DrawContainer,
};
use packets::layer::Layer;
use packets::mobile_flags::MobileFlags;
use packets::movement::Notoriety;
use packets::speech::{SendSpeech, SpeechType};
use packets::status::StatusBarInfo;
use packets::system::{
    ClientVersionRequest, EnableFeaturesLegacy, LoginComplete, OverallLightLevel, SetWeather,
    WarMode,
};
use packets::traits::{encode_packet, ManualPacket, BasicPacket};
use packets::world::{DrawMobile, EquippedItem};
use protocol::RawPacket;
use u_core::ProtocolVersion as ProtVer;

use crate::uo_engine::entity::{DemoEntity, MobileData};

// ── Individual packet builders ────────────────────────────────────────────

/// Build a CharacterLocaleAndBody (0x1B) packet.
///
/// This is the first packet sent when a character enters the world,
/// establishing the player's serial, body graphic, position, and map
/// dimensions.
pub fn build_character_locale_and_body(
    serial: u32,
    graphic: u16,
    x: u16,
    y: u16,
    z: i8,
    direction: u8,
    map_width: u16,
    map_height: u16,
) -> RawPacket {
    RawPacket::s2c(encode_packet(&CharacterLocaleAndBody {
        id: 0x1B,
        serial,
        unknown0: 0,
        body_type: graphic,
        x,
        y,
        _pad1: (),
        z,
        facing: direction,
        unknown2: 0,
        unknown3: 0,
        _pad4: (),
        map_width_minus8: map_width,
        map_height,
        _pad5: (),
        unknown6: 0,
    }))
}

/// Build a DrawGamePlayer (0x20) packet.
///
/// Sent after CharacterLocaleAndBody to set the authoritative player
/// position, graphic, and hue.
pub fn build_draw_game_player(
    serial: u32,
    graphic: u16,
    hue: u16,
    x: u16,
    y: u16,
    z: i8,
    direction: u8,
) -> RawPacket {
    RawPacket::s2c(encode_packet(&DrawGamePlayer {
        id: 0x20,
        serial,
        body_type: graphic,
        _pad0: (),
        hue,
        flags: MobileFlags(0),
        x,
        y,
        _pad1: (),
        direction,
        z,
    }))
}

/// Build post-login default packets: OverallLightLevel + WarMode + SetWeather.
///
/// These are sent after entity streaming and LoginComplete to initialize
/// the game environment.
pub fn build_post_login_defaults(light_level: u8, weather_type: u8) -> Vec<RawPacket> {
    vec![
        RawPacket::s2c(encode_packet(&OverallLightLevel {
            id: 0x4F,
            level: light_level,
        })),
        RawPacket::s2c(encode_packet(&WarMode::new(false))),
        RawPacket::s2c(encode_packet(&SetWeather {
            id: 0x65,
            weather_type,
            num_effects: 0x00,
            temperature: 0x00,
        })),
    ]
}

// ── Version-aware container packet builders ───────────────────────────────

/// Build a DrawContainer (0x24) packet sized for the connecting client.
///
/// - Clients < 7.0.9.0 (`CV_7090`): 7-byte legacy format.
/// - Clients >= 7.0.9.0: 9-byte modern format with `draw_grid = 0x0000`.
///
/// Pass `serial` and `gump_model` as usual (e.g. `0x003C` for a backpack).
pub fn build_draw_container(
    serial: u32,
    gump_model: u16,
    version: ProtVer,
) -> RawPacket {
    RawPacket::s2c(DrawContainer::new(serial, gump_model, version).to_bytes())
}

/// Build an AddItemToContainer (0x25) packet sized for the connecting client.
///
/// - Clients < 6.0.1.8 (`CV_6017`): 20-byte legacy format (no slot index).
/// - Clients >= 6.0.1.8: 21-byte modern format with `slot_index`.
pub fn build_add_item_to_container(
    serial: u32,
    graphic: u16,
    amount: u16,
    x: u16,
    y: u16,
    container_serial: u32,
    color: u16,
    slot_index: u8,
    version: ProtVer,
) -> RawPacket {
    if version >= ProtVer::CV_6017 {
        RawPacket::s2c(encode_packet(&AddItemToContainerModern {
            id: AddItemToContainerModern::ID,
            serial,
            graphic,
            graphic_offset: 0,
            amount,
            x,
            y,
            slot_index,
            container_serial,
            color,
        }))
    } else {
        RawPacket::s2c(encode_packet(&AddItemToContainerLegacy {
            id: AddItemToContainerLegacy::ID,
            serial,
            graphic,
            graphic_offset: 0,
            amount,
            x,
            y,
            container_serial,
            color,
        }))
    }
}

/// Build a ContainerContent (0x3C) packet from a slice of container items,
/// choosing legacy (19 bytes/item) or modern (20 bytes/item, with grid_index)
/// format based on the connecting client version.
///
/// - Clients < 6.0.1.8 (`CV_6017`): legacy format (no grid index).
/// - Clients >= 6.0.1.8: modern format; `grid_index` is set to the item's
///   positional index in the slice (0, 1, 2, …).
pub fn build_container_content(
    items: &[(u32, u16, u16, u16, u16, u32, u16)], // (serial, graphic, amount, x, y, container, color)
    version: ProtVer,
) -> RawPacket {
    if version >= ProtVer::CV_6017 {
        let modern: Vec<ContainerItemModern> = items
            .iter()
            .enumerate()
            .map(|(idx, &(serial, graphic, amount, x, y, container_serial, color))| {
                ContainerItemModern {
                    serial,
                    graphic,
                    _pad0: (),
                    amount,
                    x,
                    y,
                    grid_index: idx as u8,
                    container_serial,
                    color,
                }
            })
            .collect();
        RawPacket::s2c(ContainerContent::Modern(modern).to_bytes())
    } else {
        let legacy: Vec<ContainerItemLegacy> = items
            .iter()
            .map(|&(serial, graphic, amount, x, y, container_serial, color)| {
                ContainerItemLegacy {
                    serial,
                    graphic,
                    _pad0: (),
                    amount,
                    x,
                    y,
                    container_serial,
                    color,
                }
            })
            .collect();
        RawPacket::s2c(ContainerContent::Legacy(legacy).to_bytes())
    }
}

/// Build a system welcome message as a SendSpeech (0x1C) packet.
pub fn build_welcome_message(
    serial: u32,
    graphic: u16,
    sender_name: &str,
    message: &str,
) -> RawPacket {
    RawPacket::s2c(
        SendSpeech {
            serial,
            model: graphic,
            speech_type: SpeechType::System,
            color: 90,
            font: 3,
            name: sender_name.to_string(),
            message: message.to_string(),
        }
        .to_bytes(),
    )
}

/// Build a StatusBarInfo packet from a `DemoEntity::Mobile`.
///
/// Returns `None` if the entity is not a Mobile.
/// When `is_self` is true, includes full stats (T2A format).
pub fn build_status_bar(entity: &DemoEntity, is_self: bool) -> Option<RawPacket> {
    if let DemoEntity::Mobile(m) = entity {
        let label = if m.name.is_empty() {
            format!("[mob 0x{:04X}]", m.graphic)
        } else {
            m.name.clone()
        };
        let sbi = StatusBarInfo {
            serial: m.serial,
            name: packets::u_io::FixedString::new(&label),
            hit_points: m.hits,
            max_hit_points: m.hits_max,
            name_change_flag: 0,
            status_flag: if is_self { 1 } else { 0 },
            is_female: if is_self { Some(m.graphic == 0x0191) } else { None },
            stats: if is_self {
                Some(packets::status::BaseStats {
                    strength: m.str_,
                    dexterity: m.dex,
                    intelligence: m.int,
                    stamina: m.stamina,
                    max_stamina: m.stamina_max,
                    mana: m.mana,
                    max_mana: m.mana_max,
                    gold: 0,
                    armor_rating: 0,
                    weight: 0,
                })
            } else {
                None
            },
            uoml: None,
            uor: None,
            aos: None,
            uokr: None,
        };
        Some(RawPacket::s2c(sbi.to_bytes()))
    } else {
        None
    }
}

// ── Entity creation ───────────────────────────────────────────────────────

/// Create a new player `DemoEntity::Mobile` with a horse mount.
///
/// Both `mount_serial` and `backpack_serial` must be allocated by the
/// caller via [`SerialAllocator::alloc_item`](crate::uo_engine::serial_alloc::SerialAllocator::alloc_item) to avoid serial collisions.
///
/// `skills` is the starting skill set (id → value/cap/lock).  The caller
/// supplies it so the demo-server's skill table stays out of this crate.
pub fn new_player_entity(
    serial: u32,
    x: u16,
    y: u16,
    z: i8,
    direction: u8,
    name: &str,
    graphic: u16,
    hue: u16,
    hits: u16,
    mana: u16,
    stamina: u16,
    str_: u16,
    dex: u16,
    int: u16,
    backpack_serial: u32,
    mount_serial: u32,
    skills: std::collections::BTreeMap<u16, crate::uo_engine::entity::SkillValue>,
) -> DemoEntity {
    let mount_item = EquippedItem {
        serial: mount_serial,
        graphic: 0x3E9F, // horse mount graphic
        layer: Layer::Mount,
        color: Some(hue),
    };

    let backpack_item = EquippedItem {
        serial: backpack_serial,
        graphic: 0x0E75, // standard backpack graphic
        layer: Layer::Backpack,
        color: None,
    };

    DemoEntity::Mobile(MobileData {
        serial,
        graphic,
        x,
        y,
        z,
        direction,
        color: hue,
        status: MobileFlags(0),
        notoriety: Notoriety::Innocent,
        items: vec![mount_item, backpack_item],
        name: name.to_string(),
        hits,
        hits_max: hits,
        mana,
        mana_max: mana,
        stamina,
        stamina_max: stamina,
        str_,
        dex,
        int,
        is_player: true,
        dead: false,
        living_graphic: 0,
        noto_class: crate::uo_engine::notoriety::NotorietyClass::Innocent,
        skills,
        ..Default::default()
    })
}

// ── Test account parsing ──────────────────────────────────────────────────

/// Parsed info for test accounts matching the pattern `test\d+`.
///
/// The serial is allocated dynamically at first spawn and is not stored
/// here — it lives in `WorldData::test_serials`.
#[derive(Debug, Clone)]
pub struct TestAccountInfo {
    pub name: String,
}

/// Try to parse an account name as a test account (`test\d+`).
///
/// Returns `Some(TestAccountInfo)` when the account name matches the
/// pattern `test` followed by one or more decimal digits.
pub fn parse_test_account(account: &str) -> Option<TestAccountInfo> {
    let suffix = account.strip_prefix("test")?;
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(TestAccountInfo {
        name: account.to_string(),
    })
}

// ── Playable (read-only) account ──────────────────────────────────────────

/// Reserved account name that sees the shared, read-only pool of playable
/// characters loaded from the `.uolog` (`WorldData::playable_serials`,
/// including `EXTRA_PLAYABLE_SERIALS`).
///
/// This account always lists the whole pool, bypasses per-account character
/// storage, and is forbidden from creating new characters.  It exists so the
/// demo's replay characters stay a shared pool rather than being "claimed"
/// into one account's character list.
pub const PLAYABLE_ACCOUNT: &str = "replay";

/// Whether `account` is the reserved playable-pool account.
pub fn is_playable_account(account: &str) -> bool {
    account.eq_ignore_ascii_case(PLAYABLE_ACCOUNT)
}

// ── Created-character records ──────────────────────────────────────────────

/// A character created by a normal account through the client's
/// character-creation screen (packet 0x00).
///
/// Stored per-account so the same character is offered on the
/// character-selection screen on every reconnect.  The live entity lives
/// in the engine zone while the player is online; this record only holds
/// the data needed to re-spawn / re-list it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CharacterRecord {
    /// Allocated mobile serial (stable across reconnects).
    pub serial: u32,
    /// Character name.
    pub name: String,
    /// Body graphic (male/female human).
    pub body: u16,
    /// Skin hue.
    pub hue: u16,
    /// Map id (world / facet) the character is currently in.
    ///
    /// Tracks cross-world transfers so the character is re-listed and
    /// re-spawned in the correct world after logout.  Defaults to `0`
    /// when loading records saved before this field existed.
    #[serde(default)]
    pub world: u8,
}

// ── Legacy: self-contained test spawn ─────────────────────────────────────

/// Build a self-contained spawn sequence for simple server examples.
///
/// Spawns a male human at (1438, 1696, 0) with a horse mount,
/// sets lighting/weather, sends a welcome message, and marks login complete.
///
/// This function is used by `simple-server` and `server` examples.
/// For real game servers, use the individual builders above.
pub fn build_world_spawn() -> Vec<RawPacket> {
    let serial: u32 = 0x0123_4567;
    let graphic: u16 = 0x0190; // male human body
    let x: u16 = 1438;
    let y: u16 = 1696;
    let z: i8 = 0;

    let mut packets = Vec::new();

    // CharLocaleAndBody (0x1B)
    packets.push(build_character_locale_and_body(
        serial, graphic, x, y, z, 0, 0x1800, 0x1000,
    ));

    // DrawGamePlayer (0x20)
    packets.push(build_draw_game_player(serial, graphic, 0, x, y, z, 0));

    // OverallLightLevel (0x4F)
    packets.push(RawPacket::s2c(encode_packet(&OverallLightLevel {
        id: 0x4F,
        level: 0x00,
    })));

    // WarMode (0x72) — peace mode
    packets.push(RawPacket::s2c(encode_packet(&WarMode::new(false))));

    // LoginComplete (0x55)
    packets.push(RawPacket::s2c(encode_packet(&LoginComplete::new())));

    // SetWeather (0x65) — snow, 30 effects
    packets.push(RawPacket::s2c(encode_packet(&SetWeather {
        id: 0x65,
        weather_type: 0x02,
        num_effects: 0x1E,
        temperature: 0x00,
    })));

    // AllowAttack (0xAA) — refused
    packets.push(RawPacket::s2c(encode_packet(&AttackResponse::refused())));

    // DrawMobile (0x78) — player with horse mount
    {
        let mount_serial: u32 = 0x0123_4568;
        let mount_graphic: u16 = 16031; // 0x3E9F
        let mount_color: u16 = 0x0480;

        let draw_mob = DrawMobile {
            serial,
            graphic,
            x,
            y,
            z,
            direction: 0,
            color: 0,
            status: MobileFlags(0),
            notoriety: Notoriety::Innocent,
            items: vec![EquippedItem {
                serial: mount_serial,
                graphic: mount_graphic,
                layer: Layer::Mount,
                color: Some(mount_color),
            }],
        };
        packets.push(RawPacket::s2c(draw_mob.to_bytes()));
    }

    // Second DrawGamePlayer (refresh)
    packets.push(build_draw_game_player(serial, graphic, 0, x, y, z, 0));

    // EnableLockedClientFeatures (0xB9) — legacy, flags=0x0002
    packets.push(RawPacket::s2c(encode_packet(&EnableFeaturesLegacy::new(0x0002))));

    // ClientVersion request (0xBD)
    packets.push(RawPacket::s2c(encode_packet(&ClientVersionRequest::new())));

    // SendSpeech (0x1C)
    packets.push(build_welcome_message(serial, graphic, "Player", "Player"));

    packets
}
