//! Staying alive: health, hunger, the damage the world does and what
//! happens when players hit each other.
//!
//! The numbers are vanilla's where they are felt: exhaustion builds as you
//! sprint, mine and fight, and spends the saturation behind the food bar
//! before the bar itself; a full stomach heals, an empty one starves. Falls,
//! fire, water and the void each hurt on their own schedule, and armour
//! takes its share off the top.

use crate::player::Player;
use crate::server::Server;
use crate::world_entities;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{BlockPos, Text};
use std::sync::Arc;

/// How long a player is safe for after being hurt.
const INVULNERABLE_TICKS: u64 = 10;
/// A full breath, in ticks, and how fast it comes back on the surface.
pub const MAX_AIR: i32 = 300;
const AIR_REGAINED: i32 = 4;
/// Falls shorter than this cost nothing.
const SAFE_FALL: f32 = 3.0;
/// Four points of exhaustion spend one point of saturation, or one of food.
const EXHAUSTION_PER_POINT: f32 = 4.0;
/// How long eating takes.
const EATING_TICKS: u64 = 32;

pub fn tick(server: &Arc<Server>, tick: u64) {
    for player in server.online_players() {
        let mode = player.lock().game_mode;
        if matches!(mode, GameMode::Creative | GameMode::Spectator) {
            let mut s = player.lock();
            s.fire_ticks = 0;
            s.air = MAX_AIR;
            s.fall_distance = 0.0;
            continue;
        }
        if player.lock().health <= 0.0 {
            continue; // waiting on the respawn screen
        }
        environment(server, &player, tick);
        hunger(server, &player, tick);
        eating(server, &player, tick);
    }
}

/// What the world is doing to this player: the void, fire, lava, water.
fn environment(server: &Arc<Server>, player: &Arc<Player>, tick: u64) {
    let (x, y, z) = {
        let s = player.lock();
        (s.x, s.y, s.z)
    };
    let min_y = server.world().range.min_y;
    if y < (min_y as f64) - 64.0 {
        if tick % 10 == 0 {
            hurt(server, player, 4.0, "out_of_world", None);
        }
        return;
    }

    // One world lock for both lookups: with a full server this runs for
    // every player, every tick.
    let (feet, head) = blocks_at(server, (x, y + 0.1, z), (x, y + 1.62, z));
    let in_water = head.contains("water") || head == "minecraft:bubble_column";
    let in_lava = feet.contains("lava");
    let in_fire = feet == "minecraft:fire" || feet == "minecraft:soul_fire";
    let fire_damage = server.rules.read().unwrap_or_else(|e| e.into_inner()).game_rule_bool("fireDamage");
    let drowning = server.rules.read().unwrap_or_else(|e| e.into_inner()).game_rule_bool("drowningDamage");

    // Breath. Bubbles empty under water and fill again in the air.
    let out_of_air = {
        let mut s = player.lock();
        if in_water {
            s.air -= 1;
            if s.air < 0 {
                s.air = 0;
                true
            } else {
                false
            }
        } else {
            s.air = (s.air + AIR_REGAINED).min(MAX_AIR);
            false
        }
    };
    if out_of_air && drowning && tick % 20 == 0 {
        hurt(server, player, 2.0, "drown", None);
    }

    // Fire keeps burning after you leave it, and water puts it out.
    {
        let mut s = player.lock();
        if in_lava {
            s.fire_ticks = s.fire_ticks.max(300);
        } else if in_fire {
            s.fire_ticks = s.fire_ticks.max(160);
        } else if in_water {
            s.fire_ticks = 0;
        } else if s.fire_ticks > 0 {
            s.fire_ticks -= 1;
        }
    }
    if fire_damage {
        if in_lava && tick % 10 == 0 {
            hurt(server, player, 4.0, "lava", None);
        } else if player.lock().fire_ticks > 0 && tick % 20 == 0 {
            hurt(server, player, 1.0, if in_fire { "in_fire" } else { "on_fire" }, None);
        }
    }
    send_flags(server, player);
}

