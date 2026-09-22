//! Packets the server sends during play.
//!
//! Field layouts follow protocol 777 (Minecraft 26.3). Where 26.x changed a
//! long-standing layout, the comment says so, because those are the places
//! to look first when the next version breaks something.

use super::{write_lp_vec3, GameMode};
use crate::buffer::PacketWriter;
use crate::nbt::NbtTag;
use crate::packets::config::{write_tags, TagRegistry};
use crate::packets::{ClientboundPacket, State};
use crate::text::Text;
use crate::types::{BlockPos, GameProfile, Identifier};
use uuid::Uuid;

pub use super::clientbound_extra::*;

/// Everything the client needs to know about the dimension it is entering.
/// Shared by [`Login`] and [`Respawn`].
#[derive(Clone, Debug)]
pub struct SpawnInfo {
    /// Index into the `minecraft:dimension_type` registry we sent.
    pub dimension_type: i32,
    pub dimension_name: Identifier,
    pub hashed_seed: i64,
    pub game_mode: GameMode,
    pub previous_game_mode: Option<GameMode>,
    pub is_debug: bool,
    pub is_flat: bool,
    pub death_location: Option<(Identifier, BlockPos)>,
    pub portal_cooldown: i32,
    pub sea_level: i32,
}

impl SpawnInfo {
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.dimension_type);
        w.write_identifier(&self.dimension_name);
        w.write_i64(self.hashed_seed);
        // 26.3: both game modes are VarInts and the previous one is optional,
        // encoded as 0 for none or id + 1.
        w.write_varint(self.game_mode as i32);
        w.write_varint(self.previous_game_mode.map(|m| m as i32 + 1).unwrap_or(0));
        w.write_bool(self.is_debug);
        w.write_bool(self.is_flat);
        w.write_option(self.death_location.as_ref(), |w, (dim, pos)| {
            w.write_identifier(dim);
            w.write_block_pos(*pos);
        });
        w.write_varint(self.portal_cooldown);
        w.write_varint(self.sea_level);
    }
}

/// "Join Game". The first play packet; the client starts rendering after it.
pub struct Login {
    pub entity_id: i32,
    pub is_hardcore: bool,
    pub dimension_names: Vec<Identifier>,
    pub max_players: i32,
    pub view_distance: i32,
    pub simulation_distance: i32,
    pub reduced_debug_info: bool,
    pub enable_respawn_screen: bool,
    pub do_limited_crafting: bool,
    pub spawn: SpawnInfo,
    /// Added in 26.2.
    pub online_mode: bool,
    pub enforces_secure_chat: bool,
}

impl ClientboundPacket for Login {
    const NAME: &'static str = "login";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i32(self.entity_id);
        w.write_bool(self.is_hardcore);
        w.write_list(&self.dimension_names, |w, d| w.write_identifier(d));
        w.write_varint(self.max_players);
        w.write_varint(self.view_distance);
        w.write_varint(self.simulation_distance);
        w.write_bool(self.reduced_debug_info);
        w.write_bool(self.enable_respawn_screen);
        w.write_bool(self.do_limited_crafting);
        self.spawn.write(w);
        w.write_bool(self.online_mode);
        w.write_bool(self.enforces_secure_chat);
    }
}

pub struct Respawn {
    pub spawn: SpawnInfo,
    /// Bit 0 keeps attributes, bit 1 keeps metadata. Dimension changes keep
    /// both; a respawn after death keeps nothing.
    pub data_kept: u8,
}

impl ClientboundPacket for Respawn {
    const NAME: &'static str = "respawn";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        self.spawn.write(w);
        w.write_u8(self.data_kept);
    }
}

pub struct SetDefaultSpawnPosition {
    pub dimension: Identifier,
    pub position: BlockPos,
    pub yaw: f32,
    pub pitch: f32,
}

impl ClientboundPacket for SetDefaultSpawnPosition {
    const NAME: &'static str = "set_default_spawn_position";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_identifier(&self.dimension);
        w.write_block_pos(self.position);
        w.write_f32(self.yaw);
        w.write_f32(self.pitch);
    }
}

