//! Entities that are not players: dropped items, summoned mobs and the
//! rest. They have a position, fall with gravity and land on blocks, can be
//! ridden, and are saved per chunk in `world/entities/` region files with
//! vanilla's layout. What mobs do with themselves lives in `mobs`.
//!
//! Item entities drift a little when thrown, settle on the ground, and
//! jump into the inventory of a player who walks up to them.

use crate::items::{item_id, item_name, max_stack};
use crate::player::Player;
use crate::server::Server;
use garnet_protocol::nbt::{NbtCompound, NbtTag};
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::ItemStack;
use garnet_protocol::{BlockPos, ChunkPos, PacketWriter};
use garnet_world::anvil::RegionStore;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Entity {
    pub id: i32,
    pub uuid: Uuid,
    /// Full name such as `minecraft:zombie`.
    pub kind: String,
    pub type_id: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub velocity: (f64, f64, f64),
    pub on_ground: bool,
    pub item: Option<ItemStack>,
    pub custom_name: Option<String>,
    pub health: f32,
    pub passengers: Vec<i32>,
    pub vehicle: Option<i32>,
    /// Ticks before a dropped item can be picked up.
    pub pickup_delay: u32,
    pub age: u32,
    pub no_gravity: bool,
    /// What an experience orb is worth.
    pub xp: i32,
    /// Ticks until this mob may swing again.
    pub attack_cooldown: u32,
    /// Set when the last move ran into something, so mobs know to hop.
    pub blocked_ahead: bool,
    /// Summoned or named mobs stay put instead of despawning.
    pub persistent: bool,
}

impl Entity {
    pub fn chunk(&self) -> ChunkPos {
        ChunkPos::new((self.x.floor() as i32) >> 4, (self.z.floor() as i32) >> 4)
    }

    pub fn is_item(&self) -> bool {
        self.kind == "minecraft:item"
    }

    /// Whether the client expects living-entity metadata (health and so on).
    pub fn is_living(&self) -> bool {
        !NOT_LIVING.iter().any(|n| self.kind.trim_start_matches("minecraft:") == *n)
            && !self.kind.ends_with("_boat")
            && !self.kind.ends_with("_raft")
            && !self.kind.ends_with("_minecart")
            && !self.kind.ends_with("_display")
    }
}

const NOT_LIVING: &[&str] = &[
    "item", "experience_orb", "arrow", "spectral_arrow", "trident", "snowball", "egg", "ender_pearl", "experience_bottle", "potion",
    "splash_potion", "lingering_potion", "fireball", "small_fireball", "dragon_fireball", "wither_skull", "shulker_bullet", "llama_spit",
    "fishing_bobber", "firework_rocket", "eye_of_ender", "painting", "item_frame", "glow_item_frame", "leash_knot", "boat", "minecart",
    "tnt", "falling_block", "area_effect_cloud", "evoker_fangs", "end_crystal", "lightning_bolt", "marker", "interaction", "block_display",
    "item_display", "text_display", "wind_charge", "breeze_wind_charge", "ominous_item_spawner",
];

pub struct Entities {
    pub by_id: HashMap<i32, Entity>,
    pub by_chunk: HashMap<ChunkPos, HashSet<i32>>,
    /// Chunks whose entities changed since the last save.
    pub dirty: HashSet<ChunkPos>,
    regions: RegionStore,
}

