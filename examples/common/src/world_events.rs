//! World event → UO packet translation.
//!
//! Shared implementation used by `demo-server`, `path-server`, and
//! potentially other server examples.  The core
//! [`collect_world_event_packets`] function translates [`WorldEvent`]s
//! into raw UO packets, appending them to an output buffer.
//!
//! ## Observer pipeline hook
//!
//! Some callers (e.g. `demo-server`) feed every outbound S→C packet
//! into an [`ObserverPipeline`](framework::diorama::ObserverPipeline) for
//! cross-validation.  To support this without hard-coding the dependency,
//! packet-producing functions accept an `on_s2c: &mut dyn FnMut(&[u8])`
//! callback that is invoked for each packet that should be ingested.
//! Callers that don't need this pass a no-op closure: `&mut |_: &[u8]| {}`.

use std::time::{Duration, Instant};

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};

use network::error;
use network::session::Session;

use packets::character::{CharacterAnimation, DisplayDeathAction, DrawGamePlayer, UpdateMobile};
use packets::interaction::{CorpseClothing, CorpseClothingEntry};
use packets::mobile_flags::MobileFlags;
use packets::movement::Notoriety;
use packets::speech::{SendSpeech, SpeechType, UnicodeSpeech};
use packets::system::{
    OverallLightLevel, PlayMidiMusic, PlaySoundEffect, Season, SeasonalInformation, SetWeather,
};

use framework::continuum::WorldEvent;
use framework::ecumene::TileRect;

use u_core::ProtocolVersion;

use crate::uo_engine::auth::AccessLevel;

// ── Per-viewer notoriety ──────────────────────────────────────────────────

/// Byte offset of the notoriety field inside a raw DrawMobile (0x78) packet.
///
/// Layout: id(1) + len(2) + serial(4) + graphic(2) + x(2) + y(2) + z(1)
/// + direction(1) + color(2) + status(1) = 18, then the notoriety byte.
const DRAW_MOBILE_NOTORIETY_OFFSET: usize = 18;

/// Resolve the wire notoriety byte for `target` as seen by `viewer`.
///
/// Returns `None` when there is no reputation context (non-mobile, or the
/// viewer's own context is unknown) so callers fall back to the snapshot's
/// stored notoriety byte.
fn resolve_viewer_notoriety(
    viewer: &dyn PlayerView,
    target_serial: u32,
    target: &framework::continuum::EntitySnapshot,
) -> Option<u8> {
    use crate::uo_engine::notoriety::{resolve_notoriety, NotorietyClass, NotorietyView};

    let tctx = target.notoriety_ctx.as_ref()?;

    let target_view = NotorietyView {
        class: NotorietyClass::from_u8(tctx.class),
        guild_id: tctx.guild_id,
        is_player: tctx.is_player,
    };

    // Viewer's own context; default to an innocent player when unknown so
    // guild matching simply yields "no guild relation".
    let viewer_view = match viewer.notoriety_ctx() {
        Some(v) => NotorietyView {
            class: NotorietyClass::from_u8(v.class),
            guild_id: v.guild_id,
            is_player: v.is_player,
        },
        None => NotorietyView::default(),
    };

    let is_self = target_serial == viewer.serial();

    // The target lists the viewer as an aggressor ⇒ viewer may freely attack.
    let now = crate::uo_engine::entity::MobileData::now_epoch_ms();
    let aggressor_to_viewer = tctx
        .aggressors
        .iter()
        .any(|(s, until)| *s == viewer.serial() && *until > now);

    let wire = resolve_notoriety(&viewer_view, &target_view, is_self, aggressor_to_viewer);
    Some(notoriety_to_wire_u8(wire))
}

/// Map a [`Notoriety`] enum value to its wire byte.
fn notoriety_to_wire_u8(n: Notoriety) -> u8 {
    match n {
        Notoriety::Invalid => 0,
        Notoriety::Innocent => 1,
        Notoriety::Ally => 2,
        Notoriety::Attackable => 3,
        Notoriety::Criminal => 4,
        Notoriety::Enemy => 5,
        Notoriety::Murderer => 6,
        Notoriety::Translucent => 7,
        Notoriety::Unknown(v) => v,
    }
}

/// Prepare the raw bytes for a mobile snapshot to send to `viewer`.
///
/// Two transformations are applied when necessary:
/// 1. **Modern re-encode**: if the viewer's client is >= 7.0.33.1 and the
///    snapshot is a `0x78 DrawMobile` or `0xD3 DrawMobileExtended`, the
///    equipment list is re-encoded in the modern fixed 9-byte-per-item format.
///    This is needed because snapshots are cached in the legacy format (no
///    receiver-version knowledge at ingest time).
/// 2. **Notoriety patch**: the notoriety byte is overwritten with the value
///    seen from this particular viewer's perspective.
///
/// When neither transformation is needed the original `Bytes` arc is returned
/// cheaply without allocation.
fn prepare_mobile_raw(
    player: &dyn PlayerView,
    serial: u32,
    snapshot: &framework::continuum::EntitySnapshot,
) -> bytes::Bytes {
    use packets::world::{DrawMobile, DrawMobileExtended};
    use packets::traits::ManualPacket;

    let version = player.client_version();
    let raw = &snapshot.raw;
    let pkt_id = match raw.first() { Some(&id) => id, None => return raw.clone() };

    // Step 1: modern re-encode for DrawMobile / DrawMobileExtended.
    let reencoded: Option<bytes::Bytes> =
        if version >= ProtocolVersion::CV_70331 {
            match pkt_id {
                0x78 => {
                    DrawMobile::from_bytes(raw).ok().map(|m| m.to_bytes_versioned(version))
                }
                0xD3 => {
                    DrawMobileExtended::from_bytes(raw).ok().map(|m| m.to_bytes_versioned(version))
                }
                _ => None,
            }
        } else {
            None
        };

    // Use re-encoded bytes if available, otherwise fall back to cached raw.
    let base: bytes::Bytes = reencoded.unwrap_or_else(|| raw.clone());

    // Step 2: notoriety patch (only applicable to 0x78 packets).
    let Some(noto) = resolve_viewer_notoriety(player, serial, snapshot) else {
        return base;
    };
    if base.first() != Some(&0x78) || base.len() <= DRAW_MOBILE_NOTORIETY_OFFSET {
        return base;
    }
    if base[DRAW_MOBILE_NOTORIETY_OFFSET] == noto {
        return base; // already correct — avoid allocation
    }
    let mut patched = base.to_vec();
    patched[DRAW_MOBILE_NOTORIETY_OFFSET] = noto;
    bytes::Bytes::from(patched)
}
// ── Hidden-entity filter constant ─────────────────────────────────────────

