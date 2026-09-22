//! Commands: the registry, who is running them, and the built-in set.
//!
//! Commands from mods are registered with a `mod_id`; running one raises
//! `Event::Command` for that mod instead of a Rust handler.

use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::Text;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};

/// Who is running a command and how to answer them.
#[derive(Clone)]
pub enum CommandSender {
    Console,
    Player(Arc<Player>),
    /// A panel user; replies are collected and returned over HTTP.
    Panel { username: String, replies: Arc<Mutex<Vec<String>>> },
    Rcon { replies: Arc<Mutex<Vec<String>>> },
    Mod { mod_id: String },
    /// `/execute as <player>`: acts and stands as the player, but keeps the
    /// permissions and receives the replies of whoever ran `/execute`.
    As { player: Arc<Player>, origin: Box<CommandSender> },
}

impl CommandSender {
    pub fn name(&self) -> String {
        match self {
            CommandSender::Console => "Console".into(),
            CommandSender::Player(p) => p.name().to_owned(),
            CommandSender::Panel { username, .. } => format!("panel:{username}"),
            CommandSender::Rcon { .. } => "Rcon".into(),
            CommandSender::Mod { mod_id } => format!("mod:{mod_id}"),
            CommandSender::As { player, .. } => player.name().to_owned(),
        }
    }

    pub fn player(&self) -> Option<&Arc<Player>> {
        match self {
            CommandSender::Player(p) | CommandSender::As { player: p, .. } => Some(p),
            _ => None,
        }
    }

    pub fn reply(&self, text: Text) {
        match self {
            CommandSender::Console | CommandSender::Mod { .. } => tracing::info!("{}", text.to_plain()),
            CommandSender::Player(p) => p.send(&cb::SystemChat {
                content: text,
                overlay: false,
            }),
            CommandSender::Panel { replies, .. } | CommandSender::Rcon { replies } => {
                replies.lock().unwrap_or_else(|e| e.into_inner()).push(text.to_plain());
            }
            CommandSender::As { origin, .. } => origin.reply(text),
        }
    }

    pub fn reply_error(&self, message: impl Into<String>) {
        self.reply(Text::new(message.into()).color("red"));
    }

    /// Console, panel, RCON and mods can do anything; players go through
    /// permissions (ops have everything unless a node denies it).
    pub fn has_permission(&self, server: &Server, node: &str) -> bool {
        match self {
            CommandSender::Player(p) => server.has_permission(p.uuid, node),
            CommandSender::As { origin, .. } => origin.has_permission(server, node),
            _ => true,
        }
    }
}

pub type Handler = fn(&Arc<Server>, &CommandSender, &[String]) -> Result<(), String>;

#[derive(Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub usage: String,
    pub permission: Option<String>,
    pub handler: Option<Handler>,
    pub mod_id: Option<String>,
}

#[derive(Default)]
pub struct CommandRegistry {
    commands: RwLock<BTreeMap<String, Command>>,
}

impl CommandRegistry {
    pub fn register(&self, command: Command) {
        self.commands
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(command.name.clone(), command);
    }

    pub fn register_mod_command(&self, mod_id: &str, name: &str, description: &str, permission: Option<String>) {
        self.register(Command {
            name: name.to_ascii_lowercase(),
            description: description.to_owned(),
            usage: format!("/{name}"),
            permission,
            handler: None,
            mod_id: Some(mod_id.to_owned()),
        });
    }

