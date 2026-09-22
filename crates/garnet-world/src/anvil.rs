//! Vanilla region files (`region/r.<x>.<z>.mca`) and `level.dat`.
//!
//! Region layout:
//!
//! ```text
//! 0x0000  1024 x (3-byte sector offset, 1-byte sector count)   chunk locations
//! 0x1000  1024 x u32                                            timestamps
//! 0x2000  chunk sectors of 4096 bytes:
//!           u32 length, u8 compression (1 gzip, 2 zlib, 3 none), payload
//! ```
//!
//! Chunk payloads are NBT with block *names* in each section's palette, which
//! is what makes saves survive version bumps (see `compat`).

use crate::chunk::{Chunk, ChunkSection, BIOME_VOLUME, SECTION_VOLUME};
use crate::compat;
use crate::HeightRange;
use anyhow::{bail, Context, Result};
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use garnet_data::{BlockRegistry, DynamicRegistries};
use garnet_protocol::nbt::{NbtCompound, NbtTag};
use garnet_protocol::ChunkPos;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SECTOR: usize = 4096;
const COMPRESSION_GZIP: u8 = 1;
const COMPRESSION_ZLIB: u8 = 2;
const COMPRESSION_NONE: u8 = 3;

/// Lookups the loader/saver needs to translate names <-> ids.
pub struct AnvilContext<'a> {
    pub blocks: &'a BlockRegistry,
    pub dynamic: &'a DynamicRegistries,
    pub data_version: i32,
}

/// One open region file.
pub struct RegionFile {
    file: File,
    locations: [u32; 1024],
    timestamps: [u32; 1024],
    /// Which 4 KB sectors are in use (index 0 and 1 are the header).
    used: Vec<bool>,
}

impl RegionFile {
    pub fn path_for(region_dir: &Path, chunk: ChunkPos) -> PathBuf {
        region_dir.join(format!("r.{}.{}.mca", chunk.x >> 5, chunk.z >> 5))
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let len = file.metadata()?.len() as usize;
        let mut locations = [0u32; 1024];
        let mut timestamps = [0u32; 1024];
        if len < SECTOR * 2 {
            // New or truncated file: write an empty header.
            file.set_len(0)?;
            file.write_all(&[0u8; SECTOR * 2])?;
        } else {
            let mut header = [0u8; SECTOR * 2];
            file.seek(SeekFrom::Start(0))?;
            file.read_exact(&mut header)?;
            for i in 0..1024 {
                locations[i] = u32::from_be_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
                timestamps[i] = u32::from_be_bytes(header[SECTOR + i * 4..SECTOR + i * 4 + 4].try_into().unwrap());
            }
        }
        let sector_count = (len.max(SECTOR * 2)).div_ceil(SECTOR);
        let mut used = vec![false; sector_count.max(2)];
        used[0] = true;
        used[1] = true;
        for loc in locations {
            let (offset, count) = (loc >> 8, loc & 0xFF);
            for s in offset..offset + count {
                if (s as usize) < used.len() {
                    used[s as usize] = true;
                }
            }
        }
        Ok(Self {
            file,
            locations,
            timestamps,
            used,
        })
    }

    fn index(chunk: ChunkPos) -> usize {
        ((chunk.x & 31) + (chunk.z & 31) * 32) as usize
    }

    pub fn has_chunk(&self, chunk: ChunkPos) -> bool {
        self.locations[Self::index(chunk)] != 0
    }

    /// Reads the raw NBT root of a chunk, or `None` if it was never saved.
    pub fn read_chunk_nbt(&mut self, chunk: ChunkPos) -> Result<Option<NbtCompound>> {
        let loc = self.locations[Self::index(chunk)];
        if loc == 0 {
            return Ok(None);
        }
        let offset = (loc >> 8) as u64 * SECTOR as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut head = [0u8; 5];
        self.file.read_exact(&mut head)?;
        let length = u32::from_be_bytes(head[..4].try_into().unwrap()) as usize;
        if length < 1 || length > 32 * SECTOR * 8 {
            bail!("chunk {:?} has an implausible length {length}", chunk);
        }
        let mut payload = vec![0u8; length - 1];
        self.file.read_exact(&mut payload)?;
        let raw = match head[4] {
            COMPRESSION_GZIP => {
                let mut out = Vec::new();
                GzDecoder::new(&payload[..]).read_to_end(&mut out)?;
                out
            }
            COMPRESSION_ZLIB => {
                let mut out = Vec::new();
                ZlibDecoder::new(&payload[..]).read_to_end(&mut out)?;
                out
            }
            COMPRESSION_NONE => payload,
            other => bail!("chunk {:?} uses unsupported compression {other}", chunk),
        };
        let (_, root, _) = NbtTag::read_named(&raw)?;
        match root {
            NbtTag::Compound(c) => Ok(Some(c)),
            _ => bail!("chunk {:?} root is not a compound", chunk),
        }
    }

