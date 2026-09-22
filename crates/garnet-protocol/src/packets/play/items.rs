//! Item stacks on the wire, and the inventory packets in both directions.
//!
//! An item stack is a count, an item id and a "component patch": the
//! components added to or removed from the item's defaults. Components the
//! server understands (enchantments, names, damage) it builds itself; any
//! other patch a client sends (creative mode spawn eggs, potions...) is kept
//! as the raw bytes it arrived in and sent back out untouched, so nothing is
//! lost even for components this server has no code for.

use crate::buffer::{PacketReader, PacketWriter};
use crate::packets::{ClientboundPacket, ServerboundPacket, State};
use crate::text::Text;
use crate::Result;

/// Ids from `minecraft:data_component_type` (protocol 777).
pub mod component {
    pub const MAX_STACK_SIZE: i32 = 1;
    pub const MAX_DAMAGE: i32 = 2;
    pub const DAMAGE: i32 = 3;
    pub const UNBREAKABLE: i32 = 4;
    pub const CUSTOM_NAME: i32 = 6;
    pub const LORE: i32 = 11;
    pub const ENCHANTMENTS: i32 = 13;
    pub const REPAIR_COST: i32 = 19;
    /// What is in a bottle: which potion, and anything added to it.
    pub const POTION_CONTENTS: i32 = 53;
    /// What an enchanted book carries, as opposed to what it is enchanted
    /// with: a book's own enchantments do nothing until an anvil moves them
    /// onto something.
    pub const STORED_ENCHANTMENTS: i32 = 45;
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ItemStack {
    /// Index into `minecraft:item`; meaningless when `count` is 0.
    pub item: i32,
    pub count: i32,
    /// Encoded component patch (added count, removed count, entries).
    /// Empty means "no changes".
    pub patch: Vec<u8>,
}

impl ItemStack {
    pub const EMPTY: ItemStack = ItemStack {
        item: 0,
        count: 0,
        patch: Vec::new(),
    };

    pub fn new(item: i32, count: i32) -> Self {
        Self {
            item,
            count,
            patch: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count <= 0
    }

    /// Same item and components: the two can stack together.
    pub fn same_kind(&self, other: &ItemStack) -> bool {
        !self.is_empty() && !other.is_empty() && self.item == other.item && self.patch == other.patch
    }

    pub fn write(&self, w: &mut PacketWriter) {
        if self.is_empty() {
            w.write_varint(0);
            return;
        }
        w.write_varint(self.count);
        w.write_varint(self.item);
        if self.patch.is_empty() {
            w.write_varint(0);
            w.write_varint(0);
        } else {
            w.write_bytes(&self.patch);
        }
    }

    /// Reads a stack whose patch runs to the end of the packet (the last
    /// field of `set_creative_mode_slot`), keeping the patch bytes as-is.
    pub fn read_to_end(r: &mut PacketReader) -> Result<Self> {
        let count = r.read_varint()?;
        if count <= 0 {
            return Ok(Self::EMPTY);
        }
        let item = r.read_varint()?;
        let patch = r.remaining_bytes().to_vec();
        let patch = if patch == [0, 0] { Vec::new() } else { patch };
        Ok(Self { item, count, patch })
    }
}

/// Builds a component patch with the components this server knows.
#[derive(Default)]
pub struct PatchBuilder {
    added: Vec<(i32, Vec<u8>)>,
    removed: Vec<i32>,
}

impl PatchBuilder {
    pub fn enchantments(self, levels: &[(i32, i32)]) -> Self {
        self.enchantment_list(component::ENCHANTMENTS, levels)
    }

    /// The enchantments an enchanted book holds for later.
    pub fn stored_enchantments(self, levels: &[(i32, i32)]) -> Self {
        self.enchantment_list(component::STORED_ENCHANTMENTS, levels)
    }

    fn enchantment_list(mut self, component: i32, levels: &[(i32, i32)]) -> Self {
        let mut w = PacketWriter::new();
        w.write_varint(levels.len() as i32);
        for (enchantment, level) in levels {
            w.write_varint(*enchantment);
            w.write_varint(*level);
        }
        self.added.push((component, w.into_inner()));
        self
    }

    /// What is in a bottle. Vanilla's shape: the potion itself, then a
    /// colour, extra effects and a name, none of which we add.
    pub fn potion(mut self, potion: i32) -> Self {
        let mut w = PacketWriter::new();
        w.write_bool(true);
        w.write_varint(potion);
        w.write_bool(false); // no colour of its own
        w.write_varint(0); // no effects beyond the potion's own
        w.write_bool(false); // no name of its own
        self.added.push((component::POTION_CONTENTS, w.into_inner()));
        self
    }

