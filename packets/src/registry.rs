//! Packet descriptor registry.
//!
//! [`PacketRegistry`] maps `(packet_id, PacketDirection)` to a descriptor
//! function that decodes raw bytes and returns a [`DecodedOutcome`] in the
//! requested [`OutputFormat`].
//!
//! Internally the registry is two `[Option<DescriptorFn>; 256]` arrays —
//! one per direction — so lookup is a single array index, zero allocations,
//! no `HashMap`.
//!
//! # Example
//!
//! ```ignore
//! use packets::registry::{PacketRegistry, DescribeResult, OutputFormat};
//! use u_core::packet::PacketDirection::ServerToClient;
//!
//! let reg = PacketRegistry::default();
//!
//! // Debug format (always available):
//! let result = reg.describe(0x73, &data, ServerToClient, OutputFormat::Debug);
//!
//! // JSON format (requires `serde` feature):
//! // let result = reg.describe(0x73, &data, ServerToClient, OutputFormat::Json);
//! ```

use u_core::packet::PacketDirection;

use crate::traits::{ManualPacket, BasicPacket};

// ── OutputFormat ───────────────────────────────────────────────────────────

/// Requested output format for packet decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// `Debug` representation — always available.
    Debug,
    /// JSON via `serde_json` — requires the `serde` feature.
    #[cfg(feature = "serde")]
    Json,
}

// ── DecodedPacket ──────────────────────────────────────────────────────────

/// Decoded packet output in the requested format.
#[derive(Debug, Clone)]
pub enum DecodedOutcome {
    /// `Debug` string representation.
    Debug(String),
    /// JSON value — only available with the `serde` feature.
    #[cfg(feature = "serde")]
    Json(serde_json::Value),
}

impl DecodedOutcome {
    /// Extract the inner string regardless of variant.
    ///
    /// For `Debug` returns the debug string as-is.
    /// For `Json` returns the JSON serialized to a string.
    pub fn into_string(self) -> String {
        match self {
            Self::Debug(s) => s,
            #[cfg(feature = "serde")]
            Self::Json(v) => v.to_string(),
        }
    }

    /// Borrow the inner string, or serialize JSON to a new `String`.
    pub fn as_string(&self) -> String {
        match self {
            Self::Debug(s) => s.clone(),
            #[cfg(feature = "serde")]
            Self::Json(v) => v.to_string(),
        }
    }
}

impl std::fmt::Display for DecodedOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Debug(s) => f.write_str(s),
            #[cfg(feature = "serde")]
            Self::Json(v) => write!(f, "{v}"),
        }
    }
}

// ── DescriptorFn ───────────────────────────────────────────────────────────

/// A descriptor function: decodes raw packet bytes in the requested format
/// and returns a [`DecodedOutcome`], or an error string on decode failure.
pub type DecoderFn = fn(&[u8], &OutputFormat) -> Result<DecodedOutcome, String>;

// ── DescribeResult ─────────────────────────────────────────────────────────

/// Result of looking up and decoding a packet in the registry.
#[derive(Debug)]
pub enum DecodedResult {
    /// Packet decoded successfully.
    Ok(DecodedOutcome),
    /// Packet is registered but decoding failed — contains the error message.
    DecodeError(String),
    /// No descriptor registered for this `(id, direction)` pair.
    Unknown,
}

// ── PacketRegistry ─────────────────────────────────────────────────────────

/// A registry of packet descriptor functions keyed by `(packet_id, direction)`.
///
/// Backed by two fixed-size `[Option<DescriptorFn>; 256]` arrays, one per
/// direction. Lookup is `O(1)` with no heap allocations at query time.
///
/// Use [`PacketRegistry::default()`] to get a registry pre-populated with all
/// known UO packets, or [`PacketRegistry::new()`] to start empty and register
/// selectively.
pub struct PacketRegistry {
    c2s: [Option<DecoderFn>; 256],
    s2c: [Option<DecoderFn>; 256],
}

