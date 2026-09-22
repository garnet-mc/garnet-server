//! `/summon`, `/ride`, `/data`, and entity selectors (`@e[type=...]`) for
//! `/kill` and friends.

use crate::commands::{Command, CommandRegistry, CommandSender, Handler};
use crate::player::Player;
use crate::server::Server;
use crate::vanilla_commands::resolve_targets;
use crate::world_entities::{self, Entity};
use garnet_protocol::Text;
use std::sync::Arc;

pub fn register(registry: &CommandRegistry) {
    let add = |name: &str, description: &str, usage: &str, handler: Handler| {
        registry.register(Command {
            name: name.into(),
            description: description.into(),
            usage: usage.into(),
            permission: None,
            handler: Some(handler),
            mod_id: None,
        });
    };
    add("summon", "Spawns an entity", "/summon <type> [x y z] [{CustomName:\"...\"}]", cmd_summon);
    add("ride", "Mounts or dismounts", "/ride <target> mount <vehicle> | /ride <target> dismount", cmd_ride);
    add("data", "Reads block and entity data", "/data get block <x> <y> <z> | /data get entity <target>", cmd_data);
}

/// Something a selector can point at.
pub enum Target {
    Player(Arc<Player>),
    Entity(i32),
}

impl Target {
    pub fn entity_id(&self, _server: &Server) -> i32 {
        match self {
            Target::Player(p) => p.entity_id,
            Target::Entity(id) => *id,
        }
    }
}

/// `@e` and `@e[type=zombie,limit=1,sort=nearest,distance=..10]`; anything
/// else falls back to the player selectors.
pub fn resolve_entities(server: &Server, sender: &CommandSender, arg: &str) -> Result<Vec<Target>, String> {
    let (selector, filters) = match arg.split_once('[') {
        Some((s, f)) => (s, f.trim_end_matches(']')),
        None => (arg, ""),
    };
    if selector != "@e" {
        return Ok(resolve_targets(server, sender, arg)?.into_iter().map(Target::Player).collect());
    }
    let mut type_filter: Option<(String, bool)> = None;
    let mut limit = usize::MAX;
    let mut nearest = false;
    let mut max_distance: Option<f64> = None;
    let mut name_filter: Option<String> = None;
    for pair in filters.split(',').filter(|s| !s.is_empty()) {
        let (key, value) = pair.split_once('=').ok_or_else(|| format!("Bad selector option '{pair}'."))?;
        match key.trim() {
            "type" => {
                let (negated, v) = match value.strip_prefix('!') {
                    Some(v) => (true, v),
                    None => (false, value),
                };
                let full = if v.contains(':') { v.to_owned() } else { format!("minecraft:{v}") };
                type_filter = Some((full, negated));
            }
            "limit" => limit = value.parse().map_err(|_| "limit must be a number.".to_owned())?,
            "sort" => nearest = value == "nearest",
            "distance" => {
                let upper = value.split("..").last().unwrap_or(value);
                max_distance = upper.parse().ok();
            }
            "name" => name_filter = Some(value.trim_matches('"').to_owned()),
            _ => {}
        }
    }
    let origin = sender.player().map(|p| {
        let s = p.lock();
        (s.x, s.y, s.z)
    });
    let mut found: Vec<(f64, Target)> = Vec::new();
    let matches_type = |kind: &str| match &type_filter {
        None => true,
        Some((t, negated)) => (kind == t) != *negated,
    };
    if matches_type("minecraft:player") {
        for p in server.online_players() {
            if name_filter.as_ref().is_some_and(|n| n != p.name()) {
                continue;
            }
            let d = origin.map(|(x, y, z)| {
                let s = p.lock();
                ((s.x - x).powi(2) + (s.y - y).powi(2) + (s.z - z).powi(2)).sqrt()
            });
            found.push((d.unwrap_or(0.0), Target::Player(p)));
        }
    }
    {
        let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
        for e in entities.by_id.values() {
            if !matches_type(&e.kind) {
                continue;
            }
            if name_filter.as_ref().is_some_and(|n| e.custom_name.as_deref() != Some(n.as_str())) {
                continue;
            }
            let d = origin.map(|(x, y, z)| ((e.x - x).powi(2) + (e.y - y).powi(2) + (e.z - z).powi(2)).sqrt());
            found.push((d.unwrap_or(0.0), Target::Entity(e.id)));
        }
    }
    if let Some(max) = max_distance {
        found.retain(|(d, _)| *d <= max);
    }
    if nearest {
        found.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }
    Ok(found.into_iter().take(limit).map(|(_, t)| t).collect())
}

fn coord(arg: &str, base: f64) -> Result<f64, String> {
    if let Some(rest) = arg.strip_prefix('~') {
        if rest.is_empty() {
            return Ok(base);
        }
        return rest.parse::<f64>().map(|d| base + d).map_err(|_| format!("'{arg}' is not a coordinate."));
    }
    arg.parse::<f64>().map_err(|_| format!("'{arg}' is not a coordinate."))
}

