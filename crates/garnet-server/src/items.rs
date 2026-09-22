//! Items in play: placing the held block, picking up what a broken block
//! drops, inventory clicks, and the item-name helpers the commands use.

use crate::inventory::{max_stack_size, OFFHAND};
use crate::loot::Tool;
use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::items::{enchantments_in, ItemStack};
use garnet_protocol::packets::play::serverbound::ContainerClick;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{BlockPos, Direction};
use std::collections::BTreeMap;
use std::sync::Arc;

pub fn item_id(server: &Server, name: &str) -> Option<i32> {
    let full = if name.contains(':') { name.to_owned() } else { format!("minecraft:{name}") };
    server.data.registries.id_of("item", &full)
}

pub fn item_name(server: &Server, id: i32) -> String {
    server
        .data
        .registries
        .get("item")
        .and_then(|r| r.name_of(id))
        .unwrap_or("minecraft:air")
        .to_owned()
}

pub fn max_stack(server: &Server, id: i32) -> i32 {
    max_stack_size(&item_name(server, id))
}

/// Sends the whole inventory; the simplest way to keep the client honest.
pub fn sync_inventory(player: &Player) {
    let packet = player.lock().inventory.content_packet();
    player.send(&packet);
}

/// Gives items to a player, returning how many did not fit.
pub fn give(server: &Server, player: &Player, stack: ItemStack) -> i32 {
    let limit = max_stack(server, stack.item);
    let left = player.lock().inventory.add(stack, limit);
    sync_inventory(player);
    left.count.max(0)
}

pub fn handle_click(server: &Server, player: &Player, click: &ContainerClick) {
    if click.container_id != 0 {
        return;
    }
    let creative = player.lock().game_mode == GameMode::Creative;
    // Slots 1-4 are the two-by-two grid in the player's own screen, and
    // slot 0 is what it makes.
    let taking_result = click.slot == 0 && !player.lock().inventory.slots[0].is_empty();
    {
        let mut state = player.lock();
        let limit = |id: i32| max_stack(server, id);
        state.inventory.click(click, &limit, creative);
    }
    if (0..5).contains(&click.slot) {
        let mut state = player.lock();
        if taking_result {
            for slot in &mut state.inventory.slots[1..5] {
                if slot.is_empty() {
                    continue;
                }
                slot.count -= 1;
                if slot.count <= 0 {
                    *slot = ItemStack::EMPTY;
                }
            }
        }
        let grid: Vec<ItemStack> = state.inventory.slots[1..5].to_vec();
        drop(state);
        let names = crate::recipes::grid_names(server, &grid);
        let result = server
            .recipes
            .result(&server.data, &names, 2)
            .and_then(|(name, count)| {
                item_id(server, &name).map(|id| ItemStack {
                    item: id,
                    count,
                    patch: Vec::new(),
                })
            })
            .unwrap_or(ItemStack::EMPTY);
        player.lock().inventory.slots[0] = result;
    }
    sync_inventory(player);
}

pub fn handle_creative_slot(server: &Arc<Server>, player: &Arc<Player>, slot: i16, stack: ItemStack) {
    if player.lock().game_mode != GameMode::Creative {
        sync_inventory(player);
        return;
    }
    if slot < 0 {
        // Thrown out of the inventory screen.
        let (yaw, pitch, x, y, z) = {
            let s = player.lock();
            (s.yaw, s.pitch, s.x, s.y, s.z)
        };
        throw_from(server, stack, yaw, pitch, x, y, z);
        return;
    }
    let mut state = player.lock();
    state.inventory.set(slot as usize, stack);
}

pub fn swap_hands(player: &Player) {
    {
        let mut state = player.lock();
        let held = crate::inventory::HOTBAR_START + state.held_slot.clamp(0, 8) as usize;
        state.inventory.slots.swap(held, OFFHAND);
    }
    sync_inventory(player);
}

// ---- breaking ----

/// Gives the player whatever the broken block drops (survival only).
pub fn collect_drops(server: &Arc<Server>, player: &Arc<Player>, state: u32, pos: BlockPos) {
    let (mode, held) = {
        let s = player.lock();
        (s.game_mode, s.inventory.held(s.held_slot).clone())
    };
    if mode != GameMode::Survival {
        return;
    }
    let Some(block) = server.data.blocks.block_of_state(state as i32) else { return };
    let props = server.data.blocks.state(state as i32).map(|s| s.properties.clone()).unwrap_or_default();
    let held_name = if held.is_empty() { None } else { Some(item_name(server, held.item)) };
    if !correct_tool(server, &block.name, held_name.as_deref()) {
        return;
    }
    let enchants = enchantments_in(&held.patch).unwrap_or_default();
    let level_of = |name: &str| {
        let id = server.data.dynamic.id_of("enchantment", name);
        enchants.iter().find(|(e, _)| Some(*e) == id).map(|(_, l)| *l).unwrap_or(0)
    };
    let tool = Tool {
        item_name: held_name.as_deref(),
        silk_touch: level_of("minecraft:silk_touch") > 0,
        fortune: level_of("minecraft:fortune"),
    };
    let drops = server.loot.drops(&block.name, &props, &tool);
    if drops.is_empty() {
        return;
    }
    let _ = player;
    for (name, count) in drops {
        if let Some(id) = item_id(server, &name) {
            let (dx, dz) = ((rand::random::<f64>() - 0.5) * 0.1, (rand::random::<f64>() - 0.5) * 0.1);
            crate::world_entities::drop_item(server, ItemStack::new(id, count), pos.x as f64 + 0.5, pos.y as f64 + 0.3, pos.z as f64 + 0.5, (dx, 0.15, dz), 10);
        }
    }
}