impl PacketRegistry {
    /// Create an empty registry with no descriptors registered.
    pub const fn new() -> Self {
        Self {
            c2s: [None; 256],
            s2c: [None; 256],
        }
    }

    #[inline]
    fn slot_mut(&mut self, dir: PacketDirection) -> &mut [Option<DecoderFn>; 256] {
        match dir {
            PacketDirection::ClientToServer => &mut self.c2s,
            PacketDirection::ServerToClient => &mut self.s2c,
        }
    }

    #[inline]
    fn slot(&self, dir: PacketDirection) -> &[Option<DecoderFn>; 256] {
        match dir {
            PacketDirection::ClientToServer => &self.c2s,
            PacketDirection::ServerToClient => &self.s2c,
        }
    }

    /// Register a [`BasicPacket`] + [`Debug`] type for the given direction.
    ///
    /// When the `serde` feature is enabled, `T` must also implement
    /// [`serde::Serialize`] to support JSON output.
    ///
    /// Panics if a descriptor is already registered for the same
    /// `(T::ID, dir)` pair.
    #[cfg(not(feature = "serde"))]
    pub fn register<T: BasicPacket + std::fmt::Debug + 'static>(&mut self, dir: PacketDirection) {
        self.insert::<T>(dir, |data, format| {
            let p = T::from_bytes(data).map_err(|e| format!("{e}"))?;
            Ok(format_packet(&p, format))
        });
    }

    /// Register a [`BasicPacket`] + [`Debug`] + [`serde::Serialize`] type (serde enabled).
    #[cfg(feature = "serde")]
    pub fn register<T: BasicPacket + std::fmt::Debug + serde::Serialize + 'static>(
        &mut self,
        dir: PacketDirection,
    ) {
        self.insert::<T>(dir, |data, format| {
            let p = T::from_bytes(data).map_err(|e| format!("{e}"))?;
            Ok(format_packet(&p, format))
        });
    }

    /// Register a [`ManualPacket`] + [`Debug`] type for the given direction.
    ///
    /// Panics if a descriptor is already registered for the same
    /// `(T::ID, dir)` pair.
    #[cfg(not(feature = "serde"))]
    pub fn register_manual<T: ManualPacket + std::fmt::Debug + 'static>(
        &mut self,
        dir: PacketDirection,
    ) {
        self.insert_manual::<T>(dir, |data, format| {
            let p = T::from_bytes(data).map_err(|e| format!("{e}"))?;
            Ok(format_packet(&p, format))
        });
    }

    /// Register a [`ManualPacket`] + [`Debug`] + [`serde::Serialize`] type (serde enabled).
    #[cfg(feature = "serde")]
    pub fn register_manual<T: ManualPacket + std::fmt::Debug + serde::Serialize + 'static>(
        &mut self,
        dir: PacketDirection,
    ) {
        self.insert_manual::<T>(dir, |data, format| {
            let p = T::from_bytes(data).map_err(|e| format!("{e}"))?;
            Ok(format_packet(&p, format))
        });
    }

    /// Internal helper: insert a descriptor for a [`BasicPacket`] type.
    fn insert<T: BasicPacket>(&mut self, dir: PacketDirection, f: DecoderFn) {
        let slot = &mut self.slot_mut(dir)[T::ID as usize];
        assert!(
            slot.is_none(),
            "duplicate PacketRegistry entry for (0x{:02X}, {:?})",
            T::ID,
            dir,
        );
        *slot = Some(f);
    }

    /// Internal helper: insert a descriptor for a [`ManualPacket`] type.
    fn insert_manual<T: ManualPacket>(&mut self, dir: PacketDirection, f: DecoderFn) {
        let slot = &mut self.slot_mut(dir)[T::ID as usize];
        assert!(
            slot.is_none(),
            "duplicate PacketRegistry entry for (0x{:02X}, {:?})",
            T::ID,
            dir,
        );
        *slot = Some(f);
    }

    /// Look up a descriptor and attempt to decode the packet bytes.
    ///
    /// The `format` parameter controls whether the output is a `Debug` string
    /// or a JSON [`serde_json::Value`] (when the `serde` feature is enabled).
    pub fn decode(
        &self,
        id: u8,
        data: &[u8],
        dir: PacketDirection,
        format: OutputFormat,
    ) -> DecodedResult {
        match self.slot(dir)[id as usize] {
            None => DecodedResult::Unknown,
            Some(f) => match f(data, &format) {
                Ok(decoded) => DecodedResult::Ok(decoded),
                Err(err) => DecodedResult::DecodeError(err),
            },
        }
    }
}

