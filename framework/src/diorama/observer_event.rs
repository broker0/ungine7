//! Typed observer events extracted from S→C packets.
//!
//! [`ObserverEvent`] is a high-level, protocol-agnostic representation of
//! interesting things happening in the game world.  Events are emitted by
//! [`ObserverPipeline::ingest_s2c`](super::ObserverPipeline::ingest_s2c) and
//! collected in an internal buffer — consumers drain them via
//! [`ObserverPipeline::drain_events`](super::ObserverPipeline::drain_events).
//!
//! The enum intentionally uses only primitive types (`u8`, `u16`, `u32`,
//! `i8`, `i16`, `String`, `Vec`, `bool`) — no dependency on the `packets`
//! crate so that downstream consumers (Lua scripts, WebSocket observers,
//! bots) can work with clean, stable data structures.

// ── ObserverEvent ─────────────────────────────────────────────────────────

/// A typed event extracted from the S→C packet stream.
///
/// Events are designed to be consumed by scripts, UI observers, and bots.
/// Only fields useful for high-level logic are exposed; raw packet bytes
/// are not included.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "lua", derive(macros::IntoLuaTable))]
#[cfg_attr(feature = "lua", lua(tag = "type", rename_all = "snake_case"))]
pub enum ObserverEvent {
    // ── Entity lifecycle ──────────────────────────────────────────────

    /// A mobile appeared in the visible set (0x78 DrawMobile).
    MobileAppeared {
        serial: u32,
        graphic: u16,
        color: u16,
        x: u16,
        y: u16,
        z: i8,
        direction: u8,
        notoriety: u8,
    },

    /// A mobile changed position/direction/appearance (0x77 UpdateMobile).
    MobileMoved {
        serial: u32,
        graphic: u16,
        color: u16,
        x: u16,
        y: u16,
        z: i8,
        direction: u8,
        notoriety: u8,
    },

    /// A mobile was removed from the visible set (0x1D DeleteObject,
    /// serial >= 0x00000001 and < 0x40000000).
    MobileRemoved { serial: u32 },

    /// An item appeared in the visible set (0x1A ObjectInfo / 0xF3 ObjectInfoSA).
    ItemAppeared {
        serial: u32,
        graphic: u16,
        color: u16,
        x: u16,
        y: u16,
        z: i8,
        count: u16,
    },

    /// An item was removed from the visible set (0x1D DeleteObject,
    /// serial >= 0x40000000).
    ItemRemoved { serial: u32 },

    // ── Own position ──────────────────────────────────────────────────

    /// The player character's authoritative position changed (0x20 DrawGamePlayer).
    PositionChanged {
        x: u16,
        y: u16,
        z: i8,
        direction: u8,
    },

    // ── Audio / Visual ────────────────────────────────────────────────

    /// A sound was played (0x54 PlaySoundEffect).
    SoundPlayed {
        sound_id: u16,
        x: u16,
        y: u16,
        z: i16,
    },

    /// A graphical effect was played (0x70 GraphicalEffect).
    EffectPlayed {
        direction_type: u8,
        source_serial: u32,
        target_serial: u32,
        graphic: u16,
        x: u16,
        y: u16,
        z: i8,
        target_x: u16,
        target_y: u16,
        target_z: i8,
        speed: u8,
        duration: u8,
        fixed_direction: bool,
        explode: bool,
    },

    /// A character animation was played (0x6E CharacterAnimation).
    AnimationPlayed {
        serial: u32,
        action: u16,
        frame_count: u8,
        repeat_count: u16,
        reverse: bool,
        repeat: bool,
        frame_delay: u8,
    },

    /// Someone spoke (0x1C SendSpeech / 0xAE UnicodeSpeech).
    Speech {
        serial: u32,
        graphic: u16,
        speech_type: u8,
        color: u16,
        font: u16,
        name: String,
        message: String,
    },

    /// A cliloc (localised) message was displayed (0xC1 ClilocMessage).
    ClilocMessage {
        serial: u32,
        cliloc_id: u32,
        speech_type: u8,
        color: u16,
        font: u16,
        name: String,
        args: String,
    },

    // ── Combat / Stats ────────────────────────────────────────────────

    /// Damage number displayed above a mobile (0xBF sub 0x0022).
    DamageDealt { serial: u32, amount: u8 },

    /// Health bar updated (0xA1 UpdateHealth).
    HpUpdated {
        serial: u32,
        hits: u16,
        max_hits: u16,
    },

    /// Mana bar updated (0xA2 UpdateMana).
    ManaUpdated {
        serial: u32,
        mana: u16,
        max_mana: u16,
    },

    /// Stamina bar updated (0xA3 UpdateStamina).
    StaminaUpdated {
        serial: u32,
        stamina: u16,
        max_stamina: u16,
    },

    // ── UI Interaction ────────────────────────────────────────────────

    /// A gump dialog was opened (0xB0 / 0xDD).
    GumpOpened {
        gump_id: u32,
        serial: u32,
        x: u32,
        y: u32,
    },

    /// A gump was closed by the server (0xBF sub 0x0004).
    GumpClosed { gump_id: u32 },

    /// Server requested a target cursor (0x6C S→C request).
    TargetRequest {
        cursor_id: u32,
        target_type: u8,
    },

    /// Server cancelled a target cursor (0x6C S→C cancel).
    TargetCancel { cursor_id: u32 },

    /// An old-style menu was opened (0x7C).
    MenuOpened {
        serial: u32,
        menu_id: u16,
        question: String,
    },

    /// A popup/context menu was displayed (0xBF sub 0x0014).
    PopupMenu {
        serial: u32,
        entries: Vec<PopupMenuEntry>,
    },

    /// A container was opened (0x24 DrawContainer).
    ContainerOpened { serial: u32, gump_id: u16 },

    // ── Environment ───────────────────────────────────────────────────

    /// Global light level changed (0x4F OverallLightLevel).
    GlobalLight { level: u8 },

    /// Weather changed (0x65 SetWeather).
    Weather {
        weather_type: u8,
        num_effects: u8,
        temperature: u8,
    },

    /// Season changed (0xBC SeasonalInformation).
    Season { season: u8, play_sound: bool },

    /// Background music changed (0x6D PlayMidiMusic).
    Music { music_id: u16 },
}

// ── PopupMenuEntry ────────────────────────────────────────────────────────

/// An entry in a popup/context menu.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "lua", derive(macros::IntoLuaTable))]
pub struct PopupMenuEntry {
    pub index: u16,
    pub cliloc_id: u32,
    pub flags: u16,
    #[cfg_attr(feature = "lua", lua(skip_none))]
    pub color: Option<u16>,
}
