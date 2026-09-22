//! Data pack functions: `.mcfunction` files under
//! `world/datapacks/<pack>/data/<namespace>/function/`, the `#minecraft:load`
//! and `#minecraft:tick` function tags, `/function`, `/schedule`, `/return`
//! and `/datapack`.
//!
//! A function is a list of commands run one after another as whoever
//! called it (with that caller's permissions). `/return` stops the current
//! function early with a value; `/schedule` runs a function some ticks from
//! now, saved with the world so it survives restarts.

use crate::commands::{Command, CommandRegistry, CommandSender, Handler};
use crate::server::Server;
use garnet_protocol::Text;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;
use std::path::PathBuf;
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
    add("function", "Runs a data pack function", "/function <namespace:name>", cmd_function);
    add("schedule", "Runs a function later", "/schedule function <name> <ticks|Ns|Nd> [append|replace] | /schedule clear <name>", cmd_schedule);
    add("return", "Stops the current function", "/return <value> | /return fail", cmd_return);
    add("datapack", "Lists, enables and disables data packs", "/datapack <list [available|enabled]|enable <name>|disable <name>>", cmd_datapack);
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduledFunction {
    pub function: String,
    pub due_tick: u64,
}

/// One data pack on disk.
pub struct DataPack {
    pub name: String,
    pub root: PathBuf,
    pub description: String,
}

pub fn packs_dir(server: &Server) -> PathBuf {
    server.root.join(&server.config().world.name).join("datapacks")
}

/// Every pack folder (or zip is not supported yet) in `world/datapacks`.
pub fn available_packs(server: &Server) -> Vec<DataPack> {
    let dir = packs_dir(server);
    let mut packs = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return packs };
    for entry in entries.flatten() {
        let root = entry.path();
        if !root.join("pack.mcmeta").exists() {
            continue;
        }
        let description = std::fs::read_to_string(root.join("pack.mcmeta"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("pack")?.get("description").cloned())
            .map(|d| match d {
                Value::String(s) => s,
                other => Text::from_json(&other).to_plain(),
            })
            .unwrap_or_default();
        packs.push(DataPack {
            name: entry.file_name().to_string_lossy().to_string(),
            root,
            description,
        });
    }
    packs.sort_by(|a, b| a.name.cmp(&b.name));
    packs
}

pub fn enabled_packs(server: &Server) -> Vec<DataPack> {
    let disabled = server.rules.read().unwrap_or_else(|e| e.into_inner()).disabled_datapacks.clone();
    available_packs(server).into_iter().filter(|p| !disabled.contains(&p.name)).collect()
}

/// Finds `namespace:path` in the enabled packs (later packs win).
fn find_function(server: &Server, id: &str) -> Option<PathBuf> {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    enabled_packs(server)
        .into_iter()
        .rev()
        .map(|p| p.root.join("data").join(namespace).join("function").join(format!("{path}.mcfunction")))
        .find(|f| f.exists())
}

