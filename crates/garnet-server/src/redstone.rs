//! Redstone: what makes a signal, what carries it, and what listens.
//!
//! Wire carries a strength that drops by one each block it travels, so
//! fifteen blocks is as far as a signal goes. Levers, buttons, torches and
//! blocks of redstone make a signal; repeaters pass one on after a delay
//! and only in the way they face. A solid block with a torch underneath, or
//! a repeater pointing into it, is powered itself and passes that on to
//! whatever sits beside it, which is what most contraptions are built out
//! of. Doors, trapdoors, gates and lamps listen.

use crate::server::Server;
use garnet_protocol::BlockPos;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The strongest a signal gets, and how far it travels.
const MAX_POWER: i32 = 15;
/// A repeater's delay is this many ticks per notch.
const REPEATER_STEP: u64 = 2;

const SIDES: [(i32, i32, i32); 4] = [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1)];
const ALL: [(i32, i32, i32); 6] = [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, 1, 0), (0, -1, 0)];

struct Block {
    name: String,
    props: BTreeMap<String, String>,
}

fn block_at(server: &Arc<Server>, pos: BlockPos) -> Option<Block> {
    let state = server.world().get_block(pos).ok()?;
    let blocks = &server.data.blocks;
    let name = blocks.block_of_state(state as i32)?.name.clone();
    let props = blocks.state(state as i32).map(|s| s.properties.clone()).unwrap_or_default();
    Some(Block { name, props })
}

