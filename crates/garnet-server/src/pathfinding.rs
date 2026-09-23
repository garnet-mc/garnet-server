//! Finding a way there.
//!
//! Mobs used to walk straight at whoever they could see and hop whatever
//! got in the way, which works in a field and not much else. This is an A*
//! over standing positions: a mob may step to a neighbouring block, climb
//! one, or drop a few, and it will not walk into lava or off a cliff it
//! cannot survive. Every search is capped, and so is the work all the mobs
//! in the world may do between them in one tick, so a crowd of them cannot
//! stall the server.

use crate::server::Server;
use garnet_protocol::BlockPos;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

/// How far a mob will look for a way round.
const MAX_RANGE: i32 = 32;
/// How many positions one search may consider.
const SEARCH_BUDGET: usize = 400;
/// What every mob in the world may spend between them in a tick.
pub const TICK_BUDGET: usize = 6000;
/// The furthest a mob will drop on purpose.
const MAX_DROP: i32 = 3;
/// How long a path is trusted before it is worked out again.
pub const REPATH_TICKS: u64 = 20;

/// A way from where a mob is to where it wants to be.
#[derive(Clone, Debug)]
pub struct Path {
    pub steps: Vec<BlockPos>,
    /// Which step the mob is walking towards.
    pub at: usize,
    /// Where this path was going, so we notice when the target moves.
    pub goal: BlockPos,
    pub found_tick: u64,
}

impl Path {
    /// Where the mob should head for next, if anywhere.
    pub fn next_step(&self) -> Option<BlockPos> {
        self.steps.get(self.at).copied()
    }