/// Spends exhaustion, heals on a full stomach and starves on an empty one.
fn hunger(server: &Arc<Server>, player: &Arc<Player>, tick: u64) {
    let (natural, difficulty) = {
        let rules = server.rules.read().unwrap_or_else(|e| e.into_inner());
        (rules.game_rule_bool("naturalRegeneration"), rules.difficulty_id())
    };
    let mut changed = false;
    let mut starve = false;
    let mut heal = 0.0f32;
    {
        let mut s = player.lock();
        if difficulty == 0 {
            // Peaceful: the bar refills and nothing goes hungry.
            if s.food < 20 && tick % 20 == 0 {
                s.food += 1;
                changed = true;
            }
            if s.health < max_health(&s) && tick % 20 == 0 {
                heal = 1.0;
            }
        } else {
            while s.exhaustion >= EXHAUSTION_PER_POINT {
                s.exhaustion -= EXHAUSTION_PER_POINT;
                if s.saturation > 0.0 {
                    s.saturation = (s.saturation - 1.0).max(0.0);
                } else if s.food > 0 {
                    s.food -= 1;
                }
                changed = true;
            }
            let hurt_enough = s.health < max_health(&s);
            if natural && hurt_enough && s.food >= 18 {
                // A full bar with saturation left heals quickly, a merely
                // full one slowly.
                let fast = s.food >= 20 && s.saturation > 0.0;
                if (fast && tick % 10 == 0) || (!fast && tick % 80 == 0) {
                    heal = 1.0;
                    s.exhaustion += 6.0;
                }
            }
            if s.food == 0 && tick % 80 == 0 {
                // Easy leaves you at half a heart's worth of hearts, normal
                // at one, hard kills.
                let floor = match difficulty {
                    1 => 10.0,
                    2 => 1.0,
                    _ => 0.0,
                };
                starve = s.health > floor;
            }
        }
    }
    if heal > 0.0 {
        let max = max_health(&player.lock());
        let mut s = player.lock();
        s.health = (s.health + heal).min(max);
        changed = true;
    }
    if starve {
        hurt(server, player, 1.0, "starve", None);
    } else if changed {
        sync(player);
    }
}

/// Finishes a meal that has been going long enough.
fn eating(server: &Arc<Server>, player: &Arc<Player>, tick: u64) {
    let since = player.lock().eating_since;
    let Some(started) = since else { return };
    if tick.saturating_sub(started) < EATING_TICKS {
        return;
    }
    let held = {
        let s = player.lock();
        let slot = s.held_slot;
        s.inventory.held(slot).clone()
    };
    player.lock().eating_since = None;
    if held.is_empty() {
        return;
    }
    let name = crate::items::item_name(server, held.item);
    let Some((food, saturation)) = food_value(&name) else { return };
    {
        let mut s = player.lock();
        s.food = (s.food + food).min(20);
        s.saturation = (s.saturation + saturation).min(s.food as f32);
        let slot = s.held_slot;
        let stack = s.inventory.held_mut(slot);
        stack.count -= 1;
        if stack.count <= 0 {
            *stack = garnet_protocol::packets::play::items::ItemStack::EMPTY;
        }
    }
    crate::items::sync_inventory(player);
    sync(player);
}

/// A player right-clicked with something: start eating if it is food.
pub fn start_using(server: &Arc<Server>, player: &Arc<Player>, tick: u64) {
    let (held, food, mode) = {
        let s = player.lock();
        let slot = s.held_slot;
        (s.inventory.held(slot).clone(), s.food, s.game_mode)
    };
    if held.is_empty() || matches!(mode, GameMode::Spectator) {
        return;
    }
    let name = crate::items::item_name(server, held.item);
    let Some((_, _)) = food_value(&name) else { return };
    // A full player can still eat a golden apple, nothing else.
    if food >= 20 && !name.contains("golden_apple") {
        return;
    }
    player.lock().eating_since = Some(tick);
}

pub fn stop_using(player: &Arc<Player>) {
    player.lock().eating_since = None;
}

