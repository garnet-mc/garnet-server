//! Crafting, from Mojang's own recipes (`datapack/minecraft/recipe/*.json`).
//!
//! Only the two grid recipes are loaded here: shaped ones, which have to
//! line up (and may be mirrored, as vanilla allows), and shapeless ones,
//! which only care that the right items are present. Ingredients may name
//! an item, a tag or a list of either. Smelting and the special recipes
//! that are written in code rather than data are left for the furnace and
//! for later.

use garnet_data::GameData;
use serde_json::Value;

/// One slot of a recipe: what may go in it.
#[derive(Debug, Clone)]
pub enum Ingredient {
    Item(String),
    Tag(String),
    AnyOf(Vec<Ingredient>),
}

impl Ingredient {
    pub fn parse(value: &Value) -> Option<Ingredient> {
        match value {
            Value::String(s) => Some(match s.strip_prefix('#') {
                Some(tag) => Ingredient::Tag(with_namespace(tag)),
                None => Ingredient::Item(with_namespace(s)),
            }),
            Value::Array(items) => {
                let parts: Vec<Ingredient> = items.iter().filter_map(Ingredient::parse).collect();
                (!parts.is_empty()).then_some(Ingredient::AnyOf(parts))
            }
            // Older packs wrote {"item": "..."} or {"tag": "..."}.
            Value::Object(map) => map
                .get("item")
                .or_else(|| map.get("tag"))
                .and_then(Ingredient::parse)
                .or_else(|| map.get("items").and_then(Ingredient::parse)),
            _ => None,
        }
    }

    pub fn matches(&self, data: &GameData, item: &str) -> bool {
        match self {
            Ingredient::Item(name) => name == item,
            Ingredient::Tag(tag) => in_tag(data, tag, item),
            Ingredient::AnyOf(parts) => parts.iter().any(|p| p.matches(data, item)),
        }
    }
}

#[derive(Debug, Clone)]
struct Shaped {
    width: usize,
    height: usize,
    /// Row-major, `None` where the pattern has a space.
    cells: Vec<Option<Ingredient>>,
    result: (String, i32),
}

#[derive(Debug, Clone)]
struct Shapeless {
    ingredients: Vec<Ingredient>,
    result: (String, i32),
}

/// One thing a furnace can turn into another.
#[derive(Debug, Clone)]
pub struct Cooking {
    ingredient: Ingredient,
    pub result: String,
    pub time: i32,
    /// Which furnaces will do it: a blast furnace only takes ores, a
    /// smoker only food.
    pub blasting: bool,
    pub smoking: bool,
}

pub struct Recipes {
    shaped: Vec<Shaped>,
    shapeless: Vec<Shapeless>,
    cooking: Vec<Cooking>,
}

impl Recipes {
    /// Reads every crafting recipe in the data pack.
    pub fn load(data: &GameData) -> Self {
        let dir = data.version_dir.join("datapack").join("minecraft").join("recipe");
        let mut shaped = Vec::new();
        let mut shapeless = Vec::new();
        let mut cooking = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("no crafting recipes in {}: {e}", dir.display());
                return Self { shaped, shapeless, cooking };
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(json) = serde_json::from_str::<Value>(&text) else { continue };
            match json.get("type").and_then(Value::as_str).unwrap_or_default() {
                "minecraft:crafting_shaped" => {
                    if let Some(recipe) = parse_shaped(&json) {
                        shaped.push(recipe);
                    }
                }
                "minecraft:crafting_shapeless" => {
                    if let Some(recipe) = parse_shapeless(&json) {
                        shapeless.push(recipe);
                    }
                }
                kind @ ("minecraft:smelting" | "minecraft:blasting" | "minecraft:smoking" | "minecraft:campfire_cooking") => {
                    if let Some(recipe) = parse_cooking(&json, kind) {
                        cooking.push(recipe);
                    }
                }
                _ => {}
            }
        }
        tracing::info!(
            "loaded {} shaped, {} shapeless and {} cooking recipes",
            shaped.len(),
            shapeless.len(),
            cooking.len()
        );
        Self {
            shaped,
            shapeless,
            cooking,
        }
    }

    /// What this grid makes, if anything. `grid` is row-major, `None` for
    /// an empty slot.
    pub fn result(&self, data: &GameData, grid: &[Option<String>], width: usize) -> Option<(String, i32)> {
        let height = grid.len() / width.max(1);
        if let Some(found) = self.match_shaped(data, grid, width, height) {
            return Some(found);
        }
        self.match_shapeless(data, grid)
    }

    fn match_shaped(&self, data: &GameData, grid: &[Option<String>], width: usize, height: usize) -> Option<(String, i32)> {
        for recipe in &self.shaped {
            if recipe.width > width || recipe.height > height {
                continue;
            }
            for dy in 0..=(height - recipe.height) {
                for dx in 0..=(width - recipe.width) {
                    for mirrored in [false, true] {
                        if fits(data, recipe, grid, width, dx, dy, mirrored) {
                            return Some(recipe.result.clone());
                        }
                    }
                }
            }
        }
        None
    }

    fn match_shapeless(&self, data: &GameData, grid: &[Option<String>]) -> Option<(String, i32)> {
        let present: Vec<&str> = grid.iter().flatten().map(String::as_str).collect();
        for recipe in &self.shapeless {
            if recipe.ingredients.len() != present.len() {
                continue;
            }
            let mut used = vec![false; present.len()];
            let matched = recipe.ingredients.iter().all(|ingredient| {
                for (i, item) in present.iter().enumerate() {
                    if !used[i] && ingredient.matches(data, item) {
                        used[i] = true;
                        return true;
                    }
                }
                false
            });
            if matched {
                return Some(recipe.result.clone());
            }
        }
        None
    }
}

