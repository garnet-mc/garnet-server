//! An online player: identity, live state and the queue of packets on the
//! way to their client.

use crate::anticheat::PlayerChecks;
use bytes::Bytes;
use garnet_data::GameData;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{ChunkPos, ClientboundPacket, GameProfile, Text};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

/// What the connection task pulls from the player's queue.
pub enum Outbound {
    /// A packet payload (id + body), compressed and encrypted by the connection.
    Packet(Bytes),
    /// Send a disconnect packet, flush and close.
    Disconnect(Text),
}

pub struct Player {
    pub uuid: Uuid,
    pub profile: GameProfile,
    pub entity_id: i32,
    pub ip: String,
    pub joined_at: Instant,
    pub state: Mutex<PlayerState>,
    data: Arc<GameData>,
    outbound: mpsc::UnboundedSender<Outbound>,
}

pub struct PlayerState {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub game_mode: GameMode,
    pub latency_ms: i32,
    /// The client's render distance setting, clamped to the server's.
    pub view_distance: i32,
    pub locale: String,
    /// Chunks this client currently has.
    pub loaded_chunks: HashSet<ChunkPos>,
    pub center_chunk: ChunkPos,
    /// Players whose entity this client has spawned.
    pub visible_players: HashSet<Uuid>,
    pub teleport_counter: i32,
    pub pending_teleport: Option<i32>,
    pub keepalive_sent: Instant,
    pub keepalive_id: i64,
    pub awaiting_keepalive: bool,
    pub checks: PlayerChecks,
    pub held_slot: i32,
    pub flying: bool,
    pub sneaking: bool,
    pub sprinting: bool,
    pub health: f32,
    pub food: i32,
    pub last_activity: Instant,
    /// The client sent `player_loaded`: it is rendering the world.
    pub loaded: bool,
    /// Chunk batches sent but not yet acknowledged by the client.
    pub unacked_batches: u32,
    /// How many chunks the client asked us to send per tick.
    pub chunks_per_tick: f32,
    pub dimension: String,
}

impl PlayerState {
    pub fn chunk(&self) -> ChunkPos {
        ChunkPos::from_block(self.x.floor() as i32, self.z.floor() as i32)
    }

    pub fn eye_position(&self) -> (f64, f64, f64) {
        let eye = if self.sneaking { 1.27 } else { 1.62 };
        (self.x, self.y + eye, self.z)
    }
}

impl Player {
    pub fn new(
        profile: GameProfile,
        entity_id: i32,
        ip: String,
        data: Arc<GameData>,
        outbound: mpsc::UnboundedSender<Outbound>,
        spawn: (f64, f64, f64),
        game_mode: GameMode,
        view_distance: i32,
    ) -> Self {
        let now = Instant::now();
        Self {
            uuid: profile.id,
            profile,
            entity_id,
            ip,
            joined_at: now,
            data,
            outbound,
            state: Mutex::new(PlayerState {
                x: spawn.0,
                y: spawn.1,
                z: spawn.2,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: false,
                game_mode,
                latency_ms: 0,
                view_distance,
                locale: "en_us".into(),
                loaded_chunks: HashSet::new(),
                center_chunk: ChunkPos::from_block(spawn.0 as i32, spawn.2 as i32),
                visible_players: HashSet::new(),
                teleport_counter: 0,
                pending_teleport: None,
                keepalive_sent: now,
                keepalive_id: 0,
                awaiting_keepalive: false,
                checks: PlayerChecks::default(),
                held_slot: 0,
                flying: false,
                sneaking: false,
                sprinting: false,
                health: 20.0,
                food: 20,
                last_activity: now,
                loaded: false,
                unacked_batches: 0,
                chunks_per_tick: 8.0,
                dimension: "minecraft:overworld".into(),
            }),
        }
    }

    pub fn name(&self) -> &str {
        &self.profile.name
    }

    pub fn lock(&self) -> MutexGuard<'_, PlayerState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Encodes and queues a packet. Never blocks; a closed connection just
    /// drops it.
    pub fn send<P: ClientboundPacket>(&self, packet: &P) {
        match self.data.packet_ids.encode(packet) {
            Ok(bytes) => self.send_raw(Bytes::from(bytes)),
            Err(err) => tracing::error!("cannot encode {} for {}: {err}", P::NAME, self.name()),
        }
    }

    /// Queues an already-encoded payload (used for broadcasts, which are
    /// encoded once and shared).
    pub fn send_raw(&self, payload: Bytes) {
        let _ = self.outbound.send(Outbound::Packet(payload));
    }

    pub fn disconnect(&self, reason: Text) {
        let _ = self.outbound.send(Outbound::Disconnect(reason));
    }

    pub fn is_connected(&self) -> bool {
        !self.outbound.is_closed()
    }
}
