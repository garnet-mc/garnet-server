//! The vanilla command set on top of the basics in `commands.rs`: chat and
//! titles, sound and particles, world state (weather, difficulty, game
//! rules, spawn, border, tick rate), blocks (setblock, fill, clone),
//! players (xp, kill, damage, effects, attributes, spectate, transfer) and
//! `execute`. Scoreboard, teams, boss bars and tags live in
//! `board_commands.rs`.
//!
//! Commands that need systems the server does not have yet (inventories,
//! loot, mobs, data packs) are registered too, so `/help` and tab
//! completion are complete, and they say plainly what they are waiting on.

use crate::commands::{find_player, teleport, Command, CommandRegistry, CommandSender, Handler};
use crate::player::{ActiveEffect, Player};
use crate::rules::{now_ms, GAME_RULES};
use crate::server::Server;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::types::{BlockPos, Identifier};
use garnet_protocol::Text;
use std::collections::BTreeMap;
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
    // Chat.
    add("me", "Describes an action", "/me <action>", cmd_me);
    add("tell", "Private message", "/tell <player> <message>", cmd_tell);
    add("w", "Private message", "/w <player> <message>", cmd_tell);
    add("teammsg", "Messages your team", "/teammsg <message>", crate::board_commands::cmd_teammsg);
    add("tm", "Messages your team", "/tm <message>", crate::board_commands::cmd_teammsg);
    add("tellraw", "Sends JSON text", "/tellraw <targets> <json>", cmd_tellraw);
    add("title", "Shows a title", "/title <targets> <clear|reset|title|subtitle|actionbar|times> ...", cmd_title);
    add("playsound", "Plays a sound", "/playsound <sound> [source] [targets] [x y z] [volume] [pitch]", cmd_playsound);
    add("stopsound", "Stops sounds", "/stopsound <targets> [source] [sound]", cmd_stopsound);
    add("particle", "Shows particles", "/particle <name> [x y z] [dx dy dz] [speed] [count]", cmd_particle);
    // World.
    add("weather", "Sets the weather", "/weather <clear|rain|thunder> [seconds]", cmd_weather);
    add("difficulty", "Shows or sets the difficulty", "/difficulty [peaceful|easy|normal|hard]", cmd_difficulty);
    add("defaultgamemode", "Sets the game mode for new players", "/defaultgamemode <mode>", cmd_defaultgamemode);
    add("gamerule", "Shows or sets a game rule", "/gamerule <rule> [value]", cmd_gamerule);
    add("setworldspawn", "Sets the world spawn", "/setworldspawn [x y z] [yaw]", cmd_setworldspawn);
    add("spawnpoint", "Sets a player's spawn point", "/spawnpoint [targets] [x y z]", cmd_spawnpoint);
    add("worldborder", "Manages the world border", "/worldborder <get|set|add|center|damage|warning> ...", cmd_worldborder);
    add("tick", "Controls the tick rate", "/tick <query|rate|freeze|unfreeze|step|sprint> ...", cmd_tick);
    add("save-off", "Turns autosave off", "/save-off", cmd_save_off);
    add("save-on", "Turns autosave on", "/save-on", cmd_save_on);
    add("reload", "Reloads lists, permissions and mods", "/reload", cmd_reload);
    add("setidletimeout", "Kicks idle players after some minutes", "/setidletimeout <minutes>", cmd_setidletimeout);
    add("setblock", "Places a block", "/setblock <x> <y> <z> <block>", cmd_setblock);
    add("fill", "Fills a region with a block", "/fill <x1> <y1> <z1> <x2> <y2> <z2> <block> [replace|hollow|outline|keep]", cmd_fill);
    add("clone", "Copies a region", "/clone <x1> <y1> <z1> <x2> <y2> <z2> <x> <y> <z>", cmd_clone);
    add("spreadplayers", "Spreads players around a point", "/spreadplayers <x> <z> <distance> <range> <targets>", cmd_spreadplayers);
    add("random", "Rolls a number", "/random [roll|value] <min..max>", cmd_random);
    // Players.
    add("teleport", "Teleports", "/teleport (same as /tp)", cmd_teleport);
    add("rotate", "Turns a player", "/rotate <player> <yaw> <pitch>", cmd_rotate);
    add("xp", "Gives or sets experience", "/xp <add|set|query> <targets> [amount] [levels|points]", cmd_xp);
    add("experience", "Gives or sets experience", "/experience <add|set|query> <targets> [amount] [levels|points]", cmd_xp);
    add("kill", "Kills a player", "/kill [targets]", cmd_kill);
    add("damage", "Damages a player", "/damage <target> <amount>", cmd_damage);
    add("effect", "Gives or clears effects", "/effect <give|clear> <targets> [effect] [seconds|infinite] [amplifier] [hideParticles]", cmd_effect);
    add("attribute", "Reads or sets an attribute", "/attribute <target> <attribute> <get|base get|base set <value>>", cmd_attribute);
    add("spectate", "Spectates a player", "/spectate [target] [player]", cmd_spectate);
    add("transfer", "Sends players to another server", "/transfer <host> [port] [targets]", cmd_transfer);
    add("banlist", "Lists bans", "/banlist [players|ips]", cmd_banlist);
    add("pardon-ip", "Removes an IP ban", "/pardon-ip <ip>", cmd_pardon_ip);
    add("execute", "Runs a command as or at a player", "/execute (as|at|positioned|if entity|unless entity) ... run <command>", cmd_execute);
    // Waiting on systems that are not here yet.
    for (name, needs) in NOT_YET {
        registry.register(Command {
            name: (*name).into(),
            description: format!("Not available yet ({needs})"),
            usage: format!("/{name}"),
            permission: None,
            handler: Some(cmd_not_yet),
            mod_id: None,
        });
    }
    crate::board_commands::register(registry);
}

/// Commands the server accepts but cannot do until the named system lands.
const NOT_YET: &[(&str, &str)] = &[
    ("give", "inventories"),
    ("clear", "inventories"),
    ("item", "inventories"),
    ("enchant", "inventories"),
    ("loot", "loot tables"),
    ("recipe", "recipes"),
    ("advancement", "advancements"),
    ("summon", "mobs and entities"),
    ("ride", "mobs and entities"),
    ("locate", "structures"),
    ("place", "structures"),
    ("fillbiome", "biome editing"),
    ("function", "data packs"),
    ("datapack", "data packs"),
    ("schedule", "data packs"),
    ("return", "data packs"),
    ("data", "block and entity data"),
    ("debug", "the profiler"),
    ("jfr", "the profiler"),
    ("perf", "the profiler"),
];

fn cmd_not_yet(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    let _ = server;
    let name = sender.name();
    let _ = name;
    Err("This command is registered but not available yet: it needs a system the server does not have (see /help for which). It is on the roadmap.".into())
}