fn short_name(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// True for anything this module has an opinion about.
pub fn is_redstone(name: &str) -> bool {
    let short = short_name(name);
    matches!(
        short,
        "redstone_wire" | "redstone_torch" | "redstone_wall_torch" | "redstone_block" | "repeater" | "redstone_lamp" | "lever"
    ) || short.ends_with("_button")
        || short.ends_with("_door")
        || short.ends_with("_trapdoor")
        || short.ends_with("_fence_gate")
        || short.ends_with("_pressure_plate")
}

/// Something changed here: look again at this block and everything that
/// could care about it.
pub fn update(server: &Arc<Server>, pos: BlockPos) {
    refresh(server, pos);
    for (dx, dy, dz) in ALL {
        refresh(server, pos.offset(dx, dy, dz));
    }
    // Wire reaches a little further than its own neighbours: a block being
    // powered changes what sits beside it too.
    for (dx, dy, dz) in ALL {
        let side = pos.offset(dx, dy, dz);
        for (ex, ey, ez) in ALL {
            refresh(server, side.offset(ex, ey, ez));
        }
    }
}

/// Recomputes one block: wire strength, a lamp's glow, a door's latch.
fn refresh(server: &Arc<Server>, pos: BlockPos) {
    let Some(block) = block_at(server, pos) else { return };
    let short = short_name(&block.name).to_owned();
    match short.as_str() {
        "redstone_wire" => refresh_wire(server, pos, &block),
        "redstone_torch" | "redstone_wall_torch" => refresh_torch(server, pos, &block),
        "repeater" => refresh_repeater(server, pos, &block),
        "redstone_lamp" => {
            let lit = incoming(server, pos) > 0;
            set_prop(server, pos, &block, "lit", lit);
        }
        _ if short.ends_with("_door") || short.ends_with("_trapdoor") || short.ends_with("_fence_gate") => {
            refresh_openable(server, pos, &block);
        }
        _ => {}
    }
}

/// Wire takes the strongest thing reaching it, minus one for the trip.
fn refresh_wire(server: &Arc<Server>, pos: BlockPos, block: &Block) {
    let now: i32 = block.props.get("power").and_then(|p| p.parse().ok()).unwrap_or(0);
    let mut best = 0;
    // A source beside it, or under it, powers the wire at full strength.
    for (dx, dy, dz) in ALL {
        let side = pos.offset(dx, dy, dz);
        best = best.max(source_power(server, side, pos));
    }
    // Wire passes power on, one weaker each block, including up and down a
    // single step the way vanilla lets it climb.
    for (dx, _, dz) in SIDES {
        for dy in [0, 1, -1] {
            let side = pos.offset(dx, dy, dz);
            if let Some(other) = block_at(server, side) {
                if short_name(&other.name) == "redstone_wire" {
                    let power: i32 = other.props.get("power").and_then(|p| p.parse().ok()).unwrap_or(0);
                    best = best.max(power - 1);
                }
            }
        }
    }
    let wanted = best.clamp(0, MAX_POWER);
    let shape = connections(server, pos);
    let shape_changed = shape.iter().any(|(side, how)| block.props.get(*side).map(String::as_str) != Some(how.as_str()));
    if wanted != now || shape_changed {
        let mut props = block.props.clone();
        props.insert("power".to_owned(), wanted.to_string());
        for (side, how) in shape {
            props.insert(side.to_owned(), how);
        }
        write(server, pos, &block.name, props);
        // Its neighbours have to think again.
        for (dx, dy, dz) in ALL {
            server.schedule_block(pos.offset(dx, dy, dz), 1);
        }
    }
}

/// Which way a length of wire runs, so it is drawn as a line rather than
/// a dot: beside it, or up the side of the block next to it.
fn connections(server: &Arc<Server>, pos: BlockPos) -> Vec<(&'static str, String)> {
    let above_is_clear = !solid(server, pos.offset(0, 1, 0));
    [("north", (0, 0, -1)), ("south", (0, 0, 1)), ("east", (1, 0, 0)), ("west", (-1, 0, 0))]
        .into_iter()
        .map(|(side, (dx, dy, dz))| {
            let neighbour = pos.offset(dx, dy, dz);
            let how = if connects(server, neighbour, pos) {
                "side"
            } else if !solid(server, neighbour) && is_wire(server, neighbour.offset(0, -1, 0)) {
                "side"
            } else if above_is_clear && solid(server, neighbour) && is_wire(server, neighbour.offset(0, 1, 0)) {
                "up"
            } else {
                "none"
            };
            (side, how.to_owned())
        })
        .collect()
}

fn is_wire(server: &Arc<Server>, pos: BlockPos) -> bool {
    block_at(server, pos).is_some_and(|b| short_name(&b.name) == "redstone_wire")
}

fn solid(server: &Arc<Server>, pos: BlockPos) -> bool {
    let Ok(state) = server.world().get_block(pos) else { return false };
    let blocks = &server.data.blocks;
    !blocks.is_air(state as i32) && !blocks.is_liquid(state as i32) && server.data.light.opacity(state) > 0
}

/// Whether wire hooks onto whatever is here.
fn connects(server: &Arc<Server>, pos: BlockPos, from: BlockPos) -> bool {
    let Some(block) = block_at(server, pos) else { return false };
    let short = short_name(&block.name);
    if matches!(short, "redstone_wire" | "redstone_block" | "lever" | "redstone_torch" | "redstone_wall_torch") {
        return true;
    }
    if short.ends_with("_button") || short.ends_with("_pressure_plate") {
        return true;
    }
    if short == "repeater" {
        // A repeater only talks to the wire in front of and behind it.
        let facing = block.props.get("facing").cloned().unwrap_or_default();
        return ahead(pos, &facing) == from || behind(pos, &facing) == from;
    }
    false
}

/// A torch is lit unless the block holding it is powered: that inversion is
/// what most logic is built from.
fn refresh_torch(server: &Arc<Server>, pos: BlockPos, block: &Block) {
    let holder = match short_name(&block.name) {
        "redstone_wall_torch" => match block.props.get("facing").map(String::as_str) {
            Some("north") => pos.offset(0, 0, 1),
            Some("south") => pos.offset(0, 0, -1),
            Some("east") => pos.offset(-1, 0, 0),
            Some("west") => pos.offset(1, 0, 0),
            _ => pos.offset(0, -1, 0),
        },
        _ => pos.offset(0, -1, 0),
    };
    let lit_now = block.props.get("lit").map(String::as_str) != Some("false");
    let should = !block_is_powered(server, holder);
    if should != lit_now {
        set_prop(server, pos, block, "lit", should);
        for (dx, dy, dz) in ALL {
            server.schedule_block(pos.offset(dx, dy, dz), 1);
        }
    }
}

/// A repeater takes what is behind it and, after its delay, gives it out
/// in front.
fn refresh_repeater(server: &Arc<Server>, pos: BlockPos, block: &Block) {
    let facing = block.props.get("facing").cloned().unwrap_or_else(|| "north".to_owned());
    let back = behind(pos, &facing);
    let input = source_power(server, back, pos) > 0 || wire_power(server, back) > 0;
    let powered_now = block.props.get("powered").map(String::as_str) == Some("true");
    if input == powered_now {
        return;
    }
    let delay: u64 = block.props.get("delay").and_then(|d| d.parse().ok()).unwrap_or(1);
    // Vanilla holds the change for the repeater's delay before passing it on.
    let scheduled = server.current_tick();
    let _ = scheduled;
    set_prop(server, pos, block, "powered", input);
    for (dx, dy, dz) in ALL {
        server.schedule_block(pos.offset(dx, dy, dz), delay * REPEATER_STEP);
    }
}

/// Doors and the rest follow whatever the signal says, and stay where a
/// hand left them otherwise.
fn refresh_openable(server: &Arc<Server>, pos: BlockPos, block: &Block) {
    let powered_now = block.props.get("powered").map(String::as_str) == Some("true");
    let powered = incoming(server, pos) > 0
        || block
            .props
            .get("half")
            .map(String::as_str)
            .map(|half| {
                // Both halves of a door share what either half can see.
                let other = if half == "lower" { pos.offset(0, 1, 0) } else { pos.offset(0, -1, 0) };
                incoming(server, other) > 0
            })
            .unwrap_or(false);
    if powered == powered_now {
        return;
    }
    let mut props = block.props.clone();
    props.insert("powered".to_owned(), powered.to_string());
    if props.contains_key("open") {
        props.insert("open".to_owned(), powered.to_string());
    }
    write(server, pos, &block.name, props);
    // The other half of a door moves with this one.
    if short_name(&block.name).ends_with("_door") {
        let other = match block.props.get("half").map(String::as_str) {
            Some("lower") => pos.offset(0, 1, 0),
            _ => pos.offset(0, -1, 0),
        };
        if let Some(twin) = block_at(server, other) {
            if twin.name == block.name {
                let mut twin_props = twin.props.clone();
                twin_props.insert("powered".to_owned(), powered.to_string());
                twin_props.insert("open".to_owned(), powered.to_string());
                write(server, other, &twin.name, twin_props);
            }
        }
    }
}

/// The strongest signal reaching this spot from anywhere around it.
fn incoming(server: &Arc<Server>, pos: BlockPos) -> i32 {
    let mut best = 0;
    for (dx, dy, dz) in ALL {
        let side = pos.offset(dx, dy, dz);
        best = best.max(source_power(server, side, pos));
        best = best.max(wire_power(server, side));
        // A solid block with a torch under it passes the signal on.
        if block_is_powered(server, side) {
            best = best.max(MAX_POWER);
        }
    }
    best
}

/// What a block gives out towards `towards`, ignoring wire.
fn source_power(server: &Arc<Server>, pos: BlockPos, towards: BlockPos) -> i32 {
    let Some(block) = block_at(server, pos) else { return 0 };
    let short = short_name(&block.name);
    let on = |name: &str| block.props.get(name).map(String::as_str) == Some("true");
    match short {
        "redstone_block" => MAX_POWER,
        "lever" => {
            if on("powered") {
                MAX_POWER
            } else {
                0
            }
        }
        "redstone_torch" | "redstone_wall_torch" => {
            if block.props.get("lit").map(String::as_str) != Some("false") {
                MAX_POWER
            } else {
                0
            }
        }
        "repeater" => {
            let facing = block.props.get("facing").cloned().unwrap_or_default();
            if on("powered") && ahead(pos, &facing) == towards {
                MAX_POWER
            } else {
                0
            }
        }
        _ if short.ends_with("_button") || short.ends_with("_pressure_plate") => {
            if on("powered") {
                MAX_POWER
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// The strength of wire at a spot.
fn wire_power(server: &Arc<Server>, pos: BlockPos) -> i32 {
    let Some(block) = block_at(server, pos) else { return 0 };
    if short_name(&block.name) != "redstone_wire" {
        return 0;
    }
    block.props.get("power").and_then(|p| p.parse().ok()).unwrap_or(0)
}

/// Whether a solid block is carrying a signal of its own: a torch beneath
/// it, a lever on it, or a repeater pointing into it.
fn block_is_powered(server: &Arc<Server>, pos: BlockPos) -> bool {
    let Some(block) = block_at(server, pos) else { return false };
    let blocks = &server.data.blocks;
    let Ok(state) = server.world().get_block(pos) else { return false };
    if blocks.is_air(state as i32) || is_redstone(&block.name) {
        return false;
    }
    for (dx, dy, dz) in ALL {
        let side = pos.offset(dx, dy, dz);
        let Some(neighbour) = block_at(server, side) else { continue };
        let short = short_name(&neighbour.name);
        // Wire powers the block it runs into, and the one beneath it.
        if short == "redstone_wire" {
            let power: i32 = neighbour.props.get("power").and_then(|p| p.parse().ok()).unwrap_or(0);
            if power > 0 && wire_points_at(server, side, pos, dy) {
                return true;
            }
        }
        let on = |name: &str| neighbour.props.get(name).map(String::as_str) == Some("true");
        let lit = neighbour.props.get("lit").map(String::as_str) != Some("false");
        let powers_this_block = match short {
            // A torch powers the block above it, not the one it stands on.
            "redstone_torch" => lit && dy == -1,
            "redstone_wall_torch" => lit && dy == 0,
            "lever" | _ if short.ends_with("_button") => on("powered") && attached_to(&neighbour, side, pos),
            "repeater" => {
                let facing = neighbour.props.get("facing").cloned().unwrap_or_default();
                on("powered") && ahead(side, &facing) == pos
            }
            _ => false,
        };
        if powers_this_block {
            return true;
        }
    }
    false
}

/// Whether a lever or button on `pos` is fixed to `target`.
fn attached_to(block: &Block, pos: BlockPos, target: BlockPos) -> bool {
    let face = block.props.get("face").map(String::as_str).unwrap_or("wall");
    let facing = block.props.get("facing").map(String::as_str).unwrap_or("north");
    let holder = match face {
        "floor" => pos.offset(0, -1, 0),
        "ceiling" => pos.offset(0, 1, 0),
        _ => behind(pos, facing),
    };
    holder == target
}

/// Whether a length of wire feeds the block beside or below it. Wire
/// powers what it runs into and what it lies on, but not what it merely
/// passes by.
fn wire_points_at(server: &Arc<Server>, wire: BlockPos, block: BlockPos, dy: i32) -> bool {
    if dy == 1 {
        return true; // the wire is lying on this block
    }
    if dy != 0 {
        return false;
    }
    let shape = connections(server, wire);
    let connected = |name: &str| shape.iter().any(|(side, how)| *side == name && how != "none");
    let (toward_block, opposite) = if block.x > wire.x {
        ("east", "west")
    } else if block.x < wire.x {
        ("west", "east")
    } else if block.z > wire.z {
        ("south", "north")
    } else {
        ("north", "south")
    };
    // A lone dot of wire powers all four sides; a line powers both its ends.
    let alone = !shape.iter().any(|(_, how)| how != "none");
    alone || connected(toward_block) || connected(opposite)
}

fn ahead(pos: BlockPos, facing: &str) -> BlockPos {
    match facing {
        "north" => pos.offset(0, 0, -1),
        "south" => pos.offset(0, 0, 1),
        "west" => pos.offset(-1, 0, 0),
        _ => pos.offset(1, 0, 0),
    }
}

fn behind(pos: BlockPos, facing: &str) -> BlockPos {
    match facing {
        "north" => pos.offset(0, 0, 1),
        "south" => pos.offset(0, 0, -1),
        "west" => pos.offset(1, 0, 0),
        _ => pos.offset(-1, 0, 0),
    }
}

fn set_prop(server: &Arc<Server>, pos: BlockPos, block: &Block, name: &str, value: bool) {
    let mut props = block.props.clone();
    if !props.contains_key(name) {
        return;
    }
    props.insert(name.to_owned(), value.to_string());
    write(server, pos, &block.name, props);
}

fn write(server: &Arc<Server>, pos: BlockPos, name: &str, props: BTreeMap<String, String>) {
    if let Some(state) = server.data.blocks.state_with(name, &props) {
        server.set_block(pos, state as u32);
    }
}