impl Entities {
    pub fn new(world_dir: &std::path::Path) -> Self {
        let dir = world_dir.join("entities");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            by_id: HashMap::new(),
            by_chunk: HashMap::new(),
            dirty: HashSet::new(),
            regions: RegionStore::new(dir),
        }
    }

    fn index(&mut self, id: i32, chunk: ChunkPos) {
        self.by_chunk.entry(chunk).or_default().insert(id);
        self.dirty.insert(chunk);
    }

    fn unindex(&mut self, id: i32, chunk: ChunkPos) {
        if let Some(set) = self.by_chunk.get_mut(&chunk) {
            set.remove(&id);
            if set.is_empty() {
                self.by_chunk.remove(&chunk);
            }
        }
        self.dirty.insert(chunk);
    }

    pub fn insert(&mut self, entity: Entity) {
        let chunk = entity.chunk();
        self.index(entity.id, chunk);
        self.by_id.insert(entity.id, entity);
    }

    pub fn remove(&mut self, id: i32) -> Option<Entity> {
        let entity = self.by_id.remove(&id)?;
        self.unindex(id, entity.chunk());
        Some(entity)
    }

    pub fn in_chunk(&self, chunk: ChunkPos) -> Vec<i32> {
        self.by_chunk.get(&chunk).map(|s| s.iter().copied().collect()).unwrap_or_default()
    }

    /// Moves an entity, keeping the chunk index right.
    pub fn set_position(&mut self, id: i32, x: f64, y: f64, z: f64) {
        let Some(entity) = self.by_id.get_mut(&id) else { return };
        let before = entity.chunk();
        entity.x = x;
        entity.y = y;
        entity.z = z;
        let after = entity.chunk();
        if before != after {
            self.unindex(id, before);
            self.index(id, after);
        } else {
            self.dirty.insert(after);
        }
    }
}

// ---- spawning ----

fn next_entity_id(server: &Server) -> i32 {
    server.allocate_entity_id()
}

/// Puts a new entity into the world and shows it to nearby players.
pub fn spawn(server: &Arc<Server>, mut entity: Entity) -> i32 {
    entity.id = next_entity_id(server);
    if entity.uuid.is_nil() {
        entity.uuid = Uuid::new_v4();
    }
    let id = entity.id;
    let chunk = entity.chunk();
    let shown = entity.clone();
    server.entities.lock().unwrap_or_else(|e| e.into_inner()).insert(entity);
    for player in watchers(server, chunk) {
        show(&player, &shown);
        player.lock().visible_entities.insert(id);
    }
    id
}

/// Removes an entity everywhere.
/// Replaces what an item entity is carrying, for a hopper that took part
/// of the stack.
pub fn set_item(server: &Arc<Server>, id: i32, stack: garnet_protocol::packets::play::items::ItemStack) {
    let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entity) = entities.by_id.get_mut(&id) {
        entity.item = Some(stack);
    }
}

pub fn despawn(server: &Arc<Server>, id: i32) -> Option<Entity> {
    let entity = server.entities.lock().unwrap_or_else(|e| e.into_inner()).remove(id)?;
    for player in server.online_players() {
        if player.lock().visible_entities.remove(&id) {
            player.send(&cb::RemoveEntities { entity_ids: vec![id] });
        }
    }
    Some(entity)
}

pub fn new_entity(server: &Server, kind: &str, x: f64, y: f64, z: f64) -> Option<Entity> {
    let full = if kind.contains(':') { kind.to_owned() } else { format!("minecraft:{kind}") };
    let type_id = server.data.registries.id_of("entity_type", &full)?;
    Some(Entity {
        id: 0,
        uuid: Uuid::new_v4(),
        kind: full,
        type_id,
        x,
        y,
        z,
        yaw: 0.0,
        pitch: 0.0,
        velocity: (0.0, 0.0, 0.0),
        on_ground: false,
        item: None,
        custom_name: None,
        health: 20.0,
        passengers: Vec::new(),
        vehicle: None,
        pickup_delay: 0,
        age: 0,
        no_gravity: false,
        xp: 0,
        attack_cooldown: 0,
        blocked_ahead: false,
        persistent: false,
    })
}

