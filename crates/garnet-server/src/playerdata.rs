//! Per-player save files in `world/playerdata/<uuid>.dat` (gzip NBT), using
//! the vanilla keys for position, rotation, game mode, health and food so
//! the files stay readable by other tools. Written atomically.

use crate::player::Player;
use crate::server::Server;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use garnet_protocol::nbt::{NbtCompound, NbtTag};
use garnet_protocol::packets::play::GameMode;
use std::io::{Read, Write};
use std::path::PathBuf;
use uuid::Uuid;

/// What we restore when a player comes back.
pub struct SavedPlayer {
    pub position: (f64, f64, f64),
    pub yaw: f32,
    pub pitch: f32,
    pub game_mode: Option<GameMode>,
    pub health: f32,
    pub food: i32,
}

fn path_for(server: &Server, uuid: Uuid) -> PathBuf {
    server
        .root
        .join(&server.config().world.name)
        .join("playerdata")
        .join(format!("{uuid}.dat"))
}

pub fn load(server: &Server, uuid: Uuid) -> Option<SavedPlayer> {
    let path = path_for(server, uuid);
    let mut raw = Vec::new();
    GzDecoder::new(std::fs::File::open(&path).ok()?).read_to_end(&mut raw).ok()?;
    let (_, root, _) = NbtTag::read_named(&raw).ok()?;
    let root = root.as_compound()?;
    let pos = root.get_list("Pos")?;
    let rot = root.get_list("Rotation").unwrap_or(&[]);
    let num = |t: Option<&NbtTag>| t.and_then(NbtTag::as_f64).unwrap_or(0.0);
    Some(SavedPlayer {
        position: (num(pos.first()), num(pos.get(1)), num(pos.get(2))),
        yaw: num(rot.first()) as f32,
        pitch: num(rot.get(1)) as f32,
        game_mode: root.get_i32("playerGameType").and_then(GameMode::from_id),
        health: root.get_f64("Health").unwrap_or(20.0) as f32,
        food: root.get_i32("foodLevel").unwrap_or(20),
    })
}

pub fn save(server: &Server, player: &Player) {
    let path = path_for(server, player.uuid);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let state = player.lock();
    let mut root = NbtCompound::new();
    root.put("DataVersion", server.data.data_version);
    root.put("Pos", vec![NbtTag::Double(state.x), NbtTag::Double(state.y), NbtTag::Double(state.z)]);
    root.put("Rotation", vec![NbtTag::Float(state.yaw), NbtTag::Float(state.pitch)]);
    root.put("playerGameType", state.game_mode as i32);
    root.put("Health", state.health);
    root.put("foodLevel", state.food);
    root.put("Dimension", state.dimension.as_str());
    root.put("OnGround", state.on_ground);
    root.put("SelectedItemSlot", state.held_slot);
    root.put("UUID", uuid_to_ints(player.uuid));
    let mut garnet = NbtCompound::new();
    garnet.put("lastName", player.name());
    garnet.put("lastIp", player.ip.as_str());
    garnet.put("lastSeen", chrono::Utc::now().to_rfc3339());
    root.put("garnet", garnet);
    drop(state);

    let mut raw = Vec::new();
    NbtTag::Compound(root).write_named("", &mut raw);
    let tmp = path.with_extension("dat.tmp");
    let write = || -> std::io::Result<()> {
        let mut enc = GzEncoder::new(std::fs::File::create(&tmp)?, Compression::default());
        enc.write_all(&raw)?;
        enc.finish()?.sync_all()?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(err) = write() {
        tracing::error!("saving player data for {}: {err}", player.name());
    }
}

fn uuid_to_ints(uuid: Uuid) -> Vec<i32> {
    let b = uuid.as_bytes();
    (0..4)
        .map(|i| i32::from_be_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]))
        .collect()
}
