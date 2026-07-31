# framework

Server-side game world engine and client-side session observation for Ultima Online: zone management, entity lifecycle, controller AI, movement validation, visible-world tracking, and bootstrap packet generation.

## Key Types

- **`Zone<E, C, P = NoItemProps>`** (`continuum::zone::Zone`) — Authoritative zone for one map: entity store, spatial index, collision snapshot, item properties, and container storage. `spawn()`, `remove()`, `update()`, `get()`, `move_entity()`, `query_area()`, `test_step()`.

- **`Worker<E, C, H, P = NoItemProps>`** (`continuum::worker::Worker`) — Async worker loop: receives `WorkerCommand`s via mpsc, dispatches to `CommandHandler`, ticks zones on an adaptive schedule, and sends `WorldEvent`s through mpsc. Created via `Worker::with_factory(rx, handler, factory)`.

- **`CommandHandler<E, C, P = NoItemProps>`** (trait, `continuum::traits::CommandHandler`) — Processes commands on a zone. `handle(&mut self, zone, cmd, event_tx)` mutates the zone and publishes `WorldEvent`s. `next_tick_at()` controls adaptive ticking; `tick()` runs when due.

- **`WorkerCommand<E, Cmd>`** (`continuum::worker::WorkerCommand`) — `MapCommand(map_id, cmd)` or `GlobalCommand(cmd)`. Sent to the worker via the mpsc channel.

- **`WorldEvent`** (`continuum::world_event::WorldEvent`) — World event sent by workers through mpsc: entity lifecycle and movement, sounds/effects/animation/speech, UI, container, weather, combat, and cross-world events. Carries map and entity context where applicable; consult the enum for the full variant set.

- **`EntityStore<E>`** (trait, `continuum::traits::EntityStore`) — Pluggable entity storage (HashMap, SlotMap, etc.). `insert()`, `remove()`, `get()`, `get_mut()`, `iter()`, `clear()`.

- **`ContainerStore`** / **`HashContainerStore`** (`continuum::container`) — Container inventory tracking. `ContainerStore` is a client-side cache updated with pre-parsed opens, content, and item upserts; `HashContainerStore` is the server-side zone implementation. `ZoneContainers` abstracts zone storage; `NoContainers` is the zero-cost default.

- **`Entity`** (trait, `vessel::objects::Entity`) — Base trait for world entities: `serial()`, `pos()`, `graphic()`, `is_mobile()`, `is_multi()`, `set_pos()`, `extract_shapes()`. Implemented by consumer types.

- **`TileShape`** (`vessel::tile_shape::TileShape`) — Physical geometry of one tile element (`Slope`, `Surface`, `Background`). Factory: `TileShape::from_static(z, &def)`, `TileShape::from_land(z_base, z_stand, z_top, tile_id, def)`.

- **`StaticDataProvider`** (trait, `vessel::traits::StaticDataProvider`) — Read-only access to tiledata, map terrain, statics, and multi definitions. `land_tile_def()`, `static_tile_def()`, `statics_at()`, `multi_parts()`.

- **`StaticWorldData`** (`ecumene::static_world_data::StaticWorldData`) — Loads UO data files and implements `StaticDataProvider`. `StaticWorldData::load(data_dir: &Path) -> io::Result<Self>`.

- **`EntityRegistry<E, S>`** (`ecumene::entity_registry::EntityRegistry`) — Spatial entity cache with multi-object shape expansion and staleness tracking. `insert()`, `remove()`, `serial_in_rect()`. Import `ShapeProvider` to call its trait-provided `get_shapes_at()` method.

- **`MovementValidator`** (`ecumene::movement::MovementValidator`) — Walkability engine: `test_step(x, y, z, direction) -> Option<i8>` and `resolve_standing_z()`. Operates on any `TileProvider`.

