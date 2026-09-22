//! The enchanting table.
//!
//! Vanilla's shape, and as far as we can manage vanilla's arithmetic.
//! Bookshelves around the table raise what it can offer, three offers are
//! drawn from a seed the player carries so they hold still between clicks,
//! and taking one spends levels and lapis. Which enchantments may appear,
//! at what level, how heavily they are weighted and what they clash with
//! all come from the game's own files, so a data pack that changes them
//! changes this too.

use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::{enchantments_in, stored_enchantments_in, ItemStack, PatchBuilder};
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{BlockPos, Text};
use serde_json::Value;
use std::sync::Arc;

pub const ITEM: usize = 0;
pub const LAPIS: usize = 1;
pub const SLOTS: usize = 2;
/// The most bookshelves a table pays attention to.
const MAX_POWER: i32 = 15;
/// The enchantments a table may offer at all.
const TABLE_TAG: &str = "minecraft:in_enchanting_table";

/// One enchantment, as the data pack describes it.
#[derive(Clone, Debug)]
pub struct Enchantment {
    pub name: String,
    pub id: i32,
    pub weight: i32,
    pub max_level: i32,
    /// What a level of it costs on an anvil.
    pub anvil_cost: i32,
    min_cost: (i32, i32),
    max_cost: (i32, i32),
    /// The item or `#tag` it goes on by itself.
    primary: String,
    /// The item or `#tag` an anvil will put it on.
    supported: String,
    /// Tags and names it will not share an item with.
    exclusive: Vec<String>,
}

impl Enchantment {
    /// The window of enchanting power this level can be rolled at.
    fn cost_range(&self, level: i32) -> (i32, i32) {
        (
            self.min_cost.0 + self.min_cost.1 * (level - 1),
            self.max_cost.0 + self.max_cost.1 * (level - 1),
        )
    }
}

/// Every enchantment the data pack defines.
#[derive(Default)]
pub struct Enchantments {
    all: Vec<Enchantment>,
}

impl Enchantments {
    pub fn load(data: &garnet_data::GameData) -> Self {
        let dir = data.version_dir.join("datapack").join("minecraft").join("enchantment");
        let mut all = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            tracing::warn!("no enchantment definitions in {}", dir.display());
            return Self { all };
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let name = format!("minecraft:{stem}");
            let Some(id) = data.dynamic.id_of("enchantment", &name) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            // Costs are written as a base plus a step for each level above
            // the first.
            let pair = |key: &str| {
                let node = json.get(key);
                (
                    node.and_then(|n| n.get("base")).and_then(Value::as_i64).unwrap_or(1) as i32,
                    node.and_then(|n| n.get("per_level_above_first"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0) as i32,
                )
            };
            let exclusive = match json.get("exclusive_set") {
                Some(Value::String(tag)) => vec![tag.clone()],
                Some(Value::Array(list)) => list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
                _ => Vec::new(),
            };
            let text_of = |key: &str| json.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
            let supported = text_of("supported_items");
            let primary = match text_of("primary_items") {
                empty if empty.is_empty() => supported.clone(),
                primary => primary,
            };
            all.push(Enchantment {
                name,
                id,
                weight: json.get("weight").and_then(Value::as_i64).unwrap_or(1) as i32,
                max_level: json.get("max_level").and_then(Value::as_i64).unwrap_or(1) as i32,
                anvil_cost: json.get("anvil_cost").and_then(Value::as_i64).unwrap_or(1) as i32,
                min_cost: pair("min_cost"),
                max_cost: pair("max_cost"),
                primary,
                supported,
                exclusive,
            });
        }
        tracing::info!("loaded {} enchantments", all.len());
        Self { all }
    }

    pub fn by_id(&self, id: i32) -> Option<&Enchantment> {
        self.all.iter().find(|e| e.id == id)
    }

    /// What this item could be given at this much enchanting power: the
    /// highest level of each enchantment whose window the power falls in.
    fn results(&self, server: &Arc<Server>, item: &str, power: i32) -> Vec<(Enchantment, i32)> {
        // A book takes anything the table knows; everything else only what
        // it is the proper item for.
        let book = item == "minecraft:book";
        let offered = server.data.tags.members("minecraft:enchantment", TABLE_TAG);
        let mut out = Vec::new();
        for enchantment in &self.all {
            if let Some(offered) = offered {
                if !offered.contains(&enchantment.id) {
                    continue;
                }
            }
            if !book && !item_matches(server, &enchantment.primary, item) {
                continue;
            }
            for level in (1..=enchantment.max_level).rev() {
                let (low, high) = enchantment.cost_range(level);
                if power >= low && power <= high {
                    out.push((enchantment.clone(), level));
                    break;
                }
            }
        }
        out
    }

