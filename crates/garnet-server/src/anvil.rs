//! The anvil: mending gear, joining enchantments and naming things, paid
//! for in levels.
//!
//! Vanilla's rules, as far as the components we can read allow. Two of the
//! same thing become one with both their lives and both their
//! enchantments; a material mends a quarter of a tool each time; a name
//! costs a level on its own. Every piece of work a thing has had makes the
//! next dearer, and past forty levels it is too expensive to touch.

use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::{
    component, components_in, enchantments_in, number_in, stored_enchantments_in, ItemStack, PatchBuilder,
};
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{BlockPos, PacketWriter, Text};
use std::sync::Arc;

pub const FIRST: usize = 0;
pub const SECOND: usize = 1;
pub const RESULT: usize = 2;
pub const SLOTS: usize = 3;
/// Past this many levels vanilla will not do the work at all.
const TOO_EXPENSIVE: i32 = 40;
/// A quarter of a tool mended per unit of its material.
const MATERIAL_SHARE: f32 = 0.25;

/// What the anvil would make of what is in it, and what it would cost.
pub struct Work {
    pub result: ItemStack,
    pub cost: i32,
    /// How many of the second stack the work uses up.
    pub spent_second: i32,
}

/// Works out the result of the two inputs and the name asked for.
pub fn plan(server: &Arc<Server>, inputs: &[ItemStack], name: Option<&str>) -> Option<Work> {
    let first = inputs.get(FIRST)?.clone();
    if first.is_empty() {
        return None;
    }
    let second = inputs.get(SECOND).cloned().unwrap_or(ItemStack::EMPTY);
    let first_name = crate::items::item_name(server, first.item);
    let limit = crate::durability::max_damage(&server.data, &first_name);

    // Every repair a thing has had makes the next one dearer.
    let mut cost = number_in(&first.patch, component::REPAIR_COST).unwrap_or(0)
        + number_in(&second.patch, component::REPAIR_COST).unwrap_or(0);
    let mut damage = number_in(&first.patch, component::DAMAGE).unwrap_or(0);
    let first_is_book = first_name == "minecraft:enchanted_book";
    let mut enchantments = carried(&first.patch, first_is_book);
    let mut worked = false;
    let mut spent_second = if second.is_empty() { 0 } else { second.count };

    if !second.is_empty() {
        let second_name = crate::items::item_name(server, second.item);
        let from_book = second_name == "minecraft:enchanted_book";
        // An enchanted book joins whatever it is put against; two of the
        // same thing join as well.
        if from_book || second.item == first.item {
            if let Some(added) = join_enchantments(server, &first_name, &mut enchantments, &second, from_book) {
                cost += added;
                worked = true;
            }
        }
        if second.item == first.item && limit > 0 {
            // Two of the same: their lives add up, with a little over.
            let other_damage = number_in(&second.patch, component::DAMAGE).unwrap_or(0);
            let left = (limit - other_damage).max(0) + (limit as f32 * 0.12) as i32;
            let before = damage;
            damage = (damage - left).max(0);
            if damage < before {
                cost += 2;
                worked = true;
            }
        } else if limit > 0 && mends(server, &first_name, &second_name) {
            // A material mends a quarter at a time.
            let per_unit = (limit as f32 * MATERIAL_SHARE) as i32;
            let mut used = 0;
            while used < second.count && damage > 0 {
                damage = (damage - per_unit).max(0);
                used += 1;
            }
            if used > 0 {
                cost += used;
                worked = true;
                spent_second = used;
            }
        } else if !worked {
            return None; // nothing these two can do for each other
        }
    }

    // Renaming is a level on its own.
    let mut patch = PatchBuilder::default();
    if damage > 0 {
        patch = patch.damage(damage);
    }
    if !enchantments.is_empty() {
        // A book keeps what it holds in a pocket of its own; everything
        // else wears its enchantments.
        patch = match first_is_book {
            true => patch.stored_enchantments(&enchantments),
            false => patch.enchantments(&enchantments),
        };
    }
    let renamed = match name {
        Some(text) if !text.is_empty() => {
            patch = patch.custom_name(&Text::new(text));
            cost += 1;
            true
        }
        _ => {
            // Keep whatever name it already had.
            if let Some((added, _)) = components_in(&first.patch) {
                if let Some((_, data)) = added.iter().find(|(id, _)| *id == component::CUSTOM_NAME) {
                    patch = patch.raw(component::CUSTOM_NAME, data.clone());
                }
            }
            false
        }
    };
    if !worked && !renamed {
        return None;
    }
    // What the next piece of work will cost on top.
    let prior = number_in(&first.patch, component::REPAIR_COST)
        .unwrap_or(0)
        .max(number_in(&second.patch, component::REPAIR_COST).unwrap_or(0));
    patch = patch.raw(component::REPAIR_COST, {
        let mut w = PacketWriter::new();
        w.write_varint(prior * 2 + 1);
        w.into_inner()
    });

    Some(Work {
        result: ItemStack {
            item: first.item,
            count: 1,
            patch: patch.build(),
        },
        cost: cost.max(1),
        spent_second,
    })
}

