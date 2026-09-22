//! Blocks you can put things in: chests, barrels, shulker boxes and the
//! rest.
//!
//! What a container holds lives where the world keeps it, in the block
//! entity beside the block, so it survives a restart and travels with the
//! region file. A player who opens one gets a window whose slots run
//! container first, then their own inventory, which is the order the client
//! expects; clicks are applied to that combined view and written straight
//! back to the block, so two people sharing a chest see the same thing.

use crate::inventory::{Inventory, HOTBAR_START, MAIN_START};
use crate::player::Player;
use crate::server::Server;
use garnet_protocol::nbt::{NbtCompound, NbtTag};
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::{ClickKind, ContainerClick, ItemStack};
use garnet_protocol::{BlockPos, Text};
use std::sync::Arc;

/// What a player currently has open.
#[derive(Clone, Debug)]
pub struct OpenContainer {
    pub pos: BlockPos,
    pub window_id: i32,
    /// How many slots belong to the window rather than the player.
    pub size: usize,
    pub title: String,
    pub kind: Kind,
}

/// Where a window's own slots live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// In the block entity beside the block.
    Block,
    /// On the player: a crafting grid with its result in slot 0.
    Crafting,
}

/// The containers we know how to open, and how big they are. Ender chests
/// are missing on purpose: they belong to the player, not the block, and
/// want their own storage.
fn container_size(block: &str) -> Option<(usize, &'static str, &'static str)> {
    let short = block.strip_prefix("minecraft:").unwrap_or(block);
    let value = match short {
        "chest" | "trapped_chest" => (27, "minecraft:generic_9x3", "Chest"),
        "barrel" => (27, "minecraft:generic_9x3", "Barrel"),
        "hopper" => (5, "minecraft:hopper", "Hopper"),
        "dispenser" => (9, "minecraft:generic_3x3", "Dispenser"),
        "dropper" => (9, "minecraft:generic_3x3", "Dropper"),
        name if name.ends_with("shulker_box") => (27, "minecraft:shulker_box", "Shulker Box"),
        _ => return None,
    };
    Some(value)
}

/// True when this block keeps its contents in a block entity we handle.
pub fn is_container(block: &str) -> bool {
    container_size(block).is_some()
}

/// Right-clicking a container block: show it.
pub fn open(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos, block: &str) -> bool {
    if block == "minecraft:crafting_table" {
        return open_crafting(server, player, pos);
    }
    let Some((size, menu, title)) = container_size(block) else {
        return false;
    };
    let items = read_items(server, pos, size);
    let window_id = {
        let mut s = player.lock();
        // Window ids wrap; zero always means the player's own inventory.
        s.next_window_id = s.next_window_id % 100 + 1;
        let id = s.next_window_id;
        s.container = Some(OpenContainer {
            pos,
            window_id: id,
            size,
            title: title.to_owned(),
            kind: Kind::Block,
        });
        id
    };
    let menu_type = server.data.registries.id_of("menu", menu).unwrap_or(0);
    player.send(&cb::OpenScreen {
        window_id,
        menu_type,
        title: Text::new(title),
    });
    send_content(server, player, &items);
    true
}

/// A crafting table: nine slots to lay things out in and one to take the
/// result from. The grid belongs to the player while it is open, and is
/// handed back when they close it.
fn open_crafting(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos) -> bool {
    let window_id = {
        let mut s = player.lock();
        s.next_window_id = s.next_window_id % 100 + 1;
        let id = s.next_window_id;
        s.crafting = vec![ItemStack::EMPTY; 10];
        s.container = Some(OpenContainer {
            pos,
            window_id: id,
            size: 10,
            title: "Crafting".to_owned(),
            kind: Kind::Crafting,
        });
        id
    };
    let menu_type = server.data.registries.id_of("menu", "minecraft:crafting").unwrap_or(0);
    player.send(&cb::OpenScreen {
        window_id,
        menu_type,
        title: Text::new("Crafting"),
    });
    let grid = player.lock().crafting.clone();
    send_content(server, player, &grid);
    true
}

/// Works out what the grid makes and puts it in the result slot.
pub fn update_result(server: &Arc<Server>, slots: &mut [ItemStack], grid: std::ops::Range<usize>, width: usize) {
    let stacks: Vec<ItemStack> = slots[grid].to_vec();
    let names = crate::recipes::grid_names(server, &stacks);
    slots[0] = match server.recipes.result(&server.data, &names, width) {
        Some((name, count)) => match crate::items::item_id(server, &name) {
            Some(id) => ItemStack {
                item: id,
                count,
                patch: Vec::new(),
            },
            None => ItemStack::EMPTY,
        },
        None => ItemStack::EMPTY,
    };
}

