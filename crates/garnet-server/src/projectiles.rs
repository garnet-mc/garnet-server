//! Things that fly: arrows, snowballs, eggs, ender pearls and thrown
//! potions.
//!
//! They all move the same way -- a little gravity, a little drag, and a
//! straight line between where they were and where they are now that is
//! checked against the blocks and everyone standing in it -- and differ
//! only in what happens when they land. Vanilla's numbers throughout: an
//! arrow leaves a fully drawn bow at three blocks a tick and hurts by how
//! fast it is going when it arrives.

use crate::player::Player;
use crate::server::Server;
use crate::world_entities::{self, Entity};
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::items::ItemStack;
use garnet_protocol::{BlockPos, ChunkPos};
use std::sync::Arc;
use uuid::Uuid;

/// How much of its speed a thing in flight keeps each tick.
const AIR_DRAG: f64 = 0.99;
const WATER_DRAG: f64 = 0.6;
/// How fast an arrow leaves a fully drawn bow, in blocks per tick.
const BOW_SPEED: f64 = 3.0;
/// What a thrown thing leaves the hand at.
const THROW_SPEED: f64 = 1.5;
/// A splash potion is lobbed rather than thrown.
const SPLASH_SPEED: f64 = 0.5;
/// How long a fully drawn bow takes.
const FULL_DRAW: u64 = 20;
/// How far from a thrown potion its effects reach.
const SPLASH_RANGE: f64 = 4.0;
/// Nothing stays in flight longer than this.
const MAX_AGE: u32 = 1200;
/// How finely the flight path is checked against the world.
const STEP: f64 = 0.1;

/// What a thing in flight is, beyond where it is going.
#[derive(Clone, Debug, Default)]
pub struct Projectile {
    /// Whoever threw it, so it does not hit them on the way out and so the
    /// client can draw it leaving their hand.
    pub owner: Option<i32>,
    pub owner_uuid: Option<Uuid>,
    /// What it does on the way in: an arrow hurts by how fast it is going.
    pub damage: f32,
    pub knockback: f32,
    pub gravity: f64,
    /// Which potion it carries, for the ones that are thrown.
    pub potion: Option<i32>,
    /// What can be picked up where it lands.
    pub pickup: Option<ItemStack>,
    /// An arrow that has landed waits to be picked up instead of moving.
    pub stuck: bool,
    /// Ticks an arrow has sat in a block.
    pub stuck_for: u32,
}

/// How long an arrow stays in the ground before it is gone.
const STUCK_LIFE: u32 = 1200;

/// Whether this kind of entity is one of the things that fly.
pub fn flies(kind: &str) -> bool {
    matches!(
        kind.strip_prefix("minecraft:").unwrap_or(kind),
        "arrow"
            | "spectral_arrow"
            | "snowball"
            | "egg"
            | "ender_pearl"
            | "experience_bottle"
            | "splash_potion"
            | "lingering_potion"
    )
}

/// A projectile that has already come to rest, for the ones that were in
/// the air when the world was last saved.
pub fn landed() -> Projectile {
    Projectile {
        stuck: true,
        ..Projectile::default()
    }
}

/// A player used something that flies. Returns whether anything was thrown.
pub fn throw_held(server: &Arc<Server>, player: &Arc<Player>) -> bool {
    let held = {
        let s = player.lock();
        let slot = s.held_slot;
        s.inventory.held(slot).clone()
    };
    if held.is_empty() {
        return false;
    }
    let name = crate::items::item_name(server, held.item);
    let (kind, speed, lob) = match name.as_str() {
        "minecraft:snowball" => ("minecraft:snowball", THROW_SPEED, 0.0),
        "minecraft:egg" => ("minecraft:egg", THROW_SPEED, 0.0),
        "minecraft:ender_pearl" => ("minecraft:ender_pearl", THROW_SPEED, 0.0),
        "minecraft:experience_bottle" => ("minecraft:experience_bottle", 0.7, -20.0),
        "minecraft:splash_potion" => ("minecraft:splash_potion", SPLASH_SPEED, -20.0),
        "minecraft:lingering_potion" => ("minecraft:lingering_potion", SPLASH_SPEED, -20.0),
        _ => return false,
    };
    let shot = Projectile {
        potion: garnet_protocol::packets::play::items::potion_in(&held.patch),
        ..Projectile::default()
    };
    if launch(server, player, kind, speed, lob, shot).is_none() {
        return false;
    }
    spend_held(server, player, 1);
    swing(server, player);
    true
}