// ---------- shared helpers ----------

/// `@a`, `@p`, `@r`, `@s` or a player name.
pub fn resolve_targets(server: &Server, sender: &CommandSender, arg: &str) -> Result<Vec<Arc<Player>>, String> {
    let selector = arg.split('[').next().unwrap_or(arg);
    match selector {
        "@a" | "@e" => Ok(server.online_players()),
        "@s" => sender.player().cloned().map(|p| vec![p]).ok_or_else(|| "@s only works for players.".to_owned()),
        "@p" => {
            let (x, y, z) = sender_position(sender).unwrap_or((0.0, 64.0, 0.0));
            let mut players = server.online_players();
            players.sort_by(|a, b| {
                let da = distance(&a.lock(), x, y, z);
                let db = distance(&b.lock(), x, y, z);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            });
            Ok(players.into_iter().take(1).collect())
        }
        "@r" => {
            let players = server.online_players();
            if players.is_empty() {
                return Ok(Vec::new());
            }
            let i = rand::random_range(0..players.len());
            Ok(vec![players[i].clone()])
        }
        name => find_player(server, name).map(|p| vec![p]),
    }
}

fn distance(state: &crate::player::PlayerState, x: f64, y: f64, z: f64) -> f64 {
    let (dx, dy, dz) = (state.x - x, state.y - y, state.z - z);
    dx * dx + dy * dy + dz * dz
}

fn sender_position(sender: &CommandSender) -> Option<(f64, f64, f64)> {
    let p = sender.player()?;
    let s = p.lock();
    Some((s.x, s.y, s.z))
}

/// One coordinate, absolute or `~` relative to `base`.
fn coord(arg: &str, base: f64) -> Result<f64, String> {
    if let Some(rest) = arg.strip_prefix('~') {
        if rest.is_empty() {
            return Ok(base);
        }
        return rest.parse::<f64>().map(|d| base + d).map_err(|_| format!("'{arg}' is not a coordinate."));
    }
    arg.parse::<f64>().map_err(|_| format!("'{arg}' is not a coordinate."))
}

/// Three coordinates starting at `args[i]`, relative to the sender.
fn position(sender: &CommandSender, args: &[String], i: usize) -> Result<(f64, f64, f64), String> {
    let base = sender_position(sender).unwrap_or((0.0, 0.0, 0.0));
    let get = |k: usize| args.get(i + k).map(String::as_str).ok_or_else(|| "Expected x y z.".to_owned());
    Ok((coord(get(0)?, base.0)?, coord(get(1)?, base.1)?, coord(get(2)?, base.2)?))
}

fn block_position(sender: &CommandSender, args: &[String], i: usize) -> Result<BlockPos, String> {
    let (x, y, z) = position(sender, args, i)?;
    Ok(BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32))
}

fn number<T: std::str::FromStr>(arg: Option<&String>, what: &str) -> Result<T, String> {
    arg.ok_or_else(|| format!("Expected {what}."))?
        .parse::<T>()
        .map_err(|_| format!("'{}' is not a valid {what}.", arg.unwrap()))
}

fn rest(args: &[String], from: usize) -> String {
    args.iter().skip(from).cloned().collect::<Vec<_>>().join(" ")
}

/// A block argument like `stone` or `oak_log[axis=y]` to a state id.
pub fn parse_block(server: &Server, arg: &str) -> Result<u32, String> {
    let (name, props) = match arg.split_once('[') {
        Some((n, p)) => (n, p.trim_end_matches(']')),
        None => (arg, ""),
    };
    let mut wanted = BTreeMap::new();
    for pair in props.split(',').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').ok_or_else(|| format!("Bad block property '{pair}'."))?;
        wanted.insert(k.trim().to_owned(), v.trim().to_owned());
    }
    let blocks = &server.data.blocks;
    let id = if wanted.is_empty() {
        blocks.default_state(name)
    } else {
        blocks.state_with(name, &wanted)
    };
    id.map(|id| id as u32).ok_or_else(|| format!("Unknown block '{arg}'."))
}

fn world_dir(server: &Server) -> std::path::PathBuf {
    server.root.join(&server.config().world.name)
}

// ---------- chat ----------

fn cmd_me(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /me <action>".into());
    }
    server.broadcast_chat(Text::new(format!("* {} {}", sender.name(), rest(args, 0))));
    Ok(())
}

fn cmd_tell(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("Usage: /tell <player> <message>".into());
    }
    let message = rest(args, 1);
    for target in resolve_targets(server, sender, &args[0])? {
        target.send(&cb::SystemChat {
            content: Text::new(format!("{} whispers to you: {message}", sender.name())).color("gray").italic(),
            overlay: false,
        });
        sender.reply(Text::new(format!("You whisper to {}: {message}", target.name())).color("gray").italic());
    }
    Ok(())
}

fn cmd_tellraw(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("Usage: /tellraw <targets> <json>".into());
    }
    let json: serde_json::Value = serde_json::from_str(&rest(args, 1)).map_err(|e| format!("Bad JSON text: {e}"))?;
    let text = Text::from_json(&json);
    for target in resolve_targets(server, sender, &args[0])? {
        target.send(&cb::SystemChat {
            content: text.clone(),
            overlay: false,
        });
    }
    Ok(())
}

