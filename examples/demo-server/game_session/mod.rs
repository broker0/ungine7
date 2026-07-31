//! Per-client game session: game loop, UO packet handling, view streaming.
//!
//! Extracted from `main.rs` to isolate all game-phase logic (movement,
//! spawning, NPC tick, view streaming) into a self-contained module.
//!
//! Event routing is handled by the
//! [`ObserverRegistry`](framework::continuum::ObserverRegistry) on the worker
//! side — the session receives only events relevant to its view
//! rectangle via a dedicated `mpsc` channel.
//!
//! ## Session modes
//!
//! Game logic can run in one of three modes, selected **per-session at
//! runtime** (see [`SessionMode`]).  The mode for a new session is taken
//! from the server's current default; an administrator can change that
//! default at runtime with the `.session` dot-command.
//!
//! * **`rust`** — all combat, magic, skills, regen, bandaging, mounting and
//!   interaction logic is implemented in Rust sub-modules.  Always available.
//!
//! * **`lua`** — game logic lives in Lua scripts; Rust only handles
//!   infrastructure (movement, items, containers, spawn, auth) and forwards
//!   game-relevant packets/events to a per-session Lua VM.  Requires the
//!   `lua` feature.
//!
//! * **`controller`** — game logic is driven by a Lua controller running in
//!   the worker tick.  Requires the `lua` feature.
//!
//! All three handler implementations share the [`game_logic::GameLogicHandler`]
//! trait; the unified [`session_loop`] picks one as a `Box<dyn …>` based on
//! the resolved mode.

// ── Session mode (runtime selection) ──────────────────────────────────────
mod session_mode;
pub(crate) use session_mode::SessionMode;

// ── Packet parsing (both modes) ──────────────────────────────────────────
mod parsed_packet;

// ── Shared infrastructure (both modes) ───────────────────────────────────
mod infra;

// ── Game-logic handler trait (both modes) ────────────────────────────────
mod game_logic;

// ── Unified pending cursor (both modes) ──────────────────────────────────
mod pending_cursor;

// ── Infrastructure modules (both modes) ──────────────────────────────────
mod dot_commands;
mod items;
mod containers;
mod movement;
mod spawn;
pub(crate) mod transfer;
mod util;
mod world_events;

// ── Player housing (placement, ownership, demolition) — rust mode ──────────
mod housing;

// ── Player ships (placement on water, re-deed, sailing) — rust mode ────────
pub(crate) mod shipping;

// ── DEV: starter items on login ───────────────────────────────────────────
mod dev_items;

// ── Session state container (rust mode) ──────────────────────────────────
mod session_state;

// ── Rust game-logic modules (rust mode) ──────────────────────────────────
mod bandage;
mod gather;
mod crafting;
mod mount;
mod potions;
pub(crate) mod poison;
mod scrolls;
pub(crate) mod recall;
mod spellbook;
mod spells;
mod shrink;
mod treasure;
mod rust_handler;

// ── Interaction module (handles containers, paperdoll, status) ────────────
mod interaction;

// ── Vendor session module (buy/sell windows + transactions) ──────────────
mod vendor_session;

// ── Bank session module (bank box open/close) ───────────────────────────
mod bank_session;

// ── Lua dot-command (standalone lua scripts, not session lua) ────────────
#[cfg(feature = "lua")]
mod lua_commands;

// ── Per-session Lua handlers (lua mode — requires `lua`) ──────────────────
#[cfg(feature = "lua")]
mod lua_handlers;
#[cfg(feature = "lua")]
mod lua_handler;

// ── Controller-session handler (requires `lua`) ───────────────────────────
#[cfg(feature = "lua")]
mod controller_handler;

/// Authoritative player state, using the shared `PlayerState` from common.
pub(super) type PlayerState = common::world_events::PlayerState<()>;

// ── Configuration ─────────────────────────────────────────────────────────

/// Set to `true` to enable observer cross-validation of player position.
const CROSS_VALIDATE: bool = false;

// ── Public entry point ────────────────────────────────────────────────────

// ── Unified session loop (both modes) ────────────────────────────────────
mod session_loop;
pub(crate) use session_loop::run_game_session;