/// Drops an item stack into the world at a position, with a little push.
pub fn drop_item(server: &Arc<Server>, stack: ItemStack, x: f64, y: f64, z: f64, velocity: (f64, f64, f64), pickup_delay: u32) -> Option<i32> {
    if stack.is_empty() {
        return None;
    }
    let mut entity = new_entity(server, "minecraft:item", x, y, z)?;
    entity.item = Some(stack);
    entity.velocity = velocity;
    entity.pickup_delay = pickup_delay;
    Some(spawn(server, entity))
}

/// The packets that make a client show an entity: spawn, metadata, riders.
fn show(player: &Player, entity: &Entity) {
    player.send(&cb::SpawnEntity {
        entity_id: entity.id,
        uuid: entity.uuid,
        entity_type: entity.type_id,
        x: entity.x,
        y: entity.y,
        z: entity.z,
        velocity: entity.velocity,
        pitch: entity.pitch,
        yaw: entity.yaw,
        head_yaw: entity.yaw,
        data: entity.xp,
    });
    if let Some(metadata) = metadata_packet(entity) {
        player.send(&metadata);
    }
    if !entity.passengers.is_empty() {
        player.send(&cb::SetPassengers {
            vehicle: entity.id,
            passengers: entity.passengers.clone(),
        });
    }
}

/// Metadata: custom name, and the item for item entities.
pub fn metadata_packet(entity: &Entity) -> Option<cb::SetEntityData> {
    let mut entries: Vec<(u8, i32, Vec<u8>)> = Vec::new();
    if let Some(name) = &entity.custom_name {
        let mut w = PacketWriter::new();
        w.write_bool(true);
        w.write_text(&garnet_protocol::Text::legacy(name));
        entries.push((2, 6, w.into_inner()));
        let mut visible = PacketWriter::new();
        visible.write_bool(true);
        entries.push((3, 8, visible.into_inner()));
    }
    if entity.no_gravity {
        let mut w = PacketWriter::new();
        w.write_bool(true);
        entries.push((5, 8, w.into_inner()));
    }
    if let Some(item) = &entity.item {
        let mut w = PacketWriter::new();
        item.write(&mut w);
        entries.push((8, 7, w.into_inner()));
    }
    if entity.is_living() && entity.health != 20.0 {
        let mut w = PacketWriter::new();
        w.write_f32(entity.health);
        entries.push((9, 3, w.into_inner()));
    }
    if entries.is_empty() {
        return None;
    }
    Some(cb::SetEntityData {
        entity_id: entity.id,
        entries,
    })
}

fn watchers(server: &Server, chunk: ChunkPos) -> Vec<Arc<Player>> {
    let ids: Vec<Uuid> = server
        .chunk_watchers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&chunk)
        .map(|s| s.iter().copied().collect())
        .unwrap_or_default();
    ids.into_iter().filter_map(|u| server.player(u)).collect()
}

// ---- visibility ----

/// Shows and hides entities for one player based on its loaded chunks.
pub fn update_visibility(server: &Arc<Server>, player: &Arc<Player>) {
    let (loaded, visible) = {
        let s = player.lock();
        (s.loaded_chunks.clone(), s.visible_entities.clone())
    };
    let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    let should: HashSet<i32> = loaded.iter().flat_map(|c| entities.in_chunk(*c)).collect();
    let appeared: Vec<&Entity> = should.difference(&visible).filter_map(|id| entities.by_id.get(id)).collect();
    let vanished: Vec<i32> = visible.difference(&should).copied().collect();
    if appeared.is_empty() && vanished.is_empty() {
        return;
    }
    for entity in &appeared {
        show(player, entity);
    }
    let appeared_ids: Vec<i32> = appeared.iter().map(|e| e.id).collect();
    drop(entities);
    if !vanished.is_empty() {
        player.send(&cb::RemoveEntities {
            entity_ids: vanished.clone(),
        });
    }
    let mut s = player.lock();
    s.visible_entities.extend(appeared_ids);
    for id in vanished {
        s.visible_entities.remove(&id);
    }
}

// ---- ticking ----

