//! Furnaces, blast furnaces and smokers.
//!
//! A lit furnace keeps working whether or not anyone is watching it, so the
//! ones that are burning are held in memory and ticked together; the block
//! entity beside the block is written back when one goes out, when the
//! world is saved, or when its chunk is put away. Anyone with the window
//! open sees the fire and the arrow move.

use crate::containers;
use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::ItemStack;
use garnet_protocol::BlockPos;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const INPUT: usize = 0;
pub const FUEL: usize = 1;
pub const OUTPUT: usize = 2;
pub const SLOTS: usize = 3;

/// What a furnace is doing right now.
#[derive(Clone, Debug, Default)]
pub struct Furnace {
    pub items: Vec<ItemStack>,
    /// Ticks of fire left, and how long this piece of fuel lasts in total.
    pub lit: i32,
    pub lit_total: i32,
    /// How far through the current item, and how long it takes.
    pub cooked: i32,
    pub cook_total: i32,
}

/// Every furnace the server is currently running.
#[derive(Default)]
pub struct Furnaces {
    burning: Mutex<HashMap<BlockPos, Furnace>>,
}

impl Furnaces {
    pub fn new() -> Self {
        Self::default()
    }
}

/// True for the blocks this module drives.
pub fn is_furnace(block: &str) -> bool {
    matches!(block, "minecraft:furnace" | "minecraft:blast_furnace" | "minecraft:smoker")
}

/// How long a fuel burns, in ticks. Vanilla's numbers for what people
/// actually burn, with the wooden things falling back on their family.
pub fn burn_time(item: &str) -> i32 {
    let short = item.strip_prefix("minecraft:").unwrap_or(item);
    match short {
        "lava_bucket" => 20000,
        "coal_block" => 16000,
        "dried_kelp_block" => 4001,
        "blaze_rod" => 2400,
        "coal" | "charcoal" => 1600,
        "bamboo_block" => 300,
        "stick" => 100,
        "bamboo" => 50,
        _ if short.ends_with("_planks") || short.ends_with("_log") || short.ends_with("_wood") => 300,
        _ if short.ends_with("_slab") && !short.contains("stone") && !short.contains("brick") => 150,
        _ if short.ends_with("_stairs") && !short.contains("stone") && !short.contains("brick") => 300,
        _ if short.ends_with("_sapling") || short.ends_with("_button") => 100,
        _ if short.ends_with("_boat") || short.ends_with("_fence") || short.ends_with("_fence_gate") => 300,
        "crafting_table" | "bookshelf" | "chest" | "barrel" | "ladder" | "bowl" => 300,
        _ => 0,
    }
}

/// Brings a furnace into memory, from its block entity if it has one.
pub fn state(server: &Arc<Server>, pos: BlockPos) -> Furnace {
    if let Some(found) = server
        .furnaces
        .burning
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&pos)
    {
        return found.clone();
    }
    read(server, pos)
}

fn read(server: &Arc<Server>, pos: BlockPos) -> Furnace {
    let items = containers::read_items(server, pos, SLOTS);
    let compound = containers::block_entity_at(server, pos);
    let number = |name: &str| compound.as_ref().and_then(|c| c.get_i32(name)).unwrap_or(0);
    Furnace {
        items,
        lit: number("lit_time_remaining"),
        lit_total: number("lit_total_time").max(1),
        cooked: number("cooking_time_spent"),
        cook_total: number("cooking_total_time").max(1),
    }
}

/// Writes a furnace back to the world and forgets it if it has gone out.
pub fn store(server: &Arc<Server>, pos: BlockPos, furnace: Furnace) {
    containers::write_items(server, pos, &furnace.items);
    containers::set_block_entity_numbers(
        server,
        pos,
        &[
            ("lit_time_remaining", furnace.lit),
            ("lit_total_time", furnace.lit_total),
            ("cooking_time_spent", furnace.cooked),
            ("cooking_total_time", furnace.cook_total),
        ],
    );
    let mut burning = server.furnaces.burning.lock().unwrap_or_else(|e| e.into_inner());
    if furnace.lit > 0 || !furnace.items[INPUT].is_empty() {
        burning.insert(pos, furnace);
    } else {
        burning.remove(&pos);
    }
}

