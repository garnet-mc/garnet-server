//! Villagers: what they do for a living, and what they will trade you.
//!
//! In 26.3 the trades are written down at last -- one file per trade under
//! `villager_trade/`, gathered into a tag for each profession and level --
//! so this reads them rather than keeping a list of its own. A villager
//! takes its profession from the workstation it is standing near, offers
//! what its level allows, learns from every trade, and restocks twice a
//! day the way it does in vanilla.

use crate::player::Player;
use crate::server::Server;
use crate::world_entities::Entity;
use garnet_data::GameData;
use garnet_protocol::nbt::{NbtCompound, NbtTag};
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::ItemStack;
use garnet_protocol::{BlockPos, Text};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// The three slots of a trading window: two things offered (slots 0 and
/// 1), and what is taken away.
pub const RESULT: usize = 2;
pub const SLOTS: usize = 3;
/// What each level of the job costs in experience, from novice up.
const LEVEL_XP: [i32; 5] = [0, 10, 70, 150, 250];
/// How often a villager gets its stock back.
const RESTOCK_EVERY: u64 = 12000;
/// How far a villager looks for somewhere to work.
const WORKSTATION_RANGE: i32 = 5;

/// One thing a villager will do, as the data pack describes it.
#[derive(Clone, Debug)]
pub struct Trade {
    pub wants: (String, i32),
    pub also_wants: Option<(String, i32)>,
    pub gives: (String, i32),
    pub max_uses: i32,
    pub xp: i32,
    pub price_multiplier: f32,
    /// Trades that hand over enchanted gear roll it when the villager is
    /// first asked, the way an enchanting table would.
    pub enchant: Option<(i32, i32, String)>,
}

/// Every trade in the data pack, by profession and level.
#[derive(Default)]
pub struct Trades {
    by_job: HashMap<(String, i32), Vec<Trade>>,
}

impl Trades {
    pub fn load(data: &GameData) -> Self {
        let root = data.version_dir.join("datapack").join("minecraft");
        let mut by_job = HashMap::new();
        let mut loaded = 0usize;
        for job in PROFESSIONS.iter().map(|(job, _)| *job) {
            for level in 1..=5 {
                let tag = root
                    .join("tags")
                    .join("villager_trade")
                    .join(job)
                    .join(format!("level_{level}.json"));
                let mut names = Vec::new();
                collect(&root, &tag, &mut names, 0);
                let trades: Vec<Trade> = names.iter().filter_map(|name| read_trade(&root, name)).collect();
                loaded += trades.len();
                if !trades.is_empty() {
                    by_job.insert((job.to_owned(), level), trades);
                }
            }
        }
        tracing::info!("loaded {loaded} villager trades across {} jobs", PROFESSIONS.len());
        Self { by_job }
    }

