//! Mobs: what spawns where, what it does while you are near, and what it
//! leaves behind.
//!
//! Every mob gets a light brain: hostiles look for the nearest player and
//! walk at them, everything else wanders. Walking is done by pushing the
//! entity along and letting the shared physics handle gravity, walls and
//! the step up onto a block. Spawning follows vanilla's shape without its
//! exactness: dark ground away from players for the hostiles, bright grass
//! for the animals, both under a cap so a busy server cannot fill up with
//! them.

use crate::server::Server;
use crate::world_entities::{self, Entity};
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::BlockPos;
use std::sync::Arc;

/// What a kind of mob is like. Speeds are blocks per tick.
struct Kind {
    name: &'static str,
    health: f32,
    speed: f64,
    damage: f32,
    hostile: bool,
}

const KINDS: &[Kind] = &[
    Kind { name: "zombie", health: 20.0, speed: 0.115, damage: 3.0, hostile: true },
    Kind { name: "spider", health: 16.0, speed: 0.15, damage: 2.0, hostile: true },
    Kind { name: "pig", health: 10.0, speed: 0.12, damage: 0.0, hostile: false },
    Kind { name: "cow", health: 10.0, speed: 0.10, damage: 0.0, hostile: false },
    Kind { name: "sheep", health: 8.0, speed: 0.10, damage: 0.0, hostile: false },
    Kind { name: "chicken", health: 4.0, speed: 0.11, damage: 0.0, hostile: false },
];

/// How far a hostile mob notices a player from.
const SIGHT: f64 = 16.0;
/// How close it has to be to land a hit, and how often it may.
const REACH: f64 = 2.0;
const ATTACK_EVERY: u32 = 20;
/// Mobs past this go away; the animals stay.
const DESPAWN_RANGE: f64 = 72.0;
/// Caps per player, so a crowded server does not fill with mobs.
const HOSTILE_CAP: usize = 12;
const PASSIVE_CAP: usize = 8;
/// Where new mobs may appear relative to a player.
const SPAWN_MIN: f64 = 24.0;
const SPAWN_MAX: f64 = 44.0;

fn kind_of(name: &str) -> Option<&'static Kind> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    KINDS.iter().find(|k| k.name == short)
}

/// True for anything this module drives.
pub fn is_mob(name: &str) -> bool {
    kind_of(name).is_some()
}

/// Whether something that would keep a player awake is close by. Vanilla
/// looks in a box eight blocks out and five up from the bed.
pub fn monsters_near(server: &Arc<Server>, pos: BlockPos) -> bool {
    let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    entities.by_id.values().any(|entity| {
        kind_of(&entity.kind).is_some_and(|kind| kind.hostile)
            && (entity.x - pos.x as f64).abs() <= 8.0
            && (entity.z - pos.z as f64).abs() <= 8.0
            && (entity.y - pos.y as f64).abs() <= 5.0
    })
}

/// Starting health for a summoned or spawned mob.
pub fn health_of(name: &str) -> f32 {
    kind_of(name).map(|k| k.health).unwrap_or(20.0)
}

pub fn tick(server: &Arc<Server>, tick: u64) {
    think(server, tick);
    if tick % 20 == 0 {
        let rules = server.rules.read().unwrap_or_else(|e| e.into_inner());
        let spawning = rules.game_rule_bool("doMobSpawning");
        let difficulty = rules.difficulty_id();
        drop(rules);
        if spawning {
            spawn_round(server, difficulty);
        }
        cull(server, difficulty);
    }
}

