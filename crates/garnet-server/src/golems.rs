//! Golems built out of blocks.
//!
//! A carved pumpkin put on the right pile of iron or snow takes the pile
//! away and leaves something standing there instead. Vanilla's shapes: an
//! iron golem is a body of two iron blocks with arms either side, a snow
//! golem is two snow blocks in a stack. The pumpkin has to go on last,
//! which is exactly when this is asked.

use crate::server::Server;
use crate::world_entities;
use garnet_protocol::BlockPos;
use std::sync::Arc;

/// What was placed, and what it might have finished.
pub fn maybe_build(server: &Arc<Server>, pos: BlockPos, placed: &str) {
    if !matches!(placed, "minecraft:carved_pumpkin" | "minecraft:jack_o_lantern") {
        return;
    }
    if iron_golem(server, pos) {
        return;
    }
    snow_golem(server, pos);
}

/// The iron shape: two blocks down from the head, with arms out either
/// side of the shoulders.
fn iron_golem(server: &Arc<Server>, head: BlockPos) -> bool {
    let shoulders = head.offset(0, -1, 0);
    let waist = head.offset(0, -2, 0);
    if !is(server, shoulders, "minecraft:iron_block") || !is(server, waist, "minecraft:iron_block") {
        return false;
    }
    // The arms go either east and west, or north and south.
    let arms = [[(1, 0), (-1, 0)], [(0, 1), (0, -1)]].into_iter().find(|pair| {
        pair.iter()
            .all(|(dx, dz)| is(server, shoulders.offset(*dx, 0, *dz), "minecraft:iron_block"))
    });
    let Some(arms) = arms else { return false };

    let mut taken = vec![head, shoulders, waist];
    taken.extend(arms.iter().map(|(dx, dz)| shoulders.offset(*dx, 0, *dz)));
    build(server, &taken, "minecraft:iron_golem", waist, 100.0)
}

/// The snow shape: a stack of two, with the pumpkin on top.
fn snow_golem(server: &Arc<Server>, head: BlockPos) -> bool {
    let middle = head.offset(0, -1, 0);
    let bottom = head.offset(0, -2, 0);
    if !is(server, middle, "minecraft:snow_block") || !is(server, bottom, "minecraft:snow_block") {
        return false;
    }
    build(server, &[head, middle, bottom], "minecraft:snow_golem", bottom, 4.0)
}

/// Takes the blocks away and stands the golem where they were.
fn build(server: &Arc<Server>, blocks: &[BlockPos], kind: &str, feet: BlockPos, health: f32) -> bool {
    let air = server.data.blocks.default_state("air").unwrap_or(0) as u32;
    for pos in blocks {
        server.set_block(*pos, air);
        crate::blocks::changed(server, *pos);
    }
    let Some(mut golem) = world_entities::new_entity(server, kind, feet.x as f64 + 0.5, feet.y as f64, feet.z as f64 + 0.5)
    else {
        return false;
    };
    golem.health = health;
    // Something somebody built stays where it was put.
    golem.persistent = true;
    world_entities::spawn(server, golem);
    tracing::debug!("a {kind} was built at {:?}", feet);
    true
}

fn is(server: &Arc<Server>, pos: BlockPos, name: &str) -> bool {
    let Ok(state) = server.world().get_block(pos) else {
        return false;
    };
    server
        .data
        .blocks
        .block_of_state(state as i32)
        .is_some_and(|block| block.name == name)
}