/// Teleports the player. The client answers with `accept_teleportation`
/// carrying the same id; movement packets before that are stale.
pub struct PlayerPosition {
    pub teleport_id: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub velocity_x: f64,
    pub velocity_y: f64,
    pub velocity_z: f64,
    pub yaw: f32,
    pub pitch: f32,
    /// Bit per field that is relative instead of absolute: 0 x, 1 y, 2 z,
    /// 3 yaw, 4 pitch, 5-7 velocity x/y/z, 8 rotate velocity.
    pub relative_flags: i32,
}

impl ClientboundPacket for PlayerPosition {
    const NAME: &'static str = "player_position";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.teleport_id);
        w.write_f64(self.x);
        w.write_f64(self.y);
        w.write_f64(self.z);
        w.write_f64(self.velocity_x);
        w.write_f64(self.velocity_y);
        w.write_f64(self.velocity_z);
        w.write_f32(self.yaw);
        w.write_f32(self.pitch);
        w.write_i32(self.relative_flags);
    }
}

/// Game state changes such as weather, game mode and the "waiting for
/// chunks" screen. See [`GameEventKind`].
pub struct GameEvent {
    pub event: GameEventKind,
    pub value: f32,
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum GameEventKind {
    NoRespawnBlock = 0,
    BeginRaining = 1,
    EndRaining = 2,
    ChangeGameMode = 3,
    WinGame = 4,
    DemoEvent = 5,
    ArrowHitPlayer = 6,
    RainLevelChange = 7,
    ThunderLevelChange = 8,
    PufferfishSting = 9,
    ElderGuardianAppearance = 10,
    EnableRespawnScreen = 11,
    LimitedCrafting = 12,
    StartWaitingForChunks = 13,
}

impl ClientboundPacket for GameEvent {
    const NAME: &'static str = "game_event";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_u8(self.event as u8);
        w.write_f32(self.value);
    }
}

pub struct SetCenterChunk {
    pub chunk_x: i32,
    pub chunk_z: i32,
}

impl ClientboundPacket for SetCenterChunk {
    const NAME: &'static str = "set_chunk_cache_center";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.chunk_x);
        w.write_varint(self.chunk_z);
    }
}

pub struct SetRenderDistance {
    pub distance: i32,
}

impl ClientboundPacket for SetRenderDistance {
    const NAME: &'static str = "set_chunk_cache_radius";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.distance);
    }
}

pub struct SetSimulationDistance {
    pub distance: i32,
}

impl ClientboundPacket for SetSimulationDistance {
    const NAME: &'static str = "set_simulation_distance";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.distance);
    }
}

pub struct ChunkBatchStart;

impl ClientboundPacket for ChunkBatchStart {
    const NAME: &'static str = "chunk_batch_start";
    const STATE: State = State::Play;
    fn write(&self, _: &mut PacketWriter) {}
}

pub struct ChunkBatchFinished {
    pub batch_size: i32,
}

impl ClientboundPacket for ChunkBatchFinished {
    const NAME: &'static str = "chunk_batch_finished";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.batch_size);
    }
}

/// A block entity inside a chunk packet.
pub struct ChunkBlockEntity {
    /// `(x & 15) << 4 | (z & 15)`.
    pub packed_xz: u8,
    pub y: i16,
    /// Index into `minecraft:block_entity_type`.
    pub type_id: i32,
    pub data: NbtTag,
}

/// Light arrays for one chunk column. Each mask has one bit per section
/// plus one below and one above the world.
#[derive(Default)]
pub struct LightData {
    pub sky_mask: Vec<u64>,
    pub block_mask: Vec<u64>,
    pub empty_sky_mask: Vec<u64>,
    pub empty_block_mask: Vec<u64>,
    /// 2048 bytes each (4 bits per block), in mask order.
    pub sky_arrays: Vec<Vec<u8>>,
    pub block_arrays: Vec<Vec<u8>>,
}