/// One step of everyone's brain.
fn think(server: &Arc<Server>, tick: u64) {
    let mobs: Vec<Entity> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities.by_id.values().filter(|e| is_mob(&e.kind)).cloned().collect()
    };
    if mobs.is_empty() {
        return;
    }
    let players: Vec<(i32, f64, f64, f64, std::sync::Arc<crate::player::Player>)> = server
        .online_players()
        .into_iter()
        .filter(|p| matches!(p.lock().game_mode, GameMode::Survival | GameMode::Adventure))
        .map(|p| {
            let (x, y, z) = {
                let s = p.lock();
                (s.x, s.y, s.z)
            };
            (p.entity_id, x, y, z, p)
        })
        .collect();

    for mob in mobs {
        let Some(kind) = kind_of(&mob.kind) else { continue };
        let nearest = players
            .iter()
            .map(|(id, x, y, z, p)| {
                let d = ((mob.x - x).powi(2) + (mob.y - y).powi(2) + (mob.z - z).powi(2)).sqrt();
                (d, *id, (*x, *y, *z), p.clone())
            })
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut goal: Option<(f64, f64)> = None;
        let mut attacked = false;
        if kind.hostile {
            if let Some((distance, _, (px, py, pz), player)) = &nearest {
                if *distance <= SIGHT {
                    goal = Some((*px, *pz));
                    if *distance <= REACH && (py - mob.y).abs() < 2.5 {
                        attacked = attack(server, &mob, kind, player);
                    }
                }
            }
        }
        if goal.is_none() && tick % 40 == (mob.id.unsigned_abs() as u64 % 40) {
            // A quiet wander, in a direction that changes now and then.
            let angle = rand::random::<f64>() * std::f64::consts::TAU;
            goal = Some((mob.x + angle.cos() * 6.0, mob.z + angle.sin() * 6.0));
        }
        if let Some((gx, gz)) = goal {
            walk(server, mob.id, kind, gx, gz);
        }
        if attacked {
            let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(stored) = entities.by_id.get_mut(&mob.id) {
                stored.attack_cooldown = ATTACK_EVERY;
            }
        } else {
            let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(stored) = entities.by_id.get_mut(&mob.id) {
                stored.attack_cooldown = stored.attack_cooldown.saturating_sub(1);
            }
        }
    }
}

/// Pushes a mob towards a spot, hopping up a block when something is in
/// the way.
fn walk(server: &Arc<Server>, id: i32, kind: &Kind, gx: f64, gz: f64) {
    let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mob) = entities.by_id.get_mut(&id) else { return };
    let (dx, dz) = (gx - mob.x, gz - mob.z);
    let distance = (dx * dx + dz * dz).sqrt();
    if distance < 0.4 {
        return;
    }
    let (ux, uz) = (dx / distance, dz / distance);
    mob.velocity.0 = ux * kind.speed;
    mob.velocity.2 = uz * kind.speed;
    mob.yaw = (-ux.atan2(uz)).to_degrees() as f32;
    let blocked = mob.blocked_ahead;
    if blocked && mob.on_ground {
        mob.velocity.1 = 0.42; // a jump, the way every mob climbs a step
    }
}

/// A mob hits a player. Returns whether the blow landed.
fn attack(server: &Arc<Server>, mob: &Entity, kind: &Kind, player: &Arc<crate::player::Player>) -> bool {
    if mob.attack_cooldown > 0 || kind.damage <= 0.0 {
        return false;
    }
    let difficulty = server.rules.read().unwrap_or_else(|e| e.into_inner()).difficulty_id();
    let scale = match difficulty {
        0 => return false, // peaceful: nothing hits you
        1 => 0.5,
        3 => 1.5,
        _ => 1.0,
    };
    crate::survival::hurt_by(
        server,
        player,
        kind.damage * scale,
        "mob_attack",
        Some((mob.x, mob.z)),
        Some(kind.name.to_owned()),
    );
    true
}

/// Sends mobs that nobody is near away again, and clears the hostiles out
/// on peaceful.
fn cull(server: &Arc<Server>, difficulty: u8) {
    let players: Vec<(f64, f64, f64)> = server
        .online_players()
        .into_iter()
        .map(|p| {
            let s = p.lock();
            (s.x, s.y, s.z)
        })
        .collect();
    let doomed: Vec<i32> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities
            .by_id
            .values()
            .filter(|e| {
                let Some(kind) = kind_of(&e.kind) else { return false };
                if e.persistent {
                    return false;
                }
                if kind.hostile && difficulty == 0 {
                    return true;
                }
                if !kind.hostile {
                    return false; // animals stay where they are put
                }
                players.is_empty()
                    || players
                        .iter()
                        .all(|(x, y, z)| ((e.x - x).powi(2) + (e.y - y).powi(2) + (e.z - z).powi(2)).sqrt() > DESPAWN_RANGE)
            })
            .map(|e| e.id)
            .collect()
    };
    for id in doomed {
        world_entities::despawn(server, id);
    }
}

