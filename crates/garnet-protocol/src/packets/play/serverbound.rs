//! Packets the client sends during play.

pub use super::items::{ClickKind, ContainerClick, SetCreativeModeSlot};

use crate::buffer::PacketReader;
use crate::packets::config::ClientInformation;
use crate::packets::{ServerboundPacket, State};
use crate::types::{BlockPos, Identifier};
use crate::{ProtocolError, Result};

pub struct ConfirmTeleport {
    pub teleport_id: i32,
}

impl ServerboundPacket for ConfirmTeleport {
    const NAME: &'static str = "accept_teleportation";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            teleport_id: r.read_varint()?,
        })
    }
}

/// An unsigned or signed chat line. We never verify signatures, but the
/// fields must still be read so the packet parses.
pub struct ChatMessage {
    pub message: String,
    pub timestamp: i64,
    pub salt: i64,
    pub signature: Option<Vec<u8>>,
}

impl ServerboundPacket for ChatMessage {
    const NAME: &'static str = "chat";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let message = r.read_string_max(256)?;
        let timestamp = r.read_i64()?;
        let salt = r.read_i64()?;
        let signature = r.read_option(|r| Ok(r.read_bytes(256)?.to_vec()))?;
        let _message_count = r.read_varint()?;
        let _acknowledged = r.read_bytes(3)?; // fixed 20-bit bit set
        let _checksum = r.read_u8()?;
        Ok(Self {
            message,
            timestamp,
            salt,
            signature,
        })
    }
}

/// `/command` without signed arguments.
pub struct ChatCommand {
    pub command: String,
}

impl ServerboundPacket for ChatCommand {
    const NAME: &'static str = "chat_command";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            command: r.read_string_max(32767)?,
        })
    }
}

/// `/command` with signed arguments (e.g. `/msg`). We only keep the text.
pub struct SignedChatCommand {
    pub command: String,
}

impl ServerboundPacket for SignedChatCommand {
    const NAME: &'static str = "chat_command_signed";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let command = r.read_string_max(32767)?;
        let _timestamp = r.read_i64()?;
        let _salt = r.read_i64()?;
        let _signatures = r.read_list(|r| {
            let _arg = r.read_string_max(16)?;
            let _sig = r.read_bytes(256)?;
            Ok(())
        })?;
        let _message_count = r.read_varint()?;
        let _acknowledged = r.read_bytes(3)?;
        let _checksum = r.read_u8()?;
        Ok(Self { command })
    }
}

pub struct ChatAck {
    pub message_count: i32,
}

impl ServerboundPacket for ChatAck {
    const NAME: &'static str = "chat_ack";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            message_count: r.read_varint()?,
        })
    }
}

pub struct ChunkBatchReceived {
    pub chunks_per_tick: f32,
}

impl ServerboundPacket for ChunkBatchReceived {
    const NAME: &'static str = "chunk_batch_received";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            chunks_per_tick: r.read_f32()?,
        })
    }
}

/// "Client Status": respawn, stats request, game rule request.
pub struct ClientCommand {
    pub action: i32,
}

impl ServerboundPacket for ClientCommand {
    const NAME: &'static str = "client_command";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            action: r.read_varint()?,
        })
    }
}

pub struct ClientTickEnd;

impl ServerboundPacket for ClientTickEnd {
    const NAME: &'static str = "client_tick_end";
    const STATE: State = State::Play;
    fn read(_: &mut PacketReader) -> Result<Self> {
        Ok(Self)
    }
}

pub struct PlayClientInformation(pub ClientInformation);

impl ServerboundPacket for PlayClientInformation {
    const NAME: &'static str = "client_information";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self(ClientInformation::read_body(r)?))
    }
}

pub struct CommandSuggestionRequest {
    pub transaction_id: i32,
    pub text: String,
}

impl ServerboundPacket for CommandSuggestionRequest {
    const NAME: &'static str = "command_suggestion";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            transaction_id: r.read_varint()?,
            text: r.read_string_max(32500)?,
        })
    }
}

pub struct ConfigurationAcknowledged;

impl ServerboundPacket for ConfigurationAcknowledged {
    const NAME: &'static str = "configuration_acknowledged";
    const STATE: State = State::Play;
    fn read(_: &mut PacketReader) -> Result<Self> {
        Ok(Self)
    }
}

pub struct ContainerClose {
    pub window_id: i32,
}

impl ServerboundPacket for ContainerClose {
    const NAME: &'static str = "container_close";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            window_id: r.read_varint()?,
        })
    }
}

pub struct ServerboundPluginMessage {
    pub channel: Identifier,
    pub data: Vec<u8>,
}

