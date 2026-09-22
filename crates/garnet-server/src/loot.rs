//! What breaking a block drops, from Mojang's own loot tables
//! (`datapack/minecraft/loot_table/blocks/<block>.json`), so stone drops
//! cobblestone, ores drop their raw material, leaves sometimes a sapling.
//!
//! The evaluator covers what block tables use: pools with rolls, item and
//! alternatives entries, the conditions (silk touch or shears on the tool,
//! block state matches, fortune table bonus, random chance, and/or/not) and
//! the count modifiers. Anything it does not understand fails closed, so
//! an odd table drops nothing rather than something wrong.

use garnet_data::GameData;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

/// What the player broke the block with.
pub struct Tool<'a> {
    pub item_name: Option<&'a str>,
    pub silk_touch: bool,
    pub fortune: i32,
}

pub struct LootTables {
    dir: PathBuf,
    cache: Mutex<HashMap<String, Option<Value>>>,
}

impl LootTables {
    pub fn new(data: &GameData) -> Self {
        Self::in_dir(data, "blocks")
    }

    /// The tables for what mobs leave behind.
    pub fn entities(data: &GameData) -> Self {
        Self::in_dir(data, "entities")
    }

    fn in_dir(data: &GameData, sub: &str) -> Self {
        Self {
            dir: data.version_dir.join("datapack").join("minecraft").join("loot_table").join(sub),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Item names and counts dropped by breaking `block` in `state`.
    pub fn drops(&self, block_name: &str, state_props: &BTreeMap<String, String>, tool: &Tool) -> Vec<(String, i32)> {
        let short = block_name.strip_prefix("minecraft:").unwrap_or(block_name).to_owned();
        let table = {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .entry(short.clone())
                .or_insert_with(|| {
                    std::fs::read_to_string(self.dir.join(format!("{short}.json")))
                        .ok()
                        .and_then(|t| serde_json::from_str(&t).ok())
                })
                .clone()
        };
        let Some(table) = table else { return Vec::new() };
        let ctx = Context {
            block: block_name,
            props: state_props,
            tool,
        };
        let mut out = Vec::new();
        for pool in table.get("pools").and_then(Value::as_array).into_iter().flatten() {
            if !condition_ok(pool.get("condition"), &ctx) {
                continue;
            }
            let rolls = number(pool.get("rolls")).max(0);
            for _ in 0..rolls {
                pick_entry(pool.get("entries").and_then(Value::as_array).unwrap_or(&Vec::new()), &ctx, &mut out);
            }
        }
        out
    }
}

struct Context<'a> {
    block: &'a str,
    props: &'a BTreeMap<String, String>,
    tool: &'a Tool<'a>,
}

/// Picks one entry from a pool by weight (among those whose conditions
/// pass) and expands it.
fn pick_entry(entries: &[Value], ctx: &Context, out: &mut Vec<(String, i32)>) {
    let usable: Vec<&Value> = entries.iter().filter(|e| condition_ok(e.get("condition"), ctx)).collect();
    if usable.is_empty() {
        return;
    }
    let total: i64 = usable.iter().map(|e| e.get("weight").and_then(Value::as_i64).unwrap_or(1)).sum();
    let mut roll = rand::random_range(0..total.max(1));
    for entry in usable {
        roll -= entry.get("weight").and_then(Value::as_i64).unwrap_or(1);
        if roll < 0 {
            expand(entry, ctx, out);
            return;
        }
    }
}

fn expand(entry: &Value, ctx: &Context, out: &mut Vec<(String, i32)>) {
    match entry.get("type").and_then(Value::as_str).unwrap_or("") {
        "minecraft:item" => {
            let Some(name) = entry.get("name").and_then(Value::as_str) else { return };
            let count = apply_modifiers(1, entry.get("modifier"), ctx);
            if count > 0 {
                out.push((name.to_owned(), count));
            }
        }
        "minecraft:alternatives" => {
            // The first child whose conditions pass.
            for child in entry.get("children").and_then(Value::as_array).into_iter().flatten() {
                if condition_ok(child.get("condition"), ctx) {
                    expand(child, ctx, out);
                    return;
                }
            }
        }
        "minecraft:group" | "minecraft:sequence" => {
            for child in entry.get("children").and_then(Value::as_array).into_iter().flatten() {
                if condition_ok(child.get("condition"), ctx) {
                    expand(child, ctx, out);
                } else if entry["type"] == "minecraft:sequence" {
                    return;
                }
            }
        }
        _ => {} // empty, nested tables, dynamic drops (shulker boxes): nothing
    }
}

/// `modifier` is a single object or a list; only counts matter here.
fn apply_modifiers(mut count: i32, modifiers: Option<&Value>, ctx: &Context) -> i32 {
    let list: Vec<&Value> = match modifiers {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(v @ Value::Object(_)) => vec![v],
        _ => Vec::new(),
    };
    for m in list {
        if !condition_ok(m.get("condition"), ctx) {
            continue;
        }
        match m.get("type").and_then(Value::as_str).unwrap_or("") {
            "minecraft:set_count" => {
                let value = number(m.get("count"));
                count = if m.get("add").and_then(Value::as_bool).unwrap_or(false) { count + value } else { value };
            }
            "minecraft:limit_count" => {
                if let Some(limit) = m.get("limit") {
                    let min = limit.get("min").and_then(Value::as_i64).map(|v| v as i32);
                    let max = limit.get("max").and_then(Value::as_i64).map(|v| v as i32);
                    if let Some(min) = min {
                        count = count.max(min);
                    }
                    if let Some(max) = max {
                        count = count.min(max);
                    }
                }
            }
            "minecraft:apply_bonus" => {
                let fortune = ctx.tool.fortune;
                if fortune > 0 {
                    match m.get("formula").and_then(Value::as_str).unwrap_or("") {
                        "minecraft:ore_drops" => count *= 1 + rand::random_range(0..=fortune).max(0),
                        "minecraft:uniform_bonus_count" => {
                            let mult = m.get("parameters").and_then(|p| p.get("bonusMultiplier")).and_then(Value::as_i64).unwrap_or(1) as i32;
                            count += rand::random_range(0..=fortune * mult);
                        }
                        _ => {}
                    }
                }
            }
            _ => {} // explosion_decay, copy_state, copy_components...
        }
    }
    count
}

/// A number provider: a plain number, or `{type: uniform, min, max}`.
fn number(value: Option<&Value>) -> i32 {
    match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0).round() as i32,
        Some(Value::Object(o)) => {
            let min = o.get("min").and_then(Value::as_f64).unwrap_or(0.0);
            let max = o.get("max").and_then(Value::as_f64).unwrap_or(min);
            if o.get("type").and_then(Value::as_str) == Some("minecraft:binomial") {
                let n = o.get("n").and_then(Value::as_i64).unwrap_or(1);
                let p = o.get("p").and_then(Value::as_f64).unwrap_or(0.5);
                return (0..n).filter(|_| rand::random::<f64>() < p).count() as i32;
            }
            rand::random_range(min.round() as i32..=max.round().max(min.round()) as i32)
        }
        _ => 1,
    }
}

