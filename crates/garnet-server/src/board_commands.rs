//! `/scoreboard`, `/team`, `/tag`, `/trigger`, `/bossbar` and `/teammsg`,
//! backed by the state in `boards.rs`.

use crate::boards::{collision_id, color_id, display_slot_id, visibility_id, BossBar, Objective, Team};
use crate::commands::{Command, CommandRegistry, CommandSender, Handler};
use crate::player::Player;
use crate::server::Server;
use crate::vanilla_commands::resolve_targets;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::Text;
use std::sync::Arc;
use uuid::Uuid;

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
    add("scoreboard", "Objectives and scores", "/scoreboard objectives <add|remove|list|setdisplay> ... | players <set|add|remove|reset|get|list> ...", cmd_scoreboard);
    add("team", "Teams", "/team <add|remove|join|leave|empty|list|modify> ...", cmd_team);
    add("tag", "Player tags", "/tag <targets> <add|remove|list> [tag]", cmd_tag);
    add("trigger", "Triggers an objective", "/trigger <objective> [add|set <value>]", cmd_trigger);
    add("bossbar", "Boss bars", "/bossbar <add|remove|set|get|list> ...", cmd_bossbar);
}

fn save_boards(server: &Server) {
    let dir = server.root.join(&server.config().world.name);
    server.boards.lock().unwrap_or_else(|e| e.into_inner()).save(&dir);
}

fn rest(args: &[String], from: usize) -> String {
    args.iter().skip(from).cloned().collect::<Vec<_>>().join(" ")
}

fn text_arg(args: &[String], from: usize, fallback: &str) -> Text {
    let raw = rest(args, from);
    if raw.is_empty() {
        return Text::new(fallback);
    }
    if raw.trim_start().starts_with('{') || raw.trim_start().starts_with('"') {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
            return Text::from_json(&json);
        }
    }
    Text::legacy(&raw)
}

/// Everything a joining player needs to see the boards as they are.
pub fn send_boards(server: &Server, player: &Player) {
    let boards = server.boards.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for objective in boards.objectives.values() {
        player.send(&objective_packet(objective, cb::ObjectiveMode::Create));
        if let Some(scores) = boards.scores.get(&objective.name) {
            for (owner, value) in scores {
                player.send(&cb::SetScore {
                    owner: owner.clone(),
                    objective: objective.name.clone(),
                    value: *value,
                });
            }
        }
    }
    for (slot, objective) in &boards.displays {
        if let Some(position) = display_slot_id(slot) {
            player.send(&cb::SetDisplayObjective {
                position,
                objective: objective.clone(),
            });
        }
    }
    for team in boards.teams.values() {
        player.send(&cb::SetPlayerTeam {
            name: team.name.clone(),
            method: cb::TeamMethod::Create {
                parameters: team_parameters(team),
                players: team.members.iter().cloned().collect(),
            },
        });
    }
    for bar in boards.boss_bars.values() {
        if bar.visible && bar.players.contains(&player.uuid) {
            player.send(&boss_add_packet(bar));
        }
    }
}

fn objective_packet(objective: &Objective, mode: cb::ObjectiveMode) -> cb::SetObjective {
    cb::SetObjective {
        name: objective.name.clone(),
        mode,
        display_name: Text::legacy(&objective.display_name),
        render_type: if objective.render_type == "hearts" { 1 } else { 0 },
    }
}

fn team_parameters(team: &Team) -> cb::TeamParameters {
    cb::TeamParameters {
        display_name: Text::legacy(&team.display_name),
        prefix: Text::legacy(&team.prefix),
        suffix: Text::legacy(&team.suffix),
        name_tag_visibility: visibility_id(&team.name_tag_visibility).unwrap_or(0),
        collision_rule: collision_id(&team.collision_rule).unwrap_or(0),
        color: color_id(&team.color),
        options: (team.friendly_fire as u8) | ((team.see_friendly_invisibles as u8) << 1),
    }
}

fn boss_add_packet(bar: &BossBar) -> cb::BossEvent {
    cb::BossEvent {
        uuid: bar.uuid,
        action: cb::BossEventAction::Add {
            title: Text::legacy(&bar.name),
            progress: bar.progress(),
            color: bar.color_id(),
            division: bar.style_id(),
            flags: 0,
        },
    }
}

