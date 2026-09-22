//! Water and lava, flowing the way vanilla's fluids do.
//!
//! A fluid block is a source (level 0) or a flow (1-7, thinner as it goes),
//! and 8 and up mean it is falling from above. Every update recomputes what
//! the block should hold from its neighbours, pours downwards first, and
//! otherwise spreads sideways towards whatever hole is nearest, which is
//! what makes water run along a channel instead of creeping evenly outwards.
//! Water with two sources beside it becomes a source of its own; lava that
//! meets water turns to stone.

use crate::server::Server;
use garnet_protocol::BlockPos;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Ticks between one spread and the next.
const WATER_DELAY: u64 = 5;
const LAVA_DELAY: u64 = 30;
/// How much thinner a flow gets with each block it travels.
const WATER_DROP: i32 = 1;
const LAVA_DROP: i32 = 2;
/// How far to look for a hole before spreading evenly.
const SLOPE_SEARCH: i32 = 4;

const SIDES: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fluid {
    Water,
    Lava,
}

impl Fluid {
    fn block(self) -> &'static str {
        match self {
            Fluid::Water => "minecraft:water",
            Fluid::Lava => "minecraft:lava",
        }
    }

    fn delay(self) -> u64 {
        match self {
            Fluid::Water => WATER_DELAY,
            Fluid::Lava => LAVA_DELAY,
        }
    }

    fn drop_off(self) -> i32 {
        match self {
            Fluid::Water => WATER_DROP,
            Fluid::Lava => LAVA_DROP,
        }
    }
}

/// The fluid at a spot and how full it is, if any.
fn at(server: &Arc<Server>, pos: BlockPos) -> Option<(Fluid, i32)> {
    let state = server.world().get_block(pos).ok()?;
    read(server, state)
}

fn read(server: &Arc<Server>, state: u32) -> Option<(Fluid, i32)> {
    let block = server.data.blocks.block_of_state(state as i32)?;
    let fluid = match block.name.as_str() {
        "minecraft:water" => Fluid::Water,
        "minecraft:lava" => Fluid::Lava,
        _ => return None,
    };
    let level = server
        .data
        .blocks
        .state(state as i32)
        .and_then(|s| s.properties.get("level").cloned())
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    Some((fluid, level))
}

/// True when a fluid may take this block over.
fn replaceable(server: &Arc<Server>, pos: BlockPos) -> bool {
    let Ok(state) = server.world().get_block(pos) else { return false };
    let blocks = &server.data.blocks;
    if blocks.is_air(state as i32) {
        return true;
    }
    if read(server, state).is_some() {
        return false; // another fluid: handled separately
    }
    // Plants and other things a flow washes away.
    let Some(block) = blocks.block_of_state(state as i32) else { return false };
    let short = block.name.strip_prefix("minecraft:").unwrap_or(&block.name);
    matches!(short, "snow" | "vine" | "dead_bush" | "torch" | "wall_torch" | "ladder")
        || short.ends_with("_grass")
        || short.ends_with("_fern")
        || short.ends_with("_flower")
        || short.ends_with("_sapling")
        || short.ends_with("_carpet")
}

/// Whether a fluid can rest on what is underneath.
fn holds_up(server: &Arc<Server>, pos: BlockPos) -> bool {
    let Ok(state) = server.world().get_block(pos) else { return true };
    let blocks = &server.data.blocks;
    !blocks.is_air(state as i32) && read(server, state).is_none() && !replaceable(server, pos)
}

fn set_level(server: &Arc<Server>, pos: BlockPos, fluid: Fluid, level: i32) {
    let mut props = BTreeMap::new();
    props.insert("level".to_owned(), level.to_string());
    if let Some(state) = server.data.blocks.state_with(fluid.block(), &props) {
        server.set_block(pos, state as u32);
        server.schedule_block(pos, fluid.delay());
    }
}

fn clear(server: &Arc<Server>, pos: BlockPos) {
    let air = server.data.blocks.default_state("air").unwrap_or(0) as u32;
    server.set_block(pos, air);
}

/// One update of the fluid at this spot.
pub fn tick(server: &Arc<Server>, pos: BlockPos) {
    let Some((fluid, level)) = at(server, pos) else { return };
    let source = level == 0;
    let falling = level >= 8;

    if !source {
        // Work out what the neighbours can still supply, and dry up if
        // nothing can.
        match supply(server, pos, fluid) {
            None => {
                clear(server, pos);
                wake_neighbours(server, pos);
                return;
            }
            Some(wanted) if wanted != level => {
                set_level(server, pos, fluid, wanted);
                wake_neighbours(server, pos);
                return;
            }
            _ => {}
        }
    }

    // Two sources beside still water make another source, which is what
    // lets a two-by-two hole refill itself.
    if fluid == Fluid::Water && !source {
        let sources = SIDES
            .iter()
            .filter(|(dx, dz)| at(server, pos.offset(*dx, 0, *dz)) == Some((Fluid::Water, 0)))
            .count();
        if sources >= 2 && holds_up(server, pos.offset(0, -1, 0)) {
            set_level(server, pos, Fluid::Water, 0);
            return;
        }
    }

    // Downhill first.
    let below = pos.offset(0, -1, 0);
    if let Some(other) = at(server, below).map(|(f, _)| f) {
        if other != fluid {
            meet(server, below, fluid, other);
            return;
        }
    } else if replaceable(server, below) {
        pour(server, below, fluid, 8);
        return;
    }
    // A falling column only spreads where it lands: on something solid,
    // or on a full block of its own fluid.
    let landed = holds_up(server, below) || at(server, below).is_some_and(|(f, l)| f == fluid && l == 0);
    if !landed {
        return;
    }
    if falling {
        spread(server, pos, fluid, fluid.drop_off());
        return;
    }
    let next = level + fluid.drop_off();
    if next <= 7 {
        spread(server, pos, fluid, next);
    }
}