- **`LosValidator`** (`ecumene::line_of_sight::LosValidator`) — 3D line-of-sight checker: `has_los(x1, y1, z1, x2, y2, z2) -> bool`. Traces a ray using 3D Bresenham, checking tile stacks for LOS-blocking geometry (`NO_SHOOT`, solid walls). Operates on any `TileProvider`. Builder: `.with_transparent_mask(TileFlags::FOLIAGE)`.

- **`LosRay`** (`ecumene::line_of_sight::LosRay`) — Zero-allocation 3D Bresenham ray iterator: `impl Iterator<Item = (u16, u16, i16)>`. Yields `(x, y, z)` for each tile along the ray. Builder: `.with_endpoints(true)` to include start/end tiles. Usable for LOS, projectile tracing, ray visualization.

- **`TileProvider`** (trait, `ecumene::tile_provider::TileProvider`) — `query_tile_stack(x, y, direction)` returns tile shapes at a position. `query_block(block)` returns a full 8x8 block.

- **`TileRect`** (`ecumene::tile_rect::TileRect`) — Axis-aligned rectangle in tile coordinates. `TileRect::from_view(cx, cy, range)`. `contains_pos()`, `difference()`.

- **`CollisionSnapshot`** (`ecumene::snapshot::CollisionSnapshot`) — Dynamic obstacle layer (items placed by game logic). `add_shape()`, `remove_entity_shapes()`, `get_dynamic_shapes()`.

- **`ControllerHost<D>`** (`anima::host::ControllerHost`) — Owns all entity controllers in a zone. `attach(serial, controller, map_id)`, `detach()`, `tick(zone, now)` / `tick_with_events(zone, now, event_tx)`, `send_event()`, `broadcast_event()`, `send_command()`.

- **`EntityController<D>`** (trait, `anima::traits::EntityController`) — Per-entity behaviour: `tick(ctx, dt)`, `on_event(ctx, event)`, `on_global_event(ctx, event)`, `on_command(ctx, cmd)`. Default-implemented; override what you need.

- **`ControllerDef`** (trait, `anima::traits::ControllerDef`) — Type family binding `Event`, `GlobalEvent`, `Command` for the controller system. `timer_event(serial, timer_id) -> Event`.

- **`ControlContext`** (`anima::context::ControlContext`) — Per-call context passed to controllers. Read: `me()`, `get_entity()`, `query_area()`, `test_step()`. Mutate: `step(direction)`, `teleport(x, y, z)`. Broadcast: `play_sound()`, `play_effect()`, `animate()`, `say()`. Access to `scheduler`.

- **`Scheduler`** (`anima::scheduler::Scheduler`) — Timer queue for controllers. `schedule(delay, action, map_id)`, `schedule_repeating(delay, interval, action, map_id)`, `cancel(id)`. Driven by `ControllerHost::tick()`.

- **`ObserverPipeline`** (`diorama::pipeline::ObserverPipeline`) — Unified packet-driven world observer: feeds S2C/C2S packets, tracks player position + visible entities + session state in one pass. `ingest_s2c(data)`, `ingest_c2s(data)`. Fields: `pos: PositionTracker`, `session: SessionView`.

- **`SessionView`** (`diorama::session_view::SessionView`) — Per-session state: `visible: VisibleWorld`, `registry: EntityRegistry`, `current_world`, `diff_overlay`. `ingest_packet(data)` handles all S2C packets. `update_view(cx, cy)` returns new view strips.

- **`VisibleWorld`** (`diorama::visible_world::VisibleWorld`) — Per-session visible entity set with equipment index and container cache. `get(serial)`, `iter()`, `update_view(cx, cy)`, `is_mounted(serial)`, `items_at(x, y)`.

- **`CompositeTileProvider`** (`diorama::composite_tiles::CompositeTileProvider`) — Layers visible items + multi shapes on static terrain for client-side movement validation. Implements `TileProvider` + `ZResolver`.

