//! The components every item starts life with, from the game's own report.
//!
//! The data generator writes one file per item under
//! `reports/minecraft/components/item/`, holding exactly what vanilla gives
//! a fresh stack: how much wear it takes, how readily it enchants, how many
//! fit in a slot, and what mends it. Reading those beats keeping tables of
//! our own, which go stale the moment a version adds a new metal.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// What a fresh stack of one item carries.
#[derive(Clone, Debug, Default)]
pub struct ItemDefaults {
    /// Wear it takes before breaking; zero for things that never break.
    pub max_damage: i32,
    /// How readily it takes enchantments; zero when it takes none.
    pub enchantable: i32,
    pub max_stack_size: i32,
    /// The item or `#tag` that mends it on an anvil, if any.
    pub repairable: Option<String>,
}

#[derive(Debug, Default)]
pub struct ItemComponents {
    items: HashMap<String, ItemDefaults>,
}

impl ItemComponents {
    /// Reads every item file under `reports/minecraft/components/item`.
    pub fn load(reports_dir: &Path) -> Result<Self> {
        let dir = reports_dir.join("minecraft").join("components").join("item");
        let mut items = HashMap::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            tracing::warn!("no item components in {}; falling back to defaults", dir.display());
            return Ok(Self { items });
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let components = &json["components"];
            let number = |key: &str| components.get(key).and_then(Value::as_i64).unwrap_or(0) as i32;
            items.insert(
                format!("minecraft:{stem}"),
                ItemDefaults {
                    max_damage: number("minecraft:max_damage"),
                    enchantable: components
                        .get("minecraft:enchantable")
                        .and_then(|c| c.get("value"))
                        .and_then(Value::as_i64)
                        .unwrap_or(0) as i32,
                    max_stack_size: match number("minecraft:max_stack_size") {
                        0 => 64,
                        size => size,
                    },
                    repairable: components
                        .get("minecraft:repairable")
                        .and_then(|c| c.get("items"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            );
        }
        tracing::info!("loaded default components for {} items", items.len());
        Ok(Self { items })
    }

    pub fn get(&self, item: &str) -> Option<&ItemDefaults> {
        self.items.get(&with_namespace(item))
    }

    pub fn max_damage(&self, item: &str) -> i32 {
        self.get(item).map(|d| d.max_damage).unwrap_or(0)
    }

    pub fn enchantable(&self, item: &str) -> i32 {
        self.get(item).map(|d| d.enchantable).unwrap_or(0)
    }

    /// How many fit in a slot; 64 for anything we have no word on.
    pub fn max_stack_size(&self, item: &str) -> i32 {
        self.get(item).map(|d| d.max_stack_size).unwrap_or(64)
    }

    pub fn repairable(&self, item: &str) -> Option<&str> {
        self.get(item).and_then(|d| d.repairable.as_deref())
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

fn with_namespace(item: &str) -> String {
    if item.contains(':') {
        item.to_owned()
    } else {
        format!("minecraft:{item}")
    }
}
