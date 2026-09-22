//! Login state: authentication, encryption and compression negotiation.

use super::{ClientboundPacket, ServerboundPacket, State};
use crate::buffer::{PacketReader, PacketWriter};
use crate::text::Text;
use crate::types::ProfileProperty;
use crate::Result;
use uuid::Uuid;

// ---------- serverbound ----------

pub struct LoginStart {
    pub name: String,
    pub uuid: Uuid,
}

impl ServerboundPacket for LoginStart {
    const NAME: &'static str = "hello";
    const STATE: State = State::Login;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            name: r.read_string_max(16)?,
            uuid: r.read_uuid()?,
        })
    }
}

/// The client's answer to [`EncryptionRequest`]. Both fields are RSA-encrypted
/// with the server's public key.
pub struct EncryptionResponse {
    pub shared_secret: Vec<u8>,
    pub verify_token: Vec<u8>,
}

impl ServerboundPacket for EncryptionResponse {
    const NAME: &'static str = "key";
    const STATE: State = State::Login;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            shared_secret: r.read_byte_array()?.to_vec(),
            verify_token: r.read_byte_array()?.to_vec(),
        })
    }
}

pub struct LoginAcknowledged;

impl ServerboundPacket for LoginAcknowledged {
    const NAME: &'static str = "login_acknowledged";
    const STATE: State = State::Login;
    fn read(_: &mut PacketReader) -> Result<Self> {
        Ok(Self)
    }
}

pub struct LoginPluginResponse {
    pub message_id: i32,
    pub data: Option<Vec<u8>>,
}

impl ServerboundPacket for LoginPluginResponse {
    const NAME: &'static str = "custom_query_answer";
    const STATE: State = State::Login;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let message_id = r.read_varint()?;
        let data = if r.read_bool()? {
            Some(r.remaining_bytes().to_vec())
        } else {
            None
        };
        Ok(Self { message_id, data })
    }
}

// ---------- clientbound ----------

/// Kicks the client. Unlike every later state this one carries the text as a
/// JSON string, not NBT.
pub struct LoginDisconnect {
    pub reason: Text,
}

impl ClientboundPacket for LoginDisconnect {
    const NAME: &'static str = "login_disconnect";
    const STATE: State = State::Login;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.reason.to_json().to_string());
    }
}

pub struct EncryptionRequest {
    /// Always empty on modern servers.
    pub server_id: String,
    /// DER-encoded RSA public key.
    pub public_key: Vec<u8>,
    pub verify_token: Vec<u8>,
    /// Whether the client must talk to Mojang's session server (online mode).
    pub should_authenticate: bool,
}

impl ClientboundPacket for EncryptionRequest {
    const NAME: &'static str = "hello";
    const STATE: State = State::Login;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.server_id);
        w.write_byte_array(&self.public_key);
        w.write_byte_array(&self.verify_token);
        w.write_bool(self.should_authenticate);
    }
}

pub struct LoginSuccess {
    pub uuid: Uuid,
    pub username: String,
    pub properties: Vec<ProfileProperty>,
    /// Added in 26.2. Identifies this play session; a fresh random UUID is fine.
    pub session_id: Uuid,
}

impl ClientboundPacket for LoginSuccess {
    const NAME: &'static str = "login_finished";
    const STATE: State = State::Login;
    fn write(&self, w: &mut PacketWriter) {
        w.write_uuid(&self.uuid);
        w.write_string(&self.username);
        w.write_list(&self.properties, |w, p| {
            w.write_string(&p.name);
            w.write_string(&p.value);
            w.write_option(p.signature.as_ref(), |w, s| w.write_string(s));
        });
        w.write_uuid(&self.session_id);
    }
}

pub struct SetCompression {
    pub threshold: i32,
}

impl ClientboundPacket for SetCompression {
    const NAME: &'static str = "login_compression";
    const STATE: State = State::Login;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.threshold);
    }
}

/// Asks the client (usually a proxy) a question on a custom channel. Used
/// for Velocity's modern player-info forwarding.
pub struct LoginPluginRequest {
    pub message_id: i32,
    pub channel: crate::types::Identifier,
    pub data: Vec<u8>,
}

impl ClientboundPacket for LoginPluginRequest {
    const NAME: &'static str = "custom_query";
    const STATE: State = State::Login;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.message_id);
        w.write_identifier(&self.channel);
        w.write_bytes(&self.data);
    }
}
