//! Brewing stands.
//!
//! A stand works the way a furnace does: it keeps going whether or not
//! anyone is watching, so the ones with something to do are held in memory
//! and ticked together, and the block entity beside the block is written
//! back when it stops or its chunk is put away. What can be brewed from
//! what comes from the data pack, which in 26.3 holds every brewing recipe
//! as a file of its own, and how many brews a piece of fuel is worth comes
//! from the fuel item's own components.

use crate::containers;
use crate::player::Player;
use crate::server::Server;
use garnet_data::GameData;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::{potion_in, ItemStack, PatchBuilder};
use garnet_protocol::{BlockPos, Text};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The three bottles, then what goes in them, then the fuel.
pub const BOTTLES: usize = 3;
pub const INGREDIENT: usize = 3;
pub const FUEL: usize = 4;
pub const SLOTS: usize = 5;
/// How long one brew takes at normal speed.
const BREW_TICKS: i32 = 400;

/// What a stand is doing right now.
#[derive(Clone, Debug, Default)]
pub struct Stand {
    pub items: Vec<ItemStack>,
    /// Ticks left of this brew, counting down, and how long it takes.
    pub brew_time: i32,
    pub brew_total: i32,
    /// Brews left in the fuel, and how many the last piece gave.
    pub fuel: i32,
    pub fuel_total: i32,
    pub speed: f32,
    /// What was in the ingredient slot when this brew started: changing it
    /// part way through stops the brew, as vanilla does.
    pub ingredient: i32,
}

/// Every stand the server is currently running.
#[derive(Default)]
pub struct Stands {
    working: Mutex<HashMap<BlockPos, Stand>>,
}

impl Stands {
    pub fn new() -> Self {
        Self::default()
    }
}

/// One way of turning what is in a bottle into something else.
#[derive(Debug, Clone)]
struct Recipe {
    /// What has to be in the bottle slot, and which potions it may hold.
    input: crate::recipes::Ingredient,
    input_potions: Option<Vec<String>>,
    /// What goes in the top.
    reagent: crate::recipes::Ingredient,
    /// What comes out, and what is in it.
    output: String,
    output_potion: Option<String>,
}

/// Every brewing recipe the data pack defines.
#[derive(Default)]
pub struct Brewing {
    recipes: Vec<Recipe>,
}

impl Brewing {
    pub fn load(data: &GameData) -> Self {
        let dir = data
            .version_dir
            .join("datapack")
            .join("minecraft")
            .join("recipe")
            .join("brewing");
        let mut recipes = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            tracing::warn!("no brewing recipes in {}", dir.display());
            return Self { recipes };
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(json) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if json.get("type").and_then(Value::as_str) != Some("minecraft:brewing") {
                continue;
            }
            if let Some(recipe) = parse(&json) {
                recipes.push(recipe);
            }
        }
        tracing::info!("loaded {} brewing recipes", recipes.len());
        Self { recipes }
    }

    /// What this bottle would become with that ingredient in the top.
    fn result(&self, server: &Arc<Server>, bottle: &ItemStack, reagent: &ItemStack) -> Option<ItemStack> {
        if bottle.is_empty() || reagent.is_empty() {
            return None;
        }
        let bottle_name = crate::items::item_name(server, bottle.item);
        let reagent_name = crate::items::item_name(server, reagent.item);
        let potion = potion_name(server, bottle);
        let recipe = self.recipes.iter().find(|recipe| {
            recipe.input.matches(&server.data, &bottle_name)
                && recipe.reagent.matches(&server.data, &reagent_name)
                && match (&recipe.input_potions, &potion) {
                    (None, _) => true,
                    (Some(allowed), Some(held)) => allowed.iter().any(|name| name == held),
                    (Some(_), None) => false,
                }
        })?;
        let id = crate::items::item_id(server, &recipe.output)?;
        let patch = match &recipe.output_potion {
            Some(name) => {
                let potion = server.data.registries.id_of("potion", name)?;
                PatchBuilder::default().potion(potion).build()
            }
            None => Vec::new(),
        };
        Some(ItemStack {
            item: id,
            count: 1,
            patch,
        })
    }
}