- **`generate_bootstrap`** (`diorama::bootstrap::generate_bootstrap`) — Reconstructs S2C login packets from `ObserverPipeline` state for late-joining clients.

- **`PositionTracker`** (`rythmos::position_tracker::PositionTracker`) — Player position from packets: `step(facing) -> bool`, `apply_character_locale()`, `apply_draw_game_player()`, `to_draw_game_player()`, `update_from_packet(data)`.

- **`MovementTracker`** (`rythmos::movement_tracker::MovementTracker`) — Standalone position + pending-move tracker: `process_c2s(data)`, `process_s2c(data, z_resolver)`. Owns `pos: PositionTracker`.

- **`ActiveMover`** (`rythmos::active_mover::ActiveMover`) — Generates `MoveRequest` packets with sequence numbering. `try_enqueue(facing, origin) -> Result<MoveRequest, StepOrigin>`, `on_ack(seq)`, `on_reject(seq)`.

- **`MoveArbiter`** (`rythmos::move_arbiter::MoveArbiter`) — Multi-client movement multiplexer through one server connection. `client_step(id, req)`, `bot_step(facing)`, `on_server_ack(ack, z_resolver)`, `on_server_reject(reject)`, `on_position_snap(dgp)`, `attach_client(id)`, `detach_client(id)`.

- **`MovePacer`** (`rythmos::move_pacer::MovePacer`) — Rate-limiter: `can_move(speed) -> bool`, `record_move()`, `time_until_ready(speed)`. `MoveSpeed` tiers: Walk (400ms), Run (200ms), MountedWalk (200ms), MountedRun (100ms).

- **`PendingQueue<T>`** (`rythmos::pending_queue::PendingQueue`) — Generic FIFO queue with UO sequence matching. `push(seq, payload)`, `on_ack(seq) -> AckOutcome`, `on_reject(seq)`.

## Usage

### Server — zone, worker, command handler, world events

```rust
use framework::continuum::{
    Zone, Worker, CommandHandler, WorkerCommand, WorldEvent,
    HashContainerStore,
};
use tokio::sync::mpsc;

// 1. Define command handler
struct MyHandler;
impl CommandHandler<MyEntity, HashContainerStore> for MyHandler {
    type Command = MyCommand;
    fn handle(&mut self, zone: &mut Zone<MyEntity, HashContainerStore>,
               cmd: MyCommand, event_tx: &mpsc::UnboundedSender<WorldEvent>) {
        match cmd {
            MyCommand::Spawn(serial, entity) => {
                zone.spawn(serial, entity);
                let _ = event_tx.send(WorldEvent::EntitySpawned { /* ... */ });
            }
            MyCommand::Move(serial, dir) => {
                zone.move_entity(serial, new_x, new_y, new_z, Some(dir));
            }
        }
    }
    fn tick(&mut self, zone: &mut Zone<MyEntity, HashContainerStore>,
             event_tx: &mpsc::UnboundedSender<WorldEvent>) {
        // periodic logic
    }
}

// 2. Create worker with zone factory
let (tx, rx) = tokio::sync::mpsc::channel(256);
let (event_tx, _event_rx) = mpsc::unbounded_channel::<WorldEvent>();
let factory = Box::new(|map_id: u8| {
    Zone::new(map_id, Some(static_data.clone()), Box::new(MyStore::new()), 896, 512)
});
let worker = Worker::with_factory_and_sender(rx, MyHandler, factory, event_tx.clone());

// 3. Pre-populate and run
// worker.zones.insert(0, zone); // optional
tokio::spawn(worker.run());

// 4. Send commands from sessions
tx.send(WorkerCommand::MapCommand(0, MyCommand::Spawn(serial, entity))).await?;
```

### Entity controllers — NPC AI, timers, scripting

