//! Static registries from `reports/registries.json`: items, entity types,
//! block entity types, particles, sounds... anything with fixed numeric ids.

use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct Registry {
    pub id: String,
    by_name: HashMap<String, i32>,
    by_id: HashMap<i32, String>,
    pub default: Option<String>,
}

impl Registry {
    pub fn id_of(&self, name: &str) -> Option<i32> {
        self.by_name.get(&with_namespace(name)).copied()
    }

    pub fn name_of(&self, id: i32) -> Option<&str> {
        self.by_id.get(&id).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }
}

#[derive(Debug, Default)]
pub struct Registries {
    map: HashMap<String, Registry>,
}

impl Registries {
    pub fn from_report(json: &serde_json::Value) -> Result<Self> {
        let root = json.as_object().context("registries.json is not an object")?;
        let mut map = HashMap::with_capacity(root.len());
        for (registry_name, def) in root {
            let mut registry = Registry {
                id: registry_name.clone(),
                default: def.get("default").and_then(|d| d.as_str()).map(str::to_owned),
                ..Default::default()
            };
            if let Some(entries) = def.get("entries").and_then(|e| e.as_object()) {
                for (entry_name, entry) in entries {
                    let id = entry["protocol_id"].as_i64().context("entry without protocol_id")? as i32;
                    registry.by_name.insert(entry_name.clone(), id);
                    registry.by_id.insert(id, entry_name.clone());
                }
            }
            map.insert(registry_name.clone(), registry);
        }
        Ok(Self { map })
    }

    pub fn get(&self, registry: &str) -> Option<&Registry> {
        self.map.get(&with_namespace(registry))
    }

    /// Shorthand: numeric id of `name` in `registry`.
    pub fn id_of(&self, registry: &str, name: &str) -> Option<i32> {
        self.get(registry)?.id_of(name)
    }

    /// Shorthand the other way: what entry `id` of `registry` is called.
    pub fn name_of(&self, registry: &str, id: i32) -> Option<&str> {
        self.get(registry)?.name_of(id)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

fn with_namespace(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}