fn cmd_title(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /title <targets> <clear|reset|title|subtitle|actionbar|times> ...";
    if args.len() < 2 {
        return Err(usage.into());
    }
    let targets = resolve_targets(server, sender, &args[0])?;
    let text = || -> Result<Text, String> {
        let raw = rest(args, 2);
        if raw.trim_start().starts_with('{') || raw.trim_start().starts_with('"') || raw.trim_start().starts_with('[') {
            let json: serde_json::Value = serde_json::from_str(&raw).map_err(|e| format!("Bad JSON text: {e}"))?;
            Ok(Text::from_json(&json))
        } else {
            Ok(Text::legacy(&raw))
        }
    };
    match args[1].as_str() {
        "clear" => targets.iter().for_each(|t| t.send(&cb::ClearTitles { reset: false })),
        "reset" => targets.iter().for_each(|t| t.send(&cb::ClearTitles { reset: true })),
        "title" => {
            let text = text()?;
            targets.iter().for_each(|t| t.send(&cb::SetTitleText { text: text.clone() }));
        }
        "subtitle" => {
            let text = text()?;
            targets.iter().for_each(|t| t.send(&cb::SetSubtitleText { text: text.clone() }));
        }
        "actionbar" => {
            let text = text()?;
            targets.iter().for_each(|t| t.send(&cb::SetActionBarText { text: text.clone() }));
        }
        "times" => {
            let fade_in: i32 = number(args.get(2), "fade in ticks")?;
            let stay: i32 = number(args.get(3), "stay ticks")?;
            let fade_out: i32 = number(args.get(4), "fade out ticks")?;
            targets.iter().for_each(|t| t.send(&cb::SetTitleAnimation { fade_in, stay, fade_out }));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

fn cmd_playsound(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /playsound <sound> [source] [targets] [x y z] [volume] [pitch]".into());
    }
    let name = Identifier::parse(&args[0]).ok_or_else(|| format!("'{}' is not a sound name.", args[0]))?;
    let registry_id = server.data.registries.id_of("sound_event", &name.to_string());
    let source = args.get(1).map(String::as_str).map(cb::SoundSource::parse).unwrap_or(Some(cb::SoundSource::Master));
    let source = source.ok_or_else(|| format!("'{}' is not a sound source.", args[1]))?;
    let targets = match args.get(2) {
        Some(t) => resolve_targets(server, sender, t)?,
        None => sender.player().cloned().map(|p| vec![p]).ok_or("Say who should hear it.")?,
    };
    let volume: f32 = args.get(6).map(|v| v.parse().unwrap_or(1.0)).unwrap_or(1.0);
    let pitch: f32 = args.get(7).map(|v| v.parse().unwrap_or(1.0)).unwrap_or(1.0);
    for target in targets {
        let (x, y, z) = if args.len() >= 6 {
            let base = {
                let s = target.lock();
                (s.x, s.y, s.z)
            };
            (coord(&args[3], base.0)?, coord(&args[4], base.1)?, coord(&args[5], base.2)?)
        } else {
            let s = target.lock();
            (s.x, s.y, s.z)
        };
        target.send(&cb::Sound {
            name: name.clone(),
            registry_id,
            source,
            x,
            y,
            z,
            volume,
            pitch,
            seed: rand::random(),
        });
    }
    Ok(())
}

fn cmd_stopsound(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /stopsound <targets> [source] [sound]".into());
    }
    let source = match args.get(1).map(String::as_str) {
        None | Some("*") => None,
        Some(s) => Some(cb::SoundSource::parse(s).ok_or_else(|| format!("'{s}' is not a sound source."))?),
    };
    let name = match args.get(2) {
        Some(n) => Some(Identifier::parse(n).ok_or_else(|| format!("'{n}' is not a sound name."))?),
        None => None,
    };
    for target in resolve_targets(server, sender, &args[0])? {
        target.send(&cb::StopSound {
            source,
            name: name.clone(),
        });
    }
    Ok(())
}

fn cmd_particle(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /particle <name> [x y z] [dx dy dz] [speed] [count]".into());
    }
    let full = if args[0].contains(':') { args[0].clone() } else { format!("minecraft:{}", args[0]) };
    let particle_type = server
        .data
        .registries
        .id_of("particle_type", &full)
        .ok_or_else(|| format!("Unknown particle '{}'.", args[0]))?;
    // Particles that carry data (dust colours, blocks, items) are not supported yet.
    const NEEDS_DATA: &[&str] = &[
        "minecraft:dust", "minecraft:dust_color_transition", "minecraft:block", "minecraft:block_marker", "minecraft:falling_dust",
        "minecraft:dust_pillar", "minecraft:item", "minecraft:entity_effect", "minecraft:sculk_charge", "minecraft:shriek",
        "minecraft:vibration", "minecraft:trail", "minecraft:tinted_leaves", "minecraft:block_crumble",
    ];
    if NEEDS_DATA.contains(&full.as_str()) {
        return Err(format!("{} needs extra data, which /particle cannot send yet.", args[0]));
    }
    let (x, y, z) = if args.len() >= 4 { position(sender, args, 1)? } else { sender_position(sender).ok_or("Give a position.")? };
    let spread = if args.len() >= 7 {
        (number::<f32>(args.get(4), "dx")?, number::<f32>(args.get(5), "dy")?, number::<f32>(args.get(6), "dz")?)
    } else {
        (0.0, 0.0, 0.0)
    };
    let max_speed: f32 = args.get(7).map(|v| v.parse().unwrap_or(0.0)).unwrap_or(0.0);
    let count: i32 = args.get(8).map(|v| v.parse().unwrap_or(1)).unwrap_or(1);
    let packet = cb::Particles {
        particle_type,
        override_limiter: false,
        always_show: false,
        x,
        y,
        z,
        spread,
        max_speed,
        count,
    };
    server.broadcast_near(BlockPos::new(x as i32, y as i32, z as i32).chunk(), &packet, None);
    Ok(())
}

// ---------- world ----------

fn cmd_weather(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /weather <clear|rain|thunder> [seconds]";
    let kind = args.first().ok_or(usage)?.as_str();
    let seconds: i64 = match args.get(1) {
        Some(s) => number(Some(s), "seconds")?,
        None => 0,
    };
    let (raining, thundering) = match kind {
        "clear" => (false, false),
        "rain" => (true, false),
        "thunder" => (true, true),
        _ => return Err(usage.into()),
    };
    set_weather(server, raining, thundering, seconds * 20);
    sender.reply(Text::new(format!("Set the weather to {kind}.")));
    Ok(())
}

/// Changes the weather and tells every client.
pub fn set_weather(server: &Arc<Server>, raining: bool, thundering: bool, ticks: i64) {
    {
        let mut rules = server.rules.write().unwrap_or_else(|e| e.into_inner());
        rules.weather.raining = raining;
        rules.weather.thundering = thundering;
        rules.weather.ticks_left = ticks;
    }
    for player in server.online_players() {
        send_weather(server, &player);
    }
    save_rules(server);
}

pub fn send_weather(server: &Server, player: &Player) {
    let weather = server.rules.read().unwrap_or_else(|e| e.into_inner()).weather.clone();
    player.send(&cb::GameEvent {
        event: if weather.raining { cb::GameEventKind::BeginRaining } else { cb::GameEventKind::EndRaining },
        value: 0.0,
    });
    player.send(&cb::GameEvent {
        event: cb::GameEventKind::RainLevelChange,
        value: if weather.raining { 1.0 } else { 0.0 },
    });
    player.send(&cb::GameEvent {
        event: cb::GameEventKind::ThunderLevelChange,
        value: if weather.thundering { 1.0 } else { 0.0 },
    });
}

pub fn save_rules(server: &Server) {
    let rules = server.rules.read().unwrap_or_else(|e| e.into_inner()).clone();
    rules.save(&world_dir(server));
}