/// One tick for every furnace with something to do.
pub fn tick(server: &Arc<Server>) {
    let positions: Vec<BlockPos> = {
        let burning = server.furnaces.burning.lock().unwrap_or_else(|e| e.into_inner());
        burning.keys().copied().collect()
    };
    for pos in positions {
        let Some(mut furnace) = server
            .furnaces
            .burning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pos)
            .cloned()
        else {
            continue;
        };
        let block = block_name(server, pos);
        if !is_furnace(&block) {
            // Someone took it away.
            server.furnaces.burning.lock().unwrap_or_else(|e| e.into_inner()).remove(&pos);
            continue;
        }
        let was_lit = furnace.lit > 0;
        step(server, &block, &mut furnace);
        let still_going = furnace.lit > 0 || furnace.cooked > 0;
        {
            let mut burning = server.furnaces.burning.lock().unwrap_or_else(|e| e.into_inner());
            if still_going {
                burning.insert(pos, furnace.clone());
            } else {
                burning.remove(&pos);
            }
        }
        if was_lit != (furnace.lit > 0) {
            set_lit(server, pos, &block, furnace.lit > 0);
        }
        if !still_going {
            // Out of fuel or out of work: leave it as the world found it.
            store(server, pos, furnace.clone());
        } else if furnace.cooked == 0 || furnace.cooked % 20 == 0 {
            containers::write_items(server, pos, &furnace.items);
        }
        show(server, pos, &furnace);
    }
}

/// Moves one furnace on by a tick.
fn step(server: &Arc<Server>, block: &str, furnace: &mut Furnace) {
    let smelting = smelt_result(server, block, &furnace.items[INPUT], &furnace.items[OUTPUT]);

    if furnace.lit > 0 {
        furnace.lit -= 1;
    } else if smelting.is_some() {
        // Light it from the fuel slot, spending one.
        let fuel = furnace.items[FUEL].clone();
        if !fuel.is_empty() {
            let time = burn_time(&crate::items::item_name(server, fuel.item));
            if time > 0 {
                furnace.lit = time;
                furnace.lit_total = time;
                let is_bucket = crate::items::item_name(server, fuel.item) == "minecraft:lava_bucket";
                let slot = &mut furnace.items[FUEL];
                slot.count -= 1;
                if slot.count <= 0 {
                    *slot = if is_bucket {
                        crate::items::item_id(server, "minecraft:bucket")
                            .map(|id| ItemStack::new(id, 1))
                            .unwrap_or(ItemStack::EMPTY)
                    } else {
                        ItemStack::EMPTY
                    };
                }
            }
        }
    }

    match smelting {
        Some((result, time)) if furnace.lit > 0 => {
            furnace.cook_total = time.max(1);
            furnace.cooked += 1;
            if furnace.cooked >= furnace.cook_total {
                furnace.cooked = 0;
                finish(server, furnace, &result);
            }
        }
        // Nothing to cook, or the fire went out: the arrow slides back.
        _ => furnace.cooked = (furnace.cooked - 2).max(0),
    }
}

/// Takes one item from the input and puts the result in the output.
fn finish(server: &Arc<Server>, furnace: &mut Furnace, result: &str) {
    let Some(id) = crate::items::item_id(server, result) else { return };
    let input = &mut furnace.items[INPUT];
    input.count -= 1;
    if input.count <= 0 {
        *input = ItemStack::EMPTY;
    }
    let output = &mut furnace.items[OUTPUT];
    if output.is_empty() {
        *output = ItemStack::new(id, 1);
    } else if output.item == id {
        output.count += 1;
    }
}

