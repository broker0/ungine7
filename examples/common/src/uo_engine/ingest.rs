//! S->C packet ingestion into entity maps.
//!
//! [`ingest_into_entity_map`] parses a single server-to-client packet and
//! upserts or deletes the corresponding entity in a
//! `HashMap<u32, DemoEntity>`.
//!
//! Supported packets:
//!
//! | Packet | Effect |
//! |--------|--------|
//! | `0x1A` ObjectInfo | Insert Item or Multi |
//! | `0xF3` ObjectInfoSA | Insert Item or Multi (SA+ format) |
//! | `0xF7` PacketList | Batch insert Items/Multis |
//! | `0x78` DrawMobile | Insert Mobile (preserves prev name/HP) |
//! | `0xD3` DrawMobileExtended | Insert Mobile (3D client format) |
//! | `0x77` UpdateMobile | Update existing Mobile position/direction |
//! | `0x1D` DeleteObject | Remove non-mobile entities |
//! | `0x11` StatusBarInfo | Update Mobile name/HP |
//! | `0x2D` MobAttributes | Update Mobile HP |
//! | `0xA1` UpdateHealth | Update Mobile HP |
//! | `0x1C` SendSpeech | Extract Mobile/Item name (overhead label) |
//! | `0xAE` UnicodeSpeech | Extract Mobile/Item name (unicode overhead) |

use std::collections::HashMap;

use log::debug;

use packets::character::UpdateMobile;
use packets::interaction::DeleteObject;
use packets::speech::{SendSpeech, SpeechType, UnicodeSpeech};
use packets::status::{MobAttributes, StatusBarInfo, UpdateHealth};
use packets::traits::{ManualPacket, BasicPacket};
use packets::world::{
    DrawMobile, DrawMobileExtended, ObjectDataType, ObjectInfo, ObjectInfoSA,
    PacketList,
};

use super::entity::{DemoEntity, MobileData};

/// Derive an intrinsic [`NotorietyClass`](super::notoriety::NotorietyClass)
/// from a wire [`Notoriety`](packets::movement::Notoriety) value seen in a
/// recorded log.  Used to colour mobiles loaded from `.uolog` data.
fn noto_class_from_wire(n: packets::movement::Notoriety) -> super::notoriety::NotorietyClass {
    use packets::movement::Notoriety;
    use super::notoriety::NotorietyClass;
    match n {
        Notoriety::Innocent => NotorietyClass::Innocent,
        Notoriety::Criminal => NotorietyClass::Criminal,
        Notoriety::Murderer => NotorietyClass::Murderer,
        Notoriety::Enemy => NotorietyClass::Enemy,
        // Ally / Attackable / Translucent / Invalid / Unknown → neutral.
        _ => NotorietyClass::Neutral,
    }
}


