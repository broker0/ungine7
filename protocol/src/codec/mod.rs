use crate::codec::packet_table::PacketLengthError;

pub mod packet_table;
pub mod packet;
pub mod encryption;



/// Unified codec-layer error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    PacketLength(PacketLengthError),
    /// Packet data size doesn't match what the table/header says
    SizeMismatch {
        packet_id: u8,
        expected: usize,
        actual: usize,
    },
    /// Sending/receiving events out of protocol order
    ProtocolViolation(&'static str),
}

impl From<PacketLengthError> for CodecError {
    fn from(e: PacketLengthError) -> Self {
        CodecError::PacketLength(e)
    }
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PacketLength(e) => write!(f, "{e}"),
            Self::SizeMismatch { packet_id, expected, actual } =>
                write!(f, "packet 0x{packet_id:02X}: expected {expected} bytes, got {actual}"),
            Self::ProtocolViolation(msg) => write!(f, "protocol violation: {msg}"),
        }
    }
}

impl std::error::Error for CodecError {}
