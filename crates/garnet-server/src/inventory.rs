//! A player's inventory and what happens when they click in it.
//!
//! Slot numbers follow the vanilla player container: 0 crafting output,
//! 1-4 crafting grid, 5-8 armour (head, chest, legs, feet), 9-35 main,
//! 36-44 hotbar, 45 off hand. The server simulates every click itself and
//! sends the whole inventory back with a new state id, so the client and
//! server can never drift apart for long.

use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::ItemStack;
use garnet_protocol::packets::play::serverbound::{ClickKind, ContainerClick};

pub const SLOTS: usize = 46;
pub const CRAFT_RESULT: usize = 0;
pub const ARMOR_START: usize = 5;
pub const MAIN_START: usize = 9;
pub const HOTBAR_START: usize = 36;
pub const OFFHAND: usize = 45;

#[derive(Clone, Debug)]
pub struct Inventory {
    pub slots: Vec<ItemStack>,
    pub cursor: ItemStack,
    /// Bumped whenever the server changes something; the client echoes it.
    pub state_id: i32,
    drag: Vec<usize>,
    drag_button: i8,
}

impl Default for Inventory {
    fn default() -> Self {
        Self::new()
    }
}

impl Inventory {
    pub fn new() -> Self {
        Self {
            slots: vec![ItemStack::EMPTY; SLOTS],
            cursor: ItemStack::EMPTY,
            state_id: 0,
            drag: Vec::new(),
            drag_button: 0,
        }
    }

    pub fn held(&self, hotbar_index: i32) -> &ItemStack {
        &self.slots[HOTBAR_START + hotbar_index.clamp(0, 8) as usize]
    }

    pub fn held_mut(&mut self, hotbar_index: i32) -> &mut ItemStack {
        &mut self.slots[HOTBAR_START + hotbar_index.clamp(0, 8) as usize]
    }

    /// Adds items the way a pickup does: top up matching stacks in the
    /// hotbar and main inventory, then fill empty slots. Returns what did
    /// not fit.
    /// Wraps a plain list of slots, for containers that are not a player.
    pub fn from_slots(slots: Vec<ItemStack>) -> Self {
        Self {
            slots,
            cursor: ItemStack::EMPTY,
            state_id: 0,
            drag: Vec::new(),
            drag_button: 0,
        }
    }

    pub fn into_slots(self) -> Vec<ItemStack> {
        self.slots
    }

    pub fn add(&mut self, mut stack: ItemStack, max_stack: i32) -> ItemStack {
        if stack.is_empty() {
            return ItemStack::EMPTY;
        }
        let order: Vec<usize> = (HOTBAR_START..HOTBAR_START + 9).chain(MAIN_START..HOTBAR_START).collect();
        for &i in &order {
            let slot = &mut self.slots[i];
            if slot.same_kind(&stack) && slot.count < max_stack {
                let moved = (max_stack - slot.count).min(stack.count);
                slot.count += moved;
                stack.count -= moved;
                if stack.count == 0 {
                    return ItemStack::EMPTY;
                }
            }
        }
        for &i in &order {
            if self.slots[i].is_empty() {
                let moved = stack.count.min(max_stack);
                self.slots[i] = ItemStack {
                    item: stack.item,
                    count: moved,
                    patch: stack.patch.clone(),
                };
                stack.count -= moved;
                if stack.count == 0 {
                    return ItemStack::EMPTY;
                }
            }
        }
        stack
    }

    /// Removes up to `max` items (all when `max` is negative) matching the
    /// filter (every item when `None`); returns how many went.
    /// Takes everything out, for a player who just died.
    pub fn take_all(&mut self) -> Vec<ItemStack> {
        let mut out = Vec::new();
        for slot in self.slots.iter_mut() {
            if !slot.is_empty() {
                out.push(std::mem::replace(slot, ItemStack::EMPTY));
            }
        }
        if !self.cursor.is_empty() {
            out.push(std::mem::replace(&mut self.cursor, ItemStack::EMPTY));
        }
        self.state_id += 1;
        out
    }

    pub fn clear(&mut self, item: Option<i32>, max: i32) -> i32 {
        let mut removed = 0;
        for slot in self.slots.iter_mut().chain(std::iter::once(&mut self.cursor)) {
            if slot.is_empty() || item.is_some_and(|i| i != slot.item) {
                continue;
            }
            let take = if max < 0 { slot.count } else { (max - removed).min(slot.count) };
            if take <= 0 {
                break;
            }
            slot.count -= take;
            removed += take;
            if slot.count == 0 {
                *slot = ItemStack::EMPTY;
            }
        }
        removed
    }

    pub fn count(&self, item: Option<i32>) -> i32 {
        self.slots
            .iter()
            .filter(|s| !s.is_empty() && item.is_none_or(|i| i == s.item))
            .map(|s| s.count)
            .sum()
    }

    pub fn set(&mut self, slot: usize, stack: ItemStack) {
        if slot < SLOTS {
            self.slots[slot] = stack;
        }
    }

