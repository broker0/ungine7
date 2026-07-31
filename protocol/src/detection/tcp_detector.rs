use std::time::Duration;
use log::{debug, trace, warn};
use crate::logs;
use crate::binder::ConnectionBinder;
use tokio::net::TcpStream;
use tokio::time::timeout;
use u_core::ProtocolVersion;
use crate::detection::strategy::DetectionStrategy;
use crate::detection::version_db::VersionDatabase;
use super::*;

/// Detector configuration
#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// Timeout waiting for data from the client
    pub timeout: Duration,
    /// Interval between repeated peeks
    pub poll_interval: Duration,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(1000),
            poll_interval: Duration::from_millis(10),
        }
    }
}

/// TCP connection detector.
/// Accepts a `TcpStream`, peeks at the buffer using `peek` (without consuming data),
/// runs it through strategies, and returns the result + stream.
#[derive(Debug)]
pub struct TcpConnectionDetector {
    config: DetectorConfig,
    strategies: Vec<Box<dyn DetectionStrategy>>,
}

impl TcpConnectionDetector {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            config,
            strategies: Vec::new(),
        }
    }

    /// Builder pattern for adding strategies
    pub fn with_strategy(mut self, strategy: Box<dyn DetectionStrategy>) -> Self {
        self.strategies.push(strategy);
        self
    }

    /// Standard detector that handles both encrypted and unencrypted clients.
    pub fn standard(version_db: VersionDatabase) -> Self {
        Self::new(DetectorConfig::default())
            .with_strategy(Box::new(login::LoginDetectionStrategy::new(version_db)))
            .with_strategy(Box::new(game::GameDetectionStrategy::new()))
    }

    /// Standard detector with a binder for game-phase version resolution.
    ///
    /// When a game connection is detected via brute-force decryption, the
    /// detector will consult the binder to resolve the correct client version
    /// using the auth key from the login phase.
    pub fn standard_with_binder(version_db: VersionDatabase, binder: ConnectionBinder) -> Self {
        Self::new(DetectorConfig::default())
            .with_strategy(Box::new(login::LoginDetectionStrategy::new(version_db)))
            .with_strategy(Box::new(game::GameDetectionStrategy::new().with_binder(binder)))
    }

    /// Standard detector with a binder and an expected login-phase version.
    ///
    /// When an unencrypted client older than 6.0.5.0 connects without an
    /// extended seed, the detector uses `expected_version` instead of the
    /// default AOS (4.0.0.0) fallback. This allows the listener to correctly
    /// match the client against the configured `allowed` list.
    pub fn standard_with_version(
        version_db: VersionDatabase,
        binder: ConnectionBinder,
        expected_version: Option<ProtocolVersion>,
    ) -> Self {
        Self::new(DetectorConfig::default())
            .with_strategy(Box::new(
                login::LoginDetectionStrategy::new(version_db)
                    .with_expected_version(expected_version)
            ))
            .with_strategy(Box::new(game::GameDetectionStrategy::new().with_binder(binder)))
    }

    /// Detector that knows the client version from a previous login phase.
    /// Useful for detecting game reconnections in a proxy.
    pub fn for_game_reconnect(version: ProtocolVersion) -> Self {
        Self::new(DetectorConfig::default())
            .with_strategy(Box::new(game::GameDetectionStrategy::new().with_version(version)))
    }

    /// Minimum number of bytes required by any strategy
    fn min_required_bytes(&self) -> usize {
        self.strategies.iter()
            .map(|s| s.min_bytes())
            .min()
            .unwrap_or(0)
    }

    /// The maximum number of bytes that may be required
    fn max_required_bytes(&self) -> usize {
        self.strategies.iter()
            .map(|s| s.min_bytes())
            .max()
            .unwrap_or(0)
    }

    /// Main method — determines the connection type.
    /// Consumes a TcpStream and returns it (data is not consumed from the buffer).
    pub async fn detect(
        &self,
        socket: &TcpStream,
    ) -> Result<Protocol, DetectionError> {
        let mut current_min_bytes = self.min_required_bytes();
        let max_bytes = self.max_required_bytes();
        let mut peek_buf = vec![0u8; max_bytes.max(128) + 32];
        
        loop {
            // Wait until enough data in the socket buffer
            let peeked_size = self.wait_for_data(socket, &mut peek_buf, current_min_bytes).await?;
            let data = &peek_buf[..peeked_size];

            trace!(target: logs::DETECTION, "peeked {} bytes from socket", peeked_size);

            let mut max_needed = None;
            let mut all_failed = true;

            // Test strategies
            for strategy in &self.strategies {
                match strategy.detect(data) {
                    Ok(Some(protocol)) => {
                        debug!(target: logs::DETECTION, "'{}' matched -> {:?}", strategy.name(), protocol);
                        return Ok(protocol);
                    }
                    Ok(None) => {
                        trace!(target: logs::DETECTION, "'{}' did not match", strategy.name());
                    }
                    Err(DetectionError::InsufficientData { minimum, .. }) => {
                        trace!(target: logs::DETECTION, "detector: '{}' needs {} bytes", strategy.name(), minimum);
                        all_failed = false;
                        let curr = max_needed.unwrap_or(0);
                        if minimum > curr { max_needed = Some(minimum); }
                    }
                    Err(e) => {
                        warn!(target: logs::DETECTION, "detector: '{}' error: {}", strategy.name(), e);
                    }
                }
            }

            if let Some(needed) = max_needed {
                if needed > current_min_bytes {
                    current_min_bytes = needed;
                    if current_min_bytes > peek_buf.len() {
                        peek_buf.resize(current_min_bytes + 32, 0);
                    }
                    continue; 
                }
            }
            
            if data.len() > 0 {
                warn!(target: logs::DETECTION, "unrecognized data of {} bytes {:02X?}", data.len(), data);
            }

            if all_failed {
                return Err(DetectionError::Unrecognized);
            } else {
                // We reached max timeout & timeout returned current bytes, but it wasn't enough.
                return Err(DetectionError::InsufficientData { 
                    received: data.len(), 
                    minimum: current_min_bytes 
                });
            }
        }
    }

    /// Waits for data to accumulate in the socket buffer via peek
    async fn wait_for_data(
        &self,
        socket: &TcpStream,
        buf: &mut [u8],
        min_bytes: usize,
    ) -> Result<usize, DetectionError> {

        let result = timeout(self.config.timeout, async {
            loop {
                match socket.peek(buf).await {
                    Ok(n) if n >= min_bytes => return Ok(n),
                    Ok(n) => {
                        trace!(target: logs::DETECTION, "detector: peeked {n}/{min_bytes} bytes, waiting...");
                        tokio::time::sleep(self.config.poll_interval).await;
                    }
                    Err(e) => return Err(DetectionError::Io(e.to_string())),
                }
            }
        }).await;

        match result {
            Ok(inner) => inner,
            Err(_) => {
                // Timeout - we'll try with what we have
                match socket.peek(buf).await {
                    Ok(n) if n > 0 => {
                        debug!(target: logs::DETECTION, "detector: timeout, but have {n} bytes — trying anyway");
                        Ok(n)
                    }
                    Ok(_) => Err(DetectionError::Timeout),
                    Err(e) => Err(DetectionError::Io(e.to_string())),
                }
            }
        }
    }
}