/// Whether a recipe sits in the grid at this offset.
fn fits(data: &GameData, recipe: &Shaped, grid: &[Option<String>], width: usize, dx: usize, dy: usize, mirrored: bool) -> bool {
    let height = grid.len() / width;
    for y in 0..height {
        for x in 0..width {
            let inside = x >= dx && x < dx + recipe.width && y >= dy && y < dy + recipe.height;
            let wanted = if inside {
                let mut rx = x - dx;
                if mirrored {
                    rx = recipe.width - 1 - rx;
                }
                recipe.cells[(y - dy) * recipe.width + rx].as_ref()
            } else {
                None
            };
            match (wanted, grid[y * width + x].as_deref()) {
                (None, None) => {}
                (Some(ingredient), Some(item)) if ingredient.matches(data, item) => {}
                _ => return false,
            }
        }
    }
    true
}

/// What a furnace of this sort makes of an item, if anything.
impl Recipes {
    pub fn smelt(&self, data: &GameData, item: &str, furnace: &str) -> Option<&Cooking> {
        self.cooking.iter().find(|recipe| {
            let allowed = match furnace {
                "minecraft:blast_furnace" => recipe.blasting,
                "minecraft:smoker" => recipe.smoking,
                _ => !recipe.blasting && !recipe.smoking,
            };
            allowed && recipe.ingredient.matches(data, item)
        })
    }
}

fn parse_cooking(json: &Value, kind: &str) -> Option<Cooking> {
    Some(Cooking {
        ingredient: Ingredient::parse(json.get("ingredient")?)?,
        result: parse_result(json)?.0,
        time: json.get("cookingtime").and_then(Value::as_i64).unwrap_or(200) as i32,
        blasting: kind == "minecraft:blasting",
        smoking: kind == "minecraft:smoking" || kind == "minecraft:campfire_cooking",
    })
}

fn parse_shaped(json: &Value) -> Option<Shaped> {
    let pattern: Vec<&str> = json.get("pattern")?.as_array()?.iter().filter_map(Value::as_str).collect();
    let height = pattern.len();
    let width = pattern.iter().map(|row| row.chars().count()).max().unwrap_or(0);
    if width == 0 || height == 0 || width > 3 || height > 3 {
        return None;
    }
    let key = json.get("key")?.as_object()?;
    let mut cells = vec![None; width * height];
    for (y, row) in pattern.iter().enumerate() {
        for (x, symbol) in row.chars().enumerate() {
            if symbol == ' ' {
                continue;
            }
            let ingredient = Ingredient::parse(key.get(&symbol.to_string())?)?;
            cells[y * width + x] = Some(ingredient);
        }
    }
    Some(Shaped {
        width,
        height,
        cells,
        result: parse_result(json)?,
    })
}

fn parse_shapeless(json: &Value) -> Option<Shapeless> {
    let list = json.get("ingredients")?.as_array()?;
    let ingredients: Vec<Ingredient> = list.iter().filter_map(Ingredient::parse).collect();
    if ingredients.len() != list.len() || ingredients.is_empty() {
        return None;
    }
    Some(Shapeless {
        ingredients,
        result: parse_result(json)?,
    })
}

fn parse_result(json: &Value) -> Option<(String, i32)> {
    let result = json.get("result")?;
    let id = result.get("id").and_then(Value::as_str)?;
    let count = result.get("count").and_then(Value::as_i64).unwrap_or(1) as i32;
    Some((with_namespace(id), count))
}

fn with_namespace(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// Whether an item belongs to a tag, by name.
fn in_tag(data: &GameData, tag: &str, item: &str) -> bool {
    let Some(members) = data.tags.members("minecraft:item", tag) else {
        return false;
    };
    let Some(id) = data.registries.id_of("item", item) else {
        return false;
    };
    members.contains(&id)
}

/// The item names in a grid of stacks, for the matcher.
pub fn grid_names(server: &crate::server::Server, stacks: &[garnet_protocol::packets::play::items::ItemStack]) -> Vec<Option<String>> {
    stacks
        .iter()
        .map(|stack| {
            if stack.is_empty() {
                None
            } else {
                Some(crate::items::item_name(server, stack.item))
            }
        })
        .collect()
}