const GRAVITY: f64 = 0.04;
const DRAG: f64 = 0.98;
const GROUND_FRICTION: f64 = 0.6;

/// Physics for every entity, and pickups for items. Once per tick.
pub fn tick(server: &Arc<Server>) {
    let ids: Vec<i32> = server.entities.lock().unwrap_or_else(|e| e.into_inner()).by_id.keys().copied().collect();
    for id in ids {
        let Some(mut entity) = server.entities.lock().unwrap_or_else(|e| e.into_inner()).by_id.get(&id).cloned() else { continue };
        entity.age = entity.age.wrapping_add(1);
        if entity.pickup_delay > 0 {
            entity.pickup_delay -= 1;
        }
        let moved = step_physics(server, &mut entity);
        // Items and other things lying still do not need updates.
        {
            let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(stored) = entities.by_id.get_mut(&id) {
                stored.age = entity.age;
                stored.pickup_delay = entity.pickup_delay;
                stored.velocity = entity.velocity;
                stored.on_ground = entity.on_ground;
            }
            if moved {
                entities.set_position(id, entity.x, entity.y, entity.z);
            }
        }
        if moved {
            let packet = cb::EntityPositionSync {
                entity_id: id,
                x: entity.x,
                y: entity.y,
                z: entity.z,
                yaw: entity.yaw,
                pitch: entity.pitch,
                on_ground: entity.on_ground,
            };
            server.broadcast_near(entity.chunk(), &packet, None);
        }
        if entity.is_item() && entity.pickup_delay == 0 {
            try_pickup(server, &entity);
        }
        // Items vanish after five minutes like vanilla.
        if entity.is_item() && entity.age > 6000 {
            despawn(server, id);
        }
    }
}

/// Gravity and simple landing. Returns whether the entity moved.
fn step_physics(server: &Server, entity: &mut Entity) -> bool {
    if entity.no_gravity || entity.vehicle.is_some() {
        return false;
    }
    let (mut vx, mut vy, mut vz) = entity.velocity;
    let standing_still = entity.on_ground && vx.abs() < 0.003 && vz.abs() < 0.003 && vy <= 0.0;
    if standing_still && solid_below(server, entity.x, entity.y, entity.z) {
        entity.velocity = (0.0, 0.0, 0.0);
        return false;
    }
    vy -= GRAVITY;
    let mut nx = entity.x + vx;
    let mut ny = entity.y + vy;
    let mut nz = entity.z + vz;
    // Walls: give up the horizontal move that would enter a solid block.
    entity.blocked_ahead = false;
    if is_solid(server, nx, entity.y + 0.1, entity.z) {
        nx = entity.x;
        vx = 0.0;
        entity.blocked_ahead = true;
    }
    if is_solid(server, nx, entity.y + 0.1, nz) {
        nz = entity.z;
        vz = 0.0;
        entity.blocked_ahead = true;
    }
    // Floor: land on top of the block below.
    if vy < 0.0 && is_solid(server, nx, ny, nz) {
        ny = ny.floor() + 1.0;
        vy = 0.0;
        entity.on_ground = true;
        vx *= GROUND_FRICTION;
        vz *= GROUND_FRICTION;
    } else {
        entity.on_ground = false;
    }
    vx *= DRAG;
    vy *= DRAG;
    vz *= DRAG;
    if ny < -80.0 {
        // Fell out of the world.
        entity.velocity = (0.0, 0.0, 0.0);
        entity.no_gravity = true;
        return false;
    }
    let moved = (nx - entity.x).abs() > 1e-4 || (ny - entity.y).abs() > 1e-4 || (nz - entity.z).abs() > 1e-4;
    entity.x = nx;
    entity.y = ny;
    entity.z = nz;
    entity.velocity = (vx, vy, vz);
    moved
}

