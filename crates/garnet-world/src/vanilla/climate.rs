//! Which biome belongs where.
//!
//! Vanilla does not paint biomes on a map. It reads six numbers at a place
//! -- how warm it is, how green, how far from the sea, how worn, how deep,
//! and how strange -- and looks for the biome whose corner of that
//! six-dimensional space the place falls nearest to. The numbers come out
//! of the density functions; the table of corners comes out of the game,
//! which keeps it in code rather than in the data pack, so it is dumped
//! once and read here.

use super::density::{Graph, Node};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

/// The table, taken from the game itself. See docs/vanilla-worldgen.md for
/// how to dump it again when the version changes.
const OVERWORLD_BIOMES: &str = include_str!("../../data/overworld_biomes.json");

/// The climate at a place, in the whole numbers the game compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub temperature: i64,
    pub humidity: i64,
    pub continentalness: i64,
    pub erosion: i64,
    pub depth: i64,
    pub weirdness: i64,
}

/// Reads the climate out of a world's density functions.
pub struct Sampler {
    temperature: Arc<Node>,
    vegetation: Arc<Node>,
    continents: Arc<Node>,
    erosion: Arc<Node>,
    depth: Arc<Node>,
    ridges: Arc<Node>,
}

impl Sampler {
    /// Builds the sampler for one world seed.
    pub fn new(datapack: &Path, seed: i64) -> Self {
        let graph = Graph::load(datapack, seed);
        let function = |name: &str| graph.function(name).unwrap_or_else(|| Arc::new(Node::Constant(0.0)));
        Self {
            temperature: function("minecraft:overworld/temperature"),
            vegetation: function("minecraft:overworld/vegetation"),
            continents: function("minecraft:overworld/continents"),
            erosion: function("minecraft:overworld/erosion"),
            depth: function("minecraft:overworld/depth"),
            ridges: function("minecraft:overworld/ridges"),
        }
    }

    /// The climate in one biome cell. Cells are four blocks across, which
    /// is why these are not block coordinates.
    pub fn sample(&self, cell_x: i32, cell_y: i32, cell_z: i32) -> Target {
        let (x, y, z) = (cell_x << 2, cell_y << 2, cell_z << 2);
        Target {
            temperature: quantise(self.temperature.sample(x, y, z)),
            humidity: quantise(self.vegetation.sample(x, y, z)),
            continentalness: quantise(self.continents.sample(x, y, z)),
            erosion: quantise(self.erosion.sample(x, y, z)),
            depth: quantise(self.depth.sample(x, y, z)),
            weirdness: quantise(self.ridges.sample(x, y, z)),
        }
    }
}

/// The game compares these as whole ten-thousandths.
fn quantise(value: f32) -> i64 {
    (value as f64 * 10000.0) as i64
}

/// One biome and the corner of climate space it answers to.
#[derive(Clone, Debug)]
struct Entry {
    biome: String,
    /// Temperature, humidity, continentalness, erosion, depth, weirdness.
    ranges: [(i64, i64); 6],
    offset: i64,
}

/// Every biome the overworld can pick, with the climate each wants.
pub struct Biomes {
    entries: Vec<Entry>,
}

impl Default for Biomes {
    fn default() -> Self {
        Self::overworld()
    }
}