/// Movement: what it costs, and how far there is left to fall.
pub fn moved(server: &Arc<Server>, player: &Arc<Player>, from: (f64, f64, f64), to: (f64, f64, f64), on_ground: bool) {
    let mode = player.lock().game_mode;
    if matches!(mode, GameMode::Creative | GameMode::Spectator) {
        player.lock().fall_distance = 0.0;
        return;
    }
    let walked = ((to.0 - from.0).powi(2) + (to.2 - from.2).powi(2)).sqrt() as f32;
    let in_water = block_name(server, to.0, to.1 + 0.1, to.2).contains("water");
    let landed = {
        let mut s = player.lock();
        if walked > 0.0 {
            // Only sprinting and swimming are hungry work; a walk is free.
            s.exhaustion += if s.sprinting {
                walked * 0.1
            } else if in_water {
                walked * 0.01
            } else {
                0.0
            };
        }
        let dy = (to.1 - from.1) as f32;
        if on_ground || in_water {
            let fallen = s.fall_distance;
            s.fall_distance = 0.0;
            fallen
        } else {
            if dy < 0.0 {
                s.fall_distance -= dy;
            }
            0.0
        }
    };
    if landed > SAFE_FALL && !in_water && server.rules.read().unwrap_or_else(|e| e.into_inner()).game_rule_bool("fallDamage") {
        let cushion = player
            .lock()
            .attributes
            .get("minecraft:safe_fall_distance")
            .copied()
            .unwrap_or(SAFE_FALL as f64) as f32;
        let damage = (landed - cushion.max(SAFE_FALL)).floor();
        if damage > 0.0 {
            hurt(server, player, damage, "fall", None);
        }
    }
}

/// Breaking a block is work.
pub fn mined(player: &Arc<Player>) {
    player.lock().exhaustion += 0.005;
}

/// A player swung at something.
pub fn attack(server: &Arc<Server>, attacker: &Arc<Player>, target_id: i32, tick: u64) {
    let (mode, x, z, sprinting, slot) = {
        let s = attacker.lock();
        (s.game_mode, s.x, s.z, s.sprinting, s.held_slot)
    };
    if matches!(mode, GameMode::Spectator) {
        return;
    }
    // A swing that comes too soon after the last one lands soft, the way
    // the attack indicator promises.
    let charge = {
        let mut s = attacker.lock();
        let since = tick.saturating_sub(s.last_attack_tick);
        s.last_attack_tick = tick;
        if since >= 12 { 1.0 } else { 0.3 + 0.7 * (since as f32 / 12.0) }
    };
    let weapon = {
        let s = attacker.lock();
        let held = s.inventory.held(slot).clone();
        if held.is_empty() {
            1.0
        } else {
            weapon_damage(&crate::items::item_name(server, held.item))
        }
    };
    let damage = weapon * charge + if sprinting && charge > 0.9 { 1.0 } else { 0.0 };
    attacker.lock().exhaustion += 0.1;

    // You can only hit what you can reach; the client has no business
    // swinging at something across the world.
    if let Some(spot) = entity_position(server, target_id) {
        let eyes = attacker.lock().eye_position();
        let creative = matches!(mode, GameMode::Creative);
        if server.anticheat.check_reach(eyes, spot, creative).is_some() {
            return;
        }
    }

    if let Some(target) = server.online_players().into_iter().find(|p| p.entity_id == target_id) {
        if target.uuid == attacker.uuid {
            return;
        }
        if !server.config().server.pvp {
            return;
        }
        hurt_by(server, &target, damage, "player_attack", Some((x, z)), Some(attacker.name().to_owned()));
    } else {
        damage_entity(server, target_id, damage, (x, z));
    }
}

/// Where an entity is, player or not, as whole blocks for the reach check.
fn entity_position(server: &Arc<Server>, entity_id: i32) -> Option<(i32, i32, i32)> {
    if let Some(player) = server.online_players().into_iter().find(|p| p.entity_id == entity_id) {
        let s = player.lock();
        return Some((s.x.floor() as i32, s.y.floor() as i32, s.z.floor() as i32));
    }
    let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    let entity = entities.by_id.get(&entity_id)?;
    Some((entity.x.floor() as i32, entity.y.floor() as i32, entity.z.floor() as i32))
}

