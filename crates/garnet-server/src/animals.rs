//! The farm: feeding animals, breeding them, shearing sheep and milking
//! cows.
//!
//! Vanilla's timings throughout. Two adults fed the right thing go into
//! love for half a minute, make one baby between them and will not breed
//! again for five minutes; a baby takes twenty minutes to grow up, and
//! every mouthful it is fed takes a tenth off what is left. A sheep sheared
//! gives one to three wool of its colour and grows it back in its own time;
//! a cow fills a bucket; a chicken lays an egg every few minutes.

use crate::player::Player;
use crate::server::Server;
use crate::world_entities::{self, Entity};
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::ItemStack;
use garnet_protocol::packets::play::GameMode;
use std::sync::Arc;

/// How long a baby takes to grow up.
const GROWING: u64 = 24000;
/// How long love lasts, and how long until an animal will breed again.
const IN_LOVE: u64 = 600;
const BREED_AGAIN: u64 = 6000;
/// How close two animals in love have to be to find each other.
const COURTING: f64 = 8.0;
/// How long a sheared sheep takes to grow its wool back.
const REGROW: u64 = 2400;
/// How often a chicken lays.
const LAYING: (u64, u64) = (6000, 12000);
/// How far an animal follows someone holding its food.
const FOLLOW: f64 = 10.0;

/// What an animal is doing with itself.
#[derive(Clone, Debug, Default)]
pub struct Animal {
    pub baby: bool,
    /// When a baby becomes an adult.
    pub grows_up: u64,
    /// When love wears off; zero when it is not in love.
    pub in_love_until: u64,
    /// The soonest it will breed again.
    pub ready_at: u64,
    /// A sheep's colour, and whether its wool is off.
    pub wool: u8,
    pub sheared: bool,
    /// When a sheared sheep gets its wool back.
    pub regrows_at: u64,
    /// When a chicken next lays.
    pub lays_at: u64,
}

/// Every animal this module looks after, and what it eats.
pub fn food_for(kind: &str) -> &'static [&'static str] {
    match kind.strip_prefix("minecraft:").unwrap_or(kind) {
        "cow" | "sheep" => &["minecraft:wheat"],
        "pig" => &["minecraft:carrot", "minecraft:potato", "minecraft:beetroot"],
        "chicken" => &[
            "minecraft:wheat_seeds",
            "minecraft:melon_seeds",
            "minecraft:pumpkin_seeds",
            "minecraft:beetroot_seeds",
        ],
        _ => &[],
    }
}

pub fn is_animal(kind: &str) -> bool {
    !food_for(kind).is_empty()
}

/// A new animal, of whatever age.
pub fn fresh(kind: &str, baby: bool, tick: u64) -> Animal {
    Animal {
        baby,
        grows_up: if baby { tick + GROWING } else { 0 },
        wool: if kind.ends_with("sheep") { random_wool() } else { 0 },
        lays_at: if kind.ends_with("chicken") {
            tick + rand::random_range(LAYING.0..LAYING.1)
        } else {
            0
        },
        ..Animal::default()
    }
}

/// Vanilla's flock: mostly white, a few of everything else, and the odd
/// pink one.
fn random_wool() -> u8 {
    match rand::random_range(0..1000) {
        0..=818 => 0,    // white
        819..=868 => 8,  // light grey
        869..=918 => 7,  // grey
        919..=968 => 15, // black
        969..=997 => 12, // brown
        _ => 6,          // pink
    }
}

/// A player right-clicked an animal. Returns whether anything came of it.
pub fn interact(server: &Arc<Server>, player: &Arc<Player>, target: i32) -> bool {
    let Some(entity) = server
        .entities
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .by_id
        .get(&target)
        .cloned()
    else {
        return false;
    };
    if !is_animal(&entity.kind) {
        return false;
    }
    let held = {
        let s = player.lock();
        let slot = s.held_slot;
        s.inventory.held(slot).clone()
    };
    if held.is_empty() {
        return false;
    }
    let item = crate::items::item_name(server, held.item);
    match item.as_str() {
        "minecraft:shears" if entity.kind.ends_with("sheep") => shear(server, player, &entity),
        "minecraft:bucket" if entity.kind.ends_with("cow") => milk(server, player),
        _ if food_for(&entity.kind).contains(&item.as_str()) => feed(server, player, &entity),
        _ => false,
    }
}

/// Feeding an adult puts it in love; feeding a baby hurries it along.
fn feed(server: &Arc<Server>, player: &Arc<Player>, entity: &Entity) -> bool {
    let tick = server.current_tick();
    let mut state = entity.animal.clone().unwrap_or_else(|| fresh(&entity.kind, false, tick));
    if state.baby {
        // A tenth off what growing up has left to run.
        let left = state.grows_up.saturating_sub(tick);
        state.grows_up = tick + left - left / 10;
    } else {
        if state.in_love_until > tick || state.ready_at > tick {
            return false; // busy, or not in the mood yet
        }
        state.in_love_until = tick + IN_LOVE;
    }
    store(server, entity.id, state);
    spend_held(server, player, 1);
    hearts(server, entity);
    true
}

