# demo-server

The largest and most feature-dense example in the workspace — and,
explicitly, the least finished. It exercises nearly every capability of the
`framework` crate (zones, movement/LOS validation, entity controllers,
container tracking, world-event broadcast, snapshot persistence, observer
bootstrap) on top of `protocol`/`network`. Useful as a reference for wiring
those systems together into a real game server; not a turnkey deployment.

Scale: ~35k lines across ~80 files — roughly 4× the next-largest example.

## Game systems

- **Combat & magic** — charge-based melee swing model with an aggro list;
  two-phase spellcasting (`begin_cast` → mana/animation, `complete_cast` →
  LOS re-check + effect); timed buffs from potions; HP/mana/stamina regen
  with a meditation mode.
- **Crafting & gathering** — data-driven smelting/blacksmithing tables;
  tool → resource-node gathering with depletion/regeneration (mining,
  lumberjacking, …) instead of an unbounded probability roll.
- **World & housing** — house placement/ownership/demolition, generic doors
  (any UO door graphic, decoded from its graphic-id parity), metadata-driven
  teleporters, monster spawn points with a live GM spawner item.
- **Creatures & vessels** — taming/pet ownership and control, mount/dismount,
  shrinking tamed animals into statues, ship deeds placed on water (see
  limitations below).
- **Trade & loot** — vendor buy/sell tables, banking, per-monster loot
  tables, tattered-map → treasure-map → guarded-chest digging.
- **Persistence** — full world snapshot save/restore (`.save`/`.load`
  dot-commands or `--load` at startup), per-account character persistence
  across restarts, and crash-recovery handling for characters that were
  online when the process died.

## Scripting: three Lua integration layers

The most distinctive part of this example. All gated behind the `lua`
Cargo feature (on by default) and hot-reloaded on file change:

1. **Async worker scripts** — run in their own task, talk to the world over
   an RPC-style API (`World(map_id)`, `get_entity`, `step`, `teleport`,
   `query_area`, `has_los`, `deal_damage`, `spawn_npc`, `attach_controller`,
   `send_gump`, …).
2. **Coroutine-based entity controllers** — a Lua script *is* an
   `EntityController`; each tick resumes a coroutine with synchronous world
   access via `ControlContext`, yielding with `sleep(ms)` / `wait_event(...)`.
3. **Per-session scripts** — a per-connection Lua VM that receives forwarded
   game packets/events and requests actions back (movement, target cursors,
   speech, sounds, gumps, …).

Which layer a connection uses is a **session mode** — `rust` (always
available), `lua`, or `controller` — chosen **per connection at runtime**,
not at compile time. The server-wide default is set via `--session-mode`
and can be changed live with the `.session` dot-command; already-connected
sessions keep their mode until they reconnect.

## Running

```powershell
cargo run -p demo-server -- --log logs/demo.uolog --data-dir <path-to-uo-client>
```

Notable flags: `--log <PATH>` (repeatable) or `--load <snapshot.json>`
(mutually exclusive) to choose the world source; `--spawn-points <PATH>` /
`--no-spawns`; `--cluster <N>` and `--move-throttle <MS>` for load-testing
setups; `--session-mode rust|lua|controller`; `--session-script` /
`--controller-script` / `--scripts-dir` (Lua feature); `--mirror-port`
(requires the `mirror` feature — ingests a live server's packets via an
external `mirror-proxy`, same as `path-server`).

## Known limitations

This example evolves faster than it gets finished — treat the following as
representative, not exhaustive:

- **Skills are static** — no training/gain system; values are seeded once
  at character creation.
- **Ships don't sail** — a placed ship is a static, walk-on platform on
  water, exactly like a house is on land; movement is future work.
- **No spell fizzle chance** — casting always succeeds regardless of skill.
- **House and ship catalogues are small placeholders**, not a complete set;
  house ownership is single-owner only (no co-owners/friends).
- **Loot tables are static** — a comment already earmarks them for a future
  data-file or Lua-driven design.
- Cross-world teleporters only move **players**; NPC/pet transfer through a
  teleporter is a documented no-op.
- A number of code paths are marked `#[allow(dead_code)]` or left as
  `TODO` (e.g. an entity-position lookup stub, two placeholder Lua-menu
  actions).

## Feature flags

- `lua` (default) — Lua runtime, `lua`/`controller` session modes.
- `mirror` (default) — optional `/ws/mirror` WebSocket endpoint; ingest a
  real server's live traffic into this server's world (see `mirror-proxy`).