/// `ObjectInfoFlags` / `MobileFlags` bit indicating a hidden entity.
const HIDDEN_FLAG: u8 = 0x80;

/// Returns `true` if the snapshot describes a multi-object (house / ship).
///
/// Multis are serialized as `0x1A ObjectInfo` (with `graphic + 0x4000`) or
/// the SA `0xF3 ObjectInfoSA` variant, whereas mobiles use `0x78 DrawMobile`.
/// We rely on the raw packet id because [`EntitySnapshot`] does not carry an
/// explicit entity-kind tag.
fn snapshot_is_multi(snapshot: &framework::continuum::EntitySnapshot) -> bool {
    match snapshot.raw.first() {
        // 0x1A ObjectInfo: id(1) + len(2) + object_id(4) → graphic at [7..9].
        // The multi marker is bit 14 (0x4000) of the graphic word.
        Some(&0x1A) => snapshot
            .raw
            .get(7..9)
            .map(|g| (u16::from_be_bytes([g[0], g[1]]) & 0x4000) != 0)
            .unwrap_or(false),
        // 0xF3 ObjectInfoSA: data_type byte (0x02 = Multi) follows the
        // 2-byte unknown-prefix at offset 3.
        Some(&0xF3) => snapshot.raw.get(3).map(|&b| b == 0x02).unwrap_or(false),
        _ => false,
    }
}

/// Returns `true` if the snapshot is a ground **item** (drawn via `0x1A
/// ObjectInfo` / `0xF3 ObjectInfoSA`), as opposed to a mobile (`0x78
/// DrawMobile`) or a multi.
///
/// Such an entity is relocated by re-sending its raw `ObjectInfo` packet at
/// the new origin (the same mechanism as a multi hull), **not** via the
/// mobile-only `UpdateMobile (0x77)`.
fn snapshot_is_item(snapshot: &framework::continuum::EntitySnapshot) -> bool {
    matches!(snapshot.raw.first(), Some(&0x1A) | Some(&0xF3)) && !snapshot_is_multi(snapshot)
}

/// Returns `true` if the event is a ship (multi) moving or turning.
///
/// Used by the session delivery layer to wrap the whole tick's packet batch
/// in `PauseClient (0x33)` so the client applies the hull and its passengers
/// as a single consistent frame (matching real-shard behaviour) — otherwise
/// the player visibly jitters as the boat and the on-deck mobile arrive in
/// separate, unsynchronised redraws.
pub fn is_ship_move_event(event: &WorldEvent) -> bool {
    match event {
        WorldEvent::ShipMoved { .. } => true,
        WorldEvent::EntityMoved { entity: Some(s), .. } => snapshot_is_multi(s),
        _ => false,
    }
}

// ── PlayerView trait ──────────────────────────────────────────────────────

/// Minimal view of player state required for world-event translation.
///
/// Both `demo-server::PlayerState` and `path-server::PlayerState` implement
/// this trait, allowing the shared [`collect_world_event_packets`] to work
/// with either.
pub trait PlayerView {
    fn serial(&self) -> u32;
    fn x(&self) -> u16;
    fn y(&self) -> u16;
    fn z(&self) -> i8;
    /// Current facing/direction byte (heading bits 0–2, running bit 7).
    fn direction(&self) -> u8;
    fn view_rect(&self) -> &TileRect;
    fn throttle_interval(&self) -> Duration;

    /// The client version of this connection.
    ///
    /// Used to select the correct wire format for version-dependent packets
    /// (e.g. the 0x78 DrawMobile equipment list is 9 bytes/item on modern
    /// clients >= 7.0.33.1).
    fn client_version(&self) -> ProtocolVersion { ProtocolVersion::SA_CLIENT }

    /// This viewer's own reputation context (guild id, class, etc.), used to
    /// resolve per-viewer notoriety colours for other mobiles.
    ///
    /// Returns `None` if the viewer has no reputation data yet (e.g. before
    /// the player entity is known); callers then fall back to base colours.
    fn notoriety_ctx(&self) -> Option<&framework::continuum::NotorietyContext> { None }

    // Mutable accessors needed for position updates and throttle tracking.
    fn set_position(&mut self, x: u16, y: u16, z: i8, direction: u8);
    fn move_throttle_get(&self, serial: u32) -> Option<Instant>;
    fn move_throttle_insert(&mut self, serial: u32, when: Instant);
    fn move_throttle_remove(&mut self, serial: u32);
}

// ── PlayerState ───────────────────────────────────────────────────────────

