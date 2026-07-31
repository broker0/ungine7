//! Secure Trading packets.
//!
//! | Packet | Name                          | Direction |
//! |--------|-------------------------------|-----------|
//! | 0x6F   | [`SecureTrading`]             | Both      |

use u_io::{BE, BinaryWriter, Decode, Encode, packet_reader};
use u_io::DecodeError;
use macros::WireEnum;

use crate::traits::{ManualPacket, PacketError, PacketSize};

// ── TradingAction ─────────────────────────────────────────────────────────

/// Action type carried in [`SecureTrading`] (0x6F).
///
/// | Wire value | Meaning                                           |
/// |------------|---------------------------------------------------|
/// | 0x00       | Start  — open a new trade session                 |
/// | 0x01       | Cancel — close / reject the session               |
/// | 0x02       | Update — toggle the "accepted" checkbox           |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum TradingAction {
    /// Server/client initiates a trade session.
    #[wire_enum(0x00, "start")]
    Start,
    /// One side cancels the trade.
    #[wire_enum(0x01, "cancel")]
    Cancel,
    /// One side updates their acceptance state.
    #[wire_enum(0x02, "update")]
    Update,
    /// Unknown / future action type.
    #[wire_enum(unknown)]
    Unknown(u8),
}

// ── 0x6F SecureTrading (variable, both directions) ────────────────────────

/// Packet 0x6F — Secure Trading (variable, both directions)
///
/// Manages a player-to-player trade session.  All three action types share
/// the same flat wire layout; [`player_name`](SecureTrading::player_name)
/// is only populated when [`action`](SecureTrading::action) is
/// [`TradingAction::Start`] and `has_name` is `1` on the wire.
///
/// # Wire layout
///
/// ```text
/// BYTE[1]   0x6F
/// BYTE[2]   total packet length
/// BYTE[1]   action             — see [`TradingAction`]
/// BYTE[4]   player_serial
/// BYTE[4]   container1_serial
/// BYTE[4]   container2_serial
/// BYTE[1]   has_name           — 1 if player name follows
/// IF has_name == 1:
///   BYTE[?] player_name        — null-terminated ASCII
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SecureTrading {
    /// What this packet is doing in the trade session.
    pub action: TradingAction,
    /// Serial of the player involved in the trade.
    pub player_serial: u32,
    /// Serial of the first trade container (initiator's side).
    pub container1_serial: u32,
    /// Serial of the second trade container (responder's side).
    pub container2_serial: u32,
    /// Display name of the trading partner.
    /// Non-empty only when [`action`](SecureTrading::action) is
    /// [`TradingAction::Start`] and the server includes `has_name = 1`.
    pub player_name: String,
}

impl ManualPacket for SecureTrading {
    const ID: u8 = 0x6F;
    const SIZE: PacketSize = PacketSize::Dynamic;

    fn from_bytes(data: &[u8]) -> Result<Self, PacketError> {
        // Minimum: id(1)+len(2)+action(1)+player(4)+c1(4)+c2(4)+has_name(1) = 17
        let mut r = packet_reader(data, 0x6F, 17, true)?;

        let action:            TradingAction = Decode::decode(&mut r)?;
        let player_serial:     u32           = Decode::decode(&mut r)?;
        let container1_serial: u32           = Decode::decode(&mut r)?;
        let container2_serial: u32           = Decode::decode(&mut r)?;
        let has_name:          u8            = Decode::decode(&mut r)?;

        let player_name = if has_name != 0 && r.remaining_len() > 0 {
            let remaining = r.remaining_len();
            let raw = r.read_slice(remaining)
                .map_err(|_| PacketError::Decode(DecodeError::Truncated))?;
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            String::from_utf8_lossy(&raw[..end]).into_owned()
        } else {
            String::new()
        };

        Ok(Self { action, player_serial, container1_serial, container2_serial, player_name })
    }
}

impl Encode<BE> for SecureTrading {
    fn encode(&self, w: &mut BinaryWriter<BE>) {
        w.put_u8(Self::ID);
        w.put_u16(0); // length placeholder — back-patched by to_bytes()

        w.put_u8(self.action.to_wire());
        w.put_u32(self.player_serial);
        w.put_u32(self.container1_serial);
        w.put_u32(self.container2_serial);

        if self.player_name.is_empty() {
            w.put_u8(0); // has_name = false
        } else {
            w.put_u8(1); // has_name = true
            w.put_slice(self.player_name.as_bytes());
            w.put_u8(0); // null terminator
        }
    }
}