impl ServerboundPacket for ServerboundPluginMessage {
    const NAME: &'static str = "custom_payload";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            channel: r.read_identifier()?,
            data: r.remaining_bytes().to_vec(),
        })
    }
}

/// Right-click on an entity. 26.1 merged the old interact/attack variants:
/// attacks now use the separate `attack` packet.
pub struct Interact {
    pub entity_id: i32,
    pub hand: i32,
    pub sneaking: bool,
}

impl ServerboundPacket for Interact {
    const NAME: &'static str = "interact";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let entity_id = r.read_varint()?;
        let hand = r.read_varint()?;
        skip_lp_vec3(r)?;
        let sneaking = r.read_bool()?;
        Ok(Self {
            entity_id,
            hand,
            sneaking,
        })
    }
}

/// Left-click on an entity.
pub struct Attack {
    pub entity_id: i32,
}

impl ServerboundPacket for Attack {
    const NAME: &'static str = "attack";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            entity_id: r.read_varint()?,
        })
    }
}

pub struct KeepAlive {
    pub id: i64,
}

impl ServerboundPacket for KeepAlive {
    const NAME: &'static str = "keep_alive";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self { id: r.read_i64()? })
    }
}

/// Bit 0 on ground, bit 1 pushing against a wall.
fn read_movement_flags(r: &mut PacketReader) -> Result<(bool, bool)> {
    let flags = r.read_u8()?;
    Ok((flags & 0x01 != 0, flags & 0x02 != 0))
}

pub struct MovePlayerPos {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub on_ground: bool,
}

impl ServerboundPacket for MovePlayerPos {
    const NAME: &'static str = "move_player_pos";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let (x, y, z) = (r.read_f64()?, r.read_f64()?, r.read_f64()?);
        let (on_ground, _) = read_movement_flags(r)?;
        Ok(Self { x, y, z, on_ground })
    }
}

pub struct MovePlayerPosRot {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl ServerboundPacket for MovePlayerPosRot {
    const NAME: &'static str = "move_player_pos_rot";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let (x, y, z) = (r.read_f64()?, r.read_f64()?, r.read_f64()?);
        let (yaw, pitch) = (r.read_f32()?, r.read_f32()?);
        let (on_ground, _) = read_movement_flags(r)?;
        Ok(Self {
            x,
            y,
            z,
            yaw,
            pitch,
            on_ground,
        })
    }
}

pub struct MovePlayerRot {
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl ServerboundPacket for MovePlayerRot {
    const NAME: &'static str = "move_player_rot";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let (yaw, pitch) = (r.read_f32()?, r.read_f32()?);
        let (on_ground, _) = read_movement_flags(r)?;
        Ok(Self { yaw, pitch, on_ground })
    }
}

pub struct MovePlayerStatusOnly {
    pub on_ground: bool,
}

impl ServerboundPacket for MovePlayerStatusOnly {
    const NAME: &'static str = "move_player_status_only";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let (on_ground, _) = read_movement_flags(r)?;
        Ok(Self { on_ground })
    }
}

pub struct PingRequest {
    pub payload: i64,
}

impl ServerboundPacket for PingRequest {
    const NAME: &'static str = "ping_request";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            payload: r.read_i64()?,
        })
    }
}

pub struct Pong {
    pub id: i32,
}

impl ServerboundPacket for Pong {
    const NAME: &'static str = "pong";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self { id: r.read_i32()? })
    }
}

/// The client started or stopped flying (creative).
pub struct ServerboundPlayerAbilities {
    pub flying: bool,
}

impl ServerboundPacket for ServerboundPlayerAbilities {
    const NAME: &'static str = "player_abilities";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            flying: r.read_u8()? & 0x02 != 0,
        })
    }
}

/// Digging and a few item actions. 26.3 renumbered this enum by inserting
/// `ChangeDestroyDirection` at index 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigStatus {
    StartDigging,
    ChangeDestroyDirection,
    CancelDigging,
    FinishDigging,
    DropItemStack,
    DropItem,
    ReleaseUseItem,
    SwapItemWithOffhand,
    SpearJab,
}

pub struct PlayerAction {
    pub status: DigStatus,
    pub position: BlockPos,
    pub face: u8,
    pub sequence: i32,
}