/// A drawn bow was let go.
pub fn loose_arrow(server: &Arc<Server>, player: &Arc<Player>, drawn_for: u64) {
    let held = {
        let s = player.lock();
        let slot = s.held_slot;
        s.inventory.held(slot).clone()
    };
    if held.is_empty() || crate::items::item_name(server, held.item) != "minecraft:bow" {
        return;
    }
    // Vanilla's draw: nothing at all under a fifth of a second, then
    // quickly up to full.
    let drawn = (drawn_for.min(FULL_DRAW) as f64) / FULL_DRAW as f64;
    let power = (drawn * drawn + drawn * 2.0) / 3.0;
    if power < 0.1 {
        return;
    }
    let free = matches!(player.lock().game_mode, garnet_protocol::packets::play::GameMode::Creative);
    let arrow = free
        .then(|| ItemStack::new(crate::items::item_id(server, "minecraft:arrow").unwrap_or(0), 1))
        .or_else(|| take_arrow(server, player));
    let Some(arrow) = arrow else { return };
    let shot = Projectile {
        damage: 2.0,
        knockback: 1.0,
        // Only an arrow that was paid for can be picked up again.
        pickup: (!free).then(|| arrow.clone()),
        ..Projectile::default()
    };
    if launch(server, player, "minecraft:arrow", BOW_SPEED * power.min(1.0), 0.0, shot).is_some() {
        crate::durability::use_held(server, player, 1);
        swing(server, player);
    }
}

/// Takes one arrow out of the quiver, wherever it is.
fn take_arrow(server: &Arc<Server>, player: &Arc<Player>) -> Option<ItemStack> {
    let mut s = player.lock();
    let slot = (0..s.inventory.slots.len()).find(|slot| {
        let stack = &s.inventory.slots[*slot];
        !stack.is_empty() && crate::items::item_name(server, stack.item) == "minecraft:arrow"
    })?;
    let taken = ItemStack::new(s.inventory.slots[slot].item, 1);
    let stack = &mut s.inventory.slots[slot];
    stack.count -= 1;
    if stack.count <= 0 {
        *stack = ItemStack::EMPTY;
    }
    drop(s);
    crate::items::sync_inventory(player);
    Some(taken)
}

/// A mob looses an arrow at something, the way a skeleton does: not
/// quite straight, and less straight the easier the difficulty.
pub fn mob_shoots(server: &Arc<Server>, from: (f64, f64, f64), at: (f64, f64, f64), owner: i32, spread: f64) {
    let (dx, dy, dz) = (at.0 - from.0, at.1 - from.1, at.2 - from.2);
    let flat = (dx * dx + dz * dz).sqrt();
    if flat < 0.01 {
        return;
    }
    // Aim a little high, as vanilla does, so the arrow drops onto them.
    let aim = (dx, dy + flat * 0.2, dz);
    let length = (aim.0 * aim.0 + aim.1 * aim.1 + aim.2 * aim.2).sqrt();
    let wobble = || (rand::random::<f64>() - rand::random::<f64>()) * spread;
    let speed = 1.6;
    let velocity = (
        aim.0 / length * speed + wobble(),
        aim.1 / length * speed + wobble(),
        aim.2 / length * speed + wobble(),
    );
    let Some(mut entity) = world_entities::new_entity(server, "minecraft:arrow", from.0, from.1, from.2) else {
        return;
    };
    entity.velocity = velocity;
    entity.no_gravity = true;
    entity.projectile = Some(Projectile {
        owner: Some(owner),
        damage: 2.0,
        knockback: 1.0,
        gravity: gravity_of("arrow"),
        // What a skeleton shoots can be gathered up afterwards, which is
        // what makes a skeleton worth standing in front of.
        pickup: crate::items::item_id(server, "minecraft:arrow").map(|id| ItemStack::new(id, 1)),
        ..Projectile::default()
    });
    world_entities::spawn(server, entity);
}