    /// Writes a chunk's NBT, reusing its old sectors when it still fits and
    /// appending otherwise. The header is updated last so a crash never
    /// leaves it pointing at half-written data.
    pub fn write_chunk_nbt(&mut self, chunk: ChunkPos, root: &NbtCompound) -> Result<()> {
        let mut raw = Vec::new();
        NbtTag::Compound(root.clone()).write_named("", &mut raw);
        let mut compressed = Vec::with_capacity(raw.len() / 3);
        let mut enc = ZlibEncoder::new(&mut compressed, Compression::default());
        enc.write_all(&raw)?;
        enc.finish()?;

        let total = compressed.len() + 5;
        let sectors_needed = total.div_ceil(SECTOR);
        if sectors_needed > 255 {
            bail!("chunk {:?} is too large to store ({} bytes)", chunk, total);
        }

        let idx = Self::index(chunk);
        let old = self.locations[idx];
        let (old_offset, old_count) = ((old >> 8) as usize, (old & 0xFF) as usize);

        let offset = if old != 0 && sectors_needed <= old_count {
            old_offset
        } else {
            // Free the old sectors and find a run of free ones (or append).
            for s in old_offset..old_offset + old_count {
                if s < self.used.len() {
                    self.used[s] = false;
                }
            }
            self.find_free_run(sectors_needed)
        };
        for s in offset..offset + sectors_needed {
            if s >= self.used.len() {
                self.used.resize(s + 1, false);
            }
            self.used[s] = true;
        }

        let mut buf = Vec::with_capacity(sectors_needed * SECTOR);
        buf.extend_from_slice(&((compressed.len() + 1) as u32).to_be_bytes());
        buf.push(COMPRESSION_ZLIB);
        buf.extend_from_slice(&compressed);
        buf.resize(sectors_needed * SECTOR, 0);
        self.file.seek(SeekFrom::Start((offset * SECTOR) as u64))?;
        self.file.write_all(&buf)?;

        let loc = ((offset as u32) << 8) | sectors_needed as u32;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        self.locations[idx] = loc;
        self.timestamps[idx] = stamp;
        self.file.seek(SeekFrom::Start((idx * 4) as u64))?;
        self.file.write_all(&loc.to_be_bytes())?;
        self.file.seek(SeekFrom::Start((SECTOR + idx * 4) as u64))?;
        self.file.write_all(&stamp.to_be_bytes())?;
        self.file.flush()?;
        Ok(())
    }

    fn find_free_run(&self, count: usize) -> usize {
        let mut run_start = 2;
        let mut run_len = 0;
        for (i, &in_use) in self.used.iter().enumerate().skip(2) {
            if in_use {
                run_len = 0;
                run_start = i + 1;
            } else {
                run_len += 1;
                if run_len == count {
                    return run_start;
                }
            }
        }
        self.used.len().max(2)
    }
}

// ---- chunk <-> NBT ----