impl LightData {
    fn write(&self, w: &mut PacketWriter) {
        w.write_bitset(&self.sky_mask);
        w.write_bitset(&self.block_mask);
        w.write_bitset(&self.empty_sky_mask);
        w.write_bitset(&self.empty_block_mask);
        w.write_list(&self.sky_arrays, |w, a| w.write_byte_array(a));
        w.write_list(&self.block_arrays, |w, a| w.write_byte_array(a));
    }
}

/// A whole chunk column. `sections` is pre-encoded by `garnet-world`
/// because the format depends on the block registry.
pub struct ChunkData {
    pub chunk_x: i32,
    pub chunk_z: i32,
    /// `(type, packed longs)`; types are 1 WORLD_SURFACE, 4 MOTION_BLOCKING,
    /// 5 MOTION_BLOCKING_NO_LEAVES.
    pub heightmaps: Vec<(i32, Vec<i64>)>,
    pub sections: Vec<u8>,
    pub block_entities: Vec<ChunkBlockEntity>,
    pub light: LightData,
}

impl ClientboundPacket for ChunkData {
    const NAME: &'static str = "level_chunk_with_light";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i32(self.chunk_x);
        w.write_i32(self.chunk_z);
        w.write_list(&self.heightmaps, |w, (kind, longs)| {
            w.write_varint(*kind);
            w.write_list(longs, |w, v| w.write_i64(*v));
        });
        w.write_byte_array(&self.sections);
        w.write_list(&self.block_entities, |w, be| {
            w.write_u8(be.packed_xz);
            w.write_i16(be.y);
            w.write_varint(be.type_id);
            w.write_nbt(&be.data);
        });
        self.light.write(w);
    }
}

pub struct UnloadChunk {
    pub chunk_x: i32,
    pub chunk_z: i32,
}

impl ClientboundPacket for UnloadChunk {
    const NAME: &'static str = "forget_level_chunk";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        // Packed as z in the high 32 bits and x in the low 32.
        w.write_i64(((self.chunk_z as i64) << 32) | (self.chunk_x as u32 as i64));
    }
}

pub struct BlockUpdate {
    pub position: BlockPos,
    pub state_id: i32,
}

impl ClientboundPacket for BlockUpdate {
    const NAME: &'static str = "block_update";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_block_pos(self.position);
        w.write_varint(self.state_id);
    }
}

/// Tells the client its block-change prediction up to `sequence` is settled.
pub struct AcknowledgeBlockChange {
    pub sequence: i32,
}

impl ClientboundPacket for AcknowledgeBlockChange {
    const NAME: &'static str = "block_changed_ack";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.sequence);
    }
}

pub struct KeepAlive {
    pub id: i64,
}

impl ClientboundPacket for KeepAlive {
    const NAME: &'static str = "keep_alive";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i64(self.id);
    }
}

pub struct Disconnect {
    pub reason: Text,
}

impl ClientboundPacket for Disconnect {
    const NAME: &'static str = "disconnect";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(&self.reason);
    }
}

/// A chat line that did not come from a signed player message. Used for
/// server messages and, on this server, for all player chat.
pub struct SystemChat {
    pub content: Text,
    /// Show above the hotbar instead of in the chat window.
    pub overlay: bool,
}

impl ClientboundPacket for SystemChat {
    const NAME: &'static str = "system_chat";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(&self.content);
        w.write_bool(self.overlay);
    }
}

pub struct SetActionBarText {
    pub text: Text,
}

impl ClientboundPacket for SetActionBarText {
    const NAME: &'static str = "set_action_bar_text";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(&self.text);
    }
}

pub struct SetTitleText {
    pub text: Text,
}

impl ClientboundPacket for SetTitleText {
    const NAME: &'static str = "set_title_text";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(&self.text);
    }
}

pub struct SetSubtitleText {
    pub text: Text,
}

impl ClientboundPacket for SetSubtitleText {
    const NAME: &'static str = "set_subtitle_text";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(&self.text);
    }
}

pub struct SetTitleAnimation {
    pub fade_in: i32,
    pub stay: i32,
    pub fade_out: i32,
}

impl ClientboundPacket for SetTitleAnimation {
    const NAME: &'static str = "set_titles_animation";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i32(self.fade_in);
        w.write_i32(self.stay);
        w.write_i32(self.fade_out);
    }
}