/// Takes one craft out of the grid: every slot that was used loses one.
fn consume_grid(slots: &mut [ItemStack], grid: std::ops::Range<usize>) {
    for slot in &mut slots[grid] {
        if slot.is_empty() {
            continue;
        }
        slot.count -= 1;
        if slot.count <= 0 {
            *slot = ItemStack::EMPTY;
        }
    }
}

/// A click inside an open container window.
pub fn click(server: &Arc<Server>, player: &Arc<Player>, click: ContainerClick) {
    let Some(open) = player.lock().container.clone() else {
        crate::items::sync_inventory(player);
        return;
    };
    if click.container_id != open.window_id {
        crate::items::sync_inventory(player);
        return;
    }

    // One flat list: the window's own slots, then the player's main
    // inventory, then their hotbar, in the order the window is laid out.
    let mut slots = match open.kind {
        Kind::Block => read_items(server, open.pos, open.size),
        Kind::Crafting => player.lock().crafting.clone(),
    };
    {
        let s = player.lock();
        slots.extend(s.inventory.slots[MAIN_START..HOTBAR_START].iter().cloned());
        slots.extend(s.inventory.slots[HOTBAR_START..HOTBAR_START + 9].iter().cloned());
    }
    let mut cursor = player.lock().inventory.cursor.clone();
    let taking_result = open.kind == Kind::Crafting && click.slot == 0;
    if taking_result && slots[0].is_empty() {
        crate::items::sync_inventory(player);
        return;
    }
    apply(server, &mut slots, &mut cursor, &click, open.size);
    if open.kind == Kind::Crafting {
        if taking_result {
            consume_grid(&mut slots, 1..10);
        }
        update_result(server, &mut slots, 1..10, 3);
    }

    // Put everything back where it came from.
    let (block_items, player_items) = slots.split_at(open.size);
    match open.kind {
        Kind::Block => write_items(server, open.pos, block_items),
        Kind::Crafting => player.lock().crafting = block_items.to_vec(),
    }
    {
        let mut s = player.lock();
        for (i, stack) in player_items[..27].iter().enumerate() {
            s.inventory.slots[MAIN_START + i] = stack.clone();
        }
        for (i, stack) in player_items[27..].iter().enumerate() {
            s.inventory.slots[HOTBAR_START + i] = stack.clone();
        }
        s.inventory.cursor = cursor;
    }
    send_content(server, player, block_items);
    if open.kind == Kind::Block {
        refresh_others(server, player, open.pos);
    }
}

/// Applies one click to the combined slot list.
fn apply(server: &Arc<Server>, slots: &mut [ItemStack], cursor: &mut ItemStack, click: &ContainerClick, size: usize) {
    let index = click.slot;
    let limit = |server: &Server, stack: &ItemStack| {
        let name = crate::items::item_name(server, stack.item);
        crate::inventory::max_stack_size(&name)
    };
    match click.kind {
        ClickKind::Pickup => {
            if index < 0 {
                return; // thrown at the window's edge
            }
            let Some(slot) = slots.get_mut(index as usize) else { return };
            let half = click.button == 1;
            if cursor.is_empty() {
                if slot.is_empty() {
                    return;
                }
                if half {
                    let keep = slot.count / 2;
                    let taken = slot.count - keep;
                    *cursor = ItemStack {
                        item: slot.item,
                        count: taken,
                        patch: slot.patch.clone(),
                    };
                    slot.count = keep;
                    if slot.count <= 0 {
                        *slot = ItemStack::EMPTY;
                    }
                } else {
                    *cursor = std::mem::replace(slot, ItemStack::EMPTY);
                }
            } else if slot.is_empty() {
                if half {
                    let mut one = cursor.clone();
                    one.count = 1;
                    *slot = one;
                    cursor.count -= 1;
                    if cursor.count <= 0 {
                        *cursor = ItemStack::EMPTY;
                    }
                } else {
                    *slot = std::mem::replace(cursor, ItemStack::EMPTY);
                }
            } else if slot.item == cursor.item && slot.patch == cursor.patch {
                let max = limit(server, slot);
                let moved = if half { 1.min(cursor.count) } else { cursor.count };
                let room = (max - slot.count).max(0).min(moved);
                slot.count += room;
                cursor.count -= room;
                if cursor.count <= 0 {
                    *cursor = ItemStack::EMPTY;
                }
            } else {
                std::mem::swap(slot, cursor);
            }
        }
        ClickKind::QuickMove => {
            // Shift-click: block to player, or player to block.
            if index < 0 {
                return;
            }
            let from = index as usize;
            if slots.get(from).map(ItemStack::is_empty).unwrap_or(true) {
                return;
            }
            let (start, end) = if from < size { (size, slots.len()) } else { (0, size) };
            move_into(server, slots, from, start, end);
        }
        ClickKind::Swap => {
            // Number keys swap with the hotbar, which sits at the end.
            if index < 0 {
                return;
            }
            let hotbar = slots.len() - 9 + (click.button.clamp(0, 8) as usize);
            let from = index as usize;
            if from != hotbar && from < slots.len() {
                slots.swap(from, hotbar);
            }
        }
        ClickKind::Throw => {
            if let Some(slot) = slots.get_mut(index.max(0) as usize) {
                *slot = ItemStack::EMPTY;
            }
        }
        _ => {}
    }
}