/// Joins the second item's enchantments into the first's, following the
/// rules the data pack sets: an enchantment only goes on an item it belongs
/// on, two of a level make the next one up, and nothing joins something it
/// clashes with. Returns what the work adds to the bill.
fn join_enchantments(
    server: &Arc<Server>,
    target: &str,
    enchantments: &mut Vec<(i32, i32)>,
    second: &ItemStack,
    from_book: bool,
) -> Option<i32> {
    let offered = carried(&second.patch, from_book);
    if offered.is_empty() {
        return None;
    }
    // A book takes anything; everything else only what it is meant for.
    let target_is_book = target == "minecraft:book" || target == "minecraft:enchanted_book";
    let mut cost = 0;
    for (enchantment, level) in offered {
        if !target_is_book && !server.enchantments.goes_on(server, enchantment, target) {
            continue;
        }
        match enchantments.iter_mut().find(|(existing, _)| *existing == enchantment) {
            Some(slot) => {
                // Two of a level make the next one up, as vanilla does.
                let merged = if slot.1 == level { level + 1 } else { slot.1.max(level) };
                let merged = merged.min(server.enchantments.max_level(enchantment));
                if merged > slot.1 {
                    cost += server.enchantments.anvil_cost(enchantment, merged, from_book);
                    slot.1 = merged;
                }
            }
            None => {
                if enchantments
                    .iter()
                    .any(|(existing, _)| server.enchantments.clash(server, *existing, enchantment))
                {
                    continue; // it will not share the item with what is there
                }
                cost += server.enchantments.anvil_cost(enchantment, level, from_book);
                enchantments.push((enchantment, level));
            }
        }
    }
    (cost > 0).then_some(cost)
}

/// The enchantments a stack carries, wherever it keeps them.
fn carried(patch: &[u8], is_book: bool) -> Vec<(i32, i32)> {
    if is_book {
        return stored_enchantments_in(patch).unwrap_or_default();
    }
    enchantments_in(patch).unwrap_or_default()
}

/// Whether this material mends that tool. The game says which items mend
/// what, as a tag on the tool itself.
fn mends(server: &Arc<Server>, tool: &str, material: &str) -> bool {
    if let Some(wanted) = server.data.item_components.repairable(tool) {
        return match wanted.strip_prefix('#') {
            Some(tag) => server
                .data
                .tags
                .members("minecraft:item", tag)
                .zip(server.data.registries.id_of("item", material))
                .is_some_and(|(members, id)| members.contains(&id)),
            None => wanted == material,
        };
    }
    mends_guess(tool, material)
}

/// What we fall back on when the item component report is missing, as it is
/// for data prepared by an older Garnet.
fn mends_guess(tool: &str, material: &str) -> bool {
    let tool = tool.strip_prefix("minecraft:").unwrap_or(tool);
    let material = material.strip_prefix("minecraft:").unwrap_or(material);
    let wants = if tool.starts_with("wooden_") {
        "planks"
    } else if tool.starts_with("stone_") {
        "cobblestone"
    } else if tool.starts_with("iron_") || tool.starts_with("chainmail_") {
        "iron_ingot"
    } else if tool.starts_with("golden_") {
        "gold_ingot"
    } else if tool.starts_with("diamond_") {
        "diamond"
    } else if tool.starts_with("netherite_") {
        "netherite_ingot"
    } else if tool == "turtle_helmet" {
        "turtle_scute"
    } else if tool == "elytra" {
        "phantom_membrane"
    } else {
        return false;
    };
    material == wants || (wants == "planks" && material.ends_with("_planks"))
}

