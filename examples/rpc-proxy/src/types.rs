use serde::{Deserialize, Serialize};
use bytes::Bytes;
use u_core::PacketDirection;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SessionId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct WsClientId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ClientRole {
    Source,
    Mirror,
}

#[derive(Debug, Clone)]
pub struct PacketFrame {
    pub data: Bytes,
    pub direction: PacketDirection,
}

#[derive(Debug, Clone, Serialize)]
pub struct FullSessionState {
    // Will be populated from diorama::SessionView and GameState
    pub character: Option<String>,
    pub position: (u16, u16, i8),
    pub world: u8,
    // TODO: add serialized SessionView
}