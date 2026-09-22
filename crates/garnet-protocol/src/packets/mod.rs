//! Packet definitions, one module per connection state.
//!
//! Packet *ids* are not hard-coded. Mojang's data generator emits a
//! `packets.json` report that maps every packet name to its id for that
//! version, and the server loads it at startup (see `garnet-data`). Each
//! packet struct only knows its vanilla *name* (for example `login_finished`),
//! and [`PacketIds`] turns that into the numeric id. That way a version bump
//! never means hunting through the code for shifted ids.

pub mod config;
pub mod handshake;
pub mod login;
pub mod play;
pub mod status;

use crate::buffer::{PacketReader, PacketWriter};
use crate::{ProtocolError, Result};
use std::collections::HashMap;

/// The connection states, in the order a client moves through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Handshake,
    Status,
    Login,
    Config,
    Play,
}

impl State {
    /// The key used for this state in `packets.json`.
    pub fn report_name(self) -> &'static str {
        match self {
            State::Handshake => "handshake",
            State::Status => "status",
            State::Login => "login",
            State::Config => "configuration",
            State::Play => "play",
        }
    }

    fn from_report_name(name: &str) -> Option<Self> {
        Some(match name {
            "handshake" => State::Handshake,
            "status" => State::Status,
            "login" => State::Login,
            "configuration" | "config" => State::Config,
            "play" => State::Play,
            _ => return None,
        })
    }
}

/// A packet the server sends.
pub trait ClientboundPacket {
    /// The vanilla name from `packets.json`, without the `minecraft:` prefix.
    const NAME: &'static str;
    const STATE: State;
    fn write(&self, w: &mut PacketWriter);
}

/// A packet the server receives.
pub trait ServerboundPacket: Sized {
    const NAME: &'static str;
    const STATE: State;
    fn read(r: &mut PacketReader) -> Result<Self>;
}

/// Name <-> id lookup tables for one protocol version.
#[derive(Debug, Clone, Default)]
pub struct PacketIds {
    pub protocol_version: i32,
    clientbound: HashMap<(State, String), i32>,
    serverbound: HashMap<(State, i32), String>,
}

impl PacketIds {
    /// Parses Mojang's `reports/packets.json`. The layout is
    /// `{ "<state>": { "clientbound": { "minecraft:<name>": { "protocol_id": N } } } }`.
    pub fn from_report(json: &serde_json::Value, protocol_version: i32) -> Result<Self> {
        let mut ids = PacketIds {
            protocol_version,
            ..Default::default()
        };
        let states = json
            .as_object()
            .ok_or_else(|| ProtocolError::Invalid("packets.json root is not an object".into()))?;
        for (state_name, directions) in states {
            let Some(state) = State::from_report_name(state_name) else {
                continue;
            };
            let Some(directions) = directions.as_object() else {
                continue;
            };
            for (direction, packets) in directions {
                let Some(packets) = packets.as_object() else {
                    continue;
                };
                for (full_name, info) in packets {
                    let name = full_name.strip_prefix("minecraft:").unwrap_or(full_name).to_owned();
                    let id = info
                        .get("protocol_id")
                        .and_then(|v| v.as_i64())
                        .ok_or_else(|| ProtocolError::Invalid(format!("packet {full_name} has no protocol_id")))?
                        as i32;
                    match direction.as_str() {
                        "clientbound" => {
                            ids.clientbound.insert((state, name), id);
                        }
                        "serverbound" => {
                            ids.serverbound.insert((state, id), name);
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(ids)
    }

    pub fn clientbound_id(&self, state: State, name: &str) -> Option<i32> {
        self.clientbound.get(&(state, name.to_owned())).copied()
    }

    pub fn serverbound_name(&self, state: State, id: i32) -> Option<&str> {
        self.serverbound.get(&(state, id)).map(String::as_str)
    }

    /// Serialises a packet to `id + body`, ready for [`crate::Codec::encode`].
    pub fn encode<P: ClientboundPacket>(&self, packet: &P) -> Result<Vec<u8>> {
        let id = self
            .clientbound_id(P::STATE, P::NAME)
            .ok_or_else(|| ProtocolError::Invalid(format!("no id for clientbound {:?} {}", P::STATE, P::NAME)))?;
        let mut w = PacketWriter::with_capacity(64);
        w.write_varint(id);
        packet.write(&mut w);
        Ok(w.into_inner())
    }

    /// Reads a serverbound packet id and returns its name plus a reader over
    /// the body. The caller matches on the name and calls `T::read`.
    pub fn decode<'a>(&self, state: State, payload: &'a [u8]) -> Result<(&str, PacketReader<'a>)> {
        let mut r = PacketReader::new(payload);
        let id = r.read_varint()?;
        let name = self
            .serverbound_name(state, id)
            .ok_or(ProtocolError::UnknownPacket { state, id })?;
        Ok((name, r))
    }
}

/// Reads a packet body, ensuring nothing is left over.
pub fn read_exact<P: ServerboundPacket>(r: &mut PacketReader) -> Result<P> {
    let packet = P::read(r)?;
    if !r.is_empty() {
        return Err(ProtocolError::Invalid(format!(
            "{} bytes left after reading {}",
            r.remaining(),
            P::NAME
        )));
    }
    Ok(packet)
}