/// Opens an anvil for a player.
pub fn open(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos) -> bool {
    let window_id = {
        let mut s = player.lock();
        s.next_window_id = s.next_window_id % 100 + 1;
        let id = s.next_window_id;
        s.anvil = vec![ItemStack::EMPTY; SLOTS];
        s.anvil_name = None;
        s.container = Some(crate::containers::OpenContainer {
            pos,
            window_id: id,
            size: SLOTS,
            kind: crate::containers::Kind::Anvil,
        });
        id
    };
    let menu_type = server.data.registries.id_of("menu", "minecraft:anvil").unwrap_or(0);
    player.send(&cb::OpenScreen {
        window_id,
        menu_type,
        title: Text::new("Repair & Name"),
    });
    let slots = player.lock().anvil.clone();
    crate::containers::send_window(server, player, &slots);
    true
}

/// The player typed a new name into the anvil.
pub fn rename(server: &Arc<Server>, player: &Arc<Player>, name: String) {
    {
        let mut s = player.lock();
        s.anvil_name = Some(name);
    }
    refresh(server, player);
}

/// Recomputes the result slot and tells the client what it would cost.
pub fn refresh(server: &Arc<Server>, player: &Arc<Player>) {
    let Some(open) = player.lock().container.clone() else { return };
    if open.kind != crate::containers::Kind::Anvil {
        return;
    }
    let (inputs, name) = {
        let s = player.lock();
        (s.anvil.clone(), s.anvil_name.clone())
    };
    let work = plan(server, &inputs, name.as_deref());
    let (result, cost) = match work {
        Some(work) => (work.result, work.cost),
        None => (ItemStack::EMPTY, 0),
    };
    {
        let mut s = player.lock();
        if s.anvil.len() > RESULT {
            s.anvil[RESULT] = result;
        }
    }
    player.send(&cb::ContainerSetData {
        window_id: open.window_id,
        property: 0,
        value: cost,
    });
    let slots = player.lock().anvil.clone();
    crate::containers::send_window(server, player, &slots);
}

/// The player took the result: charge them, hand it over, and spend the
/// inputs.
pub fn take(server: &Arc<Server>, player: &Arc<Player>) -> bool {
    let (inputs, name, mode) = {
        let s = player.lock();
        (s.anvil.clone(), s.anvil_name.clone(), s.game_mode)
    };
    let Some(work) = plan(server, &inputs, name.as_deref()) else { return false };
    let free = matches!(mode, GameMode::Creative);
    if !free {
        if work.cost >= TOO_EXPENSIVE {
            return false;
        }
        if !crate::experience::take_levels(player, work.cost) {
            return false;
        }
    }
    {
        let mut s = player.lock();
        s.anvil[FIRST] = ItemStack::EMPTY;
        // A material is only spent as far as it was needed.
        let left = s.anvil[SECOND].count - work.spent_second;
        if left > 0 {
            s.anvil[SECOND].count = left;
        } else {
            s.anvil[SECOND] = ItemStack::EMPTY;
        }
        s.anvil[RESULT] = ItemStack::EMPTY;
        s.anvil_name = None;
        // Onto the cursor, which is where a click leaves what it picked up.
        if s.inventory.cursor.is_empty() {
            s.inventory.cursor = work.result.clone();
        } else {
            let name = crate::items::item_name(server, work.result.item);
            let max = crate::inventory::max_stack_size(&server.data, &name);
            let over = s.inventory.add(work.result.clone(), max);
            if !over.is_empty() {
                drop(s);
                let (x, y, z, yaw, pitch) = {
                    let s = player.lock();
                    (s.x, s.y, s.z, s.yaw, s.pitch)
                };
                crate::items::throw_from(server, over, yaw, pitch, x, y, z);
            }
        }
    }
    if !free {
        wear_anvil(server, player);
    }
    true
}

/// An anvil chips as it is used, and eventually falls apart.
fn wear_anvil(server: &Arc<Server>, player: &Arc<Player>) {
    if rand::random::<f32>() > 0.12 {
        return;
    }
    let Some(open) = player.lock().container.clone() else { return };
    let pos = open.pos;
    let Ok(state) = server.world().get_block(pos) else { return };
    let Some(block) = server.data.blocks.block_of_state(state as i32) else { return };
    let next = match block.name.as_str() {
        "minecraft:anvil" => Some("minecraft:chipped_anvil"),
        "minecraft:chipped_anvil" => Some("minecraft:damaged_anvil"),
        _ => None,
    };
    let props = server
        .data
        .blocks
        .state(state as i32)
        .map(|s| s.properties.clone())
        .unwrap_or_default();
    match next {
        Some(name) => {
            if let Some(state) = server.data.blocks.state_with(name, &props) {
                server.set_block(pos, state as u32);
            }
        }
        None => {
            let air = server.data.blocks.default_state("air").unwrap_or(0) as u32;
            server.set_block(pos, air);
        }
    }
}
