//! Turning a chunk into the bytes of a `level_chunk_with_light` packet.
//!
//! Each section is written as:
//!
//! ```text
//! non-air block count   i16
//! liquid block count    i16   (added in 26.1)
//! block states          paletted container
//! biomes                paletted container
//! ```
//!
//! A paletted container is `bits per entry (u8)`, then a palette that depends
//! on the bit width, then the packed values as longs with **no length
//! prefix** (removed in 1.21.5). Bit width 0 means "every entry is the single
//! palette value" and carries no data at all.

use crate::chunk::{Chunk, ChunkSection, BIOME_VOLUME, SECTION_VOLUME};
use garnet_data::BlockRegistry;
use garnet_protocol::packets::play::clientbound::{ChunkData, LightData};
use garnet_protocol::PacketWriter;

/// Bit widths the client accepts for indirect (palette-indexed) storage.
const BLOCK_INDIRECT_MIN_BITS: u8 = 4;
const BLOCK_INDIRECT_MAX_BITS: u8 = 8;
const BIOME_INDIRECT_MIN_BITS: u8 = 1;
const BIOME_INDIRECT_MAX_BITS: u8 = 3;

/// Heightmap kinds the client wants, by their protocol ids.
const HEIGHTMAP_WORLD_SURFACE: i32 = 1;
const HEIGHTMAP_MOTION_BLOCKING: i32 = 4;
const HEIGHTMAP_MOTION_BLOCKING_NO_LEAVES: i32 = 5;

/// Everything the encoder needs that is not in the chunk itself.
pub struct EncodeContext<'a> {
    pub blocks: &'a BlockRegistry,
    /// Number of biomes in the registry we sent; sets the direct bit width.
    pub biome_count: usize,
}

pub fn encode_chunk(chunk: &mut Chunk, ctx: &EncodeContext) -> ChunkData {
    chunk.compact();
    let mut sections = PacketWriter::with_capacity(chunk.sections.len() * 512);
    for section in &mut chunk.sections {
        encode_section(section, ctx, &mut sections);
    }

    let heightmap = compute_heightmap(chunk, ctx.blocks);
    let packed = pack_heightmap(&heightmap, chunk.range.height);

    ChunkData {
        chunk_x: chunk.pos.x,
        chunk_z: chunk.pos.z,
        heightmaps: vec![
            (HEIGHTMAP_WORLD_SURFACE, packed.clone()),
            (HEIGHTMAP_MOTION_BLOCKING, packed.clone()),
            (HEIGHTMAP_MOTION_BLOCKING_NO_LEAVES, packed),
        ],
        sections: sections.into_inner(),
        block_entities: Vec::new(),
        light: full_bright_light(chunk.sections.len()),
    }
}

fn encode_section(section: &mut ChunkSection, ctx: &EncodeContext, w: &mut PacketWriter) {
    let blocks = ctx.blocks;
    w.write_i16(section.non_air_count(|s| blocks.is_air(s as i32)) as i16);
    w.write_i16(section.count_matching(|s| blocks.is_liquid(s as i32)) as i16);

    // Block states.
    if section.is_uniform() {
        w.write_u8(0);
        w.write_varint(section.palette[0] as i32);
    } else {
        let needed = bits_for(section.palette.len());
        if needed <= BLOCK_INDIRECT_MAX_BITS {
            let bits = needed.max(BLOCK_INDIRECT_MIN_BITS);
            w.write_u8(bits);
            w.write_varint(section.palette.len() as i32);
            for &state in &section.palette {
                w.write_varint(state as i32);
            }
            let values = section.palette_indices().iter().map(|&i| i as u64);
            write_packed(w, values, SECTION_VOLUME, bits);
        } else {
            let bits = blocks.bits_per_state();
            w.write_u8(bits);
            let palette = &section.palette;
            let values = section.palette_indices().iter().map(|&i| palette[i as usize] as u64);
            write_packed(w, values, SECTION_VOLUME, bits);
        }
    }

    // Biomes: build a tiny palette from the 64 cells.
    let mut palette: Vec<u32> = Vec::new();
    let mut indices = [0u8; BIOME_VOLUME];
    for (i, &biome) in section.biomes.iter().enumerate() {
        let idx = match palette.iter().position(|&b| b == biome) {
            Some(p) => p,
            None => {
                palette.push(biome);
                palette.len() - 1
            }
        };
        indices[i] = idx as u8;
    }
    if palette.len() == 1 {
        w.write_u8(0);
        w.write_varint(palette[0] as i32);
    } else {
        let needed = bits_for(palette.len());
        if needed <= BIOME_INDIRECT_MAX_BITS {
            let bits = needed.max(BIOME_INDIRECT_MIN_BITS);
            w.write_u8(bits);
            w.write_varint(palette.len() as i32);
            for &b in &palette {
                w.write_varint(b as i32);
            }
            write_packed(w, indices.iter().map(|&i| i as u64), BIOME_VOLUME, bits);
        } else {
            let bits = bits_for(ctx.biome_count).max(1);
            w.write_u8(bits);
            write_packed(w, indices.iter().map(|&i| palette[i as usize] as u64), BIOME_VOLUME, bits);
        }
    }
}