/// Builds a [`Chunk`] from vanilla chunk NBT. Sections not present in the
/// file are left empty.
pub fn chunk_from_nbt(root: &NbtCompound, pos: ChunkPos, range: HeightRange, ctx: &AnvilContext) -> Result<Chunk> {
    let air = ctx.blocks.default_state("air").unwrap_or(0) as u32;
    let default_biome = ctx.dynamic.id_of("worldgen/biome", "plains").unwrap_or(0) as u32;
    let mut chunk = Chunk::new_empty(pos, range, air, default_biome);

    let saved_version = root.get_i32("DataVersion").unwrap_or(0);
    if saved_version > ctx.data_version {
        tracing::warn!(
            "chunk {:?} was saved by a newer Minecraft (data version {saved_version} > {}); loading anyway",
            pos, ctx.data_version
        );
    }

    for section_tag in root.get_list("sections").unwrap_or(&[]) {
        let Some(section) = section_tag.as_compound() else { continue };
        let y = section.get_i32("Y").unwrap_or(i32::MIN);
        let index = y - range.min_section();
        if index < 0 || index as usize >= chunk.sections.len() {
            continue;
        }
        let target = &mut chunk.sections[index as usize];

        if let Some(states) = section.get_compound("block_states") {
            let palette: Vec<u32> = states
                .get_list("palette")
                .unwrap_or(&[])
                .iter()
                .map(|entry| palette_entry_to_state(entry, ctx.blocks, air))
                .collect();
            let data = states.get_long_array("data").unwrap_or(&[]);
            fill_section_blocks(target, &palette, data);
        }
        if let Some(biomes) = section.get_compound("biomes") {
            let palette: Vec<u32> = biomes
                .get_list("palette")
                .unwrap_or(&[])
                .iter()
                .map(|entry| {
                    let name = entry.as_str().unwrap_or("minecraft:plains");
                    let name = compat::current_biome_name(name);
                    ctx.dynamic.id_of("worldgen/biome", &name).map(|i| i as u32).unwrap_or_else(|| {
                        compat::warn_unknown_once("biome", &name);
                        default_biome
                    })
                })
                .collect();
            let data = biomes.get_long_array("data").unwrap_or(&[]);
            fill_section_biomes(target, &palette, data);
        }
    }

    if let Some(entities) = root.get_list("block_entities") {
        chunk.block_entities = entities.iter().filter_map(|t| t.as_compound().cloned()).collect();
    }

    // Light, if this chunk was saved with it (vanilla layout: 2048 bytes per section).
    if root.get_bool("isLightOn") == Some(true) {
        let mut light = crate::light::ChunkLight::empty(chunk.sections.len());
        let mut any = false;
        for section_tag in root.get_list("sections").unwrap_or(&[]) {
            let Some(section) = section_tag.as_compound() else { continue };
            let index = section.get_i32("Y").unwrap_or(i32::MIN) - range.min_section();
            if index < 0 || index as usize >= chunk.sections.len() {
                continue;
            }
            for (key, layer) in [("SkyLight", &mut light.sky), ("BlockLight", &mut light.block)] {
                if let Some(NbtTag::ByteArray(bytes)) = section.get(key) {
                    if bytes.len() == crate::light::SECTION_LIGHT_BYTES {
                        let mut data = Box::new([0u8; crate::light::SECTION_LIGHT_BYTES]);
                        for (d, b) in data.iter_mut().zip(bytes) {
                            *d = *b as u8;
                        }
                        layer[index as usize] = Some(data);
                        any = true;
                    }
                }
            }
        }
        if any {
            chunk.light = Some(light);
        }
    }

    // Keep everything else (structures, ticks, heightmaps...) so re-saving
    // the chunk never throws away data we do not understand yet.
    let mut extra = root.clone();
    extra.remove("sections");
    extra.remove("block_entities");
    chunk.extra = extra;
    chunk.dirty = false;
    Ok(chunk)
}

fn palette_entry_to_state(entry: &NbtTag, blocks: &BlockRegistry, air: u32) -> u32 {
    let Some(compound) = entry.as_compound() else { return air };
    let name = compat::current_block_name(compound.get_str("Name").unwrap_or("minecraft:air"));
    let mut props = BTreeMap::new();
    if let Some(p) = compound.get_compound("Properties") {
        for (k, v) in p.iter() {
            if let Some(s) = v.as_str() {
                props.insert(k.to_owned(), s.to_owned());
            }
        }
    }
    match blocks.state_with(&name, &props) {
        Some(id) => id as u32,
        None => {
            compat::warn_unknown_once("block", &name);
            air
        }
    }
}

/// Unpacks palette indices stored `bits` wide, no straddling (1.16+ layout).
fn unpack(data: &[i64], count: usize, bits: usize) -> Vec<u32> {
    let per_long = 64 / bits;
    let mask = (1u64 << bits) - 1;
    (0..count)
        .map(|i| {
            let long = data.get(i / per_long).copied().unwrap_or(0) as u64;
            ((long >> ((i % per_long) * bits)) & mask) as u32
        })
        .collect()
}