    pub fn remove_mod_commands(&self) {
        self.commands
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, c| c.mod_id.is_none());
    }

    pub fn list(&self) -> Vec<Command> {
        self.commands.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<Command> {
        self.commands.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// Splits `line`, checks permission, and runs the command. Mods get a
    /// chance to intercept every command first.
    pub fn execute(&self, server: &Arc<Server>, sender: &CommandSender, line: &str) {
        let line = line.trim().trim_start_matches('/');
        if line.is_empty() {
            return;
        }
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap().to_ascii_lowercase();
        let args: Vec<String> = parts.map(str::to_owned).collect();

        // Let mods see (and possibly cancel) the command.
        let event = garnet_api::Event::Command {
            uuid: sender.player().map(|p| p.uuid),
            name: sender.name(),
            command: line.to_owned(),
        };
        let result = server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
        if result.cancel {
            if let Some(message) = result.message {
                sender.reply(Text::legacy(&message));
            }
            return;
        }

        let Some(command) = self.get(&name) else {
            sender.reply_error(format!("Unknown command: /{name}. Try /help."));
            return;
        };
        let node = command.permission.clone().unwrap_or_else(|| format!("garnet.command.{name}"));
        if !sender.has_permission(server, &node) {
            sender.reply_error("You do not have permission to use this command.");
            return;
        }
        if !matches!(sender, CommandSender::Player(_)) || matches!(
                name.as_str(),
                "kick" | "ban" | "ban-ip" | "pardon" | "pardon-ip" | "op" | "deop" | "stop" | "whitelist" | "gamemode" | "defaultgamemode" | "tp" | "teleport"
                    | "time" | "save-all" | "save-off" | "save-on" | "reload" | "mods" | "setblock" | "fill" | "clone" | "gamerule" | "difficulty" | "weather"
                    | "worldborder" | "tick" | "kill" | "damage" | "effect" | "attribute" | "xp" | "experience" | "setworldspawn" | "transfer" | "execute"
            ) {
            server.audit.record(&sender.name(), "command", line);
        }
        match command.handler {
            Some(handler) => {
                if let Err(message) = handler(server, sender, &args) {
                    sender.reply_error(message);
                }
            }
            // A mod command: the mod already saw the event above. If it did
            // not cancel it, it did not handle it.
            None => sender.reply_error(format!("/{name} is provided by a mod that did not respond.")),
        }
    }

    /// The command tree the client uses for tab completion: every command
    /// as a literal with one greedy text argument.
    pub fn tree_packet(&self, server: &Server, sender: &CommandSender) -> cb::Commands {
        let greedy_string = server
            .data
            .registries
            .id_of("command_argument_type", "brigadier:string")
            .unwrap_or(5);
        let mut nodes = vec![cb::CommandNode {
            kind: cb::CommandNodeKind::Root,
            executable: false,
            children: Vec::new(),
            redirect: None,
        }];
        let mut root_children = Vec::new();
        for command in self.list() {
            let node = command.permission.clone().unwrap_or_else(|| format!("garnet.command.{}", command.name));
            if !sender.has_permission(server, &node) {
                continue;
            }
            let literal_index = nodes.len() as i32;
            let arg_index = literal_index + 1;
            nodes.push(cb::CommandNode {
                kind: cb::CommandNodeKind::Literal(command.name.clone()),
                executable: true,
                children: vec![arg_index],
                redirect: None,
            });
            // Greedy phrase = string parser property 2.
            nodes.push(cb::CommandNode {
                kind: cb::CommandNodeKind::Argument {
                    name: "args".into(),
                    parser_id: greedy_string,
                    properties: vec![2],
                    suggestions: Some("minecraft:ask_server".into()),
                },
                executable: true,
                children: Vec::new(),
                redirect: None,
            });
            root_children.push(literal_index);
        }
        nodes[0].children = root_children;
        cb::Commands { nodes, root: 0 }
    }
}

// ---------- built-in commands ----------

pub fn register_builtins(registry: &CommandRegistry) {
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
    add("help", "Lists commands", "/help", cmd_help);
    add("list", "Shows who is online", "/list", cmd_list);
    add("say", "Broadcasts a message", "/say <message>", cmd_say);
    add("msg", "Private message", "/msg <player> <message>", cmd_msg);
    add("kick", "Kicks a player", "/kick <player> [reason]", cmd_kick);
    add("ban", "Bans a player", "/ban <player> [reason]", cmd_ban);
    add("ban-ip", "Bans an IP address", "/ban-ip <ip|player> [reason]", cmd_ban_ip);
    add("pardon", "Removes a ban", "/pardon <player|ip>", cmd_pardon);
    add("op", "Makes a player an operator", "/op <player>", cmd_op);
    add("deop", "Removes operator status", "/deop <player>", cmd_deop);
    add("gamemode", "Changes game mode", "/gamemode <mode> [player]", cmd_gamemode);
    add("tp", "Teleports", "/tp <player> | /tp <x> <y> <z> | /tp <player> <target> | /tp <player> <x> <y> <z> [<yaw> <pitch>]", cmd_tp);
    add("time", "Sets the time", "/time set <day|night|noon|midnight|ticks>", cmd_time);
    add("whitelist", "Manages the whitelist", "/whitelist <add|remove|list|on|off> [player]", cmd_whitelist);
    add("save-all", "Saves the world", "/save-all", cmd_save);
    add("stop", "Stops the server", "/stop", cmd_stop);
    add("mods", "Lists loaded mods", "/mods", cmd_mods);
    add("panel", "Gives you a login link for the web panel", "/panel", cmd_panel);
    add("seed", "Shows the world seed", "/seed", cmd_seed);
    add("tps", "Shows server performance", "/tps", cmd_tps);
    add("spawn", "Teleports you to spawn", "/spawn", cmd_spawn);
}

pub(crate) fn find_player(server: &Server, name: &str) -> Result<Arc<Player>, String> {
    server.player_by_name(name).ok_or_else(|| format!("Player '{name}' is not online."))
}

fn cmd_help(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    sender.reply(Text::new("Commands:").color("gold"));
    for c in server.commands.list() {
        let node = c.permission.clone().unwrap_or_else(|| format!("garnet.command.{}", c.name));
        if sender.has_permission(server, &node) {
            sender.reply(Text::new(format!("{} ", c.usage)).color("yellow").append(Text::new(c.description).color("gray")));
        }
    }
    Ok(())
}

fn cmd_list(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    let players = server.online_players();
    let names: Vec<String> = players.iter().map(|p| p.name().to_owned()).collect();
    sender.reply(Text::new(format!(
        "There are {} of a max of {} players online: {}",
        players.len(),
        server.config().server.max_players,
        names.join(", ")
    )));
    Ok(())
}

fn cmd_say(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /say <message>".into());
    }
    server.broadcast_chat(Text::new(format!("[{}] {}", sender.name(), args.join(" "))).color("light_purple"));
    Ok(())
}