/// The level this block should hold, given what is around it: `None` when
/// nothing feeds it any more.
fn supply(server: &Arc<Server>, pos: BlockPos, fluid: Fluid) -> Option<i32> {
    if at(server, pos.offset(0, 1, 0)).map(|(f, _)| f) == Some(fluid) {
        return Some(8); // fed from above: falling
    }
    let mut best = None;
    for (dx, dz) in SIDES {
        let Some((other, level)) = at(server, pos.offset(dx, 0, dz)) else { continue };
        if other != fluid {
            continue;
        }
        // A block falling past counts as a full one: that is what keeps
        // the puddle at the foot of a waterfall alive.
        let effective = if level >= 8 { 0 } else { level };
        let candidate = effective + fluid.drop_off();
        if candidate <= 7 && best.map(|b| candidate < b).unwrap_or(true) {
            best = Some(candidate);
        }
    }
    best
}

/// Puts fluid into a block, turning it to stone where the two fluids meet.
fn pour(server: &Arc<Server>, pos: BlockPos, fluid: Fluid, level: i32) {
    if let Some((other, _)) = at(server, pos) {
        if other != fluid {
            meet(server, pos, fluid, other);
            return;
        }
    }
    // Lava that touches water sets, and water sets the lava it touches.
    for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, 1, 0), (0, -1, 0)] {
        let side = pos.offset(dx, dy, dz);
        let Some((other, other_level)) = at(server, side) else { continue };
        if other == fluid {
            continue;
        }
        if fluid == Fluid::Lava {
            solidify(server, pos, level == 0);
            return;
        }
        // Water reaching lava turns that lava to stone instead.
        solidify(server, side, other_level == 0);
        return;
    }
    set_level(server, pos, fluid, level);
}

/// The two fluids met: what is left behind.
fn meet(server: &Arc<Server>, pos: BlockPos, arriving: Fluid, standing: Fluid) {
    let lava_here = standing == Fluid::Lava;
    let source = at(server, pos).map(|(_, level)| level == 0).unwrap_or(false);
    let _ = arriving;
    solidify(server, pos, lava_here && source);
}

/// Obsidian where a lava source set, cobblestone where a flow did.
fn solidify(server: &Arc<Server>, pos: BlockPos, was_source: bool) {
    let name = if was_source { "obsidian" } else { "cobblestone" };
    if let Some(state) = server.data.blocks.default_state(name) {
        server.set_block(pos, state as u32);
    }
}

/// Spreads sideways, preferring whichever way leads to a hole.
fn spread(server: &Arc<Server>, pos: BlockPos, fluid: Fluid, level: i32) {
    let mut best = i32::MAX;
    let mut directions: Vec<(i32, i32)> = Vec::new();
    for (dx, dz) in SIDES {
        let side = pos.offset(dx, 0, dz);
        if !can_flow_into(server, side, fluid, level) {
            continue;
        }
        let distance = slope_distance(server, side, fluid, 1);
        if distance < best {
            best = distance;
            directions.clear();
        }
        if distance == best {
            directions.push((dx, dz));
        }
    }
    for (dx, dz) in directions {
        pour(server, pos.offset(dx, 0, dz), fluid, level);
    }
}

fn can_flow_into(server: &Arc<Server>, pos: BlockPos, fluid: Fluid, level: i32) -> bool {
    match at(server, pos) {
        Some((other, _)) if other != fluid => true, // meets the other fluid
        Some((_, existing)) => existing > level && existing < 8,
        None => replaceable(server, pos),
    }
}

/// How far to the nearest hole this way, for choosing a direction.
fn slope_distance(server: &Arc<Server>, pos: BlockPos, fluid: Fluid, depth: i32) -> i32 {
    if !holds_up(server, pos.offset(0, -1, 0)) {
        return depth; // it can fall from here
    }
    if depth >= SLOPE_SEARCH {
        return SLOPE_SEARCH + 1;
    }
    let mut best = SLOPE_SEARCH + 1;
    for (dx, dz) in SIDES {
        let side = pos.offset(dx, 0, dz);
        if !replaceable(server, side) && at(server, side).map(|(f, _)| f) != Some(fluid) {
            continue;
        }
        best = best.min(slope_distance(server, side, fluid, depth + 1));
    }
    best
}

/// Asks the fluids around a spot to look at themselves again.
pub fn wake_neighbours(server: &Arc<Server>, pos: BlockPos) {
    for (dx, dy, dz) in [(0, 1, 0), (0, -1, 0), (1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1)] {
        let side = pos.offset(dx, dy, dz);
        if let Some((fluid, _)) = at(server, side) {
            server.schedule_block(side, fluid.delay());
        }
    }
}