pub struct ClearTitles {
    pub reset: bool,
}

impl ClientboundPacket for ClearTitles {
    const NAME: &'static str = "clear_titles";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_bool(self.reset);
    }
}

pub struct TabListHeaderFooter {
    pub header: Text,
    pub footer: Text,
}

impl ClientboundPacket for TabListHeaderFooter {
    const NAME: &'static str = "tab_list";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(&self.header);
        w.write_text(&self.footer);
    }
}

/// One player's tab-list entry. Only the fields whose action bit is set in
/// [`PlayerInfoUpdate::actions`] are written, in this order.
#[derive(Clone, Debug, Default)]
pub struct PlayerInfoEntry {
    pub uuid: Uuid,
    pub profile: Option<GameProfile>,
    pub game_mode: Option<GameMode>,
    pub listed: Option<bool>,
    pub latency_ms: Option<i32>,
    pub display_name: Option<Option<Text>>,
    pub list_order: Option<i32>,
    pub show_hat: Option<bool>,
}

pub mod player_info_action {
    pub const ADD_PLAYER: u8 = 0x01;
    pub const INITIALIZE_CHAT: u8 = 0x02;
    pub const UPDATE_GAME_MODE: u8 = 0x04;
    pub const UPDATE_LISTED: u8 = 0x08;
    pub const UPDATE_LATENCY: u8 = 0x10;
    pub const UPDATE_DISPLAY_NAME: u8 = 0x20;
    pub const UPDATE_LIST_ORDER: u8 = 0x40;
    pub const UPDATE_HAT: u8 = 0x80;
}

pub struct PlayerInfoUpdate {
    pub actions: u8,
    pub entries: Vec<PlayerInfoEntry>,
}

impl PlayerInfoUpdate {
    /// The full "this player joined" update.
    pub fn add(profile: GameProfile, game_mode: GameMode, latency_ms: i32) -> Self {
        use player_info_action::*;
        Self {
            actions: ADD_PLAYER | UPDATE_GAME_MODE | UPDATE_LISTED | UPDATE_LATENCY,
            entries: vec![PlayerInfoEntry {
                uuid: profile.id,
                profile: Some(profile),
                game_mode: Some(game_mode),
                listed: Some(true),
                latency_ms: Some(latency_ms),
                ..Default::default()
            }],
        }
    }
}

impl ClientboundPacket for PlayerInfoUpdate {
    const NAME: &'static str = "player_info_update";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        use player_info_action::*;
        w.write_u8(self.actions);
        w.write_list(&self.entries, |w, e| {
            w.write_uuid(&e.uuid);
            if self.actions & ADD_PLAYER != 0 {
                let profile = e.profile.clone().unwrap_or_else(|| GameProfile {
                    id: e.uuid,
                    name: String::new(),
                    properties: Vec::new(),
                });
                w.write_string(&profile.name);
                w.write_list(&profile.properties, |w, p| {
                    w.write_string(&p.name);
                    w.write_string(&p.value);
                    w.write_option(p.signature.as_ref(), |w, s| w.write_string(s));
                });
            }
            if self.actions & INITIALIZE_CHAT != 0 {
                // We never carry chat signing sessions: "no session".
                w.write_bool(false);
            }
            if self.actions & UPDATE_GAME_MODE != 0 {
                w.write_varint(e.game_mode.unwrap_or(GameMode::Survival) as i32);
            }
            if self.actions & UPDATE_LISTED != 0 {
                w.write_bool(e.listed.unwrap_or(true));
            }
            if self.actions & UPDATE_LATENCY != 0 {
                w.write_varint(e.latency_ms.unwrap_or(0));
            }
            if self.actions & UPDATE_DISPLAY_NAME != 0 {
                let name = e.display_name.as_ref().and_then(|n| n.as_ref());
                w.write_option(name, |w, t| w.write_text(t));
            }
            if self.actions & UPDATE_LIST_ORDER != 0 {
                w.write_varint(e.list_order.unwrap_or(0));
            }
            if self.actions & UPDATE_HAT != 0 {
                w.write_bool(e.show_hat.unwrap_or(true));
            }
        });
    }
}