/// A mob lobs a potion at someone, the way a witch does: high and slow,
/// so it breaks at their feet.
pub fn mob_throws(server: &Arc<Server>, from: (f64, f64, f64), at: (f64, f64, f64), owner: i32, potion: i32) {
    let (dx, dy, dz) = (at.0 - from.0, at.1 - from.1 + 1.1, at.2 - from.2);
    let flat = (dx * dx + dz * dz).sqrt();
    if flat < 0.01 {
        return;
    }
    // Thrown up at an angle so it comes down on them.
    let aim = (dx, dy + flat * 0.2, dz);
    let length = (aim.0 * aim.0 + aim.1 * aim.1 + aim.2 * aim.2).sqrt();
    let speed = 0.75;
    let Some(mut entity) = world_entities::new_entity(server, "minecraft:splash_potion", from.0, from.1, from.2) else {
        return;
    };
    entity.velocity = (aim.0 / length * speed, aim.1 / length * speed, aim.2 / length * speed);
    entity.no_gravity = true;
    entity.projectile = Some(Projectile {
        owner: Some(owner),
        gravity: gravity_of("splash_potion"),
        potion: Some(potion),
        ..Projectile::default()
    });
    world_entities::spawn(server, entity);
}

/// Puts one thing in flight, aimed where the player is looking.
fn launch(server: &Arc<Server>, player: &Arc<Player>, kind: &str, speed: f64, lob: f32, mut shot: Projectile) -> Option<i32> {
    let (x, y, z, yaw, pitch, id, uuid) = {
        let s = player.lock();
        let eyes = s.eye_position();
        (
            eyes.0,
            eyes.1 - 0.1,
            eyes.2,
            s.yaw,
            s.pitch + lob,
            player.entity_id,
            player.uuid,
        )
    };
    shot.owner = Some(id);
    shot.owner_uuid = Some(uuid);
    if shot.gravity == 0.0 {
        shot.gravity = gravity_of(kind);
    }
    let direction = look_vector(yaw, pitch);
    let mut entity = world_entities::new_entity(server, kind, x, y, z)?;
    entity.velocity = (direction.0 * speed, direction.1 * speed, direction.2 * speed);
    entity.yaw = yaw;
    entity.pitch = pitch;
    entity.no_gravity = true; // it falls by its own rules, not the world's
    entity.projectile = Some(shot);
    Some(world_entities::spawn(server, entity))
}

/// How fast each kind falls, in blocks per tick per tick.
fn gravity_of(kind: &str) -> f64 {
    match kind.strip_prefix("minecraft:").unwrap_or(kind) {
        "arrow" | "spectral_arrow" | "trident" => 0.05,
        _ => 0.03,
    }
}

fn look_vector(yaw: f32, pitch: f32) -> (f64, f64, f64) {
    let yaw = (yaw as f64).to_radians();
    let pitch = (pitch as f64).to_radians();
    (-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos())
}

/// One tick of flight: move, and see what it ran into.
pub fn step(server: &Arc<Server>, mut entity: Entity) {
    let id = entity.id;
    let Some(mut shot) = entity.projectile.clone() else { return };
    entity.age = entity.age.wrapping_add(1);
    if entity.age > MAX_AGE {
        world_entities::despawn(server, id);
        return;
    }

    // An arrow in the ground waits to be picked up, and eventually rots.
    if shot.stuck {
        shot.stuck_for += 1;
        if shot.stuck_for > STUCK_LIFE {
            world_entities::despawn(server, id);
            return;
        }
        if let Some(arrow) = shot.pickup.clone() {
            if pick_up(server, &entity, &arrow) {
                world_entities::despawn(server, id);
                return;
            }
        }
        save(server, id, &entity, &shot);
        return;
    }

    let (vx, vy, vz) = entity.velocity;
    let from = (entity.x, entity.y, entity.z);
    let to = (from.0 + vx, from.1 + vy, from.2 + vz);

    // Anything it passes through on the way is hit first.
    if let Some(victim) = first_entity_hit(server, &entity, from, to) {
        strike(server, &entity, &shot, victim);
        // Whatever hits someone is spent: an arrow in a body is gone, and
        // a potion breaks against them.
        if entity.kind.ends_with("potion") {
            splash(server, &entity, &shot);
        }
        world_entities::despawn(server, id);
        return;
    } else if let Some(block) = first_block_hit(server, from, to) {
        if !land(server, &mut entity, &mut shot, Some(block)) {
            world_entities::despawn(server, id);
            return;
        }
    } else {
        entity.x = to.0;
        entity.y = to.1;
        entity.z = to.2;
        let drag = if in_water(server, entity.x, entity.y, entity.z) {
            WATER_DRAG
        } else {
            AIR_DRAG
        };
        entity.velocity = ((vx * drag), (vy - shot.gravity) * drag, (vz * drag));
        // It points the way it is going.
        let flat = (vx * vx + vz * vz).sqrt();
        entity.yaw = (vx.atan2(vz) as f32).to_degrees() * -1.0;
        entity.pitch = ((-vy).atan2(flat) as f32).to_degrees();
    }

    if entity.y < -80.0 {
        world_entities::despawn(server, id);
        return;
    }
    save(server, id, &entity, &shot);
    let packet = cb::EntityPositionSync {
        entity_id: id,
        x: entity.x,
        y: entity.y,
        z: entity.z,
        yaw: entity.yaw,
        pitch: entity.pitch,
        on_ground: shot.stuck,
    };
    server.broadcast_near(entity.chunk(), &packet, None);
}

