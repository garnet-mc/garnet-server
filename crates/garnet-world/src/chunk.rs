//! In-memory chunk columns.
//!
//! A chunk is 16x16 blocks wide and as tall as the dimension, split into
//! 16x16x16 sections. Each section stores a palette of block state ids plus
//! one u16 index per block, which keeps memory small (most sections have a
//! handful of distinct blocks) while staying trivial to read.

use crate::HeightRange;
use garnet_protocol::nbt::NbtCompound;
use garnet_protocol::ChunkPos;

/// Blocks per section.
pub const SECTION_VOLUME: usize = 16 * 16 * 16;
/// Biome cells per section (biomes are stored per 4x4x4 block).
pub const BIOME_VOLUME: usize = 4 * 4 * 4;

#[derive(Clone, Debug)]
pub struct ChunkSection {
    /// Distinct block state ids present in this section.
    pub palette: Vec<u32>,
    /// Index into `palette` for each block, ordered y, z, x (x fastest).
    indices: Vec<u16>,
    /// Biome ids per 4x4x4 cell, ordered y, z, x.
    pub biomes: [u32; BIOME_VOLUME],
    /// Cached count of non-air blocks; `None` means "recount".
    non_air: Option<u16>,
}

impl ChunkSection {
    /// A section filled with one block state.
    pub fn filled(state: u32, biome: u32) -> Self {
        Self {
            palette: vec![state],
            indices: vec![0; SECTION_VOLUME],
            biomes: [biome; BIOME_VOLUME],
            non_air: None,
        }
    }

    pub fn empty(air_state: u32, biome: u32) -> Self {
        let mut s = Self::filled(air_state, biome);
        s.non_air = Some(0);
        s
    }

    pub fn is_uniform(&self) -> bool {
        self.palette.len() == 1
    }

    fn index(x: usize, y: usize, z: usize) -> usize {
        (y << 8) | (z << 4) | x
    }

    pub fn get(&self, x: usize, y: usize, z: usize) -> u32 {
        self.palette[self.indices[Self::index(x, y, z)] as usize]
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, state: u32) {
        let palette_index = match self.palette.iter().position(|&s| s == state) {
            Some(i) => i,
            None => {
                self.palette.push(state);
                self.palette.len() - 1
            }
        };
        self.indices[Self::index(x, y, z)] = palette_index as u16;
        self.non_air = None;
    }

    pub fn get_biome(&self, x: usize, y: usize, z: usize) -> u32 {
        self.biomes[(y << 4) | (z << 2) | x]
    }

    pub fn set_biome(&mut self, x: usize, y: usize, z: usize, biome: u32) {
        self.biomes[(y << 4) | (z << 2) | x] = biome;
    }

    /// Palette index of every block, for encoders that want the raw layout.
    pub fn palette_indices(&self) -> &[u16] {
        &self.indices
    }

    /// Drops palette entries no block uses any more. Called before saving or
    /// sending so the wire format stays compact.
    pub fn compact(&mut self) {
        if self.palette.len() <= 1 {
            return;
        }
        let mut used = vec![false; self.palette.len()];
        for &i in &self.indices {
            used[i as usize] = true;
        }
        if used.iter().all(|&u| u) {
            return;
        }
        let mut remap = vec![0u16; self.palette.len()];
        let mut new_palette = Vec::with_capacity(self.palette.len());
        for (old, &state) in self.palette.iter().enumerate() {
            if used[old] {
                remap[old] = new_palette.len() as u16;
                new_palette.push(state);
            }
        }
        for i in &mut self.indices {
            *i = remap[*i as usize];
        }
        self.palette = new_palette;
    }

    /// Counts blocks for which `is_air` is false, caching the result.
    pub fn non_air_count(&mut self, is_air: impl Fn(u32) -> bool) -> u16 {
        if let Some(n) = self.non_air {
            return n;
        }
        let air_flags: Vec<bool> = self.palette.iter().map(|&s| is_air(s)).collect();
        let n = self.indices.iter().filter(|&&i| !air_flags[i as usize]).count() as u16;
        self.non_air = Some(n);
        n
    }

    pub fn count_matching(&self, pred: impl Fn(u32) -> bool) -> u16 {
        let flags: Vec<bool> = self.palette.iter().map(|&s| pred(s)).collect();
        self.indices.iter().filter(|&&i| flags[i as usize]).count() as u16
    }
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub pos: ChunkPos,
    pub range: HeightRange,
    /// `sections[0]` is the lowest section (`range.min_section()`).
    pub sections: Vec<ChunkSection>,
    /// Set whenever a block changes; cleared when saved.
    pub dirty: bool,
    /// Block entities as vanilla NBT compounds (`id`, `x`, `y`, `z`, ...).
    pub block_entities: Vec<NbtCompound>,
    /// Chunk-level NBT we loaded but do not interpret (structures, ticks...).
    /// Written back unchanged so nothing is lost across a load/save cycle.
    pub extra: NbtCompound,
}

impl Chunk {
    pub fn new_empty(pos: ChunkPos, range: HeightRange, air_state: u32, biome: u32) -> Self {
        Self {
            pos,
            range,
            sections: (0..range.section_count())
                .map(|_| ChunkSection::empty(air_state, biome))
                .collect(),
            dirty: false,
            block_entities: Vec::new(),
            extra: NbtCompound::new(),
        }
    }

    fn section_index(&self, y: i32) -> Option<usize> {
        if !self.range.contains(y) {
            return None;
        }
        Some(((y - self.range.min_y) >> 4) as usize)
    }

    /// Block state at chunk-local x/z (0-15) and world y.
    pub fn get_block(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        let section = self.section_index(y)?;
        Some(self.sections[section].get(x as usize & 15, (y & 15) as usize, z as usize & 15))
    }

    pub fn set_block(&mut self, x: i32, y: i32, z: i32, state: u32) -> bool {
        let Some(section) = self.section_index(y) else {
            return false;
        };
        self.sections[section].set(x as usize & 15, (y & 15) as usize, z as usize & 15, state);
        self.dirty = true;
        true
    }

    pub fn get_biome(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        let section = self.section_index(y)?;
        Some(self.sections[section].get_biome((x as usize & 15) >> 2, ((y & 15) as usize) >> 2, (z as usize & 15) >> 2))
    }

    /// Highest non-air block y at local x/z, or `None` for an empty column.
    pub fn highest_block(&self, x: i32, z: i32, is_air: impl Fn(u32) -> bool) -> Option<i32> {
        for y in (self.range.min_y..=self.range.max_y()).rev() {
            if let Some(state) = self.get_block(x, y, z) {
                if !is_air(state) {
                    return Some(y);
                }
            }
        }
        None
    }

    pub fn compact(&mut self) {
        for section in &mut self.sections {
            section.compact();
        }
    }
}
