//! Hoppers: the blocks that move things about on their own.
//!
//! One item every eight ticks, the way vanilla does it: out into whatever
//! the hopper faces, in from whatever sits on top of it, and up off the
//! floor from any item lying in the space above. Furnaces are the fiddly
//! case — what goes in the top is smelted, what goes in the side is fuel,
//! and only the finished goods come out of the bottom. A redstone signal
//! stops it.

use crate::containers;
use crate::server::Server;
use garnet_protocol::packets::play::items::ItemStack;
use garnet_protocol::BlockPos;
use std::sync::Arc;

/// Ticks between one item and the next.
const COOLDOWN: u64 = 8;
pub const SLOTS: usize = 5;

/// Which slots of a container a hopper may take from or put into, coming
/// from a given side.
fn slots_of(block: &str) -> Option<usize> {
    let short = block.strip_prefix("minecraft:").unwrap_or(block);
    Some(match short {
        "chest" | "trapped_chest" | "barrel" => 27,
        "hopper" => SLOTS,
        "dispenser" | "dropper" => 9,
        "furnace" | "blast_furnace" | "smoker" => 3,
        _ if short.ends_with("shulker_box") => 27,
        _ => return None,
    })
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

fn facing_of(server: &Arc<Server>, pos: BlockPos) -> (String, bool) {
    let Ok(state) = server.world().get_block(pos) else {
        return ("down".to_owned(), false);
    };
    let props = server
        .data
        .blocks
        .state(state as i32)
        .map(|s| s.properties.clone())
        .unwrap_or_default();
    (
        props.get("facing").cloned().unwrap_or_else(|| "down".to_owned()),
        props.get("enabled").map(String::as_str) != Some("false"),
    )
}

fn step(pos: BlockPos, facing: &str) -> BlockPos {
    match facing {
        "north" => pos.offset(0, 0, -1),
        "south" => pos.offset(0, 0, 1),
        "west" => pos.offset(-1, 0, 0),
        "east" => pos.offset(1, 0, 0),
        "up" => pos.offset(0, 1, 0),
        _ => pos.offset(0, -1, 0),
    }
}

/// One turn of a hopper: push, then pull, then look at the floor above.
pub fn tick(server: &Arc<Server>, pos: BlockPos) {
    if block_name(server, pos) != "minecraft:hopper" {
        return;
    }
    let (facing, enabled) = facing_of(server, pos);
    if !enabled {
        server.schedule_block(pos, COOLDOWN);
        return;
    }
    let mut items = containers::read_items(server, pos, SLOTS);
    let mut moved = push(server, pos, &facing, &mut items);
    if !moved {
        moved = pull(server, pos, &mut items);
    }
    if !moved {
        moved = collect(server, pos, &mut items);
    }
    if moved {
        containers::write_items(server, pos, &items);
    }
    server.schedule_block(pos, COOLDOWN);
}

/// Moves one item into whatever the hopper faces.
fn push(server: &Arc<Server>, pos: BlockPos, facing: &str, items: &mut [ItemStack]) -> bool {
    let target = step(pos, facing);
    let name = block_name(server, target);
    let Some(size) = slots_of(&name) else { return false };
    let Some(from) = items.iter().position(|stack| !stack.is_empty()) else { return false };

    let mut theirs = containers::read_items(server, target, size);
    let one = ItemStack {
        item: items[from].item,
        count: 1,
        patch: items[from].patch.clone(),
    };
    // A furnace takes fuel in its side and things to smelt on its top.
    let allowed: Vec<usize> = if is_furnace(&name) {
        if facing == "down" {
            vec![crate::furnaces::INPUT]
        } else {
            vec![crate::furnaces::FUEL]
        }
    } else {
        (0..size).collect()
    };
    let Some(into) = room_for(server, &theirs, &allowed, &one) else { return false };
    if theirs[into].is_empty() {
        theirs[into] = one;
    } else {
        theirs[into].count += 1;
    }
    items[from].count -= 1;
    if items[from].count <= 0 {
        items[from] = ItemStack::EMPTY;
    }
    store(server, target, &name, theirs);
    true
}

/// Takes one item out of whatever sits on top.
fn pull(server: &Arc<Server>, pos: BlockPos, items: &mut [ItemStack]) -> bool {
    let above = pos.offset(0, 1, 0);
    let name = block_name(server, above);
    let Some(size) = slots_of(&name) else { return false };
    let mut theirs = containers::read_items(server, above, size);
    // Only what a furnace has finished may be taken from underneath.
    let from_slots: Vec<usize> = if is_furnace(&name) {
        vec![crate::furnaces::OUTPUT]
    } else {
        (0..size).collect()
    };
    let Some(from) = from_slots.into_iter().find(|slot| !theirs[*slot].is_empty()) else { return false };
    let one = ItemStack {
        item: theirs[from].item,
        count: 1,
        patch: theirs[from].patch.clone(),
    };
    let mine: Vec<usize> = (0..items.len()).collect();
    let Some(into) = room_for(server, items, &mine, &one) else { return false };
    if items[into].is_empty() {
        items[into] = one;
    } else {
        items[into].count += 1;
    }
    theirs[from].count -= 1;
    if theirs[from].count <= 0 {
        theirs[from] = ItemStack::EMPTY;
    }
    store(server, above, &name, theirs);
    true
}

/// Picks up anything lying in the space above the hopper.
fn collect(server: &Arc<Server>, pos: BlockPos, items: &mut [ItemStack]) -> bool {
    let wanted: Vec<(i32, ItemStack)> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities
            .by_id
            .values()
            .filter(|entity| entity.is_item() && entity.pickup_delay == 0)
            .filter(|entity| {
                entity.x.floor() as i32 == pos.x
                    && entity.z.floor() as i32 == pos.z
                    && (entity.y - (pos.y as f64 + 1.0)).abs() < 1.2
            })
            .filter_map(|entity| entity.item.clone().map(|stack| (entity.id, stack)))
            .collect()
    };
    for (id, stack) in wanted {
        let mine: Vec<usize> = (0..items.len()).collect();
        let name = crate::items::item_name(server, stack.item);
        let max = crate::inventory::max_stack_size(&server.data, &name);
        let mut left = stack.clone();
        for slot in mine {
            if left.count <= 0 {
                break;
            }
            if items[slot].is_empty() {
                items[slot] = left.clone();
                left.count = 0;
            } else if items[slot].item == left.item && items[slot].patch == left.patch && items[slot].count < max {
                let room = (max - items[slot].count).min(left.count);
                items[slot].count += room;
                left.count -= room;
            }
        }
        if left.count < stack.count {
            if left.count <= 0 {
                crate::world_entities::despawn(server, id);
            } else {
                crate::world_entities::set_item(server, id, left);
            }
            return true;
        }
    }
    false
}