fn cmd_difficulty(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let Some(level) = args.first() else {
        let d = server.rules.read().unwrap_or_else(|e| e.into_inner()).difficulty.clone();
        sender.reply(Text::new(format!("The difficulty is {d}.")));
        return Ok(());
    };
    if !matches!(level.as_str(), "peaceful" | "easy" | "normal" | "hard") {
        return Err("Usage: /difficulty [peaceful|easy|normal|hard]".into());
    }
    let id = {
        let mut rules = server.rules.write().unwrap_or_else(|e| e.into_inner());
        if rules.difficulty_locked {
            return Err("The difficulty is locked.".into());
        }
        rules.difficulty = level.clone();
        rules.difficulty_id()
    };
    server.broadcast(&cb::ChangeDifficulty { difficulty: id, locked: false });
    save_rules(server);
    sender.reply(Text::new(format!("The difficulty has been set to {level}.")));
    Ok(())
}

fn cmd_defaultgamemode(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let mode = args.first().and_then(|m| GameMode::parse(m)).ok_or("Usage: /defaultgamemode <survival|creative|adventure|spectator>")?;
    server.rules.write().unwrap_or_else(|e| e.into_inner()).default_game_mode = Some(mode.name().to_owned());
    save_rules(server);
    sender.reply(Text::new(format!("The default game mode is now {}.", mode.name())));
    Ok(())
}

fn cmd_gamerule(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let Some(rule) = args.first() else {
        let rules = server.rules.read().unwrap_or_else(|e| e.into_inner());
        let mut line = String::new();
        for (name, _) in GAME_RULES {
            line.push_str(&format!("{name}={} ", rules.game_rule(name)));
        }
        sender.reply(Text::new(line.trim_end().to_owned()));
        return Ok(());
    };
    let Some(value) = args.get(1) else {
        let value = server.rules.read().unwrap_or_else(|e| e.into_inner()).game_rule(rule);
        if value.is_empty() {
            return Err(format!("Unknown game rule '{rule}'."));
        }
        sender.reply(Text::new(format!("Game rule {rule} is currently set to: {value}")));
        return Ok(());
    };
    let value = crate::rules::WorldRules::validate_game_rule(rule, value)?;
    server
        .rules
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .game_rules
        .insert(rule.clone(), value.clone());
    apply_game_rules(server);
    save_rules(server);
    sender.reply(Text::new(format!("Game rule {rule} is now set to: {value}")));
    Ok(())
}

/// Pushes the rules the server acts on into the systems that use them and
/// tells clients the values they care about.
pub fn apply_game_rules(server: &Arc<Server>) {
    let (daylight, reduced_debug) = {
        let rules = server.rules.read().unwrap_or_else(|e| e.into_inner());
        (rules.game_rule_bool("doDaylightCycle"), rules.game_rule_bool("reducedDebugInfo"))
    };
    server.config.write().unwrap_or_else(|e| e.into_inner()).world.daylight_cycle = daylight;
    let _ = reduced_debug;
    let packet = game_rule_packet(server);
    server.broadcast(&packet);
}

pub fn game_rule_packet(server: &Server) -> cb::GameRuleValues {
    let rules = server.rules.read().unwrap_or_else(|e| e.into_inner());
    let values = rules
        .game_rules
        .iter()
        .filter_map(|(name, value)| Some((Identifier::minecraft(&to_snake_case(name)), value.clone())))
        .filter(|(id, _)| server.data.registries.id_of("game_rule", &id.to_string()).is_some())
        .collect();
    cb::GameRuleValues { values }
}

fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn cmd_setworldspawn(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let pos = if args.len() >= 3 {
        block_position(sender, args, 0)?
    } else {
        let (x, y, z) = sender_position(sender).ok_or("Give a position.")?;
        BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    };
    let yaw: f32 = args.get(3).map(|v| v.parse().unwrap_or(0.0)).unwrap_or(0.0);
    server.world().settings.spawn = pos;
    server.rules.write().unwrap_or_else(|e| e.into_inner()).spawn_yaw = yaw;
    save_rules(server);
    server.broadcast(&cb::SetDefaultSpawnPosition {
        dimension: Identifier::minecraft("overworld"),
        position: pos,
        yaw,
        pitch: 0.0,
    });
    sender.reply(Text::new(format!("Set the world spawn point to {}, {}, {}.", pos.x, pos.y, pos.z)));
    Ok(())
}

fn cmd_spawnpoint(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let targets = match args.first() {
        Some(t) => resolve_targets(server, sender, t)?,
        None => sender.player().cloned().map(|p| vec![p]).ok_or("Say whose spawn point to set.")?,
    };
    let explicit = if args.len() >= 4 { Some(block_position(sender, args, 1)?) } else { None };
    for target in &targets {
        let pos = explicit.unwrap_or_else(|| {
            let s = target.lock();
            BlockPos::new(s.x.floor() as i32, s.y.floor() as i32, s.z.floor() as i32)
        });
        target.lock().spawn_point = Some(pos);
        sender.reply(Text::new(format!("Set spawn point to {}, {}, {} for {}.", pos.x, pos.y, pos.z, target.name())));
    }
    Ok(())
}

fn cmd_worldborder(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /worldborder <get|set <size> [seconds]|add <blocks> [seconds]|center <x> <z>|damage amount <n>|damage buffer <n>|warning distance <n>|warning time <n>>";
    let sub = args.first().ok_or(usage)?.as_str();
    let mut rules = server.rules.write().unwrap_or_else(|e| e.into_inner());
    let border = &mut rules.border;
    match sub {
        "get" => {
            let size = border.size;
            drop(rules);
            sender.reply(Text::new(format!("The world border is currently {size:.0} blocks wide.")));
            return Ok(());
        }
        "set" | "add" => {
            let amount: f64 = number(args.get(1), "size")?;
            let seconds: f64 = args.get(2).map(|v| v.parse().unwrap_or(0.0)).unwrap_or(0.0);
            let target = if sub == "set" { amount } else { border.size + amount };
            let target = target.clamp(1.0, 59_999_968.0);
            let old = border.size;
            if seconds <= 0.0 {
                border.size = target;
                border.target_size = target;
                border.lerp_end_ms = 0;
                let packet = cb::SetBorderSize { size: target };
                drop(rules);
                server.broadcast(&packet);
            } else {
                border.target_size = target;
                border.lerp_end_ms = now_ms() + (seconds * 1000.0) as i64;
                let packet = cb::SetBorderLerpSize {
                    old_size: old,
                    new_size: target,
                    lerp_time_ms: (seconds * 1000.0) as i64,
                };
                drop(rules);
                server.broadcast(&packet);
            }
            sender.reply(Text::new(format!("Set the world border to {target:.0} blocks wide.")));
        }
        "center" => {
            let x: f64 = number(args.get(1), "x")?;
            let z: f64 = number(args.get(2), "z")?;
            border.center_x = x;
            border.center_z = z;
            drop(rules);
            server.broadcast(&cb::SetBorderCenter { x, z });
            sender.reply(Text::new(format!("Set the center of the world border to {x:.1}, {z:.1}.")));
        }
        "damage" => {
            let value: f64 = number(args.get(2), "amount")?;
            match args.get(1).map(String::as_str) {
                Some("amount") => border.damage_per_block = value,
                Some("buffer") => border.damage_buffer = value,
                _ => return Err(usage.into()),
            }
            drop(rules);
            sender.reply(Text::new("Updated the world border damage."));
        }
        "warning" => {
            let value: i32 = number(args.get(2), "value")?;
            match args.get(1).map(String::as_str) {
                Some("distance") => {
                    border.warning_blocks = value;
                    drop(rules);
                    server.broadcast(&cb::SetBorderWarningDistance { blocks: value });
                }
                Some("time") => {
                    border.warning_seconds = value;
                    drop(rules);
                    server.broadcast(&cb::SetBorderWarningDelay { seconds: value });
                }
                _ => return Err(usage.into()),
            }
            sender.reply(Text::new("Updated the world border warning."));
        }
        _ => return Err(usage.into()),
    }
    save_rules(server);
    Ok(())
}

