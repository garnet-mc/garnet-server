//! Sky and block light for one chunk column.
//!
//! Light is stored as one nibble per block in each section. Sky light comes
//! straight down from above the world at full strength until it hits
//! something, then spreads sideways and down losing one level per block
//! (more through leaves, water and the like). Block light starts at each
//! glowing block and spreads the same way.
//!
//! The propagation runs inside one chunk and takes the light already known
//! at the borders of loaded neighbours as extra sources, so caves and
//! rooms that straddle chunk edges still come out right once both sides
//! are loaded. It is a plain breadth-first flood; a full column takes a
//! millisecond or two.

use crate::chunk::Chunk;
use garnet_data::light::LightTable;
use std::collections::VecDeque;

pub const SECTION_LIGHT_BYTES: usize = 2048;

/// Light for every section of a chunk column: `None` means all zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkLight {
    pub sky: Vec<Option<Box<[u8; SECTION_LIGHT_BYTES]>>>,
    pub block: Vec<Option<Box<[u8; SECTION_LIGHT_BYTES]>>>,
}

impl ChunkLight {
    pub fn empty(sections: usize) -> Self {
        Self {
            sky: (0..sections).map(|_| None).collect(),
            block: (0..sections).map(|_| None).collect(),
        }
    }

    fn get(layer: &[Option<Box<[u8; SECTION_LIGHT_BYTES]>>], section: usize, x: usize, y: usize, z: usize) -> u8 {
        match &layer[section] {
            None => 0,
            Some(data) => {
                let i = (y << 8) | (z << 4) | x;
                let byte = data[i >> 1];
                if i & 1 == 0 {
                    byte & 0x0F
                } else {
                    byte >> 4
                }
            }
        }
    }

    fn set(layer: &mut [Option<Box<[u8; SECTION_LIGHT_BYTES]>>], section: usize, x: usize, y: usize, z: usize, value: u8) {
        if value == 0 && layer[section].is_none() {
            return;
        }
        let data = layer[section].get_or_insert_with(|| Box::new([0u8; SECTION_LIGHT_BYTES]));
        let i = (y << 8) | (z << 4) | x;
        let byte = &mut data[i >> 1];
        if i & 1 == 0 {
            *byte = (*byte & 0xF0) | (value & 0x0F);
        } else {
            *byte = (*byte & 0x0F) | (value << 4);
        }
    }

    /// Sky light at a chunk-local block, section index + local y.
    pub fn sky_at(&self, section: usize, x: usize, y: usize, z: usize) -> u8 {
        Self::get(&self.sky, section, x, y, z)
    }

    pub fn block_at(&self, section: usize, x: usize, y: usize, z: usize) -> u8 {
        Self::get(&self.block, section, x, y, z)
    }
}

/// Light along the four side faces of a neighbouring chunk, as seen from
/// this chunk: `sky[face][index]` where index runs over the 16 blocks along
/// the shared edge for every y in the column.
pub struct Neighbours<'a> {
    /// Chunks at -x, +x, -z, +z.
    pub sides: [Option<&'a Chunk>; 4],
}

const DIRS: [(i32, i32, i32); 6] = [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)];