```rust
use framework::anima::{
    ControllerHost, EntityController, ControllerDef,
    ControlContext, Scheduler, TaskAction,
};
use std::time::Duration;
use tokio::time::Instant;

// 1. Define type family
struct MyDef;
impl ControllerDef for MyDef {
    type Event = MyEvent;
    type GlobalEvent = MyGlobalEvent;
    type Command = MyCmd;
    fn timer_event(entity_serial: u32, timer_id: u64) -> MyEvent {
        MyEvent::TimerFired { entity_serial, timer_id }
    }
}

// 2. Implement controller
struct WanderAI;
impl EntityController<MyDef> for WanderAI {
    fn tick(&mut self, ctx: &mut ControlContext, dt: Duration) {
        // schedule a repeating wander timer
        ctx.scheduler.schedule_repeating(
            Duration::from_secs(3), Duration::from_secs(3),
            TaskAction::FireTimer { entity_serial: ctx.entity_serial, timer_id: 1 },
            Some(ctx.map_id()),
        );
    }
    fn on_event(&mut self, ctx: &mut ControlContext, event: MyEvent) {
        if let MyEvent::TimerFired { timer_id: 1, .. } = event {
            let dir = random_direction();
            let _ = ctx.step(dir); // validated movement
        }
    }
    fn name(&self) -> &str { "wander" }
}

// 3. Attach and tick
let mut host = ControllerHost::<MyDef>::new();
host.attach(npc_serial, Box::new(WanderAI), zone.map_id);
// in game loop:
host.tick_with_events(&mut zone, Instant::now(), &event_tx);
```

### Observer — passive packet-driven world tracking (proxy, replay, bot)

```rust
use framework::diorama::{ObserverPipeline, generate_bootstrap};
use framework::ecumene::StaticWorldData;
use std::sync::Arc;

// 1. Create observer with static data for Z resolution
let static_data = Arc::new(StaticWorldData::load(data_dir)?);
let mut observer = ObserverPipeline::new(Some(static_data.clone()));

// 2. Feed packets
observer.ingest_s2c(&server_packet_data);  // updates position + visible + session
observer.ingest_c2s(&client_packet_data);  // queues pending moves

// 3. Read state
let pos = &observer.pos;  // x, y, z, serial, facing
let world = observer.session.current_world;
let visible = &observer.session.visible;
let entity = visible.get(serial);
let is_mounted = visible.is_mounted(player_serial);

// 4. Generate bootstrap for a late-joining client
let packets = generate_bootstrap(&observer, Some(&*static_data), client_version);
for pkt in packets {
    transport.send(pkt.data).await?;
}
```

### Movement validation — passability checks with composite tile data

```rust
use framework::ecumene::MovementValidator;
use framework::diorama::CompositeTileProvider;
use u_core::Heading;

// Combine static terrain + visible items + multi shapes
let provider = CompositeTileProvider::new(
    &*static_data,
    current_world,
    &observer.session.visible,
    &observer.session.registry,
);
let validator = MovementValidator::new(&provider);

// Check if step is possible
if let Some(new_z) = validator.test_step(x, y, z, Heading::North) {
    // step is valid, new_z is the standing Z at the destination
}
```

### Line of sight — 3D LOS checks with any TileProvider

```rust
use framework::ecumene::{LosValidator, LosRay};
use framework::diorama::CompositeTileProvider;
use files::tiledata::TileFlags;

// Combine static terrain + visible items + multi shapes (same provider as movement)
let provider = CompositeTileProvider::new(
    &*static_data,
    current_world,
    &observer.session.visible,
    &observer.session.registry,
);

// Check line of sight (caller adds eye-height offset, typically +14 for humanoids)
let los = LosValidator::new(&provider);
if los.has_los(my_x, my_y, my_z as i16 + 14, target_x, target_y, target_z as i16 + 14) {
    // target is visible
}

// See through foliage:
let los = LosValidator::new(&provider)
    .with_transparent_mask(TileFlags::FOLIAGE);

// Server-side (zone) — uses combined static + snapshot + registry data:
if zone.has_los(x1, y1, z1 as i16 + 14, x2, y2, z2 as i16 + 14) {
    // can cast spell / shoot
}

// Low-level ray iterator — projectile tracing, visualization, etc.:
for (x, y, z) in LosRay::new(x1, y1, z1, x2, y2, z2) {
    // intermediate tiles only (start/end skipped)
}

// Include start and end tiles:
let all: Vec<_> = LosRay::new(x1, y1, z1, x2, y2, z2)
    .with_endpoints(true)
    .collect();
```