/// Throws items out of the player's hand (Q): one, or the whole stack.
pub fn throw_held(server: &Arc<Server>, player: &Arc<Player>, whole_stack: bool) {
    let (stack, yaw, pitch, x, y, z) = {
        let mut s = player.lock();
        let held_slot = s.held_slot;
        let slot = s.inventory.held_mut(held_slot);
        if slot.is_empty() {
            return;
        }
        let count = if whole_stack { slot.count } else { 1 };
        let thrown = ItemStack {
            item: slot.item,
            count,
            patch: slot.patch.clone(),
        };
        slot.count -= count;
        if slot.count <= 0 {
            *slot = ItemStack::EMPTY;
        }
        (thrown, s.yaw, s.pitch, s.x, s.y, s.z)
    };
    sync_inventory(player);
    throw_from(server, stack, yaw, pitch, x, y, z);
}

/// Sends a stack flying the way the player looks, as vanilla does.
pub fn throw_from(server: &Arc<Server>, stack: ItemStack, yaw: f32, pitch: f32, x: f64, y: f64, z: f64) {
    let (yaw, pitch) = (yaw.to_radians() as f64, pitch.to_radians() as f64);
    let vx = -yaw.sin() * pitch.cos() * 0.3;
    let vy = -pitch.sin() * 0.3 + 0.1;
    let vz = yaw.cos() * pitch.cos() * 0.3;
    crate::world_entities::drop_item(server, stack, x, y + 1.3, z, (vx, vy, vz), 40);
}

/// Vanilla's "requires correct tool for drops": blocks in `mineable/pickaxe`
/// need a pickaxe, and the `needs_*_tool` tags set the tier.
fn correct_tool(server: &Server, block: &str, held: Option<&str>) -> bool {
    let in_tag = |tag: &str| {
        let Some(block_id) = server.data.registries.id_of("block", block) else { return false };
        server.data.tags.members("minecraft:block", tag).is_some_and(|m| m.contains(&block_id))
    };
    if !in_tag("minecraft:mineable/pickaxe") {
        return true;
    }
    let Some(held) = held.filter(|h| h.ends_with("_pickaxe")) else { return false };
    let tier = match held.trim_start_matches("minecraft:").trim_end_matches("_pickaxe") {
        "wooden" => 0,
        "stone" => 1,
        "copper" => 1,
        "golden" => 0,
        "iron" => 2,
        "diamond" => 3,
        "netherite" => 4,
        _ => 0,
    };
    let needed = if in_tag("minecraft:needs_diamond_tool") {
        3
    } else if in_tag("minecraft:needs_iron_tool") {
        2
    } else if in_tag("minecraft:needs_stone_tool") {
        1
    } else {
        0
    };
    tier >= needed
}

// ---- placing ----

/// Items whose block has a different name.
fn block_for_item(name: &str) -> Option<String> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    let block = match short {
        "wheat_seeds" => "wheat",
        "beetroot_seeds" => "beetroots",
        "melon_seeds" => "melon_stem",
        "pumpkin_seeds" => "pumpkin_stem",
        "torchflower_seeds" => "torchflower_crop",
        "pitcher_pod" => "pitcher_crop",
        "carrot" => "carrots",
        "potato" => "potatoes",
        "redstone" => "redstone_wire",
        "string" => "tripwire",
        "water_bucket" => "water",
        "lava_bucket" => "lava",
        "powder_snow_bucket" => "powder_snow",
        "sweet_berries" => "sweet_berry_bush",
        "glow_berries" => "cave_vines",
        "cocoa_beans" => "cocoa",
        "nether_wart" => "nether_wart",
        "kelp" => "kelp",
        "bamboo" => "bamboo_sapling",
        other => other,
    };
    Some(format!("minecraft:{block}"))
}