impl Default for PacketRegistry {
    /// Returns a registry pre-populated with all known UO packets.
    fn default() -> Self {
        build_registry()
    }
}

impl std::fmt::Debug for PacketRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.c2s.iter().filter(|s| s.is_some()).count()
            + self.s2c.iter().filter(|s| s.is_some()).count();
        f.debug_struct("PacketRegistry")
            .field("descriptors", &count)
            .finish()
    }
}

// ── Format helpers ─────────────────────────────────────────────────────────

/// Format a decoded packet according to the requested output format.
#[cfg(not(feature = "serde"))]
fn format_packet(p: &impl std::fmt::Debug, format: &OutputFormat) -> DecodedOutcome {
    match format {
        OutputFormat::Debug => DecodedOutcome::Debug(format!("{p:?}")),
    }
}

/// Format a decoded packet according to the requested output format (serde enabled).
#[cfg(feature = "serde")]
fn format_packet(
    p: &(impl std::fmt::Debug + serde::Serialize),
    format: &OutputFormat,
) -> DecodedOutcome {
    match format {
        OutputFormat::Debug => DecodedOutcome::Debug(format!("{p:?}")),
        OutputFormat::Json => {
            // serde_json::to_value should not fail for well-formed Serialize impls.
            let val = serde_json::to_value(p)
                .unwrap_or_else(|e| serde_json::Value::String(format!("<json error: {e}>")));
            DecodedOutcome::Json(val)
        }
    }
}

// ── Default registry ───────────────────────────────────────────────────────