    pub fn custom_name(mut self, name: &Text) -> Self {
        let mut w = PacketWriter::new();
        w.write_text(name);
        self.added.push((component::CUSTOM_NAME, w.into_inner()));
        self
    }

    pub fn damage(mut self, damage: i32) -> Self {
        let mut w = PacketWriter::new();
        w.write_varint(damage);
        self.added.push((component::DAMAGE, w.into_inner()));
        self
    }

    pub fn unbreakable(mut self) -> Self {
        self.added.push((component::UNBREAKABLE, Vec::new()));
        self
    }

    /// Adds a component already encoded, for carrying one across.
    pub fn raw(mut self, component: i32, data: Vec<u8>) -> Self {
        self.added.push((component, data));
        self
    }

    pub fn remove(mut self, component: i32) -> Self {
        self.removed.push(component);
        self
    }

    pub fn build(self) -> Vec<u8> {
        if self.added.is_empty() && self.removed.is_empty() {
            return Vec::new();
        }
        let mut w = PacketWriter::new();
        w.write_varint(self.added.len() as i32);
        w.write_varint(self.removed.len() as i32);
        for (id, data) in &self.added {
            w.write_varint(*id);
            w.write_bytes(data);
        }
        for id in &self.removed {
            w.write_varint(*id);
        }
        w.into_inner()
    }
}

/// The components of a patch this server wrote, in order. Returns None
/// for a patch holding anything we cannot measure the length of, so a
/// caller can leave such an item alone rather than mangle it.
pub fn components_in(patch: &[u8]) -> Option<(Vec<(i32, Vec<u8>)>, Vec<i32>)> {
    if patch.is_empty() {
        return Some((Vec::new(), Vec::new()));
    }
    let mut r = PacketReader::new(patch);
    let added = r.read_varint().ok()?;
    let removed = r.read_varint().ok()?;
    let mut out = Vec::new();
    for _ in 0..added {
        let id = r.read_varint().ok()?;
        let before = r.remaining();
        match id {
            component::DAMAGE | component::MAX_STACK_SIZE | component::MAX_DAMAGE | component::REPAIR_COST => {
                r.read_varint().ok()?;
            }
            component::UNBREAKABLE => {}
            component::CUSTOM_NAME => {
                r.read_nbt().ok()?;
            }
            component::POTION_CONTENTS => {
                if r.read_bool().ok()? {
                    r.read_varint().ok()?;
                }
                if r.read_bool().ok()? {
                    r.read_i32().ok()?;
                }
                if r.read_varint().ok()? != 0 {
                    return None; // effects of its own: we cannot measure those
                }
                if r.read_bool().ok()? {
                    r.read_string().ok()?;
                }
            }
            component::ENCHANTMENTS | component::STORED_ENCHANTMENTS => {
                let n = r.read_varint().ok()?;
                for _ in 0..n {
                    r.read_varint().ok()?;
                    r.read_varint().ok()?;
                }
            }
            _ => return None, // something we did not write: leave it be
        }
        let after = r.remaining();
        let taken = before - after;
        out.push((id, patch[patch.len() - before..patch.len() - before + taken].to_vec()));
    }
    let mut dropped = Vec::new();
    for _ in 0..removed {
        dropped.push(r.read_varint().ok()?);
    }
    Some((out, dropped))
}

/// A whole-number component of a patch, such as damage or repair cost.
pub fn number_in(patch: &[u8], component: i32) -> Option<i32> {
    let (added, _) = components_in(patch)?;
    let (_, data) = added.iter().find(|(id, _)| *id == component)?;
    PacketReader::new(data).read_varint().ok()
}

/// How damaged an item is, or None when it carries no damage.
pub fn damage_in(patch: &[u8]) -> Option<i32> {
    let (added, _) = components_in(patch)?;
    let (_, data) = added.iter().find(|(id, _)| *id == component::DAMAGE)?;
    PacketReader::new(data).read_varint().ok()
}

/// Rebuilds a patch with one component replaced or added.
pub fn with_component(patch: &[u8], id: i32, data: Vec<u8>) -> Option<Vec<u8>> {
    let (mut added, removed) = components_in(patch)?;
    match added.iter_mut().find(|(existing, _)| *existing == id) {
        Some(slot) => slot.1 = data,
        None => added.push((id, data)),
    }
    let mut w = PacketWriter::new();
    w.write_varint(added.len() as i32);
    w.write_varint(removed.len() as i32);
    for (id, data) in &added {
        w.write_varint(*id);
        w.write_bytes(data);
    }
    for id in &removed {
        w.write_varint(*id);
    }
    Some(w.into_inner())
}

pub fn enchantments_in(patch: &[u8]) -> Option<Vec<(i32, i32)>> {
    enchantment_list_in(patch, component::ENCHANTMENTS)
}

/// Which potion is in a bottle, by its id in `minecraft:potion`.
pub fn potion_in(patch: &[u8]) -> Option<i32> {
    let (added, _) = components_in(patch)?;
    let (_, data) = added.iter().find(|(id, _)| *id == component::POTION_CONTENTS)?;
    let mut r = PacketReader::new(data);
    r.read_bool().ok()?.then(|| r.read_varint().ok()).flatten()
}

/// What an enchanted book is holding, which is not the same as what it is
/// itself enchanted with.
pub fn stored_enchantments_in(patch: &[u8]) -> Option<Vec<(i32, i32)>> {
    enchantment_list_in(patch, component::STORED_ENCHANTMENTS)
}

fn enchantment_list_in(patch: &[u8], component: i32) -> Option<Vec<(i32, i32)>> {
    let (added, _) = components_in(patch)?;
    let (_, data) = added.iter().find(|(id, _)| *id == component)?;
    let mut r = PacketReader::new(data);
    let n = r.read_varint().ok()?;
    let mut out = Vec::new();
    for _ in 0..n {
        out.push((r.read_varint().ok()?, r.read_varint().ok()?));
    }
    Some(out)
}

// ---- clientbound ----

/// The whole content of a container. Window 0 is the player's inventory.
pub struct ContainerSetContent {
    pub container_id: i32,
    pub state_id: i32,
    pub items: Vec<ItemStack>,
    pub carried: ItemStack,
}

impl ClientboundPacket for ContainerSetContent {
    const NAME: &'static str = "container_set_content";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.container_id);
        w.write_varint(self.state_id);
        w.write_list(&self.items, |w, item| item.write(w));
        self.carried.write(w);
    }
}

pub struct ContainerSetSlot {
    pub container_id: i32,
    pub state_id: i32,
    pub slot: i16,
    pub item: ItemStack,
}

impl ClientboundPacket for ContainerSetSlot {
    const NAME: &'static str = "container_set_slot";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.container_id);
        w.write_varint(self.state_id);
        w.write_i16(self.slot);
        self.item.write(w);
    }
}