pub struct PlayerInfoRemove {
    pub uuids: Vec<Uuid>,
}

impl ClientboundPacket for PlayerInfoRemove {
    const NAME: &'static str = "player_info_remove";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_list(&self.uuids, |w, u| w.write_uuid(u));
    }
}

pub struct SpawnEntity {
    pub entity_id: i32,
    pub uuid: Uuid,
    /// Index into `minecraft:entity_type`.
    pub entity_type: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub velocity: (f64, f64, f64),
    pub pitch: f32,
    pub yaw: f32,
    pub head_yaw: f32,
    /// Extra data whose meaning depends on the entity type; 0 for players.
    pub data: i32,
}

impl ClientboundPacket for SpawnEntity {
    const NAME: &'static str = "add_entity";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_uuid(&self.uuid);
        w.write_varint(self.entity_type);
        w.write_f64(self.x);
        w.write_f64(self.y);
        w.write_f64(self.z);
        write_lp_vec3(w, self.velocity.0, self.velocity.1, self.velocity.2);
        w.write_angle(self.pitch);
        w.write_angle(self.yaw);
        w.write_angle(self.head_yaw);
        w.write_varint(self.data);
    }
}

pub struct RemoveEntities {
    pub entity_ids: Vec<i32>,
}

impl ClientboundPacket for RemoveEntities {
    const NAME: &'static str = "remove_entities";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_list(&self.entity_ids, |w, id| w.write_varint(*id));
    }
}

/// Converts a block-space delta to the fixed-point short the move packets use.
pub fn movement_delta(from: f64, to: f64) -> i16 {
    ((to * 4096.0) - (from * 4096.0)).round().clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

/// Small movement (under 8 blocks per axis). Larger moves need
/// [`EntityPositionSync`].
pub struct MoveEntityPos {
    pub entity_id: i32,
    pub delta_x: i16,
    pub delta_y: i16,
    pub delta_z: i16,
    pub on_ground: bool,
}

impl ClientboundPacket for MoveEntityPos {
    const NAME: &'static str = "move_entity_pos";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        // 26.3 moved the on-ground flag in front of the deltas as a VarInt.
        w.write_varint(self.on_ground as i32);
        w.write_i16(self.delta_x);
        w.write_i16(self.delta_y);
        w.write_i16(self.delta_z);
    }
}

pub struct MoveEntityPosRot {
    pub entity_id: i32,
    pub delta_x: i16,
    pub delta_y: i16,
    pub delta_z: i16,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl ClientboundPacket for MoveEntityPosRot {
    const NAME: &'static str = "move_entity_pos_rot";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_varint(self.on_ground as i32);
        w.write_i16(self.delta_x);
        w.write_i16(self.delta_y);
        w.write_i16(self.delta_z);
        w.write_angle(self.yaw);
        w.write_angle(self.pitch);
    }
}

pub struct MoveEntityRot {
    pub entity_id: i32,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl ClientboundPacket for MoveEntityRot {
    const NAME: &'static str = "move_entity_rot";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_bool(self.on_ground);
        w.write_angle(self.yaw);
        w.write_angle(self.pitch);
    }
}

pub struct RotateHead {
    pub entity_id: i32,
    pub head_yaw: f32,
}

impl ClientboundPacket for RotateHead {
    const NAME: &'static str = "rotate_head";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_angle(self.head_yaw);
    }
}

/// Absolute position update; the "teleport entity" of older versions.
pub struct EntityPositionSync {
    pub entity_id: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl ClientboundPacket for EntityPositionSync {
    const NAME: &'static str = "entity_position_sync";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        // 26.3: a relative-flags VarInt precedes the position; 0 = all absolute.
        w.write_varint(0);
        w.write_f64(self.x);
        w.write_f64(self.y);
        w.write_f64(self.z);
        w.write_f32(self.yaw);
        w.write_f32(self.pitch);
        w.write_bool(self.on_ground);
    }
}