/// Bits needed to index `count` distinct values (`count` = 1 -> 0 bits).
fn bits_for(count: usize) -> u8 {
    let mut bits = 0u8;
    while (1usize << bits) < count {
        bits += 1;
    }
    bits
}

/// Packs `count` values of `bits` width into longs. Values never straddle a
/// long boundary (the 1.16+ layout); leftover high bits stay zero.
fn write_packed(w: &mut PacketWriter, values: impl Iterator<Item = u64>, count: usize, bits: u8) {
    let per_long = 64 / bits as usize;
    let long_count = count.div_ceil(per_long);
    let mut longs = vec![0u64; long_count];
    for (i, value) in values.enumerate() {
        let long_index = i / per_long;
        let shift = (i % per_long) * bits as usize;
        longs[long_index] |= value << shift;
    }
    for long in longs {
        w.write_u64(long);
    }
}

/// Height of the highest non-air block + 1, relative to the world floor, for
/// each x/z column (z-major order like vanilla: index = z * 16 + x).
fn compute_heightmap(chunk: &Chunk, blocks: &BlockRegistry) -> [u32; 256] {
    let mut heights = [0u32; 256];
    for z in 0..16 {
        for x in 0..16 {
            if let Some(y) = chunk.highest_block(x, z, |s| blocks.is_air(s as i32)) {
                heights[(z * 16 + x) as usize] = (y - chunk.range.min_y + 1) as u32;
            }
        }
    }
    heights
}

/// Heightmaps use `ceil(log2(height + 1))` bits per entry, packed like sections.
pub fn pack_heightmap(heights: &[u32; 256], world_height: i32) -> Vec<i64> {
    let bits = bits_for(world_height as usize + 1).max(1);
    let mut w = PacketWriter::new();
    write_packed(&mut w, heights.iter().map(|&h| h as u64), 256, bits);
    w.as_slice()
        .chunks(8)
        .map(|c| i64::from_be_bytes(c.try_into().unwrap()))
        .collect()
}

/// Sky light 15 everywhere and no block light. Simple and good enough until
/// real light propagation lands; the client still darkens at night.
fn full_bright_light(section_count: usize) -> LightData {
    // One bit per section plus one below and one above the world.
    let mask_bits = section_count + 2;
    let all_set = mask_words(mask_bits);
    LightData {
        sky_mask: all_set.clone(),
        block_mask: Vec::new(),
        empty_sky_mask: Vec::new(),
        empty_block_mask: all_set,
        sky_arrays: (0..mask_bits).map(|_| vec![0xFF; 2048]).collect(),
        block_arrays: Vec::new(),
    }
}

fn mask_words(bits: usize) -> Vec<u64> {
    let mut words = vec![0u64; bits.div_ceil(64)];
    for bit in 0..bits {
        words[bit / 64] |= 1 << (bit % 64);
    }
    words
}