/// Functions listed in a function tag such as `#minecraft:tick`.
fn tagged_functions(server: &Server, tag: &str) -> Vec<String> {
    let (namespace, path) = tag.trim_start_matches('#').split_once(':').unwrap_or(("minecraft", tag));
    let mut out = Vec::new();
    for pack in enabled_packs(server) {
        let file = pack.root.join("data").join(namespace).join("tags").join("function").join(format!("{path}.json"));
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let Ok(json) = serde_json::from_str::<Value>(&text) else { continue };
        if json.get("replace").and_then(Value::as_bool).unwrap_or(false) {
            out.clear();
        }
        for value in json.get("values").and_then(Value::as_array).into_iter().flatten() {
            let name = match value {
                Value::String(s) => s.clone(),
                Value::Object(o) => o.get("id").and_then(Value::as_str).unwrap_or("").to_owned(),
                _ => continue,
            };
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

thread_local! {
    /// Set by `/return` while a function runs; read by the runner.
    static RETURNED: RefCell<Option<Option<i32>>> = const { RefCell::new(None) };
    static DEPTH: RefCell<u32> = const { RefCell::new(0) };
}

const MAX_DEPTH: u32 = 64;

/// Runs a function as `sender`; returns the number of commands run, or the
/// `/return` value when the function returned.
pub fn run_function(server: &Arc<Server>, sender: &CommandSender, id: &str) -> Result<i32, String> {
    let path = find_function(server, id).ok_or_else(|| format!("Unknown function '{id}'."))?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    if DEPTH.with(|d| *d.borrow()) >= MAX_DEPTH {
        return Err("Functions are nested too deeply.".into());
    }
    DEPTH.with(|d| *d.borrow_mut() += 1);
    let mut ran = 0;
    let mut result = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('$') {
            // Macro lines need arguments, which /function does not pass yet.
            continue;
        }
        server.commands.execute(server, sender, line);
        ran += 1;
        if let Some(value) = RETURNED.with(|r| r.borrow_mut().take()) {
            result = Some(value);
            break;
        }
    }
    DEPTH.with(|d| *d.borrow_mut() -= 1);
    match result {
        Some(Some(value)) => Ok(value),
        Some(None) => Err(format!("Function {id} failed.")),
        None => Ok(ran),
    }
}

fn cmd_function(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let id = args.first().ok_or("Usage: /function <namespace:name>")?;
    if let Some(tag) = id.strip_prefix('#') {
        let functions = tagged_functions(server, tag);
        if functions.is_empty() {
            return Err(format!("No functions are tagged {id}."));
        }
        let mut total = 0;
        for f in &functions {
            total += run_function(server, sender, f)?;
        }
        sender.reply(Text::new(format!("Ran {} function(s) with {total} command(s).", functions.len())));
        return Ok(());
    }
    let count = run_function(server, sender, id)?;
    sender.reply(Text::new(format!("Ran function {id} ({count} command(s)).")));
    Ok(())
}

fn cmd_return(_: &Arc<Server>, _: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /return <value> | /return fail";
    let value = match args.first().map(String::as_str) {
        Some("fail") => None,
        Some(v) => Some(v.parse::<i32>().map_err(|_| usage.to_owned())?),
        None => return Err(usage.into()),
    };
    if DEPTH.with(|d| *d.borrow()) == 0 {
        return Err("/return only works inside a function.".into());
    }
    RETURNED.with(|r| *r.borrow_mut() = Some(value));
    Ok(())
}

fn parse_time(arg: &str) -> Result<u64, String> {
    let (number, unit) = match arg.chars().last() {
        Some('t') => (&arg[..arg.len() - 1], 1),
        Some('s') => (&arg[..arg.len() - 1], 20),
        Some('d') => (&arg[..arg.len() - 1], 24_000),
        _ => (arg, 1),
    };
    let n: f64 = number.parse().map_err(|_| format!("'{arg}' is not a time; use ticks, or a number with t, s or d."))?;
    Ok((n * unit as f64).round().max(1.0) as u64)
}

fn cmd_schedule(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /schedule function <name> <time> [append|replace] | /schedule clear <name>";
    match args.first().map(String::as_str) {
        Some("function") => {
            let name = args.get(1).ok_or(usage)?.clone();
            if find_function(server, &name).is_none() {
                return Err(format!("Unknown function '{name}'."));
            }
            let delay = parse_time(args.get(2).ok_or(usage)?)?;
            let append = args.get(3).map(String::as_str) == Some("append");
            let due = server.current_tick() + delay;
            {
                let mut rules = server.rules.write().unwrap_or_else(|e| e.into_inner());
                if !append {
                    rules.scheduled_functions.retain(|s| s.function != name);
                }
                rules.scheduled_functions.push(ScheduledFunction { function: name.clone(), due_tick: due });
            }
            crate::vanilla_commands::save_rules(server);
            sender.reply(Text::new(format!("Scheduled function '{name}' in {delay} ticks.")));
        }
        Some("clear") => {
            let name = args.get(1).ok_or(usage)?;
            let removed = {
                let mut rules = server.rules.write().unwrap_or_else(|e| e.into_inner());
                let before = rules.scheduled_functions.len();
                rules.scheduled_functions.retain(|s| &s.function != name);
                before - rules.scheduled_functions.len()
            };
            if removed == 0 {
                return Err(format!("No schedule for '{name}'."));
            }
            crate::vanilla_commands::save_rules(server);
            sender.reply(Text::new(format!("Removed {removed} schedule(s) for '{name}'.")));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

fn cmd_datapack(server: &Arc<Server>, sender: &CommandSender, args: &[String]) -> Result<(), String> {
    let usage = "Usage: /datapack list [available|enabled] | /datapack enable <name> | /datapack disable <name>";
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let disabled = server.rules.read().unwrap_or_else(|e| e.into_inner()).disabled_datapacks.clone();
            let packs = available_packs(server);
            let (on, off): (Vec<_>, Vec<_>) = packs.iter().partition(|p| !disabled.contains(&p.name));
            let show = |list: &[&DataPack]| {
                if list.is_empty() {
                    "none".to_owned()
                } else {
                    list.iter().map(|p| format!("[{}] {}", p.name, p.description).trim().to_owned()).collect::<Vec<_>>().join(", ")
                }
            };
            match args.get(1).map(String::as_str) {
                Some("available") => sender.reply(Text::new(format!("Available packs: {}", show(&off)))),
                Some("enabled") => sender.reply(Text::new(format!("Enabled packs: {}", show(&on)))),
                _ => {
                    sender.reply(Text::new(format!("Enabled packs: {}", show(&on))));
                    sender.reply(Text::new(format!("Available packs: {}", show(&off))));
                }
            }
            sender.reply(Text::new(format!("Packs live in {}.", packs_dir(server).display())).color("gray"));
        }
        Some(action @ ("enable" | "disable")) => {
            let name = args.get(1).ok_or(usage)?;
            if !available_packs(server).iter().any(|p| &p.name == name) {
                return Err(format!("Unknown data pack '{name}'."));
            }
            {
                let mut rules = server.rules.write().unwrap_or_else(|e| e.into_inner());
                rules.disabled_datapacks.retain(|n| n != name);
                if action == "disable" {
                    rules.disabled_datapacks.push(name.clone());
                }
            }
            crate::vanilla_commands::save_rules(server);
            refresh_presence(server);
            if action == "enable" {
                run_tag(server, "minecraft:load");
            }
            sender.reply(Text::new(format!("{}d data pack {name}.", capitalize(action))));
        }
        _ => return Err(usage.into()),
    }
    Ok(())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Runs every function in a tag as the console.
pub fn run_tag(server: &Arc<Server>, tag: &str) {
    for function in tagged_functions(server, tag) {
        if let Err(err) = run_function(server, &CommandSender::Console, &function) {
            tracing::warn!("function {function} ({tag}): {err}");
        }
    }
}

/// Called once per server tick: `#minecraft:tick` and due schedules.
pub fn tick(server: &Arc<Server>, tick: u64) {
    let due: Vec<String> = {
        let mut rules = server.rules.write().unwrap_or_else(|e| e.into_inner());
        let (due, later): (Vec<_>, Vec<_>) = rules.scheduled_functions.drain(..).partition(|s| s.due_tick <= tick);
        rules.scheduled_functions = later;
        due.into_iter().map(|s| s.function).collect()
    };
    for function in due {
        if let Err(err) = run_function(server, &CommandSender::Console, &function) {
            tracing::warn!("scheduled function {function}: {err}");
        }
    }
    // The tick tag every tick; skipped entirely when there are no packs.
    if has_packs(server) {
        run_tag(server, "minecraft:tick");
    }
}

fn has_packs(server: &Server) -> bool {
    server.datapacks_present.load(std::sync::atomic::Ordering::Relaxed)
}

/// Checks the datapacks folder once at start (and after /datapack changes).
pub fn refresh_presence(server: &Server) {
    let present = !enabled_packs(server).is_empty();
    server.datapacks_present.store(present, std::sync::atomic::Ordering::Relaxed);
}
