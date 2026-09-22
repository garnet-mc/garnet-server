//! Server list ping.

use super::{ClientboundPacket, ServerboundPacket, State};
use crate::buffer::{PacketReader, PacketWriter};
use crate::Result;
use serde::{Deserialize, Serialize};

/// The JSON document shown in the multiplayer server list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusJson {
    pub version: StatusVersion,
    pub players: StatusPlayers,
    pub description: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    #[serde(rename = "enforcesSecureChat")]
    pub enforces_secure_chat: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusVersion {
    pub name: String,
    pub protocol: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusPlayers {
    pub max: i32,
    pub online: i32,
    #[serde(default)]
    pub sample: Vec<StatusPlayerSample>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusPlayerSample {
    pub name: String,
    pub id: String,
}

pub struct StatusRequest;

impl ServerboundPacket for StatusRequest {
    const NAME: &'static str = "status_request";
    const STATE: State = State::Status;
    fn read(_: &mut PacketReader) -> Result<Self> {
        Ok(Self)
    }
}

pub struct PingRequest {
    pub payload: i64,
}

impl ServerboundPacket for PingRequest {
    const NAME: &'static str = "ping_request";
    const STATE: State = State::Status;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            payload: r.read_i64()?,
        })
    }
}

pub struct StatusResponse {
    pub json: String,
}

impl ClientboundPacket for StatusResponse {
    const NAME: &'static str = "status_response";
    const STATE: State = State::Status;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.json);
    }
}

pub struct PongResponse {
    pub payload: i64,
}

impl ClientboundPacket for PongResponse {
    const NAME: &'static str = "pong_response";
    const STATE: State = State::Status;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i64(self.payload);
    }
}
