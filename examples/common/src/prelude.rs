//! Convenience prelude for example binaries.
//!
//! ```rust,ignore
//! use common::prelude::*;
//! ```

// Logging helpers.
pub use crate::logging::init_logger;

// CLI argument groups.
pub use crate::args::{DataDirArgs, ProxyArgs, Socks5Args, VerbosityArgs};

// UO engine — common game-server types.
pub use crate::uo_engine::entity::DemoEntity;
pub use crate::uo_engine::handler::{EngineCommand, EngineHandler, MobileStepResult};
pub use crate::uo_engine::store::DemoStore;
pub use crate::uo_engine::rpc as engine_rpc;
pub use crate::uo_engine::controller::{
    GameCommand, DemoControllerDef, EntityEvent, DemoGameEvent,
};
pub use crate::uo_engine::StaticData;