### Multi-client movement arbitration (proxy with multiple mirror clients)

```rust
use framework::rythmos::{MoveArbiter, ClientResponse, MoveSpeed, MovePacer};
use packets::traits::BasicPacket;

// 1. Create arbiter (max 4 unacked steps)
let mut arbiter = MoveArbiter::new(4);
arbiter.attach_client(client_id);

// 2. Client submits a move
let result = arbiter.client_step(client_id, &move_request);
if let Some(server_req) = result.to_server {
    send_to_server(server_req.to_bytes()).await?;
}

// 3. Server acks
let responses = arbiter.on_server_ack(&ack, Some(&z_resolver));
for (cid, response) in responses {
    match response {
        ClientResponse::Ack { their_seq, notoriety } => { /* send MoveAck */ }
        ClientResponse::Draw { draw } => { /* send DrawGamePlayer */ }
        ClientResponse::Reject { their_seq, draw } => { /* send MoveReject + DGP */ }
    }
}

// 4. Rate-limit bot movement
let mut pacer = MovePacer::new();
let speed = MoveSpeed::from_facing(facing, mounted);
if pacer.can_move(speed) {
    let result = arbiter.bot_step(facing);
    if let Some(server_req) = result.to_server {
        send_to_server(server_req.to_bytes()).await?;
        pacer.record_move();
    }
}
```

### Zone — direct entity management and spatial queries

```rust
use framework::continuum::Zone;
use framework::ecumene::TileRect;
use u_core::Heading;

// Spawn, query, move
zone.spawn(serial, entity);
let entity = zone.get(serial);
zone.move_entity(serial, new_x, new_y, new_z, Some(direction));

// Area query via spatial index
let area = TileRect::from_view(cx, cy, 18);
let nearby: Vec<E> = zone.query_area(&area);

// Passability check
if let Some(new_z) = zone.test_step(x, y, z, Heading::North) {
    // walkable
}

// Clear for world reset
zone.clear_all();
```

## Secondary API

### Zone — additional methods

```rust
impl<E: Entity, C: ZoneContainers, P: ZoneItemProps> Zone<E, C, P> {
    fn update(&mut self, id: u32, data: E)            // replace entity, rebuild collision
    fn resolve_standing_z(&self, x, y, z_hint, dir) -> Option<i8>
    fn has_los(&self, x1, y1, z1, x2, y2, z2) -> bool // 3D LOS via zone tile provider
    fn clear_all(&mut self)                            // remove all entities + collision
}
```

### Worker — constructors

```rust
impl<E, C, H, P> Worker<E, C, H, P> {
    fn new(rx, handler) -> Self                        // no factory, no external sender
    fn with_factory(rx, handler, factory) -> Self      // auto-create zones on first access
    fn with_factory_and_sender(rx, handler, factory, event_tx) -> Self
    async fn run(self)                                 // adaptive tick scheduling
    async fn run_with_tick(self, interval: Duration)
}
```

### ContainerStore — pre-parsed updates

```rust
impl ContainerStore {
    fn ingest_open(&mut self, serial: u32, gump_model: u16) -> u32
    fn ingest_content(&mut self, serial: u32, items: Vec<ContainerItem>) -> u32
    fn ingest_item_upsert(&mut self, serial: u32, item: ContainerItem) -> u32
    fn get(&self, serial: u32) -> Option<&ContainerInfo>
    fn remove(&mut self, serial: u32) -> bool
    fn iter(&self) -> impl Iterator<Item = (u32, &ContainerInfo)>
}
```

