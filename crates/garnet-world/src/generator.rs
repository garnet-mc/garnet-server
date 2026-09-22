//! Terrain generators. These produce brand new chunks; anything already on
//! disk is loaded by `anvil` instead.

use crate::chunk::Chunk;
use crate::HeightRange;
use garnet_data::{BlockRegistry, DynamicRegistries};
use garnet_protocol::ChunkPos;
use noise::{Fbm, MultiFractal, NoiseFn, Perlin};

pub const SEA_LEVEL: i32 = 63;

pub trait WorldGenerator: Send + Sync {
    fn generate(&self, pos: ChunkPos, range: HeightRange) -> Chunk;
    /// The y of the first air block above ground at world x/z, used for
    /// placing the spawn point.
    fn surface_y(&self, x: i32, z: i32) -> i32;
}

/// Block state ids the generators need, resolved once from the registry.
#[derive(Clone, Copy, Debug)]
pub struct Blocks {
    pub air: u32,
    pub bedrock: u32,
    pub stone: u32,
    pub deepslate: u32,
    pub dirt: u32,
    pub grass_block: u32,
    pub sand: u32,
    pub gravel: u32,
    pub water: u32,
    pub oak_log: u32,
    pub oak_leaves: u32,
    pub short_grass: u32,
    pub snow_block: u32,
}

impl Blocks {
    pub fn resolve(registry: &BlockRegistry) -> Self {
        let get = |name: &str| registry.default_state(name).unwrap_or(0) as u32;
        Self {
            air: get("air"),
            bedrock: get("bedrock"),
            stone: get("stone"),
            deepslate: get("deepslate"),
            dirt: get("dirt"),
            grass_block: get("grass_block"),
            sand: get("sand"),
            gravel: get("gravel"),
            water: get("water"),
            oak_log: get("oak_log"),
            oak_leaves: get("oak_leaves"),
            short_grass: get("short_grass"),
            snow_block: get("snow_block"),
        }
    }
}

/// Biome ids the generators use, resolved from the synced biome registry.
#[derive(Clone, Copy, Debug)]
pub struct Biomes {
    pub plains: u32,
    pub forest: u32,
    pub ocean: u32,
    pub beach: u32,
    pub snowy_plains: u32,
}

impl Biomes {
    pub fn resolve(dynamic: &DynamicRegistries) -> Self {
        let get = |name: &str| dynamic.id_of("worldgen/biome", name).unwrap_or(0) as u32;
        Self {
            plains: get("plains"),
            forest: get("forest"),
            ocean: get("ocean"),
            beach: get("beach"),
            snowy_plains: get("snowy_plains"),
        }
    }
}

/// Superflat: a fixed stack of layers, bottom up.
pub struct FlatGenerator {
    pub layers: Vec<(u32, i32)>,
    pub biome: u32,
    pub air: u32,
}

impl FlatGenerator {
    /// The classic bedrock / dirt / dirt / dirt / grass world.
    pub fn classic(blocks: &Blocks, biome: u32) -> Self {
        Self {
            layers: vec![(blocks.bedrock, 1), (blocks.dirt, 3), (blocks.grass_block, 1)],
            biome,
            air: blocks.air,
        }
    }

    fn top_y(&self, range: HeightRange) -> i32 {
        range.min_y + self.layers.iter().map(|(_, n)| n).sum::<i32>()
    }
}

impl WorldGenerator for FlatGenerator {
    fn generate(&self, pos: ChunkPos, range: HeightRange) -> Chunk {
        let mut chunk = Chunk::new_empty(pos, range, self.air, self.biome);
        let mut y = range.min_y;
        for &(state, count) in &self.layers {
            for _ in 0..count {
                for z in 0..16 {
                    for x in 0..16 {
                        chunk.set_block(x, y, z, state);
                    }
                }
                y += 1;
            }
        }
        chunk.dirty = false;
        chunk
    }

    fn surface_y(&self, _x: i32, _z: i32) -> i32 {
        self.top_y(HeightRange::OVERWORLD)
    }
}

/// Rolling hills with oceans, beaches and scattered oak trees. Not vanilla
/// terrain, but pleasant to walk around in and fast to generate.
pub struct NoiseGenerator {
    seed: u32,
    blocks: Blocks,
    biomes: Biomes,
    continents: Fbm<Perlin>,
    hills: Fbm<Perlin>,
    detail: Fbm<Perlin>,
}

impl NoiseGenerator {
    pub fn new(seed: i64, blocks: Blocks, biomes: Biomes) -> Self {
        let seed = seed as u32;
        Self {
            seed,
            blocks,
            biomes,
            continents: Fbm::<Perlin>::new(seed).set_octaves(3).set_frequency(0.0015),
            hills: Fbm::<Perlin>::new(seed.wrapping_add(1)).set_octaves(4).set_frequency(0.008),
            detail: Fbm::<Perlin>::new(seed.wrapping_add(2)).set_octaves(2).set_frequency(0.05),
        }
    }

