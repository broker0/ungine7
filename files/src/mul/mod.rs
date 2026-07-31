//! Low-level MUL/IDX primitives.
//!
//! - [`IndexEntry`] — a single record from an `.idx` file
//! - [`MulIndex`] — parsed `.idx` file (array of [`IndexEntry`])

mod index;

pub use index::{IndexEntry, MulIndex, INVALID_OFFSET};
