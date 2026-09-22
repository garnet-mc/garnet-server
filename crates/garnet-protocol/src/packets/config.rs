//! Configuration state: happens between login and play. The server sends the
//! registries, tags and feature flags the client needs to render the world.

use super::{ClientboundPacket, ServerboundPacket, State};
use crate::buffer::{PacketReader, PacketWriter};
use crate::nbt::NbtTag;
use crate::text::Text;
use crate::types::Identifier;
use crate::Result;

// ---------- serverbound ----------

/// The player's client settings. Also sent in the play state when changed.
#[derive(Clone, Debug)]
pub struct ClientInformation {
    pub locale: String,
    pub view_distance: i8,
    /// 0 = enabled, 1 = commands only, 2 = hidden.
    pub chat_mode: i32,
    pub chat_colors: bool,
    pub skin_parts: u8,
    /// 0 = left, 1 = right.
    pub main_hand: i32,
    pub text_filtering: bool,
    pub allow_server_listing: bool,
    /// 0 = all, 1 = decreased, 2 = minimal.
    pub particle_status: i32,
}

impl ClientInformation {
    /// Shared by the config and play variants, which have identical bodies.
    pub fn read_body(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            locale: r.read_string_max(16)?,
            view_distance: r.read_i8()?,
            chat_mode: r.read_varint()?,
            chat_colors: r.read_bool()?,
            skin_parts: r.read_u8()?,
            main_hand: r.read_varint()?,
            text_filtering: r.read_bool()?,
            allow_server_listing: r.read_bool()?,
            particle_status: r.read_varint()?,
        })
    }
}

impl ServerboundPacket for ClientInformation {
    const NAME: &'static str = "client_information";
    const STATE: State = State::Config;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Self::read_body(r)
    }
}

pub struct ServerboundPluginMessage {
    pub channel: Identifier,
    pub data: Vec<u8>,
}

impl ServerboundPacket for ServerboundPluginMessage {
    const NAME: &'static str = "custom_payload";
    const STATE: State = State::Config;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            channel: r.read_identifier()?,
            data: r.remaining_bytes().to_vec(),
        })
    }
}

pub struct AcknowledgeFinishConfiguration;

impl ServerboundPacket for AcknowledgeFinishConfiguration {
    const NAME: &'static str = "finish_configuration";
    const STATE: State = State::Config;
    fn read(_: &mut PacketReader) -> Result<Self> {
        Ok(Self)
    }
}

pub struct ServerboundKeepAlive {
    pub id: i64,
}

impl ServerboundPacket for ServerboundKeepAlive {
    const NAME: &'static str = "keep_alive";
    const STATE: State = State::Config;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self { id: r.read_i64()? })
    }
}

/// A data pack the client has built in, so we can skip sending its contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

pub struct ServerboundKnownPacks {
    pub packs: Vec<KnownPack>,
}

impl ServerboundPacket for ServerboundKnownPacks {
    const NAME: &'static str = "select_known_packs";
    const STATE: State = State::Config;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let packs = r.read_list(|r| {
            Ok(KnownPack {
                namespace: r.read_string()?,
                id: r.read_string()?,
                version: r.read_string()?,
            })
        })?;
        Ok(Self { packs })
    }
}

pub struct ResourcePackResponse {
    pub uuid: uuid::Uuid,
    /// 0 success, 1 declined, 2 failed download, 3 accepted, 4 downloaded,
    /// 5 invalid url, 6 failed reload, 7 discarded.
    pub result: i32,
}

impl ServerboundPacket for ResourcePackResponse {
    const NAME: &'static str = "resource_pack";
    const STATE: State = State::Config;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            uuid: r.read_uuid()?,
            result: r.read_varint()?,
        })
    }
}

pub struct AcceptCodeOfConduct;

impl ServerboundPacket for AcceptCodeOfConduct {
    const NAME: &'static str = "accept_code_of_conduct";
    const STATE: State = State::Config;
    fn read(_: &mut PacketReader) -> Result<Self> {
        Ok(Self)
    }
}