    /// Whether the mob has arrived at the step it was walking towards, and
    /// should move on to the next one.
    pub fn advance(&mut self, x: f64, y: f64, z: f64) {
        while let Some(step) = self.next_step() {
            let flat = ((x - (step.x as f64 + 0.5)).powi(2) + (z - (step.z as f64 + 0.5)).powi(2)).sqrt();
            // Close enough horizontally, and on about the right level.
            if flat < 0.6 && (y - step.y as f64).abs() < 1.2 {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    pub fn done(&self) -> bool {
        self.at >= self.steps.len()
    }
}

/// One position in the search, ordered by how promising it is.
struct Step {
    cost: f64,
    guess: f64,
    pos: BlockPos,
}

impl PartialEq for Step {
    fn eq(&self, other: &Self) -> bool {
        self.guess == other.guess
    }
}
impl Eq for Step {}
impl Ord for Step {
    fn cmp(&self, other: &Self) -> Ordering {
        // A min-heap out of Rust's max-heap.
        other.guess.partial_cmp(&self.guess).unwrap_or(Ordering::Equal)
    }
}
impl PartialOrd for Step {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Works out a way from `from` to somewhere next to `to`, spending at most
/// what is left of `budget`.
pub fn find(server: &Arc<Server>, from: BlockPos, to: BlockPos, budget: &mut usize) -> Option<Vec<BlockPos>> {
    if *budget == 0 {
        return None;
    }
    if (from.x - to.x).abs() > MAX_RANGE || (from.z - to.z).abs() > MAX_RANGE || (from.y - to.y).abs() > MAX_RANGE {
        return None;
    }
    let mut world = server.world();
    let blocks = &server.data.blocks;
    let mut standing = |pos: BlockPos| -> Ground {
        let below = world.get_block(pos.offset(0, -1, 0)).unwrap_or(0) as i32;
        let feet = world.get_block(pos).unwrap_or(0) as i32;
        let head = world.get_block(pos.offset(0, 1, 0)).unwrap_or(0) as i32;
        ground(blocks, below, feet, head)
    };

    let mut came_from: HashMap<i64, BlockPos> = HashMap::new();
    let mut best: HashMap<i64, f64> = HashMap::new();
    let mut open = BinaryHeap::new();
    open.push(Step {
        cost: 0.0,
        guess: distance(from, to),
        pos: from,
    });
    best.insert(key(from), 0.0);
    let mut spent = 0usize;
    let mut closest = (from, distance(from, to));

    while let Some(Step { cost, pos, .. }) = open.pop() {
        if spent >= SEARCH_BUDGET.min(*budget) {
            break;
        }
        spent += 1;
        if pos == to || (pos.x == to.x && pos.z == to.z && (pos.y - to.y).abs() <= 1) {
            *budget = budget.saturating_sub(spent);
            return Some(retrace(&came_from, pos));
        }
        let left = distance(pos, to);
        if left < closest.1 {
            closest = (pos, left);
        }
        if cost > best.get(&key(pos)).copied().unwrap_or(f64::MAX) {
            continue; // a better way here has already been found
        }
        for (next, step_cost) in neighbours(pos, &mut standing) {
            let so_far = cost + step_cost;
            if so_far >= best.get(&key(next)).copied().unwrap_or(f64::MAX) {
                continue;
            }
            best.insert(key(next), so_far);
            came_from.insert(key(next), pos);
            open.push(Step {
                cost: so_far,
                guess: so_far + distance(next, to),
                pos: next,
            });
        }
    }
    *budget = budget.saturating_sub(spent);
    // Nowhere near, but going the right way is better than standing still.
    if closest.0 != from && closest.1 + 1.0 < distance(from, to) {
        return Some(retrace(&came_from, closest.0));
    }
    None
}

/// Whether stepping here would hurt: lava, fire and the rest. Used by a
/// mob that has no path and is shuffling towards its goal in a straight
/// line, so that it stops at the edge rather than walking in.
pub fn dangerous(server: &Arc<Server>, pos: BlockPos) -> bool {
    let mut world = server.world();
    let blocks = &server.data.blocks;
    [pos, pos.offset(0, -1, 0)].iter().any(|at| {
        let state = world.get_block(*at).unwrap_or(0) as i32;
        blocks
            .block_of_state(state)
            .map(|block| harmful(block.name.strip_prefix("minecraft:").unwrap_or(&block.name)))
            .unwrap_or(false)
    })
}

/// What a mob would find at a position: whether it can stand there and
/// what it would cost.
#[derive(Clone, Copy, PartialEq)]
enum Ground {
    /// Solid underfoot, room for a mob.
    Firm,
    /// Standing in water: passable, but slower.
    Wet,
    /// Open air: only useful as somewhere to fall through.
    Empty,
    /// Lava, fire and the rest: never worth it.
    Harmful,
    /// A wall.
    Blocked,
}

fn ground(blocks: &garnet_data::BlockRegistry, below: i32, feet: i32, head: i32) -> Ground {
    let name = |state: i32| {
        blocks
            .block_of_state(state)
            .map(|b| b.name.strip_prefix("minecraft:").unwrap_or(&b.name).to_owned())
            .unwrap_or_default()
    };
    let passable = |state: i32| blocks.is_air(state) || soft(&name(state));
    if harmful(&name(feet)) || harmful(&name(head)) || harmful(&name(below)) {
        return Ground::Harmful;
    }
    if !passable(feet) || !passable(head) {
        return Ground::Blocked;
    }
    if blocks.is_liquid(feet) {
        return Ground::Wet;
    }
    if blocks.is_air(below) || blocks.is_liquid(below) || soft(&name(below)) {
        return Ground::Empty;
    }
    Ground::Firm
}

/// Blocks a mob walks through without minding.
fn soft(short: &str) -> bool {
    matches!(
        short,
        "torch"
            | "wall_torch"
            | "redstone_wire"
            | "tripwire"
            | "string"
            | "snow"
            | "dead_bush"
            | "vine"
            | "ladder"
            | "sugar_cane"
    ) || short.ends_with("_grass")
        || short.ends_with("_fern")
        || short.ends_with("_flower")
        || short.ends_with("_sapling")
        || short.ends_with("_button")
        || short.ends_with("_pressure_plate")
}

/// Blocks that hurt, which a path goes round.
fn harmful(short: &str) -> bool {
    matches!(
        short,
        "lava"
            | "fire"
            | "soul_fire"
            | "cactus"
            | "magma_block"
            | "sweet_berry_bush"
            | "campfire"
            | "soul_campfire"
            | "wither_rose"
    )
}

/// Where a mob could go from here, and what each way costs.
fn neighbours(pos: BlockPos, standing: &mut impl FnMut(BlockPos) -> Ground) -> Vec<(BlockPos, f64)> {
    const AROUND: [(i32, i32); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];
    let mut out = Vec::new();
    for (dx, dz) in AROUND {
        let diagonal = dx != 0 && dz != 0;
        // A mob will not cut a corner through two walls.
        if diagonal {
            let side_x = standing(pos.offset(dx, 0, 0));
            let side_z = standing(pos.offset(0, 0, dz));
            if matches!(side_x, Ground::Blocked | Ground::Harmful) || matches!(side_z, Ground::Blocked | Ground::Harmful) {
                continue;
            }
        }
        let flat = if diagonal { 1.4 } else { 1.0 };
        let level = pos.offset(dx, 0, dz);
        match standing(level) {
            Ground::Firm => out.push((level, flat)),
            Ground::Wet => out.push((level, flat + 1.0)),
            Ground::Empty => {
                // Nothing underfoot: fall, if it is not too far.
                let mut drop = 1;
                while drop <= MAX_DROP {
                    let under = level.offset(0, -drop, 0);
                    match standing(under) {
                        Ground::Firm => {
                            out.push((under, flat + drop as f64 * 0.5));
                            break;
                        }
                        Ground::Wet => {
                            out.push((under, flat + 1.0));
                            break;
                        }
                        Ground::Empty => drop += 1,
                        _ => break,
                    }
                }
            }
            Ground::Blocked => {
                // Something in the way: climb it, if it is only a step.
                let up = level.offset(0, 1, 0);
                if standing(up) == Ground::Firm && standing(pos.offset(0, 1, 0)) != Ground::Blocked {
                    out.push((up, flat + 0.5));
                }
            }
            Ground::Harmful => {}
        }
    }
    out
}

fn retrace(came_from: &HashMap<i64, BlockPos>, mut pos: BlockPos) -> Vec<BlockPos> {
    let mut steps = vec![pos];
    while let Some(previous) = came_from.get(&key(pos)) {
        pos = *previous;
        steps.push(pos);
    }
    steps.reverse();
    // The first step is where the mob already is.
    if steps.len() > 1 {
        steps.remove(0);
    }
    steps
}

fn distance(a: BlockPos, b: BlockPos) -> f64 {
    let (dx, dy, dz) = ((a.x - b.x) as f64, (a.y - b.y) as f64, (a.z - b.z) as f64);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn key(pos: BlockPos) -> i64 {
    (pos.x as i64 & 0x3FF_FFFF) << 38 | (pos.z as i64 & 0x3FF_FFFF) << 12 | (pos.y as i64 & 0xFFF)
}
