//! The rock and the water: one column of a vanilla world.
//!
//! Everything under this has been checked against the game a piece at a
//! time. Here they are put together the way the game puts them together:
//! the density says where the rock is, and the aquifer says what fills
//! everything else.

use super::aquifer::{Aquifer, GlobalFluid, Session, Substance};
use super::density::{Graph, Node};
use super::rng::Xoroshiro;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

/// Vanilla's lava sits this far down wherever there is nothing else.
const LAVA_LEVEL: i32 = -54;

/// A world's terrain, as its settings describe it.
pub struct Terrain {
    final_density: Arc<Node>,
    aquifer: Aquifer,
    pub sea_level: i32,
    pub min_y: i32,
    pub height: i32,
    pub default_block: String,
    pub default_fluid: String,
}

impl Terrain {
    /// Reads the overworld's noise settings for a seed. Nothing comes back
    /// if the data pack has no generation data in it.
    pub fn overworld(datapack: &Path, seed: i64) -> Option<Self> {
        let path = datapack
            .join("minecraft")
            .join("worldgen")
            .join("noise_settings")
            .join("overworld.json");
        let json: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let mut graph = Graph::load(datapack, seed);
        let final_density = graph.function("minecraft:overworld/final_density")?;

        let aquifers = json.get("aquifers")?;
        let mut function = |key: &str| -> Arc<Node> {
            match aquifers.get(key) {
                Some(value) => graph.inline(value),
                None => Arc::new(Node::Constant(0.0)),
            }
        };
        let barrier = function("barrier");
        let floodedness = function("fluid_level_floodedness");
        let spread = function("fluid_level_spread");
        let lava = function("lava");
        let exclusion = function("exclusion");
        let surface_level = function("surface_level");

        let sea_level = json.get("sea_level").and_then(Value::as_i64).unwrap_or(63) as i32;
        let noise = json.get("noise");
        Some(Self {
            final_density,
            aquifer: Aquifer::new(
                barrier,
                floodedness,
                spread,
                lava,
                exclusion,
                surface_level,
                // The pockets are placed from a source of the world's own,
                // named for them and then asked per position.
                Xoroshiro::from_seed(seed)
                    .fork_positional()
                    .from_hash_of("minecraft:aquifer")
                    .fork_positional(),
                GlobalFluid {
                    sea_level,
                    lava_level: LAVA_LEVEL.min(sea_level),
                },
            ),
            sea_level,
            min_y: noise.and_then(|n| n.get("min_y")).and_then(Value::as_i64).unwrap_or(-64) as i32,
            height: noise.and_then(|n| n.get("height")).and_then(Value::as_i64).unwrap_or(384) as i32,
            default_block: json
                .get("default_block")
                .and_then(|b| b.get("Name").or(Some(b)))
                .and_then(Value::as_str)
                .unwrap_or("minecraft:stone")
                .to_owned(),
            default_fluid: json
                .get("default_fluid")
                .and_then(|b| b.get("Name").or(Some(b)))
                .and_then(Value::as_str)
                .unwrap_or("minecraft:water")
                .to_owned(),
        })
    }

    /// A working set of caches, good for one chunk.
    pub fn session(&self) -> Filler<'_> {
        Filler {
            terrain: self,
            aquifer: self.aquifer.session(),
        }
    }
}

/// Fills blocks, keeping the caches a chunk's worth of work wants.
pub struct Filler<'a> {
    terrain: &'a Terrain,
    aquifer: Session<'a>,
}

impl Filler<'_> {
    /// What stands at a block: rock, air, or a fluid.
    pub fn substance(&mut self, x: i32, y: i32, z: i32) -> Substance {
        let density = self.terrain.final_density.sample(x, y, z);
        self.aquifer.substance(x, y, z, density)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datapack() -> Option<std::path::PathBuf> {
        let base = std::env::var("GARNET_DATA").ok()?;
        let candidate = std::path::PathBuf::from(base).join("versions/26.3/datapack");
        candidate
            .join("minecraft/worldgen/noise_settings/overworld.json")
            .exists()
            .then_some(candidate)
    }

    /// Five columns of the world, top to bottom, against what the game
    /// itself puts there: `#` rock, `~` water, `.` air. Written as runs so
    /// they can be read: `.257~8#61` is 257 of air, then 8 of water, then
    /// 61 of rock.
    #[test]
    fn columns_match_the_game() {
        let Some(datapack) = datapack() else {
            eprintln!("no prepared data pack found; skipping");
            return;
        };
        let terrain = Terrain::overworld(&datapack, 1234567890123).expect("overworld settings");
        let expected: &[((i32, i32), &str)] = &[
            ((0, 0), ".257~8#61.10#48"),
            ((100, -250), ".239#145"),
            ((-1500, 3000), ".217#48~9#45.9#56"),
            ((5000, 5000), ".257~15#112"),
            ((37, -412), ".250#61.9#64"),
        ];
        for ((x, z), want) in expected {
            let mut filler = terrain.session();
            let mut column = String::new();
            for y in (terrain.min_y..terrain.min_y + terrain.height).rev() {
                column.push(match filler.substance(*x, y, *z) {
                    Substance::Rock => '#',
                    Substance::Water => '~',
                    Substance::Lava => 'L',
                    Substance::Air => '.',
                });
            }
            assert_eq!(runs(&column), *want, "column at {x},{z}\n  got {}", runs(&column));
        }
    }

    /// The same column written as runs of each kind.
    fn runs(column: &str) -> String {
        let mut out = String::new();
        let mut last = None;
        let mut count = 0;
        for ch in column.chars() {
            if Some(ch) == last {
                count += 1;
            } else {
                if let Some(previous) = last {
                    out.push(previous);
                    out.push_str(&count.to_string());
                }
                last = Some(ch);
                count = 1;
            }
        }
        if let Some(previous) = last {
            out.push(previous);
            out.push_str(&count.to_string());
        }
        out
    }
}