fn pack(values: impl Iterator<Item = u32>, count: usize, bits: usize) -> Vec<i64> {
    let per_long = 64 / bits;
    let mut longs = vec![0i64; count.div_ceil(per_long)];
    for (i, v) in values.enumerate() {
        longs[i / per_long] |= (v as i64) << ((i % per_long) * bits);
    }
    longs
}

fn bits_for(count: usize) -> usize {
    let mut bits = 0;
    while (1usize << bits) < count {
        bits += 1;
    }
    bits
}

fn fill_section_blocks(section: &mut ChunkSection, palette: &[u32], data: &[i64]) {
    if palette.is_empty() {
        return;
    }
    if palette.len() == 1 || data.is_empty() {
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    section.set(x, y, z, palette[0]);
                }
            }
        }
        return;
    }
    let bits = bits_for(palette.len()).max(4);
    let indices = unpack(data, SECTION_VOLUME, bits);
    for (i, idx) in indices.into_iter().enumerate() {
        let state = palette.get(idx as usize).copied().unwrap_or(palette[0]);
        section.set(i & 15, i >> 8, (i >> 4) & 15, state);
    }
}

fn fill_section_biomes(section: &mut ChunkSection, palette: &[u32], data: &[i64]) {
    if palette.is_empty() {
        return;
    }
    if palette.len() == 1 || data.is_empty() {
        section.biomes = [palette[0]; BIOME_VOLUME];
        return;
    }
    let bits = bits_for(palette.len()).max(1);
    for (i, idx) in unpack(data, BIOME_VOLUME, bits).into_iter().enumerate() {
        section.biomes[i] = palette.get(idx as usize).copied().unwrap_or(palette[0]);
    }
}

/// Serialises a chunk to vanilla NBT.
pub fn chunk_to_nbt(chunk: &mut Chunk, ctx: &AnvilContext) -> NbtCompound {
    chunk.compact();
    let mut root = chunk.extra.clone();
    let saved_version = root.get_i32("DataVersion").unwrap_or(0);
    root.put("DataVersion", saved_version.max(ctx.data_version));
    root.put("xPos", chunk.pos.x);
    root.put("zPos", chunk.pos.z);
    root.put("yPos", chunk.range.min_section());
    root.put("Status", "minecraft:full");
    root.put("isLightOn", chunk.light.is_some());

    let mut sections = Vec::with_capacity(chunk.sections.len());
    for (i, section) in chunk.sections.iter().enumerate() {
        let mut tag = NbtCompound::new();
        tag.put("Y", (chunk.range.min_section() + i as i32) as i8);

        // Block states: palette of {Name, Properties}.
        let mut palette = Vec::with_capacity(section.palette.len());
        for &state in &section.palette {
            let mut entry = NbtCompound::new();
            let name = ctx.blocks.block_of_state(state as i32).map(|b| b.name.as_str()).unwrap_or("minecraft:air");
            entry.put("Name", name);
            if let Some(s) = ctx.blocks.state(state as i32) {
                if !s.properties.is_empty() {
                    let mut props = NbtCompound::new();
                    for (k, v) in &s.properties {
                        props.put(k.clone(), v.as_str());
                    }
                    entry.put("Properties", props);
                }
            }
            palette.push(NbtTag::Compound(entry));
        }
        let mut block_states = NbtCompound::new();
        block_states.put("palette", palette);
        if section.palette.len() > 1 {
            let bits = bits_for(section.palette.len()).max(4);
            let data = pack(section.palette_indices().iter().map(|&i| i as u32), SECTION_VOLUME, bits);
            block_states.put("data", data);
        }
        tag.put("block_states", block_states);

        // Biomes: palette of names.
        let mut biome_palette: Vec<u32> = Vec::new();
        let mut biome_indices = [0u32; BIOME_VOLUME];
        for (i, &b) in section.biomes.iter().enumerate() {
            let idx = match biome_palette.iter().position(|&p| p == b) {
                Some(p) => p,
                None => {
                    biome_palette.push(b);
                    biome_palette.len() - 1
                }
            };
            biome_indices[i] = idx as u32;
        }
        let biome_reg = ctx.dynamic.get("worldgen/biome");
        let names: Vec<NbtTag> = biome_palette
            .iter()
            .map(|&b| {
                let name = biome_reg
                    .and_then(|r| r.entries.get(b as usize))
                    .map(|(id, _)| id.to_string())
                    .unwrap_or_else(|| "minecraft:plains".to_owned());
                NbtTag::String(name)
            })
            .collect();
        let mut biomes = NbtCompound::new();
        biomes.put("palette", names);
        if biome_palette.len() > 1 {
            let bits = bits_for(biome_palette.len()).max(1);
            biomes.put("data", pack(biome_indices.iter().copied(), BIOME_VOLUME, bits));
        }
        tag.put("biomes", biomes);
        if let Some(light) = &chunk.light {
            if let Some(sky) = &light.sky[i] {
                tag.put("SkyLight", NbtTag::ByteArray(sky.iter().map(|b| *b as i8).collect()));
            }
            if let Some(block) = &light.block[i] {
                tag.put("BlockLight", NbtTag::ByteArray(block.iter().map(|b| *b as i8).collect()));
            }
        }
        sections.push(NbtTag::Compound(tag));
    }
    root.put("sections", sections);
    root.put(
        "block_entities",
        chunk.block_entities.iter().cloned().map(NbtTag::Compound).collect::<Vec<_>>(),
    );
    root
}