/// Hits a mob or other non-player entity.
fn damage_entity(server: &Arc<Server>, entity_id: i32, damage: f32, from: (f64, f64)) {
    let (dead, pos, kind) = {
        let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entity) = entities.by_id.get_mut(&entity_id) else { return };
        if entity.kind == "minecraft:item" {
            return;
        }
        entity.health -= damage;
        let dx = entity.x - from.0;
        let dz = entity.z - from.1;
        let len = (dx * dx + dz * dz).sqrt().max(0.001);
        entity.velocity.0 += dx / len * 0.4;
        entity.velocity.1 = 0.36;
        entity.velocity.2 += dz / len * 0.4;
        (entity.health <= 0.0, (entity.x, entity.y, entity.z), entity.kind.clone())
    };
    let chunk = garnet_protocol::ChunkPos::from_block(pos.0.floor() as i32, pos.2.floor() as i32);
    server.broadcast_near(chunk, &cb::HurtAnimation { entity_id, yaw: 0.0 }, None);
    if dead {
        drop_loot(server, &kind, pos);
        world_entities::despawn(server, entity_id);
    }
}

/// What a dead mob leaves on the ground, from its own loot table.
fn drop_loot(server: &Arc<Server>, kind: &str, pos: (f64, f64, f64)) {
    let tool = crate::loot::Tool {
        item_name: None,
        silk_touch: false,
        fortune: 0,
    };
    let drops = server.mob_loot.drops(kind, &std::collections::BTreeMap::new(), &tool);
    for (name, count) in drops {
        if count <= 0 {
            continue; // a roll of nothing
        }
        let Some(id) = crate::items::item_id(server, &name) else { continue };
        let spread = || (rand::random::<f64>() - 0.5) * 0.2;
        world_entities::drop_item(
            server,
            garnet_protocol::packets::play::items::ItemStack::new(id, count),
            pos.0,
            pos.1 + 0.4,
            pos.2,
            (spread(), 0.15, spread()),
            10,
        );
    }
}

/// Damage from the world, with no one to blame.
pub fn hurt(server: &Arc<Server>, player: &Arc<Player>, amount: f32, kind: &str, from: Option<(f64, f64)>) {
    hurt_by(server, player, amount, kind, from, None);
}

/// Takes health off a player, with armour, effects and the safety window
/// all accounted for. `from` is where the blow came from, for knockback.
pub fn hurt_by(
    server: &Arc<Server>,
    player: &Arc<Player>,
    amount: f32,
    kind: &str,
    from: Option<(f64, f64)>,
    attacker: Option<String>,
) {
    let tick = server.current_tick();
    let (health, mode) = {
        let s = player.lock();
        (s.health, s.game_mode)
    };
    if matches!(mode, GameMode::Creative | GameMode::Spectator) || health <= 0.0 {
        return;
    }
    {
        let s = player.lock();
        if kind != "out_of_world" && tick.saturating_sub(s.last_hurt_tick) < INVULNERABLE_TICKS {
            return;
        }
    }
    let fire = matches!(kind, "in_fire" | "on_fire" | "lava");
    if fire && player.lock().effects.iter().any(|e| e.name.ends_with("fire_resistance")) {
        return;
    }

    let mut left = amount;
    if !matches!(kind, "out_of_world" | "starve" | "drown") {
        // Armour takes a flat share; twenty points is the vanilla cap.
        let points = armour_points(server, player).min(20.0);
        left *= 1.0 - points / 25.0;
    }
    if let Some(resistance) = player.lock().effects.iter().find(|e| e.name.ends_with("resistance")) {
        left *= 1.0 - ((resistance.amplifier + 1) as f32 * 0.2).min(1.0);
    }

    let health = {
        let mut s = player.lock();
        s.health = (s.health - left).max(0.0);
        s.last_hurt_tick = tick;
        s.exhaustion += 0.1;
        s.health
    };

    let damage_type = server.data.dynamic.id_of("damage_type", kind).unwrap_or(0);
    let hurt_packet = cb::HurtAnimation {
        entity_id: player.entity_id,
        yaw: 0.0,
    };
    player.send(&cb::DamageEvent {
        entity_id: player.entity_id,
        damage_type,
    });
    player.send(&hurt_packet);
    let chunk = player.lock().chunk();
    server.broadcast_near(chunk, &hurt_packet, Some(player.uuid));

    if let Some((ax, az)) = from {
        let (px, pz) = {
            let s = player.lock();
            (s.x, s.z)
        };
        let (dx, dz) = (px - ax, pz - az);
        let len = (dx * dx + dz * dz).sqrt().max(0.001);
        player.send(&cb::SetEntityMotion {
            entity_id: player.entity_id,
            x: dx / len * 0.4,
            y: 0.36,
            z: dz / len * 0.4,
        });
    }

    if health <= 0.0 {
        die(server, player, kind, attacker);
    } else {
        sync(player);
    }
}

