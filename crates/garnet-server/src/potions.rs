//! Potions: what is in a bottle, and what drinking it does.
//!
//! Which potion a bottle holds is a component on the item, and the effects
//! each one gives are the game's own numbers, taken from its potion list.
//! Drinking works like eating: it takes the same while, then the effects
//! land and the bottle is left in the hand.

use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::items::{potion_in, ItemStack};
use std::sync::Arc;

/// Every potion, with the effects it gives: the effect, how long it lasts
/// in ticks, and how strong it is. Straight from the game's potion list.
const POTIONS: &[(&str, &[(&str, i32, i32)])] = &[
    ("water", &[]),
    ("mundane", &[]),
    ("thick", &[]),
    ("awkward", &[]),
    ("night_vision", &[("night_vision", 3600, 0)]),
    ("long_night_vision", &[("night_vision", 9600, 0)]),
    ("invisibility", &[("invisibility", 3600, 0)]),
    ("long_invisibility", &[("invisibility", 9600, 0)]),
    ("leaping", &[("jump_boost", 3600, 0)]),
    ("long_leaping", &[("jump_boost", 9600, 0)]),
    ("strong_leaping", &[("jump_boost", 1800, 1)]),
    ("fire_resistance", &[("fire_resistance", 3600, 0)]),
    ("long_fire_resistance", &[("fire_resistance", 9600, 0)]),
    ("swiftness", &[("speed", 3600, 0)]),
    ("long_swiftness", &[("speed", 9600, 0)]),
    ("strong_swiftness", &[("speed", 1800, 1)]),
    ("slowness", &[("slowness", 1800, 0)]),
    ("long_slowness", &[("slowness", 4800, 0)]),
    ("strong_slowness", &[("slowness", 400, 3)]),
    ("turtle_master", &[("slowness", 400, 3), ("resistance", 400, 2)]),
    ("long_turtle_master", &[("slowness", 800, 3), ("resistance", 800, 2)]),
    ("strong_turtle_master", &[("slowness", 400, 5), ("resistance", 400, 3)]),
    ("water_breathing", &[("water_breathing", 3600, 0)]),
    ("long_water_breathing", &[("water_breathing", 9600, 0)]),
    ("healing", &[("instant_health", 1, 0)]),
    ("strong_healing", &[("instant_health", 1, 1)]),
    ("harming", &[("instant_damage", 1, 0)]),
    ("strong_harming", &[("instant_damage", 1, 1)]),
    ("poison", &[("poison", 900, 0)]),
    ("long_poison", &[("poison", 1800, 0)]),
    ("strong_poison", &[("poison", 432, 1)]),
    ("regeneration", &[("regeneration", 900, 0)]),
    ("long_regeneration", &[("regeneration", 1800, 0)]),
    ("strong_regeneration", &[("regeneration", 450, 1)]),
    ("strength", &[("strength", 3600, 0)]),
    ("long_strength", &[("strength", 9600, 0)]),
    ("strong_strength", &[("strength", 1800, 1)]),
    ("weakness", &[("weakness", 1800, 0)]),
    ("long_weakness", &[("weakness", 4800, 0)]),
    ("luck", &[("luck", 6000, 0)]),
    ("slow_falling", &[("slow_falling", 1800, 0)]),
    ("long_slow_falling", &[("slow_falling", 4800, 0)]),
    ("wind_charged", &[("wind_charged", 3600, 0)]),
    ("weaving", &[("weaving", 3600, 0)]),
    ("oozing", &[("oozing", 3600, 0)]),
    ("infested", &[("infested", 3600, 0)]),
];

/// The effects a potion gives, by its name in `minecraft:potion`.
pub fn effects_of(potion: &str) -> &'static [(&'static str, i32, i32)] {
    let short = potion.strip_prefix("minecraft:").unwrap_or(potion);
    POTIONS
        .iter()
        .find(|(name, _)| *name == short)
        .map(|(_, effects)| *effects)
        .unwrap_or(&[])
}

/// How far a player can reach water with a bottle.
const FILL_REACH: f64 = 5.0;

/// A glass bottle held out at water fills with it. Vanilla looks for the
/// water itself rather than the block clicked, so this follows the line of
/// sight until it finds a source.
pub fn fill_bottle(server: &Arc<Server>, player: &Arc<Player>) -> bool {
    let (held, slot) = {
        let s = player.lock();
        let slot = s.held_slot;
        (s.inventory.held(slot).clone(), slot)
    };
    if held.is_empty() || crate::items::item_name(server, held.item) != "minecraft:glass_bottle" {
        return false;
    }
    let Some(pos) = water_in_sight(server, player) else {
        return false;
    };
    let _ = pos;
    let Some(bottle) = crate::items::item_id(server, "minecraft:potion") else {
        return false;
    };
    let Some(water) = server.data.registries.id_of("potion", "minecraft:water") else {
        return false;
    };
    let filled = ItemStack {
        item: bottle,
        count: 1,
        patch: garnet_protocol::packets::play::items::PatchBuilder::default()
            .potion(water)
            .build(),
    };
    let creative = matches!(player.lock().game_mode, garnet_protocol::packets::play::GameMode::Creative);
    let over = {
        let mut s = player.lock();
        if !creative {
            let stack = s.inventory.held_mut(slot);
            stack.count -= 1;
            if stack.count <= 0 {
                *stack = ItemStack::EMPTY;
            }
        }
        let stack = s.inventory.held_mut(slot);
        if stack.is_empty() {
            *stack = filled;
            ItemStack::EMPTY
        } else {
            s.inventory.add(filled, 1)
        }
    };
    if !over.is_empty() {
        let (x, y, z, yaw, pitch) = {
            let s = player.lock();
            (s.x, s.y, s.z, s.yaw, s.pitch)
        };
        crate::items::throw_from(server, over, yaw, pitch, x, y, z);
    }
    crate::items::sync_inventory(player);
    true
}

