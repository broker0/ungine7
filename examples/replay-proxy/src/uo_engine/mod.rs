//! Re-export from `common::uo_engine`.
//!
//! The concrete entity model, store, and static-data wrapper now live in
//! `examples/common/src/uo_engine/`.  This module re-exports everything
//! so that `crate::uo_engine::entity::Entity` etc. continue to resolve.

pub use common::uo_engine::*;
