//! Where the water is.
//!
//! The density says which places are rock and which are not; the aquifer
//! says what fills the rest. Most of it is air, but underground the world
//! is dotted with pockets of water and lava at their own levels, and where
//! two pockets meet the rock between them is thickened so they do not run
//! into each other. The pockets sit on a loose grid, each one jittered
//! within its cell, and every block asks the handful of pockets nearest it
//! what it should be.

use super::density::Node;
use super::rng::{PositionalSource, RandomSource};
use std::collections::HashMap;
use std::sync::Arc;

/// How far apart the pockets sit, and how far each may wander in its cell.
const SPACING_XZ: i32 = 16;
const SPACING_Y: i32 = 12;
const RANGE_XZ: i32 = 10;
const RANGE_Y: i32 = 9;
/// Two pockets this close in distance are treated as one.
const SIMILARITY_SCALE: f64 = 25.0;
/// Below this the world has no fluid at all.
const NO_FLUID: i32 = i32::MIN / 2;
/// The columns around a pocket that are looked at to see how deep it sits.
const SURFACE_SAMPLES: [(i32, i32); 13] = [
    (0, 0),
    (-2, -1),
    (-1, -1),
    (0, -1),
    (1, -1),
    (-3, 0),
    (-2, 0),
    (-1, 0),
    (1, 0),
    (-2, 1),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// What one pocket holds, and up to where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fluid {
    pub level: i32,
    /// True for lava, false for water.
    pub lava: bool,
}

impl Fluid {
    /// What this pocket puts at a height: its fluid, or nothing.
    fn at(&self, y: i32) -> Option<bool> {
        (y < self.level).then_some(self.lava)
    }
}

/// What a block turns out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Substance {
    /// Whatever the world is made of: stone, or deepslate lower down.
    Rock,
    Air,
    Water,
    Lava,
}

/// The world's own fluid, before any pocket has its say: sea water down to
/// the sea floor, and lava in the deep.
#[derive(Clone, Copy, Debug)]
pub struct GlobalFluid {
    pub sea_level: i32,
    pub lava_level: i32,
}

impl GlobalFluid {
    fn at(&self, y: i32) -> Fluid {
        if y < self.lava_level {
            Fluid {
                level: self.lava_level,
                lava: true,
            }
        } else {
            Fluid {
                level: self.sea_level,
                lava: false,
            }
        }
    }
}

/// The noises and functions an aquifer reads, out of the noise settings.
pub struct Aquifer {
    barrier: Arc<Node>,
    floodedness: Arc<Node>,
    spread: Arc<Node>,
    lava: Arc<Node>,
    exclusion: Arc<Node>,
    surface_level: Arc<Node>,
    random: PositionalSource,
    global: GlobalFluid,
}

impl Aquifer {
    pub fn new(
        barrier: Arc<Node>,
        floodedness: Arc<Node>,
        spread: Arc<Node>,
        lava: Arc<Node>,
        exclusion: Arc<Node>,
        surface_level: Arc<Node>,
        random: PositionalSource,
        global: GlobalFluid,
    ) -> Self {
        Self {
            barrier,
            floodedness,
            spread,
            lava,
            exclusion,
            surface_level,
            random,
            global,
        }
    }

    /// A working set of caches for one chunk's worth of asking.
    pub fn session(&self) -> Session<'_> {
        Session {
            aquifer: self,
            pockets: HashMap::new(),
            surfaces: HashMap::new(),
        }
    }
}

/// The caches that make a chunk's worth of questions affordable: where each
/// nearby pocket sits, what it holds, and how high the ground is.
pub struct Session<'a> {
    aquifer: &'a Aquifer,
    pockets: HashMap<(i32, i32, i32), (i32, i32, i32, Fluid)>,
    surfaces: HashMap<(i32, i32), i32>,
}