/// Shift-click: pour one slot into the first slots of another range that
/// will take it.
fn move_into(server: &Arc<Server>, slots: &mut [ItemStack], from: usize, start: usize, end: usize) {
    let mut moving = slots[from].clone();
    let name = crate::items::item_name(server, moving.item);
    let max = crate::inventory::max_stack_size(&name);
    // Top up matching stacks first, the way vanilla does.
    for i in start..end {
        if moving.count <= 0 {
            break;
        }
        if slots[i].item == moving.item && slots[i].patch == moving.patch && slots[i].count < max {
            let room = (max - slots[i].count).min(moving.count);
            slots[i].count += room;
            moving.count -= room;
        }
    }
    for i in start..end {
        if moving.count <= 0 {
            break;
        }
        if slots[i].is_empty() {
            slots[i] = moving.clone();
            moving.count = 0;
        }
    }
    slots[from] = if moving.count > 0 {
        moving
    } else {
        ItemStack::EMPTY
    };
}

/// Closing the window. A block keeps what it holds; a crafting grid gives
/// what is still on it back to the player, or drops it at their feet.
pub fn close(server: &Arc<Server>, player: &Arc<Player>) {
    let open = player.lock().container.take();
    // Whatever was on the cursor goes back to the player either way, or it
    // would quietly disappear.
    let mut left: Vec<ItemStack> = Vec::new();
    {
        let mut s = player.lock();
        let cursor = std::mem::replace(&mut s.inventory.cursor, ItemStack::EMPTY);
        if !cursor.is_empty() {
            left.push(cursor);
        }
    }
    if open.map(|c| c.kind) == Some(Kind::Crafting) {
        let mut s = player.lock();
        let grid = std::mem::take(&mut s.crafting);
        drop(s);
        left.extend(grid.into_iter().skip(1).filter(|stack| !stack.is_empty()));
    }
    if left.is_empty() {
        return;
    }
    for stack in left {
        let name = crate::items::item_name(server, stack.item);
        let max = crate::inventory::max_stack_size(&name);
        let over = player.lock().inventory.add(stack, max);
        if !over.is_empty() {
            let (x, y, z, yaw, pitch) = {
                let s = player.lock();
                (s.x, s.y, s.z, s.yaw, s.pitch)
            };
            crate::items::throw_from(server, over, yaw, pitch, x, y, z);
        }
    }
    crate::items::sync_inventory(player);
}

/// Sends the whole window: the block's slots, then the player's own.
fn send_content(server: &Arc<Server>, player: &Arc<Player>, items: &[ItemStack]) {
    let Some(open) = player.lock().container.clone() else { return };
    let mut all: Vec<ItemStack> = items.to_vec();
    let (cursor, state_id) = {
        let mut s = player.lock();
        all.extend(s.inventory.slots[MAIN_START..HOTBAR_START].iter().cloned());
        all.extend(s.inventory.slots[HOTBAR_START..HOTBAR_START + 9].iter().cloned());
        s.inventory.state_id += 1;
        (s.inventory.cursor.clone(), s.inventory.state_id)
    };
    let _ = server;
    player.send(&cb::ContainerSetContent {
        container_id: open.window_id,
        state_id,
        items: all,
        carried: cursor,
    });
}

/// Anyone else looking into the same block sees the change too.
fn refresh_others(server: &Arc<Server>, actor: &Arc<Player>, pos: BlockPos) {
    let viewers: Vec<Arc<Player>> = server
        .online_players()
        .into_iter()
        .filter(|p| p.uuid != actor.uuid && p.lock().container.as_ref().is_some_and(|c| c.pos == pos))
        .collect();
    for viewer in viewers {
        let size = viewer.lock().container.as_ref().map(|c| c.size).unwrap_or(27);
        let items = read_items(server, pos, size);
        send_content(server, &viewer, &items);
    }
}

