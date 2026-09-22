//! Things going off: primed TNT and the hole it leaves.
//!
//! Vanilla casts a few hundred rays out of the middle and eats away at
//! each one according to how much the blocks in the way resist; this does
//! the same with fewer rays, which gives the same ragged sphere without
//! the cost. Whatever is caught in it takes damage by how close it was,
//! and about a fifth of what breaks is left on the ground.

use crate::server::Server;
use crate::world_entities;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::BlockPos;
use std::sync::Arc;

/// How strong TNT is, and how long its fuse burns.
const TNT_POWER: f32 = 4.0;
const FUSE_TICKS: u64 = 80;
/// Rays cast out of the middle, per axis of the cube around it.
const RAYS: i32 = 12;
/// One in this many broken blocks is left to pick up.
const DROP_ONE_IN: i32 = 4;

/// Lights the fuse: the block goes now, the bang comes later.
pub fn prime(server: &Arc<Server>, pos: BlockPos) {
    let air = server.data.blocks.default_state("air").unwrap_or(0) as u32;
    server.set_block(pos, air);
    let mut fuses = server.fuses.lock().unwrap_or_else(|e| e.into_inner());
    fuses.push((pos, server.current_tick() + FUSE_TICKS));
}

/// Sets off whatever fuses have burned down.
pub fn tick(server: &Arc<Server>) {
    let now = server.current_tick();
    let due: Vec<BlockPos> = {
        let mut fuses = server.fuses.lock().unwrap_or_else(|e| e.into_inner());
        let (ready, waiting): (Vec<_>, Vec<_>) = fuses.drain(..).partition(|(_, when)| *when <= now);
        *fuses = waiting;
        ready.into_iter().map(|(pos, _)| pos).collect()
    };
    for pos in due {
        explode(server, pos.x as f64 + 0.5, pos.y as f64 + 0.5, pos.z as f64 + 0.5, TNT_POWER);
    }
}

/// How much a block stands up to a blast. Most things give way at once.
fn resistance(name: &str) -> f32 {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    match short {
        "bedrock" | "barrier" | "end_portal_frame" | "reinforced_deepslate" | "command_block" => f32::INFINITY,
        "obsidian" | "crying_obsidian" | "respawn_anchor" | "ancient_debris" | "enchanting_table" | "anvil" => 1200.0,
        "water" | "lava" => 100.0,
        "netherite_block" | "ender_chest" => 600.0,
        "end_stone" | "end_stone_bricks" => 9.0,
        "cobblestone" | "stone" | "deepslate" | "stone_bricks" | "bricks" => 6.0,
        "iron_door" | "iron_bars" | "iron_block" => 5.0,
        _ if short.ends_with("_ore") || short.ends_with("_block") => 3.0,
        _ if short.ends_with("_leaves") || short == "tnt" || short.ends_with("_carpet") => 0.0,
        _ => 0.5,
    }
}