/// The border as one packet, for players joining.
pub fn border_packet(server: &Server) -> cb::InitializeBorder {
    let rules = server.rules.read().unwrap_or_else(|e| e.into_inner());
    let b = &rules.border;
    cb::InitializeBorder {
        center_x: b.center_x,
        center_z: b.center_z,
        old_size: b.size,
        new_size: b.target_size,
        lerp_time_ms: b.remaining_lerp_ms(),
        absolute_max_size: 29_999_984,
        warning_blocks: b.warning_blocks,
        warning_time: b.warning_seconds,
    }
}

fn cmd_tick(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /tick <query|rate <n>|freeze|unfreeze|step [ticks]|sprint <ticks>|sprint stop>";
    let sub = args.first().ok_or(usage)?.as_str();
    let mut rules = server.rules.write().unwrap_or_else(|e| e.into_inner());
    match sub {
        "query" => {
            let (rate, frozen) = (rules.tick.rate, rules.tick.frozen);
            drop(rules);
            let (tps, ms) = server.tps();
            sender.reply(Text::new(format!(
                "Target rate {rate:.1} ticks per second{}; running at {tps:.1} ({ms:.1} ms per tick).",
                if frozen { ", frozen" } else { "" }
            )));
            return Ok(());
        }
        "rate" => {
            let rate: f32 = number(args.get(1), "rate")?;
            if !(1.0..=10000.0).contains(&rate) {
                return Err("The tick rate must be between 1 and 10000.".into());
            }
            rules.tick.rate = rate;
            sender.reply(Text::new(format!("Set the target tick rate to {rate:.1} per second.")));
        }
        "freeze" => {
            rules.tick.frozen = true;
            sender.reply(Text::new("The game is now frozen."));
        }
        "unfreeze" => {
            rules.tick.frozen = false;
            rules.tick.steps_left = 0;
            sender.reply(Text::new("The game is no longer frozen."));
        }
        "step" => {
            if !rules.tick.frozen {
                return Err("The game is not frozen; use /tick freeze first.".into());
            }
            let steps: i32 = args.get(1).map(|v| v.parse().unwrap_or(1)).unwrap_or(1);
            rules.tick.steps_left = steps.max(1);
            let packet = cb::TickingStep { steps: steps.max(1) };
            drop(rules);
            server.broadcast(&packet);
            sender.reply(Text::new(format!("Stepping {steps} tick(s).")));
            return Ok(());
        }
        "sprint" => {
            // Sprinting means running as fast as possible; the server is
            // already ticking on time, so this only reports.
            drop(rules);
            sender.reply(Text::new("The server is already keeping up; there is nothing to sprint through."));
            return Ok(());
        }
        _ => return Err(usage.into()),
    }
    let packet = cb::TickingState {
        tick_rate: rules.tick.rate,
        frozen: rules.tick.frozen,
    };
    drop(rules);
    server.broadcast(&packet);
    save_rules(server);
    Ok(())
}

pub fn tick_packet(server: &Server) -> cb::TickingState {
    let rules = server.rules.read().unwrap_or_else(|e| e.into_inner());
    cb::TickingState {
        tick_rate: rules.tick.rate,
        frozen: rules.tick.frozen,
    }
}

fn cmd_save_off(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    server.rules.write().unwrap_or_else(|e| e.into_inner()).autosave = false;
    sender.reply(Text::new("Automatic saving is now disabled."));
    Ok(())
}

fn cmd_save_on(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    server.rules.write().unwrap_or_else(|e| e.into_inner()).autosave = true;
    sender.reply(Text::new("Automatic saving is now enabled."));
    Ok(())
}

fn cmd_reload(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    server.reload_lists().map_err(|e| e.to_string())?;
    let result = server.mods.lock().unwrap_or_else(|e| e.into_inner()).reload();
    match result {
        Ok(()) => sender.reply(Text::new("Reloaded the ban, op and whitelist files, permissions and mods.")),
        Err(err) => sender.reply(Text::new(format!("Reloaded lists and permissions; mods failed to reload: {err:#}")).color("red")),
    }
    Ok(())
}

fn cmd_setidletimeout(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let minutes: u64 = number(args.first(), "minutes")?;
    server.config.write().unwrap_or_else(|e| e.into_inner()).server.afk_kick_minutes = minutes;
    sender.reply(Text::new(format!("The player idle timeout is now {minutes} minute(s).")));
    Ok(())
}

// ---------- blocks ----------

fn cmd_setblock(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 4 {
        return Err("Usage: /setblock <x> <y> <z> <block>".into());
    }
    let pos = block_position(sender, args, 0)?;
    let state = parse_block(server, &args[3])?;
    if !server.set_block(pos, state) {
        return Err("That block is already there.".into());
    }
    sender.reply(Text::new(format!("Changed the block at {}, {}, {}.", pos.x, pos.y, pos.z)));
    Ok(())
}

const MAX_FILL_BLOCKS: i64 = 32768 * 4;

fn region(sender: &CommandSender, args: &[String], i: usize) -> Result<(BlockPos, BlockPos), String> {
    let a = block_position(sender, args, i)?;
    let b = block_position(sender, args, i + 3)?;
    Ok((
        BlockPos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        BlockPos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    ))
}