/// What this furnace would make of what is in it, and how long it takes.
fn smelt_result(server: &Arc<Server>, block: &str, input: &ItemStack, output: &ItemStack) -> Option<(String, i32)> {
    if input.is_empty() {
        return None;
    }
    let name = crate::items::item_name(server, input.item);
    let recipe = server.recipes.smelt(&server.data, &name, block)?;
    let result_id = crate::items::item_id(server, &recipe.result)?;
    // The output slot has to have room for it.
    if !output.is_empty() && (output.item != result_id || output.count >= 64) {
        return None;
    }
    Some((recipe.result.clone(), recipe.time))
}

/// Switches the block between its lit and unlit states.
fn set_lit(server: &Arc<Server>, pos: BlockPos, block: &str, lit: bool) {
    let current = server.world().get_block(pos).ok();
    let mut props = current
        .and_then(|state| server.data.blocks.state(state as i32))
        .map(|state| state.properties.clone())
        .unwrap_or_default();
    props.insert("lit".to_owned(), lit.to_string());
    if let Some(state) = server.data.blocks.state_with(block, &props) {
        server.set_block(pos, state as u32);
    }
}

/// Sends the fire and the arrow to whoever is watching.
fn show(server: &Arc<Server>, pos: BlockPos, furnace: &Furnace) {
    for player in server.online_players() {
        let Some(open) = player.lock().container.clone() else { continue };
        if open.pos != pos || open.kind != containers::Kind::Furnace {
            continue;
        }
        for (property, value) in [
            (0, furnace.lit),
            (1, furnace.lit_total.max(1)),
            (2, furnace.cooked),
            (3, furnace.cook_total.max(1)),
        ] {
            player.send(&cb::ContainerSetData {
                window_id: open.window_id,
                property,
                value,
            });
        }
        containers::send_window(server, &player, &furnace.items);
    }
}

/// Wakes the furnaces in a chunk that was just loaded.
pub fn load_chunk(server: &Arc<Server>, chunk: garnet_protocol::ChunkPos) {
    let positions: Vec<BlockPos> = {
        let mut world = server.world();
        let Ok(loaded) = world.chunk_mut(chunk) else { return };
        loaded
            .block_entities
            .iter()
            .filter(|be| be.get_str("id").is_some_and(|id| id.contains("furnace") || id.contains("smoker")))
            .filter(|be| be.get_i32("lit_time_remaining").unwrap_or(0) > 0)
            .filter_map(|be| Some(BlockPos::new(be.get_i32("x")?, be.get_i32("y")?, be.get_i32("z")?)))
            .collect()
    };
    for pos in positions {
        let furnace = read(server, pos);
        server
            .furnaces
            .burning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pos, furnace);
    }
}

/// Writes every running furnace back, for a save or a shutdown.
pub fn save_all(server: &Arc<Server>) {
    let all: Vec<(BlockPos, Furnace)> = {
        let burning = server.furnaces.burning.lock().unwrap_or_else(|e| e.into_inner());
        burning.iter().map(|(pos, f)| (*pos, f.clone())).collect()
    };
    for (pos, furnace) in all {
        containers::write_items(server, pos, &furnace.items);
        containers::set_block_entity_numbers(
            server,
            pos,
            &[
                ("lit_time_remaining", furnace.lit),
                ("lit_total_time", furnace.lit_total),
                ("cooking_time_spent", furnace.cooked),
                ("cooking_total_time", furnace.cook_total),
            ],
        );
    }
}

/// A player put something in, or took something out.
pub fn touched(server: &Arc<Server>, pos: BlockPos, items: Vec<ItemStack>) {
    let mut furnace = state(server, pos);
    furnace.items = items;
    store(server, pos, furnace);
}

pub fn opened(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos) {
    let furnace = state(server, pos);
    show(server, pos, &furnace);
    let _ = player;
}

fn block_name(server: &Arc<Server>, pos: BlockPos) -> String {
    let state = server.world().get_block(pos).unwrap_or(0);
    server
        .data
        .blocks
        .block_of_state(state as i32)
        .map(|b| b.name.clone())
        .unwrap_or_default()
}
