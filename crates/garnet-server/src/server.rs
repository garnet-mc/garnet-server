//! The server: shared state plus the 20 Hz tick loop.
//!
//! Concurrency model, chosen so thousands of players stay comfortable:
//! - every connection runs in its own task and handles its own packets;
//! - there is no global lock, only short-lived locks per subsystem
//!   (players map, world, watchers, mods) that are never held across awaits;
//! - broadcasts are encoded once and the bytes shared;
//! - a player only receives packets about chunks they watch.

use crate::anticheat::AntiCheat;
use crate::audit::Audit;
use crate::commands::CommandRegistry;
use crate::config::GarnetConfig;
use crate::lists::Lists;
use crate::logging::LogSink;
use crate::player::Player;
use anyhow::Result;
use bytes::Bytes;
use garnet_admin::Panel;
use garnet_api::{Action, Permissions};
use garnet_data::GameData;
use garnet_mods::ModRuntime;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{ChunkPos, ClientboundPacket, Text};
use garnet_voice::VoiceServer;
use garnet_world::World;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use uuid::Uuid;

/// RSA key pair used for the login encryption handshake.
pub struct EncryptionKeys {
    pub private: rsa::RsaPrivateKey,
    /// DER-encoded SubjectPublicKeyInfo, what the client expects.
    pub public_der: Vec<u8>,
}

/// A timer a mod asked for.
pub struct ScheduledTask {
    pub mod_id: String,
    pub id: String,
    pub due_tick: u64,
    pub repeat_every: Option<u64>,
}

pub struct Server {
    pub config: RwLock<GarnetConfig>,
    pub config_path: PathBuf,
    pub root: PathBuf,
    pub data: Arc<GameData>,
    pub world: Mutex<World>,
    pub players: RwLock<HashMap<Uuid, Arc<Player>>>,
    /// Which players currently have each chunk loaded.
    pub chunk_watchers: Mutex<HashMap<ChunkPos, HashSet<Uuid>>>,
    /// Which chunk each player stands in, for finding neighbours cheaply.
    pub players_by_chunk: Mutex<HashMap<ChunkPos, HashSet<Uuid>>>,
    /// Encoded chunk packets, shared between every player that needs them.
    pub chunk_cache: Mutex<HashMap<ChunkPos, Bytes>>,
    /// Chunks currently being generated on the blocking pool.
    pub generating: Mutex<HashSet<ChunkPos>>,
    pub lists: Lists,
    pub permissions: RwLock<Permissions>,
    pub audit: Audit,
    pub anticheat: AntiCheat,
    pub mods: Mutex<ModRuntime>,
    /// Actions mods asked for; applied on the next tick, outside the mod lock.
    pub mod_actions: Mutex<Vec<(String, Action)>>,
    pub scheduled: Mutex<Vec<ScheduledTask>>,
    pub commands: CommandRegistry,
    /// Game rules, difficulty, weather, border, tick rate.
    pub rules: RwLock<crate::rules::WorldRules>,
    /// Scoreboard, teams and boss bars.
    pub boards: Mutex<crate::boards::Boards>,
    /// Block drop tables.
    pub loot: crate::loot::LootTables,
    /// What the crafting grid can make.
    pub recipes: crate::recipes::Recipes,
    /// What an enchanting table may offer, and on what.
    pub enchantments: crate::enchanting::Enchantments,
    /// What mobs leave behind.
    pub mob_loot: crate::loot::LootTables,
    /// The furnaces that are currently burning.
    pub furnaces: crate::furnaces::Furnaces,
    /// What a brewing stand can make of what is in it.
    pub brewing: crate::brewing::Brewing,
    /// The brewing stands that are currently working.
    pub stands: crate::brewing::Stands,
    /// Blocks waiting for their turn: a button to pop out, sand to fall.
    pub block_ticks: Mutex<Vec<(garnet_protocol::BlockPos, u64)>>,
    /// Lit fuses, and the tick each one runs out.
    pub fuses: Mutex<Vec<(garnet_protocol::BlockPos, u64)>>,
    /// Blocks that changed this tick, for the observers watching them.
    pub changed_blocks: Mutex<Vec<garnet_protocol::BlockPos>>,
    /// What the tick is doing right now, as a pointer to a static string.
    pub stage: AtomicUsize,
    pub stage_len: AtomicUsize,
    /// Items on the ground, mobs and other non-player entities.
    pub entities: Mutex<crate::world_entities::Entities>,
    pub voice: Option<VoiceServer>,
    pub panel: Mutex<Option<Panel>>,
    pub logs: LogSink,
    pub keys: EncryptionKeys,
    pub started: Instant,
    pub tick: AtomicU64,
    pub tick_times: Mutex<VecDeque<f32>>,
    pub next_entity_id: AtomicI32,
    pub shutdown: watch::Sender<bool>,
    pub stopping: AtomicBool,
    /// Whether any data pack is enabled (skips the per-tick function tag otherwise).
    pub datapacks_present: AtomicBool,
    pub http: reqwest::Client,
}