fn cmd_fill(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 7 {
        return Err("Usage: /fill <x1> <y1> <z1> <x2> <y2> <z2> <block> [replace [filter]|hollow|outline|keep|destroy]".into());
    }
    let (min, max) = region(sender, args, 0)?;
    let volume = (max.x - min.x + 1) as i64 * (max.y - min.y + 1) as i64 * (max.z - min.z + 1) as i64;
    if volume > MAX_FILL_BLOCKS {
        return Err(format!("Too many blocks ({volume}); the limit is {MAX_FILL_BLOCKS}."));
    }
    let state = parse_block(server, &args[6])?;
    let mode = args.get(7).map(String::as_str).unwrap_or("replace");
    let filter = match (mode, args.get(8)) {
        ("replace", Some(f)) => Some(parse_block(server, f)?),
        _ => None,
    };
    let air = server.data.blocks.default_state("air").unwrap_or(0) as u32;
    let mut changed = 0usize;
    let mut touched = std::collections::HashSet::new();
    {
        let mut world = server.world();
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                for x in min.x..=max.x {
                    let on_shell = x == min.x || x == max.x || y == min.y || y == max.y || z == min.z || z == max.z;
                    let pos = BlockPos::new(x, y, z);
                    let current = world.get_block(pos).map_err(|e| e.to_string())?;
                    let wanted = match mode {
                        "hollow" => {
                            if on_shell {
                                state
                            } else {
                                air
                            }
                        }
                        "outline" => {
                            if on_shell {
                                state
                            } else {
                                continue;
                            }
                        }
                        "keep" => {
                            if server.data.blocks.is_air(current as i32) {
                                state
                            } else {
                                continue;
                            }
                        }
                        "replace" | "destroy" => match filter {
                            Some(f) if f != current => continue,
                            _ => state,
                        },
                        other => return Err(format!("Unknown fill mode '{other}'.")),
                    };
                    if world.set_block(pos, wanted).map_err(|e| e.to_string())? {
                        changed += 1;
                        touched.insert(pos.chunk());
                    }
                }
            }
        }
    }
    for chunk in touched {
        server.refresh_chunk(chunk);
    }
    if changed == 0 {
        return Err("No blocks were filled.".into());
    }
    sender.reply(Text::new(format!("Successfully filled {changed} block(s).")));
    Ok(())
}

fn cmd_clone(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 9 {
        return Err("Usage: /clone <x1> <y1> <z1> <x2> <y2> <z2> <x> <y> <z>".into());
    }
    let (min, max) = region(sender, args, 0)?;
    let dest = block_position(sender, args, 6)?;
    let volume = (max.x - min.x + 1) as i64 * (max.y - min.y + 1) as i64 * (max.z - min.z + 1) as i64;
    if volume > MAX_FILL_BLOCKS {
        return Err(format!("Too many blocks ({volume}); the limit is {MAX_FILL_BLOCKS}."));
    }
    let mut blocks = Vec::with_capacity(volume as usize);
    let mut touched = std::collections::HashSet::new();
    {
        let mut world = server.world();
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                for x in min.x..=max.x {
                    blocks.push(world.get_block(BlockPos::new(x, y, z)).map_err(|e| e.to_string())?);
                }
            }
        }
        let mut i = 0;
        for y in 0..=(max.y - min.y) {
            for z in 0..=(max.z - min.z) {
                for x in 0..=(max.x - min.x) {
                    let pos = BlockPos::new(dest.x + x, dest.y + y, dest.z + z);
                    if world.set_block(pos, blocks[i]).map_err(|e| e.to_string())? {
                        touched.insert(pos.chunk());
                    }
                    i += 1;
                }
            }
        }
    }
    for chunk in touched {
        server.refresh_chunk(chunk);
    }
    sender.reply(Text::new(format!("Successfully cloned {volume} block(s).")));
    Ok(())
}

fn cmd_spreadplayers(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 5 {
        return Err("Usage: /spreadplayers <x> <z> <distance> <range> <targets>".into());
    }
    let base = sender_position(sender).unwrap_or((0.0, 0.0, 0.0));
    let cx = coord(&args[0], base.0)?;
    let cz = coord(&args[1], base.2)?;
    let distance: f64 = number(args.get(2), "distance")?;
    let range: f64 = number(args.get(3), "range")?;
    let targets = resolve_targets(server, sender, &args[4])?;
    let mut placed: Vec<(f64, f64)> = Vec::new();
    for target in &targets {
        let mut spot = None;
        for _ in 0..200 {
            let x = cx + rand::random_range(-range..=range);
            let z = cz + rand::random_range(-range..=range);
            if placed.iter().all(|(px, pz)| ((px - x).powi(2) + (pz - z).powi(2)).sqrt() >= distance) {
                spot = Some((x, z));
                break;
            }
        }
        let (x, z) = spot.ok_or("Could not spread players that far apart; lower the distance or raise the range.")?;
        placed.push((x, z));
        let y = surface_height(server, x.floor() as i32, z.floor() as i32);
        teleport(target, x.floor() + 0.5, y as f64, z.floor() + 0.5, 0.0, 0.0);
    }
    sender.reply(Text::new(format!("Spread {} player(s) around {cx:.0}, {cz:.0}.", targets.len())));
    Ok(())
}

/// The y just above the highest block in a column, loading the chunk if needed.
pub fn surface_height(server: &Server, x: i32, z: i32) -> i32 {
    let mut world = server.world();
    let pos = BlockPos::new(x, 64, z);
    let Ok(chunk) = world.chunk_mut(pos.chunk()) else {
        return 64;
    };
    let blocks = &server.data.blocks;
    chunk.highest_block(x & 15, z & 15, |s| blocks.is_air(s as i32)).map(|y| y + 1).unwrap_or(64)
}

fn cmd_random(_: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /random [roll|value] <min..max>";
    let spec = args.iter().find(|a| a.contains("..")).ok_or(usage)?;
    let (lo, hi) = spec.split_once("..").ok_or(usage)?;
    let lo: i64 = lo.parse().map_err(|_| usage.to_owned())?;
    let hi: i64 = hi.parse().map_err(|_| usage.to_owned())?;
    if hi < lo {
        return Err("The maximum must not be smaller than the minimum.".into());
    }
    let value = rand::random_range(lo..=hi);
    sender.reply(Text::new(format!("Rolled {value} ({lo}..{hi}).")));
    Ok(())
}

// ---------- players ----------

fn cmd_teleport(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    match server.commands.get("tp").and_then(|c| c.handler) {
        Some(handler) => handler(server, sender, args),
        None => Err("/tp is not available.".into()),
    }
}

fn cmd_rotate(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err("Usage: /rotate <player> <yaw> <pitch>".into());
    }
    let yaw: f32 = number(args.get(1), "yaw")?;
    let pitch: f32 = number(args.get(2), "pitch")?;
    for target in resolve_targets(server, sender, &args[0])? {
        let (x, y, z) = {
            let s = target.lock();
            (s.x, s.y, s.z)
        };
        teleport(&target, x, y, z, yaw, pitch);
    }
    Ok(())
}