// ---------- clientbound ----------

pub struct ClientboundPluginMessage {
    pub channel: Identifier,
    pub data: Vec<u8>,
}

impl ClientboundPacket for ClientboundPluginMessage {
    const NAME: &'static str = "custom_payload";
    const STATE: State = State::Config;
    fn write(&self, w: &mut PacketWriter) {
        w.write_identifier(&self.channel);
        w.write_bytes(&self.data);
    }
}

pub struct ConfigDisconnect {
    pub reason: Text,
}

impl ClientboundPacket for ConfigDisconnect {
    const NAME: &'static str = "disconnect";
    const STATE: State = State::Config;
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(&self.reason);
    }
}

pub struct FinishConfiguration;

impl ClientboundPacket for FinishConfiguration {
    const NAME: &'static str = "finish_configuration";
    const STATE: State = State::Config;
    fn write(&self, _: &mut PacketWriter) {}
}

pub struct ClientboundKeepAlive {
    pub id: i64,
}

impl ClientboundPacket for ClientboundKeepAlive {
    const NAME: &'static str = "keep_alive";
    const STATE: State = State::Config;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i64(self.id);
    }
}

/// One entry of a data-driven registry. `data` is `None` when the client
/// already has the entry from a known pack.
#[derive(Clone, Debug)]
pub struct RegistryEntry {
    pub id: Identifier,
    pub data: Option<NbtTag>,
}

pub struct RegistryData {
    pub registry: Identifier,
    pub entries: Vec<RegistryEntry>,
}

impl ClientboundPacket for RegistryData {
    const NAME: &'static str = "registry_data";
    const STATE: State = State::Config;
    fn write(&self, w: &mut PacketWriter) {
        w.write_identifier(&self.registry);
        w.write_list(&self.entries, |w, e| {
            w.write_identifier(&e.id);
            w.write_option(e.data.as_ref(), |w, nbt| w.write_nbt(nbt));
        });
    }
}

pub struct FeatureFlags {
    pub features: Vec<Identifier>,
}

impl ClientboundPacket for FeatureFlags {
    const NAME: &'static str = "update_enabled_features";
    const STATE: State = State::Config;
    fn write(&self, w: &mut PacketWriter) {
        w.write_list(&self.features, |w, f| w.write_identifier(f));
    }
}

/// All tags of one registry: tag name -> numeric ids of the members.
pub struct TagRegistry {
    pub registry: Identifier,
    pub tags: Vec<(Identifier, Vec<i32>)>,
}

pub struct UpdateTags {
    pub registries: Vec<TagRegistry>,
}

impl ClientboundPacket for UpdateTags {
    const NAME: &'static str = "update_tags";
    const STATE: State = State::Config;
    fn write(&self, w: &mut PacketWriter) {
        write_tags(w, &self.registries);
    }
}

/// Shared with the play-state variant of the packet.
pub fn write_tags(w: &mut PacketWriter, registries: &[TagRegistry]) {
    w.write_list(registries, |w, reg| {
        w.write_identifier(&reg.registry);
        w.write_list(&reg.tags, |w, (name, ids)| {
            w.write_identifier(name);
            w.write_list(ids, |w, id| w.write_varint(*id));
        });
    });
}

pub struct ClientboundKnownPacks {
    pub packs: Vec<KnownPack>,
}

impl ClientboundPacket for ClientboundKnownPacks {
    const NAME: &'static str = "select_known_packs";
    const STATE: State = State::Config;
    fn write(&self, w: &mut PacketWriter) {
        w.write_list(&self.packs, |w, p| {
            w.write_string(&p.namespace);
            w.write_string(&p.id);
            w.write_string(&p.version);
        });
    }
}

/// Shows a text the player must acknowledge before joining (26.2+).
pub struct CodeOfConduct {
    pub text: String,
}

impl ClientboundPacket for CodeOfConduct {
    const NAME: &'static str = "code_of_conduct";
    const STATE: State = State::Config;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.text);
    }
}
