//! Core packet traits and helpers — re-exported from `io`.

pub use u_io::{encode_packet, packet_reader, packet_writer, ManualPacket, PacketError, BasicPacket};
pub use u_io::packet::PacketSize;
