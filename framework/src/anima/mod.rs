//! Entity controller system.
//!
//! A controller manages the behavior of a single entity: AI for mobs,
//! NPC scripts, item game mechanics.
//!
//! The framework provides the infrastructure (traits, dispatching,
//! scheduler), while the consumer defines the specific event and command
//! types through [`ControllerDef`] — analogous to
//! `CommandHandler { type Command }` in `continuum`.
//!
//! # Architecture
//!
//! - [`ControllerDef`] — binds the Event/GlobalEvent/Command types
//! - [`EntityController<D>`] — the main trait, implemented for each behavior type
//! - [`ControlContext`] — zone access context, passed to every call
//! - [`ControllerHost<D>`] — owns all zone controllers, routes events
//! - [`Scheduler`] — scheduler for deferred tasks (timers, repeating actions)
//! - [`AccessLevel`] — controller access level (ReadOnly / Safe / Full)

pub mod context;
pub mod error;
pub mod host;
pub mod scheduler;
pub mod traits;

pub use context::{ControlContext, EntityInfo, ZoneAccess};
pub use error::ControllerError;
pub use host::ControllerHost;
pub use scheduler::{Scheduler, TaskAction, TaskId};
pub use traits::{AccessLevel, ControllerDef, EntityController};
