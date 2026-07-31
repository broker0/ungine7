//! Bootstrap packet generation from [`ObserverPipeline`] state.
//!
//! When a client connects to a session that already has an active world
//! (e.g. a mirror client joining a proxy, or a server bootstrapping from
//! a recorded log), the UO server will not resend the initial login
//! stream.  This module reconstructs the necessary S→C packets from the
//! data accumulated in [`ObserverPipeline`] so the client can render the
//! world.
//!
//! # Packets generated
//!
//! 1. `0x1B CharacterLocaleAndBody` — player serial, body, position, map size.
//! 2. `0xB9 EnableFeatures`   — if we saw one from the server (cached raw bytes).
//! 3. `0xBF SetMap`           — current world index (always sent).
//! 4. `0xBF EnableMapDiff`    — map/statics diff info (if we saw one from the server).
//! 5. `0xC8 ClientViewRange`  — if differs from default (18).
//! 6. `0x20 DrawGamePlayer`   — current position / serial (if tracker is ready).
//! 7. World objects from the visible set — one packet per entity, serialised
//!    directly from the stored [`EntityData`]:
//!    - `ObjectInfo (0x1A)` for pre-SA items/multis.
//!    - `ObjectInfoSA (0xF3)` for SA+ items/multis.
//!    - `DrawMobile (0x78)` for mobiles (with full equipment, hue, notoriety).
//! 8. `0x55 LoginComplete`    — signals the client that world loading is done.
//!
//! Because the visible set stores the full deserialised packet structures,
//! bootstrap faithfully reproduces hue, equipment, status flags, and
//! notoriety.

use log::debug;
use u_core::RawPacket;

use crate::ecumene::StaticDataProvider;
use super::{EntityData, ObserverPipeline};

/// Generate bootstrap packets from the current [`ObserverPipeline`] state.
///
/// World objects are serialised directly from their stored [`EntityData`],
/// so the wire format matches what the server originally sent (pre-SA
/// `ObjectInfo` stays as `ObjectInfo`; SA+ `ObjectInfoSA` stays as
/// `ObjectInfoSA`).  No format conversion is needed.
pub fn generate_bootstrap(
    observer: &ObserverPipeline,
    static_data: Option<&dyn StaticDataProvider>,
    _client_version: u_core::ProtocolVersion,
) -> Vec<RawPacket> {
    let mut out: Vec<RawPacket> = Vec::new();

    // 1. CharacterLocaleAndBody (0x1B) — must be first; initialises the
    //    client's player state and map boundaries.
    if observer.pos.is_ready() {
        use packets::character::CharacterLocaleAndBody;
        use packets::traits::BasicPacket;

        let world = observer.session.current_world;

        // Resolve map dimensions: prefer loaded data, fall back to defaults.
        let (map_w, map_h) = static_data
            .and_then(|sd| sd.map_tile_dimensions(world))
            .unwrap_or((0x1800, 0x1000));

        let clb = CharacterLocaleAndBody {
            id: CharacterLocaleAndBody::ID,
            serial: observer.pos.serial,
            unknown0: 0,
            body_type: observer.pos.body_type,
            x: observer.pos.x,
            y: observer.pos.y,
            _pad1: (),
            z: observer.pos.z,
            facing: observer.pos.facing.raw(),
            unknown2: 0,
            unknown3: 0,
            _pad4: (),
            map_width_minus8: map_w,
            map_height: map_h,
            _pad5: (),
            unknown6: 0,
        };
        out.push(RawPacket::s2c(clb.to_bytes()));
    }

    // 2. EnableFeatures (0xB9) — resend cached raw bytes verbatim.
    if let Some(ref raw) = observer.session.last_enable_features {
        out.push(RawPacket::s2c(raw.clone()));
    }

    // 3. SetMap (0xBF sub 0x0008) — always sent so the client switches to
    //    the correct facet.  0x1B does not carry the world index.
    {
        use packets::system::GeneralInfo;
        use packets::traits::ManualPacket;
        let world = observer.session.current_world;
        let set_map = GeneralInfo::SetMap { world };
        out.push(RawPacket::s2c(set_map.to_bytes()));
        debug!("[bootstrap] SetMap: world {world}");
    }

    // 4. EnableMapDiff (0xBF sub 0x0018) — resend cached raw bytes so the
    //    client loads the correct diff files from its local data dir.
    if let Some(ref raw) = observer.session.last_enable_map_diff {
        out.push(RawPacket::s2c(raw.clone()));
        debug!("[bootstrap] EnableMapDiff: {} bytes", raw.len());
    }

    // 5. ClientViewRange (0xC8) — if the current view range differs from
    //    the default (18), tell the client.
    {
        use packets::system::ClientViewRange;
        use packets::traits::BasicPacket;
        let range = observer.session.view_range();
        if range != ClientViewRange::DEFAULT as u16 {
            let cvr = ClientViewRange::new(range as u8);
            out.push(RawPacket::s2c(cvr.to_bytes()));
            debug!("[bootstrap] ClientViewRange: {range}");
        }
    }

    // 6. DrawGamePlayer (0x20) — authoritative position.
    if observer.pos.is_ready() {
        use packets::traits::BasicPacket;
        let dgp = observer.pos.to_draw_game_player();
        out.push(RawPacket::s2c(dgp.to_bytes()));
    }

    // 7. Visible objects — serialise directly from stored EntityData.
    //    The format matches the original server packet: pre-SA items are
    //    sent as ObjectInfo (0x1A), SA+ items as ObjectInfoSA (0xF3),
    //    mobiles as DrawMobile (0x78) with full equipment.
    for entity in observer.session.visible.iter() {
        let pkt_bytes = match &entity.data {
            EntityData::ItemClassic { packet, .. } => {
                use packets::traits::ManualPacket;
                packet.to_bytes()
            }
            EntityData::ItemSA { packet, .. } => {
                use packets::traits::BasicPacket;
                packet.to_bytes()
            }
            EntityData::Mobile { packet } => {
                use packets::traits::ManualPacket;
                packet.to_bytes()
            }
        };
        out.push(RawPacket::s2c(pkt_bytes));
    }

    // 8. LoginComplete (0x55) — last packet, tells the client the world is
    //    loaded and it can start rendering.
    {
        use packets::system::LoginComplete;
        use packets::traits::BasicPacket;
        out.push(RawPacket::s2c(LoginComplete::new().to_bytes()));
    }

    debug!(
        "[bootstrap] generated {} packets (world={}, visible={})",
        out.len(),
        observer.session.current_world,
        observer.session.visible.len(),
    );

    out
}