impl Biomes {
    pub fn overworld() -> Self {
        let Ok(json) = serde_json::from_str::<Value>(OVERWORLD_BIOMES) else {
            return Self { entries: Vec::new() };
        };
        let mut entries = Vec::new();
        for entry in json.get("entries").and_then(Value::as_array).into_iter().flatten() {
            let Some(biome) = entry.get("biome").and_then(Value::as_str) else {
                continue;
            };
            let Some(list) = entry.get("ranges").and_then(Value::as_array) else {
                continue;
            };
            let mut ranges = [(0i64, 0i64); 6];
            for (slot, range) in ranges.iter_mut().zip(list) {
                let pair = range.as_array().map(Vec::as_slice).unwrap_or_default();
                *slot = (
                    pair.first().and_then(Value::as_i64).unwrap_or(0),
                    pair.get(1).and_then(Value::as_i64).unwrap_or(0),
                );
            }
            entries.push(Entry {
                biome: biome.to_owned(),
                ranges,
                offset: entry.get("offset").and_then(Value::as_i64).unwrap_or(0),
            });
        }
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The biome whose corner of climate space this place is nearest to.
    pub fn nearest(&self, target: &Target) -> Option<&str> {
        let wanted = [
            target.temperature,
            target.humidity,
            target.continentalness,
            target.erosion,
            target.depth,
            target.weirdness,
        ];
        let mut best: Option<(i64, &Entry)> = None;
        for entry in &self.entries {
            let mut fitness = entry.offset * entry.offset;
            for (range, value) in entry.ranges.iter().zip(wanted.iter()) {
                let distance = distance_to(*range, *value);
                fitness += distance * distance;
            }
            if best.is_none_or(|(closest, _)| fitness < closest) {
                best = Some((fitness, entry));
            }
        }
        best.map(|(_, entry)| entry.biome.as_str())
    }
}

/// How far outside a range a value falls; nothing if it is inside.
fn distance_to((min, max): (i64, i64), value: i64) -> i64 {
    let above = value - max;
    let below = min - value;
    if above > 0 {
        above
    } else {
        below.max(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The data pack to read, from a prepared copy of the game's data.
    /// Point `GARNET_DATA` at the data directory to run this; without one
    /// there is nothing to compare against and the test stands aside.
    fn datapack() -> Option<std::path::PathBuf> {
        let base = std::env::var("GARNET_DATA").ok()?;
        let candidate = std::path::PathBuf::from(base).join("versions/26.3/datapack");
        candidate
            .join("minecraft/worldgen/noise/temperature.json")
            .exists()
            .then_some(candidate)
    }

    /// The climate and the biome at six places, against what the game
    /// itself reports for the same seed. See `scratchpad/oracle`.
    #[test]
    fn biomes_match_the_game() {
        let Some(datapack) = datapack() else {
            eprintln!("no prepared data pack found; skipping");
            return;
        };
        let sampler = Sampler::new(&datapack, 1234567890123);
        let biomes = Biomes::overworld();
        let expected: &[(i32, i32, i32, [i64; 6], &str)] = &[
            (0, 16, 0, [5720, -980, 2293, 4857, 2821, -520], "minecraft:desert"),
            (
                100,
                16,
                -250,
                [4897, -330, 6641, 2490, 4494, 7358],
                "minecraft:dripstone_caves",
            ),
            (
                -1500,
                16,
                3000,
                [3065, 1871, 10020, -1732, 6766, -4069],
                "minecraft:dripstone_caves",
            ),
            (5000, 16, 5000, [1325, -1362, -3896, -2782, 2512, 643], "minecraft:ocean"),
            (320, 8, -64, [3372, 863, 3187, 3744, 5019, 5752], "minecraft:dripstone_caves"),
            (-77, 20, 412, [6084, -4826, 628, 5099, 3182, 1289], "minecraft:desert"),
        ];
        for (x, y, z, climate, biome) in expected {
            let target = sampler.sample(x >> 2, y >> 2, z >> 2);
            let got = [
                target.temperature,
                target.humidity,
                target.continentalness,
                target.erosion,
                target.depth,
                target.weirdness,
            ];
            assert_eq!(&got, climate, "climate at {x},{y},{z}");
            assert_eq!(biomes.nearest(&target), Some(*biome), "biome at {x},{y},{z}");
        }
    }

    #[test]
    fn the_biome_table_is_the_whole_one() {
        let biomes = Biomes::overworld();
        assert_eq!(biomes.len(), 7594, "the table dumped from the game has this many corners");
    }
}
