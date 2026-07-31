use super::*;

/// Detection strategy: analyzes raw bytes and attempts to recognize the protocol.
/// Pure function, no I/O, no async. Can be used in tests,
/// in offline analysis, in wasm.
pub trait DetectionStrategy: Send + Sync + std::fmt::Debug {
    /// Strategy name (for logging)
    fn name(&self) -> &str;

    /// Minimum number of bytes required for a detection attempt
    fn min_bytes(&self) -> usize;

    /// Attempt to detect the protocol from the buffer.
    /// - `Ok(Some(...))` — protocol determined
    /// - `Ok(None)` — data does not match this strategy, but there is no error
    /// - `Err(...)` — fatal error
    fn detect(&self, buf: &[u8]) -> Result<Option<Protocol>, DetectionError>;
}