impl ServerboundPacket for PlayerAction {
    const NAME: &'static str = "player_action";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let status = match r.read_varint()? {
            0 => DigStatus::StartDigging,
            1 => DigStatus::ChangeDestroyDirection,
            2 => DigStatus::CancelDigging,
            3 => DigStatus::FinishDigging,
            4 => DigStatus::DropItemStack,
            5 => DigStatus::DropItem,
            6 => DigStatus::ReleaseUseItem,
            7 => DigStatus::SwapItemWithOffhand,
            8 => DigStatus::SpearJab,
            other => return Err(ProtocolError::Invalid(format!("bad player action {other}"))),
        };
        Ok(Self {
            status,
            position: r.read_block_pos()?,
            face: r.read_u8()?,
            sequence: r.read_varint()?,
        })
    }
}

/// Sprinting, horse jumping, elytra. Sneaking moved to [`PlayerInput`] in 1.21.6.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerCommandAction {
    LeaveBed,
    StartSprinting,
    StopSprinting,
    StartHorseJump,
    StopHorseJump,
    OpenVehicleInventory,
    StartElytraFlying,
}

pub struct PlayerCommand {
    pub entity_id: i32,
    pub action: PlayerCommandAction,
    pub jump_boost: i32,
}

impl ServerboundPacket for PlayerCommand {
    const NAME: &'static str = "player_command";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let entity_id = r.read_varint()?;
        let action = match r.read_varint()? {
            0 => PlayerCommandAction::LeaveBed,
            1 => PlayerCommandAction::StartSprinting,
            2 => PlayerCommandAction::StopSprinting,
            3 => PlayerCommandAction::StartHorseJump,
            4 => PlayerCommandAction::StopHorseJump,
            5 => PlayerCommandAction::OpenVehicleInventory,
            6 => PlayerCommandAction::StartElytraFlying,
            other => return Err(ProtocolError::Invalid(format!("bad player command {other}"))),
        };
        Ok(Self {
            entity_id,
            action,
            jump_boost: r.read_varint()?,
        })
    }
}

/// Raw movement keys. Bits: 1 forward, 2 backward, 4 left, 8 right,
/// 16 jump, 32 sneak, 64 sprint.
pub struct PlayerInput {
    pub flags: u8,
}

impl PlayerInput {
    pub fn sneaking(&self) -> bool {
        self.flags & 0x20 != 0
    }
}

impl ServerboundPacket for PlayerInput {
    const NAME: &'static str = "player_input";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self { flags: r.read_u8()? })
    }
}

/// The client finished loading and can be shown the world.
pub struct PlayerLoaded;

impl ServerboundPacket for PlayerLoaded {
    const NAME: &'static str = "player_loaded";
    const STATE: State = State::Play;
    fn read(_: &mut PacketReader) -> Result<Self> {
        Ok(Self)
    }
}

pub struct SetCarriedItem {
    pub slot: i16,
}

impl ServerboundPacket for SetCarriedItem {
    const NAME: &'static str = "set_carried_item";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self { slot: r.read_i16()? })
    }
}

/// Arm swing. 26.3 renamed this from `swing` and dropped the hand field.
pub struct Punch;

impl ServerboundPacket for Punch {
    const NAME: &'static str = "punch";
    const STATE: State = State::Play;
    fn read(_: &mut PacketReader) -> Result<Self> {
        Ok(Self)
    }
}

pub struct UseItemOn {
    pub hand: i32,
    pub position: BlockPos,
    pub face: i32,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub cursor_z: f32,
    pub inside_block: bool,
    pub against_world_border: bool,
    pub sequence: i32,
}

impl ServerboundPacket for UseItemOn {
    const NAME: &'static str = "use_item_on";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            hand: r.read_varint()?,
            position: r.read_block_pos()?,
            face: r.read_varint()?,
            cursor_x: r.read_f32()?,
            cursor_y: r.read_f32()?,
            cursor_z: r.read_f32()?,
            inside_block: r.read_bool()?,
            against_world_border: r.read_bool()?,
            sequence: r.read_varint()?,
        })
    }
}

pub struct UseItem {
    pub hand: i32,
    pub sequence: i32,
    pub yaw: f32,
    pub pitch: f32,
}

impl ServerboundPacket for UseItem {
    const NAME: &'static str = "use_item";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        Ok(Self {
            hand: r.read_varint()?,
            sequence: r.read_varint()?,
            yaw: r.read_f32()?,
            pitch: r.read_f32()?,
        })
    }
}

/// Skips an LpVec3 (see `write_lp_vec3`) without decoding it.
fn skip_lp_vec3(r: &mut PacketReader) -> Result<()> {
    let low = r.read_u16()?;
    if low == 0 {
        return Ok(());
    }
    let _mid = r.read_i32()?;
    // Header lives in the low three bits (little-endian first byte).
    if low.to_le_bytes()[0] & 0x04 != 0 {
        let _scale_tail = r.read_varint()?;
    }
    Ok(())
}