/// The end of a life: the death screen, the message, and what is left behind.
fn die(server: &Arc<Server>, player: &Arc<Player>, kind: &str, attacker: Option<String>) {
    let message = Text::new(death_message(player.name(), kind, attacker.as_deref()));
    let keep = server
        .rules
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .game_rule_bool("keepInventory");
    if !keep {
        let (x, y, z) = {
            let s = player.lock();
            (s.x, s.y, s.z)
        };
        let dropped: Vec<_> = {
            let mut s = player.lock();
            let items = s.inventory.take_all();
            s.xp_level = 0;
            s.xp_total = 0;
            items
        };
        for stack in dropped {
            let spread = |v: f64| v + (rand::random::<f64>() - 0.5) * 0.2;
            world_entities::drop_item(server, stack, x, y + 0.6, z, (spread(0.0), 0.2, spread(0.0)), 40);
        }
        crate::items::sync_inventory(player);
    }
    crate::vanilla_commands::kill_player(server, player, message);
}

/// Back on your feet: full health, a half-full stomach, and a clean slate.
pub fn respawned(server: &Arc<Server>, player: &Arc<Player>) {
    {
        let mut s = player.lock();
        s.health = 20.0;
        s.food = 20;
        s.saturation = 5.0;
        s.exhaustion = 0.0;
        s.air = MAX_AIR;
        s.fire_ticks = 0;
        s.fall_distance = 0.0;
        s.eating_since = None;
    }
    sync(player);
    send_flags(server, player);
}

/// Tells the client what its hearts and hunger bar should look like.
pub fn sync(player: &Arc<Player>) {
    let (health, food, saturation) = {
        let s = player.lock();
        (s.health, s.food, s.saturation)
    };
    player.send(&cb::SetHealth {
        health,
        food,
        saturation,
    });
}

/// Burning shows on the player; so do the bubbles running out.
fn send_flags(server: &Arc<Server>, player: &Arc<Player>) {
    let (burning, air) = {
        let s = player.lock();
        (s.fire_ticks > 0, s.air)
    };
    let was = player.lock().shown_burning;
    if was == burning {
        return;
    }
    player.lock().shown_burning = burning;
    let packet = crate::entities::flags_packet(player, burning, air);
    player.send(&packet);
    let chunk = player.lock().chunk();
    server.broadcast_near(chunk, &packet, Some(player.uuid));
}

fn max_health(state: &crate::player::PlayerState) -> f32 {
    state
        .attributes
        .get("minecraft:max_health")
        .copied()
        .unwrap_or(20.0) as f32
}

/// Armour points from what the player is wearing.
fn armour_points(server: &Arc<Server>, player: &Arc<Player>) -> f32 {
    let worn: Vec<String> = {
        let s = player.lock();
        (5..9)
            .map(|slot| {
                let stack = &s.inventory.slots[slot];
                if stack.is_empty() {
                    String::new()
                } else {
                    crate::items::item_name(server, stack.item)
                }
            })
            .collect()
    };
    worn.iter().map(|name| armour_value(name)).sum()
}

/// Vanilla's armour points, by material and piece.
fn armour_value(name: &str) -> f32 {
    let piece = |helmet: f32, chest: f32, legs: f32, boots: f32| {
        if name.ends_with("_helmet") {
            helmet
        } else if name.ends_with("_chestplate") {
            chest
        } else if name.ends_with("_leggings") {
            legs
        } else if name.ends_with("_boots") {
            boots
        } else {
            0.0
        }
    };
    if name.contains("leather") {
        piece(1.0, 3.0, 2.0, 1.0)
    } else if name.contains("golden") {
        piece(2.0, 5.0, 3.0, 1.0)
    } else if name.contains("chainmail") {
        piece(2.0, 5.0, 4.0, 1.0)
    } else if name.contains("iron") {
        piece(2.0, 6.0, 5.0, 2.0)
    } else if name.contains("diamond") {
        piece(3.0, 8.0, 6.0, 3.0)
    } else if name.contains("netherite") {
        piece(3.0, 8.0, 6.0, 3.0)
    } else if name == "minecraft:turtle_helmet" {
        2.0
    } else {
        0.0
    }
}