fn parse(json: &Value) -> Option<Recipe> {
    let input = json.get("input")?;
    let reagent = json.get("reagent")?;
    let output = json.get("output")?;
    Some(Recipe {
        input: crate::recipes::Ingredient::parse(input.get("item")?)?,
        input_potions: potions_of(input),
        reagent: crate::recipes::Ingredient::parse(reagent.get("item")?)?,
        output: with_namespace(output.get("id")?.as_str()?),
        output_potion: output
            .get("components")
            .and_then(|c| c.get("minecraft:potion_contents"))
            .and_then(|contents| contents.get("potion").or(Some(contents)))
            .and_then(Value::as_str)
            .map(with_namespace),
    })
}

/// The potions a recipe will accept in the bottle: one name, or a list.
fn potions_of(input: &Value) -> Option<Vec<String>> {
    let potions = input.get("potion_contents")?.get("potions")?;
    match potions {
        Value::String(name) => Some(vec![with_namespace(name)]),
        Value::Array(list) => Some(list.iter().filter_map(Value::as_str).map(with_namespace).collect()),
        _ => None,
    }
}

fn with_namespace(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// Which potion a bottle holds, by name.
fn potion_name(server: &Arc<Server>, bottle: &ItemStack) -> Option<String> {
    let id = potion_in(&bottle.patch)?;
    server.data.registries.name_of("potion", id).map(str::to_owned)
}

/// Brings a stand into memory, from its block entity if it has one.
pub fn state(server: &Arc<Server>, pos: BlockPos) -> Stand {
    if let Some(found) = server.stands.working.lock().unwrap_or_else(|e| e.into_inner()).get(&pos) {
        return found.clone();
    }
    read(server, pos)
}

fn read(server: &Arc<Server>, pos: BlockPos) -> Stand {
    let items = containers::read_items(server, pos, SLOTS);
    let compound = containers::block_entity_at(server, pos);
    let number = |name: &str| compound.as_ref().and_then(|c| c.get_i32(name)).unwrap_or(0);
    Stand {
        items,
        brew_time: number("BrewTime"),
        brew_total: number("total_brew_time").max(1),
        fuel: number("Fuel"),
        fuel_total: number("total_fuel").max(1),
        speed: 1.0,
        ingredient: 0,
    }
}

/// Writes a stand back to the world, and forgets it if it has nothing left
/// to do.
pub fn store(server: &Arc<Server>, pos: BlockPos, stand: Stand) {
    containers::write_items(server, pos, &stand.items);
    containers::set_block_entity_numbers(
        server,
        pos,
        &[
            ("BrewTime", stand.brew_time),
            ("total_brew_time", stand.brew_total),
            ("Fuel", stand.fuel),
            ("total_fuel", stand.fuel_total),
        ],
    );
    let mut working = server.stands.working.lock().unwrap_or_else(|e| e.into_inner());
    if busy(&stand) {
        working.insert(pos, stand);
    } else {
        working.remove(&pos);
    }
}

/// Whether a stand is worth ticking: it has fuel to spend or a brew going.
fn busy(stand: &Stand) -> bool {
    stand.brew_time > 0 || stand.fuel > 0 || !stand.items[FUEL].is_empty() || !stand.items[INGREDIENT].is_empty()
}

/// A chunk came back: pick up any stands in it that were left working.
pub fn load_chunk(server: &Arc<Server>, chunk: garnet_protocol::ChunkPos) {
    let positions: Vec<BlockPos> = {
        let mut world = server.world();
        let Ok(loaded) = world.chunk_mut(chunk) else { return };
        loaded
            .block_entities
            .iter()
            .filter(|be| be.get_str("id") == Some("minecraft:brewing_stand"))
            .filter_map(|be| Some(BlockPos::new(be.get_i32("x")?, be.get_i32("y")?, be.get_i32("z")?)))
            .collect()
    };
    for pos in positions {
        let stand = read(server, pos);
        if busy(&stand) {
            server
                .stands
                .working
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(pos, stand);
        }
    }
}

/// Writes every stand back, for a save or a shutdown.
pub fn save_all(server: &Arc<Server>) {
    let all: Vec<(BlockPos, Stand)> = {
        let working = server.stands.working.lock().unwrap_or_else(|e| e.into_inner());
        working.iter().map(|(pos, stand)| (*pos, stand.clone())).collect()
    };
    for (pos, stand) in all {
        containers::write_items(server, pos, &stand.items);
        containers::set_block_entity_numbers(
            server,
            pos,
            &[
                ("BrewTime", stand.brew_time),
                ("total_brew_time", stand.brew_total),
                ("Fuel", stand.fuel),
                ("total_fuel", stand.fuel_total),
            ],
        );
    }
}

/// One tick for every stand with something to do.
pub fn tick(server: &Arc<Server>) {
    let positions: Vec<BlockPos> = {
        let working = server.stands.working.lock().unwrap_or_else(|e| e.into_inner());
        working.keys().copied().collect()
    };
    for pos in positions {
        let Some(mut stand) = server
            .stands
            .working
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pos)
            .cloned()
        else {
            continue;
        };
        if block_name(server, pos) != "minecraft:brewing_stand" {
            // Someone took it away.
            server.stands.working.lock().unwrap_or_else(|e| e.into_inner()).remove(&pos);
            continue;
        }
        step(server, pos, &mut stand);
        {
            let mut working = server.stands.working.lock().unwrap_or_else(|e| e.into_inner());
            if busy(&stand) {
                working.insert(pos, stand.clone());
            } else {
                working.remove(&pos);
            }
        }
        show_bottles(server, pos, &stand);
        if !busy(&stand) || stand.brew_time == 0 {
            store(server, pos, stand.clone());
        }
        show(server, pos, &stand);
    }
}

