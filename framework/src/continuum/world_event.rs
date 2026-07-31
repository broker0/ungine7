//! World events published by the [`Worker`](super::worker::Worker) and routed to connected sessions.
//!
//! When the authoritative world state changes (entity moves, spawns,
//! is removed, etc.) a [`WorldEvent`] is published via the worker's
//! [`tokio::sync::mpsc::unbounded_channel`].  The single consumer
//! (`BaseHandler`) drains these events, coalesces entity movements,
//! and fans them out to per-session mpsc channels through the observer
//! registry.

use u_core::{MobilePos, Pos3D};

use crate::vessel::EntitySnapshot;

/// Describes a single change to a container's contents.
///
/// Used inside [`WorldEvent::ContainerContentsUpdated`] to communicate
/// item additions, removals, and updates to sessions that have the
/// container open.
#[derive(Debug, Clone)]
pub enum ContainerContentChange {
    /// An item was added to the container.
    ItemAdded {
        item_serial: u32,
        graphic: u16,
        amount: u16,
        /// Gump-relative X position inside the container.
        x: u16,
        /// Gump-relative Y position inside the container.
        y: u16,
        color: u16,
    },
    /// An item was removed from the container.
    ItemRemoved {
        item_serial: u32,
    },
    /// An item's amount or position was updated (stack merge, partial
    /// consume, etc.).  The client treats 0x25 as an upsert, so sending
    /// an updated `AddItemToContainer` replaces the existing entry.
    ItemUpdated {
        item_serial: u32,
        graphic: u16,
        amount: u16,
        x: u16,
        y: u16,
        color: u16,
    },
}

/// A world-state change that should be communicated to nearby observers.
///
/// The `map_id` field identifies which map the event belongs to, so
/// sessions on other maps can cheaply skip irrelevant events.
///
/// Entity events carry an optional [`EntitySnapshot`] so that observers
/// can emit S→C packets directly without querying the worker.
#[derive(Debug, Clone)]
pub enum WorldEvent {
    /// A mobile moved or turned in place.
    ///
    /// When `old_pos.pos3d() == new_pos.pos3d()` only the facing
    /// changed (turn-in-place).  Observers should still send an
    /// `UpdateMobile` so that other clients see the new direction.
    EntityMoved {
        map_id: u8,
        serial: u32,
        old_pos: MobilePos,
        new_pos: MobilePos,
        /// Snapshot of the entity *after* the move.
        entity: Option<EntitySnapshot>,
        /// `true` when the move was a teleport (script, `.tele`, map
        /// transfer) rather than a normal one-tile step.  Sessions use
        /// this to decide whether to send `DrawGamePlayer` (teleport)
        /// or silently skip the event for the player's own serial
        /// (normal step already handled via `MoveAck`).
        is_teleport: bool,
    },

    /// A ship (multi) moved one tile, carrying its on-deck passengers.
    ///
    /// This is an **atomic** alternative to emitting one `EntityMoved` for the
    /// hull plus one per passenger: bundling them in a single event guarantees
    /// the session drains and renders the hull move and every passenger snap
    /// in one batch (one `PauseClient` frame), so on-deck mobiles never jitter
    /// out of sync with the hull.
    ///
    /// For the player's *own* serial among `passengers`, sessions update only
    /// the position and **keep the player's current facing** (the heading is
    /// owned by the player's `MoveRequest`s, not the sailing tick) — this
    /// prevents the tick from rolling back a turn the player just made.
    ShipMoved {
        map_id: u8,
        ship_serial: u32,
        ship_old_pos: Pos3D,
        ship_new_pos: Pos3D,
        /// Snapshot of the hull multi after the move (for the `ObjectInfo` redraw).
        ship_snapshot: Option<EntitySnapshot>,
        /// Passengers carried by this move: `(serial, old_pos, new_pos, snapshot)`.
        passengers: Vec<(u32, MobilePos, MobilePos, Option<EntitySnapshot>)>,
        /// Deck items (cargo) carried by this move:
        /// `(serial, old_pos, new_pos, snapshot)`.
        ///
        /// Unlike passengers, items have no facing and are redrawn with their
        /// raw `0x1A ObjectInfo` packet at the new origin (same as the hull),
        /// so they relocate atomically within the same `ShipMoved` frame.
        cargo: Vec<(u32, Pos3D, Pos3D, Option<EntitySnapshot>)>,
    },

    /// A new entity was spawned into the world.
    EntitySpawned {
        map_id: u8,
        serial: u32,
        pos: Pos3D,
        /// Snapshot of the spawned entity.
        entity: Option<EntitySnapshot>,
    },

    /// An entity was removed from the world.
    EntityRemoved {
        map_id: u8,
        serial: u32,
        last_pos: Pos3D,
    },

