//! Framework error types.

use std::fmt;

use protocol::packets::login::{LoginDenied, LoginRejected};
use protocol::detection::DetectionError;
use protocol::transport::builder::TransportBuildError;
use protocol::transport::TransportError;

/// Unified error type for `network` operations.
#[derive(Debug)]
pub enum NetworkError {
    /// Protocol detection failed.
    Detection(DetectionError),
    /// Transport build error.
    TransportBuild(TransportBuildError),
    /// Transport error (relay, send/recv).
    Transport(TransportError),
    /// Connection rejected (version/encryption mismatch, handler rejection).
    Rejected(String),
    /// Network I/O error.
    Io(std::io::Error),
    /// No pending connection for the given auth key.
    NoPendingConnection(u32),
    /// No game server address in bound connection.
    NoGameServerAddress,
    /// Login denied by the login server (packet 0x82).
    LoginDenied(LoginDenied),
    /// Login rejected by the game server (packet 0x53).
    LoginRejected(LoginRejected),
    /// Server disconnected unexpectedly during a protocol sequence.
    Disconnected,
    /// Failed to parse a server packet.
    ProtocolError(String),
}

/// Convenience alias used throughout the framework.
pub type Result<T> = std::result::Result<T, NetworkError>;

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Detection(e) => write!(f, "detection failed: {e}"),
            Self::TransportBuild(e) => write!(f, "transport build error: {e}"),
            Self::Transport(e) => write!(f, "transport error: {e}"),
            Self::Rejected(reason) => write!(f, "connection rejected: {reason}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::NoPendingConnection(key) => {
                write!(f, "no pending connection for key=0x{key:08X}")
            }
            Self::NoGameServerAddress => write!(f, "no game server address"),
            Self::LoginDenied(p) => write!(f, "login denied: {}", p.reason),
            Self::LoginRejected(p) => write!(f, "game login rejected: {}", p.reason),
            Self::Disconnected => write!(f, "server disconnected unexpectedly"),
            Self::ProtocolError(msg) => write!(f, "protocol error: {msg}"),
        }
    }
}

impl std::error::Error for NetworkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Detection(e) => Some(e),
            Self::TransportBuild(e) => Some(e),
            Self::Transport(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DetectionError> for NetworkError {
    fn from(e: DetectionError) -> Self {
        Self::Detection(e)
    }
}

impl From<TransportBuildError> for NetworkError {
    fn from(e: TransportBuildError) -> Self {
        Self::TransportBuild(e)
    }
}

impl From<TransportError> for NetworkError {
    fn from(e: TransportError) -> Self {
        Self::Transport(e)
    }
}

impl From<std::io::Error> for NetworkError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