/// One player inventory slot, outside any open container.
pub struct SetPlayerInventory {
    pub slot: i32,
    pub item: ItemStack,
}

impl ClientboundPacket for SetPlayerInventory {
    const NAME: &'static str = "set_player_inventory";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.slot);
        self.item.write(w);
    }
}

pub struct SetCursorItem {
    pub item: ItemStack,
}

impl ClientboundPacket for SetCursorItem {
    const NAME: &'static str = "set_cursor_item";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        self.item.write(w);
    }
}

// ---- serverbound ----

/// Creative mode: the client puts an item straight into a slot (-1 means
/// it was thrown out of the inventory).
pub struct SetCreativeModeSlot {
    pub slot: i16,
    pub item: ItemStack,
}

impl ServerboundPacket for SetCreativeModeSlot {
    const NAME: &'static str = "set_creative_mode_slot";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let slot = r.read_i16()?;
        let item = ItemStack::read_to_end(r)?;
        Ok(Self { slot, item })
    }
}

/// What kind of click happened in a container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickKind {
    Pickup,
    QuickMove,
    Swap,
    Clone,
    Throw,
    QuickCraft,
    PickupAll,
}

/// A click in a container. The client also sends its idea of the slots it
/// changed (as hashes); the server simulates the click itself and resends
/// the truth, so those hashes are skipped.
pub struct ContainerClick {
    pub container_id: i32,
    pub state_id: i32,
    pub slot: i16,
    pub button: i8,
    pub kind: ClickKind,
}

impl ServerboundPacket for ContainerClick {
    const NAME: &'static str = "container_click";
    const STATE: State = State::Play;
    fn read(r: &mut PacketReader) -> Result<Self> {
        let container_id = r.read_varint()?;
        let state_id = r.read_varint()?;
        let slot = r.read_i16()?;
        let button = r.read_i8()?;
        let kind = match r.read_varint()? {
            0 => ClickKind::Pickup,
            1 => ClickKind::QuickMove,
            2 => ClickKind::Swap,
            3 => ClickKind::Clone,
            4 => ClickKind::Throw,
            5 => ClickKind::QuickCraft,
            _ => ClickKind::PickupAll,
        };
        Ok(Self {
            container_id,
            state_id,
            slot,
            button,
            kind,
        })
    }
}