/// Whether something falling would land here. Light is no guide to this:
/// glass and hoppers let light through and still hold an item up, while
/// grass and torches are solid to the eye and not to the foot.
fn is_solid(server: &Server, x: f64, y: f64, z: f64) -> bool {
    let pos = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
    let state = server.world().get_block(pos).unwrap_or(0);
    let blocks = &server.data.blocks;
    if blocks.is_air(state as i32) || blocks.is_liquid(state as i32) {
        return false;
    }
    let Some(block) = blocks.block_of_state(state as i32) else { return false };
    !walks_through(block.name.strip_prefix("minecraft:").unwrap_or(&block.name))
}

/// The blocks nothing stands on.
fn walks_through(short: &str) -> bool {
    matches!(
        short,
        "torch" | "wall_torch" | "soul_torch" | "soul_wall_torch" | "redstone_wire" | "redstone_torch"
            | "redstone_wall_torch" | "lever" | "tripwire" | "tripwire_hook" | "string" | "ladder" | "vine"
            | "dead_bush" | "cobweb" | "nether_portal" | "end_portal" | "light" | "structure_void" | "kelp"
            | "kelp_plant" | "seagrass" | "tall_seagrass" | "sugar_cane" | "bamboo_sapling" | "fire" | "soul_fire"
            | "wheat" | "carrots" | "potatoes" | "beetroots" | "nether_wart" | "sweet_berry_bush" | "cocoa"
            | "melon_stem" | "pumpkin_stem" | "attached_melon_stem" | "attached_pumpkin_stem" | "glow_lichen"
    ) || short.ends_with("_grass")
        || short.ends_with("_fern")
        || short.ends_with("_flower")
        || short.ends_with("_sapling")
        || short.ends_with("_button")
        || short.ends_with("_rail")
        || short == "rail"
        || short.ends_with("_sign")
        || short.ends_with("_banner")
        || short.ends_with("_pressure_plate")
        || short.ends_with("_coral_fan")
        || short.ends_with("_torch")
}

fn solid_below(server: &Server, x: f64, y: f64, z: f64) -> bool {
    is_solid(server, x, y - 0.05, z)
}

/// A player within reach takes the item.
fn try_pickup(server: &Arc<Server>, entity: &Entity) {
    let Some(stack) = entity.item.clone() else { return };
    let nearby = server.online_players().into_iter().find(|p| {
        let s = p.lock();
        if matches!(s.game_mode, garnet_protocol::packets::play::GameMode::Spectator) || s.health <= 0.0 {
            return false;
        }
        let dx = s.x - entity.x;
        let dy = (s.y + 0.9) - entity.y;
        let dz = s.z - entity.z;
        dx * dx + dy * dy + dz * dz < 1.6 * 1.6
    });
    let Some(player) = nearby else { return };
    let limit = max_stack(server, stack.item);
    let count = stack.count;
    let left = player.lock().inventory.add(stack.clone(), limit);
    let taken = count - left.count.max(0);
    if taken <= 0 {
        return;
    }
    crate::items::sync_inventory(&player);
    server.broadcast_near(
        entity.chunk(),
        &cb::TakeItemEntity {
            item_id: entity.id,
            player_id: player.entity_id,
            amount: taken,
        },
        None,
    );
    if left.is_empty() {
        despawn(server, entity.id);
    } else {
        let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = entities.by_id.get_mut(&entity.id) {
            e.item = Some(left);
        }
        let packet = entities.by_id.get(&entity.id).and_then(metadata_packet);
        drop(entities);
        if let Some(packet) = packet {
            server.broadcast_near(entity.chunk(), &packet, None);
        }
    }
}

// ---- riding ----