fn cmd_xp(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /xp <add|set|query> <targets> [amount] [levels|points]";
    let sub = args.first().ok_or(usage)?.as_str();
    let targets = resolve_targets(server, sender, args.get(1).ok_or(usage)?)?;
    let levels = args.get(3).map(String::as_str) != Some("points");
    for target in targets {
        match sub {
            "query" => {
                let s = target.lock();
                sender.reply(Text::new(format!("{} has {} levels and {} total experience.", target.name(), s.xp_level, s.xp_total)));
            }
            "add" | "set" => {
                let amount: i32 = number(args.get(2), "amount")?;
                let mut s = target.lock();
                if levels {
                    s.xp_level = if sub == "add" { s.xp_level + amount } else { amount }.max(0);
                } else {
                    s.xp_total = if sub == "add" { s.xp_total + amount } else { amount }.max(0);
                    s.xp_level = level_for_points(s.xp_total);
                }
                let (level, total) = (s.xp_level, s.xp_total);
                drop(s);
                target.send(&cb::SetExperience { bar: 0.0, level, total });
                sender.reply(Text::new(format!("{} now has {level} levels.", target.name())));
            }
            _ => return Err(usage.into()),
        }
    }
    Ok(())
}

/// Vanilla's level curve.
fn level_for_points(points: i32) -> i32 {
    let mut level = 0;
    let mut remaining = points;
    loop {
        let needed = if level >= 30 {
            112 + (level - 30) * 9
        } else if level >= 15 {
            37 + (level - 15) * 5
        } else {
            7 + level * 2
        };
        if remaining < needed {
            return level;
        }
        remaining -= needed;
        level += 1;
    }
}

fn cmd_kill(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let targets = match args.first() {
        Some(t) => resolve_targets(server, sender, t)?,
        None => sender.player().cloned().map(|p| vec![p]).ok_or("Say who to kill.")?,
    };
    for target in targets {
        kill_player(server, &target, Text::new(format!("{} was killed", target.name())));
        sender.reply(Text::new(format!("Killed {}.", target.name())));
    }
    Ok(())
}

/// Sets health to zero and shows the death screen; the client answers with
/// a respawn request that `net::play` handles.
pub fn kill_player(server: &Arc<Server>, player: &Arc<Player>, message: Text) {
    {
        let mut s = player.lock();
        s.health = 0.0;
    }
    player.send(&cb::SetHealth {
        health: 0.0,
        food: player.lock().food,
        saturation: 0.0,
    });
    player.send(&cb::PlayerCombatKill {
        player_id: player.entity_id,
        message: message.clone(),
    });
    if server.rules.read().unwrap_or_else(|e| e.into_inner()).game_rule_bool("showDeathMessages") {
        server.broadcast_chat(message);
    }
}

fn cmd_damage(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("Usage: /damage <target> <amount>".into());
    }
    let amount: f32 = number(args.get(1), "amount")?;
    for target in resolve_targets(server, sender, &args[0])? {
        damage_player(server, &target, amount, "generic");
        sender.reply(Text::new(format!("Applied {amount} damage to {}.", target.name())));
    }
    Ok(())
}

pub fn damage_player(server: &Arc<Server>, player: &Arc<Player>, amount: f32, damage_type: &str) {
    let (health, food, mode) = {
        let mut s = player.lock();
        if matches!(s.game_mode, GameMode::Creative | GameMode::Spectator) {
            return;
        }
        s.health = (s.health - amount).max(0.0);
        (s.health, s.food, s.game_mode)
    };
    let _ = mode;
    let damage_type = server.data.dynamic.id_of("damage_type", damage_type).unwrap_or(0);
    player.send(&cb::DamageEvent {
        entity_id: player.entity_id,
        damage_type,
    });
    player.send(&cb::HurtAnimation {
        entity_id: player.entity_id,
        yaw: 0.0,
    });
    if health <= 0.0 {
        kill_player(server, player, Text::new(format!("{} died", player.name())));
    } else {
        player.send(&cb::SetHealth {
            health,
            food,
            saturation: 5.0,
        });
    }
}