fn cmd_msg(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("Usage: /msg <player> <message>".into());
    }
    let target = find_player(server, &args[0])?;
    let message = args[1..].join(" ");
    target.send(&cb::SystemChat {
        content: Text::new(format!("{} whispers: {message}", sender.name())).color("gray").italic(),
        overlay: false,
    });
    sender.reply(Text::new(format!("To {}: {message}", target.name())).color("gray").italic());
    Ok(())
}

fn cmd_kick(server: &Arc<Server>, _sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /kick <player> [reason]".into());
    }
    let target = find_player(server, &args[0])?;
    let reason = if args.len() > 1 { args[1..].join(" ") } else { "Kicked by an operator.".into() };
    target.disconnect(Text::new(reason.clone()));
    server.broadcast_chat(Text::new(format!("{} was kicked: {reason}", target.name())).color("yellow"));
    Ok(())
}

fn cmd_ban(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /ban <player> [reason]".into());
    }
    let reason = if args.len() > 1 { args[1..].join(" ") } else { String::new() };
    let (uuid, name) = match server.player_by_name(&args[0]) {
        Some(p) => (p.uuid, p.name().to_owned()),
        None => (garnet_protocol::GameProfile::offline(&args[0]).id, args[0].clone()),
    };
    server.lists.ban(uuid, &name, &reason, &sender.name(), None).map_err(|e| e.to_string())?;
    if let Some(p) = server.player_by_name(&name) {
        p.disconnect(Text::new(format!("You are banned from this server.\n{reason}")));
    }
    sender.reply(Text::new(format!("Banned {name}.")));
    Ok(())
}

fn cmd_ban_ip(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /ban-ip <ip|player> [reason]".into());
    }
    let reason = if args.len() > 1 { args[1..].join(" ") } else { String::new() };
    let ip = match server.player_by_name(&args[0]) {
        Some(p) => p.ip.clone(),
        None => args[0].clone(),
    };
    server.lists.ban_ip(&ip, &reason, &sender.name(), None).map_err(|e| e.to_string())?;
    for p in server.online_players() {
        if p.ip == ip {
            p.disconnect(Text::new(format!("Your IP address is banned from this server.\n{reason}")));
        }
    }
    sender.reply(Text::new(format!("Banned IP {ip}.")));
    Ok(())
}

fn cmd_pardon(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /pardon <player|ip>".into());
    }
    if server.lists.pardon(&args[0]).map_err(|e| e.to_string())? {
        sender.reply(Text::new(format!("Unbanned {}.", args[0])));
        Ok(())
    } else {
        Err(format!("{} is not banned.", args[0]))
    }
}

fn set_op(server: &Arc<Server>, sender: &CommandSender, name: &str, op: bool) -> Result<(), String> {
    let (uuid, name) = match server.player_by_name(name) {
        Some(p) => (p.uuid, p.name().to_owned()),
        None => (garnet_protocol::GameProfile::offline(name).id, name.to_owned()),
    };
    let changed = server.lists.set_op(uuid, &name, op).map_err(|e| e.to_string())?;
    if !changed {
        return Err(format!("{name} is {} an operator.", if op { "already" } else { "not" }));
    }
    if let Some(p) = server.player(uuid) {
        p.send(&cb::EntityEvent {
            entity_id: p.entity_id,
            status: if op { 28 } else { 24 },
        });
        p.send(&server.commands.tree_packet(server, &CommandSender::Player(Arc::clone(&p))));
        p.send(&cb::SystemChat {
            content: Text::new(if op { "You are now an operator." } else { "You are no longer an operator." }).color("gray"),
            overlay: false,
        });
    }
    sender.reply(Text::new(format!("{} {name}.", if op { "Opped" } else { "De-opped" })));
    Ok(())
}