impl Server {
    pub fn config(&self) -> std::sync::RwLockReadGuard<'_, GarnetConfig> {
        self.config.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The world, locked. The guard remembers that this thread holds it,
    /// so a write attempted in the middle of a read is caught instead of
    /// quietly waiting on itself.
    pub fn world(&self) -> WorldGuard<'_> {
        // Taking it twice on one thread waits on this thread's own lock:
        // say which caller did it rather than freezing the server.
        debug_assert!(!Self::reading_world(), "the world is already locked by this thread");
        if Self::reading_world() {
            tracing::error!(
                "the world was locked twice by one thread: read what you need, drop the guard, then write"
            );
        }
        let inner = self.world.lock().unwrap_or_else(|e| e.into_inner());
        WORLD_DEPTH.with(|depth| depth.set(depth.get() + 1));
        WorldGuard { inner }
    }

    /// Whether this thread already has the world open.
    fn reading_world() -> bool {
        WORLD_DEPTH.with(|depth| depth.get() > 0)
    }

    pub fn current_tick(&self) -> u64 {
        self.tick.load(Ordering::Relaxed)
    }

    pub fn allocate_entity_id(&self) -> i32 {
        self.next_entity_id.fetch_add(1, Ordering::Relaxed)
    }

    // ---- players ----

    pub fn player(&self, uuid: Uuid) -> Option<Arc<Player>> {
        self.players.read().unwrap_or_else(|e| e.into_inner()).get(&uuid).cloned()
    }

    pub fn player_by_name(&self, name: &str) -> Option<Arc<Player>> {
        self.players
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|p| p.name().eq_ignore_ascii_case(name))
            .cloned()
    }

    pub fn online_players(&self) -> Vec<Arc<Player>> {
        self.players.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    pub fn online_count(&self) -> usize {
        self.players.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn add_player(&self, player: Arc<Player>) {
        let chunk = player.lock().chunk();
        self.players.write().unwrap_or_else(|e| e.into_inner()).insert(player.uuid, Arc::clone(&player));
        self.players_by_chunk
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(chunk)
            .or_default()
            .insert(player.uuid);
    }

    pub fn remove_player(&self, uuid: Uuid) -> Option<Arc<Player>> {
        let player = self.players.write().unwrap_or_else(|e| e.into_inner()).remove(&uuid)?;
        let (chunk, loaded) = {
            let state = player.lock();
            (state.chunk(), state.loaded_chunks.iter().copied().collect::<Vec<_>>())
        };
        let mut by_chunk = self.players_by_chunk.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = by_chunk.get_mut(&chunk) {
            set.remove(&uuid);
            if set.is_empty() {
                by_chunk.remove(&chunk);
            }
        }
        drop(by_chunk);
        let mut watchers = self.chunk_watchers.lock().unwrap_or_else(|e| e.into_inner());
        for pos in loaded {
            if let Some(set) = watchers.get_mut(&pos) {
                set.remove(&uuid);
                if set.is_empty() {
                    watchers.remove(&pos);
                }
            }
        }
        Some(player)
    }

    /// Call when a player crosses into another chunk.
    pub fn move_player_chunk(&self, uuid: Uuid, from: ChunkPos, to: ChunkPos) {
        let mut by_chunk = self.players_by_chunk.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = by_chunk.get_mut(&from) {
            set.remove(&uuid);
            if set.is_empty() {
                by_chunk.remove(&from);
            }
        }
        by_chunk.entry(to).or_default().insert(uuid);
    }

    // ---- sending ----

    pub fn encode<P: ClientboundPacket>(&self, packet: &P) -> Option<Bytes> {
        match self.data.packet_ids.encode(packet) {
            Ok(b) => Some(Bytes::from(b)),
            Err(err) => {
                tracing::error!("cannot encode {}: {err}", P::NAME);
                None
            }
        }
    }

    /// Sends to every online player.
    pub fn broadcast<P: ClientboundPacket>(&self, packet: &P) {
        let Some(bytes) = self.encode(packet) else { return };
        for player in self.online_players() {
            player.send_raw(bytes.clone());
        }
    }

    pub fn broadcast_except<P: ClientboundPacket>(&self, packet: &P, except: Uuid) {
        let Some(bytes) = self.encode(packet) else { return };
        for player in self.online_players() {
            if player.uuid != except {
                player.send_raw(bytes.clone());
            }
        }
    }

    /// Sends to everyone who has `chunk` loaded, except `except`.
    pub fn broadcast_near<P: ClientboundPacket>(&self, chunk: ChunkPos, packet: &P, except: Option<Uuid>) {
        let Some(bytes) = self.encode(packet) else { return };
        let targets: Vec<Uuid> = self
            .chunk_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&chunk)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        if targets.is_empty() {
            return;
        }
        let players = self.players.read().unwrap_or_else(|e| e.into_inner());
        for uuid in targets {
            if Some(uuid) == except {
                continue;
            }
            if let Some(p) = players.get(&uuid) {
                p.send_raw(bytes.clone());
            }
        }
    }

    pub fn broadcast_chat(&self, text: Text) {
        tracing::info!(target: "chat", "{}", text.to_plain());
        self.broadcast(&cb::SystemChat {
            content: text,
            overlay: false,
        });
    }

    // ---- world ----

    /// Encoded chunk packet, from the cache or freshly encoded. `None` if the
    /// chunk is not loaded (the tick loop then arranges generation).
    pub fn chunk_packet(&self, pos: ChunkPos) -> Option<Bytes> {
        if let Some(bytes) = self.chunk_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&pos) {
            return Some(bytes.clone());
        }
        let mut world = self.world();
        if !world.is_loaded(pos) {
            return None;
        }
        let packet = world.chunk_packet(pos).ok()?;
        drop(world);
        let bytes = self.encode(&packet)?;
        self.chunk_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(pos, bytes.clone());
        Some(bytes)
    }

    pub fn invalidate_chunk(&self, pos: ChunkPos) {
        self.chunk_cache.lock().unwrap_or_else(|e| e.into_inner()).remove(&pos);
    }

    /// After many blocks changed at once: drop the cached packet and make
    /// every watcher fetch the chunk again on the next tick.
    pub fn refresh_chunk(&self, pos: ChunkPos) {
        self.invalidate_chunk(pos);
        let watchers: Vec<Uuid> = self
            .chunk_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pos)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        for uuid in watchers {
            if let Some(player) = self.player(uuid) {
                player.lock().loaded_chunks.remove(&pos);
            }
        }
    }

    /// Re-reads the ban, op and whitelist files and permissions from disk.
    pub fn reload_lists(&self) -> anyhow::Result<()> {
        let lists = crate::lists::Lists::load(&self.root)?;
        self.lists.replace(lists);
        let path = self.root.join("permissions.toml");
        let text = std::fs::read_to_string(&path)?;
        let permissions: garnet_api::Permissions = toml::from_str(&text).map_err(|e| anyhow::anyhow!("{} is invalid: {e}", path.display()))?;
        *self.permissions.write().unwrap_or_else(|e| e.into_inner()) = permissions;
        Ok(())
    }

    /// Makes sure a chunk is loaded or on its way. Disk reads and terrain
    /// generation both run on worker threads; the chunk appears a tick later.
    pub fn request_chunk(self: &Arc<Self>, pos: ChunkPos) {
        {
            let mut world = self.world();
            if world.is_loaded(pos) {
                world.touch(pos);
                return;
            }
        }
        {
            let mut generating = self.generating.lock().unwrap_or_else(|e| e.into_inner());
            if !generating.insert(pos) {
                return;
            }
        }
        let server = Arc::clone(self);
        let (regions, generator, (data, range)) = {
            let world = self.world();
            (world.regions(), world.generator(), world.anvil_context())
        };
        tokio::task::spawn_blocking(move || {
            let from_disk = match regions.read(pos) {
                Ok(Some(nbt)) => {
                    let ctx = garnet_world::anvil::AnvilContext {
                        blocks: &data.blocks,
                        dynamic: &data.dynamic,
                        data_version: data.data_version,
                    };
                    match garnet_world::anvil::chunk_from_nbt(&nbt, pos, range, &ctx) {
                        Ok(chunk) => Some(chunk),
                        Err(err) => {
                            tracing::error!("chunk {:?} is corrupt ({err}); regenerating it", pos);
                            None
                        }
                    }
                }
                Ok(None) => None,
                Err(err) => {
                    tracing::error!("reading chunk {:?}: {err}", pos);
                    None
                }
            };
            match from_disk {
                Some(chunk) => server.world().insert_loaded_chunk(chunk),
                None => server.world().insert_chunk(generator.generate(pos, range)),
            }
            crate::world_entities::load_chunk(&server, pos);
            crate::furnaces::load_chunk(&server, pos);
            crate::brewing::load_chunk(&server, pos);
            crate::hoppers::load_chunk(&server, pos);
            server.generating.lock().unwrap_or_else(|e| e.into_inner()).remove(&pos);
        });
    }

    /// Asks for `blocks::scheduled` to run on this block in a while.
    pub fn schedule_block(&self, pos: garnet_protocol::BlockPos, delay: u64) {
        let when = self.current_tick() + delay.max(1);
        let mut ticks = self.block_ticks.lock().unwrap_or_else(|e| e.into_inner());
        if !ticks.iter().any(|(p, _)| *p == pos) {
            ticks.push((pos, when));
        }
    }

    pub fn set_block(&self, pos: garnet_protocol::BlockPos, state: u32) -> bool {
        // Writing while still holding a read would wait on this thread's
        // own lock forever. Say which block it was rather than hanging.
        debug_assert!(
            !Self::reading_world(),
            "set_block while this thread still holds the world"
        );
        if Self::reading_world() {
            tracing::error!(
                "set_block at {},{},{} while this thread still holds the world: read first, then write",
                pos.x,
                pos.y,
                pos.z
            );
        }
        let changed = self.world().set_block(pos, state).unwrap_or(false);
        if changed {
            // This block, and the one above it, may now have nothing
            // holding them up; the fluids beside it may have somewhere new
            // to go.
            self.schedule_block(pos, 2);
            self.schedule_block(pos.offset(0, 1, 0), 2);
            self.changed_blocks.lock().unwrap_or_else(|e| e.into_inner()).push(pos);
            for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, 1, 0), (0, -1, 0)] {
                self.schedule_block(pos.offset(dx, dy, dz), 5);
            }
            self.invalidate_chunk(pos.chunk());
            self.broadcast_near(
                pos.chunk(),
                &cb::BlockUpdate {
                    position: pos,
                    state_id: state as i32,
                },
                None,
            );
        }
        changed
    }

    pub fn default_game_mode(&self) -> GameMode {
        let from_rules = self.rules.read().unwrap_or_else(|e| e.into_inner()).default_game_mode.clone();
        from_rules
            .and_then(|m| GameMode::parse(&m))
            .or_else(|| GameMode::parse(&self.config().world.gamemode))
            .unwrap_or(GameMode::Survival)
    }

    pub fn is_op(&self, uuid: Uuid) -> bool {
        self.lists.is_op(uuid)
    }

    pub fn has_permission(&self, uuid: Uuid, permission: &str) -> bool {
        let perms = self.permissions.read().unwrap_or_else(|e| e.into_inner());
        perms.check(&uuid.to_string(), self.is_op(uuid), permission)
    }

    pub fn tps(&self) -> (f32, f32) {
        let times = self.tick_times.lock().unwrap_or_else(|e| e.into_inner());
        if times.is_empty() {
            return (20.0, 0.0);
        }
        let avg_ms = times.iter().sum::<f32>() / times.len() as f32;
        let tps = (1000.0 / avg_ms.max(50.0)).min(20.0);
        (tps, avg_ms)
    }

    pub fn request_stop(&self) {
        if !self.stopping.swap(true, Ordering::SeqCst) {
            let _ = self.shutdown.send(true);
        }
    }

    // ---- the tick loop ----

    /// Notes what the tick is busy with, so a stall can say where it
    /// stopped rather than only that it did.
    pub fn doing(&self, what: &'static str) {
        self.stage.store(what.as_ptr() as usize, Ordering::Relaxed);
        self.stage_len.store(what.len(), Ordering::Relaxed);
    }

    fn stage_name(&self) -> String {
        let ptr = self.stage.load(Ordering::Relaxed) as *const u8;
        let len = self.stage_len.load(Ordering::Relaxed);
        if ptr.is_null() || len == 0 {
            return "starting up".to_owned();
        }
        // The pointer is always to a 'static string this binary owns.
        unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len)) }.to_owned()
    }

    /// Watches the tick counter from outside the tick loop: if the server
    /// stops ticking, something is stuck, and saying so beats going quiet.
    pub fn watch_ticks(self: &Arc<Self>) {
        let server = Arc::clone(self);
        std::thread::Builder::new()
            .name("garnet-watchdog".to_owned())
            .spawn(move || {
                let mut last = server.current_tick();
                let mut stalled = 0u32;
                loop {
                    std::thread::sleep(Duration::from_secs(5));
                    let now = server.current_tick();
                    if now == last {
                        stalled += 5;
                        if stalled % 15 == 0 {
                            tracing::error!(
                                "the server has not ticked for {stalled}s (stuck at tick {now} during {});                                  players will see the world frozen",
                                server.stage_name()
                            );
                        }
                    } else {
                        stalled = 0;
                        last = now;
                    }
                }
            })
            .ok();
    }

    pub async fn run_ticks(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut shutdown = self.shutdown.subscribe();
        let mut last_autosave = Instant::now();
        let mut last_unload = Instant::now();
        let mut last_backup = Instant::now();
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.changed() => break,
            }
            let started = Instant::now();
            let tick = self.tick.fetch_add(1, Ordering::Relaxed) + 1;

            if !self.tick_world_paused() {
                self.tick_time(tick);
                self.tick_weather();
            }
            self.tick_players(tick);
            self.doing("light");
            self.tick_light();
            crate::vanilla_commands::tick_effects(&self, tick);
            crate::functions::tick(&self, tick);
            self.doing("entities");
            crate::world_entities::tick(&self);
            self.doing("survival");
            crate::survival::tick(&self, tick);
            self.doing("mobs");
            crate::mobs::tick(&self, tick);
            self.doing("furnaces");
            crate::furnaces::tick(&self);
            self.doing("brewing");
            crate::brewing::tick(&self);
            self.doing("blocks");
            crate::blocks::tick(&self, tick);
            self.doing("explosions");
            crate::explosions::tick(&self);
            self.doing("experience");
            crate::experience::tick(&self);
            self.apply_mod_actions();
            self.tick_mods(tick);

            let config = self.config();
            let autosave = Duration::from_secs(config.world.autosave_minutes.max(1) * 60);
            let unload_after = Duration::from_secs(config.world.chunk_unload_seconds.max(5));
            let backups_enabled = config.backups.enabled;
            let backup_every = Duration::from_secs(config.backups.interval_hours.max(1) * 3600);
            drop(config);

            if last_autosave.elapsed() >= autosave {
                last_autosave = Instant::now();
                if self.rules.read().unwrap_or_else(|e| e.into_inner()).autosave {
                    self.save_everything("autosave");
                }
            }
            if last_unload.elapsed() >= Duration::from_secs(15) {
                last_unload = Instant::now();
                self.unload_unwatched_chunks(unload_after);
            }
            if backups_enabled && last_backup.elapsed() >= backup_every {
                last_backup = Instant::now();
                let server = Arc::clone(&self);
                tokio::task::spawn_blocking(move || {
                    if let Err(err) = crate::backup::create(&server, "scheduled") {
                        tracing::error!("scheduled backup failed: {err:#}");
                    }
                });
            }

            let elapsed = started.elapsed().as_secs_f32() * 1000.0;
            let mut times = self.tick_times.lock().unwrap_or_else(|e| e.into_inner());
            if times.len() >= 100 {
                times.pop_front();
            }
            times.push_back(elapsed);
            if elapsed > 100.0 {
                tracing::warn!("tick {tick} took {elapsed:.0} ms");
            }
        }
    }

    /// Recomputes light where blocks changed and sends the new light to
    /// everyone who has those chunks.
    fn tick_light(&self) {
        let changed = self.world().flush_light();
        for pos in changed {
            self.invalidate_chunk(pos);
            let packet = self.world().light_packet(pos);
            if let Some(packet) = packet {
                self.broadcast_near(pos, &packet, None);
            }
        }
    }

    /// `/tick freeze` stops the world clock and weather; `/tick step` lets
    /// a few ticks through.
    fn tick_world_paused(&self) -> bool {
        let mut rules = self.rules.write().unwrap_or_else(|e| e.into_inner());
        if !rules.tick.frozen {
            return false;
        }
        if rules.tick.steps_left > 0 {
            rules.tick.steps_left -= 1;
            return false;
        }
        true
    }

    /// Weather runs out on its own like vanilla: rain for a while, then
    /// clear for longer.
    fn tick_weather(self: &Arc<Self>) {
        let now_raining = {
            let mut rules = self.rules.write().unwrap_or_else(|e| e.into_inner());
            if !rules.game_rule_bool("doWeatherCycle") {
                return;
            }
            if rules.weather.ticks_left <= 0 {
                // Start a timer for the current weather.
                rules.weather.ticks_left = if rules.weather.raining {
                    rand::random_range(12_000..24_000)
                } else {
                    rand::random_range(12_000..180_000)
                };
                return;
            }
            rules.weather.ticks_left -= 1;
            if rules.weather.ticks_left > 0 {
                return;
            }
            rules.weather.raining
        };
        let thunder = !now_raining && rand::random::<f32>() < 0.25;
        crate::vanilla_commands::set_weather(self, !now_raining, thunder, 0);
    }

    fn tick_time(&self, tick: u64) {
        let daylight = self.config().world.daylight_cycle;
        let (age, time_of_day) = {
            let mut world = self.world();
            world.settings.age += 1;
            if daylight {
                world.settings.time_of_day = (world.settings.time_of_day + 1) % 24000;
            }
            (world.settings.age, world.settings.time_of_day)
        };
        if tick % 20 == 0 {
            self.broadcast(&cb::SetTime {
                world_age: age,
                time_of_day,
                advancing: daylight,
            });
        }
    }

    fn tick_players(self: &Arc<Self>, tick: u64) {
        let now = Instant::now();
        let (server_view, afk_minutes) = {
            let c = self.config();
            (c.server.view_distance, c.server.afk_kick_minutes)
        };
        for player in self.online_players() {
            // Keep-alives: one every ten seconds, thirty seconds to answer.
            {
                let mut state = player.lock();
                if state.awaiting_keepalive && now.duration_since(state.keepalive_sent) > Duration::from_secs(30) {
                    drop(state);
                    player.disconnect(Text::translate("disconnect.timeout", vec![]));
                    continue;
                }
                if !state.awaiting_keepalive && now.duration_since(state.keepalive_sent) > Duration::from_secs(10) {
                    state.keepalive_sent = now;
                    state.keepalive_id = now.elapsed().as_millis() as i64 ^ (tick as i64);
                    state.awaiting_keepalive = true;
                    let id = state.keepalive_id;
                    drop(state);
                    player.send(&cb::KeepAlive { id });
                }
            }
            if afk_minutes > 0 {
                let idle = now.duration_since(player.lock().last_activity);
                if idle > Duration::from_secs(afk_minutes * 60) {
                    player.disconnect(Text::new("You were kicked for being idle."));
                    continue;
                }
            }
            crate::chunks::stream_chunks(self, &player, server_view);
            if tick % 10 == 0 {
                crate::entities::update_visibility(self, &player);
                crate::world_entities::update_visibility(self, &player);
            }
            if let Some(voice) = &self.voice {
                if tick % 4 == 0 {
                    let state = player.lock();
                    voice.update_position(
                        player.uuid,
                        garnet_voice::Position {
                            dimension: 0,
                            x: state.x as f32,
                            y: state.y as f32,
                            z: state.z as f32,
                        },
                    );
                }
            }
        }
    }

    fn tick_mods(self: &Arc<Self>, tick: u64) {
        // Timers first, so a mod's timer fires before it sees the tick.
        let due: Vec<(String, String)> = {
            let mut scheduled = self.scheduled.lock().unwrap_or_else(|e| e.into_inner());
            let mut fired = Vec::new();
            scheduled.retain_mut(|task| {
                if task.due_tick > tick {
                    return true;
                }
                fired.push((task.mod_id.clone(), task.id.clone()));
                match task.repeat_every {
                    Some(every) => {
                        task.due_tick = tick + every;
                        true
                    }
                    None => false,
                }
            });
            fired
        };
        let mut mods = self.mods.lock().unwrap_or_else(|e| e.into_inner());
        for (_, id) in due {
            mods.dispatch(&garnet_api::Event::TimerFired { id });
        }
        mods.dispatch(&garnet_api::Event::Tick { tick });
    }

    /// Runs the actions mods queued since last tick.
    fn apply_mod_actions(self: &Arc<Self>) {
        let queued: Vec<(String, Action)> = std::mem::take(&mut *self.mod_actions.lock().unwrap_or_else(|e| e.into_inner()));
        for (mod_id, action) in queued {
            crate::mod_host::apply_action(self, &mod_id, action);
        }
    }

    pub fn save_everything(self: &Arc<Self>, why: &str) {
        let started = Instant::now();
        let chunks = match self.world().save() {
            Ok(n) => n,
            Err(err) => {
                tracing::error!("world save failed: {err:#}");
                0
            }
        };
        for player in self.online_players() {
            crate::playerdata::save(self, &player);
        }
        crate::world_entities::save_all(self);
        crate::furnaces::save_all(self);
        crate::brewing::save_all(self);
        tracing::info!("{why}: saved {chunks} chunks in {} ms", started.elapsed().as_millis());
    }

    fn unload_unwatched_chunks(self: &Arc<Self>, idle: Duration) {
        let keep: HashSet<ChunkPos> = self
            .chunk_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect();
        let unloaded = match self.world().unload_unused(&keep, idle) {
            Ok(n) => n,
            Err(err) => {
                tracing::error!("unloading chunks: {err:#}");
                return;
            }
        };
        if unloaded > 0 {
            let mut cache = self.chunk_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.retain(|pos, _| keep.contains(pos));
            tracing::debug!("unloaded {unloaded} idle chunks");
        }
        crate::world_entities::unload_unwatched(self);
    }

    pub fn memory_mb() -> u64 {
        // Resident set size where the platform makes it cheap to read.
        #[cfg(target_os = "linux")]
        {
            if let Ok(text) = std::fs::read_to_string("/proc/self/statm") {
                if let Some(pages) = text.split_whitespace().nth(1).and_then(|p| p.parse::<u64>().ok()) {
                    return pages * 4096 / 1024 / 1024;
                }
            }
        }
        0
    }
}

pub fn generate_keys() -> Result<EncryptionKeys> {
    use rsa::pkcs8::EncodePublicKey;
    let mut rng = rand_core::OsRng;
    let private = rsa::RsaPrivateKey::new(&mut rng, 1024)?;
    let public_der = private.to_public_key().to_public_key_der()?.as_bytes().to_vec();
    Ok(EncryptionKeys { private, public_der })
}


thread_local! {
    /// How many world guards this thread is holding.
    static WORLD_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// A lock on the world that counts itself, so re-entrant writes are caught.
pub struct WorldGuard<'a> {
    inner: std::sync::MutexGuard<'a, World>,
}

impl std::ops::Deref for WorldGuard<'_> {
    type Target = World;

    fn deref(&self) -> &World {
        &self.inner
    }
}

impl std::ops::DerefMut for WorldGuard<'_> {
    fn deref_mut(&mut self) -> &mut World {
        &mut self.inner
    }
}

impl Drop for WorldGuard<'_> {
    fn drop(&mut self) {
        WORLD_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}