    fn at(&self, job: &str, level: i32) -> &[Trade] {
        self.by_job.get(&(job.to_owned(), level)).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Follows a trade tag, and the tags it points at, down to plain names.
fn collect(root: &std::path::Path, tag_path: &std::path::Path, out: &mut Vec<String>, depth: usize) {
    if depth > 4 {
        return; // a tag that points at itself, somehow
    }
    let Ok(text) = std::fs::read_to_string(tag_path) else { return };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let Some(values) = json.get("values").and_then(Value::as_array) else {
        return;
    };
    for value in values {
        let Some(name) = value.as_str().or_else(|| value.get("id").and_then(Value::as_str)) else {
            continue;
        };
        match name.strip_prefix('#') {
            Some(other) => {
                let path = other.strip_prefix("minecraft:").unwrap_or(other);
                let next = root.join("tags").join("villager_trade").join(format!("{path}.json"));
                collect(root, &next, out, depth + 1);
            }
            None => out.push(name.strip_prefix("minecraft:").unwrap_or(name).to_owned()),
        }
    }
}

/// Reads one trade, or gives nothing for the ones we cannot honour.
fn read_trade(root: &std::path::Path, name: &str) -> Option<Trade> {
    let path = root.join("villager_trade").join(format!("{name}.json"));
    let text = std::fs::read_to_string(path).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    // A trade that depends on where the villager came from, or that hands
    // over a map of somewhere we would have to go and find, is left out
    // until we can do it properly.
    if json.get("merchant_predicate").is_some() {
        return None;
    }
    let enchant = match json.get("given_item_modifier") {
        None => None,
        Some(modifiers) => enchanting_modifier(modifiers)?,
    };
    let cost = |node: Option<&Value>| -> Option<(String, i32)> {
        let node = node?;
        let id = node.get("id").and_then(Value::as_str)?;
        let count = node.get("count").and_then(Value::as_i64).unwrap_or(1) as i32;
        Some((with_namespace(id), count))
    };
    Some(Trade {
        wants: cost(json.get("wants"))?,
        also_wants: cost(json.get("additional_wants")),
        gives: cost(json.get("gives"))?,
        max_uses: json.get("max_uses").and_then(Value::as_i64).unwrap_or(12) as i32,
        xp: json.get("xp").and_then(Value::as_i64).unwrap_or(0) as i32,
        price_multiplier: json.get("reputation_discount").and_then(Value::as_f64).unwrap_or(0.05) as f32,
        enchant,
    })
}

/// The one item modifier we can carry out: enchanting what is handed over.
/// Anything else means the trade is left out.
fn enchanting_modifier(modifiers: &Value) -> Option<Option<(i32, i32, String)>> {
    let list = match modifiers {
        Value::Array(list) => list.clone(),
        other => vec![other.clone()],
    };
    let mut found = None;
    for modifier in list {
        match modifier.get("type").and_then(Value::as_str) {
            Some("minecraft:enchant_with_levels") => {
                let levels = modifier.get("levels");
                let min = levels
                    .and_then(|l| l.get("min").or(Some(l)))
                    .and_then(Value::as_f64)
                    .unwrap_or(5.0) as i32;
                let max = levels
                    .and_then(|l| l.get("max").or(Some(l)))
                    .and_then(Value::as_f64)
                    .unwrap_or(19.0) as i32;
                let options = modifier
                    .get("options")
                    .and_then(Value::as_str)
                    .unwrap_or("#minecraft:on_traded_equipment")
                    .to_owned();
                found = Some((min, max, options));
            }
            // A filter that throws the item away if the enchanting did not
            // take is no trouble: ours always takes.
            Some("minecraft:filtered") => {}
            _ => return None,
        }
    }
    Some(found)
}

fn with_namespace(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// What a villager is, beyond a thing that walks about.
#[derive(Clone, Debug, Default)]
pub struct Villager {
    /// Empty until it finds somewhere to work.
    pub job: String,
    pub level: i32,
    pub xp: i32,
    pub offers: Vec<Offer>,
    /// When its stock comes back.
    pub restock_at: u64,
}

/// One of a villager's trades, as it stands today.
#[derive(Clone, Debug)]
pub struct Offer {
    pub wants: (i32, i32),
    pub also_wants: Option<(i32, i32)>,
    pub gives: ItemStack,
    pub uses: i32,
    pub max_uses: i32,
    pub xp: i32,
    pub price_multiplier: f32,
}

impl Offer {
    pub fn sold_out(&self) -> bool {
        self.uses >= self.max_uses
    }
}

/// The jobs, and the block a villager has to stand near to take one.
const PROFESSIONS: &[(&str, &str)] = &[
    ("armorer", "minecraft:blast_furnace"),
    ("butcher", "minecraft:smoker"),
    ("cartographer", "minecraft:cartography_table"),
    ("cleric", "minecraft:brewing_stand"),
    ("farmer", "minecraft:composter"),
    ("fisherman", "minecraft:barrel"),
    ("fletcher", "minecraft:fletching_table"),
    ("leatherworker", "minecraft:cauldron"),
    ("librarian", "minecraft:lectern"),
    ("mason", "minecraft:stonecutter"),
    ("shepherd", "minecraft:loom"),
    ("toolsmith", "minecraft:smithing_table"),
    ("weaponsmith", "minecraft:grindstone"),
];

/// The job a block gives, if it gives one.
fn job_of_block(block: &str) -> Option<&'static str> {
    PROFESSIONS.iter().find(|(_, station)| *station == block).map(|(job, _)| *job)
}

/// Looks around a villager for somewhere to work.
fn find_work(server: &Arc<Server>, entity: &Entity) -> Option<String> {
    let here = BlockPos::new(entity.x.floor() as i32, entity.y.floor() as i32, entity.z.floor() as i32);
    let mut world = server.world();
    for dy in -2..=2 {
        for dz in -WORKSTATION_RANGE..=WORKSTATION_RANGE {
            for dx in -WORKSTATION_RANGE..=WORKSTATION_RANGE {
                let pos = here.offset(dx, dy, dz);
                let Ok(state) = world.get_block(pos) else { continue };
                let Some(block) = server.data.blocks.block_of_state(state as i32) else {
                    continue;
                };
                if let Some(job) = job_of_block(&block.name) {
                    return Some(job.to_owned());
                }
            }
        }
    }
    None
}

/// Every so often: villagers without a job look for one, and everyone
/// gets their stock back.
pub fn tick(server: &Arc<Server>, tick: u64) {
    if tick % 100 != 0 {
        return;
    }
    let villagers: Vec<Entity> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities
            .by_id
            .values()
            .filter(|entity| entity.kind == "minecraft:villager")
            .cloned()
            .collect()
    };
    for entity in villagers {
        let mut state = entity.villager.clone().unwrap_or_default();
        let mut changed = false;
        if state.job.is_empty() {
            if let Some(job) = find_work(server, &entity) {
                state.job = job;
                state.level = 1;
                state.offers = roll_offers(server, &state);
                state.restock_at = tick + RESTOCK_EVERY;
                changed = true;
                tracing::debug!("a villager took up {}", state.job);
            }
        } else if tick >= state.restock_at {
            for offer in &mut state.offers {
                offer.uses = 0;
            }
            state.restock_at = tick + RESTOCK_EVERY;
            changed = true;
        }
        if changed {
            store(server, entity.id, state);
        }
    }
}

/// Picks what this villager will offer at its level: two new trades for
/// each level it has reached.
fn roll_offers(server: &Arc<Server>, state: &Villager) -> Vec<Offer> {
    let mut offers = state.offers.clone();
    for level in 1..=state.level {
        // Only the trades this level brings, and only if it has none yet.
        if offers
            .iter()
            .any(|offer| offer.xp > 0 && offer.max_uses > 0 && level_of(offer.xp) == level)
        {
            continue;
        }
        let choices = server.trades.at(&state.job, level);
        if choices.is_empty() {
            continue;
        }
        let mut picked = 0;
        let mut tried = 0;
        while picked < 2 && tried < choices.len() * 2 {
            tried += 1;
            let trade = &choices[rand::random_range(0..choices.len())];
            let Some(offer) = build(server, trade) else { continue };
            if offers
                .iter()
                .any(|other| other.gives.item == offer.gives.item && other.wants == offer.wants)
            {
                continue;
            }
            offers.push(offer);
            picked += 1;
        }
    }
    offers
}

/// Which level a trade belongs to, judged by what it teaches.
fn level_of(xp: i32) -> i32 {
    match xp {
        0..=2 => 1,
        3..=5 => 2,
        6..=10 => 3,
        11..=15 => 4,
        _ => 5,
    }
}

/// Turns a trade from the data pack into an offer with real item ids.
fn build(server: &Arc<Server>, trade: &Trade) -> Option<Offer> {
    let wants = (crate::items::item_id(server, &trade.wants.0)?, trade.wants.1);
    let also_wants = match &trade.also_wants {
        Some((name, count)) => Some((crate::items::item_id(server, name)?, *count)),
        None => None,
    };
    let gives_id = crate::items::item_id(server, &trade.gives.0)?;
    let mut gives = ItemStack::new(gives_id, trade.gives.1);
    if let Some((min, max, options)) = &trade.enchant {
        let level = rand::random_range(*min..=*max);
        let rolled = server
            .enchantments
            .roll_for(server, &trade.gives.0, level, options, rand::random::<i64>() & 0x7FFF_FFFF);
        if !rolled.is_empty() {
            gives.patch = garnet_protocol::packets::play::items::PatchBuilder::default()
                .enchantments(&rolled)
                .build();
        }
    }
    Some(Offer {
        wants,
        also_wants,
        gives,
        uses: 0,
        max_uses: trade.max_uses,
        xp: trade.xp,
        price_multiplier: trade.price_multiplier,
    })
}

/// A villager's job and its trades, written the way vanilla writes them
/// so a world opened in a vanilla client finds its shopkeepers where it
/// left them. The one addition is `garnet_patch` beside a sold item, for
/// the components we keep our own way; vanilla ignores what it does not
/// know.
pub fn to_nbt(server: &Server, state: &Villager, now: u64, into: &mut NbtCompound) {
    let mut data = NbtCompound::new();
    data.put("profession", format!("minecraft:{}", state.job).as_str());
    data.put("level", state.level);
    let mut recipes = Vec::new();
    for offer in &state.offers {
        let mut recipe = NbtCompound::new();
        recipe.put("buy", cost_nbt(server, offer.wants));
        if let Some(second) = offer.also_wants {
            recipe.put("buyB", cost_nbt(server, second));
        }
        recipe.put("sell", stack_nbt(server, &offer.gives));
        recipe.put("uses", offer.uses);
        recipe.put("maxUses", offer.max_uses);
        recipe.put("rewardExp", offer.xp > 0);
        recipe.put("xp", offer.xp);
        recipe.put("priceMultiplier", offer.price_multiplier);
        recipes.push(NbtTag::Compound(recipe));
    }
    let mut offers = NbtCompound::new();
    offers.put("Recipes", recipes);
    into.put("VillagerData", data);
    into.put("Xp", state.xp);
    into.put("Offers", offers);
    into.put("RestockIn", state.restock_at.saturating_sub(now) as i32);
}

/// Reads a villager back out of a saved entity.
pub fn from_nbt(server: &Server, c: &NbtCompound, now: u64) -> Option<Villager> {
    let data = c.get_compound("VillagerData")?;
    let job = data.get_str("profession").unwrap_or("").to_owned();
    let job = job.strip_prefix("minecraft:").unwrap_or(&job).to_owned();
    let mut offers = Vec::new();
    if let Some(recipes) = c.get_compound("Offers").and_then(|o| o.get_list("Recipes")) {
        for recipe in recipes.iter().filter_map(NbtTag::as_compound) {
            let Some(wants) = cost_from(server, recipe.get_compound("buy")) else {
                continue;
            };
            let Some(gives) = stack_from(server, recipe.get_compound("sell")) else {
                continue;
            };
            offers.push(Offer {
                wants,
                also_wants: cost_from(server, recipe.get_compound("buyB")),
                gives,
                uses: recipe.get_i32("uses").unwrap_or(0),
                max_uses: recipe.get_i32("maxUses").unwrap_or(12),
                xp: recipe.get_i32("xp").unwrap_or(0),
                price_multiplier: recipe.get_f64("priceMultiplier").unwrap_or(0.05) as f32,
            });
        }
    }
    Some(Villager {
        job,
        level: data.get_i32("level").unwrap_or(1).clamp(1, 5),
        xp: c.get_i32("Xp").unwrap_or(0),
        offers,
        restock_at: now + c.get_i32("RestockIn").unwrap_or(0).max(0) as u64,
    })
}

fn cost_nbt(server: &Server, cost: (i32, i32)) -> NbtCompound {
    let mut out = NbtCompound::new();
    out.put("id", crate::items::item_name(server, cost.0).as_str());
    out.put("count", cost.1);
    out
}

fn stack_nbt(server: &Server, stack: &ItemStack) -> NbtCompound {
    let mut out = NbtCompound::new();
    out.put("id", crate::items::item_name(server, stack.item).as_str());
    out.put("count", stack.count);
    if !stack.patch.is_empty() {
        out.put(
            "garnet_patch",
            NbtTag::ByteArray(stack.patch.iter().map(|b| *b as i8).collect()),
        );
    }
    out
}

fn cost_from(server: &Server, node: Option<&NbtCompound>) -> Option<(i32, i32)> {
    let node = node?;
    let id = crate::items::item_id(server, node.get_str("id")?)?;
    Some((id, node.get_i32("count").unwrap_or(1)))
}

fn stack_from(server: &Server, node: Option<&NbtCompound>) -> Option<ItemStack> {
    let node = node?;
    let id = crate::items::item_id(server, node.get_str("id")?)?;
    let patch = match node.get("garnet_patch") {
        Some(NbtTag::ByteArray(bytes)) => bytes.iter().map(|b| *b as u8).collect(),
        _ => Vec::new(),
    };
    Some(ItemStack {
        item: id,
        count: node.get_i32("count").unwrap_or(1),
        patch,
    })
}

/// A player right-clicked a villager: show them the shop.
pub fn open(server: &Arc<Server>, player: &Arc<Player>, entity: &Entity) -> bool {
    let Some(state) = entity.villager.clone() else { return false };
    if state.job.is_empty() || state.offers.is_empty() {
        return false; // nothing to sell yet
    }
    let window_id = {
        let mut s = player.lock();
        s.next_window_id = s.next_window_id % 100 + 1;
        let id = s.next_window_id;
        s.trading = vec![ItemStack::EMPTY; SLOTS];
        s.trading_with = Some(entity.id);
        s.trade_index = 0;
        s.container = Some(crate::containers::OpenContainer {
            pos: BlockPos::new(entity.x.floor() as i32, entity.y.floor() as i32, entity.z.floor() as i32),
            window_id: id,
            size: SLOTS,
            kind: crate::containers::Kind::Merchant,
        });
        id
    };
    let menu_type = server.data.registries.id_of("menu", "minecraft:merchant").unwrap_or(0);
    player.send(&cb::OpenScreen {
        window_id,
        menu_type,
        title: Text::new(job_name(&state.job)),
    });
    send_offers(player, window_id, &state);
    crate::containers::send_window(server, player, &vec![ItemStack::EMPTY; SLOTS]);
    true
}

fn job_name(job: &str) -> String {
    let mut name = job.to_owned();
    if let Some(first) = name.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    name
}

fn send_offers(player: &Arc<Player>, window_id: i32, state: &Villager) {
    let offers = state
        .offers
        .iter()
        .map(|offer| cb::MerchantOffer {
            wants: offer.wants,
            also_wants: offer.also_wants,
            gives: offer.gives.clone(),
            uses: offer.uses,
            max_uses: offer.max_uses,
            xp: offer.xp,
            discount: 0,
            price_multiplier: offer.price_multiplier,
            demand: 0,
        })
        .collect();
    player.send(&cb::MerchantOffers {
        window_id,
        offers,
        level: state.level,
        xp: state.xp,
        show_progress: true,
        can_restock: true,
    });
}

/// The player picked a different trade from the list.
pub fn select(server: &Arc<Server>, player: &Arc<Player>, index: i32) {
    {
        let mut s = player.lock();
        s.trade_index = index.max(0) as usize;
    }
    refresh(server, player);
}

/// Works out what the window should show: the result slot holds what the
/// chosen trade would give, if the player has put down what it wants.
pub fn refresh(server: &Arc<Server>, player: &Arc<Player>) {
    let Some(open) = player.lock().container.clone() else { return };
    if open.kind != crate::containers::Kind::Merchant {
        return;
    }
    let (slots, index, villager) = {
        let s = player.lock();
        (s.trading.clone(), s.trade_index, s.trading_with)
    };
    let Some(state) = villager.and_then(|id| state_of(server, id)) else {
        return;
    };
    let result = match state.offers.get(index) {
        Some(offer) if !offer.sold_out() && paid(&slots, offer) => offer.gives.clone(),
        _ => ItemStack::EMPTY,
    };
    {
        let mut s = player.lock();
        if s.trading.len() > RESULT {
            s.trading[RESULT] = result;
        }
    }
    let slots = player.lock().trading.clone();
    crate::containers::send_window(server, player, &slots);
}

/// Whether what is on the counter covers what the trade asks for.
fn paid(slots: &[ItemStack], offer: &Offer) -> bool {
    let has = |item: i32, count: i32| {
        slots
            .iter()
            .take(RESULT)
            .filter(|stack| stack.item == item && !stack.is_empty())
            .map(|stack| stack.count)
            .sum::<i32>()
            >= count
    };
    has(offer.wants.0, offer.wants.1) && offer.also_wants.is_none_or(|(item, count)| has(item, count))
}

/// The player took what the trade gives: charge them for it.
pub fn take(server: &Arc<Server>, player: &Arc<Player>) -> bool {
    let (slots, index, villager) = {
        let s = player.lock();
        (s.trading.clone(), s.trade_index, s.trading_with)
    };
    let Some(id) = villager else { return false };
    let Some(mut state) = state_of(server, id) else { return false };
    let Some(offer) = state.offers.get(index).cloned() else {
        return false;
    };
    if offer.sold_out() || !paid(&slots, &offer) {
        return false;
    }
    {
        let mut s = player.lock();
        spend(&mut s.trading, offer.wants.0, offer.wants.1);
        if let Some((item, count)) = offer.also_wants {
            spend(&mut s.trading, item, count);
        }
        s.trading[RESULT] = ItemStack::EMPTY;
        // Onto the cursor, where a click leaves what it picked up.
        if s.inventory.cursor.is_empty() {
            s.inventory.cursor = offer.gives.clone();
        } else {
            let name = crate::items::item_name(server, offer.gives.item);
            let max = crate::inventory::max_stack_size(&server.data, &name);
            let over = s.inventory.add(offer.gives.clone(), max);
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
    // The villager learns from it, and may take up a new line of work.
    if let Some(stored) = state.offers.get_mut(index) {
        stored.uses += 1;
    }
    state.xp += offer.xp;
    let earned = LEVEL_XP
        .iter()
        .enumerate()
        .filter(|(_, needed)| state.xp >= **needed)
        .map(|(level, _)| level as i32 + 1)
        .max()
        .unwrap_or(1);
    if earned > state.level {
        state.level = earned.min(5);
        state.offers = roll_offers(server, &state);
    }
    let window = player.lock().container.clone();
    store(server, id, state.clone());
    if let Some(open) = window {
        send_offers(player, open.window_id, &state);
    }
    crate::experience::give(player, offer.xp.max(1));
    refresh(server, player);
    true
}

fn spend(slots: &mut [ItemStack], item: i32, mut count: i32) {
    for slot in slots.iter_mut().take(RESULT) {
        if slot.is_empty() || slot.item != item || count <= 0 {
            continue;
        }
        let taken = slot.count.min(count);
        slot.count -= taken;
        count -= taken;
        if slot.count <= 0 {
            *slot = ItemStack::EMPTY;
        }
    }
}

fn state_of(server: &Arc<Server>, id: i32) -> Option<Villager> {
    let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    entities.by_id.get(&id).and_then(|entity| entity.villager.clone())
}

fn store(server: &Arc<Server>, id: i32, state: Villager) {
    let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(stored) = entities.by_id.get_mut(&id) {
        stored.villager = Some(state);
    }
}