fn cmd_op(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /op <player>".into());
    }
    set_op(server, sender, &args[0], true)
}

fn cmd_deop(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /deop <player>".into());
    }
    set_op(server, sender, &args[0], false)
}

pub fn set_game_mode(server: &Arc<Server>, player: &Arc<Player>, mode: GameMode) {
    let previous = {
        let mut state = player.lock();
        let previous = state.game_mode;
        state.game_mode = mode;
        state.flying = matches!(mode, GameMode::Spectator);
        previous
    };
    if previous == mode {
        return;
    }
    player.send(&cb::GameEvent {
        event: cb::GameEventKind::ChangeGameMode,
        value: mode as i32 as f32,
    });
    player.send(&cb::PlayerAbilities::for_game_mode(mode));
    server.broadcast(&cb::PlayerInfoUpdate {
        actions: cb::player_info_action::UPDATE_GAME_MODE,
        entries: vec![cb::PlayerInfoEntry {
            uuid: player.uuid,
            game_mode: Some(mode),
            ..Default::default()
        }],
    });
    let event = garnet_api::Event::GameModeChange {
        uuid: player.uuid,
        from: crate::mod_host::to_api_mode(previous),
        to: crate::mod_host::to_api_mode(mode),
    };
    server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
}

fn cmd_gamemode(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /gamemode <survival|creative|adventure|spectator> [player]".into());
    }
    let mode = GameMode::parse(&args[0]).ok_or_else(|| format!("Unknown game mode '{}'.", args[0]))?;
    let target = match args.get(1) {
        Some(name) => find_player(server, name)?,
        None => sender.player().cloned().ok_or("Specify a player.")?,
    };
    set_game_mode(server, &target, mode);
    sender.reply(Text::new(format!("Set {}'s game mode to {}.", target.name(), mode.name())));
    Ok(())
}

pub fn teleport(player: &Arc<Player>, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) {
    let id = {
        let mut state = player.lock();
        state.teleport_counter += 1;
        state.pending_teleport = Some(state.teleport_counter);
        state.checks.awaiting_teleport = true;
        state.x = x;
        state.y = y;
        state.z = z;
        state.yaw = yaw;
        state.pitch = pitch;
        state.teleport_counter
    };
    player.send(&cb::PlayerPosition {
        teleport_id: id,
        x,
        y,
        z,
        velocity_x: 0.0,
        velocity_y: 0.0,
        velocity_z: 0.0,
        yaw,
        pitch,
        relative_flags: 0,
    });
}

fn cmd_tp(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /tp <player> | /tp <x> <y> <z> | /tp <player> <target> | /tp <player> <x> <y> <z> [<yaw> <pitch>]";
    let parse = |s: &str| s.parse::<f64>().map_err(|_| usage.to_owned());
    let mut facing = None;
    let (who, dest) = match args.len() {
        1 => {
            let me = sender.player().cloned().ok_or(usage)?;
            let target = find_player(server, &args[0])?;
            let s = target.lock();
            (me, (s.x, s.y, s.z))
        }
        2 => {
            let who = find_player(server, &args[0])?;
            let target = find_player(server, &args[1])?;
            let s = target.lock();
            (who, (s.x, s.y, s.z))
        }
        3 => (
            sender.player().cloned().ok_or(usage)?,
            (parse(&args[0])?, parse(&args[1])?, parse(&args[2])?),
        ),
        4 => (
            find_player(server, &args[0])?,
            (parse(&args[1])?, parse(&args[2])?, parse(&args[3])?),
        ),
        6 => {
            facing = Some((parse(&args[4])? as f32, parse(&args[5])? as f32));
            (
                find_player(server, &args[0])?,
                (parse(&args[1])?, parse(&args[2])?, parse(&args[3])?),
            )
        }
        _ => return Err(usage.into()),
    };
    let (yaw, pitch) = facing.unwrap_or_else(|| {
        let s = who.lock();
        (s.yaw, s.pitch)
    });
    teleport(&who, dest.0, dest.1, dest.2, yaw, pitch);
    sender.reply(Text::new(format!("Teleported {} to {:.1}, {:.1}, {:.1}.", who.name(), dest.0, dest.1, dest.2)));
    Ok(())
}