    /// Whether an anvil would put this enchantment on this item.
    pub fn goes_on(&self, server: &Arc<Server>, id: i32, item: &str) -> bool {
        self.by_id(id)
            .is_some_and(|enchantment| item_matches(server, &enchantment.supported, item))
    }

    /// The highest level this enchantment goes to.
    pub fn max_level(&self, id: i32) -> i32 {
        self.by_id(id).map(|e| e.max_level).unwrap_or(1)
    }

    /// What joining this enchantment costs on an anvil. A book is half
    /// the price of the same enchantment on the thing itself.
    pub fn anvil_cost(&self, id: i32, level: i32, from_book: bool) -> i32 {
        let each = self.by_id(id).map(|e| e.anvil_cost).unwrap_or(1);
        let each = if from_book { (each / 2).max(1) } else { each };
        each * level.max(1)
    }

    /// Whether two enchantments will not sit on the same item.
    pub fn clash(&self, server: &Arc<Server>, one: i32, other: i32) -> bool {
        if one == other {
            return true;
        }
        let (Some(one), Some(other)) = (self.by_id(one), self.by_id(other)) else {
            return false;
        };
        excludes(server, one, other) || excludes(server, other, one)
    }
}

fn excludes(server: &Arc<Server>, one: &Enchantment, other: &Enchantment) -> bool {
    one.exclusive.iter().any(|set| match set.strip_prefix('#') {
        Some(tag) => server
            .data
            .tags
            .members("minecraft:enchantment", tag)
            .is_some_and(|members| members.contains(&other.id)),
        None => *set == other.name,
    })
}

/// What an item is already enchanted with. An enchanted book keeps its
/// enchantments in a pocket of its own, to be moved onto something else
/// on an anvil.
fn already_on(item: &str, patch: &[u8]) -> Vec<(i32, i32)> {
    if item == "minecraft:enchanted_book" {
        return stored_enchantments_in(patch).unwrap_or_default();
    }
    enchantments_in(patch).unwrap_or_default()
}

/// Whether an item is the one named, or in the tag named.
fn item_matches(server: &Arc<Server>, wanted: &str, item: &str) -> bool {
    if wanted.is_empty() {
        return false;
    }
    match wanted.strip_prefix('#') {
        Some(tag) => {
            let Some(members) = server.data.tags.members("minecraft:item", tag) else {
                return false;
            };
            let Some(id) = server.data.registries.id_of("item", item) else {
                return false;
            };
            members.contains(&id)
        }
        None => wanted == item,
    }
}

/// `java.util.Random`, so our rolls land where vanilla's would.
struct Rolls {
    seed: i64,
}

impl Rolls {
    const MULTIPLIER: i64 = 0x5DEECE66D;
    const MASK: i64 = (1 << 48) - 1;

    fn new(seed: i64) -> Self {
        Self {
            seed: (seed ^ Self::MULTIPLIER) & Self::MASK,
        }
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(Self::MULTIPLIER).wrapping_add(0xB) & Self::MASK;
        (self.seed >> (48 - bits)) as i32
    }