/// Authoritative player state, generic over per-server extensions.
///
/// Both `demo-server` and `path-server` use this struct directly:
///   - `demo-server` uses `PlayerState<()>`
///   - `path-server` uses `PlayerState<Option<PendingTarget>>`
///
/// All common fields (position, view rect, move throttle) are top-level;
/// the `extra` field holds server-specific data without requiring
/// indirection through a `base` sub-struct.
pub struct PlayerState<Extra = ()> {
    pub serial: u32,
    pub world: u8,
    pub x: u16,
    pub y: u16,
    pub z: i8,
    pub direction: u8,
    pub view_rect: TileRect,
    /// Chebyshev view distance in tiles (each direction from the player).
    ///
    /// Negotiated via packet `0xC8 ClientViewRange`.  Defaults to 18 (the
    /// classic-client value) until the client sends a `0xC8` request.  The
    /// server uses this value instead of a hard-coded constant so that
    /// clients that request a larger range (e.g. ClassicUO / EC at 24)
    /// receive entities all the way to their requested distance.
    pub view_range: u16,
    /// Per-entity move throttle.  When `throttle_interval` is non-zero,
    /// stores the last time an UpdateMobile was sent for each entity.
    pub move_throttle: std::collections::HashMap<u32, Instant>,
    /// Minimum interval between UpdateMobile packets for the same
    /// entity.  `Duration::ZERO` means throttling is disabled.
    pub throttle_interval: Duration,
    /// This player's own reputation context, refreshed when the player's
    /// mobile is (re)loaded.  Used to colour other mobiles per-viewer.
    pub notoriety_ctx: Option<framework::continuum::NotorietyContext>,
    /// Client version of the connected session — used to select the correct
    /// wire format for version-dependent packets (e.g. DrawMobile equipment
    /// list format differs between legacy and modern clients).
    pub client_version: ProtocolVersion,
    /// Server-specific extension data.
    pub extra: Extra,
}

impl<E> PlayerView for PlayerState<E> {
    fn serial(&self) -> u32 { self.serial }
    fn x(&self) -> u16 { self.x }
    fn y(&self) -> u16 { self.y }
    fn z(&self) -> i8 { self.z }
    fn direction(&self) -> u8 { self.direction }
    fn view_rect(&self) -> &TileRect { &self.view_rect }
    fn throttle_interval(&self) -> Duration { self.throttle_interval }
    fn client_version(&self) -> ProtocolVersion { self.client_version }
    fn notoriety_ctx(&self) -> Option<&framework::continuum::NotorietyContext> {
        self.notoriety_ctx.as_ref()
    }
    fn set_position(&mut self, x: u16, y: u16, z: i8, direction: u8) {
        self.x = x;
        self.y = y;
        self.z = z;
        self.direction = direction;
    }
    fn move_throttle_get(&self, serial: u32) -> Option<Instant> {
        self.move_throttle.get(&serial).copied()
    }
    fn move_throttle_insert(&mut self, serial: u32, when: Instant) {
        self.move_throttle.insert(serial, when);
    }
    fn move_throttle_remove(&mut self, serial: u32) {
        self.move_throttle.remove(&serial);
    }
}

// ── World event → packets ─────────────────────────────────────────────────