/// Arm swing of another entity (26.3 split this out of the animation packet).
pub struct SwingAnimation {
    pub entity_id: i32,
    pub off_hand: bool,
}

impl ClientboundPacket for SwingAnimation {
    const NAME: &'static str = "swing_animation";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_varint(self.off_hand as i32);
        // Swing type and duration in ticks; these are the vanilla defaults.
        w.write_varint(1);
        w.write_varint(6);
    }
}

/// Entity metadata. Each entry is `(index, type id, value bytes)` already
/// encoded by the caller; the list ends with 0xFF.
pub struct SetEntityData {
    pub entity_id: i32,
    pub entries: Vec<(u8, i32, Vec<u8>)>,
}

impl ClientboundPacket for SetEntityData {
    const NAME: &'static str = "set_entity_data";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        for (index, type_id, value) in &self.entries {
            w.write_u8(*index);
            w.write_varint(*type_id);
            w.write_bytes(value);
        }
        w.write_u8(0xFF);
    }
}

/// World time. 26.1 replaced the single day-time long with a list of clocks
/// so dimensions can tick at different rates; clock 0 is the overworld.
pub struct SetTime {
    pub world_age: i64,
    pub time_of_day: i64,
    /// `false` freezes the day/night cycle (the `doDaylightCycle` rule).
    pub advancing: bool,
}

impl ClientboundPacket for SetTime {
    const NAME: &'static str = "set_time";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i64(self.world_age);
        w.write_varint(1);
        w.write_varint(0); // clock id
        w.write_varlong(self.time_of_day);
        w.write_f32(0.0); // partial tick
        w.write_f32(if self.advancing { 1.0 } else { 0.0 }); // rate
    }
}

pub struct SetHealth {
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
}

impl ClientboundPacket for SetHealth {
    const NAME: &'static str = "set_health";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_f32(self.health);
        w.write_varint(self.food);
        w.write_f32(self.saturation);
    }
}

pub struct SetHeldSlot {
    pub slot: i32,
}

impl ClientboundPacket for SetHeldSlot {
    const NAME: &'static str = "set_held_slot";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.slot);
    }
}

pub struct PlayerAbilities {
    pub invulnerable: bool,
    pub flying: bool,
    pub allow_flying: bool,
    pub instant_break: bool,
    pub flying_speed: f32,
    pub fov_modifier: f32,
}

impl PlayerAbilities {
    pub fn for_game_mode(mode: GameMode) -> Self {
        let creative_like = matches!(mode, GameMode::Creative | GameMode::Spectator);
        Self {
            invulnerable: creative_like,
            flying: mode == GameMode::Spectator,
            allow_flying: creative_like,
            instant_break: mode == GameMode::Creative,
            flying_speed: 0.05,
            fov_modifier: 0.1,
        }
    }
}

impl ClientboundPacket for PlayerAbilities {
    const NAME: &'static str = "player_abilities";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        let mut flags = 0u8;
        if self.invulnerable {
            flags |= 0x01;
        }
        if self.flying {
            flags |= 0x02;
        }
        if self.allow_flying {
            flags |= 0x04;
        }
        if self.instant_break {
            flags |= 0x08;
        }
        w.write_u8(flags);
        w.write_f32(self.flying_speed);
        w.write_f32(self.fov_modifier);
    }
}

pub struct SetExperience {
    pub bar: f32,
    pub level: i32,
    pub total: i32,
}

impl ClientboundPacket for SetExperience {
    const NAME: &'static str = "set_experience";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_f32(self.bar);
        w.write_varint(self.level);
        w.write_varint(self.total);
    }
}

pub struct ClientboundPluginMessage {
    pub channel: Identifier,
    pub data: Vec<u8>,
}

impl ClientboundPacket for ClientboundPluginMessage {
    const NAME: &'static str = "custom_payload";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_identifier(&self.channel);
        w.write_bytes(&self.data);
    }
}

pub struct PingResponse {
    pub payload: i64,
}

impl ClientboundPacket for PingResponse {
    const NAME: &'static str = "pong_response";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i64(self.payload);
    }
}