/// Blocks a new block may replace.
fn replaceable(server: &Server, state: u32) -> bool {
    let blocks = &server.data.blocks;
    if blocks.is_air(state as i32) || blocks.is_liquid(state as i32) {
        return true;
    }
    let name = blocks.block_of_state(state as i32).map(|b| b.name.as_str()).unwrap_or("");
    matches!(
        name.trim_start_matches("minecraft:"),
        "short_grass" | "tall_grass" | "fern" | "large_fern" | "dead_bush" | "seagrass" | "tall_seagrass" | "vine" | "fire" | "snow"
            | "structure_void" | "light" | "bubble_column" | "hanging_roots" | "glow_lichen" | "sculk_vein" | "warped_roots"
            | "crimson_roots" | "nether_sprouts" | "short_dry_grass" | "tall_dry_grass" | "leaf_litter" | "bush" | "firefly_bush"
    )
}

/// Places the held block against the clicked face. `true` when a block
/// was placed and the item was used.
#[allow(clippy::too_many_arguments)]
pub fn place_held(server: &Arc<Server>, player: &Arc<Player>, target: BlockPos, face: i32, cursor: (f32, f32, f32), hand: i32) -> bool {
    let (mode, held_slot, stack, yaw, pitch) = {
        let s = player.lock();
        let slot = if hand == 1 { OFFHAND } else { crate::inventory::HOTBAR_START + s.held_slot.clamp(0, 8) as usize };
        (s.game_mode, slot, s.inventory.slots[slot].clone(), s.yaw, s.pitch)
    };
    if stack.is_empty() || matches!(mode, GameMode::Spectator | GameMode::Adventure) {
        return false;
    }
    let item = item_name(server, stack.item);
    let Some(block_name) = block_for_item(&item) else { return false };
    let blocks = &server.data.blocks;
    let Some(block) = blocks.block(&block_name) else { return false };
    let Some(dir) = Direction::from_id(face) else { return false };

    let clicked = server.world().get_block(target).unwrap_or(0);
    let pos = if replaceable(server, clicked) && !blocks.is_liquid(clicked as i32) {
        target
    } else {
        let (dx, dy, dz) = dir.offset();
        target.offset(dx, dy, dz)
    };
    let current = server.world().get_block(pos).unwrap_or(0);
    if !replaceable(server, current) {
        return false;
    }

    // Orientation from the face, the cursor and where the player looks.
    let mut props: BTreeMap<String, String> = BTreeMap::new();
    let has = |p: &str| block.properties.iter().any(|(name, _)| name == p);
    let allowed = |p: &str, v: &str| block.properties.iter().any(|(name, values)| name == p && values.iter().any(|x| x == v));
    if has("axis") {
        props.insert(
            "axis".into(),
            match dir {
                Direction::Up | Direction::Down => "y",
                Direction::East | Direction::West => "x",
                _ => "z",
            }
            .into(),
        );
    }
    if has("facing") {
        let horizontal = horizontal_facing(yaw);
        let toward_player = opposite(horizontal);
        let facing = if allowed("facing", "up") && allowed("facing", "down") && !block_name.ends_with("_stairs") {
            // Six-way blocks (dispensers, observers, pistons) face away from the player,
            // unless placed on a floor or ceiling while looking steeply.
            if pitch > 60.0 {
                "up"
            } else if pitch < -60.0 {
                "down"
            } else {
                horizontal
            }
        } else if block_name.ends_with("_stairs") || block_name.ends_with("_door") || block_name.ends_with("_trapdoor") || block_name.ends_with("_fence_gate") || block_name.ends_with("_bed") {
            horizontal
        } else {
            toward_player
        };
        if allowed("facing", facing) {
            props.insert("facing".into(), facing.into());
        }
    }
    if has("half") && allowed("half", "top") {
        let top = matches!(dir, Direction::Down) || (!matches!(dir, Direction::Up) && cursor.1 > 0.5);
        props.insert("half".into(), if top { "top" } else { "bottom" }.into());
    }
    if has("type") && allowed("type", "top") {
        let top = matches!(dir, Direction::Down) || (!matches!(dir, Direction::Up) && cursor.1 > 0.5);
        props.insert("type".into(), if top { "top" } else { "bottom" }.into());
    }
    if has("rotation") {
        let rotation = (((yaw + 180.0) / 22.5).round() as i32).rem_euclid(16);
        props.insert("rotation".into(), rotation.to_string());
    }
    let state = blocks.state_with(&block_name, &props).or(Some(block.default_state)).unwrap() as u32;

    if !server.set_block(pos, state) {
    crate::blocks::changed(server, pos);
    server.schedule_block(pos, 2);
        return false;
    }
    if mode == GameMode::Survival {
        let mut s = player.lock();
        let slot = &mut s.inventory.slots[held_slot];
        slot.count -= 1;
        if slot.count <= 0 {
            *slot = ItemStack::EMPTY;
        }
    }
    sync_inventory(player);
    true
}

fn horizontal_facing(yaw: f32) -> &'static str {
    match ((yaw + 180.0).rem_euclid(360.0) / 90.0).round() as i32 % 4 {
        0 => "north",
        1 => "east",
        2 => "south",
        _ => "west",
    }
}

fn opposite(facing: &str) -> &'static str {
    match facing {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        _ => "east",
    }
}