/// The first slot of `allowed` that would take this item.
fn room_for(server: &Arc<Server>, slots: &[ItemStack], allowed: &[usize], one: &ItemStack) -> Option<usize> {
    let name = crate::items::item_name(server, one.item);
    let max = crate::inventory::max_stack_size(&server.data, &name);
    allowed
        .iter()
        .copied()
        .find(|slot| {
            let there = &slots[*slot];
            there.item == one.item && there.patch == one.patch && there.count < max
        })
        .or_else(|| allowed.iter().copied().find(|slot| slots[*slot].is_empty()))
}

fn is_furnace(name: &str) -> bool {
    crate::furnaces::is_furnace(name)
}

/// Writes a container back, going through the furnace's own bookkeeping
/// when that is what it is.
fn store(server: &Arc<Server>, pos: BlockPos, name: &str, items: Vec<ItemStack>) {
    if is_furnace(name) {
        crate::furnaces::touched(server, pos, items);
    } else {
        containers::write_items(server, pos, &items);
    }
}

/// Wakes the hoppers in a chunk that has just been loaded.
pub fn load_chunk(server: &Arc<Server>, chunk: garnet_protocol::ChunkPos) {
    let positions: Vec<BlockPos> = {
        let mut world = server.world();
        let Ok(loaded) = world.chunk_mut(chunk) else { return };
        loaded
            .block_entities
            .iter()
            .filter(|be| be.get_str("id") == Some("minecraft:hopper"))
            .filter_map(|be| Some(BlockPos::new(be.get_i32("x")?, be.get_i32("y")?, be.get_i32("z")?)))
            .collect()
    };
    for pos in positions {
        server.schedule_block(pos, COOLDOWN);
    }
}