fn condition_ok(condition: Option<&Value>, ctx: &Context) -> bool {
    let Some(condition) = condition else { return true };
    match condition {
        Value::Null => true,
        // A named predicate from data/minecraft/predicate.
        Value::String(name) => match name.as_str() {
            "minecraft:tool/can_silk_touch" => ctx.tool.silk_touch,
            "minecraft:tool/can_shear" => ctx.tool.item_name == Some("minecraft:shears"),
            _ => false,
        },
        Value::Array(list) => list.iter().all(|c| condition_ok(Some(c), ctx)),
        Value::Object(o) => match o.get("type").and_then(Value::as_str).unwrap_or("") {
            "minecraft:survives_explosion" | "minecraft:location_check" => true,
            "minecraft:entity_properties" => o.get("predicate").map(|p| p.as_object().is_some_and(|m| m.is_empty())).unwrap_or(false),
            "minecraft:inverted" => !condition_ok(o.get("term"), ctx),
            "minecraft:any_of" => o.get("terms").and_then(Value::as_array).is_some_and(|t| t.iter().any(|c| condition_ok(Some(c), ctx))),
            "minecraft:all_of" => o.get("terms").and_then(Value::as_array).is_some_and(|t| t.iter().all(|c| condition_ok(Some(c), ctx))),
            "minecraft:random_chance" => rand::random::<f64>() < o.get("chance").and_then(Value::as_f64).unwrap_or(0.0),
            "minecraft:table_bonus" => {
                let chances = o.get("chances").and_then(Value::as_array);
                let level = ctx.tool.fortune.max(0) as usize;
                let chance = chances
                    .and_then(|c| c.get(level.min(c.len().saturating_sub(1))))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                rand::random::<f64>() < chance
            }
            "minecraft:match_block" | "minecraft:block_state_property" => {
                let block = o.get("blocks").or_else(|| o.get("block")).and_then(Value::as_str).unwrap_or("");
                if !block.is_empty() && block != ctx.block {
                    return false;
                }
                let wanted = o.get("state").or_else(|| o.get("properties")).and_then(Value::as_object);
                wanted.is_none_or(|w| {
                    w.iter().all(|(k, v)| {
                        let expected = match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        ctx.props.get(k) == Some(&expected)
                    })
                })
            }
            "minecraft:match_tool" => {
                let wants_silk = condition.to_string().contains("silk_touch");
                let wants_shears = condition.to_string().contains("shears");
                (!wants_silk || ctx.tool.silk_touch) && (!wants_shears || ctx.tool.item_name == Some("minecraft:shears"))
            }
            _ => false,
        },
        _ => false,
    }
}