    fn int(&mut self, bound: i32) -> i32 {
        if bound <= 0 {
            return 0;
        }
        if bound & -bound == bound {
            // A power of two needs no rejection.
            return ((bound as i64).wrapping_mul(self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let value = bits % bound;
            // Reject the tail that would skew the spread.
            if bits - value + (bound - 1) >= 0 {
                return value;
            }
        }
    }

    fn float(&mut self) -> f32 {
        self.next(24) as f32 / (1 << 24) as f32
    }
}

/// Bookshelves around a table, the way vanilla counts them: a ring two
/// blocks out, level with the table and one above, each shelf needing a
/// clear space between it and the table.
fn power(server: &Arc<Server>, pos: BlockPos) -> i32 {
    let mut found = 0;
    for dz in -1..=1i32 {
        for dx in -1..=1i32 {
            if dx == 0 && dz == 0 {
                continue;
            }
            for dy in 0..=1 {
                if !is_air(server, pos.offset(dx, dy, dz)) {
                    continue; // the line of sight is blocked
                }
                if is_bookshelf(server, pos.offset(dx * 2, dy, dz * 2)) {
                    found += 1;
                }
                if dx != 0 && dz != 0 {
                    // The corners of the ring hold two more each.
                    if is_bookshelf(server, pos.offset(dx * 2, dy, dz)) {
                        found += 1;
                    }
                    if is_bookshelf(server, pos.offset(dx, dy, dz * 2)) {
                        found += 1;
                    }
                }
            }
        }
    }
    found.min(MAX_POWER)
}

fn is_bookshelf(server: &Arc<Server>, pos: BlockPos) -> bool {
    block_name(server, pos) == "minecraft:bookshelf"
}

fn is_air(server: &Arc<Server>, pos: BlockPos) -> bool {
    let Ok(state) = server.world().get_block(pos) else {
        return false;
    };
    server.data.blocks.is_air(state as i32)
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

/// What the three slots ask for, in levels.
fn costs(server: &Arc<Server>, item: &str, shelves: i32, seed: i64) -> [i32; 3] {
    if server.data.item_components.enchantable(item) <= 0 {
        return [0, 0, 0];
    }
    let mut rolls = Rolls::new(seed);
    let mut out = [0; 3];
    for (slot, cost) in out.iter_mut().enumerate() {
        let base = rolls.int(8) + 1 + (shelves >> 1) + rolls.int(shelves + 1);
        *cost = match slot {
            0 => (base / 3).max(1),
            1 => base * 2 / 3 + 1,
            _ => base.max(shelves * 2),
        };
    }
    out
}

/// Rolls what one offer actually gives.
fn roll(server: &Arc<Server>, item: &str, cost: i32, seed: i64) -> Vec<(i32, i32)> {
    let enchantability = server.data.item_components.enchantable(item);
    if enchantability <= 0 || cost <= 0 {
        return Vec::new();
    }
    let mut rolls = Rolls::new(seed);
    // Vanilla stirs the cost about before looking anything up, so the same
    // offer is worth a little more or less each time.
    let step = enchantability / 4 + 1;
    let mut power = cost + 1 + rolls.int(step) + rolls.int(step);
    let spread = (rolls.float() + rolls.float() - 1.0) * 0.15;
    power = ((power as f32 + power as f32 * spread).round() as i32).max(1);

    let mut available = server.enchantments.results(server, item, power);
    let mut chosen: Vec<(Enchantment, i32)> = Vec::new();
    let Some(first) = weighted(&mut rolls, &mut available) else {
        return Vec::new();
    };
    chosen.push(first);
    // Each further enchantment is less likely than the last.
    while rolls.int(50) <= power {
        if let Some((last, _)) = chosen.last().cloned() {
            available.retain(|(other, _)| !server.enchantments.clash(server, last.id, other.id));
        }
        let Some(next) = weighted(&mut rolls, &mut available) else {
            break;
        };
        chosen.push(next);
        power /= 2;
    }
    // A book gives one of them back, so a whole set is never had at once.
    if item == "minecraft:book" && chosen.len() > 1 {
        let dropped = rolls.int(chosen.len() as i32) as usize;
        chosen.remove(dropped);
    }
    chosen.into_iter().map(|(e, level)| (e.id, level)).collect()
}

/// Draws one enchantment by weight and takes it out of the running.
fn weighted(rolls: &mut Rolls, available: &mut Vec<(Enchantment, i32)>) -> Option<(Enchantment, i32)> {
    let total: i32 = available.iter().map(|(e, _)| e.weight).sum();
    if total <= 0 {
        return None;
    }
    let mut roll = rolls.int(total);
    let index = available.iter().position(|(e, _)| {
        roll -= e.weight;
        roll < 0
    })?;
    Some(available.remove(index))
}

/// Opens a table for a player.
pub fn open(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos) -> bool {
    let window_id = {
        let mut s = player.lock();
        s.next_window_id = s.next_window_id % 100 + 1;
        let id = s.next_window_id;
        s.enchanting = vec![ItemStack::EMPTY; SLOTS];
        if s.enchant_seed == 0 {
            s.enchant_seed = fresh_seed();
        }
        s.container = Some(crate::containers::OpenContainer {
            pos,
            window_id: id,
            size: SLOTS,
            kind: crate::containers::Kind::Enchanting,
        });
        id
    };
    let menu_type = server.data.registries.id_of("menu", "minecraft:enchantment").unwrap_or(0);
    player.send(&cb::OpenScreen {
        window_id,
        menu_type,
        title: Text::new("Enchant"),
    });
    refresh(server, player);
    true
}

/// Works out the three offers and tells the client about them.
pub fn refresh(server: &Arc<Server>, player: &Arc<Player>) {
    let Some(open) = player.lock().container.clone() else {
        return;
    };
    if open.kind != crate::containers::Kind::Enchanting {
        return;
    }
    let (slots, seed) = {
        let s = player.lock();
        (s.enchanting.clone(), s.enchant_seed)
    };
    let item = slots.first().cloned().unwrap_or(ItemStack::EMPTY);
    let lapis = slots.get(LAPIS).map(|stack| stack.count).unwrap_or(0);
    let shelves = power(server, open.pos);

    let mut levels = [0; 3];
    // The hint the client shows in each slot: one enchantment and its level.
    let mut hints = [-1; 3];
    let mut hint_levels = [-1; 3];
    let name = crate::items::item_name(server, item.item);
    // Nothing is offered for an item that is already enchanted.
    if !item.is_empty() && already_on(&name, &item.patch).is_empty() {
        levels = costs(server, &name, shelves, seed);
        for slot in 0..3 {
            let rolled = roll(server, &name, levels[slot], seed.wrapping_add(slot as i64));
            match rolled.first() {
                // A slot with no lapis behind it is not offered.
                Some(&(id, level)) if lapis > slot as i32 => {
                    hints[slot] = id;
                    hint_levels[slot] = level;
                }
                _ => levels[slot] = 0,
            }
        }
    }
    for (property, value) in [
        (0, levels[0]),
        (1, levels[1]),
        (2, levels[2]),
        (3, (seed & 0xFFFF_FFF0) as i32),
        (4, hints[0]),
        (5, hints[1]),
        (6, hints[2]),
        (7, hint_levels[0]),
        (8, hint_levels[1]),
        (9, hint_levels[2]),
    ] {
        player.send(&cb::ContainerSetData {
            window_id: open.window_id,
            property,
            value,
        });
    }
    crate::containers::send_window(server, player, &slots);
}

/// The player clicked one of the three offers.
pub fn choose(server: &Arc<Server>, player: &Arc<Player>, offer: i32) {
    let Some(open) = player.lock().container.clone() else {
        return;
    };
    if open.kind != crate::containers::Kind::Enchanting || !(0..3).contains(&offer) {
        return;
    }
    let slot = offer as usize;
    let (slots, seed, mode, level) = {
        let s = player.lock();
        (s.enchanting.clone(), s.enchant_seed, s.game_mode, s.xp_level)
    };
    let item = slots.first().cloned().unwrap_or(ItemStack::EMPTY);
    let name = crate::items::item_name(server, item.item);
    if item.is_empty() || !already_on(&name, &item.patch).is_empty() {
        return;
    }
    let shelves = power(server, open.pos);
    let cost = costs(server, &name, shelves, seed)[slot];
    // The offer asks to be that many levels deep, but only takes the slot's
    // own worth: one level and one lapis for the first, three for the last.
    let spent = offer + 1;
    let lapis = slots.get(LAPIS).map(|stack| stack.count).unwrap_or(0);
    let free = matches!(mode, GameMode::Creative);
    if cost == 0 || (!free && (level < cost || level < spent || lapis < spent)) {
        return;
    }
    let rolled = roll(server, &name, cost, seed.wrapping_add(slot as i64));
    if rolled.is_empty() {
        return;
    }
    if !free && !crate::experience::take_levels(player, spent) {
        return;
    }
    {
        let mut s = player.lock();
        // A book becomes an enchanted book, which holds its enchantments
        // for an anvil to move onto something else.
        let becomes_book = name == "minecraft:book";
        let patch = if becomes_book {
            PatchBuilder::default().stored_enchantments(&rolled).build()
        } else {
            PatchBuilder::default().enchantments(&rolled).build()
        };
        s.enchanting[ITEM] = ItemStack {
            item: match becomes_book {
                true => server
                    .data
                    .registries
                    .id_of("item", "minecraft:enchanted_book")
                    .unwrap_or(item.item),
                false => item.item,
            },
            count: item.count,
            patch,
        };
        if !free {
            let lapis = &mut s.enchanting[LAPIS];
            lapis.count -= spent;
            if lapis.count <= 0 {
                *lapis = ItemStack::EMPTY;
            }
        }
        // A fresh seed, so the next three offers are not the same three.
        s.enchant_seed = fresh_seed();
    }
    let given: Vec<String> = rolled
        .iter()
        .map(|(id, level)| {
            let enchantment = server
                .enchantments
                .by_id(*id)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| id.to_string());
            format!("{enchantment} {level}")
        })
        .collect();
    tracing::debug!("{} enchanted {name} with {}", player.name(), given.join(", "));
    refresh(server, player);
}

fn fresh_seed() -> i64 {
    rand::random::<i64>() & 0x7FFF_FFFF
}
