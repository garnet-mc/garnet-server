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
    /// How it fights: with its hands, with a bow, or by blowing up.
    fights: Fight,
    /// The undead catch fire in the morning.
    burns_by_day: bool,
}

/// What a mob does when it gets to you.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fight {
    Melee,
    /// Keeps its distance and shoots.
    Bow,
    /// Walks up and goes off, with this much force.
    Blast,
    None,
}

const KINDS: &[Kind] = &[
    Kind {
        name: "zombie",
        health: 20.0,
        speed: 0.115,
        damage: 3.0,
        hostile: true,
        fights: Fight::Melee,
        burns_by_day: true,
    },
    Kind {
        name: "skeleton",
        health: 20.0,
        speed: 0.12,
        damage: 2.0,
        hostile: true,
        fights: Fight::Bow,
        burns_by_day: true,
    },
    Kind {
        name: "creeper",
        health: 20.0,
        speed: 0.12,
        damage: 0.0,
        hostile: true,
        fights: Fight::Blast,
        burns_by_day: false,
    },
    Kind {
        name: "spider",
        health: 16.0,
        speed: 0.15,
        damage: 2.0,
        hostile: true,
        fights: Fight::Melee,
        burns_by_day: false,
    },
    Kind {
        name: "pig",
        health: 10.0,
        speed: 0.12,
        damage: 0.0,
        hostile: false,
        fights: Fight::None,
        burns_by_day: false,
    },
    Kind {
        name: "cow",
        health: 10.0,
        speed: 0.10,
        damage: 0.0,
        hostile: false,
        fights: Fight::None,
        burns_by_day: false,
    },
    Kind {
        name: "sheep",
        health: 8.0,
        speed: 0.10,
        damage: 0.0,
        hostile: false,
        fights: Fight::None,
        burns_by_day: false,
    },
    Kind {
        name: "chicken",
        health: 4.0,
        speed: 0.11,
        damage: 0.0,
        hostile: false,
        fights: Fight::None,
        burns_by_day: false,
    },
];

