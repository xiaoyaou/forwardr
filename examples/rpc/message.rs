use std::sync::atomic::AtomicU32;

use bytes::Bytes;

use crate::error::Error;

static MESSAGE_ID: AtomicU32 = AtomicU32::new(1);

pub fn get_mid() -> u32 {
    MESSAGE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub enum MessageType {
    ClientRequest,
    ClientResponse,
    ServerPush,
    HeartbeatRequest,
    HeartbeatResponse,
    RpcRequest,
    RpcResponse,
}

impl TryFrom<u8> for MessageType {
    type Error = Error;

    fn try_from(value: u8) -> std::result::Result<Self, Self::Error> {
        let mt = match value {
            0 => MessageType::ClientRequest,
            1 => MessageType::ClientResponse,
            2 => MessageType::ServerPush,
            3 => MessageType::HeartbeatRequest,
            4 => MessageType::HeartbeatResponse,
            5 => MessageType::RpcRequest,
            6 => MessageType::RpcResponse,
            _ => {
                return Err(Error::new(format!("bad message type {value} found")));
            }
        };
        Ok(mt)
    }
}

pub struct Message {
    pub id: u32,
    pub message_type: MessageType,
    pub bytes: Bytes,
}

pub struct Request {
    pub name: String,
    pub function: String,
    pub data: Bytes,
}

pub enum Response {
    Success(Bytes),
    Error(String),
}

pub struct Forward {
    pub index: u8,
    pub args: Bytes,
}