// ---------- scoreboard ----------

fn cmd_scoreboard(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /scoreboard objectives <add <name> <criteria> [display]|remove <name>|list|setdisplay <slot> [name]> | players <set|add|remove <target> <objective> <n>|reset <target> [objective]|get <target> <objective>|list [target]>";
    match (args.first().map(String::as_str), args.get(1).map(String::as_str)) {
        (Some("objectives"), Some("list")) => {
            let boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
            let names: Vec<String> = boards.objectives.values().map(|o| format!("{} ({})", o.name, o.criteria)).collect();
            drop(boards);
            sender.reply(Text::new(if names.is_empty() { "There are no objectives.".to_owned() } else { format!("Objectives: {}", names.join(", ")) }));
        }
        (Some("objectives"), Some("add")) => {
            let name = args.get(2).ok_or(usage)?.clone();
            let criteria = args.get(3).ok_or(usage)?.clone();
            let display_name = if args.len() > 4 { rest(args, 4) } else { name.clone() };
            let objective = Objective {
                name: name.clone(),
                criteria,
                display_name,
                render_type: "integer".into(),
            };
            {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                if boards.objectives.contains_key(&name) {
                    return Err(format!("An objective called '{name}' already exists."));
                }
                boards.objectives.insert(name.clone(), objective.clone());
            }
            server.broadcast(&objective_packet(&objective, cb::ObjectiveMode::Create));
            sender.reply(Text::new(format!("Created new objective {name}.")));
        }
        (Some("objectives"), Some("remove")) => {
            let name = args.get(2).ok_or(usage)?;
            {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                if boards.objectives.remove(name).is_none() {
                    return Err(format!("Unknown objective '{name}'."));
                }
                boards.scores.remove(name);
                boards.displays.retain(|_, o| o != name);
            }
            server.broadcast(&cb::SetObjective {
                name: name.clone(),
                mode: cb::ObjectiveMode::Remove,
                display_name: Text::empty(),
                render_type: 0,
            });
            sender.reply(Text::new(format!("Removed objective {name}.")));
        }
        (Some("objectives"), Some("modify")) => {
            let name = args.get(2).ok_or(usage)?;
            let objective = {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                let objective = boards.objectives.get_mut(name).ok_or_else(|| format!("Unknown objective '{name}'."))?;
                match args.get(3).map(String::as_str) {
                    Some("displayname") => objective.display_name = rest(args, 4),
                    Some("rendertype") => objective.render_type = args.get(4).cloned().unwrap_or_else(|| "integer".into()),
                    _ => return Err(usage.into()),
                }
                objective.clone()
            };
            server.broadcast(&objective_packet(&objective, cb::ObjectiveMode::Update));
            sender.reply(Text::new(format!("Updated objective {name}.")));
        }
        (Some("objectives"), Some("setdisplay")) => {
            let slot = args.get(2).ok_or(usage)?;
            let position = display_slot_id(slot).ok_or_else(|| format!("Unknown display slot '{slot}'."))?;
            let objective = args.get(3).cloned().unwrap_or_default();
            {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                if !objective.is_empty() && !boards.objectives.contains_key(&objective) {
                    return Err(format!("Unknown objective '{objective}'."));
                }
                if objective.is_empty() {
                    boards.displays.remove(slot);
                } else {
                    boards.displays.insert(slot.clone(), objective.clone());
                }
            }
            server.broadcast(&cb::SetDisplayObjective { position, objective });
            sender.reply(Text::new(format!("Set display slot {slot}.")));
        }
        (Some("players"), Some(op @ ("set" | "add" | "remove"))) => {
            let target = args.get(2).ok_or(usage)?;
            let objective = args.get(3).ok_or(usage)?;
            let amount: i32 = args.get(4).ok_or(usage)?.parse().map_err(|_| "The value must be a whole number.".to_owned())?;
            let owners = owners_for(server, sender, target);
            for owner in owners {
                let value = {
                    let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                    if !boards.objectives.contains_key(objective) {
                        return Err(format!("Unknown objective '{objective}'."));
                    }
                    let scores = boards.scores.entry(objective.clone()).or_default();
                    let current = scores.get(&owner).copied().unwrap_or(0);
                    let value = match op {
                        "set" => amount,
                        "add" => current + amount,
                        _ => current - amount,
                    };
                    scores.insert(owner.clone(), value);
                    value
                };
                server.broadcast(&cb::SetScore {
                    owner: owner.clone(),
                    objective: objective.clone(),
                    value,
                });
                sender.reply(Text::new(format!("Set [{objective}] for {owner} to {value}.")));
            }
        }
        (Some("players"), Some("reset")) => {
            let target = args.get(2).ok_or(usage)?;
            let objective = args.get(3).cloned();
            for owner in owners_for(server, sender, target) {
                {
                    let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                    match &objective {
                        Some(o) => {
                            boards.scores.get_mut(o).map(|s| s.remove(&owner));
                        }
                        None => boards.scores.values_mut().for_each(|s| {
                            s.remove(&owner);
                        }),
                    }
                }
                server.broadcast(&cb::ResetScore {
                    owner: owner.clone(),
                    objective: objective.clone(),
                });
                sender.reply(Text::new(format!("Reset scores of {owner}.")));
            }
        }
        (Some("players"), Some("get")) => {
            let target = args.get(2).ok_or(usage)?;
            let objective = args.get(3).ok_or(usage)?;
            let boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
            let value = boards.scores.get(objective).and_then(|s| s.get(target)).copied();
            drop(boards);
            match value {
                Some(v) => sender.reply(Text::new(format!("{target} has {v} [{objective}]."))),
                None => return Err(format!("{target} has no score for {objective}.")),
            }
        }
        (Some("players"), Some("list")) => {
            let boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
            match args.get(2) {
                Some(target) => {
                    let lines: Vec<String> = boards
                        .scores
                        .iter()
                        .filter_map(|(o, s)| s.get(target).map(|v| format!("{o}: {v}")))
                        .collect();
                    drop(boards);
                    sender.reply(Text::new(format!("{target}: {}", if lines.is_empty() { "no scores".to_owned() } else { lines.join(", ") })));
                }
                None => {
                    let mut owners: Vec<&String> = boards.scores.values().flat_map(|s| s.keys()).collect();
                    owners.sort();
                    owners.dedup();
                    let list = owners.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                    drop(boards);
                    sender.reply(Text::new(if list.is_empty() { "There are no tracked players.".to_owned() } else { format!("Tracked: {list}") }));
                }
            }
            return Ok(());
        }
        _ => return Err(usage.into()),
    }
    save_boards(server);
    Ok(())
}

