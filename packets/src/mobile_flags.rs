//! Mobile status flags.
//!
//! [`MobileFlags`] is a single-byte bitfield shared by every packet that
//! describes a mobile's visual / combat state:
//!
//! | Packet          | Opcode | Field name     |
//! |-----------------|--------|----------------|
//! | [`DrawMobile`]  | 0x78   | `status`       |
//! | [`DrawMobileExtended`] | 0xD3 | `status`  |
//! | [`UpdateMobile`]| 0x77   | `status_flags` |
//! | [`DrawGamePlayer`] | 0x20 | `flags`       |
//! | [`OpenPaperdoll`]  | 0x88 | `flags`       |
//!
//! [`DrawMobile`]: crate::world::DrawMobile
//! [`DrawMobileExtended`]: crate::world::DrawMobileExtended
//! [`UpdateMobile`]: crate::character::UpdateMobile
//! [`DrawGamePlayer`]: crate::character::DrawGamePlayer
//! [`OpenPaperdoll`]: crate::character::OpenPaperdoll

use u_io::{BinaryWriter, ByteOrder, Decode, DecodeError, Encode, ReadPrimitives};

/// Bitwise mobile status flags, transmitted as a single `u8` on the wire.
///
/// # Bit layout
///
/// | Bit    | Meaning                                    |
/// |--------|--------------------------------------------|
/// | `0x01` | War mode (AOS+ clients)                    |
/// | `0x02` | Can alter paperdoll                        |
/// | `0x04` | Poisoned                                   |
/// | `0x08` | Golden / yellow health bar                 |
/// | `0x10` | Unknown / reserved                         |
/// | `0x20` | Unknown / reserved                         |
/// | `0x40` | War mode (pre-AOS clients)                 |
/// | `0x80` | Hidden                                     |
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct MobileFlags(pub u8);

impl MobileFlags {
    /// No flags set.
    pub const NONE: Self = Self(0);

    // ── Constructors ───────────────────────────────────────────────────

    /// Construct from a raw wire byte.
    #[inline]
    pub const fn from_raw(byte: u8) -> Self {
        Self(byte)
    }

    /// Return the raw wire byte.
    #[inline]
    pub const fn to_raw(self) -> u8 {
        self.0
    }

    // ── Flag accessors ─────────────────────────────────────────────────

    /// War mode active (AOS+ clients use bit `0x01`).
    #[inline]
    pub fn war_mode_aos(self) -> bool {
        self.0 & 0x01 != 0
    }

    /// Can alter paperdoll (bit `0x02`).
    #[inline]
    pub fn can_alter_paperdoll(self) -> bool {
        self.0 & 0x02 != 0
    }

    /// Mobile is poisoned (bit `0x04`).
    #[inline]
    pub fn poisoned(self) -> bool {
        self.0 & 0x04 != 0
    }

    /// Mobile has a golden / yellow health bar (bit `0x08`).
    #[inline]
    pub fn golden_health(self) -> bool {
        self.0 & 0x08 != 0
    }

    /// War mode active (pre-AOS clients use bit `0x40`).
    #[inline]
    pub fn war_mode_legacy(self) -> bool {
        self.0 & 0x40 != 0
    }

    /// Mobile is hidden (bit `0x80`).
    #[inline]
    pub fn hidden(self) -> bool {
        self.0 & 0x80 != 0
    }

    // ── Builder helpers ────────────────────────────────────────────────

    /// Return a copy with the "poisoned" flag set or cleared.
    #[inline]
    pub fn with_poisoned(self, val: bool) -> Self {
        Self(if val { self.0 | 0x04 } else { self.0 & !0x04 })
    }

    /// Return a copy with the "war mode (legacy)" flag set or cleared.
    #[inline]
    pub fn with_war_mode(self, val: bool) -> Self {
        Self(if val { self.0 | 0x40 } else { self.0 & !0x40 })
    }

    /// Return a copy with the "hidden" flag set or cleared.
    #[inline]
    pub fn with_hidden(self, val: bool) -> Self {
        Self(if val { self.0 | 0x80 } else { self.0 & !0x80 })
    }
}

impl std::fmt::Debug for MobileFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts: Vec<&'static str> = Vec::new();
        if self.war_mode_aos()         { parts.push("WAR_MODE_AOS"); }
        if self.can_alter_paperdoll()  { parts.push("CAN_ALTER_PAPERDOLL"); }
        if self.poisoned()             { parts.push("POISONED"); }
        if self.golden_health()        { parts.push("GOLDEN_HEALTH"); }
        if self.war_mode_legacy()      { parts.push("WAR_MODE"); }
        if self.hidden()               { parts.push("HIDDEN"); }
        if parts.is_empty() {
            write!(f, "MobileFlags(0x{:02X})", self.0)
        } else {
            write!(f, "MobileFlags({})", parts.join(" | "))
        }
    }
}

impl std::fmt::Display for MobileFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:02X}", self.0)
    }
}

impl From<u8> for MobileFlags {
    #[inline]
    fn from(b: u8) -> Self { Self(b) }
}

impl From<MobileFlags> for u8 {
    #[inline]
    fn from(f: MobileFlags) -> Self { f.0 }
}

impl<E: ByteOrder> Decode<E> for MobileFlags {
    fn decode<R: ReadPrimitives<E>>(reader: &mut R) -> Result<Self, DecodeError> {
        let b: u8 = Decode::<E>::decode(reader)?;
        Ok(Self(b))
    }
}

impl<E: ByteOrder> Encode<E> for MobileFlags {
    fn encode(&self, writer: &mut BinaryWriter<E>) {
        writer.put_u8(self.0);
    }
}
