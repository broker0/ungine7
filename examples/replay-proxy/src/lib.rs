//! `replay-proxy` library — reusable components for UO session recording
//! and playback.
//!
//! # Module overview
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`packet_log`] | `.uolog` file format: reading, writing, scanning |
//! | [`log_player`] | Stateful log replay player, snapshot-based seeking |
//! | [`framework::rythmos::PositionTracker`] | Player position derived from log packets |
//! | [`record_session`] | Transparent proxy relay that records to `.uolog` |
//! | [`replay_session`] | Full replay session orchestration (fake game server) |
//! | [`replay_handler`] | Engine command handler for the replay shadow worker |
//! | [`server_list`] | UO server-list patching / building for replay entries |
//! | [`dot_commands`] | In-world dot-command and gump-dialog state machine |
//! | [`uo_engine`] | Concrete entity/store implementations for the continuum |
//! | [`framework::diorama`] | World model: session view, visible set (terrain data in `common`) |
//!
//! Physics continuum and static world data are provided by `uo-examples-common`:
//! - `framework::continuum` — zones, movement, tile shapes
//! - `framework::ecumene::StaticWorldData` — terrain from UO data files

pub mod dot_commands;
pub mod log_player;
pub mod packet_log;
pub mod record_session;
pub mod replay_handler;
pub mod replay_session;
pub mod server_list;
pub mod uo_engine;
pub mod web;

// Re-export the continuum from common so downstream code can use
// `replay_proxy::continuum` without knowing about uo-examples-common.
pub use framework::continuum;
