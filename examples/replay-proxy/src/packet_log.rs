//! Re-export from `common::packet_log`.
//!
//! The `.uolog` file format (reading, writing, scanning, filename generation)
//! now lives in `examples/common/src/packet_log.rs`.  This module re-exports
//! everything so that `crate::packet_log::LogEntry` etc. continue to resolve.

pub use common::packet_log::*;