    /// Everything, for `container_set_content` on window 0.
    pub fn content_packet(&mut self) -> cb::ContainerSetContent {
        self.state_id = self.state_id.wrapping_add(1);
        cb::ContainerSetContent {
            container_id: 0,
            state_id: self.state_id,
            items: self.slots.clone(),
            carried: self.cursor.clone(),
        }
    }

    /// Applies a click from the client. `max_stack` gives the stack limit
    /// of an item id.
    pub fn click(&mut self, click: &ContainerClick, max_stack: &dyn Fn(i32) -> i32, creative: bool) {
        let slot = click.slot;
        let index = usize::try_from(slot).ok().filter(|&i| i < SLOTS);
        match click.kind {
            ClickKind::Pickup => {
                let Some(i) = index else { return }; // outside: a throw, which needs item entities
                if i == CRAFT_RESULT {
                    return;
                }
                let limit = max_stack(if self.cursor.is_empty() { self.slots[i].item } else { self.cursor.item });
                if click.button == 0 {
                    self.left_click(i, limit);
                } else {
                    self.right_click(i, limit);
                }
            }
            ClickKind::QuickMove => {
                let Some(i) = index else { return };
                self.shift_click(i, max_stack);
            }
            ClickKind::Swap => {
                let Some(i) = index else { return };
                let target = if click.button == 40 { OFFHAND } else { HOTBAR_START + (click.button.clamp(0, 8) as usize) };
                if target != i && i != CRAFT_RESULT {
                    self.slots.swap(i, target);
                }
            }
            ClickKind::Clone => {
                let Some(i) = index else { return };
                if creative && self.cursor.is_empty() && !self.slots[i].is_empty() {
                    let mut copy = self.slots[i].clone();
                    copy.count = max_stack(copy.item);
                    self.cursor = copy;
                }
            }
            ClickKind::Throw => {
                // Dropping items on the ground needs item entities; until
                // then the item simply stays where it was.
            }
            ClickKind::QuickCraft => self.drag(click.button, index, max_stack),
            ClickKind::PickupAll => {
                if self.cursor.is_empty() {
                    return;
                }
                let limit = max_stack(self.cursor.item);
                for i in (MAIN_START..SLOTS).chain(ARMOR_START..MAIN_START) {
                    if self.cursor.count >= limit {
                        break;
                    }
                    if self.slots[i].same_kind(&self.cursor) {
                        let moved = (limit - self.cursor.count).min(self.slots[i].count);
                        self.cursor.count += moved;
                        self.slots[i].count -= moved;
                        if self.slots[i].count == 0 {
                            self.slots[i] = ItemStack::EMPTY;
                        }
                    }
                }
            }
        }
    }

    fn left_click(&mut self, i: usize, limit: i32) {
        let slot = self.slots[i].clone();
        if self.cursor.is_empty() {
            self.cursor = slot;
            self.slots[i] = ItemStack::EMPTY;
        } else if slot.is_empty() {
            let moved = self.cursor.count.min(limit);
            self.slots[i] = ItemStack {
                item: self.cursor.item,
                count: moved,
                patch: self.cursor.patch.clone(),
            };
            self.cursor.count -= moved;
            if self.cursor.count == 0 {
                self.cursor = ItemStack::EMPTY;
            }
        } else if slot.same_kind(&self.cursor) {
            let moved = (limit - slot.count).clamp(0, self.cursor.count);
            self.slots[i].count += moved;
            self.cursor.count -= moved;
            if self.cursor.count == 0 {
                self.cursor = ItemStack::EMPTY;
            }
        } else {
            self.slots[i] = std::mem::replace(&mut self.cursor, slot);
        }
    }

    fn right_click(&mut self, i: usize, limit: i32) {
        let slot = self.slots[i].clone();
        if self.cursor.is_empty() {
            if slot.is_empty() {
                return;
            }
            let half = (slot.count + 1) / 2;
            self.cursor = ItemStack {
                item: slot.item,
                count: half,
                patch: slot.patch.clone(),
            };
            self.slots[i].count -= half;
            if self.slots[i].count == 0 {
                self.slots[i] = ItemStack::EMPTY;
            }
        } else if slot.is_empty() {
            self.slots[i] = ItemStack {
                item: self.cursor.item,
                count: 1,
                patch: self.cursor.patch.clone(),
            };
            self.cursor.count -= 1;
            if self.cursor.count == 0 {
                self.cursor = ItemStack::EMPTY;
            }
        } else if slot.same_kind(&self.cursor) && slot.count < limit {
            self.slots[i].count += 1;
            self.cursor.count -= 1;
            if self.cursor.count == 0 {
                self.cursor = ItemStack::EMPTY;
            }
        } else {
            self.slots[i] = std::mem::replace(&mut self.cursor, slot);
        }
    }