// ---- level.dat ----

/// Reads `level.dat` (gzip NBT). Returns the `Data` compound.
pub fn read_level_dat(path: &Path) -> Result<NbtCompound> {
    let mut raw = Vec::new();
    GzDecoder::new(File::open(path)?).read_to_end(&mut raw)?;
    let (_, root, _) = NbtTag::read_named(&raw)?;
    let data = root
        .as_compound()
        .and_then(|r| r.get_compound("Data"))
        .cloned()
        .context("level.dat has no Data compound")?;
    Ok(data)
}

/// Writes `level.dat` atomically: to `level.dat.tmp` first, then renamed
/// over the old file, keeping `level.dat_old` as vanilla does.
pub fn write_level_dat(path: &Path, data: &NbtCompound) -> Result<()> {
    let mut root = NbtCompound::new();
    root.put("Data", data.clone());
    let mut raw = Vec::new();
    NbtTag::Compound(root).write_named("", &mut raw);
    let tmp = path.with_extension("dat.tmp");
    {
        let mut enc = GzEncoder::new(File::create(&tmp)?, Compression::default());
        enc.write_all(&raw)?;
        enc.finish()?.sync_all()?;
    }
    if path.exists() {
        let _ = std::fs::rename(path, path.with_extension("dat_old"));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        let values: Vec<u32> = (0..SECTION_VOLUME as u32).map(|i| i % 13).collect();
        let packed = pack(values.iter().copied(), SECTION_VOLUME, 4);
        assert_eq!(packed.len(), SECTION_VOLUME / 16);
        assert_eq!(unpack(&packed, SECTION_VOLUME, 4), values);
    }
}

/// All region files of a world behind one lock, so chunk I/O can happen on
/// worker threads without holding the world lock.
pub struct RegionStore {
    dir: PathBuf,
    open: Mutex<HashMap<(i32, i32), RegionFile>>,
}

impl RegionStore {
    pub fn new(region_dir: PathBuf) -> Self {
        Self {
            dir: region_dir,
            open: Mutex::new(HashMap::new()),
        }
    }

    fn with_region<T>(&self, chunk: ChunkPos, f: impl FnOnce(&mut RegionFile) -> Result<T>) -> Result<T> {
        let key = (chunk.x >> 5, chunk.z >> 5);
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if !open.contains_key(&key) {
            let path = RegionFile::path_for(&self.dir, chunk);
            open.insert(key, RegionFile::open(&path)?);
        }
        f(open.get_mut(&key).unwrap())
    }

    pub fn read(&self, chunk: ChunkPos) -> Result<Option<NbtCompound>> {
        self.with_region(chunk, |r| r.read_chunk_nbt(chunk))
    }

    pub fn write(&self, chunk: ChunkPos, root: &NbtCompound) -> Result<()> {
        self.with_region(chunk, |r| r.write_chunk_nbt(chunk, root))
    }

    /// Closes every open file (an idle server should hold nothing open).
    pub fn close_all(&self) {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}
