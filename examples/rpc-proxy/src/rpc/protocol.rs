use serde::{Deserialize, Serialize};

use framework::diorama::{EntityData, WorldEntity};

// ── Requests (C→S over WebSocket) ─────────────────────────────────────────

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsRequest {
    ListSessions,
    Attach       { session_id: u64 },
    GetState,
    InjectPacket { hex: String },
    Subscribe    { filter: Option<Vec<u8>> },
    Unsubscribe,
    GetItems,
    GetMobiles,
    GetMobile    { serial: u32 },
    GetEquipment { serial: u32 },
    UseObject    { serial: u32 },
    /// `heading`: 0=N 1=NE 2=E 3=SE 4=S 5=SW 6=W 7=NW.
    /// `raw`: skip passability validation.
    Step         { heading: u8, raw: Option<bool> },
    /// Run Lua source code on the attached session.
    #[cfg(feature = "lua")]
    RunScript    { code: String },
    /// Run a Lua script file on the attached session.
    #[cfg(feature = "lua")]
    RunScriptFile { path: String },
    /// Stop the currently running Lua script.
    #[cfg(feature = "lua")]
    StopScript,
    Ping,
}

// ── Responses (S→C over WebSocket) ────────────────────────────────────────

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Sessions  { sessions: Vec<SessionInfo> },
    Attached  { session_id: u64, character: String },
    State     { state: crate::types::FullSessionState },
    /// Streamed packet event (from Subscribe).
    Packet    { event: PacketEvent },
    /// Response to GetItems.
    Items     { items: Vec<WorldItemData> },
    /// Response to GetMobiles.
    Mobiles   { mobiles: Vec<WorldMobileData> },
    /// Response to GetMobile.
    Mobile    { mobile: Option<WorldMobileData> },
    /// Response to GetEquipment.
    Equipment { serial: u32, equipment: Vec<EquippedItemEntry> },
    /// Confirmation that UseObject was sent.
    Used      { serial: u32 },
    /// Result of a Step command.
    /// `blocked` is true when passability check failed and no step was queued.
    Stepped   { heading: u8, blocked: bool },
    /// Lua script started successfully.
    #[cfg(feature = "lua")]
    ScriptStarted,
    /// Lua script stopped.
    #[cfg(feature = "lua")]
    ScriptStopped,
    /// Lua script error.
    #[cfg(feature = "lua")]
    ScriptError { message: String },
    Error     { message: String },
    Pong,
}

// ── Common data types ──────────────────────────────────────────────────────

#[derive(Serialize, Debug, Clone)]
pub struct SessionInfo {
    pub id:        u64,
    pub character: Option<String>,
    pub players:   u32,
}

#[derive(Serialize, Debug, Clone)]
pub struct PacketEvent {
    /// `"s2c"` or `"c2s"`.
    pub direction: String,
    /// Hex packet ID, e.g. `"0x78"`.
    pub id: String,
    /// Full packet payload as a lowercase hex string.
    pub hex: String,
    /// Decoded packet fields as JSON — `null` for unknown/unparseable packets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<serde_json::Value>,
}

// ── WorldItemData ──────────────────────────────────────────────────────────

/// Snapshot of a single item (non-mobile) from the visible set.
#[derive(Serialize, Debug, Clone)]
#[cfg_attr(feature = "lua", derive(macros::IntoLuaTable))]
pub struct WorldItemData {
    pub serial:  u32,
    pub graphic: u16,
    pub color:   u16,
    pub x:       u16,
    pub y:       u16,
    pub z:       i8,
    #[cfg_attr(feature = "lua", lua(skip))]
    pub world:   u8,
    pub count:   u16,
    pub flags:   u8,
}

impl WorldItemData {
    pub fn from_entity(entity: &WorldEntity, world: u8) -> Option<Self> {
        match &entity.data {
            EntityData::ItemClassic { packet: p, .. } => Some(Self {
                serial: entity.serial, graphic: p.graphic, color: p.dye.unwrap_or(0),
                x: p.x, y: p.y, z: p.z, world, count: p.amount.unwrap_or(1),
                flags: p.flags.map_or(0, |f| f.0),
            }),
            EntityData::ItemSA { packet: p, .. } => Some(Self {
                serial: entity.serial, graphic: p.graphic, color: p.hue,
                x: p.x, y: p.y, z: p.z, world, count: p.amount, flags: p.flags,
            }),
            EntityData::Mobile { .. } => None,
        }
    }
}

// ── Mobile data ────────────────────────────────────────────────────────────

/// One item worn by a mobile.
#[derive(Serialize, Debug, Clone)]
#[cfg_attr(feature = "lua", derive(macros::IntoLuaTable))]
pub struct EquippedItemEntry {
    pub serial:  u32,
    pub graphic: u16,
    pub color:   u16,
    pub layer:   u8,
    /// Serial of the mobile that is wearing this item.
    pub parent:  u32,
}

/// Snapshot of a mobile entity visible in the world.
#[derive(Serialize, Debug, Clone)]
#[cfg_attr(feature = "lua", derive(macros::IntoLuaTable))]
pub struct WorldMobileData {
    pub serial:    u32,
    pub graphic:   u16,
    pub color:     u16,
    pub x:         u16,
    pub y:         u16,
    pub z:         i8,
    pub direction: u8,
    #[cfg_attr(feature = "lua", lua(skip))]
    pub world:     u8,
    pub flags:     u8,
    pub notoriety: u8,
    pub equipment: Vec<EquippedItemEntry>,
}

impl WorldMobileData {
    pub fn from_entity(entity: &WorldEntity, world: u8) -> Option<Self> {
        let EntityData::Mobile { packet: p } = &entity.data else { return None; };
        Some(Self {
            serial:    entity.serial,
            graphic:   p.graphic,
            color:     p.color,
            x:         p.x,
            y:         p.y,
            z:         p.z,
            direction: p.direction,
            world,
            flags:     p.status.0,
            notoriety: p.notoriety.to_wire(),
            equipment: p.items.iter().map(|eq| EquippedItemEntry {
                serial:  eq.serial,
                graphic: eq.graphic,
                color:   eq.color.unwrap_or(0),
                layer:   eq.layer.to_wire(),
                parent:  entity.serial,
            }).collect(),
        })
    }
}