/// Convert a world event into outbound UO packets, appending them to `out`.
///
/// Events are pre-filtered by the observer registry — only events within
/// the session's view rectangle (and on the correct map) arrive here.
///
/// `on_s2c` is called for each packet that should be fed into an observer
/// pipeline (or any other S2C packet sink).  Pass `&mut |_: &[u8]| {}` if
/// no observer is needed.
///
/// This function does NOT send packets — it collects them into `out`
/// so the caller can batch-flush them in a single TCP write.
pub fn collect_world_event_packets(
    player: &mut dyn PlayerView,
    event: &WorldEvent,
    access_level: AccessLevel,
    on_s2c: &mut dyn FnMut(&[u8]),
    out: &mut Vec<RawPacket>,
) {
    // Helper: returns `true` if the entity is hidden and the observer
    // does not have sufficient access to see hidden entities (GM+).
    let is_hidden_from_observer = |snapshot: &framework::continuum::EntitySnapshot| -> bool {
        snapshot.status_flags & HIDDEN_FLAG != 0
            && access_level < AccessLevel::GameMaster
    };

    match event {
        WorldEvent::ShipMoved {
            ship_serial,
            ship_old_pos,
            ship_new_pos,
            ship_snapshot,
            passengers,
            cargo,
            ..
        } => {
            // ── 1. Relocate the hull ──────────────────────────────────
            //
            // A multi can only be relocated by re-sending its `0x1A
            // ObjectInfo` at the new origin (no `UpdateMobile` for multis).
            let hull_old_vis = player
                .view_rect()
                .contains_pos(&u_core::Pos3D::new(ship_old_pos.x, ship_old_pos.y, ship_old_pos.z));
            let hull_new_vis = player
                .view_rect()
                .contains_pos(&u_core::Pos3D::new(ship_new_pos.x, ship_new_pos.y, ship_new_pos.z));
            if let Some(s) = ship_snapshot {
                if !is_hidden_from_observer(s) {
                    if hull_new_vis {
                        on_s2c(&s.raw);
                        out.push(RawPacket::s2c(s.raw.clone()));
                    } else if hull_old_vis {
                        collect_delete_object(*ship_serial, on_s2c, out);
                    }
                }
            }

            // ── 2. Snap each passenger ─────────────────────────────────
            for (serial, old_pos, new_pos, entity) in passengers {
                if *serial == player.serial() {
                    // The carrying tick moves the player by coordinates only.
                    // Keep the player's *current* facing (owned by the client
                    // via MoveRequest) so the tick never rolls back a turn the
                    // player just made.  Always send a DrawGamePlayer so the
                    // hull and the player stay in lockstep within this frame.
                    let cur_dir = player.direction();
                    player.set_position(new_pos.x, new_pos.y, new_pos.z, cur_dir);
                    if let Some(s) = entity {
                        let pkt = RawPacket::s2c(encode_packet(&DrawGamePlayer {
                            id: 0x20,
                            serial: *serial,
                            body_type: s.graphic,
                            _pad0: (),
                            hue: s.hue,
                            flags: MobileFlags(s.status_flags),
                            x: new_pos.x,
                            y: new_pos.y,
                            _pad1: (),
                            direction: cur_dir,
                            z: new_pos.z,
                        }));
                        on_s2c(&pkt.data);
                        out.push(pkt);
                    }
                    continue;
                }

                // Other passengers (NPCs etc.) — relocate within / into / out
                // of view exactly like a normal EntityMoved.
                let was_visible = player.view_rect().contains_mpos(old_pos);
                let now_visible = player.view_rect().contains_mpos(new_pos);

                if let Some(s) = entity {
                    if snapshot_is_multi(s) {
                        if is_hidden_from_observer(s) { continue; }
                        if now_visible {
                            on_s2c(&s.raw);
                            out.push(RawPacket::s2c(s.raw.clone()));
                        } else if was_visible {
                            collect_delete_object(*serial, on_s2c, out);
                        }
                        continue;
                    }
                }

                if now_visible && was_visible {
                    if let Some(s) = entity {
                        if is_hidden_from_observer(s) { continue; }
                        let noto = resolve_viewer_notoriety(player, *serial, s)
                            .unwrap_or(s.notoriety);
                        let pkt = RawPacket::s2c(encode_packet(&UpdateMobile {
                            id: 0x77,
                            serial: *serial,
                            model: s.graphic,
                            x: new_pos.x,
                            y: new_pos.y,
                            z: new_pos.z,
                            direction: new_pos.facing.raw(),
                            hue: s.hue,
                            status_flags: MobileFlags(s.status_flags),
                            notoriety: Notoriety::from_wire(noto),
                        }));
                        on_s2c(&pkt.data);
                        out.push(pkt);
                    }
                } else if now_visible && !was_visible {
                    if let Some(s) = entity {
                        if is_hidden_from_observer(s) { continue; }
                        let raw = prepare_mobile_raw(player, *serial, s);
                        on_s2c(&raw);
                        out.push(RawPacket::s2c(raw));
                    }
                } else if !now_visible && was_visible {
                    player.move_throttle_remove(*serial);
                    collect_delete_object(*serial, on_s2c, out);
                }
            }

            // ── 3. Relocate carried deck items (cargo) ─────────────────
            //
            // Items are drawn via `0x1A ObjectInfo` (their raw snapshot), the
            // same mechanism as the hull — never `UpdateMobile`.  Re-send the
            // raw packet at the new origin when in view; delete when it leaves.
            for (serial, old_pos, new_pos, entity) in cargo {
                let was_visible = player
                    .view_rect()
                    .contains_pos(&u_core::Pos3D::new(old_pos.x, old_pos.y, old_pos.z));
                let now_visible = player
                    .view_rect()
                    .contains_pos(&u_core::Pos3D::new(new_pos.x, new_pos.y, new_pos.z));

                if let Some(s) = entity {
                    if is_hidden_from_observer(s) { continue; }
                    if now_visible {
                        on_s2c(&s.raw);
                        out.push(RawPacket::s2c(s.raw.clone()));
                    } else if was_visible {
                        collect_delete_object(*serial, on_s2c, out);
                    }
                } else if was_visible && !now_visible {
                    collect_delete_object(*serial, on_s2c, out);
                }
            }
        }

        WorldEvent::EntityMoved {
            serial,
            old_pos,
            new_pos,
            entity,
            is_teleport,
            ..
        } => {
            if *serial == player.serial() {
                if !is_teleport {
                    // Normal step — already handled inline via MoveAck.
                    // Nothing to send; the client's position is up to date.
                    return;
                }

                // External move (teleport / script) — send DrawGamePlayer
                // so the client snaps to the new position.
                player.set_position(new_pos.x, new_pos.y, new_pos.z, new_pos.facing.raw());

                if let Some(s) = entity {
                    let pkt = RawPacket::s2c(encode_packet(&DrawGamePlayer {
                        id: 0x20,
                        serial: *serial,
                        body_type: s.graphic,
                        _pad0: (),
                        hue: s.hue,
                        flags: MobileFlags(s.status_flags),
                        x: new_pos.x,
                        y: new_pos.y,
                        _pad1: (),
                        direction: new_pos.facing.raw(),
                        z: new_pos.z,
                    }));
                    on_s2c(&pkt.data);
                    out.push(pkt);
                }
                return;
            }

            let was_visible = player.view_rect().contains_mpos(old_pos);
            let now_visible = player.view_rect().contains_mpos(new_pos);

            // ── Multi (ship / movable structure) relocation ──────────────
            //
            // A multi cannot be moved with `UpdateMobile (0x77)` — the
            // client only knows multis via `0x1A ObjectInfo` (or the SA
            // `0xF3` variant).  Re-sending that packet at the new origin
            // relocates the hull *in place*, without the `DeleteObject
            // (0x1D)` + redraw pair that caused the per-tick flicker.
            if let Some(s) = entity {
                if snapshot_is_multi(s) {
                    if is_hidden_from_observer(s) { return; }
                    if now_visible {
                        // Both moved-within-view and entered-view collapse to
                        // "draw the multi at its new position".
                        on_s2c(&s.raw);
                        out.push(RawPacket::s2c(s.raw.clone()));
                    } else if was_visible {
                        // Left view — remove it.
                        collect_delete_object(*serial, on_s2c, out);
                    }
                    return;
                }

                // ── Ground item relocation ──────────────────────────────
                //
                // An item is drawn via `0x1A ObjectInfo`, not the mobile-only
                // `0x77 UpdateMobile`.  Re-send its raw snapshot (which already
                // carries the new coordinates) to relocate it in place; this is
                // how a deck item carried by a ship turn reaches the client.
                if snapshot_is_item(s) {
                    if is_hidden_from_observer(s) { return; }
                    if now_visible {
                        on_s2c(&s.raw);
                        out.push(RawPacket::s2c(s.raw.clone()));
                    } else if was_visible {
                        collect_delete_object(*serial, on_s2c, out);
                    }
                    return;
                }
            }

            if now_visible && was_visible {
                // Entity moved or turned within view — send UpdateMobile.
                // Optionally throttle per-entity to reduce bandwidth.
                if let Some(s) = entity {
                    if is_hidden_from_observer(s) { return; }
                }
                if player.throttle_interval() > Duration::ZERO {
                    let now = Instant::now();
                    if let Some(last) = player.move_throttle_get(*serial) {
                        if now.duration_since(last) < player.throttle_interval() {
                            return; // throttled — skip
                        }
                    }
                    player.move_throttle_insert(*serial, now);
                }

                if let Some(s) = entity {
                    let noto = resolve_viewer_notoriety(player, *serial, s)
                        .unwrap_or(s.notoriety);
                    let pkt = RawPacket::s2c(encode_packet(&UpdateMobile {
                        id: 0x77,
                        serial: *serial,
                        model: s.graphic,
                        x: new_pos.x,
                        y: new_pos.y,
                        z: new_pos.z,
                        direction: new_pos.facing.raw(),
                        hue: s.hue,
                        status_flags: MobileFlags(s.status_flags),
                        notoriety: Notoriety::from_wire(noto),
                    }));
                    on_s2c(&pkt.data);
                    out.push(pkt);
                }
            } else if now_visible && !was_visible {
                // Entity entered view — send full DrawMobile (0x78).
                if let Some(s) = entity {
                    if is_hidden_from_observer(s) { return; }
                    let raw = prepare_mobile_raw(player, *serial, s);
                    on_s2c(&raw);
                    out.push(RawPacket::s2c(raw));
                }
            } else if !now_visible && was_visible {
                // Entity left view — send DeleteObject (0x1D).
                player.move_throttle_remove(*serial);
                collect_delete_object(*serial, on_s2c, out);
            }
        }

        WorldEvent::EntitySpawned { serial, entity, .. } => {
            if let Some(s) = entity {
                if is_hidden_from_observer(s) { return; }
                let raw = prepare_mobile_raw(player, *serial, s);
                on_s2c(&raw);
                out.push(RawPacket::s2c(raw));
            }
        }

        WorldEvent::EntityRemoved { serial, .. } => {
            player.move_throttle_remove(*serial);
            collect_delete_object(*serial, on_s2c, out);
        }

        WorldEvent::EntityUpdated { serial, entity, .. } => {
            if let Some(s) = entity {
                if is_hidden_from_observer(s) { return; }
                let raw = prepare_mobile_raw(player, *serial, s);
                on_s2c(&raw);
                out.push(RawPacket::s2c(raw));
            }
        }

        WorldEvent::SoundPlayed {
            sound_id, x, y, z, ..
        } => {
            let pkt = RawPacket::s2c(encode_packet(&PlaySoundEffect {
                id: 0x54,
                mode: 0x01,
                sound_model: *sound_id,
                unknown: 0,
                x: *x,
                y: *y,
                z: *z,
            }));
            out.push(pkt);
        }

        WorldEvent::EffectPlayed {
            direction_type,
            source_serial,
            target_serial,
            graphic,
            x,
            y,
            z,
            target_x,
            target_y,
            target_z,
            speed,
            duration,
            fixed_direction,
            explode,
            ..
        } => {
            use packets::world::GraphicalEffect;
            let pkt = RawPacket::s2c(encode_packet(&GraphicalEffect {
                id: 0x70,
                direction_type: *direction_type,
                source_serial: *source_serial,
                target_serial: *target_serial,
                model: *graphic,
                x: *x,
                y: *y,
                z: *z,
                target_x: *target_x,
                target_y: *target_y,
                target_z: *target_z,
                speed: *speed,
                duration: *duration,
                _pad0: (),
                fixed_direction: u8::from(*fixed_direction),
                explode: u8::from(*explode),
            }));
            out.push(pkt);
        }

        WorldEvent::AnimationPlayed {
            serial,
            action,
            frame_count,
            repeat_count,
            reverse,
            repeat,
            frame_delay,
            ..
        } => {
            let pkt = RawPacket::s2c(encode_packet(&CharacterAnimation {
                id: 0x6E,
                serial: *serial,
                action: *action,
                _pad0: (),
                frame_count: *frame_count,
                repeat_count: *repeat_count,
                direction: u8::from(*reverse),
                repeat_flag: u8::from(*repeat),
                frame_delay: *frame_delay,
            }));
            out.push(pkt);
        }

        WorldEvent::Speech {
            serial,
            graphic,
            speech_type,
            color,
            font,
            name,
            message,
            ..
        } => {
            let st = SpeechType::from_wire(*speech_type);
            let pkt = RawPacket::s2c(
                SendSpeech {
                    serial: *serial,
                    model: *graphic,
                    speech_type: st,
                    color: *color,
                    font: *font,
                    name: name.clone(),
                    message: message.clone(),
                }
                .to_bytes(),
            );
            out.push(pkt);
        }

        WorldEvent::GlobalLight { level, .. } => {
            out.push(RawPacket::s2c(encode_packet(&OverallLightLevel {
                id: 0x4F,
                level: *level,
            })));
        }

        WorldEvent::Weather { weather_type, num_effects, temperature, .. } => {
            out.push(RawPacket::s2c(encode_packet(&SetWeather {
                id: 0x65,
                weather_type: *weather_type,
                num_effects: *num_effects,
                temperature: *temperature,
            })));
        }

        WorldEvent::Season { season, play_sound, .. } => {
            let season_val = match season {
                0 => Season::Spring,
                1 => Season::Summer,
                2 => Season::Fall,
                3 => Season::Winter,
                4 => Season::Desolation,
                v => Season::Unknown(*v),
            };
            out.push(RawPacket::s2c(encode_packet(&SeasonalInformation {
                id: 0xBC,
                season: season_val,
                play_sound: if *play_sound { 1 } else { 0 },
            })));
        }

        WorldEvent::Music { music_id, .. } => {
            out.push(RawPacket::s2c(encode_packet(&PlayMidiMusic {
                id: 0x6D,
                music_id: *music_id,
            })));
        }

        WorldEvent::MobileKilled {
            serial,
            body_graphic,
            hue,
            x,
            y,
            z,
            direction,
            corpse_serial,
            corpse_items,
            ..
        } => {
            // 1. DisplayDeathAction (0xAF) — death animation
            out.push(RawPacket::s2c(encode_packet(&DisplayDeathAction::new(
                *serial,
                *corpse_serial,
            ))));

            // 2. ObjectInfo (0x1A) — the corpse as a world item
            //    graphic = 0x2006 (human corpse), amount = body_graphic
            use packets::world::{ObjectInfo, ObjectInfoFlags};
            out.push(RawPacket::s2c(ObjectInfo {
                object_id: *corpse_serial,
                graphic: 0x2006,
                amount: Some(*body_graphic),
                graphic_increment: None,
                x: *x,
                y: *y,
                facing: Some(*direction & 0x07),
                z: *z,
                dye: Some(*hue),
                flags: Some(ObjectInfoFlags(0)),
            }.to_bytes()));

            // 3. CorpseClothing (0x89) — equipment visible on the corpse
            if !corpse_items.is_empty() {
                let entries: Vec<CorpseClothingEntry> = corpse_items
                    .iter()
                    .map(|(layer, item_serial, _gfx, _color)| CorpseClothingEntry {
                        layer: packets::layer::Layer::from_wire(*layer),
                        item_id: *item_serial,
                    })
                    .collect();
                out.push(RawPacket::s2c(CorpseClothing {
                    corpse_id: *corpse_serial,
                    items: entries,
                }.to_bytes()));
            }

            // 4. DeleteObject (0x1D) — remove the living mobile
            collect_delete_object(*serial, on_s2c, out);
        }

        WorldEvent::PlayerDied {
            serial,
            body_graphic,
            ghost_graphic,
            hue,
            x,
            y,
            z,
            direction,
            corpse_serial,
            corpse_items,
            entity,
            mount_item_serial,
            ..
        } => {
            use packets::world::{ObjectInfo, ObjectInfoFlags};

            // 1. DisplayDeathAction (0xAF) — death animation.
            push_s2c(out, on_s2c, encode_packet(&DisplayDeathAction::new(
                *serial,
                *corpse_serial,
            )));

            // 2. ObjectInfo (0x1A) — the corpse as a world item.
            push_s2c(out, on_s2c, ObjectInfo {
                object_id: *corpse_serial,
                graphic: 0x2006,
                amount: Some(*body_graphic),
                graphic_increment: None,
                x: *x,
                y: *y,
                facing: Some(*direction & 0x07),
                z: *z,
                dye: Some(*hue),
                flags: Some(ObjectInfoFlags(0)),
            }.to_bytes());

            // 3. CorpseClothing (0x89) — equipment visible on the corpse.
            if !corpse_items.is_empty() {
                let entries: Vec<CorpseClothingEntry> = corpse_items
                    .iter()
                    .map(|(layer, item_serial, _gfx, _color)| CorpseClothingEntry {
                        layer: packets::layer::Layer::from_wire(*layer),
                        item_id: *item_serial,
                    })
                    .collect();
                push_s2c(out, on_s2c, CorpseClothing {
                    corpse_id: *corpse_serial,
                    items: entries,
                }.to_bytes());
            }

            // 4. Swap the body to the ghost graphic.
            if *serial == player.serial() {
                // Own client: remove the mount item so the player is no longer
                // shown riding (the mount becomes a separate NPC).
                if let Some(mount_serial) = mount_item_serial {
                    collect_delete_object(*mount_serial, on_s2c, out);
                }

                // Own client: DrawGamePlayer (0x20) with the ghost body.
                push_s2c(out, on_s2c, encode_packet(&DrawGamePlayer {
                    id: 0x20,
                    serial: *serial,
                    body_type: *ghost_graphic,
                    _pad0: (),
                    hue: *hue,
                    flags: MobileFlags(0),
                    x: *x,
                    y: *y,
                    _pad1: (),
                    direction: *direction,
                    z: *z,
                }));

                // Own client: 0x20 updates only the body, not equipment.
                // Send the full DrawMobile (0x78) snapshot so the player sees
                // the burial shroud (death robe) and the items dropped to the
                // corpse are removed from their paperdoll without a relog.
                if let Some(s) = entity {
                    let raw = s.raw.clone();
                    on_s2c(&raw);
                    out.push(RawPacket::s2c(raw));
                }

                // System message for the dying player.
                out.push(crate::world_events::system_message_gray(
                    "You are dead. Find a healer to be resurrected.",
                ));
            } else {
                // Observer: full DrawMobile (0x78) with the ghost body and
                // current equipment.  Unlike UpdateMobile (0x77), this carries
                // the equipment list so the mount layer (dropped on death) is
                // removed from the observer's view.
                if let Some(s) = entity {
                    let raw = prepare_mobile_raw(player, *serial, s);
                    on_s2c(&raw);
                    out.push(RawPacket::s2c(raw));
                } else {
                    // Fallback: body-only update if no snapshot is available.
                    push_s2c(out, on_s2c, encode_packet(&UpdateMobile {
                        id: UpdateMobile::ID,
                        serial: *serial,
                        model: *ghost_graphic,
                        x: *x,
                        y: *y,
                        z: *z,
                        direction: *direction,
                        hue: *hue,
                        status_flags: MobileFlags(0),
                        notoriety: Notoriety::Innocent,
                    }));
                }
            }
        }

        WorldEvent::PlayerResurrected {
            serial,
            body_graphic,
            hue,
            x,
            y,
            z,
            direction,
            new_hits,
            max_hits,
            entity,
            ..
        } => {
            use packets::status::UpdateHealth;

            if *serial == player.serial() {
                // Own client: DrawGamePlayer (0x20) with the living body.
                push_s2c(out, on_s2c, encode_packet(&DrawGamePlayer {
                    id: 0x20,
                    serial: *serial,
                    body_type: *body_graphic,
                    _pad0: (),
                    hue: *hue,
                    flags: MobileFlags(0),
                    x: *x,
                    y: *y,
                    _pad1: (),
                    direction: *direction,
                    z: *z,
                }));

                // Own client: 0x20 updates only the body, not equipment.
                // Send the full DrawMobile (0x78) snapshot so the items
                // returned from the corpse re-appear on the paperdoll (and the
                // burial shroud is removed) without needing to re-equip or relog.
                if let Some(s) = entity {
                    let raw = s.raw.clone();
                    on_s2c(&raw);
                    out.push(RawPacket::s2c(raw));
                }

                out.push(crate::world_events::system_message_gray(
                    "You have been resurrected.",
                ));

                // Resurrection sound (0x0214) for the resurrected player.
                push_s2c(out, on_s2c, encode_packet(&PlaySoundEffect {
                    id: 0x54,
                    mode: 0x01,
                    sound_model: 0x0214,
                    unknown: 0,
                    x: *x,
                    y: *y,
                    z: *z as i16,
                }));
            } else {
                // Observer: full DrawMobile (0x78) with the living body and
                // current equipment, so any stale mount layer is reconciled.
                if let Some(s) = entity {
                    let raw = prepare_mobile_raw(player, *serial, s);
                    on_s2c(&raw);
                    out.push(RawPacket::s2c(raw));
                } else {
                    // Fallback: body-only update if no snapshot is available.
                    push_s2c(out, on_s2c, encode_packet(&UpdateMobile {
                        id: UpdateMobile::ID,
                        serial: *serial,
                        model: *body_graphic,
                        x: *x,
                        y: *y,
                        z: *z,
                        direction: *direction,
                        hue: *hue,
                        status_flags: MobileFlags(0),
                        notoriety: Notoriety::Innocent,
                    }));
                }
            }

            // Health bar update for both.
            push_s2c(out, on_s2c, encode_packet(&UpdateHealth {
                id: UpdateHealth::ID,
                serial: *serial,
                max_health: *max_hits,
                current_health: *new_hits,
            }));
        }

        WorldEvent::GhostVisibilityChanged { serial, visible, entity, .. } => {
            // The ghost's own client always sees itself via DrawGamePlayer,
            // so it ignores visibility changes about its own serial.
            if *serial == player.serial() {
                return;
            }
            if *visible {
                if let Some(s) = entity {
                    let raw = prepare_mobile_raw(player, *serial, s);
                    on_s2c(&raw);
                    out.push(RawPacket::s2c(raw));
                }
            } else {
                collect_delete_object(*serial, on_s2c, out);
            }
        }

        WorldEvent::DamageDealt {
            serial, source_serial, amount, new_hits, max_hits, ..
        } => {
            // Send UpdateHealth (0xA1) for HP bar update.
            use packets::status::UpdateHealth;
            out.push(RawPacket::s2c(encode_packet(&UpdateHealth {
                id: UpdateHealth::ID,
                serial: *serial,
                max_health: *max_hits,
                current_health: *new_hits,
            })));

            // Overhead damage number — only for the player who dealt the damage.
            if *source_serial == player.serial() {
                use packets::u_io::{FixedString, NullUnicodeString};
                out.push(RawPacket::s2c(encode_packet(&UnicodeSpeech {
                    id: UnicodeSpeech::ID,
                    len: 0,
                    serial: *serial,
                    model: 0,
                    speech_type: SpeechType::Normal,
                    color: 0x0085,
                    font: 9,
                    language: FixedString("ENU".to_string()),
                    name: FixedString(String::new()),
                    message: NullUnicodeString(format!("-{}", amount)),
                })));
            }
        }

        WorldEvent::MobileHealed {
            serial, new_hits, max_hits, ..
        } => {
            use packets::status::UpdateHealth;
            out.push(RawPacket::s2c(encode_packet(&UpdateHealth {
                id: UpdateHealth::ID,
                serial: *serial,
                max_health: *max_hits,
                current_health: *new_hits,
            })));
        }

        WorldEvent::ManaStaminaChanged {
            serial, mana, max_mana, stamina, max_stamina, ..
        } => {
            // Only send to the mobile's own session.
            if *serial == player.serial() {
                use packets::status::{UpdateMana, UpdateStamina};
                out.push(RawPacket::s2c(encode_packet(&UpdateMana {
                    id: UpdateMana::ID,
                    serial: *serial,
                    max_mana: *max_mana,
                    current_mana: *mana,
                })));
                out.push(RawPacket::s2c(encode_packet(&UpdateStamina {
                    id: UpdateStamina::ID,
                    serial: *serial,
                    max_stamina: *max_stamina,
                    current_stamina: *stamina,
                })));
            }
        }

        WorldEvent::BaseStatChanged {
            serial, str_, dex, int,
            hits, hits_max, mana, mana_max, stamina, stamina_max, ..
        } => {
            // Only send to the mobile's own session.
            // UO has no lightweight per-stat update packet, so we build a
            // full StatusBarInfo (0x11) with the stats carried in the event.
            if *serial == player.serial() {
                use packets::status::StatusBarInfo;
                let sbi = StatusBarInfo {
                    serial: *serial,
                    name: packets::u_io::FixedString::new(""),
                    hit_points: *hits,
                    max_hit_points: *hits_max,
                    name_change_flag: 0,
                    status_flag: 1, // full stats (self)
                    is_female: Some(false), // TODO: derive from mobile graphic when available
                    stats: Some(packets::status::BaseStats {
                        strength: *str_,
                        dexterity: *dex,
                        intelligence: *int,
                        stamina: *stamina,
                        max_stamina: *stamina_max,
                        mana: *mana,
                        max_mana: *mana_max,
                        gold: 0,
                        armor_rating: 0,
                        weight: 0,
                    }),
                    uoml: None,
                    uor: None,
                    aos: None,
                    uokr: None,
                };
                out.push(RawPacket::s2c(sbi.to_bytes()));
            }
        }

        WorldEvent::ContainerContentsUpdated { .. } => {
            // Container content updates are handled by session-level code
            // which has access to open_containers state.  The shared
            // world-event translator cannot decide whether to forward
            // these packets, so this is intentionally a no-op here.
        }

        // Targeted events are handled by session-level code that knows
        // whether this session is the intended recipient.
        WorldEvent::TargetedGump { .. }
        | WorldEvent::TargetedMessage { .. }
        | WorldEvent::TargetedCloseGump { .. }
        | WorldEvent::TargetedTargetCursor { .. }
        | WorldEvent::TargetedCrossWorldTeleport { .. }
        | WorldEvent::SnapshotRestored { .. } => {}
    }
}

