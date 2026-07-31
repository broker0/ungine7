//! Movement stepping — position tracking and active movement.
//!
//! The `rythmos` module provides diorama-independent movement primitives:
//! position tracking, pending-move queues, active movement generation,
//! movement arbitration, and movement pacing.
//!
//! It does **not** depend on the `diorama` module.  Z resolution is
//! abstracted behind the [`ZResolver`] trait, which is implemented by
 //! diorama-specific types (e.g. `CompositeTileProvider`).
//!
//! # Components
//!
//! ## Position tracking
//!
//! - [`PositionTracker`] — low-level position extraction from individual
//!   position-carrying packets (`0x1B`, `0x20`, `0x77`, `0x78`).
//! - [`MovementTracker`] — standalone movement tracker (pending-move queue,
//!   `MoveAck`/`MoveReject` handling, Z resolution via [`ZResolver`]).
//!
//! ## Active movement
//!
//! - [`PendingQueue`] — generic pending-move queue with UO sequence
//!   matching (ack/reject/drain).
//! - [`ActiveMover`] — active movement queue that generates `MoveRequest`
//!   packets with its own sequence numbering.
//! - [`MoveArbiter`] — multiplexes movement from multiple clients and/or
//!   a bot through a single server connection.
//!
//! ## Pacing
//!
//! - [`MoveSpeed`] — walk / run speed tiers.
//! - [`MovePacer`] — enforces minimum delay between consecutive steps.
//!
//! ## Abstraction
//!
//! - [`ZResolver`] — trait for resolving standing Z at a tile.

pub mod active_mover;
pub mod move_arbiter;
pub mod move_pacer;
pub mod movement_tracker;
pub mod pending_queue;
pub mod position_tracker;
pub mod z_resolver;

pub use active_mover::{ActiveMover, ClientId, PendingStep, StepOrigin};
pub use move_arbiter::{ArbiterResult, ClientResponse, MoveArbiter};
pub use move_pacer::{MovePacer, MoveSpeed};
pub use movement_tracker::MovementTracker;
pub use pending_queue::{AckOutcome, PendingQueue};
pub use position_tracker::PositionTracker;
pub use z_resolver::ZResolver;