pub fn mount(server: &Arc<Server>, passenger: i32, vehicle: i32) -> Result<(), String> {
    if passenger == vehicle {
        return Err("An entity cannot ride itself.".into());
    }
    let (chunk, passengers) = {
        let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        let Some(v) = entities.by_id.get_mut(&vehicle) else { return Err("The vehicle is not an entity that can be ridden.".into()) };
        if !v.passengers.contains(&passenger) {
            v.passengers.push(passenger);
        }
        let out = (v.chunk(), v.passengers.clone());
        if let Some(p) = entities.by_id.get_mut(&passenger) {
            p.vehicle = Some(vehicle);
        }
        out
    };
    server.broadcast_near(chunk, &cb::SetPassengers { vehicle, passengers }, None);
    Ok(())
}

pub fn dismount(server: &Arc<Server>, passenger: i32) -> bool {
    // The vehicle is whichever entity lists the passenger (players ride too).
    let update = {
        let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        let vehicle = entities.by_id.values_mut().find(|v| v.passengers.contains(&passenger)).map(|v| {
            v.passengers.retain(|p| *p != passenger);
            (v.chunk(), v.id, v.passengers.clone())
        });
        if let Some(p) = entities.by_id.get_mut(&passenger) {
            p.vehicle = None;
        }
        vehicle
    };
    match update {
        Some((chunk, vehicle, passengers)) => {
            server.broadcast_near(chunk, &cb::SetPassengers { vehicle, passengers }, None);
            true
        }
        None => false,
    }
}

// ---- saving and loading ----

fn uuid_to_ints(uuid: Uuid) -> Vec<i32> {
    let b = uuid.as_u128();
    vec![(b >> 96) as i32, (b >> 64) as i32, (b >> 32) as i32, b as i32]
}

fn ints_to_uuid(ints: &[i32]) -> Option<Uuid> {
    if ints.len() != 4 {
        return None;
    }
    let v = ((ints[0] as u32 as u128) << 96) | ((ints[1] as u32 as u128) << 64) | ((ints[2] as u32 as u128) << 32) | ints[3] as u32 as u128;
    Some(Uuid::from_u128(v))
}

fn to_nbt(server: &Server, e: &Entity) -> NbtCompound {
    let mut c = NbtCompound::new();
    c.put("id", e.kind.as_str());
    c.put("Pos", vec![NbtTag::Double(e.x), NbtTag::Double(e.y), NbtTag::Double(e.z)]);
    c.put("Motion", vec![NbtTag::Double(e.velocity.0), NbtTag::Double(e.velocity.1), NbtTag::Double(e.velocity.2)]);
    c.put("Rotation", vec![NbtTag::Float(e.yaw), NbtTag::Float(e.pitch)]);
    c.put("UUID", uuid_to_ints(e.uuid));
    c.put("OnGround", e.on_ground);
    c.put("Health", e.health);
    c.put("Age", e.age as i32);
    c.put("PickupDelay", e.pickup_delay as i16 as i32);
    c.put("NoGravity", e.no_gravity);
    if let Some(name) = &e.custom_name {
        c.put("CustomName", name.as_str());
    }
    if let Some(item) = &e.item {
        let mut i = NbtCompound::new();
        i.put("id", item_name(server, item.item).as_str());
        i.put("count", item.count);
        if !item.patch.is_empty() {
            i.put("garnet_patch", NbtTag::ByteArray(item.patch.iter().map(|b| *b as i8).collect()));
        }
        c.put("Item", i);
    }
    c
}

