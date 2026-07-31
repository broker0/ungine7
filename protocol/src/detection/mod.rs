// detection/mod.rs

pub mod tcp_detector;
pub mod version_db;
pub mod strategy;
pub mod login;
pub mod game;

use crate::protocol::{Protocol};

/// Detection errors
#[derive(Debug, Clone)]
pub enum DetectionError {
    /// Not enough data to determine the connection type
    InsufficientData { received: usize, minimum: usize },
    /// Connection type not recognized
    Unrecognized,
    /// Data timeout
    Timeout,
    /// I/O error
    Io(String),
}

impl std::fmt::Display for DetectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientData { received, minimum } =>
                write!(f, "insufficient data: got {received}, need at least {minimum}"),
            Self::Unrecognized =>
                write!(f, "connection type not recognized"),
            Self::Timeout =>
                write!(f, "detection timed out"),
            Self::Io(e) =>
                write!(f, "I/O error during detection: {e}"),
        }
    }
}

impl std::error::Error for DetectionError {}
