//! The `World`: an in-memory chunk cache in front of a terrain generator and
//! an Anvil save directory.

use crate::anvil::{self, AnvilContext, RegionStore};
use crate::chunk::Chunk;
use crate::encode::{encode_chunk, EncodeContext};
use crate::generator::{Biomes, Blocks, FlatGenerator, NoiseGenerator, WorldGenerator};
use crate::light;
use crate::HeightRange;
use anyhow::{Context, Result};
use garnet_data::GameData;
use garnet_protocol::nbt::NbtCompound;
use garnet_protocol::packets::play::clientbound::ChunkData;
use garnet_protocol::{BlockPos, ChunkPos};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Settings that define a world, stored in `level.dat`.
#[derive(Clone, Debug)]
pub struct WorldSettings {
    pub name: String,
    pub seed: i64,
    pub generator: GeneratorKind,
    pub spawn: BlockPos,
    /// Ticks since the world was created.
    pub age: i64,
    /// Time of day in ticks (0-24000).
    pub time_of_day: i64,
    pub daylight_cycle: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratorKind {
    Flat,
    Noise,
}

impl GeneratorKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "flat" | "superflat" => Some(Self::Flat),
            "noise" | "default" | "normal" => Some(Self::Noise),
            _ => None,
        }
    }
}

pub struct World {
    pub settings: WorldSettings,
    pub range: HeightRange,
    pub dimension: garnet_protocol::Identifier,
    data: Arc<GameData>,
    generator: Arc<dyn WorldGenerator>,
    chunks: HashMap<ChunkPos, Chunk>,
    /// When each chunk was last touched by a player, for unloading.
    last_used: HashMap<ChunkPos, Instant>,
    save_dir: PathBuf,
    /// Region files; shared so chunk I/O can run off the world lock.
    regions: Arc<RegionStore>,
    /// `level.dat` tags we do not interpret, preserved on save.
    level_extra: NbtCompound,
    pub air_state: u32,
    /// Chunks whose light must be recomputed before they are sent again.
    light_dirty: HashSet<ChunkPos>,
}

impl World {
    /// Opens (or creates) the world in `save_dir`.
    pub fn open(save_dir: &Path, data: Arc<GameData>, defaults: WorldSettings) -> Result<Self> {
        std::fs::create_dir_all(save_dir.join("region"))?;
        let blocks = Blocks::resolve(&data.blocks);
        let biomes = Biomes::resolve(&data.dynamic);

        let level_path = save_dir.join("level.dat");
        let (settings, level_extra) = if level_path.exists() {
            let tag = anvil::read_level_dat(&level_path).with_context(|| format!("reading {}", level_path.display()))?;
            (settings_from_level_dat(&tag, &defaults), tag)
        } else {
            (defaults, NbtCompound::new())
        };

        let generator: Arc<dyn WorldGenerator> = match settings.generator {
            GeneratorKind::Flat => Arc::new(FlatGenerator::classic(&blocks, biomes.plains)),
            GeneratorKind::Noise => Arc::new(NoiseGenerator::new(settings.seed, blocks, biomes)),
        };

        let mut world = Self {
            settings,
            range: HeightRange::OVERWORLD,
            dimension: garnet_protocol::Identifier::minecraft("overworld"),
            data,
            generator,
            chunks: HashMap::new(),
            last_used: HashMap::new(),
            save_dir: save_dir.to_owned(),
            regions: Arc::new(RegionStore::new(save_dir.join("region"))),
            level_extra,
            air_state: blocks.air,
            light_dirty: HashSet::new(),
        };
        if !level_path.exists() {
            world.pick_spawn();
            world.save_level_dat()?;
            tracing::info!("created world '{}' (seed {})", world.settings.name, world.settings.seed);
        } else {
            tracing::info!("loaded world '{}' (seed {})", world.settings.name, world.settings.seed);
        }
        Ok(world)
    }

    /// Chooses a spawn on solid ground near the origin.
    fn pick_spawn(&mut self) {
        let mut best = BlockPos::new(0, self.generator.surface_y(0, 0), 0);
        // Walk outwards a little to avoid spawning in the sea.
        for radius in (0..256).step_by(16) {
            let y = self.generator.surface_y(radius, 0);
            if y > crate::generator::SEA_LEVEL + 1 {
                best = BlockPos::new(radius, y, 0);
                break;
            }
        }
        self.settings.spawn = best;
    }

    pub fn data(&self) -> &Arc<GameData> {
        &self.data
    }