/// Writes a projectile's flight back to where the world keeps it.
fn save(server: &Arc<Server>, id: i32, entity: &Entity, shot: &Projectile) {
    let mut entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(stored) = entities.by_id.get_mut(&id) {
        stored.age = entity.age;
        stored.velocity = entity.velocity;
        stored.yaw = entity.yaw;
        stored.pitch = entity.pitch;
        stored.projectile = Some(shot.clone());
    }
    entities.set_position(id, entity.x, entity.y, entity.z);
}

/// It arrived. Returns whether it is still in the world afterwards: an
/// arrow sticks where it landed, everything else breaks.
fn land(server: &Arc<Server>, entity: &mut Entity, shot: &mut Projectile, block: Option<BlockPos>) -> bool {
    if let Some(pos) = block {
        // Stop just short of the block it hit.
        entity.x = entity.x + entity.velocity.0 * 0.5;
        entity.y = entity.y + entity.velocity.1 * 0.5;
        entity.z = entity.z + entity.velocity.2 * 0.5;
        let _ = pos;
    }
    match entity.kind.strip_prefix("minecraft:").unwrap_or(&entity.kind) {
        "arrow" | "spectral_arrow" => {
            entity.velocity = (0.0, 0.0, 0.0);
            shot.stuck = true;
            hit_sound(server, entity, "minecraft:entity.arrow.hit");
            true
        }
        "splash_potion" | "lingering_potion" => {
            splash(server, entity, shot);
            false
        }
        "ender_pearl" => {
            teleport_owner(server, entity, shot);
            false
        }
        "egg" => {
            hatch(server, entity);
            false
        }
        "experience_bottle" => {
            crate::experience::drop_orbs(server, entity.x, entity.y, entity.z, rand::random_range(3..=11));
            false
        }
        _ => {
            hit_sound(server, entity, "minecraft:entity.snowball.throw");
            false
        }
    }
}

/// What a projectile does to whatever it hits.
fn strike(server: &Arc<Server>, entity: &Entity, shot: &Projectile, victim: Victim) {
    let speed = {
        let (vx, vy, vz) = entity.velocity;
        (vx * vx + vy * vy + vz * vz).sqrt()
    };
    // An arrow hurts by how fast it is going; everything else thrown does
    // what it does regardless.
    let damage = match entity.kind.as_str() {
        "minecraft:arrow" | "minecraft:spectral_arrow" => ((speed * shot.damage as f64).ceil() as f32).max(1.0),
        _ => 0.0,
    };
    match victim {
        Victim::Player(player) => {
            if damage > 0.0 {
                let from = (entity.x, entity.z);
                crate::survival::hurt(server, &player, damage, "arrow", Some(from));
            }
            knock_back(server, player.entity_id, entity, shot);
        }
        Victim::Mob(id) => {
            if damage > 0.0 {
                crate::survival::damage_entity_directly(server, id, damage, (entity.x, entity.z));
            }
            knock_back(server, id, entity, shot);
        }
    }
    hit_sound(server, entity, "minecraft:entity.arrow.hit");
}