/// The first water source along the player's line of sight.
fn water_in_sight(server: &Arc<Server>, player: &Arc<Player>) -> Option<garnet_protocol::BlockPos> {
    let (eyes, yaw, pitch) = {
        let s = player.lock();
        (s.eye_position(), s.yaw, s.pitch)
    };
    let yaw = (yaw as f64).to_radians();
    let pitch = (pitch as f64).to_radians();
    let direction = (-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos());
    let mut step = 0.0;
    while step <= FILL_REACH {
        let point = (
            eyes.0 + direction.0 * step,
            eyes.1 + direction.1 * step,
            eyes.2 + direction.2 * step,
        );
        let pos = garnet_protocol::BlockPos::new(point.0.floor() as i32, point.1.floor() as i32, point.2.floor() as i32);
        if is_water_source(server, pos) {
            return Some(pos);
        }
        step += 0.2;
    }
    None
}

/// Only a full source fills a bottle, not water on its way somewhere.
fn is_water_source(server: &Arc<Server>, pos: garnet_protocol::BlockPos) -> bool {
    let Ok(state) = server.world().get_block(pos) else {
        return false;
    };
    let Some(block) = server.data.blocks.state(state as i32) else {
        return false;
    };
    let Some(name) = server.data.blocks.block_of_state(state as i32).map(|b| b.name.as_str()) else {
        return false;
    };
    let level_zero = block.properties.get("level").map(|l| l == "0").unwrap_or(false);
    let waterlogged = block.properties.get("waterlogged").map(|w| w == "true").unwrap_or(false);
    (name == "minecraft:water" && level_zero) || waterlogged
}

/// Whether this item is something a player drinks.
pub fn is_drink(item: &str) -> bool {
    matches!(item, "minecraft:potion" | "minecraft:milk_bucket")
}

/// Finishes a drink: the effects land, and whatever the bottle leaves
/// behind goes back in the hand.
pub fn drink(server: &Arc<Server>, player: &Arc<Player>, held: &ItemStack) {
    let name = crate::items::item_name(server, held.item);
    if name == "minecraft:milk_bucket" {
        clear_effects(server, player);
    } else {
        for (effect, duration, amplifier) in potion_effects(server, held) {
            // Healing and harming are not effects that sit on a player:
            // they happen the moment the bottle is empty.
            match effect.as_str() {
                "minecraft:instant_health" => {
                    let heal = 4.0 * (1 << amplifier.clamp(0, 6)) as f32;
                    crate::survival::heal(server, player, heal);
                    continue;
                }
                "minecraft:instant_damage" => {
                    let harm = 6.0 * (1 << amplifier.clamp(0, 6)) as f32;
                    crate::survival::hurt(server, player, harm, "magic", None);
                    continue;
                }
                _ => {}
            }
            let Some(id) = server.data.id_of("mob_effect", &effect) else {
                continue;
            };
            crate::vanilla_commands::give_effect(server, player, &effect, id, amplifier, duration, true);
        }
    }
    spend(server, player, &name);
}

/// What drinking this bottle would give.
fn potion_effects(server: &Arc<Server>, held: &ItemStack) -> Vec<(String, i32, i32)> {
    let Some(id) = potion_in(&held.patch) else { return Vec::new() };
    let Some(potion) = server.data.registries.name_of("potion", id) else {
        return Vec::new();
    };
    effects_of(potion)
        .iter()
        .map(|(effect, duration, amplifier)| (format!("minecraft:{effect}"), *duration, *amplifier))
        .collect()
}

/// Takes the drink out of the hand, leaving the empty bottle behind.
fn spend(server: &Arc<Server>, player: &Arc<Player>, item: &str) {
    if matches!(
        player.lock().game_mode,
        garnet_protocol::packets::play::GameMode::Creative | garnet_protocol::packets::play::GameMode::Spectator
    ) {
        return;
    }
    let left = server
        .data
        .item_components
        .use_remainder(item)
        .and_then(|name| crate::items::item_id(server, name));
    let over = {
        let mut s = player.lock();
        let slot = s.held_slot;
        let stack = s.inventory.held_mut(slot);
        stack.count -= 1;
        let emptied = stack.count <= 0;
        if emptied {
            *stack = ItemStack::EMPTY;
        }
        match left {
            // The bottle takes the hand back when nothing is left of the
            // drink; otherwise it has to find room.
            Some(id) if emptied => {
                *stack = ItemStack::new(id, 1);
                ItemStack::EMPTY
            }
            Some(id) => s.inventory.add(ItemStack::new(id, 1), 64),
            None => ItemStack::EMPTY,
        }
    };
    if !over.is_empty() {
        let (x, y, z, yaw, pitch) = {
            let s = player.lock();
            (s.x, s.y, s.z, s.yaw, s.pitch)
        };
        crate::items::throw_from(server, over, yaw, pitch, x, y, z);
    }
    crate::items::sync_inventory(player);
}

/// Milk washes everything off.
fn clear_effects(server: &Arc<Server>, player: &Arc<Player>) {
    let gone: Vec<crate::player::ActiveEffect> = {
        let mut s = player.lock();
        std::mem::take(&mut s.effects)
    };
    for effect in gone {
        player.send(&garnet_protocol::packets::play::clientbound::RemoveMobEffect {
            entity_id: player.entity_id,
            effect: effect.id,
        });
    }
    let _ = server;
}