    /// Terrain height at a world x/z (the y of the topmost solid block).
    fn height_at(&self, x: i32, z: i32) -> i32 {
        let (fx, fz) = (x as f64, z as f64);
        let continent = self.continents.get([fx, fz]); // -1..1, slow
        let hill = self.hills.get([fx, fz]);
        let detail = self.detail.get([fx, fz]);
        // Continents push land up or down around sea level; hills add relief
        // on land only so the sea floor stays gentle.
        let base = SEA_LEVEL as f64 + 2.0 + continent * 28.0;
        let land = ((continent + 0.15) * 2.0).clamp(0.0, 1.0);
        let relief = hill * 24.0 * land + detail * 2.0;
        (base + relief).round() as i32
    }

    fn is_snowy(&self, x: i32, z: i32) -> bool {
        // Cold bands drift across the map with a very slow noise.
        self.continents.get([z as f64 * 0.4 + 10_000.0, x as f64 * 0.4]) > 0.42
    }

    /// Deterministic per-column randomness for decorations.
    fn column_hash(&self, x: i32, z: i32) -> u32 {
        let mut h = self.seed ^ (x as u32).wrapping_mul(0x9E37_79B9) ^ (z as u32).wrapping_mul(0x85EB_CA6B);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2C1B_3C6D);
        h ^= h >> 12;
        h = h.wrapping_mul(0x297A_2D39);
        h ^ (h >> 15)
    }
}

impl WorldGenerator for NoiseGenerator {
    fn generate(&self, pos: ChunkPos, range: HeightRange) -> Chunk {
        let b = self.blocks;
        let mut chunk = Chunk::new_empty(pos, range, b.air, self.biomes.plains);
        let mut heights = [[0i32; 16]; 16];

        for z in 0..16 {
            for x in 0..16 {
                let (wx, wz) = (pos.x * 16 + x, pos.z * 16 + z);
                let height = self.height_at(wx, wz).clamp(range.min_y + 2, range.max_y() - 8);
                heights[x as usize][z as usize] = height;
                let snowy = self.is_snowy(wx, wz);
                let underwater = height < SEA_LEVEL;
                let beach = !underwater && height <= SEA_LEVEL + 1;

                let biome = if underwater {
                    self.biomes.ocean
                } else if beach {
                    self.biomes.beach
                } else if snowy {
                    self.biomes.snowy_plains
                } else if self.column_hash(wx / 64, wz / 64) % 3 == 0 {
                    self.biomes.forest
                } else {
                    self.biomes.plains
                };
                for y in (range.min_y..=height.max(SEA_LEVEL)).step_by(4) {
                    chunk.sections[((y - range.min_y) >> 4) as usize].set_biome(
                        (x >> 2) as usize,
                        ((y & 15) >> 2) as usize,
                        (z >> 2) as usize,
                        biome,
                    );
                }

                for y in range.min_y..=height {
                    let depth = height - y;
                    let state = if y == range.min_y {
                        b.bedrock
                    } else if y < 0 && depth > 4 {
                        b.deepslate
                    } else if depth == 0 {
                        if underwater {
                            if self.column_hash(wx, wz) % 4 == 0 { b.gravel } else { b.sand }
                        } else if beach {
                            b.sand
                        } else if snowy {
                            b.snow_block
                        } else {
                            b.grass_block
                        }
                    } else if depth <= 3 {
                        if underwater || beach { b.sand } else { b.dirt }
                    } else {
                        b.stone
                    };
                    chunk.set_block(x, y, z, state);
                }
                for y in (height + 1)..=SEA_LEVEL {
                    chunk.set_block(x, y, z, b.water);
                }
            }
        }

        // Decorations: grass tufts and oak trees on grass, kept inside the
        // chunk so we never have to touch neighbours.
        for z in 2..14 {
            for x in 2..14 {
                let (wx, wz) = (pos.x * 16 + x, pos.z * 16 + z);
                let ground = heights[x as usize][z as usize];
                if chunk.get_block(x, ground, z) != Some(b.grass_block) {
                    continue;
                }
                let roll = self.column_hash(wx, wz) % 1000;
                if roll < 60 {
                    chunk.set_block(x, ground + 1, z, b.short_grass);
                } else if roll < 68 {
                    place_oak(&mut chunk, x, ground + 1, z, &b, 4 + (roll % 3) as i32);
                }
            }
        }

        chunk.dirty = false;
        chunk
    }

    fn surface_y(&self, x: i32, z: i32) -> i32 {
        self.height_at(x, z).max(SEA_LEVEL) + 1
    }
}

fn place_oak(chunk: &mut Chunk, x: i32, y: i32, z: i32, b: &Blocks, trunk: i32) {
    for dy in 0..trunk {
        chunk.set_block(x, y + dy, z, b.oak_log);
    }
    let top = y + trunk;
    for dy in -2i32..=1 {
        let radius: i32 = if dy >= 0 { 1 } else { 2 };
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                // Round off the corners of the leaf blob.
                if dx.abs() == radius && dz.abs() == radius && (dy == 1 || dy == -2 && radius == 2 && (dx + dz) % 2 == 0) {
                    continue;
                }
                let (lx, ly, lz) = (x + dx, top + dy, z + dz);
                if chunk.get_block(lx, ly, lz) == Some(b.air) {
                    chunk.set_block(lx, ly, lz, b.oak_leaves);
                }
            }
        }
    }
}