    /// An entity's state was updated (equipment, stats, hue, etc.)
    /// without a position change.
    EntityUpdated {
        map_id: u8,
        serial: u32,
        pos: Pos3D,
        /// Snapshot of the entity after the update.
        entity: Option<EntitySnapshot>,
    },

    /// A ghost player's visibility to *other* observers changed.
    ///
    /// When `visible` is `true`, observers (other than the ghost itself)
    /// should draw the ghost mobile (`DrawMobile`); when `false`, they should
    /// remove it (`DeleteObject`).  The ghost's own session ignores this
    /// event — it always sees its own body via `DrawGamePlayer`.
    GhostVisibilityChanged {
        map_id: u8,
        serial: u32,
        visible: bool,
        x: u16,
        y: u16,
        /// Snapshot of the ghost mobile (used to draw it when becoming visible).
        entity: Option<EntitySnapshot>,
    },

    /// A sound effect was played at a world position.
    SoundPlayed {
        map_id: u8,
        sound_id: u16,
        x: u16,
        y: u16,
        z: i16,
    },

    /// A graphical effect was spawned (projectile, lightning, stationary).
    EffectPlayed {
        map_id: u8,
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

    /// A character animation was triggered.
    AnimationPlayed {
        map_id: u8,
        serial: u32,
        action: u16,
        frame_count: u8,
        repeat_count: u16,
        reverse: bool,
        repeat: bool,
        frame_delay: u8,
        /// Position of the entity (for spatial routing).
        x: u16,
        y: u16,
    },

    /// A speech message was broadcast in the world.
    Speech {
        map_id: u8,
        serial: u32,
        graphic: u16,
        speech_type: u8,
        color: u16,
        font: u16,
        name: String,
        message: String,
        /// Position of the speaker (for range filtering).
        x: u16,
        y: u16,
    },

    /// Global light level changed (broadcast to all sessions on the map).
    GlobalLight {
        map_id: u8,
        /// Light level: `0x00` = full day, `0x1F` = pitch black.
        level: u8,
    },

    /// Weather changed (broadcast to all sessions on the map).
    Weather {
        map_id: u8,
        /// Weather type: 0=rain, 1=storm, 2=snow, 0xFF=none.
        weather_type: u8,
        /// Number of weather particles.
        num_effects: u8,
        /// Temperature value.
        temperature: u8,
    },

    /// Season changed (broadcast to all sessions on the map).
    Season {
        map_id: u8,
        /// Season: 0=spring, 1=summer, 2=fall, 3=winter, 4=desolation.
        season: u8,
        /// Whether to play the transition sound effect.
        play_sound: bool,
    },

    /// Background music changed (broadcast to all sessions on the map).
    Music {
        map_id: u8,
        /// Music track ID.
        music_id: u16,
    },

    /// A mobile was killed — produces corpse + death animation.
    ///
    /// The game session translates this into the full UO death sequence:
    /// `DisplayDeathAction` (0xAF) + `ObjectInfo` corpse (0x1A) +
    /// `CorpseClothing` (0x89) + `DeleteObject` (0x1D).
    MobileKilled {
        map_id: u8,
        /// Serial of the mobile that died.
        serial: u32,
        /// Body graphic of the mobile (used as corpse `amount`).
        body_graphic: u16,
        /// Hue of the corpse.
        hue: u16,
        /// Position of death.
        x: u16,
        y: u16,
        z: i8,
        /// Direction the corpse faces.
        direction: u8,
        /// Serial allocated for the corpse item.
        corpse_serial: u32,
        /// Equipment items to display on the corpse.
        /// Each: (layer_wire_value, item_serial, item_graphic, item_color).
        corpse_items: Vec<(u8, u32, u16, u16)>,
    },

    /// A player character died and became a ghost.
    ///
    /// Unlike [`MobileKilled`](Self::MobileKilled), the player mobile is
    /// **not** removed from the world.  A corpse item is created (carrying
    /// non-newbie equipment), the player's body becomes a ghost graphic, and
    /// the player stays in the world able to walk around as a spirit until
    /// resurrected.
    ///
    /// The game session translates this into:
    /// `DisplayDeathAction` (0xAF) + `ObjectInfo` corpse (0x1A) +
    /// `CorpseClothing` (0x89), and for the dying player's own session a body
    /// swap to the ghost graphic + a death status.
    PlayerDied {
        map_id: u8,
        /// Serial of the player that died.
        serial: u32,
        /// The living body graphic, used as the corpse `amount`.
        body_graphic: u16,
        /// The ghost body graphic the player now wears.
        ghost_graphic: u16,
        /// Hue of the corpse / player.
        hue: u16,
        /// Position of death.
        x: u16,
        y: u16,
        z: i8,
        /// Direction the corpse faces.
        direction: u8,
        /// Serial allocated for the corpse item.
        corpse_serial: u32,
        /// Equipment items to display on the corpse.
        /// Each: (layer_wire_value, item_serial, item_graphic, item_color).
        corpse_items: Vec<(u8, u32, u16, u16)>,
        /// Snapshot of the ghost mobile after death.  Observers redraw it with
        /// a full `DrawMobile` (0x78) so equipment (e.g. a stale mount layer)
        /// is reconciled — `UpdateMobile` (0x77) cannot do this.
        entity: Option<EntitySnapshot>,
        /// Serial of the mount item the player was wearing, if any.  The dying
        /// player's own client receives a `DeleteObject` for it so the mount
        /// is removed from their view (the mount becomes a separate NPC).
        mount_item_serial: Option<u32>,
    },

    /// A player character was resurrected from a ghost back to a living body.
    ///
    /// The game session re-renders the player with the living body graphic,
    /// updates the health bar, and plays the resurrection sound.
    PlayerResurrected {
        map_id: u8,
        /// Serial of the player that was resurrected.
        serial: u32,
        /// The restored living body graphic.
        body_graphic: u16,
        /// Hue of the player.
        hue: u16,
        /// Position.
        x: u16,
        y: u16,
        z: i8,
        /// Direction the player faces.
        direction: u8,
        /// HP restored on resurrection.
        new_hits: u16,
        /// Max HP.
        max_hits: u16,
        /// Snapshot of the living mobile after resurrection.  Observers redraw
        /// it with a full `DrawMobile` (0x78) so equipment (e.g. a stale mount
        /// layer) is reconciled — `UpdateMobile` (0x77) cannot do this.
        entity: Option<EntitySnapshot>,
    },

    /// Damage was dealt to a mobile.
    ///
    /// Used to send `UpdateHealth` (0xA1) and a damage indicator
    /// to observing clients.
    DamageDealt {
        map_id: u8,
        /// Serial of the mobile that took damage.
        serial: u32,
        /// Serial of the entity that dealt the damage (0 if unknown).
        source_serial: u32,
        /// Amount of damage dealt (after reduction/amplification).
        amount: u16,
        /// New HP after damage.
        new_hits: u16,
        /// Max HP of the target.
        max_hits: u16,
        /// Position of the target (for spatial routing).
        x: u16,
        y: u16,
    },

    /// A mobile was healed.
    ///
    /// Used to send `UpdateHealth` (0xA1) to observing clients.
    MobileHealed {
        map_id: u8,
        /// Serial of the mobile that was healed.
        serial: u32,
        /// Amount healed.
        amount: u16,
        /// New HP after healing.
        new_hits: u16,
        /// Max HP.
        max_hits: u16,
        /// Position (for spatial routing).
        x: u16,
        y: u16,
    },

    /// A mobile's mana/stamina changed (consumed by spell or regenerated).
    ///
    /// Sent to the mobile's own session to update mana/stamina bars.
    ManaStaminaChanged {
        map_id: u8,
        serial: u32,
        mana: u16,
        max_mana: u16,
        stamina: u16,
        max_stamina: u16,
        x: u16,
        y: u16,
    },

    /// A mobile's base stat (str / dex / int) changed.
    ///
    /// Carries all stats needed to build a full `StatusBarInfo` (0x11)
    /// packet, since UO has no lightweight per-stat update packet.
    BaseStatChanged {
        map_id: u8,
        serial: u32,
        str_: u16,
        dex: u16,
        int: u16,
        hits: u16,
        hits_max: u16,
        mana: u16,
        mana_max: u16,
        stamina: u16,
        stamina_max: u16,
        x: u16,
        y: u16,
    },

    /// Contents of a container changed — items were added, removed, or
    /// updated.
    ///
    /// Routed spatially using the container's world position (resolved
    /// from the root parent entity).  Each session checks whether it
    /// currently has this container open before forwarding packets to
    /// the client.
    ///
    /// The `changes` vector may contain multiple entries for compound
    /// operations (e.g. partial stack pickup = remove original + add
    /// remainder).
    ContainerContentsUpdated {
        map_id: u8,
        /// Serial of the container whose contents changed.
        container_serial: u32,
        /// World position for spatial routing (resolved from the
        /// container's root parent — ground item or mobile).
        x: u16,
        y: u16,
        /// The individual changes that occurred.
        changes: Vec<ContainerContentChange>,
    },

    // ── Targeted events (from per-object controllers to a specific player) ──

    /// Send a gump dialog to a specific player.
    ///
    /// Emitted by an object's controller (e.g. a teleport pillar) to open
    /// a gump for the player who used the object.  Routed directly to the
    /// target session by the observer registry.
    TargetedGump {
        map_id: u8,
        /// Which player should receive the gump.
        target_player: u32,
        /// Serial of the object that opened the gump (used by the session
        /// to route the gump response back to the correct controller).
        source_serial: u32,
        gump_id: u32,
        /// Gump window position on the client screen.
        gump_x: u32,
        gump_y: u32,
        /// Gump layout string (UO gump command language).
        layout: String,
        /// Text lines referenced by `text` commands in the layout.
        text_lines: Vec<String>,
        /// Position for spatial routing.
        pos_x: u16,
        pos_y: u16,
        /// When `true`, the session marks this gump as "blocking" — the
        /// player cannot cast spells or use skills until the gump is
        /// closed or answered.  Bandages remain allowed.
        blocking: bool,
    },

    /// Send a system message to a specific player.
    ///
    /// Emitted by an object's controller to display feedback text
    /// (e.g. "You are too far away.") to the player who interacted
    /// with the object.
    TargetedMessage {
        map_id: u8,
        /// Which player should receive the message.
        target_player: u32,
        message: String,
        color: u16,
        /// Position for spatial routing.
        pos_x: u16,
        pos_y: u16,
    },

    /// Close a gump for a specific player.
    TargetedCloseGump {
        map_id: u8,
        /// Which player should have the gump closed.
        target_player: u32,
        gump_id: u32,
        /// Position for spatial routing.
        pos_x: u16,
        pos_y: u16,
    },

    /// Send a target cursor to a specific player.
    ///
    /// Emitted by an object's controller to request the player to select
    /// a target.  The session translates this into S→C packet 0x6C.
    /// The cursor response arrives back via `GameCommand::TargetResponse`.
    TargetedTargetCursor {
        map_id: u8,
        /// Which player should receive the target cursor.
        target_player: u32,
        /// Cursor ID — used to correlate the response.
        cursor_id: u32,
        /// Cursor type: 0 = select object, 1 = harmful, 2 = beneficial.
        cursor_type: u8,
    },

    /// Teleport a specific player to another world (map facet).
    ///
    /// Emitted by an object's controller (e.g. a cross-world teleporter)
    /// when the destination lies on a different map than the controller's
    /// zone.  Controllers are bound to a single zone and cannot perform a
    /// worker-level cross-map transfer themselves, so they delegate the
    /// move to the target player's session, which executes the atomic
    /// transfer (`transfer_player`).
    ///
    /// Routed directly to the target session by the observer registry
    /// (by `target_player` only — no spatial routing).
    TargetedCrossWorldTeleport {
        /// Which player should be teleported.
        target_player: u32,
        /// Destination map facet (world).
        map_id: u8,
        x: u16,
        y: u16,
        z: i8,
    },

    /// A zone snapshot was restored (`.load` command).
    ///
    /// Carries the list of entities that had a `"controller"`
    /// entry in their `ItemProps.meta`.  Listeners (e.g. the controller
    /// restore task) should re-attach controllers for these entities.
    ///
    /// Also carries `logout_pending`: entities that had a `"logout_pending"`
    /// entry in their `ItemProps.meta`.  These characters were mid-logout when
    /// the snapshot was taken; listeners should immediately transfer them to
    /// the offline-storage zone so they are not left resident in the live world.
    ///
    /// Also carries `player_serials`: player characters (`is_player = true`)
    /// that were online in a live-world zone when the snapshot was taken and
    /// had no active session when the server restarted (crash-recovery).
    /// Listeners should transfer these to the storage zone immediately so they
    /// are not left as uncontrolled residents in the live world.
    /// Only populated when `crash_recovery = true` was set on `RestoreSnapshot`.
    SnapshotRestored {
        map_id: u8,
        /// `(entity_serial, controller_id)` pairs extracted from
        /// `item_props.meta["controller"]`.
        ///
        /// The `controller_id` uses the `"type:params"` format
        /// (e.g. `"wander:3"`, `"lua:travel_stone.lua"`).
        controller_metas: Vec<(u32, String)>,
        /// `(entity_serial, return_address)` pairs extracted from
        /// `item_props.meta["logout_pending"]`.
        ///
        /// `return_address` is `"world|x|y|z|dir"` — the position to restore
        /// the character to when they next log in.  The restore task should
        /// arm the logout reaper with `delay = Duration::ZERO` so the
        /// transfer to the storage zone happens immediately.
        logout_pending: Vec<(u32, String)>,
        /// `(entity_serial, return_address)` pairs for player characters that
        /// were online in a live-world zone when the snapshot was taken and
        /// had no session after restart (crash-recovery orphans).
        ///
        /// `return_address` is `"world|x|y|z|dir"`.  The restore task should
        /// arm the logout reaper with `delay = Duration::ZERO`.
        /// Only populated when `RestoreSnapshot::crash_recovery = true`.
        player_serials: Vec<(u32, String)>,
    },
}
