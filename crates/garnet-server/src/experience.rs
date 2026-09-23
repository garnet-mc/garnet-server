//! Experience: the orbs mobs and furnaces leave behind, and the levels
//! they add up to.
//!
//! Vanilla's arithmetic: a level costs more the higher you are, orbs come
//! in fixed sizes so a big reward is a handful rather than a cloud, and an
//! orb drifts towards anyone who comes near before it is picked up.

use crate::player::{Player, PlayerState};
use crate::server::Server;
use crate::world_entities::{self, Entity};
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::GameMode;
use std::sync::Arc;

/// How close a player has to be for an orb to come to them, and to take it.
const ATTRACT_RANGE: f64 = 7.0;
const PICKUP_RANGE: f64 = 1.1;
/// The sizes vanilla splits a reward into, largest first.
const ORB_SIZES: [i32; 11] = [1237, 617, 307, 149, 73, 37, 17, 7, 3, 2, 1];

/// Points needed to climb one more level from here.
pub fn to_next_level(level: i32) -> i32 {
    if level >= 31 {
        9 * level - 158
    } else if level >= 16 {
        5 * level - 38
    } else {
        2 * level + 7
    }
}

/// Gives a player experience, in points, and tells their client.
pub fn give(player: &Arc<Player>, amount: i32) {
    if amount == 0 {
        return;
    }
    {
        let mut s = player.lock();
        add_points(&mut s, amount);
    }
    sync(player);
}

/// Takes levels off a player. False when they cannot afford it.
pub fn take_levels(player: &Arc<Player>, levels: i32) -> bool {
    {
        let mut s = player.lock();
        if s.xp_level < levels {
            return false;
        }
        s.xp_level -= levels;
        s.xp_progress = 0.0;
        // The total is only ever a tally for the death screen; keep it sane.
        s.xp_total = (0..s.xp_level).map(to_next_level).sum();
    }
    sync(player);
    true
}

/// Moves a player's experience by `amount` points, levelling as it goes.
fn add_points(state: &mut PlayerState, amount: i32) {
    let mut points = (state.xp_progress * to_next_level(state.xp_level) as f32).round() as i32 + amount;
    state.xp_total = (state.xp_total + amount.max(0)).max(0);
    while points < 0 && state.xp_level > 0 {
        state.xp_level -= 1;
        points += to_next_level(state.xp_level);
    }
    if points < 0 {
        points = 0;
    }
    loop {
        let needed = to_next_level(state.xp_level);
        if points < needed {
            break;
        }
        points -= needed;
        state.xp_level += 1;
    }
    state.xp_progress = points as f32 / to_next_level(state.xp_level) as f32;
}

pub fn sync(player: &Arc<Player>) {
    let (bar, level, total) = {
        let s = player.lock();
        (s.xp_progress, s.xp_level, s.xp_total)
    };
    player.send(&cb::SetExperience { bar, level, total });
}

/// Scatters a reward on the ground as orbs.
pub fn drop_orbs(server: &Arc<Server>, x: f64, y: f64, z: f64, mut amount: i32) {
    while amount > 0 {
        let size = ORB_SIZES.iter().copied().find(|size| *size <= amount).unwrap_or(1);
        amount -= size;
        let Some(mut orb) = world_entities::new_entity(server, "experience_orb", x, y, z) else { return };
        orb.xp = size;
        orb.velocity = (
            (rand::random::<f64>() - 0.5) * 0.2,
            0.1 + rand::random::<f64>() * 0.1,
            (rand::random::<f64>() - 0.5) * 0.2,
        );
        world_entities::spawn(server, orb);
    }
}

/// Orbs drift towards whoever is near, and are taken when they arrive.
pub fn tick(server: &Arc<Server>) {
    let orbs: Vec<Entity> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities
            .by_id
            .values()
            .filter(|entity| entity.kind == "minecraft:experience_orb")
            .cloned()
            .collect()
    };
    if orbs.is_empty() {
        return;
    }
    let players: Vec<(Arc<Player>, f64, f64, f64)> = server
        .online_players()
        .into_iter()
        .filter(|p| !matches!(p.lock().game_mode, GameMode::Spectator))
        .map(|p| {
            let (x, y, z) = {
                let s = p.lock();
                (s.x, s.y, s.z)
            };
            (p, x, y, z)
        })
        .collect();

    for orb in orbs {
        let nearest = players
            .iter()
            .map(|(player, x, y, z)| {
                let distance = ((orb.x - x).powi(2) + (orb.y - y).powi(2) + (orb.z - z).powi(2)).sqrt();
                (distance, player, (*x, *y, *z))
            })
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let Some((distance, player, (px, py, pz))) = nearest else { continue };
        if distance > ATTRACT_RANGE {
            continue;
        }
        if distance <= PICKUP_RANGE {
            give(player, orb.xp);
            player.send(&cb::TakeItemEntity {
                item_id: orb.id,
                player_id: player.entity_id,
                amount: 1,
            });
            world_entities::despawn(server, orb.id);
            continue;
        }
        // Drift towards them, faster the closer they are.
        let pull = 0.9 / distance.max(0.4);
        let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stored) = entities.by_id.get_mut(&orb.id) {
            stored.velocity.0 += (px - orb.x) * pull * 0.05;
            stored.velocity.1 += (py + 0.8 - orb.y) * pull * 0.05;
            stored.velocity.2 += (pz - orb.z) * pull * 0.05;
            stored.no_gravity = false;
        }
    }
}

/// What breaking this block is worth, the way vanilla pays for ores.
pub fn for_block(block: &str) -> i32 {
    let short = block.strip_prefix("minecraft:").unwrap_or(block);
    let range = match short {
        "coal_ore" | "deepslate_coal_ore" => (0, 2),
        "diamond_ore" | "deepslate_diamond_ore" | "emerald_ore" | "deepslate_emerald_ore" => (3, 7),
        "lapis_ore" | "deepslate_lapis_ore" | "nether_quartz_ore" => (2, 5),
        "redstone_ore" | "deepslate_redstone_ore" => (1, 5),
        "nether_gold_ore" => (0, 1),
        "spawner" => (15, 43),
        _ => return 0,
    };
    rand::random_range(range.0..=range.1)
}

/// What killing this mob is worth.
pub fn for_mob(kind: &str) -> i32 {
    let short = kind.strip_prefix("minecraft:").unwrap_or(kind);
    match short {
        "zombie" | "skeleton" | "spider" | "creeper" | "husk" | "stray" | "drowned" | "cave_spider" | "zombie_villager" => 5,
        "enderman" | "witch" | "blaze" | "piglin" => 6,
        "pig" | "cow" | "sheep" | "chicken" | "rabbit" | "horse" | "wolf" | "cat" => rand::random_range(1..=3),
        "wither" => 50,
        "ender_dragon" => 500,
        _ => 0,
    }
}