### ContainerInfo — container state

```rust
impl ContainerInfo {
    fn new(serial: u32, gump_model: u16) -> Self
    fn serial(&self) -> u32
    fn gump_model(&self) -> u16
    fn item_count(&self) -> usize
    fn set_items(&mut self, items: Vec<ContainerItem>)
    fn upsert_item(&mut self, item: ContainerItem)
    fn remove_item(&mut self, serial: u32) -> bool
    fn item_serials(&self) -> Vec<u32>
    fn find_item(&self, serial: u32) -> Option<&ContainerItem>
    fn find_item_mut(&mut self, serial: u32) -> Option<&mut ContainerItem>
}
```

### ControlContext — full method list

```rust
impl ControlContext<'_> {
    // Read
    fn me(&self) -> Option<EntityInfo>
    fn get_entity(&self, serial: u32) -> Option<EntityInfo>
    fn query_area(&self, area: &TileRect) -> Vec<EntityInfo>
    fn test_step(&self, x, y, z, dir) -> Option<i8>
    fn resolve_standing_z(&self, x, y, z_hint, dir) -> Option<i8>
    fn map_id(&self) -> u8
    // Mutate (Safe+)
    fn step(&mut self, dir) -> Result<Pos3D, ControllerError>
    fn teleport(&mut self, x, y, z) -> Result<(), ControllerError>
    fn teleport_other(&mut self, serial, x, y, z) -> Result<(), ControllerError>  // Full only
    // Broadcast
    fn play_sound(&self, sound_id, x, y, z)
    fn play_effect(&self, ...)
    fn animate(&self, serial, action, frame_count, repeat_count, ...)
    fn say(&self, serial, graphic, speech_type, color, font, name, message, x, y)
}
```

### Scheduler — timer management

```rust
impl Scheduler {
    fn schedule(&mut self, delay, action, map_id: Option<u8>) -> TaskId
    fn schedule_repeating(&mut self, delay, interval, action, map_id: Option<u8>) -> TaskId
    fn cancel(&mut self, id: TaskId)
    fn len(&self) -> usize
    fn is_empty(&self) -> bool
}

enum TaskAction {
    FireTimer { entity_serial: u32, timer_id: u64 },
    Callback(Option<Box<dyn FnOnce() + Send>>),
}
```

### PositionTracker — typed apply methods

```rust
impl PositionTracker {
    fn is_ready(&self) -> bool                         // serial != 0
    fn step(&mut self, facing: Facing) -> bool         // turn or move, returns true if tile crossed
    fn to_draw_game_player(&self) -> DrawGamePlayer
    fn apply_character_locale(&mut self, &CharacterLocaleAndBody)    // 0x1B
    fn apply_draw_game_player(&mut self, &DrawGamePlayer)            // 0x20
    fn apply_update_mobile(&mut self, &UpdateMobile) -> bool         // 0x77, serial match
    fn apply_draw_mobile(&mut self, &DrawMobile) -> bool             // 0x78, serial match
    fn update_from_packet(&mut self, data: &[u8])                    // auto-dispatch
}
```

### SessionView — view management

```rust
impl SessionView {
    fn new(cx, cy, view_range) -> Self
    fn with_static_data(cx, cy, view_range, Arc<dyn StaticDataProvider>) -> Self
    fn with_data_dir(cx, cy, view_range, sd, data_dir) -> Self
    fn ingest_packet(&mut self, data: &[u8])           // handles all S2C packet types
    fn update_view(&mut self, cx, cy) -> Vec<TileRect> // returns new view strips
    fn view_rect(&self) -> &TileRect
    fn view_range(&self) -> u16
    fn rebuild_registry(&mut self)
    fn sweep_stale(&mut self) -> usize
}
```

### VisibleWorld — queries