/// Score owners: online players for selectors, otherwise the literal name
/// (vanilla lets any string hold a score).
fn owners_for(server: &Server, sender: &CommandSender, target: &str) -> Vec<String> {
    if target.starts_with('@') {
        resolve_targets(server, sender, target)
            .unwrap_or_default()
            .iter()
            .map(|p| p.name().to_owned())
            .collect()
    } else {
        vec![target.to_owned()]
    }
}

// ---------- teams ----------

fn cmd_team(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /team <add <name> [display]|remove <name>|join <name> [members]|leave <members>|empty <name>|list [name]|modify <name> <option> <value>>";
    match args.first().map(String::as_str) {
        Some("list") => {
            let boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
            let line = match args.get(1) {
                Some(name) => {
                    let team = boards.teams.get(name).ok_or_else(|| format!("Unknown team '{name}'."))?;
                    format!("Team {} has {} member(s): {}", name, team.members.len(), team.members.iter().cloned().collect::<Vec<_>>().join(", "))
                }
                None => {
                    let names: Vec<String> = boards.teams.keys().cloned().collect();
                    if names.is_empty() { "There are no teams.".to_owned() } else { format!("Teams: {}", names.join(", ")) }
                }
            };
            drop(boards);
            sender.reply(Text::new(line));
            return Ok(());
        }
        Some("add") => {
            let name = args.get(1).ok_or(usage)?.clone();
            let team = Team {
                name: name.clone(),
                display_name: if args.len() > 2 { rest(args, 2) } else { name.clone() },
                color: "reset".into(),
                friendly_fire: true,
                see_friendly_invisibles: true,
                name_tag_visibility: "always".into(),
                collision_rule: "always".into(),
                ..Default::default()
            };
            {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                if boards.teams.contains_key(&name) {
                    return Err(format!("A team called '{name}' already exists."));
                }
                boards.teams.insert(name.clone(), team.clone());
            }
            server.broadcast(&cb::SetPlayerTeam {
                name: name.clone(),
                method: cb::TeamMethod::Create {
                    parameters: team_parameters(&team),
                    players: Vec::new(),
                },
            });
            sender.reply(Text::new(format!("Created team {name}.")));
        }
        Some("remove") => {
            let name = args.get(1).ok_or(usage)?;
            if server.boards.lock().unwrap_or_else(|e| e.into_inner()).teams.remove(name).is_none() {
                return Err(format!("Unknown team '{name}'."));
            }
            server.broadcast(&cb::SetPlayerTeam {
                name: name.clone(),
                method: cb::TeamMethod::Remove,
            });
            sender.reply(Text::new(format!("Removed team {name}.")));
        }
        Some("join") => {
            let name = args.get(1).ok_or(usage)?;
            let members: Vec<String> = match args.get(2) {
                Some(selector) => owners_for(server, sender, selector),
                None => vec![sender.player().map(|p| p.name().to_owned()).ok_or("Say who should join.")?],
            };
            let left: Vec<(String, String)> = {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                if !boards.teams.contains_key(name) {
                    return Err(format!("Unknown team '{name}'."));
                }
                let mut left = Vec::new();
                for (team_name, team) in boards.teams.iter_mut() {
                    if team_name == name {
                        continue;
                    }
                    for m in &members {
                        if team.members.remove(m) {
                            left.push((team_name.clone(), m.clone()));
                        }
                    }
                }
                let team = boards.teams.get_mut(name).unwrap();
                team.members.extend(members.iter().cloned());
                left
            };
            for (team_name, member) in left {
                server.broadcast(&cb::SetPlayerTeam {
                    name: team_name,
                    method: cb::TeamMethod::RemovePlayers(vec![member]),
                });
            }
            server.broadcast(&cb::SetPlayerTeam {
                name: name.clone(),
                method: cb::TeamMethod::AddPlayers(members.clone()),
            });
            sender.reply(Text::new(format!("Added {} member(s) to team {name}.", members.len())));
        }
        Some("leave") => {
            let members: Vec<String> = match args.get(1) {
                Some(selector) => owners_for(server, sender, selector),
                None => vec![sender.player().map(|p| p.name().to_owned()).ok_or("Say who should leave.")?],
            };
            let left: Vec<(String, String)> = {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                let mut left = Vec::new();
                for (team_name, team) in boards.teams.iter_mut() {
                    for m in &members {
                        if team.members.remove(m) {
                            left.push((team_name.clone(), m.clone()));
                        }
                    }
                }
                left
            };
            for (team_name, member) in &left {
                server.broadcast(&cb::SetPlayerTeam {
                    name: team_name.clone(),
                    method: cb::TeamMethod::RemovePlayers(vec![member.clone()]),
                });
            }
            sender.reply(Text::new(format!("Removed {} member(s) from their teams.", left.len())));
        }
        Some("empty") => {
            let name = args.get(1).ok_or(usage)?;
            let members: Vec<String> = {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                let team = boards.teams.get_mut(name).ok_or_else(|| format!("Unknown team '{name}'."))?;
                std::mem::take(&mut team.members).into_iter().collect()
            };
            server.broadcast(&cb::SetPlayerTeam {
                name: name.clone(),
                method: cb::TeamMethod::RemovePlayers(members.clone()),
            });
            sender.reply(Text::new(format!("Removed {} member(s) from team {name}.", members.len())));
        }
        Some("modify") => {
            let name = args.get(1).ok_or(usage)?;
            let option = args.get(2).ok_or(usage)?.as_str();
            let value = rest(args, 3);
            let team = {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                let team = boards.teams.get_mut(name).ok_or_else(|| format!("Unknown team '{name}'."))?;
                match option {
                    "displayName" => team.display_name = value,
                    "prefix" => team.prefix = value,
                    "suffix" => team.suffix = value,
                    "color" => {
                        if value != "reset" && color_id(&value).is_none() {
                            return Err(format!("Unknown colour '{value}'."));
                        }
                        team.color = value;
                    }
                    "friendlyFire" => team.friendly_fire = value == "true",
                    "seeFriendlyInvisibles" => team.see_friendly_invisibles = value == "true",
                    "nametagVisibility" => {
                        visibility_id(&value).ok_or_else(|| format!("Unknown visibility '{value}'."))?;
                        team.name_tag_visibility = value;
                    }
                    "collisionRule" => {
                        collision_id(&value).ok_or_else(|| format!("Unknown collision rule '{value}'."))?;
                        team.collision_rule = value;
                    }
                    _ => return Err("Options: displayName, prefix, suffix, color, friendlyFire, seeFriendlyInvisibles, nametagVisibility, collisionRule".into()),
                }
                team.clone()
            };
            server.broadcast(&cb::SetPlayerTeam {
                name: name.clone(),
                method: cb::TeamMethod::Update(team_parameters(&team)),
            });
            sender.reply(Text::new(format!("Updated team {name}.")));
        }
        _ => return Err(usage.into()),
    }
    save_boards(server);
    Ok(())
}