/// What a hit with this in hand is worth, bare hands being one.
fn weapon_damage(name: &str) -> f32 {
    let tier = |sword: f32, axe: f32| {
        if name.ends_with("_sword") {
            sword
        } else if name.ends_with("_axe") {
            axe
        } else if name.ends_with("_pickaxe") {
            sword - 2.0
        } else if name.ends_with("_shovel") {
            sword - 2.5
        } else {
            1.0
        }
    };
    if name.contains("wooden") || name.contains("golden") {
        tier(4.0, 7.0)
    } else if name.contains("stone") {
        tier(5.0, 9.0)
    } else if name.contains("iron") {
        tier(6.0, 9.0)
    } else if name.contains("diamond") {
        tier(7.0, 9.0)
    } else if name.contains("netherite") {
        tier(8.0, 10.0)
    } else if name == "minecraft:trident" {
        9.0
    } else {
        1.0
    }
}

/// Hunger and saturation restored, for the food worth carrying.
fn food_value(name: &str) -> Option<(i32, f32)> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    let value = match short {
        "apple" => (4, 2.4),
        "golden_apple" | "enchanted_golden_apple" => (4, 9.6),
        "bread" => (5, 6.0),
        "cookie" => (2, 0.4),
        "melon_slice" => (2, 1.2),
        "sweet_berries" | "glow_berries" => (2, 0.4),
        "carrot" => (3, 3.6),
        "golden_carrot" => (6, 14.4),
        "potato" => (1, 0.6),
        "baked_potato" => (5, 6.0),
        "beetroot" => (1, 1.2),
        "dried_kelp" => (1, 0.6),
        "beef" | "porkchop" | "mutton" => (3, 1.8),
        "cooked_beef" | "cooked_porkchop" | "cooked_mutton" => (8, 12.8),
        "chicken" => (2, 1.2),
        "cooked_chicken" => (6, 7.2),
        "rabbit" => (3, 1.8),
        "cooked_rabbit" => (5, 6.0),
        "cod" | "salmon" => (2, 0.4),
        "cooked_cod" => (5, 6.0),
        "cooked_salmon" => (6, 9.6),
        "tropical_fish" => (1, 0.2),
        "rotten_flesh" => (4, 0.8),
        "spider_eye" => (2, 3.2),
        "pumpkin_pie" => (8, 4.8),
        "mushroom_stew" | "beetroot_soup" | "rabbit_stew" | "suspicious_stew" => (6, 7.2),
        _ => return None,
    };
    Some(value)
}

fn death_message(name: &str, kind: &str, attacker: Option<&str>) -> String {
    match kind {
        "fall" => format!("{name} fell from a high place"),
        "drown" => format!("{name} drowned"),
        "starve" => format!("{name} starved to death"),
        "lava" => format!("{name} tried to swim in lava"),
        "in_fire" | "on_fire" => format!("{name} went up in flames"),
        "out_of_world" => format!("{name} fell out of the world"),
        "player_attack" => match attacker {
            Some(killer) => format!("{name} was slain by {killer}"),
            None => format!("{name} was slain"),
        },
        _ => format!("{name} died"),
    }
}

/// The blocks at two spots, by full name, under one lock on the world.
fn blocks_at(server: &Server, first: (f64, f64, f64), second: (f64, f64, f64)) -> (String, String) {
    let at = |p: (f64, f64, f64)| BlockPos::new(p.0.floor() as i32, p.1.floor() as i32, p.2.floor() as i32);
    let (a, b) = {
        let mut world = server.world();
        (world.get_block(at(first)).ok(), world.get_block(at(second)).ok())
    };
    let name = |state: Option<u32>| {
        state
            .and_then(|s| server.data.blocks.block_of_state(s as i32))
            .map(|b| b.name.clone())
            .unwrap_or_default()
    };
    (name(a), name(b))
}

/// The block at a spot, by full name, or an empty string outside the world.
fn block_name(server: &Server, x: f64, y: f64, z: f64) -> String {
    blocks_at(server, (x, y, z), (x, y, z)).0
}