fn cmd_spawn(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    let player = sender.player().cloned().ok_or("Only players can use /spawn.")?;
    let spawn = server.world().settings.spawn;
    teleport(&player, spawn.x as f64 + 0.5, spawn.y as f64, spawn.z as f64 + 0.5, 0.0, 0.0);
    Ok(())
}

fn cmd_time(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /time set <day|night|noon|midnight|ticks>";
    if args.len() != 2 || args[0] != "set" {
        return Err(usage.into());
    }
    let ticks = match args[1].as_str() {
        "day" => 1000,
        "noon" => 6000,
        "night" => 13000,
        "midnight" => 18000,
        other => other.parse::<i64>().map_err(|_| usage.to_owned())?,
    };
    let (age, daylight) = {
        let mut world = server.world();
        world.settings.time_of_day = ticks.rem_euclid(24000);
        (world.settings.age, world.settings.daylight_cycle)
    };
    server.broadcast(&cb::SetTime {
        world_age: age,
        time_of_day: ticks.rem_euclid(24000),
        advancing: daylight,
    });
    sender.reply(Text::new(format!("Set the time to {ticks}.")));
    Ok(())
}

fn cmd_whitelist(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /whitelist <add|remove|list|on|off> [player]";
    match args.first().map(String::as_str) {
        Some("add") => {
            let name = args.get(1).ok_or(usage)?;
            let uuid = server.player_by_name(name).map(|p| p.uuid).unwrap_or_else(|| garnet_protocol::GameProfile::offline(name).id);
            server.lists.whitelist_add(uuid, name).map_err(|e| e.to_string())?;
            sender.reply(Text::new(format!("Added {name} to the whitelist.")));
        }
        Some("remove") => {
            let name = args.get(1).ok_or(usage)?;
            server.lists.whitelist_remove(name).map_err(|e| e.to_string())?;
            sender.reply(Text::new(format!("Removed {name} from the whitelist.")));
        }
        Some("list") => sender.reply(Text::new(format!("Whitelist: {}", server.lists.whitelist().join(", ")))),
        Some("on") | Some("off") => {
            let on = args[0] == "on";
            server.config.write().unwrap_or_else(|e| e.into_inner()).server.whitelist = on;
            sender.reply(Text::new(format!("Whitelist is now {} (until restart; set it in garnet.toml to keep it).", if on { "on" } else { "off" })));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

fn cmd_save(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    server.save_everything("manual save");
    sender.reply(Text::new("Saved the world."));
    Ok(())
}

fn cmd_stop(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    sender.reply(Text::new("Stopping the server."));
    server.request_stop();
    Ok(())
}

fn cmd_mods(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    let mods = server.mods.lock().unwrap_or_else(|e| e.into_inner()).list();
    if mods.is_empty() {
        sender.reply(Text::new("No mods loaded."));
    }
    for m in mods {
        sender.reply(
            Text::new(format!("{} {} ", m.name, m.version))
                .color(if m.enabled { "green" } else { "gray" })
                .append(Text::new(m.description).color("gray")),
        );
    }
    Ok(())
}

fn cmd_panel(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    let player = sender.player().ok_or("Run /panel in game to get a login link.")?;
    if !server.is_op(player.uuid) {
        return Err("Only operators can log in to the panel this way.".into());
    }
    let panel = server.panel.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let panel = panel.ok_or("The web panel is disabled.")?;
    let link = panel.login_link(player.name(), garnet_admin::auth::Role::Admin);
    player.send(&cb::SystemChat {
        content: Text::new("Click to open the admin panel (link is valid for 15 minutes): ")
            .color("gray")
            .append(
                Text::new(link.clone())
                    .color("aqua")
                    .on_click(garnet_protocol::text::ClickEvent::OpenUrl { url: link }),
            ),
        overlay: false,
    });
    server.audit.record(player.name(), "panel-link", "requested a panel login link");
    Ok(())
}

fn cmd_seed(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    sender.reply(Text::new(format!("Seed: {}", server.world().settings.seed)));
    Ok(())
}

fn cmd_tps(server: &Arc<Server>, sender: &CommandSender, _: &[String]) -> Result<(), String> {
    let (tps, ms) = server.tps();
    sender.reply(Text::new(format!(
        "TPS: {tps:.1} ({ms:.1} ms per tick), {} players, {} chunks loaded",
        server.online_count(),
        server.world().loaded_chunk_count()
    )));
    Ok(())
}