    fn anvil_ctx(&self) -> AnvilContext<'_> {
        AnvilContext {
            blocks: &self.data.blocks,
            dynamic: &self.data.dynamic,
            data_version: self.data.data_version,
        }
    }

    /// The region files, for loading and saving chunks on worker threads.
    pub fn regions(&self) -> Arc<RegionStore> {
        Arc::clone(&self.regions)
    }

    /// What a worker thread needs to turn chunk NBT into a [`Chunk`].
    pub fn anvil_context(&self) -> (Arc<GameData>, HeightRange) {
        (Arc::clone(&self.data), self.range)
    }

    /// Returns the chunk, loading it from disk or generating it if needed.
    pub fn chunk_mut(&mut self, pos: ChunkPos) -> Result<&mut Chunk> {
        self.last_used.insert(pos, Instant::now());
        if !self.chunks.contains_key(&pos) {
            let chunk = match self.regions.read(pos) {
                Ok(Some(nbt)) => anvil::chunk_from_nbt(&nbt, pos, self.range, &self.anvil_ctx())?,
                Ok(None) => self.generator.generate(pos, self.range),
                Err(err) => {
                    tracing::error!("chunk {:?} is corrupt ({err}); regenerating it", pos);
                    self.generator.generate(pos, self.range)
                }
            };
            let mut chunk = chunk;
            if !chunk.dirty {
                // Loaded from disk: clean. Generated: must be written out.
                chunk.dirty = chunk.extra.is_empty();
            }
            self.chunks.insert(pos, chunk);
            self.light_arrived(pos);
        }
        Ok(self.chunks.get_mut(&pos).unwrap())
    }

    // ---- light ----

    /// The four side neighbours of a chunk, when loaded.
    fn neighbours(&self, pos: ChunkPos) -> light::Neighbours<'_> {
        light::Neighbours {
            sides: [
                self.chunks.get(&ChunkPos::new(pos.x - 1, pos.z)),
                self.chunks.get(&ChunkPos::new(pos.x + 1, pos.z)),
                self.chunks.get(&ChunkPos::new(pos.x, pos.z - 1)),
                self.chunks.get(&ChunkPos::new(pos.x, pos.z + 1)),
            ],
        }
    }

    /// Computes light for a chunk that just arrived (unless it came with
    /// light from disk) and lets its neighbours pick up light from it.
    fn light_arrived(&mut self, pos: ChunkPos) {
        let has_light = self.chunks.get(&pos).map(|c| c.light.is_some()).unwrap_or(true);
        if !has_light {
            self.recompute_light(pos);
        }
        for side in [ChunkPos::new(pos.x - 1, pos.z), ChunkPos::new(pos.x + 1, pos.z), ChunkPos::new(pos.x, pos.z - 1), ChunkPos::new(pos.x, pos.z + 1)] {
            if self.chunks.contains_key(&side) {
                self.light_dirty.insert(side);
            }
        }
    }

    /// Recomputes one chunk's light; `true` when it changed.
    fn recompute_light(&mut self, pos: ChunkPos) -> bool {
        let Some(mut chunk) = self.chunks.remove(&pos) else { return false };
        let before = chunk.light.clone();
        light::compute(&mut chunk, &self.data.light, &self.neighbours(pos));
        let changed = before.as_ref() != chunk.light.as_ref();
        if changed {
            chunk.dirty = true;
        }
        self.chunks.insert(pos, chunk);
        changed
    }

    /// A block at `pos` changed in a way that affects light: this chunk,
    /// and any neighbour within reach of the change, need a recompute.
    fn light_touched(&mut self, pos: BlockPos) {
        let chunk = pos.chunk();
        self.light_dirty.insert(chunk);
        let lx = pos.x & 15;
        let lz = pos.z & 15;
        if lx < 15 {
            self.light_dirty.insert(ChunkPos::new(chunk.x - 1, chunk.z));
        }
        if lx > 0 {
            self.light_dirty.insert(ChunkPos::new(chunk.x + 1, chunk.z));
        }
        if lz < 15 {
            self.light_dirty.insert(ChunkPos::new(chunk.x, chunk.z - 1));
        }
        if lz > 0 {
            self.light_dirty.insert(ChunkPos::new(chunk.x, chunk.z + 1));
        }
    }

    /// Recomputes chunks marked dirty, a few per call so a burst of new
    /// chunks never stalls a tick; returns the ones whose light actually
    /// changed so the server can tell the players watching them.
    pub fn flush_light(&mut self) -> Vec<ChunkPos> {
        const PER_CALL: usize = 8;
        let mut changed = Vec::new();
        for _ in 0..PER_CALL {
            let Some(pos) = self.light_dirty.iter().next().copied() else { break };
            self.light_dirty.remove(&pos);
            if self.chunks.contains_key(&pos) && self.recompute_light(pos) {
                changed.push(pos);
            }
        }
        changed
    }

    /// The current light of a loaded chunk as a `light_update` packet.
    pub fn light_packet(&self, pos: ChunkPos) -> Option<garnet_protocol::packets::play::clientbound::LightUpdate> {
        let chunk = self.chunks.get(&pos)?;
        Some(garnet_protocol::packets::play::clientbound::LightUpdate {
            chunk_x: pos.x,
            chunk_z: pos.z,
            light: crate::encode::light_data(chunk),
        })
    }

    /// Makes sure a loaded chunk has light before it is encoded.
    fn ensure_light(&mut self, pos: ChunkPos) {
        let missing = self.chunks.get(&pos).map(|c| c.light.is_none()).unwrap_or(false);
        if missing || self.light_dirty.remove(&pos) {
            self.recompute_light(pos);
        }
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }

    /// The terrain generator, shareable so chunks can be generated on a
    /// worker thread without holding the world lock.
    pub fn generator(&self) -> Arc<dyn WorldGenerator> {
        Arc::clone(&self.generator)
    }

    /// Loads a chunk from disk into the cache if it exists there. Returns
    /// whether the chunk is now loaded. Does not generate.
    pub fn try_load_from_disk(&mut self, pos: ChunkPos) -> Result<bool> {
        if self.chunks.contains_key(&pos) {
            return Ok(true);
        }
        match self.regions.read(pos) {
            Ok(Some(nbt)) => {
                let chunk = anvil::chunk_from_nbt(&nbt, pos, self.range, &self.anvil_ctx())?;
                self.chunks.insert(pos, chunk);
                self.last_used.insert(pos, Instant::now());
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(err) => {
                tracing::error!("chunk {:?} is corrupt ({err}); it will be regenerated", pos);
                Ok(false)
            }
        }
    }

    /// Adds a freshly generated chunk (see [`World::generator`]). Generated
    /// terrain is saved like vanilla does, so a later change to the
    /// generator never alters chunks players have already seen.
    pub fn insert_chunk(&mut self, mut chunk: Chunk) {
        chunk.dirty = true;
        let pos = chunk.pos;
        self.last_used.insert(pos, Instant::now());
        self.chunks.entry(pos).or_insert(chunk);
        self.light_arrived(pos);
    }

    /// Adds a chunk read from disk (clean: nothing to save yet).
    pub fn insert_loaded_chunk(&mut self, chunk: Chunk) {
        let pos = chunk.pos;
        self.last_used.insert(pos, Instant::now());
        self.chunks.entry(pos).or_insert(chunk);
        self.light_arrived(pos);
    }

    pub fn touch(&mut self, pos: ChunkPos) {
        if self.chunks.contains_key(&pos) {
            self.last_used.insert(pos, Instant::now());
        }
    }

    pub fn is_loaded(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    pub fn loaded_chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Block state at a world position, loading the chunk if needed.
    pub fn get_block(&mut self, pos: BlockPos) -> Result<u32> {
        let chunk = self.chunk_mut(pos.chunk())?;
        Ok(chunk.get_block(pos.x, pos.y, pos.z).unwrap_or(0))
    }

    pub fn set_block(&mut self, pos: BlockPos, state: u32) -> Result<bool> {
        let table = Arc::clone(&self.data);
        let chunk = self.chunk_mut(pos.chunk())?;
        let before = chunk.get_block(pos.x, pos.y, pos.z).unwrap_or(0);
        let changed = chunk.set_block(pos.x, pos.y, pos.z, state);
        if changed {
            let light = &table.light;
            if light.opacity(before) != light.opacity(state) || light.emission(before) != light.emission(state) {
                self.light_touched(pos);
            }
        }
        Ok(changed)
    }

    /// Encodes a chunk for the network.
    pub fn chunk_packet(&mut self, pos: ChunkPos) -> Result<ChunkData> {
        let biome_count = self.data.dynamic.get("worldgen/biome").map(|r| r.entries.len()).unwrap_or(1);
        let data = Arc::clone(&self.data);
        self.chunk_mut(pos)?;
        self.ensure_light(pos);
        let chunk = self.chunk_mut(pos)?;
        Ok(encode_chunk(
            chunk,
            &EncodeContext {
                blocks: &data.blocks,
                biome_count,
            },
        ))
    }

    /// Writes every dirty chunk and `level.dat`. Returns how many chunks
    /// were written.
    pub fn save(&mut self) -> Result<usize> {
        let mut written = 0;
        let dirty: Vec<ChunkPos> = self.chunks.iter().filter(|(_, c)| c.dirty).map(|(p, _)| *p).collect();
        for pos in dirty {
            self.save_chunk(pos)?;
            written += 1;
        }
        self.save_level_dat()?;
        Ok(written)
    }

    fn save_chunk(&mut self, pos: ChunkPos) -> Result<()> {
        let data = Arc::clone(&self.data);
        let ctx = AnvilContext {
            blocks: &data.blocks,
            dynamic: &data.dynamic,
            data_version: data.data_version,
        };
        let Some(chunk) = self.chunks.get_mut(&pos) else { return Ok(()) };
        let nbt = anvil::chunk_to_nbt(chunk, &ctx);
        chunk.dirty = false;
        self.regions.write(pos, &nbt)?;
        Ok(())
    }

    /// Unloads chunks nobody has used for a while, saving dirty ones first.
    /// `keep` lists chunks players can currently see.
    pub fn unload_unused(&mut self, keep: &HashSet<ChunkPos>, idle: std::time::Duration) -> Result<usize> {
        let now = Instant::now();
        let stale: Vec<ChunkPos> = self
            .chunks
            .keys()
            .filter(|pos| !keep.contains(pos))
            .filter(|pos| self.last_used.get(pos).map(|t| now.duration_since(*t) > idle).unwrap_or(true))
            .copied()
            .collect();
        for pos in &stale {
            if self.chunks.get(pos).map(|c| c.dirty).unwrap_or(false) {
                self.save_chunk(*pos)?;
            }
            self.chunks.remove(pos);
            self.last_used.remove(pos);
        }
        if self.chunks.is_empty() {
            // Close region files too so an idle server holds nothing open.
            self.regions.close_all();
        }
        Ok(stale.len())
    }

    pub fn save_level_dat(&mut self) -> Result<()> {
        let mut tag = self.level_extra.clone();
        let s = &self.settings;
        let existing_version = tag.get_i32("DataVersion").unwrap_or(0);
        tag.put("DataVersion", existing_version.max(self.data.data_version));
        tag.put("LevelName", s.name.as_str());
        tag.put("SpawnX", s.spawn.x);
        tag.put("SpawnY", s.spawn.y);
        tag.put("SpawnZ", s.spawn.z);
        tag.put("Time", s.age);
        tag.put("DayTime", s.time_of_day);
        tag.put("LastPlayed", chrono_now_millis());
        tag.put("initialized", true);
        let mut version = NbtCompound::new();
        version.put("Id", self.data.data_version);
        version.put("Name", self.data.version.as_str());
        version.put("Series", "main");
        version.put("Snapshot", false);
        tag.put("Version", version);
        // Vanilla keeps the seed under WorldGenSettings; keep both in sync.
        let mut gen = tag.get_compound("WorldGenSettings").cloned().unwrap_or_default();
        gen.put("seed", s.seed);
        tag.put("WorldGenSettings", gen);
        let mut rules = tag.get_compound("GameRules").cloned().unwrap_or_default();
        rules.put("doDaylightCycle", if s.daylight_cycle { "true" } else { "false" });
        tag.put("GameRules", rules);
        // Our own settings live under a namespaced key so vanilla ignores them.
        let mut garnet = NbtCompound::new();
        garnet.put("generator", match s.generator {
            GeneratorKind::Flat => "flat",
            GeneratorKind::Noise => "noise",
        });
        tag.put("garnet", garnet);
        anvil::write_level_dat(&self.save_dir.join("level.dat"), &tag)
    }
}

fn settings_from_level_dat(tag: &NbtCompound, defaults: &WorldSettings) -> WorldSettings {
    let seed = tag
        .get_compound("WorldGenSettings")
        .and_then(|g| g.get_i64("seed"))
        .or_else(|| tag.get_i64("RandomSeed"))
        .unwrap_or(defaults.seed);
    let generator = tag
        .get_compound("garnet")
        .and_then(|m| m.get_str("generator"))
        .and_then(GeneratorKind::parse)
        .unwrap_or(defaults.generator);
    WorldSettings {
        name: tag.get_str("LevelName").unwrap_or(&defaults.name).to_owned(),
        seed,
        generator,
        spawn: BlockPos::new(
            tag.get_i32("SpawnX").unwrap_or(defaults.spawn.x),
            tag.get_i32("SpawnY").unwrap_or(defaults.spawn.y),
            tag.get_i32("SpawnZ").unwrap_or(defaults.spawn.z),
        ),
        age: tag.get_i64("Time").unwrap_or(0),
        time_of_day: tag.get_i64("DayTime").unwrap_or(0),
        daylight_cycle: tag
            .get_compound("GameRules")
            .and_then(|r| r.get_str("doDaylightCycle"))
            .map(|v| v != "false")
            .unwrap_or(defaults.daylight_cycle),
    }
}

fn chrono_now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
