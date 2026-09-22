//! Tools and armour wearing out.
//!
//! Every swing of a sword and every block broken with a tool costs one
//! point, armour loses one for every four hearts its wearer takes, and
//! when a thing runs out of points it breaks in the hand. Unbreaking is
//! honoured: each level gives the item a chance to shrug the wear off.

use crate::player::Player;
use garnet_protocol::packets::play::items::{component, damage_in, with_component, ItemStack};
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::PacketWriter;
use crate::server::Server;
use std::sync::Arc;

/// How much wear a thing takes before it breaks, from the game's own
/// defaults.
pub fn max_damage(data: &garnet_data::GameData, item: &str) -> i32 {
    if let Some(defaults) = data.item_components.get(item) {
        return defaults.max_damage;
    }
    wear_guess(item)
}

/// What we fall back on when the item component report is missing, as it is
/// for data prepared by an older Garnet.
fn wear_guess(item: &str) -> i32 {
    let short = item.strip_prefix("minecraft:").unwrap_or(item);
    let tool = |wood: i32, stone: i32, iron: i32, gold: i32, diamond: i32, netherite: i32| {
        if short.starts_with("wooden_") {
            wood
        } else if short.starts_with("stone_") {
            stone
        } else if short.starts_with("iron_") {
            iron
        } else if short.starts_with("golden_") {
            gold
        } else if short.starts_with("diamond_") {
            diamond
        } else if short.starts_with("netherite_") {
            netherite
        } else {
            0
        }
    };
    if short.ends_with("_sword")
        || short.ends_with("_pickaxe")
        || short.ends_with("_axe")
        || short.ends_with("_shovel")
        || short.ends_with("_hoe")
    {
        return tool(59, 131, 250, 32, 1561, 2031);
    }
    if short.ends_with("_helmet") {
        return tool(55, 0, 165, 77, 363, 407);
    }
    if short.ends_with("_chestplate") {
        return tool(80, 0, 240, 112, 528, 592);
    }
    if short.ends_with("_leggings") {
        return tool(75, 0, 225, 105, 495, 555);
    }
    if short.ends_with("_boots") {
        return tool(65, 0, 195, 91, 429, 481);
    }
    match short {
        "bow" => 384,
        "crossbow" => 465,
        "trident" => 250,
        "shield" => 336,
        "fishing_rod" => 64,
        "flint_and_steel" => 64,
        "shears" => 238,
        "elytra" => 432,
        "turtle_helmet" => 275,
        "chainmail_helmet" => 165,
        "chainmail_chestplate" => 240,
        "chainmail_leggings" => 225,
        "chainmail_boots" => 195,
        _ => 0,
    }
}

/// Wears an item by `amount`, returning what is left of it: `None` when it
/// has broken.
fn wear(server: &Arc<Server>, stack: &ItemStack, amount: i32) -> Option<ItemStack> {
    let name = crate::items::item_name(server, stack.item);
    let limit = max_damage(&server.data, &name);
    if limit == 0 || amount <= 0 {
        return Some(stack.clone());
    }
    // Unbreaking: each level is a chance the wear does not count.
    let unbreaking = enchantment_level(server, stack, "minecraft:unbreaking");
    let mut taken = 0;
    for _ in 0..amount {
        if unbreaking > 0 && rand::random_range(0..unbreaking + 1) > 0 {
            continue;
        }
        taken += 1;
    }
    if taken == 0 {
        return Some(stack.clone());
    }
    let damage = damage_in(&stack.patch).unwrap_or(0) + taken;
    if damage >= limit {
        return None;
    }
    let mut value = PacketWriter::new();
    value.write_varint(damage);
    let Some(patch) = with_component(&stack.patch, component::DAMAGE, value.into_inner()) else {
        // A patch we cannot read is left exactly as it was.
        tracing::warn!("could not record wear on {name}; leaving it alone");
        return Some(stack.clone());
    };
    Some(ItemStack {
        item: stack.item,
        count: stack.count,
        patch,
    })
}

fn enchantment_level(server: &Arc<Server>, stack: &ItemStack, name: &str) -> i32 {
    // Enchantments are data-driven, so their ids come from the data pack.
    let Some(id) = server.data.id_of("enchantment", name) else { return 0 };
    garnet_protocol::packets::play::items::enchantments_in(&stack.patch)
        .unwrap_or_default()
        .into_iter()
        .find(|(enchantment, _)| *enchantment == id)
        .map(|(_, level)| level)
        .unwrap_or(0)
}

/// Wears the item a player is holding, and tells them if it breaks.
pub fn use_held(server: &Arc<Server>, player: &Arc<Player>, amount: i32) {
    if matches!(
        player.lock().game_mode,
        garnet_protocol::packets::play::GameMode::Creative | garnet_protocol::packets::play::GameMode::Spectator
    ) {
        return;
    }
    let (slot, stack) = {
        let s = player.lock();
        let held = s.held_slot;
        (
            crate::inventory::HOTBAR_START + held.clamp(0, 8) as usize,
            s.inventory.held(held).clone(),
        )
    };
    if stack.is_empty() {
        return;
    }
    let left = wear(server, &stack, amount);
    let broke = left.is_none();
    {
        let mut s = player.lock();
        s.inventory.slots[slot] = left.unwrap_or(ItemStack::EMPTY);
    }
    crate::items::sync_inventory(player);
    if broke {
        break_sound(server, player);
    }
}

/// Wears whatever armour a player has on, as taking a hit does.
pub fn take_hit(server: &Arc<Server>, player: &Arc<Player>, damage: f32) {
    if matches!(
        player.lock().game_mode,
        garnet_protocol::packets::play::GameMode::Creative | garnet_protocol::packets::play::GameMode::Spectator
    ) {
        return;
    }
    // Vanilla: a point of wear for every four hearts' worth, at least one.
    let amount = ((damage / 4.0).floor() as i32).max(1);
    let mut broke = false;
    {
        let worn: Vec<(usize, ItemStack)> = {
            let s = player.lock();
            (5..9)
                .filter(|slot| !s.inventory.slots[*slot].is_empty())
                .map(|slot| (slot, s.inventory.slots[slot].clone()))
                .collect()
        };
        for (slot, stack) in worn {
            let left = wear(server, &stack, amount);
            broke |= left.is_none();
            let mut s = player.lock();
            s.inventory.slots[slot] = left.unwrap_or(ItemStack::EMPTY);
        }
    }
    crate::items::sync_inventory(player);
    if broke {
        break_sound(server, player);
    }
}

fn break_sound(server: &Arc<Server>, player: &Arc<Player>) {
    let Some(name) = garnet_protocol::Identifier::parse("minecraft:entity.item.break") else { return };
    let registry_id = server.data.registries.id_of("sound_event", &name.to_string());
    let (x, y, z) = {
        let s = player.lock();
        (s.x, s.y, s.z)
    };
    player.send(&cb::Sound {
        name,
        registry_id,
        source: cb::SoundSource::Players,
        x,
        y,
        z,
        volume: 0.8,
        pitch: 0.9,
        seed: rand::random(),
    });
}