/// How close a creeper gets before it lights itself, and how long the
/// fuse burns.
const FUSE_RANGE: f64 = 3.0;
const FUSE_TICKS: u32 = 30;
/// How much of the world a creeper takes with it.
const BLAST: f32 = 3.0;
/// A skeleton keeps this far away, and shoots this often.
const BOW_RANGE: f64 = 15.0;
const KEEP_AWAY: f64 = 5.0;
const SHOOT_EVERY: u32 = 40;

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
        burn_the_undead(server);
    }
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
                match kind.fights {
                    // A bow wants room: it closes to a comfortable range
                    // and backs off if you get too near.
                    Fight::Bow if *distance <= BOW_RANGE => {
                        goal = match *distance < KEEP_AWAY {
                            true => Some((mob.x * 2.0 - px, mob.z * 2.0 - pz)),
                            false if *distance > KEEP_AWAY + 3.0 => Some((*px, *pz)),
                            false => None,
                        };
                        attacked = shoot(server, &mob, (*px, *py, *pz));
                    }
                    Fight::Blast if *distance <= SIGHT => {
                        goal = Some((*px, *pz));
                        fuse(server, &mob, *distance);
                    }
                    Fight::Melee if *distance <= SIGHT => {
                        goal = Some((*px, *pz));
                        if *distance <= REACH && (py - mob.y).abs() < 2.5 {
                            attacked = attack(server, &mob, kind, player);
                        }
                    }
                    _ => {}
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
        if mob.on_ground {
            crate::redstone::step_on(
                server,
                BlockPos::new(mob.x.floor() as i32, mob.y.floor() as i32, mob.z.floor() as i32),
            );
        }
        if attacked {
            let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(stored) = entities.by_id.get_mut(&mob.id) {
                stored.attack_cooldown = match kind.fights {
                    Fight::Bow => SHOOT_EVERY,
                    _ => ATTACK_EVERY,
                };
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

/// A skeleton looses an arrow, if it has waited long enough and can see
/// where it is aiming. Returns whether it shot.
fn shoot(server: &Arc<Server>, mob: &Entity, at: (f64, f64, f64)) -> bool {
    if mob.attack_cooldown > 0 {
        return false;
    }
    let difficulty = server.rules.read().unwrap_or_else(|e| e.into_inner()).difficulty_id();
    if difficulty == 0 {
        return false;
    }
    let from = (mob.x, mob.y + 1.4, mob.z);
    // Vanilla aims a third of the way up the body, not at the eyes, and
    // lets the arc do the rest.
    let body = (at.0, at.1 + 0.6, at.2);
    if !can_see(server, from, (at.0, at.1 + 1.4, at.2)) {
        return false;
    }
    // The easier the game, the wilder the shot.
    let spread = match difficulty {
        1 => 0.16,
        3 => 0.04,
        _ => 0.10,
    };
    crate::projectiles::mob_shoots(server, from, body, mob.id, spread);
    true
}

/// Whether there is clear air between two points.
fn can_see(server: &Arc<Server>, from: (f64, f64, f64), to: (f64, f64, f64)) -> bool {
    let line = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
    let length = (line.0 * line.0 + line.1 * line.1 + line.2 * line.2).sqrt();
    if length < 0.001 {
        return true;
    }
    let mut travelled = 0.5;
    while travelled < length - 0.5 {
        let share = travelled / length;
        let at = (from.0 + line.0 * share, from.1 + line.1 * share, from.2 + line.2 * share);
        let pos = BlockPos::new(at.0.floor() as i32, at.1.floor() as i32, at.2.floor() as i32);
        let state = server.world().get_block(pos).unwrap_or(0);
        if !server.data.blocks.is_air(state as i32) && !server.data.blocks.is_liquid(state as i32) {
            return false;
        }
        travelled += 0.5;
    }
    true
}

/// A creeper close enough to a player lights itself, and goes off when the
/// fuse runs out. Walking away puts it out again.
fn fuse(server: &Arc<Server>, mob: &Entity, distance: f64) {
    let difficulty = server.rules.read().unwrap_or_else(|e| e.into_inner()).difficulty_id();
    if difficulty == 0 {
        return;
    }
    let lit = {
        let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        let Some(stored) = entities.by_id.get_mut(&mob.id) else {
            return;
        };
        if distance > FUSE_RANGE + 1.0 {
            stored.fuse = 0;
            return;
        }
        if distance > FUSE_RANGE {
            return; // close, but not yet
        }
        if stored.fuse == 0 {
            stored.fuse = FUSE_TICKS;
            false
        } else {
            stored.fuse -= 1;
            stored.fuse == 0
        }
    };
    if mob.fuse == 0 {
        hiss(server, mob);
    }
    if lit {
        crate::explosions::explode(server, mob.x, mob.y, mob.z, BLAST);
        world_entities::despawn(server, mob.id);
    }
}

/// The sound everyone learns to dread.
fn hiss(server: &Arc<Server>, mob: &Entity) {
    let Some(name) = garnet_protocol::Identifier::parse("minecraft:entity.creeper.primed") else {
        return;
    };
    let registry_id = server.data.registries.id_of("sound_event", &name.to_string());
    server.broadcast_near(
        garnet_protocol::ChunkPos::from_block(mob.x.floor() as i32, mob.z.floor() as i32),
        &garnet_protocol::packets::play::clientbound::Sound {
            name,
            registry_id,
            source: garnet_protocol::packets::play::clientbound::SoundSource::Hostile,
            x: mob.x,
            y: mob.y,
            z: mob.z,
            volume: 1.0,
            pitch: 0.5,
            seed: rand::random(),
        },
        None,
    );
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

/// Morning comes for the undead: anything standing in open daylight
/// catches fire and burns down.
fn burn_the_undead(server: &Arc<Server>) {
    let daylight = {
        let time = server.world().settings.time_of_day % 24000;
        let raining = server.rules.read().unwrap_or_else(|e| e.into_inner()).weather.raining;
        // Vanilla's daylight, near enough: the hours either side of noon.
        (1000..12000).contains(&time) && !raining
    };
    if !daylight {
        return;
    }
    let undead: Vec<(i32, f64, f64, f64)> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities
            .by_id
            .values()
            .filter(|entity| kind_of(&entity.kind).is_some_and(|kind| kind.burns_by_day) && entity.health > 0.0)
            .map(|entity| (entity.id, entity.x, entity.y, entity.z))
            .collect()
    };
    for (id, x, y, z) in undead {
        let caught = under_open_sky(server, x, y, z);
        world_entities::set_burning(server, id, caught);
        if caught {
            crate::survival::damage_entity_directly(server, id, 1.0, (x, z));
        }
    }
}

/// Whether the sky above a spot is clear all the way up.
fn under_open_sky(server: &Arc<Server>, x: f64, y: f64, z: f64) -> bool {
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    let head = y.floor() as i32 + 1;
    let mut world = server.world();
    let blocks = &server.data.blocks;
    for above in head..=319 {
        let Ok(state) = world.get_block(BlockPos::new(bx, above, bz)) else {
            return false;
        };
        if blocks.is_air(state as i32) {
            continue;
        }
        // Water counts: a zombie standing in it is safe from the sun.
        return false;
    }
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