/// A push in the direction it was travelling.
fn knock_back(server: &Arc<Server>, target: i32, entity: &Entity, shot: &Projectile) {
    if shot.knockback <= 0.0 {
        return;
    }
    let (vx, _, vz) = entity.velocity;
    let flat = (vx * vx + vz * vz).sqrt().max(0.001);
    let push = 0.4 * shot.knockback as f64;
    server.broadcast_near(
        entity.chunk(),
        &cb::SetEntityMotion {
            entity_id: target,
            x: vx / flat * push,
            y: 0.3,
            z: vz / flat * push,
        },
        None,
    );
}

/// Who a projectile can run into.
enum Victim {
    Player(Arc<Player>),
    Mob(i32),
}

/// The first player or mob standing in the way, if any.
fn first_entity_hit(server: &Arc<Server>, entity: &Entity, from: (f64, f64, f64), to: (f64, f64, f64)) -> Option<Victim> {
    // A thing does not hit whoever threw it, at least on the way out.
    let owner = entity.projectile.as_ref().and_then(|shot| shot.owner);
    let young = entity.age < 5;
    let mut best: Option<(f64, Victim)> = None;
    for player in server.online_players() {
        if young && Some(player.entity_id) == owner {
            continue;
        }
        let (x, y, z, mode) = {
            let s = player.lock();
            (s.x, s.y, s.z, s.game_mode)
        };
        if matches!(
            mode,
            garnet_protocol::packets::play::GameMode::Creative | garnet_protocol::packets::play::GameMode::Spectator
        ) {
            continue;
        }
        // A player is about a third of a block wide at the shoulders and
        // stands nearly two high.
        if let Some(distance) = crosses(from, to, (x, y + 0.9, z), 0.9) {
            if best.as_ref().is_none_or(|(closest, _)| distance < *closest) {
                best = Some((distance, Victim::Player(player.clone())));
            }
        }
    }
    let mobs: Vec<(i32, f64, f64, f64)> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities
            .by_id
            .values()
            .filter(|other| other.id != entity.id && other.is_living() && other.health > 0.0)
            .map(|other| (other.id, other.x, other.y, other.z))
            .collect()
    };
    for (id, x, y, z) in mobs {
        if Some(id) == owner {
            continue;
        }
        if let Some(distance) = crosses(from, to, (x, y + 0.9, z), 0.9) {
            if best.as_ref().is_none_or(|(closest, _)| distance < *closest) {
                best = Some((distance, Victim::Mob(id)));
            }
        }
    }
    best.map(|(_, victim)| victim)
}

/// Whether a flight passes within `radius` of a point, and how far along it
/// does so.
fn crosses(from: (f64, f64, f64), to: (f64, f64, f64), point: (f64, f64, f64), radius: f64) -> Option<f64> {
    let line = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
    let length = (line.0 * line.0 + line.1 * line.1 + line.2 * line.2).sqrt();
    if length < 1e-6 {
        return None;
    }
    let mut travelled = 0.0;
    while travelled <= length {
        let share = travelled / length;
        let at = (from.0 + line.0 * share, from.1 + line.1 * share, from.2 + line.2 * share);
        let gap = ((at.0 - point.0).powi(2) + (at.1 - point.1).powi(2) + (at.2 - point.2).powi(2)).sqrt();
        if gap <= radius {
            return Some(travelled);
        }
        travelled += STEP;
    }
    None
}

/// The first block the flight runs into.
fn first_block_hit(server: &Arc<Server>, from: (f64, f64, f64), to: (f64, f64, f64)) -> Option<BlockPos> {
    let line = (to.0 - from.0, to.1 - from.1, to.2 - from.2);
    let length = (line.0 * line.0 + line.1 * line.1 + line.2 * line.2).sqrt();
    if length < 1e-6 {
        return None;
    }
    let mut travelled = 0.0;
    while travelled <= length {
        let share = travelled / length;
        let at = (from.0 + line.0 * share, from.1 + line.1 * share, from.2 + line.2 * share);
        let pos = BlockPos::new(at.0.floor() as i32, at.1.floor() as i32, at.2.floor() as i32);
        if stops_a_flight(server, pos) {
            return Some(pos);
        }
        travelled += STEP;
    }
    None
}