```rust
impl VisibleWorld {
    fn new(cx, cy, range) -> Self
    fn get(&self, serial) -> Option<&WorldEntity>
    fn iter(&self) -> impl Iterator<Item = &WorldEntity>
    fn is_mounted(&self, serial) -> bool
    fn items_at(&self, x, y) -> impl Iterator<Item = &WorldEntity>
    fn serials(&self) -> HashSet<u32>
    fn update_view(&mut self, cx, cy) -> Vec<TileRect>
    fn set_view_range(&mut self, range)
    fn clear(&mut self)
    fn containers(&self) -> &ContainerStore
    fn containers_mut(&mut self) -> &mut ContainerStore
}
```

### EntityRegistry — spatial cache

```rust
impl<E: Entity, S: SpatialIndex> EntityRegistry<E, S> {
    fn new(static_data, map_id, cache_mode) -> Self
    fn insert(&mut self, entity: &E, world: u8)
    fn remove(&mut self, serial: u32)
    fn serial_in_rect(&self, serial, &TileRect) -> bool
    fn len(&self) -> usize
    fn set_world(&mut self, world: u8)
    fn clear_world(&mut self, world: u8)
    fn arm_staleness(&mut self)
    fn should_sweep(&self) -> bool
    fn sweep_stale(&mut self, rect: &TileRect) -> usize
    fn static_data(&self) -> Option<&Arc<dyn StaticDataProvider>>
    fn cache_mode(&self) -> CacheMode
}

enum CacheMode { None, MultisOnly, ItemsAndMultis, All }
```

### TileRect — geometry

```rust
impl TileRect {
    fn from_view(cx: u16, cy: u16, range: u16) -> Self
    fn point(x: u16, y: u16) -> Self
    fn contains_pos(&self, pos: &Pos3D) -> bool
    fn difference(&self, other: &TileRect) -> Vec<TileRect>  // new strips
}
```

### MovementValidator — step & Z resolution

```rust
impl<'a, T: TileProvider> MovementValidator<'a, T> {
    fn new(provider: &'a T) -> Self
    fn test_step(&self, x, y, z, dir) -> Option<i8>
    fn resolve_standing_z(&self, x, y, z_hint, dir) -> Option<i8>
}
```

### LosValidator — 3D line-of-sight

```rust
impl<'a, T: TileProvider> LosValidator<'a, T> {
    fn new(provider: &'a T) -> Self
    fn with_transparent_mask(self, mask: u64) -> Self   // e.g. TileFlags::FOLIAGE
    fn has_los(&self, x1, y1, z1, x2, y2, z2) -> bool  // z as i16 (caller adds eye height)
}
```

### LosRay — 3D Bresenham ray iterator

```rust
impl LosRay {
    fn new(x1, y1, z1, x2, y2, z2) -> Self             // z as i16; skips endpoints by default
    fn with_endpoints(self, emit: bool) -> Self          // true = yield start + end tiles
}
// impl Iterator<Item = (u16, u16, i16)> for LosRay
```

### DiffOverlay — map/statics patching

```rust
impl DiffOverlay {
    fn new() -> Self
    fn load_and_apply(&mut self, data_dir: &Path, entries: &[(u32, u32)])
    fn is_empty(&self) -> bool
}
```

### StaticWorldData — data loading

```rust
impl StaticWorldData {
    fn load(data_dir: &Path) -> io::Result<Self>       // loads tiledata, map*, statics*, multi*
}
// Implements StaticDataProvider
```

### ActiveMover — sequence management

```rust
impl ActiveMover {
    fn new(max_pending: usize) -> Self                 // 1-4
    fn can_enqueue(&self) -> bool
    fn try_enqueue(&mut self, facing, origin) -> Result<MoveRequest, StepOrigin>
    fn on_ack(&mut self, seq) -> AckOutcome<PendingStep>
    fn on_reject(&mut self, seq) -> (Option<(u8, PendingStep)>, Vec<(u8, PendingStep)>)
    fn clear(&mut self) -> Vec<(u8, PendingStep)>
}
```