/// Legacy wrapper: handle a single world event and send immediately.
///
/// Used during initial entity streaming (spawn phase) where events
/// arrive one at a time and must be sent before LoginComplete.
pub async fn handle_world_event<P: PlayerView, F: FnMut(&[u8])>(
    session: &mut Session,
    player: &mut P,
    event: &WorldEvent,
    access_level: AccessLevel,
    on_s2c: &mut F,
) -> error::Result<()> {
    let mut pkts = Vec::new();
    collect_world_event_packets(player, event, access_level, on_s2c, &mut pkts);
    if !pkts.is_empty() {
        session.send_all(pkts).await?;
    }
    Ok(())
}

/// Append a DeleteObject (0x1D) packet to `out`.
pub fn collect_delete_object(
    serial: u32,
    on_s2c: &mut dyn FnMut(&[u8]),
    out: &mut Vec<RawPacket>,
) {
    use packets::interaction::DeleteObject;
    let pkt = RawPacket::s2c(encode_packet(&DeleteObject {
        id: 0x1D,
        serial,
    }));
    on_s2c(&pkt.data);
    out.push(pkt);
}

/// Build a gray system message (`SendSpeech` 0x1C, type System) addressed to
/// the client (serial `0xFFFF_FFFF`).
pub fn system_message_gray(message: &str) -> RawPacket {
    RawPacket::s2c(
        SendSpeech {
            serial: 0xFFFF_FFFF,
            model: 0xFFFF,
            speech_type: SpeechType::System,
            color: 0x03B2, // gray
            font: 3,
            name: String::new(),
            message: message.to_string(),
        }
        .to_bytes(),
    )
}