/// Whether a block is solid enough to stop something in flight.
fn stops_a_flight(server: &Arc<Server>, pos: BlockPos) -> bool {
    let Ok(state) = server.world().get_block(pos) else {
        return false;
    };
    let blocks = &server.data.blocks;
    if blocks.is_air(state as i32) || blocks.is_liquid(state as i32) {
        return false;
    }
    let Some(block) = blocks.block_of_state(state as i32) else {
        return false;
    };
    let short = block.name.strip_prefix("minecraft:").unwrap_or(&block.name);
    // An arrow flies through what a player can walk through.
    !matches!(short, "light" | "structure_void" | "barrier" | "air")
        && !short.ends_with("_grass")
        && !short.ends_with("_fern")
        && !short.ends_with("_flower")
        && !matches!(
            short,
            "torch"
                | "wall_torch"
                | "redstone_wire"
                | "tripwire"
                | "string"
                | "vine"
                | "dead_bush"
                | "kelp"
                | "kelp_plant"
                | "seagrass"
                | "tall_seagrass"
                | "wheat"
                | "carrots"
                | "potatoes"
                | "beetroots"
                | "nether_wart"
                | "sugar_cane"
                | "fire"
                | "soul_fire"
                | "nether_portal"
        )
}

fn in_water(server: &Arc<Server>, x: f64, y: f64, z: f64) -> bool {
    let pos = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
    let Ok(state) = server.world().get_block(pos) else {
        return false;
    };
    server.data.blocks.is_liquid(state as i32)
}

/// A landed arrow goes to whoever walks over it.
fn pick_up(server: &Arc<Server>, entity: &Entity, arrow: &ItemStack) -> bool {
    for player in server.online_players() {
        let (x, y, z, mode) = {
            let s = player.lock();
            (s.x, s.y, s.z, s.game_mode)
        };
        if matches!(mode, garnet_protocol::packets::play::GameMode::Spectator) {
            continue;
        }
        let gap = ((entity.x - x).powi(2) + (entity.y - y - 0.5).powi(2) + (entity.z - z).powi(2)).sqrt();
        if gap > 1.4 {
            continue;
        }
        let over = player.lock().inventory.add(arrow.clone(), 64);
        if !over.is_empty() {
            continue; // no room for it
        }
        crate::items::sync_inventory(&player);
        player.send(&cb::TakeItemEntity {
            item_id: entity.id,
            player_id: player.entity_id,
            amount: 1,
        });
        return true;
    }
    false
}

/// A thrown potion breaks: everyone near enough gets what was in it, less
/// the further away they were.
fn splash(server: &Arc<Server>, entity: &Entity, shot: &Projectile) {
    let Some(potion) = shot.potion else {
        broken(server, entity);
        return;
    };
    let Some(name) = server.data.registries.name_of("potion", potion).map(str::to_owned) else {
        broken(server, entity);
        return;
    };
    let effects = crate::potions::effects_of(&name);
    // Mobs caught in it take the healing or the harm; the rest of the
    // effects need a mob that can carry them, which is for later.
    let mobs: Vec<(i32, f64, f64, f64)> = {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        entities
            .by_id
            .values()
            .filter(|other| other.is_living() && other.health > 0.0)
            .map(|other| (other.id, other.x, other.y, other.z))
            .collect()
    };
    for (id, x, y, z) in mobs {
        // Whoever threw it knows better than to stand in it.
        if Some(id) == shot.owner {
            continue;
        }
        let gap = ((entity.x - x).powi(2) + (entity.y - y).powi(2) + (entity.z - z).powi(2)).sqrt();
        if gap > SPLASH_RANGE {
            continue;
        }
        let share = 1.0 - gap / SPLASH_RANGE;
        for (effect, _, amplifier) in effects {
            if *effect == "instant_damage" {
                let harm = 6.0 * (1 << amplifier.clamp(&0, &6)) as f32 * share as f32;
                crate::survival::damage_entity_directly(server, id, harm.max(1.0), (entity.x, entity.z));
            }
        }
    }
    for player in server.online_players() {
        let (x, y, z, mode) = {
            let s = player.lock();
            (s.x, s.y, s.z, s.game_mode)
        };
        if matches!(mode, garnet_protocol::packets::play::GameMode::Spectator) {
            continue;
        }
        let gap = ((entity.x - x).powi(2) + (entity.y - y).powi(2) + (entity.z - z).powi(2)).sqrt();
        if gap > SPLASH_RANGE {
            continue;
        }
        // Vanilla: full strength at the middle, nothing at the edge.
        let share = 1.0 - gap / SPLASH_RANGE;
        for (effect, duration, amplifier) in effects {
            let effect = format!("minecraft:{effect}");
            match effect.as_str() {
                "minecraft:instant_health" => {
                    let heal = 4.0 * (1 << amplifier.clamp(&0, &6)) as f32 * share as f32;
                    crate::survival::heal(server, &player, heal.max(1.0));
                }
                "minecraft:instant_damage" => {
                    let harm = 6.0 * (1 << amplifier.clamp(&0, &6)) as f32 * share as f32;
                    crate::survival::hurt(server, &player, harm.max(1.0), "magic", None);
                }
                _ => {
                    let Some(id) = server.data.id_of("mob_effect", &effect) else {
                        continue;
                    };
                    // A thrown potion is weaker than one drunk.
                    let ticks = ((*duration as f64) * 0.75 * share).round() as i32;
                    if ticks > 20 {
                        crate::vanilla_commands::give_effect(server, &player, &effect, id, *amplifier, ticks, true);
                    }
                }
            }
        }
    }
    broken(server, entity);
}