impl Session<'_> {
    /// What a block is, given how solid the density says it is.
    pub fn substance(&mut self, x: i32, y: i32, z: i32, density: f32) -> Substance {
        if density > 0.0 {
            return Substance::Rock;
        }
        let global = self.aquifer.global.at(y);
        if global.lava && global.at(y).is_some() {
            return Substance::Lava;
        }

        // The pockets whose cells this block sits among.
        let anchor = (grid_xz(x - 5), grid_y(y + 1), grid_xz(z - 5));
        let mut nearest: [(i32, (i32, i32, i32)); 4] = [(i32::MAX, (0, 0, 0)); 4];
        for dx in 0..=1 {
            for dy in -1..=1 {
                for dz in 0..=1 {
                    let cell = (anchor.0 + dx, anchor.1 + dy, anchor.2 + dz);
                    let (px, py, pz, _) = self.pocket(cell);
                    let distance = {
                        let (ox, oy, oz) = (px - x, py - y, pz - z);
                        ox * ox + oy * oy + oz * oz
                    };
                    // Keep the four closest, in order.
                    for slot in 0..4 {
                        if distance <= nearest[slot].0 {
                            nearest[slot..].rotate_right(1);
                            nearest[slot] = (distance, cell);
                            break;
                        }
                    }
                }
            }
        }

        let first = self.pocket(nearest[0].1).3;
        let fluid_here = match first.at(y) {
            Some(true) => Substance::Lava,
            Some(false) => Substance::Water,
            None => Substance::Air,
        };
        let similarity_12 = similarity(nearest[0].0, nearest[1].0);
        if similarity_12 <= 0.0 {
            return fluid_here;
        }

        // Water sitting straight on lava is left alone rather than walled
        // off, so the two meet and the game sorts it out.
        if fluid_here == Substance::Water && self.aquifer.global.at(y - 1).at(y - 1) == Some(true) {
            return fluid_here;
        }

        let second = self.pocket(nearest[1].1).3;
        let mut barrier_noise = f64::NAN;
        let pressure = self.pressure(x, y, z, &mut barrier_noise, first, second);
        if density as f64 + similarity_12 * pressure > 0.0 {
            return Substance::Rock;
        }
        let third = self.pocket(nearest[2].1).3;
        let similarity_13 = similarity(nearest[0].0, nearest[2].0);
        if similarity_13 > 0.0 {
            let pressure = self.pressure(x, y, z, &mut barrier_noise, first, third);
            if density as f64 + similarity_12 * similarity_13 * pressure > 0.0 {
                return Substance::Rock;
            }
        }
        let similarity_23 = similarity(nearest[1].0, nearest[2].0);
        if similarity_23 > 0.0 {
            let pressure = self.pressure(x, y, z, &mut barrier_noise, second, third);
            if density as f64 + similarity_12 * similarity_23 * pressure > 0.0 {
                return Substance::Rock;
            }
        }
        fluid_here
    }

    /// How much rock stands between two pockets at this height: nothing
    /// where they agree, and a wall where one is much higher than the
    /// other or they hold different things.
    fn pressure(&mut self, x: i32, y: i32, z: i32, barrier_noise: &mut f64, one: Fluid, other: Fluid) -> f64 {
        let (first, second) = (one.at(y), other.at(y));
        // Lava against water is always walled off.
        if matches!((first, second), (Some(true), Some(false)) | (Some(false), Some(true))) {
            return 2.0;
        }
        let difference = (one.level - other.level).abs();
        if difference == 0 {
            return 0.0;
        }
        let average = 0.5 * (one.level + other.level) as f64;
        let above_average = y as f64 + 0.5 - average;
        let half = difference as f64 / 2.0;
        let towards_middle = half - above_average.abs();
        // Above the middle the wall thins quickly, below it slowly.
        let gradient = if above_average > 0.0 {
            let centre = towards_middle;
            if centre > 0.0 {
                centre / 1.5
            } else {
                centre / 2.5
            }
        } else {
            let centre = 3.0 + towards_middle;
            if centre > 0.0 {
                centre / 3.0
            } else {
                centre / 10.0
            }
        };
        // The noise is only worth reading where the wall is in the
        // balance; well inside or well outside it changes nothing.
        let noise = if (-2.0..=2.0).contains(&gradient) {
            if barrier_noise.is_nan() {
                *barrier_noise = self.aquifer.barrier.sample(x, y, z) as f64;
            }
            *barrier_noise
        } else {
            0.0
        };
        2.0 * (noise + gradient)
    }

    /// Where the pocket for a cell sits, and what it holds.
    fn pocket(&mut self, cell: (i32, i32, i32)) -> (i32, i32, i32, Fluid) {
        if let Some(found) = self.pockets.get(&cell) {
            return *found;
        }
        let mut random = self.aquifer.random.at(cell.0, cell.1, cell.2);
        let x = cell.0 * SPACING_XZ + random.next_int_bound(RANGE_XZ);
        let y = cell.1 * SPACING_Y + random.next_int_bound(RANGE_Y);
        let z = cell.2 * SPACING_XZ + random.next_int_bound(RANGE_XZ);
        let fluid = self.fluid_at(x, y, z);
        let found = (x, y, z, fluid);
        self.pockets.insert(cell, found);
        found
    }

    /// What a pocket at this spot holds, and up to where.
    fn fluid_at(&mut self, x: i32, y: i32, z: i32) -> Fluid {
        let global = self.aquifer.global.at(y);
        let top = y + SPACING_Y;
        let bottom = y - SPACING_Y;
        let mut lowest_surface = i32::MAX;
        let mut under_the_sea = false;

        for (ox, oz) in SURFACE_SAMPLES {
            let (sx, sz) = (x + ox * 16, z + oz * 16);
            let surface = self.surface(sx, sz);
            let adjusted = surface + 8;
            let centre = ox == 0 && oz == 0;
            // A pocket well under the ground is just ground water.
            if centre && bottom > adjusted {
                return global;
            }
            let pokes_out = top > adjusted;
            if pokes_out || centre {
                let at_surface = self.aquifer.global.at(adjusted);
                if at_surface.at(adjusted).is_some() {
                    if centre {
                        under_the_sea = true;
                    }
                    if pokes_out {
                        return at_surface;
                    }
                }
            }
            lowest_surface = lowest_surface.min(surface);
        }

        let level = self.fluid_level(x, y, z, global, lowest_surface, under_the_sea);
        Fluid {
            level,
            lava: self.is_lava(x, y, z, global, level),
        }
    }

    /// How high the fluid in a pocket stands: to the sea in a flooded
    /// place, at its own level in a half-flooded one, and nowhere at all
    /// in a dry one.
    fn fluid_level(&mut self, x: i32, y: i32, z: i32, global: Fluid, lowest_surface: i32, under_the_sea: bool) -> i32 {
        let (partly, fully) = if self.aquifer.exclusion.sample(x, y, z) > 0.0 {
            // Somewhere the world has said outright it wants no water.
            (-1.0, -1.0)
        } else {
            let below_surface = (lowest_surface + 8 - y) as f64;
            // Right under the sea floor a pocket is as flooded as the sea
            // above it; sixty-four blocks down it is on its own.
            let factor = if under_the_sea {
                (1.0 - below_surface / 64.0).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let noise = (self.aquifer.floodedness.sample(x, y, z) as f64).clamp(-1.0, 1.0);
            let fully_threshold = map(factor, 1.0, 0.0, -0.3, 0.8);
            let partly_threshold = map(factor, 1.0, 0.0, -0.8, 0.4);
            (noise - partly_threshold, noise - fully_threshold)
        };
        if fully > 0.0 {
            global.level
        } else if partly > 0.0 {
            self.spread_level(x, y, z, lowest_surface)
        } else {
            NO_FLUID
        }
    }

    /// A pocket's own level, which wanders a little from the middle of the
    /// forty-block band it sits in.
    fn spread_level(&mut self, x: i32, y: i32, z: i32, lowest_surface: i32) -> i32 {
        let cell = (x.div_euclid(16), y.div_euclid(40), z.div_euclid(16));
        let middle = cell.1 * 40 + 20;
        let spread = self.aquifer.spread.sample(cell.0, cell.1, cell.2) as f64 * 10.0;
        // Down to the nearest three blocks, as the game rounds it.
        let quantised = (spread / 3.0).floor() as i32 * 3;
        lowest_surface.min(middle + quantised)
    }

    /// Whether a pocket holds lava rather than water: only the deep ones,
    /// and only where the lava noise says so.
    fn is_lava(&mut self, x: i32, y: i32, z: i32, global: Fluid, level: i32) -> bool {
        if global.lava {
            return true;
        }
        if level > -10 || level == NO_FLUID {
            return false;
        }
        let cell = (x.div_euclid(64), y.div_euclid(40), z.div_euclid(64));
        self.aquifer.lava.sample(cell.0, cell.1, cell.2).abs() > 0.3
    }

    /// How high the ground stands over a column, to the nearest four
    /// blocks, which is as finely as the game asks.
    fn surface(&mut self, x: i32, z: i32) -> i32 {
        let key = ((x >> 2) << 2, (z >> 2) << 2);
        if let Some(found) = self.surfaces.get(&key) {
            return *found;
        }
        let level = self.aquifer.surface_level.sample(key.0, 0, key.1).floor() as i32;
        self.surfaces.insert(key, level);
        level
    }
}

/// How alike two distances are: one when they are equal, falling away to
/// nothing as one pocket gets further than the other.
fn similarity(closest: i32, other: i32) -> f64 {
    1.0 - (other - closest) as f64 / SIMILARITY_SCALE
}

/// Maps a value from one range onto another, as the game's helper does.
fn map(value: f64, from_low: f64, from_high: f64, to_low: f64, to_high: f64) -> f64 {
    to_low + (value - from_low) * (to_high - to_low) / (from_high - from_low)
}

fn grid_xz(block: i32) -> i32 {
    block >> 4
}

fn grid_y(block: i32) -> i32 {
    block.div_euclid(SPACING_Y)
}