    /// Shift-click: hotbar and main inventory trade places; armour, the
    /// off hand and the crafting grid empty into them.
    fn shift_click(&mut self, i: usize, max_stack: &dyn Fn(i32) -> i32) {
        let stack = std::mem::replace(&mut self.slots[i], ItemStack::EMPTY);
        if stack.is_empty() {
            return;
        }
        let limit = max_stack(stack.item);
        let targets: Vec<usize> = if (HOTBAR_START..HOTBAR_START + 9).contains(&i) {
            (MAIN_START..HOTBAR_START).collect()
        } else if (MAIN_START..HOTBAR_START).contains(&i) {
            (HOTBAR_START..HOTBAR_START + 9).collect()
        } else {
            (MAIN_START..HOTBAR_START + 9).collect()
        };
        let mut rest = stack;
        for &t in &targets {
            if self.slots[t].same_kind(&rest) && self.slots[t].count < limit {
                let moved = (limit - self.slots[t].count).min(rest.count);
                self.slots[t].count += moved;
                rest.count -= moved;
                if rest.count == 0 {
                    return;
                }
            }
        }
        for &t in &targets {
            if self.slots[t].is_empty() {
                self.slots[t] = rest;
                return;
            }
        }
        self.slots[i] = rest;
    }

    /// Dragging: start (button 0/4/8), add a slot (1/5/9), finish (2/6/10).
    fn drag(&mut self, button: i8, index: Option<usize>, max_stack: &dyn Fn(i32) -> i32) {
        match button {
            0 | 4 | 8 => {
                self.drag.clear();
                self.drag_button = button;
            }
            1 | 5 | 9 => {
                if let Some(i) = index {
                    if i != CRAFT_RESULT && (self.slots[i].is_empty() || self.slots[i].same_kind(&self.cursor)) && !self.drag.contains(&i) {
                        self.drag.push(i);
                    }
                }
            }
            _ => {
                let slots = std::mem::take(&mut self.drag);
                if self.cursor.is_empty() || slots.is_empty() {
                    return;
                }
                let limit = max_stack(self.cursor.item);
                let per_slot = match self.drag_button {
                    4 => 1,
                    8 => limit,
                    _ => (self.cursor.count / slots.len() as i32).max(1),
                };
                for i in slots {
                    if self.cursor.count == 0 {
                        break;
                    }
                    let room = limit - self.slots[i].count;
                    let moved = per_slot.min(room).min(self.cursor.count);
                    if moved <= 0 {
                        continue;
                    }
                    if self.slots[i].is_empty() {
                        self.slots[i] = ItemStack {
                            item: self.cursor.item,
                            count: 0,
                            patch: self.cursor.patch.clone(),
                        };
                    }
                    self.slots[i].count += moved;
                    if self.drag_button != 8 {
                        self.cursor.count -= moved;
                    }
                }
                if self.cursor.count == 0 {
                    self.cursor = ItemStack::EMPTY;
                }
            }
        }
    }
}

/// Stack limits by item name; vanilla's defaults are 64, 16 and 1.
pub fn max_stack_size(name: &str) -> i32 {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    const SINGLE_SUFFIXES: &[&str] = &[
        "_sword", "_pickaxe", "_axe", "_shovel", "_hoe", "_helmet", "_chestplate", "_leggings", "_boots", "_horse_armor", "_boat",
        "_raft", "_minecart", "_bucket", "_music_disc", "_spawn_egg",
    ];
    const SINGLE: &[&str] = &[
        "bow", "crossbow", "trident", "shield", "elytra", "fishing_rod", "flint_and_steel", "shears", "bucket", "saddle", "potion",
        "splash_potion", "lingering_potion", "totem_of_undying", "enchanted_book", "written_book", "writable_book", "cake", "spyglass",
        "brush", "mace", "carrot_on_a_stick", "warped_fungus_on_a_stick", "goat_horn", "bundle", "minecart", "turtle_helmet", "wolf_armor",
    ];
    const SIXTEEN: &[&str] = &["ender_pearl", "snowball", "egg", "honey_bottle", "armor_stand", "bucket_of_cod", "written_book"];
    if SINGLE.contains(&short) || SINGLE_SUFFIXES.iter().any(|s| short.ends_with(s)) || short.starts_with("music_disc") || short.ends_with("_spawn_egg") {
        return 1;
    }
    if SIXTEEN.contains(&short) || short.ends_with("_sign") || short.ends_with("_banner") || short.ends_with("_bed") || short.ends_with("_bundle") {
        return 16;
    }
    64
}

/// Slot names as `/item` and `/replaceitem` spell them.
pub fn slot_by_name(name: &str, held: i32) -> Option<usize> {
    Some(match name {
        "weapon" | "weapon.mainhand" => HOTBAR_START + held.clamp(0, 8) as usize,
        "weapon.offhand" => OFFHAND,
        "armor.head" => ARMOR_START,
        "armor.chest" => ARMOR_START + 1,
        "armor.legs" => ARMOR_START + 2,
        "armor.feet" => ARMOR_START + 3,
        _ => {
            let (kind, n) = name.split_once('.')?;
            let n: usize = n.parse().ok()?;
            match kind {
                "hotbar" if n < 9 => HOTBAR_START + n,
                "inventory" if n < 27 => MAIN_START + n,
                "container" if n < SLOTS => n,
                _ => return None,
            }
        }
    })
}