pub struct ChangeDifficulty {
    /// 0 peaceful, 1 easy, 2 normal, 3 hard.
    pub difficulty: u8,
    pub locked: bool,
}

impl ClientboundPacket for ChangeDifficulty {
    const NAME: &'static str = "change_difficulty";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_u8(self.difficulty);
        w.write_bool(self.locked);
    }
}

/// Sends the player to another server. The client reconnects with the
/// `Transfer` intent.
pub struct Transfer {
    pub host: String,
    pub port: i32,
}

impl ClientboundPacket for Transfer {
    const NAME: &'static str = "transfer";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.host);
        w.write_varint(self.port);
    }
}

pub struct PlayUpdateTags {
    pub registries: Vec<TagRegistry>,
}

impl ClientboundPacket for PlayUpdateTags {
    const NAME: &'static str = "update_tags";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        write_tags(w, &self.registries);
    }
}

/// Sends the player back to the configuration state, for example to swap
/// resource packs. The client answers with `configuration_acknowledged`.
pub struct StartConfiguration;

impl ClientboundPacket for StartConfiguration {
    const NAME: &'static str = "start_configuration";
    const STATE: State = State::Play;
    fn write(&self, _: &mut PacketWriter) {}
}

/// The command tree used for tab completion. Nodes reference each other by
/// index; index `root` is the entry point.
pub struct Commands {
    pub nodes: Vec<CommandNode>,
    pub root: i32,
}

pub struct CommandNode {
    pub kind: CommandNodeKind,
    pub executable: bool,
    pub children: Vec<i32>,
    pub redirect: Option<i32>,
}

pub enum CommandNodeKind {
    Root,
    Literal(String),
    Argument {
        name: String,
        /// Index into the parser registry (`minecraft:command_argument_type`).
        parser_id: i32,
        /// Parser-specific properties, already encoded.
        properties: Vec<u8>,
        suggestions: Option<Identifier>,
    },
}

impl ClientboundPacket for Commands {
    const NAME: &'static str = "commands";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_list(&self.nodes, |w, node| {
            let mut flags = match &node.kind {
                CommandNodeKind::Root => 0,
                CommandNodeKind::Literal(_) => 1,
                CommandNodeKind::Argument { .. } => 2,
            };
            if node.executable {
                flags |= 0x04;
            }
            if node.redirect.is_some() {
                flags |= 0x08;
            }
            if let CommandNodeKind::Argument { suggestions: Some(_), .. } = &node.kind {
                flags |= 0x10;
            }
            w.write_u8(flags);
            w.write_list(&node.children, |w, c| w.write_varint(*c));
            if let Some(redirect) = node.redirect {
                w.write_varint(redirect);
            }
            match &node.kind {
                CommandNodeKind::Root => {}
                CommandNodeKind::Literal(name) => w.write_string(name),
                CommandNodeKind::Argument {
                    name,
                    parser_id,
                    properties,
                    suggestions,
                } => {
                    w.write_string(name);
                    w.write_varint(*parser_id);
                    w.write_bytes(properties);
                    if let Some(s) = suggestions {
                        w.write_identifier(s);
                    }
                }
            }
        });
        w.write_varint(self.root);
    }
}

/// Tab-completion results for a `command_suggestion` request.
pub struct CommandSuggestions {
    pub transaction_id: i32,
    pub start: i32,
    pub length: i32,
    pub matches: Vec<(String, Option<Text>)>,
}

impl ClientboundPacket for CommandSuggestions {
    const NAME: &'static str = "command_suggestions";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.transaction_id);
        w.write_varint(self.start);
        w.write_varint(self.length);
        w.write_list(&self.matches, |w, (text, tooltip)| {
            w.write_string(text);
            w.write_option(tooltip.as_ref(), |w, t| w.write_text(t));
        });
    }
}

/// Entity status byte, e.g. 24-28 set the op permission level shown in F3.
pub struct EntityEvent {
    pub entity_id: i32,
    pub status: u8,
}

impl ClientboundPacket for EntityEvent {
    const NAME: &'static str = "entity_event";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_i32(self.entity_id);
        w.write_u8(self.status);
    }
}