fn cmd_summon(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let kind = args.first().ok_or("Usage: /summon <type> [x y z] [{CustomName:\"...\"}]")?;
    if kind == "player" || kind == "minecraft:player" {
        return Err("Players cannot be summoned.".into());
    }
    let base = sender.player().map(|p| {
        let s = p.lock();
        (s.x, s.y, s.z)
    });
    let (x, y, z) = if args.len() >= 4 {
        let b = base.unwrap_or((0.0, 0.0, 0.0));
        (coord(&args[1], b.0)?, coord(&args[2], b.1)?, coord(&args[3], b.2)?)
    } else {
        base.ok_or("Give a position.")?
    };
    let mut entity = world_entities::new_entity(server, kind, x, y, z).ok_or_else(|| format!("Unknown entity type '{kind}'."))?;
    // A little NBT: only CustomName and NoGravity are read for now.
    let nbt = args.iter().skip(4).cloned().collect::<Vec<_>>().join(" ");
    if let Some(name) = nbt.split("CustomName:").nth(1) {
        let name = name.trim_start().trim_start_matches('\'').trim_start_matches('"');
        let end = name.find(['"', '\'']).unwrap_or(name.len());
        entity.custom_name = Some(name[..end].to_owned());
    }
    if nbt.contains("NoGravity:1") || nbt.contains("NoGravity:true") {
        entity.no_gravity = true;
    }
    entity.persistent = true;
    entity.health = crate::mobs::health_of(&entity.kind);
    let id = world_entities::spawn(server, entity);
    sender.reply(Text::new(format!("Summoned new {} (id {id}).", kind.trim_start_matches("minecraft:"))));
    Ok(())
}

fn cmd_ride(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /ride <target> mount <vehicle> | /ride <target> dismount";
    let targets = resolve_entities(server, sender, args.first().ok_or(usage)?)?;
    let target = targets.into_iter().next().ok_or("No entity matched.")?;
    match args.get(1).map(String::as_str) {
        Some("mount") => {
            let vehicle = resolve_entities(server, sender, args.get(2).ok_or(usage)?)?.into_iter().next().ok_or("No vehicle matched.")?;
            let Target::Entity(vehicle_id) = vehicle else { return Err("Players cannot be ridden.".into()) };
            world_entities::mount(server, target.entity_id(server), vehicle_id)?;
            sender.reply(Text::new("Mounted."));
        }
        Some("dismount") => {
            if !world_entities::dismount(server, target.entity_id(server)) {
                return Err("That entity is not riding anything.".into());
            }
            sender.reply(Text::new("Dismounted."));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

/// Kills or removes whatever a selector matched; used by `/kill`.
pub fn kill_targets(server: &Arc<Server>, sender: &CommandSender, targets: Vec<Target>) -> usize {
    let mut count = 0;
    for target in targets {
        match target {
            Target::Player(p) => {
                crate::vanilla_commands::kill_player(server, &p, Text::new(format!("{} was killed", p.name())));
                count += 1;
            }
            Target::Entity(id) => {
                if world_entities::despawn(server, id).is_some() {
                    count += 1;
                }
            }
        }
    }
    let _ = sender;
    count
}

fn cmd_data(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /data get block <x> <y> <z> | /data get entity <target>";
    if args.first().map(String::as_str) != Some("get") {
        return Err(usage.into());
    }
    match args.get(1).map(String::as_str) {
        Some("block") => {
            if args.len() < 5 {
                return Err(usage.into());
            }
            let base = sender.player().map(|p| {
                let s = p.lock();
                (s.x, s.y, s.z)
            }).unwrap_or((0.0, 0.0, 0.0));
            let pos = garnet_protocol::BlockPos::new(
                coord(&args[2], base.0)?.floor() as i32,
                coord(&args[3], base.1)?.floor() as i32,
                coord(&args[4], base.2)?.floor() as i32,
            );
            let state = server.world().get_block(pos).unwrap_or(0);
            let blocks = &server.data.blocks;
            let name = blocks.block_of_state(state as i32).map(|b| b.name.clone()).unwrap_or_default();
            let props = blocks.state(state as i32).map(|s| s.properties.clone()).unwrap_or_default();
            let props: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let block_entity = server.world().chunk_mut(pos.chunk()).ok().and_then(|c| {
                c.block_entities
                    .iter()
                    .find(|be| be.get_i32("x") == Some(pos.x) && be.get_i32("y") == Some(pos.y) && be.get_i32("z") == Some(pos.z))
                    .map(|be| garnet_protocol::nbt::NbtTag::Compound(be.clone()).to_json().to_string())
            });
            sender.reply(Text::new(format!(
                "{name}[{}] at {}, {}, {}{}",
                props.join(","),
                pos.x,
                pos.y,
                pos.z,
                block_entity.map(|b| format!(" with block entity {b}")).unwrap_or_default()
            )));
        }
        Some("entity") => {
            let target = resolve_entities(server, sender, args.get(2).ok_or(usage)?)?.into_iter().next().ok_or("No entity matched.")?;
            let line = match target {
                Target::Player(p) => {
                    let s = p.lock();
                    format!(
                        "{}: player at {:.2}, {:.2}, {:.2}, health {:.1}, food {}, level {} ({:.0}%), mode {}, tags {:?}",
                        p.name(),
                        s.x,
                        s.y,
                        s.z,
                        s.health,
                        s.food,
                        s.xp_level,
                        s.xp_progress * 100.0,
                        s.game_mode.name(),
                        s.tags
                    )
                }
                Target::Entity(id) => {
                    let entities = server.entities.lock().unwrap_or_else(|e| e.into_inner());
                    let e: &Entity = entities.by_id.get(&id).ok_or("That entity is gone.")?;
                    format!(
                        "{} (id {}, {}) at {:.2}, {:.2}, {:.2}{}{}{}",
                        e.kind,
                        e.id,
                        e.uuid,
                        e.x,
                        e.y,
                        e.z,
                        e.custom_name.as_ref().map(|n| format!(", named \"{n}\"")).unwrap_or_default(),
                        e.item.as_ref().map(|i| format!(", item {} x{}", crate::items::item_name(server, i.item), i.count)).unwrap_or_default(),
                        if e.passengers.is_empty() { String::new() } else { format!(", passengers {:?}", e.passengers) }
                    )
                }
            };
            sender.reply(Text::new(line));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}