/// Moves one stand on by a tick, the way vanilla does.
fn step(server: &Arc<Server>, pos: BlockPos, stand: &mut Stand) {
    // Fuel first: an empty stand takes one piece and remembers what it is
    // worth.
    if stand.fuel <= 0 {
        let fuel = stand.items[FUEL].clone();
        let name = crate::items::item_name(server, fuel.item);
        if let Some((uses, speed)) = fuel_value(server, &name) {
            if !fuel.is_empty() && uses > 0 {
                stand.fuel = uses;
                stand.fuel_total = uses;
                stand.speed = speed;
                let slot = &mut stand.items[FUEL];
                slot.count -= 1;
                if slot.count <= 0 {
                    *slot = ItemStack::EMPTY;
                }
            }
        }
    }

    let brewable = brewable(server, stand);
    let ingredient = stand.items[INGREDIENT].item;
    if stand.brew_time > 0 {
        stand.brew_time -= 1;
        if stand.brew_time == 0 && brewable {
            brew(server, pos, stand);
        } else if !brewable || ingredient != stand.ingredient {
            // The ingredient changed, or there is nothing to brew: stop.
            stand.brew_time = 0;
        }
    } else if brewable && stand.fuel > 0 {
        let speed = if stand.speed > 0.0 { stand.speed } else { 1.0 };
        stand.fuel -= 1;
        stand.brew_time = (BREW_TICKS as f32 / speed).ceil() as i32;
        stand.brew_total = stand.brew_time;
        stand.ingredient = ingredient;
    }
}

/// Whether anything in the three bottles would change.
fn brewable(server: &Arc<Server>, stand: &Stand) -> bool {
    let reagent = &stand.items[INGREDIENT];
    if reagent.is_empty() {
        return false;
    }
    (0..BOTTLES).any(|slot| server.brewing.result(server, &stand.items[slot], reagent).is_some())
}

/// The brew finished: every bottle it applies to becomes what it makes,
/// and one ingredient is spent.
fn brew(server: &Arc<Server>, pos: BlockPos, stand: &mut Stand) {
    let reagent = stand.items[INGREDIENT].clone();
    for slot in 0..BOTTLES {
        if let Some(result) = server.brewing.result(server, &stand.items[slot], &reagent) {
            stand.items[slot] = result;
        }
    }
    // Whatever the ingredient leaves behind goes in its place, as a bucket
    // does in a furnace.
    let name = crate::items::item_name(server, reagent.item);
    let left = server
        .data
        .item_components
        .use_remainder(&name)
        .and_then(|item| crate::items::item_id(server, item));
    let slot = &mut stand.items[INGREDIENT];
    slot.count -= 1;
    if slot.count <= 0 {
        *slot = match left {
            Some(id) => ItemStack::new(id, 1),
            None => ItemStack::EMPTY,
        };
    }
    sound(server, pos);
}