fn build_registry() -> PacketRegistry {
    use PacketDirection::{ClientToServer as C2S, ServerToClient as S2C};

    use crate::action::*;
    use crate::buff::*;
    use crate::character::*;
    use crate::chat::*;
    use crate::gump::*;
    use crate::house::*;
    use crate::interaction::*;
    use crate::login::*;
    use crate::map::*;
    use crate::movement::*;
    use crate::profile::*;
    use crate::redirect::*;
    use crate::seed::*;
    use crate::skills::*;
    use crate::speech::*;
    use crate::status::*;
    use crate::system::*;
    use crate::tooltip::*;
    use crate::trade::*;
    use crate::world::*;

    let mut reg = PacketRegistry::new();

    // ── Login flow ─────────────────────────────────────────────────────
    reg.register::<AccountLogin>(C2S);
    reg.register::<SelectServer>(C2S);
    reg.register::<GameLogin>(C2S);
    reg.register::<LoginCharacter>(C2S);
    reg.register::<CreateCharacter>(C2S);
    reg.register::<LoginDenied>(S2C);
    reg.register::<LoginRejected>(S2C);
    reg.register::<GameServerList>(S2C);
    reg.register::<CharacterList>(S2C);

    // ── Character ──────────────────────────────────────────────────────
    reg.register::<CharacterLocaleAndBody>(S2C);
    reg.register::<DrawGamePlayer>(S2C);
    reg.register::<UpdateMobile>(S2C);
    reg.register::<OpenPaperdoll>(S2C);
    reg.register::<NewCharacterAnimation>(S2C);    // 0xE2 S→C (KR)
    reg.register::<DisplayDeathAction>(S2C);       // 0xAF S→C

    // ── Interaction ───────────────────────────────────────────────────
    reg.register::<RequestAttack>(C2S);            // 0x05 C→S
    reg.register::<GetMobileStatus>(C2S);
    reg.register::<DoubleClick>(C2S);
    reg.register::<PickUpItem>(C2S);
    reg.register::<SingleClick>(C2S);
    reg.register::<WearItem>(C2S);
    reg.register::<TargetCursor>(C2S);
    reg.register::<TargetCursor>(S2C);
    reg.register::<AttackResponse>(S2C);
    reg.register::<MultiPlacement>(C2S);              // 0x99 C→S
    reg.register::<MultiPlacement>(S2C);              // 0x99 S→C
    reg.register::<DeleteObject>(S2C);
    reg.register_manual::<DrawContainer>(S2C);
    reg.register::<EquipItem>(S2C);
    reg.register::<FightOccurring>(S2C);
    reg.register::<RejectMoveItem>(S2C);               // 0x27 S→C
    reg.register::<DraggingOfItem>(S2C);               // 0x23 S→C

    reg.register_manual::<DropItem>(C2S);
    reg.register_manual::<AddItemToContainer>(S2C);
    reg.register_manual::<ContainerContent>(S2C);
    reg.register_manual::<CorpseClothing>(S2C);
    reg.register::<DyeWindow>(C2S);                   // 0x95 C→S
    reg.register::<DyeWindow>(S2C);                   // 0x95 S→C
    reg.register_manual::<ConsoleEntryPrompt>(C2S);   // 0x9A C→S
    reg.register_manual::<ConsoleEntryPrompt>(S2C);   // 0x9A S→C
    reg.register_manual::<BuyItems>(C2S);             // 0x3B C→S
    reg.register_manual::<BuyItems>(S2C);             // 0x3B S→C (purchase confirmation)
    reg.register::<SellListReply>(C2S);               // 0x9F C→S

    // ── Status ────────────────────────────────────────────────────────
    reg.register::<MobAttributes>(S2C);
    reg.register_manual::<StatusBarInfo>(S2C);
    reg.register::<UpdateHealth>(S2C);
    reg.register::<UpdateMana>(S2C);
    reg.register::<UpdateStamina>(S2C);
    reg.register_manual::<UpdateMobileStatus>(S2C); // 0xDE S→C
    reg.register_manual::<NewHealthBarStatus>(S2C); // 0x16 S→C
    reg.register::<OldHealthBarStatus>(S2C);        // 0x17 S→C

    // ── Speech ────────────────────────────────────────────────────────
    reg.register_manual::<TalkRequest>(C2S);            // 0x03 C→S
    reg.register_manual::<SendSpeech>(S2C);
    reg.register::<UnicodeSpeech>(S2C);
    reg.register_manual::<SpeechRequest>(C2S);
    reg.register_manual::<ClilocMessage>(S2C);

    // ── Gump ──────────────────────────────────────────────────────────
    reg.register_manual::<OpenDialogBox>(S2C);        // 0x7C S→C
    reg.register_manual::<ResponseToDialogBox>(C2S);  // 0x7D C→S
    reg.register_manual::<SendGumpDialog>(S2C);
    reg.register_manual::<GumpMenuSelection>(C2S);
    reg.register_manual::<SendCompressedGump>(S2C);  // 0xDD S→C

    // ── Movement ──────────────────────────────────────────────────────
    reg.register::<MoveRequest>(C2S);
    reg.register::<MoveReject>(S2C);
    reg.register::<ResyncRequest>(C2S);   // 0x22 C→S
    reg.register::<MoveAck>(S2C);         // 0x22 S→C

    // ── World ─────────────────────────────────────────────────────────
    reg.register_manual::<ObjectInfo>(S2C);
    reg.register_manual::<DrawMobile>(S2C);
    reg.register_manual::<DrawMobileExtended>(S2C); // 0xD3 S→C (3D client)
    reg.register::<ObjectInfoSA>(S2C);             // 0xF3 S→C (client >= 7.0)
    reg.register_manual::<PacketList>(S2C);        // 0xF7 S→C (High Seas batch)
    reg.register::<RemoveWaypoint>(S2C);           // 0xE6 S→C
    reg.register_manual::<DisplayWaypoint>(S2C);   // 0xE5 S→C
    reg.register::<Particle3DEffect>(S2C);         // 0xC7 S→C (3D client)

    // ── Skills ────────────────────────────────────────────────────────
    reg.register::<SetSkillLock>(C2S);         // 0x3A C→S
    reg.register_manual::<SendSkills>(S2C);    // 0x3A S→C

    // ── Redirect ──────────────────────────────────────────────────────
    reg.register::<ServerRedirect>(S2C);

    // ── Seeds ─────────────────────────────────────────────────────────
    reg.register::<ExtendedSeed>(C2S);

    // ── System ────────────────────────────────────────────────────────
    reg.register::<PauseClient>(S2C);             // 0x33 S→C
    reg.register::<Ping>(C2S);
    reg.register::<Ping>(S2C);
    reg.register::<LoginComplete>(S2C);
    reg.register::<PersonalLightLevel>(S2C);       // 0x4E S→C
    reg.register::<OverallLightLevel>(S2C);
    reg.register::<PlaySoundEffect>(S2C);
    reg.register::<SetWeather>(S2C);
    reg.register::<WarMode>(C2S);
    reg.register::<WarMode>(S2C);
    reg.register::<SeasonalInformation>(S2C);
    reg.register::<ClientVersionResponse>(C2S);
    reg.register::<ClientVersionRequest>(S2C);
    reg.register::<CharacterAnimation>(S2C);
    reg.register::<GraphicalEffect>(S2C);
    reg.register::<PlayMidiMusic>(S2C);
    reg.register::<NewSubserver>(S2C);
    reg.register::<ClientViewRange>(C2S);          // 0xC8 C→S
    reg.register::<ClientViewRange>(S2C);          // 0xC8 S→C (echo)
    reg.register::<Time>(S2C);                     // 0x5B S→C
    reg.register::<OpenWebBrowser>(S2C);           // 0xA5 S→C

    reg.register_manual::<SellList>(S2C);
    reg.register_manual::<OpenBuyWindow>(S2C);

    // ── Action ────────────────────────────────────────────────────────
    reg.register_manual::<TextCommand>(C2S);
    reg.register_manual::<EnableFeatures>(S2C);

    // ── General Information ───────────────────────────────────────────
    reg.register_manual::<GeneralInfo>(S2C);
    reg.register_manual::<GeneralInfo>(C2S);

    // ── Tooltip (Mega Cliloc) ─────────────────────────────────────────
    reg.register_manual::<MegaClilocRequest>(C2S);   // 0xD6 C→S
    reg.register_manual::<MegaClilocResponse>(S2C);  // 0xD6 S→C
    reg.register::<TooltipRevision>(S2C);             // 0xDC S→C

    // ── Chat ──────────────────────────────────────────────────────────
    reg.register_manual::<ChatMessage>(S2C);          // 0xB2 S→C
    reg.register_manual::<ChatText>(C2S);             // 0xB3 C→S
    reg.register::<OpenChatWindow>(C2S);              // 0xB5 C→S

    // ── Buff / Debuff ─────────────────────────────────────────────────
    reg.register_manual::<BuffDebuff>(S2C);           // 0xDF S→C

    // ── Map / Cartography ─────────────────────────────────────────────
    reg.register_manual::<MapPacket>(C2S);            // 0x56 C→S
    reg.register_manual::<MapPacket>(S2C);            // 0x56 S→C
    reg.register::<MapMessage>(S2C);                  // 0x90 S→C
    reg.register::<NewMapMessage>(S2C);               // 0xF5 S→C

    // ── Character Profile ─────────────────────────────────────────────
    reg.register_manual::<CharProfile>(C2S);          // 0xB8 C→S
    reg.register_manual::<CharProfile>(S2C);          // 0xB8 S→C

    // ── Custom House ──────────────────────────────────────────────────
    reg.register_manual::<SendCustomHouse>(S2C);      // 0xD8 S→C

    // ── Secure Trading ────────────────────────────────────────────────
    reg.register_manual::<SecureTrading>(C2S);        // 0x6F C→S
    reg.register_manual::<SecureTrading>(S2C);        // 0x6F S→C

    reg
}
