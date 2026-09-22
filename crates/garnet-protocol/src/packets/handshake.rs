//! The very first packet of every connection.

use super::{ServerboundPacket, State};
use crate::buffer::PacketReader;
use crate::{ProtocolError, Result};

/// What the client wants to do after the handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Status,
    Login,
    /// The client was sent here by another server with a Transfer packet.
    Transfer,
}

#[derive(Clone, Debug)]
pub struct Handshake {
    pub protocol_version: i32,
    pub server_address: String,
    pub server_port: u16,
    pub intent: Intent,
}

impl ServerboundPacket for Handshake {
    const NAME: &'static str = "intention";
    const STATE: State = State::Handshake;

    fn read(r: &mut PacketReader) -> Result<Self> {
        let protocol_version = r.read_varint()?;
        let server_address = r.read_string_max(255)?;
        let server_port = r.read_u16()?;
        let intent = match r.read_varint()? {
            1 => Intent::Status,
            2 => Intent::Login,
            3 => Intent::Transfer,
            other => return Err(ProtocolError::Invalid(format!("bad handshake intent {other}"))),
        };
        Ok(Self {
            protocol_version,
            server_address,
            server_port,
            intent,
        })
    }
}
