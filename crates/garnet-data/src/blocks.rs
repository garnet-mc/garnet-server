//! Block states, from `reports/blocks.json`.
//!
//! Every combination of a block's properties is a *state* with a numeric id.
//! Chunk packets carry those ids, so they must match the client exactly, and
//! they change whenever Mojang adds a block. Hence: read them from the report.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct BlockState {
    /// Index of the owning block in [`BlockRegistry::blocks`].
    pub block: usize,
    pub id: i32,
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Block {
    /// Full name such as `minecraft:oak_stairs`.
    pub name: String,
    /// Property name -> allowed values, in report order.
    pub properties: Vec<(String, Vec<String>)>,
    pub default_state: i32,
    pub first_state: i32,
    pub last_state: i32,
}

#[derive(Debug, Default)]
pub struct BlockRegistry {
    pub blocks: Vec<Block>,
    by_name: HashMap<String, usize>,
    /// Indexed by state id; every id from 0 to `state_count() - 1` exists.
    states: Vec<BlockState>,
    air_states: Vec<bool>,
    liquid_states: Vec<bool>,
}

impl BlockRegistry {
    pub fn from_report(json: &serde_json::Value) -> Result<Self> {
        let map = json.as_object().context("blocks.json is not an object")?;
        let mut blocks = Vec::with_capacity(map.len());
        let mut states: Vec<Option<BlockState>> = Vec::new();

        for (name, def) in map {
            let block_index = blocks.len();
            let mut properties = Vec::new();
            if let Some(props) = def.get("properties").and_then(|p| p.as_object()) {
                for (prop, values) in props {
                    let values = values
                        .as_array()
                        .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                        .unwrap_or_default();
                    properties.push((prop.clone(), values));
                }
            }
            let state_list = def.get("states").and_then(|s| s.as_array()).context("block without states")?;
            let mut default_state = None;
            let mut first = i32::MAX;
            let mut last = i32::MIN;
            for state in state_list {
                let id = state["id"].as_i64().context("state without id")? as i32;
                first = first.min(id);
                last = last.max(id);
                if state.get("default").and_then(|d| d.as_bool()).unwrap_or(false) {
                    default_state = Some(id);
                }
                let mut props = BTreeMap::new();
                if let Some(p) = state.get("properties").and_then(|p| p.as_object()) {
                    for (k, v) in p {
                        props.insert(k.clone(), v.as_str().unwrap_or_default().to_owned());
                    }
                }
                let idx = id as usize;
                if states.len() <= idx {
                    states.resize(idx + 1, None);
                }
                states[idx] = Some(BlockState {
                    block: block_index,
                    id,
                    properties: props,
                });
            }
            blocks.push(Block {
                name: name.clone(),
                properties,
                default_state: default_state.unwrap_or(first),
                first_state: first,
                last_state: last,
            });
        }

        let states: Vec<BlockState> = states
            .into_iter()
            .enumerate()
            .map(|(i, s)| s.with_context(|| format!("block state id {i} has no block")))
            .collect::<Result<_>>()?;

        let by_name: HashMap<String, usize> = blocks.iter().enumerate().map(|(i, b)| (b.name.clone(), i)).collect();

        let air_states = states
            .iter()
            .map(|s| matches!(blocks[s.block].name.as_str(), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"))
            .collect();
        let liquid_states = states
            .iter()
            .map(|s| {
                matches!(blocks[s.block].name.as_str(), "minecraft:water" | "minecraft:lava")
                    || s.properties.get("waterlogged").map(|v| v == "true").unwrap_or(false)
            })
            .collect();

        Ok(Self {
            blocks,
            by_name,
            states,
            air_states,
            liquid_states,
        })
    }

    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    /// Bits needed to store any state id directly (the client computes the
    /// same number from its own registry).
    pub fn bits_per_state(&self) -> u8 {
        let mut bits = 0;
        while (1usize << bits) < self.state_count() {
            bits += 1;
        }
        bits.max(4)
    }

    pub fn block(&self, name: &str) -> Option<&Block> {
        let name = with_namespace(name);
        self.by_name.get(&name).map(|&i| &self.blocks[i])
    }

    /// The default state id of a block, e.g. `stone` -> 1.
    pub fn default_state(&self, name: &str) -> Option<i32> {
        self.block(name).map(|b| b.default_state)
    }

    pub fn state(&self, id: i32) -> Option<&BlockState> {
        self.states.get(usize::try_from(id).ok()?)
    }

    pub fn block_of_state(&self, id: i32) -> Option<&Block> {
        self.state(id).map(|s| &self.blocks[s.block])
    }

    /// Finds the state of `name` whose properties match `props`. Missing
    /// properties fall back to the block's default, which is how Anvil
    /// chunks written by older versions are read.
    pub fn state_with(&self, name: &str, props: &BTreeMap<String, String>) -> Option<i32> {
        let block = self.block(name)?;
        let default = self.state(block.default_state)?;
        (block.first_state..=block.last_state).find(|&id| {
            let state = &self.states[id as usize];
            state.properties.iter().all(|(k, v)| {
                let wanted = props.get(k).or_else(|| default.properties.get(k));
                wanted == Some(v)
            })
        })
    }

    pub fn is_air(&self, id: i32) -> bool {
        self.air_states.get(id as usize).copied().unwrap_or(true)
    }

    pub fn is_liquid(&self, id: i32) -> bool {
        self.liquid_states.get(id as usize).copied().unwrap_or(false)
    }
}

fn with_namespace(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}