fn from_nbt(server: &Server, c: &NbtCompound) -> Option<Entity> {
    let kind = c.get_str("id")?;
    let pos = c.get_list("Pos")?;
    let num = |t: Option<&NbtTag>| t.and_then(NbtTag::as_f64).unwrap_or(0.0);
    let mut e = new_entity(server, kind, num(pos.first()), num(pos.get(1)), num(pos.get(2)))?;
    if let Some(m) = c.get_list("Motion") {
        e.velocity = (num(m.first()), num(m.get(1)), num(m.get(2)));
    }
    if let Some(r) = c.get_list("Rotation") {
        e.yaw = num(r.first()) as f32;
        e.pitch = num(r.get(1)) as f32;
    }
    if let Some(NbtTag::IntArray(ints)) = c.get("UUID") {
        if let Some(uuid) = ints_to_uuid(ints) {
            e.uuid = uuid;
        }
    }
    e.on_ground = c.get_bool("OnGround").unwrap_or(false);
    e.health = c.get_f64("Health").map(|h| h as f32).unwrap_or(20.0);
    e.age = c.get_i32("Age").unwrap_or(0).max(0) as u32;
    e.pickup_delay = c.get_i32("PickupDelay").unwrap_or(0).max(0) as u32;
    e.no_gravity = c.get_bool("NoGravity").unwrap_or(false);
    e.custom_name = c.get_str("CustomName").map(str::to_owned);
    if let Some(item) = c.get_compound("Item") {
        let id = item.get_str("id").and_then(|n| item_id(server, n))?;
        let patch = match item.get("garnet_patch") {
            Some(NbtTag::ByteArray(b)) => b.iter().map(|x| *x as u8).collect(),
            _ => Vec::new(),
        };
        e.item = Some(ItemStack {
            item: id,
            count: item.get_i32("count").unwrap_or(1),
            patch,
        });
    }
    Some(e)
}

/// Loads the entities saved for a chunk that just became loaded.
pub fn load_chunk(server: &Arc<Server>, chunk: ChunkPos) {
    let root = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        if entities.by_chunk.contains_key(&chunk) {
            return;
        }
        match entities.regions.read(chunk) {
            Ok(root) => root,
            Err(err) => {
                tracing::warn!("reading entities of chunk {:?}: {err}", chunk);
                None
            }
        }
    };
    let Some(root) = root else { return };
    let list: Vec<Entity> = root
        .get_list("Entities")
        .unwrap_or(&[])
        .iter()
        .filter_map(|t| t.as_compound())
        .filter_map(|c| from_nbt(server, c))
        .collect();
    for entity in list {
        spawn(server, entity);
    }
    server.entities.lock().unwrap_or_else(|e| e.into_inner()).dirty.remove(&chunk);
}

/// Writes the entities of one chunk; drops them from memory when `unload`.
pub fn save_chunk(server: &Arc<Server>, chunk: ChunkPos, unload: bool) {
    let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    let ids = entities.in_chunk(chunk);
    let list: Vec<NbtTag> = ids
        .iter()
        .filter_map(|id| entities.by_id.get(id))
        .map(|e| NbtTag::Compound(to_nbt(server, e)))
        .collect();
    let mut root = NbtCompound::new();
    root.put("DataVersion", server.data.data_version);
    root.put("Position", vec![chunk.x, chunk.z]);
    root.put("Entities", list);
    if let Err(err) = entities.regions.write(chunk, &root) {
        tracing::error!("saving entities of chunk {:?}: {err}", chunk);
    }
    entities.dirty.remove(&chunk);
    if unload {
        for id in ids {
            entities.remove(id);
        }
    }
}

/// Saves every chunk with changes.
pub fn save_all(server: &Arc<Server>) {
    let dirty: Vec<ChunkPos> = server.entities.lock().unwrap_or_else(|e| e.into_inner()).dirty.iter().copied().collect();
    for chunk in dirty {
        save_chunk(server, chunk, false);
    }
}

/// Entities in chunks nobody watches any more are saved and dropped.
pub fn unload_unwatched(server: &Arc<Server>) {
    let watched: HashSet<ChunkPos> = server.chunk_watchers.lock().unwrap_or_else(|e| e.into_inner()).keys().copied().collect();
    let loaded: Vec<ChunkPos> = server.entities.lock().unwrap_or_else(|e| e.into_inner()).by_chunk.keys().copied().collect();
    for chunk in loaded {
        if !watched.contains(&chunk) && !server.world().is_loaded(chunk) {
            save_chunk(server, chunk, true);
        }
    }
}