/// Shearing a sheep: one to three wool of its colour, and the shears wear.
fn shear(server: &Arc<Server>, player: &Arc<Player>, entity: &Entity) -> bool {
    let tick = server.current_tick();
    let mut state = entity.animal.clone().unwrap_or_else(|| fresh(&entity.kind, false, tick));
    if state.sheared || state.baby {
        return false;
    }
    state.sheared = true;
    state.regrows_at = tick + REGROW;
    let colour = state.wool;
    store(server, entity.id, state);
    show(server, entity.id);
    let name = wool_item(colour);
    if let Some(id) = crate::items::item_id(server, name) {
        let count = rand::random_range(1..=3);
        world_entities::drop_item(
            server,
            ItemStack::new(id, count),
            entity.x,
            entity.y + 0.5,
            entity.z,
            (0.0, 0.1, 0.0),
            10,
        );
    }
    crate::durability::use_held(server, player, 1);
    true
}

/// A bucket held out at a cow comes back full.
fn milk(server: &Arc<Server>, player: &Arc<Player>) -> bool {
    let Some(milk) = crate::items::item_id(server, "minecraft:milk_bucket") else {
        return false;
    };
    let creative = matches!(player.lock().game_mode, GameMode::Creative);
    let over = {
        let mut s = player.lock();
        let slot = s.held_slot;
        if !creative {
            let stack = s.inventory.held_mut(slot);
            stack.count -= 1;
            if stack.count <= 0 {
                *stack = ItemStack::EMPTY;
            }
        }
        let stack = s.inventory.held_mut(slot);
        if stack.is_empty() {
            *stack = ItemStack::new(milk, 1);
            ItemStack::EMPTY
        } else {
            s.inventory.add(ItemStack::new(milk, 1), 1)
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

/// Growing up, falling out of love, wool coming back and eggs being laid.
pub fn tick(server: &Arc<Server>, tick: u64) {
    if tick % 20 != 0 {
        return;
    }
    let animals: Vec<Entity> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities.by_id.values().filter(|e| is_animal(&e.kind)).cloned().collect()
    };
    let mut courting: Vec<Entity> = Vec::new();
    for animal in animals {
        let Some(mut state) = animal.animal.clone() else {
            // An animal from an older save, or one summoned by hand.
            store(server, animal.id, fresh(&animal.kind, false, tick));
            continue;
        };
        let mut changed = false;
        if state.baby && tick >= state.grows_up {
            state.baby = false;
            state.grows_up = 0;
            changed = true;
        }
        if state.sheared && tick >= state.regrows_at {
            state.sheared = false;
            changed = true;
        }
        if state.in_love_until > 0 && tick >= state.in_love_until {
            state.in_love_until = 0;
            changed = true;
        } else if state.in_love_until > tick && !state.baby {
            courting.push(animal.clone());
        }
        if animal.kind.ends_with("chicken") && !state.baby && state.lays_at > 0 && tick >= state.lays_at {
            lay(server, &animal);
            state.lays_at = tick + rand::random_range(LAYING.0..LAYING.1);
            changed = true;
        }
        if changed {
            let sheared_or_grown = state.baby;
            store(server, animal.id, state);
            let _ = sheared_or_grown;
            show(server, animal.id);
        }
    }
    breed(server, courting, tick);
}

/// Two of a kind in love near each other make a third.
fn breed(server: &Arc<Server>, courting: Vec<Entity>, tick: u64) {
    let mut paired: Vec<i32> = Vec::new();
    for (index, one) in courting.iter().enumerate() {
        if paired.contains(&one.id) {
            continue;
        }
        let Some(other) = courting.iter().skip(index + 1).find(|other| {
            other.kind == one.kind
                && !paired.contains(&other.id)
                && ((one.x - other.x).powi(2) + (one.y - other.y).powi(2) + (one.z - other.z).powi(2)).sqrt() <= COURTING
        }) else {
            continue;
        };
        paired.push(one.id);
        paired.push(other.id);
        for parent in [one, other] {
            let mut state = parent.animal.clone().unwrap_or_default();
            state.in_love_until = 0;
            state.ready_at = tick + BREED_AGAIN;
            store(server, parent.id, state);
        }
        let (x, y, z) = ((one.x + other.x) / 2.0, one.y, (one.z + other.z) / 2.0);
        if let Some(mut baby) = world_entities::new_entity(server, &one.kind, x, y, z) {
            baby.health = crate::mobs::health_of(&one.kind);
            baby.animal = Some(fresh(&one.kind, true, tick));
            baby.persistent = true;
            let id = world_entities::spawn(server, baby);
            show(server, id);
        }
        crate::experience::drop_orbs(server, x, y, z, rand::random_range(1..=7));
    }
}

/// A chicken leaves an egg behind.
fn lay(server: &Arc<Server>, chicken: &Entity) {
    let Some(egg) = crate::items::item_id(server, "minecraft:egg") else {
        return;
    };
    world_entities::drop_item(
        server,
        ItemStack::new(egg, 1),
        chicken.x,
        chicken.y + 0.2,
        chicken.z,
        (0.0, 0.05, 0.0),
        10,
    );
}

/// Where an animal wants to be: near whoever is holding what it eats.
pub fn following(server: &Arc<Server>, animal: &Entity) -> Option<(f64, f64, f64)> {
    let food = food_for(&animal.kind);
    if food.is_empty() {
        return None;
    }
    for player in server.online_players() {
        let (x, y, z, held, mode) = {
            let s = player.lock();
            let slot = s.held_slot;
            (s.x, s.y, s.z, s.inventory.held(slot).clone(), s.game_mode)
        };
        if matches!(mode, GameMode::Spectator) || held.is_empty() {
            continue;
        }
        let distance = ((animal.x - x).powi(2) + (animal.y - y).powi(2) + (animal.z - z).powi(2)).sqrt();
        if distance > FOLLOW || distance < 2.0 {
            continue;
        }
        let item = crate::items::item_name(server, held.item);
        if food.contains(&item.as_str()) {
            return Some((x, y, z));
        }
    }
    None
}

/// The metadata that says how old an animal is, and what a sheep is
/// wearing.
pub fn metadata(entity: &Entity) -> Vec<(u8, i32, Vec<u8>)> {
    let Some(state) = &entity.animal else { return Vec::new() };
    let mut entries = Vec::new();
    if state.baby {
        entries.push((16u8, 8, vec![1]));
    }
    if entity.kind.ends_with("sheep") {
        let byte = state.wool & 0x0F | if state.sheared { 0x10 } else { 0 };
        entries.push((17u8, 0, vec![byte]));
    }
    entries
}

/// Tells everyone watching what an animal looks like now.
fn show(server: &Arc<Server>, id: i32) {
    let entity = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        match entities.by_id.get(&id) {
            Some(entity) => entity.clone(),
            None => return,
        }
    };
    let entries = metadata(&entity);
    if entries.is_empty() {
        return;
    }
    server.broadcast_near(entity.chunk(), &cb::SetEntityData { entity_id: id, entries }, None);
}

fn store(server: &Arc<Server>, id: i32, state: Animal) {
    let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(stored) = entities.by_id.get_mut(&id) {
        stored.animal = Some(state);
    }
}

/// The little hearts, or at least the sound of them.
fn hearts(server: &Arc<Server>, entity: &Entity) {
    let Some(name) = garnet_protocol::Identifier::parse("minecraft:entity.generic.eat") else {
        return;
    };
    let registry_id = server.data.registries.id_of("sound_event", &name.to_string());
    server.broadcast_near(
        entity.chunk(),
        &cb::Sound {
            name,
            registry_id,
            source: cb::SoundSource::Neutral,
            x: entity.x,
            y: entity.y,
            z: entity.z,
            volume: 1.0,
            pitch: 1.0,
            seed: rand::random(),
        },
        None,
    );
}

fn spend_held(server: &Arc<Server>, player: &Arc<Player>, count: i32) {
    if matches!(player.lock().game_mode, GameMode::Creative) {
        return;
    }
    let _ = server;
    {
        let mut s = player.lock();
        let slot = s.held_slot;
        let stack = s.inventory.held_mut(slot);
        stack.count -= count;
        if stack.count <= 0 {
            *stack = ItemStack::EMPTY;
        }
    }
    crate::items::sync_inventory(player);
}

fn wool_item(colour: u8) -> &'static str {
    match colour {
        1 => "minecraft:orange_wool",
        2 => "minecraft:magenta_wool",
        3 => "minecraft:light_blue_wool",
        4 => "minecraft:yellow_wool",
        5 => "minecraft:lime_wool",
        6 => "minecraft:pink_wool",
        7 => "minecraft:gray_wool",
        8 => "minecraft:light_gray_wool",
        9 => "minecraft:cyan_wool",
        10 => "minecraft:purple_wool",
        11 => "minecraft:blue_wool",
        12 => "minecraft:brown_wool",
        13 => "minecraft:green_wool",
        14 => "minecraft:red_wool",
        15 => "minecraft:black_wool",
        _ => "minecraft:white_wool",
    }
}