/// Everything a container block is holding, padded to its size.
pub fn read_items(server: &Server, pos: BlockPos, size: usize) -> Vec<ItemStack> {
    let mut items = vec![ItemStack::EMPTY; size];
    let Some(compound) = block_entity(server, pos) else { return items };
    let Some(list) = compound.get_list("Items") else { return items };
    for tag in list {
        let Some(entry) = tag.as_compound() else { continue };
        let slot = entry.get_i32("Slot").unwrap_or(-1);
        if slot < 0 || slot as usize >= size {
            continue;
        }
        let Some(name) = entry.get_str("id") else { continue };
        let Some(id) = crate::items::item_id(server, name) else { continue };
        let patch = match entry.get("garnet_patch") {
            Some(NbtTag::ByteArray(bytes)) => bytes.iter().map(|b| *b as u8).collect(),
            _ => Vec::new(),
        };
        items[slot as usize] = ItemStack {
            item: id,
            count: entry.get_i32("count").unwrap_or(1),
            patch,
        };
    }
    items
}

/// Writes the contents back into the block entity beside the block.
pub fn write_items(server: &Server, pos: BlockPos, items: &[ItemStack]) {
    let list: Vec<NbtTag> = items
        .iter()
        .enumerate()
        .filter(|(_, stack)| !stack.is_empty())
        .map(|(slot, stack)| {
            let mut entry = NbtCompound::new();
            entry.put("Slot", slot as i32);
            entry.put("id", crate::items::item_name(server, stack.item).as_str());
            entry.put("count", stack.count);
            if !stack.patch.is_empty() {
                entry.put("garnet_patch", NbtTag::ByteArray(stack.patch.iter().map(|b| *b as i8).collect()));
            }
            NbtTag::Compound(entry)
        })
        .collect();
    let mut world = server.world();
    let Ok(chunk) = world.chunk_mut(pos.chunk()) else { return };
    if let Some(existing) = chunk.block_entities.iter_mut().find(|be| {
        be.get_i32("x") == Some(pos.x) && be.get_i32("y") == Some(pos.y) && be.get_i32("z") == Some(pos.z)
    }) {
        existing.put("Items", list);
    } else {
        let mut compound = NbtCompound::new();
        compound.put("x", pos.x);
        compound.put("y", pos.y);
        compound.put("z", pos.z);
        compound.put("id", "minecraft:chest");
        compound.put("Items", list);
        chunk.block_entities.push(compound);
    }
    chunk.dirty = true;
}

/// Tips a broken container's contents onto the ground.
pub fn spill(server: &Arc<Server>, pos: BlockPos, block: &str) {
    let Some((size, _, _)) = container_size(block) else { return };
    let items = read_items(server, pos, size);
    if items.iter().all(ItemStack::is_empty) {
        return;
    }
    for stack in items.into_iter().filter(|s| !s.is_empty()) {
        let spread = || (rand::random::<f64>() - 0.5) * 0.2;
        crate::world_entities::drop_item(
            server,
            stack,
            pos.x as f64 + 0.5,
            pos.y as f64 + 0.5,
            pos.z as f64 + 0.5,
            (spread(), 0.2, spread()),
            10,
        );
    }
    remove_block_entity(server, pos);
    // Anyone looking into it is looking at nothing now.
    for player in server.online_players() {
        let open = player.lock().container.clone();
        if open.is_some_and(|c| c.pos == pos) {
            player.send(&cb::ContainerClose {
                window_id: player.lock().container.as_ref().map(|c| c.window_id).unwrap_or(0),
            });
            close(server, &player);
        }
    }
}

/// Drops the block entity with the block, so nothing stale is saved.
fn remove_block_entity(server: &Server, pos: BlockPos) {
    let mut world = server.world();
    let Ok(chunk) = world.chunk_mut(pos.chunk()) else { return };
    chunk.block_entities.retain(|be| {
        be.get_i32("x") != Some(pos.x) || be.get_i32("y") != Some(pos.y) || be.get_i32("z") != Some(pos.z)
    });
    chunk.dirty = true;
}

fn block_entity(server: &Server, pos: BlockPos) -> Option<NbtCompound> {
    let mut world = server.world();
    let chunk = world.chunk_mut(pos.chunk()).ok()?;
    chunk
        .block_entities
        .iter()
        .find(|be| be.get_i32("x") == Some(pos.x) && be.get_i32("y") == Some(pos.y) && be.get_i32("z") == Some(pos.z))
        .cloned()
}

/// Puts a stack into a container block, for hoppers and the like later on.
pub fn insert(server: &Server, pos: BlockPos, block: &str, stack: ItemStack) -> Option<ItemStack> {
    let (size, _, _) = container_size(block)?;
    let mut items = read_items(server, pos, size);
    let mut inventory = Inventory::from_slots(items.clone());
    let name = crate::items::item_name(server, stack.item);
    let left = inventory.add(stack, crate::inventory::max_stack_size(&name));
    items = inventory.into_slots();
    write_items(server, pos, &items);
    if left.is_empty() {
        None
    } else {
        Some(left)
    }
}