### PendingQueue — generic sequence matching

```rust
impl<T> PendingQueue<T> {
    fn push(&mut self, seq: u8, payload: T)
    fn on_ack(&mut self, server_seq: u8) -> AckOutcome<T>
    fn on_reject(&mut self, server_seq: u8) -> (Option<(u8, T)>, Vec<(u8, T)>)
    fn drain_all(&mut self) -> Vec<(u8, T)>
    fn clear(&mut self)
    fn len(&self) -> usize
}

enum AckOutcome<T> {
    Matched(T),
    Desync(Vec<(u8, T)>),
}
```

### ZResolver — trait

```rust
trait ZResolver {
    fn resolve_standing_z(&self, x, y, z_hint, direction) -> Option<i8>;
}
// Implemented by CompositeTileProvider
```

### SpatialIndex — trait

```rust
trait SpatialIndex: Clone {
    fn insert(&mut self, serial: u32, rect: TileRect);
    fn remove(&mut self, serial: u32);
    fn query_point(&self, x: u16, y: u16) -> Vec<u32>;
    fn query_rect(&self, rect: &TileRect) -> Vec<u32>;
    fn clear(&mut self);
}

// Built-in implementations
BlockSpatialIndex   // tile-position-based (point queries, used in Zone)
BBoxSpatialIndex    // bounding-box-based (multi overlap queries, used in EntityRegistry)
```

## Architecture

```
Application (server / proxy / bot)
    |
    v
Worker ← WorkerCommand (mpsc)
    |
    ├── CommandHandler::handle(zone, cmd, event_tx)
    |       |
    |       v
    |   Zone<E, C, P>
    |       ├── EntityStore<E>        ← entity CRUD
    |       ├── BlockSpatialIndex     ← area queries
    |       ├── CollisionSnapshot     ← dynamic obstacles
    |       ├── EntityRegistry        ← multi shapes
    |       └── ZoneContainers (C)    ← container inventory
    |
    ├── CommandHandler::tick(zone, event_tx)
    |       |
    |       v
    |   ControllerHost<D>::tick_with_events(zone, now, event_tx)
    |       ├── Scheduler → TaskAction::FireTimer → EntityController::on_event
    |       └── EntityController::tick(ControlContext, dt)
    |               ├── ctx.step()    → ZoneAccess::move_entity
    |               ├── ctx.query_area()
    |               └── ctx.play_sound() / say() / animate()
    |
    └── mpsc::UnboundedSender<WorldEvent>
            |
            v
        Game sessions (filter by map_id + view rect)
```

Client-side / proxy observation:

```
S→C packets ──→ ObserverPipeline::ingest_s2c()
                    |
                    ├── SessionView::ingest_packet()
                    |       ├── VisibleWorld (entity set + equipment index + containers)
                    |       └── EntityRegistry (multi shapes, spatial cache)
                    |
                    └── PositionTracker + PendingQueue
                            ├── MoveAck matching → step + Z resolution
                            └── MoveReject → position snap

C→S packets ──→ ObserverPipeline::ingest_c2s()
                    └── PendingQueue::push(seq, facing)
```

Movement validation and line of sight:

```
CompositeTileProvider (diorama)           ZoneTileProvider (continuum)
    ├── StaticTileProvider                    ├── StaticTileProvider
    │       └── StaticDataProvider            │       └── StaticDataProvider
    │           └── DiffOverlay (optional)    ├── CollisionSnapshot
    ├── VisibleWorld (items at tile)          └── EntityRegistry (multi shapes)
    └── EntityRegistry (multi shapes)
            |                                         |
            v                                         v
        MovementValidator::test_step(x, y, z, dir) -> Option<i8>
        LosValidator::has_los(x1, y1, z1, x2, y2, z2) -> bool
```