/// The glass and the colour everyone sees when a potion lands.
fn broken(server: &Arc<Server>, entity: &Entity) {
    hit_sound(server, entity, "minecraft:entity.splash_potion.break");
    server.broadcast_near(
        entity.chunk(),
        &cb::LevelEvent {
            event: 2002, // a potion breaking, which the client draws for us
            pos: BlockPos::new(entity.x.floor() as i32, entity.y.floor() as i32, entity.z.floor() as i32),
            data: 0,
            global: false,
        },
        None,
    );
}

/// An ender pearl takes whoever threw it to where it landed.
fn teleport_owner(server: &Arc<Server>, entity: &Entity, shot: &Projectile) {
    let Some(uuid) = shot.owner_uuid else { return };
    let Some(player) = server.player(uuid) else { return };
    // Read where they are looking first: teleporting takes the same lock.
    let (yaw, pitch) = {
        let mut s = player.lock();
        s.x = entity.x;
        s.y = entity.y;
        s.z = entity.z;
        (s.yaw, s.pitch)
    };
    crate::commands::teleport(&player, entity.x, entity.y, entity.z, yaw, pitch);
    // The trip costs five hearts' worth, as vanilla charges.
    crate::survival::hurt(server, &player, 5.0, "fall", None);
    hit_sound(server, entity, "minecraft:entity.enderman.teleport");
}

/// A thrown egg sometimes leaves a chick behind.
fn hatch(server: &Arc<Server>, entity: &Entity) {
    hit_sound(server, entity, "minecraft:entity.egg.throw");
    if rand::random_range(0..8) != 0 {
        return;
    }
    if let Some(mut chick) = world_entities::new_entity(server, "minecraft:chicken", entity.x, entity.y, entity.z) {
        chick.health = 4.0;
        world_entities::spawn(server, chick);
    }
}

fn hit_sound(server: &Arc<Server>, entity: &Entity, sound: &str) {
    let Some(name) = garnet_protocol::Identifier::parse(sound) else {
        return;
    };
    let registry_id = server.data.registries.id_of("sound_event", &name.to_string());
    server.broadcast_near(
        ChunkPos::from_block(entity.x.floor() as i32, entity.z.floor() as i32),
        &cb::Sound {
            name,
            registry_id,
            source: cb::SoundSource::Neutral,
            x: entity.x,
            y: entity.y,
            z: entity.z,
            volume: 1.0,
            pitch: 1.0,
            seed: rand::random(),
        },
        None,
    );
}

/// Takes what was thrown out of the hand.
fn spend_held(server: &Arc<Server>, player: &Arc<Player>, count: i32) {
    if matches!(player.lock().game_mode, garnet_protocol::packets::play::GameMode::Creative) {
        return;
    }
    let _ = server;
    {
        let mut s = player.lock();
        let slot = s.held_slot;
        let stack = s.inventory.held_mut(slot);
        stack.count -= count;
        if stack.count <= 0 {
            *stack = ItemStack::EMPTY;
        }
    }
    crate::items::sync_inventory(player);
}

fn swing(server: &Arc<Server>, player: &Arc<Player>) {
    let chunk = player.lock().chunk();
    server.broadcast_near(
        chunk,
        &cb::SwingAnimation {
            entity_id: player.entity_id,
            off_hand: false,
        },
        Some(player.uuid),
    );
}