/// Tries to put a few new mobs in the dark (or on the grass) around each
/// player.
fn spawn_round(server: &Arc<Server>, difficulty: u8) {
    for player in server.online_players() {
        let (px, py, pz, mode) = {
            let s = player.lock();
            (s.x, s.y, s.z, s.game_mode)
        };
        if !matches!(mode, GameMode::Survival | GameMode::Adventure) {
            continue;
        }
        let (hostiles, passives) = count_near(server, px, py, pz);
        for _ in 0..3 {
            let angle = rand::random::<f64>() * std::f64::consts::TAU;
            let radius = SPAWN_MIN + rand::random::<f64>() * (SPAWN_MAX - SPAWN_MIN);
            let x = px + angle.cos() * radius;
            let z = pz + angle.sin() * radius;
            let Some((y, ground)) = ground_at(server, x, z, py) else { continue };
            let light = light_at(server, x, y, z);
            let hostile = light.0 <= 0 && difficulty > 0 && hostiles < HOSTILE_CAP;
            let passive = light.1 >= 9 && ground == "minecraft:grass_block" && passives < PASSIVE_CAP;
            let candidates: Vec<&Kind> = if hostile {
                KINDS.iter().filter(|k| k.hostile).collect()
            } else if passive {
                KINDS.iter().filter(|k| !k.hostile).collect()
            } else {
                continue;
            };
            let kind = candidates[rand::random_range(0..candidates.len())];
            let pack = if kind.hostile { 1 } else { rand::random_range(2..5) };
            for n in 0..pack {
                let ox = x + (n as f64 - 1.0) * 0.6;
                let oz = z + (n as f64 - 1.0) * 0.6;
                let Some((sy, _)) = ground_at(server, ox, oz, py) else { continue };
                if let Some(mut entity) = world_entities::new_entity(server, kind.name, ox, sy, oz) {
                    entity.health = kind.health;
                    world_entities::spawn(server, entity);
                }
            }
            break; // one group per player per round is plenty
        }
    }
}

/// How many mobs of each sort are already near this spot.
fn count_near(server: &Arc<Server>, x: f64, y: f64, z: f64) -> (usize, usize) {
    let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    let mut hostiles = 0;
    let mut passives = 0;
    for entity in entities.by_id.values() {
        let Some(kind) = kind_of(&entity.kind) else { continue };
        let distance = ((entity.x - x).powi(2) + (entity.y - y).powi(2) + (entity.z - z).powi(2)).sqrt();
        if distance > SPAWN_MAX + 16.0 {
            continue;
        }
        if kind.hostile {
            hostiles += 1;
        } else {
            passives += 1;
        }
    }
    (hostiles, passives)
}

/// The standing height at x/z near the player's own level, and what the
/// block underfoot is.
fn ground_at(server: &Arc<Server>, x: f64, z: f64, near_y: f64) -> Option<(f64, String)> {
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    let top = (near_y as i32 + 16).min(319);
    let bottom = (near_y as i32 - 24).max(-63);
    let mut world = server.world();
    let blocks = &server.data.blocks;
    for y in (bottom..=top).rev() {
        let below = world.get_block(BlockPos::new(bx, y, bz)).ok()?;
        if blocks.is_air(below as i32) || blocks.is_liquid(below as i32) {
            continue;
        }
        let feet = world.get_block(BlockPos::new(bx, y + 1, bz)).ok()?;
        let head = world.get_block(BlockPos::new(bx, y + 2, bz)).ok()?;
        if !blocks.is_air(feet as i32) || !blocks.is_air(head as i32) {
            continue;
        }
        let name = blocks.block_of_state(below as i32).map(|b| b.name.clone()).unwrap_or_default();
        return Some(((y + 1) as f64, name));
    }
    None
}

/// Block light and sky light at a spot.
fn light_at(server: &Arc<Server>, x: f64, y: f64, z: f64) -> (u8, u8) {
    let pos = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
    let mut world = server.world();
    let Ok(chunk) = world.chunk_mut(pos.chunk()) else { return (15, 15) };
    let Some(light) = chunk.light.as_ref() else { return (15, 15) };
    if !chunk.range.contains(pos.y) {
        return (15, 15);
    }
    let section = ((pos.y - chunk.range.min_y) >> 4) as usize;
    let (lx, ly, lz) = ((pos.x & 15) as usize, (pos.y & 15) as usize, (pos.z & 15) as usize);
    (light.block_at(section, lx, ly, lz), light.sky_at(section, lx, ly, lz))
}