/// Parse a single S->C packet and upsert/delete the corresponding entity.
pub fn ingest_into_entity_map(
    data: &[u8],
    _world: u8,
    map: &mut HashMap<u32, DemoEntity>,
) {
    if data.is_empty() {
        return;
    }

    match data[0] {
        id if id == ObjectInfo::ID => {
            if let Ok(obj) = ObjectInfo::from_bytes(data) {
                let is_multi = obj.multi_id().is_some();
                let graphic: u16 = obj.multi_id().unwrap_or(obj.graphic);
                let entity = if is_multi {
                    DemoEntity::Multi {
                        serial: obj.object_id,
                        graphic,
                        x: obj.x,
                        y: obj.y,
                        z: obj.z,
                        owner: 0,
                        door_serials: Vec::new(),
                        sign_serial: 0,
                    }
                } else {
                    DemoEntity::Item {
                        serial: obj.object_id,
                        graphic,
                        color: obj.dye.unwrap_or(0),
                        amount: obj.amount.unwrap_or(1),
                        x: obj.x,
                        y: obj.y,
                        z: obj.z,
                        is_container: false,
                        hidden: false,
                        facing: None,
                    }
                };
                map.insert(obj.object_id, entity);
            }
        }
        id if id == ObjectInfoSA::ID => {
            if let Ok(obj) = ObjectInfoSA::from_bytes(data) {
                let is_multi = obj.data_type == ObjectDataType::Multi;
                let entity = if is_multi {
                    DemoEntity::Multi {
                        serial: obj.serial,
                        graphic: obj.graphic,
                        x: obj.x,
                        y: obj.y,
                        z: obj.z,
                        owner: 0,
                        door_serials: Vec::new(),
                        sign_serial: 0,
                    }
                } else {
                    DemoEntity::Item {
                        serial: obj.serial,
                        graphic: obj.graphic,
                        color: obj.hue,
                        amount: obj.amount,
                        x: obj.x,
                        y: obj.y,
                        z: obj.z,
                        is_container: false,
                        hidden: false,
                        facing: None,
                    }
                };
                map.insert(obj.serial, entity);
            }
        }
        id if id == PacketList::ID => {
            if let Ok(list) = PacketList::from_bytes(data) {
                for obj in &list.items {
                    let is_multi = obj.data_type == ObjectDataType::Multi;
                    let entity = if is_multi {
                        DemoEntity::Multi {
                            serial: obj.serial,
                            graphic: obj.graphic,
                            x: obj.x,
                            y: obj.y,
                            z: obj.z,
                            owner: 0,
                            door_serials: Vec::new(),
                            sign_serial: 0,
                        }
                    } else {
                        DemoEntity::Item {
                            serial: obj.serial,
                            graphic: obj.graphic,
                            color: obj.hue,
                            amount: obj.amount,
                            x: obj.x,
                            y: obj.y,
                            z: obj.z,
                            is_container: false,
                            hidden: false,
                            facing: None,
                        }
                    };
                    map.insert(obj.serial, entity);
                }
            }
        }
        id if id == DrawMobile::ID => {
            match DrawMobile::from_bytes(data) {
                Ok(mob) => {
                    let prev = map.get(&mob.serial).and_then(|e| e.mobile()).map(|m| {
                        (m.name.clone(), m.hits, m.hits_max, m.mana, m.mana_max, m.stamina, m.stamina_max, m.str_, m.dex, m.int)
                    }).unwrap_or_default();
                    map.insert(
                        mob.serial,
                        DemoEntity::Mobile(MobileData {
                            serial: mob.serial,
                            graphic: mob.graphic,
                            x: mob.x,
                            y: mob.y,
                            z: mob.z,
                            direction: mob.direction,
                            color: mob.color,
                            status: mob.status,
                            notoriety: mob.notoriety,
                            items: mob.items,
                            name: prev.0,
                            hits: prev.1,
                            hits_max: prev.2,
                            mana: prev.3,
                            mana_max: prev.4,
                            stamina: prev.5,
                            stamina_max: prev.6,
                            str_: prev.7,
                            dex: prev.8,
                            int: prev.9,
                            is_player: false,
                            dead: false,
                            living_graphic: 0,
                            noto_class: noto_class_from_wire(mob.notoriety),
                            ..Default::default()
                        }),
                    );
                }
                Err(e) => {
                    debug!(
                        "ingest 0x78 DrawMobile: parse failed ({} bytes): {e}",
                        data.len()
                    );
                }
            }
        }
        id if id == DrawMobileExtended::ID => {
            match DrawMobileExtended::from_bytes(data) {
                Ok(mob) => {
                    let prev = map.get(&mob.serial).and_then(|e| e.mobile()).map(|m| {
                        (m.name.clone(), m.hits, m.hits_max, m.mana, m.mana_max, m.stamina, m.stamina_max, m.str_, m.dex, m.int)
                    }).unwrap_or_default();
                    map.insert(
                        mob.serial,
                        DemoEntity::Mobile(MobileData {
                            serial: mob.serial,
                            graphic: mob.graphic,
                            x: mob.x,
                            y: mob.y,
                            z: mob.z,
                            direction: mob.direction,
                            color: mob.color,
                            status: mob.status,
                            notoriety: mob.notoriety,
                            items: mob.items,
                            name: prev.0,
                            hits: prev.1,
                            hits_max: prev.2,
                            mana: prev.3,
                            mana_max: prev.4,
                            stamina: prev.5,
                            stamina_max: prev.6,
                            str_: prev.7,
                            dex: prev.8,
                            int: prev.9,
                            is_player: false,
                            dead: false,
                            living_graphic: 0,
                            noto_class: noto_class_from_wire(mob.notoriety),
                            ..Default::default()
                        }),
                    );
                }
                Err(e) => {
                    debug!(
                        "ingest 0xD3 DrawMobileExtended: parse failed ({} bytes): {e}",
                        data.len()
                    );
                }
            }
        }
        id if id == UpdateMobile::ID => {
            if let Ok(upd) = UpdateMobile::from_bytes(data) {
                if let Some(DemoEntity::Mobile(m)) = map.get_mut(&upd.serial)
                {
                    m.x = upd.x;
                    m.y = upd.y;
                    m.z = upd.z;
                    m.direction = upd.direction;
                    m.color = upd.hue;
                    m.status = upd.status_flags;
                    m.notoriety = upd.notoriety;
                }
            }
        }
        id if id == DeleteObject::ID => {
            if let Ok(d) = DeleteObject::from_bytes(data) {
                let is_mobile = matches!(map.get(&d.serial), Some(DemoEntity::Mobile(_)));
                if !is_mobile {
                    map.remove(&d.serial);
                }
            }
        }
        id if id == StatusBarInfo::ID => {
            if let Ok(sbi) = StatusBarInfo::from_bytes(data) {
                if let Some(DemoEntity::Mobile(m)) = map.get_mut(&sbi.serial)
                {
                    let n = sbi.name.to_string();
                    if !n.is_empty() {
                        m.name = n;
                    }
                    m.hits = sbi.hit_points;
                    m.hits_max = sbi.max_hit_points;
                    if let Some(ref stats) = sbi.stats {
                        m.mana = stats.mana;
                        m.mana_max = stats.max_mana;
                        m.stamina = stats.stamina;
                        m.stamina_max = stats.max_stamina;
                        m.str_ = stats.strength;
                        m.dex = stats.dexterity;
                        m.int = stats.intelligence;
                    }
                }
            }
        }
        id if id == MobAttributes::ID => {
            if let Ok(ma) = MobAttributes::from_bytes(data) {
                if let Some(DemoEntity::Mobile(m)) = map.get_mut(&ma.serial) {
                    m.hits = ma.hits_current;
                    m.hits_max = ma.hits_max;
                    m.mana = ma.mana_current;
                    m.mana_max = ma.mana_max;
                    m.stamina = ma.stam_current;
                    m.stamina_max = ma.stam_max;
                }
            }
        }
        id if id == UpdateHealth::ID => {
            if let Ok(uh) = UpdateHealth::from_bytes(data) {
                if let Some(DemoEntity::Mobile(m)) = map.get_mut(&uh.serial) {
                    m.hits = uh.current_health;
                    m.hits_max = uh.max_health;
                }
            }
        }
        // SendSpeech (0x1C) carries the mobile name in the `name` field.
        //
        // Pattern for a legitimate object-name label (not system/combat text):
        //   - serial is valid (non-zero, not 0xFFFF_FFFF)
        //   - speech_type == Normal (overhead text, not system corner)
        //   - name is non-empty (the NPC / creature name) OR
        //     message is non-empty and serial refers to a known object
        //
        // We only fill the mobile name here; item names require access to
        // zone.item_props and are handled in ingest_ops::handle_ingest_packet.
        id if id == SendSpeech::ID => {
            if let Ok(speech) = SendSpeech::from_bytes(data) {
                let valid_serial = speech.serial != 0 && speech.serial != 0xFFFF_FFFF;
                let is_overhead = matches!(speech.speech_type, SpeechType::Normal);
                if valid_serial && is_overhead && !speech.name.is_empty() {
                    if let Some(DemoEntity::Mobile(m)) = map.get_mut(&speech.serial) {
                        if m.name.is_empty() {
                            m.name = speech.name.clone();
                        }
                    }
                }
            }
        }
        // UnicodeSpeech (0xAE) — UTF-16 variant of SendSpeech.
        // Same pattern: extract NPC/mobile name from overhead Normal speech.
        id if id == UnicodeSpeech::ID => {
            if let Ok(speech) = UnicodeSpeech::from_bytes(data) {
                let valid_serial = speech.serial != 0 && speech.serial != 0xFFFF_FFFF;
                let is_overhead = matches!(speech.speech_type, SpeechType::Normal);
                let name_str = speech.name.to_string();
                if valid_serial && is_overhead && !name_str.is_empty() {
                    if let Some(DemoEntity::Mobile(m)) = map.get_mut(&speech.serial) {
                        if m.name.is_empty() {
                            m.name = name_str;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}
