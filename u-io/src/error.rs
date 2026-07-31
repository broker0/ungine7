//! Unified error type for binary (de)serialization.

/// Error returned by [`Decode`](crate::Decode) implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Not enough bytes remaining in the buffer / unexpected EOF on a stream.
    Truncated,
    /// A string field contained invalid data.
    InvalidString,
    /// A constant/sentinel field had an unexpected value.
    BadConstant,
    /// An I/O error occurred while reading from a stream.
    ///
    /// Stored as a string because `io::Error` is not `Clone + PartialEq`.
    Io(String),
    /// Catch-all with a human-readable message.
    Other(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "buffer truncated"),
            DecodeError::InvalidString => write!(f, "invalid string data"),
            DecodeError::BadConstant => write!(f, "unexpected constant value"),
            DecodeError::Io(msg) => write!(f, "I/O error: {msg}"),
            DecodeError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            DecodeError::Truncated
        } else {
            DecodeError::Io(e.to_string())
        }
    }
}

impl From<DecodeError> for std::io::Error {
    fn from(e: DecodeError) -> Self {
        match e {
            DecodeError::Truncated => {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, e)
            }
            _ => std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        }
    }
}