/// How many brews a fuel is worth, and how fast it makes them.
fn fuel_value(server: &Arc<Server>, item: &str) -> Option<(i32, f32)> {
    let (uses, speed) = server.data.item_components.brewing_fuel(item)?;
    Some((server.data.providers.int(uses, 20), server.data.providers.float(speed, 1.0)))
}

/// Which of the three bottle slots are filled.
fn bottles_in(stand: &Stand) -> [bool; BOTTLES] {
    [
        !stand.items[0].is_empty(),
        !stand.items[1].is_empty(),
        !stand.items[2].is_empty(),
    ]
}

/// The block itself shows how many bottles are in it. Nothing is written
/// unless it has actually changed, so this can be called freely.
fn show_bottles(server: &Arc<Server>, pos: BlockPos, stand: &Stand) {
    let Ok(state) = server.world().get_block(pos) else { return };
    let Some(current) = server.data.blocks.state(state as i32) else {
        return;
    };
    let mut props = current.properties.clone();
    let mut changed = false;
    for (slot, filled) in bottles_in(stand).iter().enumerate() {
        let key = format!("has_bottle_{slot}");
        let wanted = filled.to_string();
        changed |= props.get(&key) != Some(&wanted);
        props.insert(key, wanted);
    }
    if !changed {
        return;
    }
    if let Some(next) = server.data.blocks.state_with("minecraft:brewing_stand", &props) {
        server.set_block(pos, next as u32);
    }
}

fn block_name(server: &Arc<Server>, pos: BlockPos) -> String {
    let Ok(state) = server.world().get_block(pos) else {
        return String::new();
    };
    server
        .data
        .blocks
        .block_of_state(state as i32)
        .map(|block| block.name.clone())
        .unwrap_or_default()
}

fn sound(server: &Arc<Server>, pos: BlockPos) {
    let Some(name) = garnet_protocol::Identifier::parse("minecraft:block.brewing_stand.brew") else {
        return;
    };
    let registry_id = server.data.registries.id_of("sound_event", &name.to_string());
    server.broadcast_near(
        garnet_protocol::ChunkPos::from_block(pos.x, pos.z),
        &cb::Sound {
            name,
            registry_id,
            source: cb::SoundSource::Blocks,
            x: pos.x as f64 + 0.5,
            y: pos.y as f64 + 0.5,
            z: pos.z as f64 + 0.5,
            volume: 1.0,
            pitch: 1.0,
            seed: rand::random(),
        },
        None,
    );
}

/// Opens a stand for a player.
pub fn open(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos) -> bool {
    let stand = state(server, pos);
    let window_id = {
        let mut s = player.lock();
        s.next_window_id = s.next_window_id % 100 + 1;
        let id = s.next_window_id;
        s.container = Some(containers::OpenContainer {
            pos,
            window_id: id,
            size: SLOTS,
            kind: containers::Kind::Brewing,
        });
        id
    };
    let menu_type = server.data.registries.id_of("menu", "minecraft:brewing_stand").unwrap_or(0);
    player.send(&cb::OpenScreen {
        window_id,
        menu_type,
        title: Text::new("Brewing Stand"),
    });
    containers::send_window(server, player, &stand.items);
    show(server, pos, &stand);
    true
}

/// Someone moved something in an open stand.
pub fn touched(server: &Arc<Server>, pos: BlockPos, items: Vec<ItemStack>) {
    let mut stand = state(server, pos);
    stand.items = items;
    show_bottles(server, pos, &stand);
    store(server, pos, stand);
}

/// Sends the bubbles and the fuel gauge to whoever is watching.
fn show(server: &Arc<Server>, pos: BlockPos, stand: &Stand) {
    for player in server.online_players() {
        let Some(open) = player.lock().container.clone() else {
            continue;
        };
        if open.pos != pos || open.kind != containers::Kind::Brewing {
            continue;
        }
        for (property, value) in [
            (0, stand.brew_time),
            (1, stand.fuel),
            (2, stand.brew_total),
            (3, stand.fuel_total),
        ] {
            player.send(&cb::ContainerSetData {
                window_id: open.window_id,
                property,
                value,
            });
        }
    }
}