/// Recomputes both light layers of `chunk` from scratch.
pub fn compute(chunk: &mut Chunk, table: &LightTable, neighbours: &Neighbours) {
    let height = chunk.sections.len() * 16;
    let min_y = chunk.range.min_y;

    // Working copies as flat arrays: index = (y * 16 + z) * 16 + x, y from the world floor.
    let mut opacity = vec![0u8; height * 256];
    let mut emission = vec![0u8; height * 256];
    for (si, section) in chunk.sections.iter().enumerate() {
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let state = section.get(x, y, z);
                    let i = ((si * 16 + y) * 16 + z) * 16 + x;
                    opacity[i] = table.opacity(state);
                    emission[i] = table.emission(state);
                }
            }
        }
    }

    let mut sky = vec![0u8; height * 256];
    let mut block = vec![0u8; height * 256];
    let mut queue: VecDeque<(usize, u8)> = VecDeque::new();

    // Sky: straight down first, free while nothing is in the way.
    for z in 0..16 {
        for x in 0..16 {
            let mut level: u8 = 15;
            for y in (0..height).rev() {
                let i = (y * 16 + z) * 16 + x;
                let o = opacity[i];
                if o > 0 {
                    level = level.saturating_sub(o.max(1));
                }
                if level == 0 {
                    break;
                }
                sky[i] = level;
                queue.push_back((i, level));
            }
        }
    }
    seed_from_neighbours(chunk, neighbours, height, min_y, true, &opacity, &mut sky, &mut queue);
    flood(&mut sky, &opacity, height, &mut queue);

    // Block light: every glowing block is a source.
    for (i, &e) in emission.iter().enumerate() {
        if e > 0 {
            block[i] = e;
            queue.push_back((i, e));
        }
    }
    seed_from_neighbours(chunk, neighbours, height, min_y, false, &opacity, &mut block, &mut queue);
    flood(&mut block, &opacity, height, &mut queue);

    // Pack the results back into nibbles.
    let mut light = ChunkLight::empty(chunk.sections.len());
    for y in 0..height {
        for z in 0..16 {
            for x in 0..16 {
                let i = (y * 16 + z) * 16 + x;
                ChunkLight::set(&mut light.sky, y / 16, x, y & 15, z, sky[i]);
                ChunkLight::set(&mut light.block, y / 16, x, y & 15, z, block[i]);
            }
        }
    }
    chunk.light = Some(light);
}

/// Light at the border of a loaded neighbour spills into this chunk.
#[allow(clippy::too_many_arguments)]
fn seed_from_neighbours(
    chunk: &Chunk,
    neighbours: &Neighbours,
    height: usize,
    min_y: i32,
    sky: bool,
    opacity: &[u8],
    levels: &mut [u8],
    queue: &mut VecDeque<(usize, u8)>,
) {
    let _ = min_y;
    // (neighbour index, our x or z at the edge, their x or z at the edge, edge runs along z?)
    let faces = [(0usize, 0usize, 15usize, true), (1, 15, 0, true), (2, 0, 15, false), (3, 15, 0, false)];
    for (n, ours, theirs, along_z) in faces {
        let Some(other) = neighbours.sides[n] else { continue };
        let Some(other_light) = &other.light else { continue };
        if other.sections.len() != chunk.sections.len() {
            continue;
        }
        for y in 0..height {
            for k in 0..16 {
                let (x, z, tx, tz) = if along_z { (ours, k, theirs, k) } else { (k, ours, k, theirs) };
                let their_level = if sky {
                    other_light.sky_at(y / 16, tx, y & 15, tz)
                } else {
                    other_light.block_at(y / 16, tx, y & 15, tz)
                };
                if their_level <= 1 {
                    continue;
                }
                let i = (y * 16 + z) * 16 + x;
                let mine = their_level.saturating_sub(opacity[i].max(1));
                if mine > levels[i] {
                    levels[i] = mine;
                    queue.push_back((i, mine));
                }
            }
        }
    }
}

/// Breadth-first spread: each step costs at least one level, more through
/// blocks that dim light.
fn flood(levels: &mut [u8], opacity: &[u8], height: usize, queue: &mut VecDeque<(usize, u8)>) {
    while let Some((i, level)) = queue.pop_front() {
        if levels[i] != level || level <= 1 {
            continue;
        }
        let x = (i & 15) as i32;
        let z = ((i >> 4) & 15) as i32;
        let y = (i >> 8) as i32;
        for (dx, dy, dz) in DIRS {
            let (nx, ny, nz) = (x + dx, y + dy, z + dz);
            if !(0..16).contains(&nx) || !(0..16).contains(&nz) || ny < 0 || ny >= height as i32 {
                continue;
            }
            let j = ((ny as usize * 16 + nz as usize) * 16) + nx as usize;
            let next = level.saturating_sub(opacity[j].max(1));
            if next > levels[j] {
                levels[j] = next;
                queue.push_back((j, next));
            }
        }
    }
}
