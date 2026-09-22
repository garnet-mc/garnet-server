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
    pub xp_level: i32,
    pub xp_total: i32,
    pub spawn_point: Option<garnet_protocol::BlockPos>,
    pub tags: Vec<String>,
    pub effects: Vec<crate::player::ActiveEffect>,
    pub attributes: Vec<(String, f64)>,
    /// (slot, item name, count, component patch)
    pub inventory: Vec<(usize, String, i32, Vec<u8>)>,
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
        xp_level: root.get_i32("XpLevel").unwrap_or(0),
        xp_total: root.get_i32("XpTotal").unwrap_or(0),
        spawn_point: match (root.get_i32("SpawnX"), root.get_i32("SpawnY"), root.get_i32("SpawnZ")) {
            (Some(x), Some(y), Some(z)) => Some(garnet_protocol::BlockPos::new(x, y, z)),
            _ => None,
        },
        tags: root
            .get_list("Tags")
            .map(|l| l.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect())
            .unwrap_or_default(),
        effects: root
            .get_list("active_effects")
            .map(|l| l.iter().filter_map(read_effect).collect())
            .unwrap_or_default(),
        inventory: root
            .get_list("Inventory")
            .map(|l| {
                l.iter()
                    .filter_map(|t| {
                        let c = t.as_compound()?;
                        let slot = c.get_i32("Slot")?;
                        let patch = match c.get("garnet_patch") {
                            Some(NbtTag::ByteArray(b)) => b.iter().map(|x| *x as u8).collect(),
                            _ => Vec::new(),
                        };
                        Some((slot as usize, c.get_str("id")?.to_owned(), c.get_i32("count").unwrap_or(1), patch))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        attributes: root
            .get_list("garnet_attributes")
            .map(|l| {
                l.iter()
                    .filter_map(|t| {
                        let c = t.as_compound()?;
                        Some((c.get_str("id")?.to_owned(), c.get_f64("base")?))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Vanilla's layout: id string, amplifier, duration in ticks, flags.
fn read_effect(tag: &NbtTag) -> Option<crate::player::ActiveEffect> {
    let c = tag.as_compound()?;
    let name = c.get_str("id")?.to_owned();
    let duration = c.get_i32("duration").unwrap_or(0);
    Some(crate::player::ActiveEffect {
        name,
        id: c.get_i32("garnet_id").unwrap_or(0),
        amplifier: c.get_i32("amplifier").unwrap_or(0),
        // Durations are stored as ticks left; the caller turns that into a tick number.
        expires_tick: if duration < 0 { None } else { Some(duration as u64) },
        particles: c.get_i32("show_particles").unwrap_or(1) != 0,
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
    root.put("XpLevel", state.xp_level);
    root.put("XpTotal", state.xp_total);
    if let Some(spawn) = state.spawn_point {
        root.put("SpawnX", spawn.x);
        root.put("SpawnY", spawn.y);
        root.put("SpawnZ", spawn.z);
    }
    if !state.tags.is_empty() {
        root.put("Tags", state.tags.iter().map(|t| NbtTag::String(t.clone())).collect::<Vec<_>>());
    }
    if !state.effects.is_empty() {
        let now = server.current_tick();
        let effects: Vec<NbtTag> = state
            .effects
            .iter()
            .map(|e| {
                let mut c = NbtCompound::new();
                c.put("id", e.name.as_str());
                c.put("garnet_id", e.id);
                c.put("amplifier", e.amplifier);
                c.put("duration", e.expires_tick.map(|t| t.saturating_sub(now) as i32).unwrap_or(-1));
                c.put("show_particles", e.particles as i32);
                NbtTag::Compound(c)
            })
            .collect();
        root.put("active_effects", effects);
    }
    let inventory: Vec<NbtTag> = state
        .inventory
        .slots
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.is_empty())
        .map(|(slot, s)| {
            let mut c = NbtCompound::new();
            c.put("Slot", slot as i8);
            c.put("id", crate::items::item_name(server, s.item).as_str());
            c.put("count", s.count);
            if !s.patch.is_empty() {
                c.put("garnet_patch", NbtTag::ByteArray(s.patch.iter().map(|b| *b as i8).collect()));
            }
            NbtTag::Compound(c)
        })
        .collect();
    root.put("Inventory", inventory);
    if !state.attributes.is_empty() {
        let attributes: Vec<NbtTag> = state
            .attributes
            .iter()
            .map(|(id, base)| {
                let mut c = NbtCompound::new();
                c.put("id", id.as_str());
                c.put("base", *base);
                NbtTag::Compound(c)
            })
            .collect();
        root.put("garnet_attributes", attributes);
    }
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
