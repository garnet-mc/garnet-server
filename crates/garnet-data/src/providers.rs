//! The small numbers other data points at.
//!
//! A few components do not carry a number themselves but the name of one:
//! brewing fuel says its worth is `minecraft:brewing/uses_default`, and
//! that file holds the 20. They are plain JSON numbers, so this keeps a
//! map of them for whoever needs to follow such a name.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Providers {
    ints: HashMap<String, i32>,
    floats: HashMap<String, f32>,
}

impl Providers {
    pub fn load(datapack_dir: &Path) -> Result<Self> {
        let mut ints = HashMap::new();
        let mut floats = HashMap::new();
        for namespace in read_namespaces(datapack_dir) {
            let name = namespace.file_name().unwrap_or_default().to_string_lossy().to_string();
            read_all(&namespace.join("context_int_provider"), &name, &mut |key, value| {
                if let Some(number) = as_number(&value) {
                    ints.insert(key, number as i32);
                }
            });
            read_all(&namespace.join("context_float_provider"), &name, &mut |key, value| {
                if let Some(number) = as_number(&value) {
                    floats.insert(key, number as f32);
                }
            });
        }
        Ok(Self { ints, floats })
    }

    /// A whole number by name, or `fallback` when the pack has no such name.
    pub fn int(&self, name: &str, fallback: i32) -> i32 {
        self.ints.get(name).copied().unwrap_or(fallback)
    }

    pub fn float(&self, name: &str, fallback: f32) -> f32 {
        self.floats.get(name).copied().unwrap_or(fallback)
    }
}

/// Only the plain numbers; the ones written as a range or a shape need a
/// context we do not have here, and whoever asks falls back instead.
fn as_number(value: &Value) -> Option<f64> {
    value.as_f64()
}

fn read_namespaces(datapack_dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(datapack_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// Walks one provider directory, naming each file the way the data does:
/// `<namespace>:<path below the directory, without .json>`.
fn read_all(dir: &Path, namespace: &str, found: &mut impl FnMut(String, Value)) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(relative) = path.strip_prefix(dir) else { continue };
            let key = format!(
                "{namespace}:{}",
                relative.with_extension("").to_string_lossy().replace('\\', "/")
            );
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            found(key, value);
        }
    }
}