fn cmd_effect(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /effect give <targets> <effect> [seconds|infinite] [amplifier] [hideParticles] | /effect clear [targets] [effect]";
    let sub = args.first().ok_or(usage)?.as_str();
    match sub {
        "give" => {
            let targets = resolve_targets(server, sender, args.get(1).ok_or(usage)?)?;
            let name = args.get(2).ok_or(usage)?;
            let full = if name.contains(':') { name.clone() } else { format!("minecraft:{name}") };
            let effect = server.data.registries.id_of("mob_effect", &full).ok_or_else(|| format!("Unknown effect '{name}'."))?;
            let duration = match args.get(3).map(String::as_str) {
                None => 30 * 20,
                Some("infinite") => -1,
                Some(s) => s.parse::<i32>().map(|s| s * 20).map_err(|_| "Seconds must be a number or 'infinite'.".to_owned())?,
            };
            let amplifier: i32 = args.get(4).map(|v| v.parse().unwrap_or(0)).unwrap_or(0);
            let hide_particles = args.get(5).map(String::as_str) == Some("true");
            for target in &targets {
                give_effect(server, target, &full, effect, amplifier, duration, !hide_particles);
            }
            sender.reply(Text::new(format!("Applied {name} to {} player(s).", targets.len())));
        }
        "clear" => {
            let targets = match args.get(1) {
                Some(t) => resolve_targets(server, sender, t)?,
                None => sender.player().cloned().map(|p| vec![p]).ok_or("Say whose effects to clear.")?,
            };
            let only = args.get(2).map(|n| if n.contains(':') { n.clone() } else { format!("minecraft:{n}") });
            for target in &targets {
                let removed: Vec<ActiveEffect> = {
                    let mut s = target.lock();
                    let (gone, keep): (Vec<_>, Vec<_>) = s.effects.drain(..).partition(|e| only.as_ref().is_none_or(|o| *o == e.name));
                    s.effects = keep;
                    gone
                };
                for effect in removed {
                    target.send(&cb::RemoveMobEffect {
                        entity_id: target.entity_id,
                        effect: effect.id,
                    });
                }
            }
            sender.reply(Text::new("Cleared effects."));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

pub fn give_effect(server: &Arc<Server>, player: &Arc<Player>, name: &str, id: i32, amplifier: i32, duration: i32, particles: bool) {
    let expires_tick = if duration < 0 { None } else { Some(server.current_tick() + duration as u64) };
    {
        let mut s = player.lock();
        s.effects.retain(|e| e.name != name);
        s.effects.push(ActiveEffect {
            name: name.to_owned(),
            id,
            amplifier,
            expires_tick,
            particles,
        });
    }
    player.send(&cb::UpdateMobEffect {
        entity_id: player.entity_id,
        effect: id,
        amplifier,
        duration,
        ambient: false,
        show_particles: particles,
        show_icon: true,
    });
}

/// Called every tick: drops effects that ran out.
pub fn tick_effects(server: &Arc<Server>, tick: u64) {
    for player in server.online_players() {
        let expired: Vec<ActiveEffect> = {
            let mut s = player.lock();
            let (gone, keep): (Vec<_>, Vec<_>) = s.effects.drain(..).partition(|e| e.expires_tick.is_some_and(|t| t <= tick));
            s.effects = keep;
            gone
        };
        for effect in expired {
            player.send(&cb::RemoveMobEffect {
                entity_id: player.entity_id,
                effect: effect.id,
            });
        }
    }
}

fn cmd_attribute(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /attribute <target> <attribute> get | base get | base set <value>";
    if args.len() < 3 {
        return Err(usage.into());
    }
    let target = find_player(server, &args[0])?;
    let name = if args[1].contains(':') { args[1].clone() } else { format!("minecraft:{}", args[1]) };
    let attribute = server.data.registries.id_of("attribute", &name).ok_or_else(|| format!("Unknown attribute '{}'.", args[1]))?;
    let default = default_attribute(&name);
    match (args[2].as_str(), args.get(3).map(String::as_str)) {
        ("get", _) | ("base", Some("get")) => {
            let value = target.lock().attributes.get(&name).copied().unwrap_or(default);
            sender.reply(Text::new(format!("{} {} is {value}.", target.name(), args[1])));
        }
        ("base", Some("set")) => {
            let value: f64 = number(args.get(4), "value")?;
            target.lock().attributes.insert(name.clone(), value);
            target.send(&cb::UpdateAttributes {
                entity_id: target.entity_id,
                attributes: vec![cb::AttributeSnapshot {
                    attribute,
                    base: value,
                    modifiers: Vec::new(),
                }],
            });
            sender.reply(Text::new(format!("Set {} {} to {value}.", target.name(), args[1])));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

fn default_attribute(name: &str) -> f64 {
    match name {
        "minecraft:max_health" => 20.0,
        "minecraft:movement_speed" => 0.1,
        "minecraft:attack_damage" => 1.0,
        "minecraft:attack_speed" => 4.0,
        "minecraft:armor" | "minecraft:armor_toughness" | "minecraft:knockback_resistance" | "minecraft:luck" => 0.0,
        "minecraft:block_interaction_range" => 4.5,
        "minecraft:entity_interaction_range" => 3.0,
        "minecraft:step_height" => 0.6,
        "minecraft:jump_strength" => 0.42,
        "minecraft:gravity" => 0.08,
        "minecraft:safe_fall_distance" => 3.0,
        "minecraft:fall_damage_multiplier" | "minecraft:scale" | "minecraft:water_movement_efficiency" => 1.0,
        "minecraft:sneaking_speed" => 0.3,
        "minecraft:block_break_speed" | "minecraft:mining_efficiency" => 1.0,
        _ => 0.0,
    }
}

fn cmd_spectate(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let player = match args.get(1) {
        Some(p) => find_player(server, p)?,
        None => sender.player().cloned().ok_or("Say who is spectating.")?,
    };
    if player.lock().game_mode != GameMode::Spectator {
        return Err(format!("{} is not in spectator mode.", player.name()));
    }
    let target = match args.first() {
        Some(t) => find_player(server, t)?,
        None => player.clone(),
    };
    player.send(&cb::SetCamera {
        entity_id: target.entity_id,
    });
    Ok(())
}

fn cmd_transfer(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let host = args.first().ok_or("Usage: /transfer <host> [port] [targets]")?.clone();
    let port: i32 = args.get(1).map(|p| p.parse().unwrap_or(25565)).unwrap_or(25565);
    let targets = match args.get(2) {
        Some(t) => resolve_targets(server, sender, t)?,
        None => sender.player().cloned().map(|p| vec![p]).ok_or("Say who to transfer.")?,
    };
    for target in &targets {
        target.send(&cb::Transfer {
            host: host.clone(),
            port,
        });
    }
    sender.reply(Text::new(format!("Transferring {} player(s) to {host}:{port}.", targets.len())));
    Ok(())
}

fn cmd_banlist(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let (players, ips) = server.lists.bans();
    let listed: Vec<String> = if args.first().map(String::as_str) == Some("ips") {
        ips.iter().map(|b| format!("{} ({})", b.ip, b.reason)).collect()
    } else {
        players.iter().map(|b| format!("{} ({})", b.name, b.reason)).collect()
    };
    if listed.is_empty() {
        sender.reply(Text::new("There are no bans."));
    } else {
        sender.reply(Text::new(format!("There are {} ban(s): {}", listed.len(), listed.join(", "))));
    }
    Ok(())
}

fn cmd_pardon_ip(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let ip = args.first().ok_or("Usage: /pardon-ip <ip>")?;
    if server.lists.pardon(ip).map_err(|e| e.to_string())? {
        sender.reply(Text::new(format!("Unbanned IP {ip}.")));
        Ok(())
    } else {
        Err(format!("{ip} is not banned."))
    }
}

// ---------- execute ----------

fn cmd_execute(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /execute [as <targets>] [at <target>] [positioned <x> <y> <z>] [if|unless entity <targets>] run <command>";
    let mut senders: Vec<CommandSender> = vec![sender.clone()];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "as" => {
                let selector = args.get(i + 1).ok_or(usage)?;
                let mut next = Vec::new();
                for s in &senders {
                    for p in resolve_targets(server, s, selector)? {
                        next.push(CommandSender::As {
                            player: p,
                            origin: Box::new(sender.clone()),
                        });
                    }
                }
                senders = next;
                i += 2;
            }
            "at" => {
                // Positions come from the sender's own player, so "at" only
                // changes which player we stand at for relative coordinates.
                let selector = args.get(i + 1).ok_or(usage)?;
                let _ = resolve_targets(server, sender, selector)?;
                i += 2;
            }
            "positioned" => {
                let _ = position(sender, args, i + 1)?;
                i += 4;
            }
            "if" | "unless" => {
                if args.get(i + 1).map(String::as_str) != Some("entity") {
                    return Err(usage.into());
                }
                let selector = args.get(i + 2).ok_or(usage)?;
                let found = !resolve_targets(server, sender, selector).unwrap_or_default().is_empty();
                if found != (args[i] == "if") {
                    return Ok(());
                }
                i += 3;
            }
            "run" => {
                let command = rest(args, i + 1);
                if command.is_empty() {
                    return Err(usage.into());
                }
                for s in &senders {
                    server.commands.execute(server, s, &command);
                }
                return Ok(());
            }
            _ => return Err(usage.into()),
        }
    }
    Err(usage.into())
}