/// The bang itself.
pub fn explode(server: &Arc<Server>, x: f64, y: f64, z: f64, power: f32) {
    let mut broken: Vec<BlockPos> = Vec::new();
    // Rays out of the middle, each eaten away by what it passes through.
    for sx in 0..RAYS {
        for sy in 0..RAYS {
            for sz in 0..RAYS {
                let edge = sx == 0 || sx == RAYS - 1 || sy == 0 || sy == RAYS - 1 || sz == 0 || sz == RAYS - 1;
                if !edge {
                    continue;
                }
                let dx = sx as f64 / (RAYS - 1) as f64 * 2.0 - 1.0;
                let dy = sy as f64 / (RAYS - 1) as f64 * 2.0 - 1.0;
                let dz = sz as f64 / (RAYS - 1) as f64 * 2.0 - 1.0;
                let length = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-6);
                let (dx, dy, dz) = (dx / length, dy / length, dz / length);
                let mut strength = power * (0.7 + rand::random::<f32>() * 0.6);
                let (mut rx, mut ry, mut rz) = (x, y, z);
                while strength > 0.0 {
                    let pos = BlockPos::new(rx.floor() as i32, ry.floor() as i32, rz.floor() as i32);
                    let name = block_name(server, pos);
                    if !name.is_empty() && name != "minecraft:air" {
                        let resist = resistance(&name);
                        if !resist.is_finite() {
                            break;
                        }
                        strength -= (resist + 0.3) * 0.3;
                        if strength > 0.0 && !broken.contains(&pos) {
                            broken.push(pos);
                        }
                    }
                    strength -= 0.225;
                    rx += dx * 0.3;
                    ry += dy * 0.3;
                    rz += dz * 0.3;
                }
            }
        }
    }

    let air = server.data.blocks.default_state("air").unwrap_or(0) as u32;
    for pos in &broken {
        let name = block_name(server, *pos);
        server.set_block(*pos, air);
        // A little of it survives to be picked up.
        if rand::random_range(0..DROP_ONE_IN) == 0 {
            let props = std::collections::BTreeMap::new();
            let tool = crate::loot::Tool {
                item_name: None,
                silk_touch: false,
                fortune: 0,
            };
            for (item, count) in server.loot.drops(&name, &props, &tool) {
                if let Some(id) = crate::items::item_id(server, &item) {
                    world_entities::drop_item(
                        server,
                        garnet_protocol::packets::play::items::ItemStack::new(id, count),
                        pos.x as f64 + 0.5,
                        pos.y as f64 + 0.5,
                        pos.z as f64 + 0.5,
                        (0.0, 0.1, 0.0),
                        10,
                    );
                }
            }
        }
        // Another charge caught in the blast goes off as well.
        if name == "minecraft:tnt" {
            prime(server, *pos);
        }
    }

    // Everyone close enough feels it.
    for player in server.online_players() {
        let (px, py, pz) = {
            let s = player.lock();
            (s.x, s.y, s.z)
        };
        let distance = ((px - x).powi(2) + (py - y).powi(2) + (pz - z).powi(2)).sqrt();
        let reach = power as f64 * 2.0;
        if distance > reach {
            continue;
        }
        let share = 1.0 - distance / reach;
        let damage = ((share * share + share) / 2.0 * 7.0 * reach + 1.0) as f32;
        crate::survival::hurt_by(server, &player, damage, "explosion", Some((x, z)), None);
    }
    let hit: Vec<(i32, f32)> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities
            .by_id
            .values()
            .filter(|e| !e.is_item())
            .filter_map(|e| {
                let distance = ((e.x - x).powi(2) + (e.y - y).powi(2) + (e.z - z).powi(2)).sqrt();
                let reach = power as f64 * 2.0;
                (distance <= reach).then(|| {
                    let share = 1.0 - distance / reach;
                    (e.id, ((share * share + share) / 2.0 * 7.0 * reach + 1.0) as f32)
                })
            })
            .collect()
    };
    for (id, damage) in hit {
        crate::survival::damage_entity_directly(server, id, damage, (x, z));
    }

    let chunk = garnet_protocol::ChunkPos::from_block(x.floor() as i32, z.floor() as i32);
    if let Some(sound) = garnet_protocol::Identifier::parse("minecraft:entity.generic.explode") {
        let registry_id = server.data.registries.id_of("sound_event", &sound.to_string());
        server.broadcast_near(
            chunk,
            &cb::Sound {
                name: sound,
                registry_id,
                source: cb::SoundSource::Blocks,
                x,
                y,
                z,
                volume: 4.0,
                pitch: 1.0,
                seed: rand::random(),
            },
            None,
        );
    }
}

fn block_name(server: &Arc<Server>, pos: BlockPos) -> String {
    let Ok(state) = server.world().get_block(pos) else { return String::new() };
    server
        .data
        .blocks
        .block_of_state(state as i32)
        .map(|b| b.name.clone())
        .unwrap_or_default()
}