pub fn cmd_teammsg(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: /teammsg <message>".into());
    }
    let name = sender.name();
    let members: Vec<String> = {
        let boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
        boards.team_of(&name).map(|t| t.members.iter().cloned().collect()).ok_or("You are not on a team.")?
    };
    let text = Text::new(format!("[Team] <{name}> {}", rest(args, 0))).color("aqua");
    for member in members {
        if let Some(player) = server.player_by_name(&member) {
            player.send(&cb::SystemChat {
                content: text.clone(),
                overlay: false,
            });
        }
    }
    Ok(())
}

// ---------- tags ----------

fn cmd_tag(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /tag <targets> <add|remove|list> [tag]";
    let targets = resolve_targets(server, sender, args.first().ok_or(usage)?)?;
    match args.get(1).map(String::as_str) {
        Some("add") => {
            let tag = args.get(2).ok_or(usage)?;
            for t in &targets {
                t.lock().tags.insert(tag.clone());
            }
            sender.reply(Text::new(format!("Added tag '{tag}' to {} player(s).", targets.len())));
        }
        Some("remove") => {
            let tag = args.get(2).ok_or(usage)?;
            for t in &targets {
                t.lock().tags.remove(tag);
            }
            sender.reply(Text::new(format!("Removed tag '{tag}' from {} player(s).", targets.len())));
        }
        Some("list") => {
            for t in &targets {
                let tags: Vec<String> = t.lock().tags.iter().cloned().collect();
                sender.reply(Text::new(format!("{} has {} tag(s): {}", t.name(), tags.len(), tags.join(", "))));
            }
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

// ---------- trigger ----------

fn cmd_trigger(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /trigger <objective> [add|set <value>]";
    let player = sender.player().cloned().ok_or("Only players can trigger.")?;
    let objective = args.first().ok_or(usage)?;
    let amount: i32 = match (args.get(1).map(String::as_str), args.get(2)) {
        (None, _) => 1,
        (Some("add" | "set"), Some(v)) => v.parse().map_err(|_| "The value must be a whole number.".to_owned())?,
        _ => return Err(usage.into()),
    };
    let value = {
        let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
        let is_trigger = boards.objectives.get(objective).is_some_and(|o| o.criteria == "trigger");
        if !is_trigger {
            return Err("You can only trigger objectives with the 'trigger' criteria.".into());
        }
        let scores = boards.scores.entry(objective.clone()).or_default();
        let current = scores.get(player.name()).copied().unwrap_or(0);
        let value = if args.get(1).map(String::as_str) == Some("set") { amount } else { current + amount };
        scores.insert(player.name().to_owned(), value);
        value
    };
    server.broadcast(&cb::SetScore {
        owner: player.name().to_owned(),
        objective: objective.clone(),
        value,
    });
    save_boards(server);
    sender.reply(Text::new(format!("Triggered [{objective}].")));
    Ok(())
}

// ---------- boss bars ----------

fn cmd_bossbar(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /bossbar <add <id> <name>|remove <id>|list|get <id> <value|max|players|visible>|set <id> <name|color|style|value|max|visible|players> ...>";
    match args.first().map(String::as_str) {
        Some("list") => {
            let boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<String> = boards.boss_bars.keys().cloned().collect();
            drop(boards);
            sender.reply(Text::new(if ids.is_empty() { "There are no boss bars.".to_owned() } else { format!("Boss bars: {}", ids.join(", ")) }));
            return Ok(());
        }
        Some("add") => {
            let id = args.get(1).ok_or(usage)?.clone();
            let name = text_arg(args, 2, &id).to_plain();
            let bar = BossBar {
                id: id.clone(),
                uuid: Uuid::new_v4(),
                name,
                color: "white".into(),
                style: "progress".into(),
                value: 0,
                max: 100,
                visible: true,
                players: Default::default(),
            };
            let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
            if boards.boss_bars.contains_key(&id) {
                return Err(format!("A boss bar called '{id}' already exists."));
            }
            boards.boss_bars.insert(id.clone(), bar);
            drop(boards);
            sender.reply(Text::new(format!("Created custom boss bar {id}.")));
        }
        Some("remove") => {
            let id = args.get(1).ok_or(usage)?;
            let bar = server.boards.lock().unwrap_or_else(|e| e.into_inner()).boss_bars.remove(id).ok_or_else(|| format!("Unknown boss bar '{id}'."))?;
            for uuid in &bar.players {
                if let Some(p) = server.player(*uuid) {
                    p.send(&cb::BossEvent {
                        uuid: bar.uuid,
                        action: cb::BossEventAction::Remove,
                    });
                }
            }
            sender.reply(Text::new(format!("Removed custom boss bar {id}.")));
        }
        Some("get") => {
            let id = args.get(1).ok_or(usage)?;
            let boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
            let bar = boards.boss_bars.get(id).ok_or_else(|| format!("Unknown boss bar '{id}'."))?;
            let line = match args.get(2).map(String::as_str) {
                Some("value") => format!("{} has a value of {}.", bar.name, bar.value),
                Some("max") => format!("{} has a maximum of {}.", bar.name, bar.max),
                Some("visible") => format!("{} is {}.", bar.name, if bar.visible { "shown" } else { "hidden" }),
                Some("players") => format!("{} is shown to {} player(s).", bar.name, bar.players.len()),
                _ => return Err(usage.into()),
            };
            drop(boards);
            sender.reply(Text::new(line));
            return Ok(());
        }
        Some("set") => {
            let id = args.get(1).ok_or(usage)?;
            let what = args.get(2).ok_or(usage)?.as_str();
            let (bar, removed_from): (BossBar, Vec<Uuid>) = {
                let mut boards = server.boards.lock().unwrap_or_else(|e| e.into_inner());
                let bar = boards.boss_bars.get_mut(id).ok_or_else(|| format!("Unknown boss bar '{id}'."))?;
                let mut removed = Vec::new();
                match what {
                    "name" => bar.name = text_arg(args, 3, &bar.id.clone()).to_plain(),
                    "color" => bar.color = args.get(3).cloned().unwrap_or_else(|| "white".into()),
                    "style" => bar.style = args.get(3).cloned().unwrap_or_else(|| "progress".into()),
                    "value" => bar.value = args.get(3).and_then(|v| v.parse().ok()).ok_or("The value must be a whole number.")?,
                    "max" => bar.max = args.get(3).and_then(|v| v.parse().ok()).ok_or("The maximum must be a whole number.")?,
                    "visible" => bar.visible = args.get(3).map(String::as_str) == Some("true"),
                    "players" => {
                        let wanted: std::collections::BTreeSet<Uuid> = match args.get(3) {
                            Some(selector) => resolve_targets(server, sender, selector)?.iter().map(|p| p.uuid).collect(),
                            None => Default::default(),
                        };
                        removed = bar.players.difference(&wanted).copied().collect();
                        bar.players = wanted;
                    }
                    _ => return Err(usage.into()),
                }
                (bar.clone(), removed)
            };
            // Tell everyone the bar is shown to; re-adding is the simplest way to sync every field.
            for uuid in removed_from {
                if let Some(p) = server.player(uuid) {
                    p.send(&cb::BossEvent {
                        uuid: bar.uuid,
                        action: cb::BossEventAction::Remove,
                    });
                }
            }
            for uuid in &bar.players {
                if let Some(p) = server.player(*uuid) {
                    p.send(&cb::BossEvent {
                        uuid: bar.uuid,
                        action: cb::BossEventAction::Remove,
                    });
                    if bar.visible {
                        p.send(&boss_add_packet(&bar));
                    }
                }
            }
            sender.reply(Text::new(format!("Updated boss bar {id}.")));
        }
        _ => return Err(usage.into()),
    }
    save_boards(server);
    Ok(())
}