/// Push an already-encoded S→C packet into `out`, also feeding the observer
/// hook so cross-validation pipelines stay in sync.
fn push_s2c(out: &mut Vec<RawPacket>, on_s2c: &mut dyn FnMut(&[u8]), data: bytes::Bytes) {
    let pkt = RawPacket::s2c(data);
    on_s2c(&pkt.data);
    out.push(pkt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use packets::traits::ManualPacket;
    use packets::world::DrawMobile;

    #[test]
    fn draw_mobile_notoriety_offset_is_correct() {
        // Encode a DrawMobile with a distinctive notoriety value and verify
        // the byte at DRAW_MOBILE_NOTORIETY_OFFSET matches.
        let dm = DrawMobile {
            serial: 0x0123_4567,
            graphic: 0x0190,
            x: 1000,
            y: 2000,
            z: 5,
            direction: 2,
            color: 0x83EA,
            status: MobileFlags(0),
            notoriety: Notoriety::Murderer, // wire byte 6
            items: Vec::new(),
        };
        let bytes = dm.to_bytes();
        assert!(bytes.len() > DRAW_MOBILE_NOTORIETY_OFFSET);
        assert_eq!(bytes[0], 0x78);
        assert_eq!(bytes[DRAW_MOBILE_NOTORIETY_OFFSET], 6);
    }
}
