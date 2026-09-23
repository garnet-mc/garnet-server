//! Reading the vanilla data pack out of the server jar: the "dynamic"
//! registries the client must be sent (dimension types, biomes, damage
//! types...) and the tag files.

use crate::registries::Registries;
use anyhow::{bail, Context, Result};
use garnet_protocol::nbt::NbtTag;
use garnet_protocol::Identifier;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Registries the client expects to receive in `Registry Data` packets, in
/// the order the vanilla server sends them (Minecraft 26.3). Mojang adds a
/// registry here every few versions; if the client kicks with a "missing
/// registry" error after a version bump, this is the list to update. A
/// `synced_registries.json` file in the version directory overrides it.
pub const SYNCED_REGISTRIES: &[&str] = &[
    "worldgen/biome",
    "chat_type",
    "trim_pattern",
    "trim_material",
    "wolf_variant",
    "wolf_sound_variant",
    "pig_variant",
    "pig_sound_variant",
    "frog_variant",
    "cat_variant",
    "cat_sound_variant",
    "cow_sound_variant",
    "cow_variant",
    "chicken_sound_variant",
    "chicken_variant",
    "zombie_nautilus_variant",
    "painting_variant",
    "sulfur_cube_archetype",
    "dimension_type",
    "damage_type",
    "banner_pattern",
    "enchantment",
    "jukebox_song",
    "instrument",
    "test_environment",
    "test_instance",
    "dialog",
    "world_clock",
    "timeline",
    "decorated_pot_pattern",
    "block_transformer",
    "worldgen/block_state_provider",
];

/// Biome definitions carry world-generation data the client never uses.
/// Dropping it keeps the registry packet a few hundred KB smaller.
const BIOME_KEYS_NOT_SYNCED: &[&str] = &["features", "carvers", "spawners", "spawn_costs", "creature_spawn_probability"];

/// Modern server jars are "bundlers": a small launcher jar that contains the
/// real server jar under `META-INF/versions/`. This extracts the inner jar
/// (or returns the path unchanged for old-style jars).
pub fn extract_inner_jar(bundler_jar: &Path, version_dir: &Path) -> Result<PathBuf> {
    let file = std::fs::File::open(bundler_jar)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let Ok(mut list) = zip.by_name("META-INF/versions.list") else {
        return Ok(bundler_jar.to_owned());
    };
    let mut text = String::new();
    list.read_to_string(&mut text)?;
    drop(list);
    // Each line is "<sha1>\t<version id>\t<path inside META-INF/versions/>".
    let inner_name = text
        .lines()
        .next()
        .and_then(|l| l.split('\t').nth(2))
        .context("META-INF/versions.list is empty")?
        .to_owned();
    let target = version_dir.join("server-inner.jar");
    if !target.exists() {
        let mut entry = zip.by_name(&format!("META-INF/versions/{inner_name}"))?;
        let mut out = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(target)
}

/// `version.json` at the root of the inner jar tells us the protocol and
/// data versions without needing to know them in advance.
pub fn extract_version_json(inner_jar: &Path, target: &Path) -> Result<()> {
    let file = std::fs::File::open(inner_jar)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip.by_name("version.json").context("jar has no version.json")?;
    let mut text = String::new();
    entry.read_to_string(&mut text)?;
    std::fs::write(target, text)?;
    Ok(())
}

/// Copies the parts of `data/` we need into `target_dir`, keeping the same
/// layout (`data/<namespace>/<registry>/<name>.json`).
pub fn extract_datapack(inner_jar: &Path, target_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(inner_jar)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let wanted: Vec<String> = SYNCED_REGISTRIES.iter().map(|r| format!("/{r}/")).collect();
    let mut count = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_owned();
        if !name.starts_with("data/") || !name.ends_with(".json") {
            continue;
        }
        // "data/minecraft/dimension_type/overworld.json" -> after the namespace
        let Some(after_ns) = name.splitn(3, '/').nth(2) else { continue };
        let after_ns = format!("/{after_ns}");
        let is_tag = after_ns.starts_with("/tags/");
        let is_registry = wanted.iter().any(|w| after_ns.starts_with(w));
        // Block loot tables decide what breaking a block drops, and the
        // recipes are what players craft with.
        let is_block_loot = after_ns.starts_with("/loot_table/blocks/") || after_ns.starts_with("/loot_table/entities/");
        let is_recipe = after_ns.starts_with("/recipe/");
        // Brewing fuel points at these for how many brews it is worth.
        let is_provider = after_ns.starts_with("/context_int_provider/") || after_ns.starts_with("/context_float_provider/");
        // What villagers will trade you, which 26.3 writes down at last.
        let is_trade = after_ns.starts_with("/villager_trade/");
        if !is_tag && !is_registry && !is_block_loot && !is_recipe && !is_provider && !is_trade {
            continue;
        }
        let out_path = target_dir.join(name.trim_start_matches("data/"));
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out)?;
        count += 1;
    }
    tracing::info!("extracted {count} data pack files");
    Ok(())
}

/// One data-driven registry with its entries in id order.
#[derive(Debug, Clone)]
pub struct DynamicRegistry {
    pub id: Identifier,
    /// Entry name and its NBT payload; the index is the numeric id.
    pub entries: Vec<(Identifier, NbtTag)>,
}

impl DynamicRegistry {
    pub fn index_of(&self, name: &str) -> Option<i32> {
        let wanted = Identifier::parse(name)?;
        self.entries.iter().position(|(id, _)| *id == wanted).map(|i| i as i32)
    }
}

#[derive(Debug, Default)]
pub struct DynamicRegistries {
    pub registries: Vec<DynamicRegistry>,
}

impl DynamicRegistries {
    pub fn load(datapack_dir: &Path) -> Result<Self> {
        let synced = load_synced_list(datapack_dir.parent().unwrap_or(datapack_dir));
        let mut registries = Vec::new();
        for reg_name in &synced {
            let mut entries = Vec::new();
            for namespace_dir in read_dirs(datapack_dir)? {
                let namespace = namespace_dir.file_name().unwrap().to_string_lossy().to_string();
                let reg_dir = namespace_dir.join(reg_name);
                if !reg_dir.is_dir() {
                    continue;
                }
                for file in json_files(&reg_dir)? {
                    let rel = file.strip_prefix(&reg_dir)?.with_extension("");
                    let entry_name = rel.to_string_lossy().replace('\\', "/");
                    let text = std::fs::read_to_string(&file)?;
                    let mut json: serde_json::Value =
                        serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
                    if *reg_name == "worldgen/biome" {
                        if let Some(obj) = json.as_object_mut() {
                            for key in BIOME_KEYS_NOT_SYNCED {
                                obj.remove(*key);
                            }
                        }
                    }
                    entries.push((Identifier::new(&namespace, &entry_name), NbtTag::from_json(&json)));
                }
            }
            if entries.is_empty() {
                tracing::warn!("synced registry {reg_name} has no entries in the data pack");
            }
            // Stable order so numeric ids are the same on every start.
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            registries.push(DynamicRegistry {
                id: Identifier::minecraft(reg_name),
                entries,
            });
        }
        Ok(Self { registries })
    }

    pub fn get(&self, registry: &str) -> Option<&DynamicRegistry> {
        let wanted = Identifier::parse(registry)?;
        self.registries.iter().find(|r| r.id == wanted)
    }

    /// Numeric id of `entry` in `registry`, e.g. the overworld dimension type.
    pub fn id_of(&self, registry: &str, entry: &str) -> Option<i32> {
        self.get(registry)?.index_of(entry)
    }
}

fn load_synced_list(version_dir: &Path) -> Vec<String> {
    let override_path = version_dir.join("synced_registries.json");
    if let Ok(text) = std::fs::read_to_string(&override_path) {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&text) {
            tracing::info!("using synced registry list from {}", override_path.display());
            return list;
        }
    }
    SYNCED_REGISTRIES.iter().map(|s| s.to_string()).collect()
}

/// Tags for every registry we can resolve: registry id -> (tag, member ids).
#[derive(Debug, Default)]
pub struct Tags {
    pub registries: Vec<(Identifier, Vec<(Identifier, Vec<i32>)>)>,
}

impl Tags {
    /// Member ids of one tag, e.g. `("minecraft:block", "minecraft:mineable/pickaxe")`.
    pub fn members(&self, registry: &str, tag: &str) -> Option<&[i32]> {
        let registry = with_namespace(registry);
        let tag = with_namespace(tag);
        self.registries
            .iter()
            .find(|(r, _)| r.to_string() == registry)
            .and_then(|(_, tags)| tags.iter().find(|(t, _)| t.to_string() == tag))
            .map(|(_, ids)| ids.as_slice())
    }

    pub fn load(tags_root: &Path, registries: &Registries, dynamic: &DynamicRegistries) -> Result<Self> {
        // tags_root is "<datapack>/tags" but the files live under
        // "<datapack>/<namespace>/tags/<registry path>/<tag>.json".
        let datapack_dir = tags_root.parent().context("tags dir has no parent")?;
        let mut per_registry: BTreeMap<String, BTreeMap<String, RawTag>> = BTreeMap::new();

        for namespace_dir in read_dirs(datapack_dir)? {
            let namespace = namespace_dir.file_name().unwrap().to_string_lossy().to_string();
            let tags_dir = namespace_dir.join("tags");
            if !tags_dir.is_dir() {
                continue;
            }
            for file in json_files(&tags_dir)? {
                let rel = file.strip_prefix(&tags_dir)?.with_extension("");
                let rel = rel.to_string_lossy().replace('\\', "/");
                // The registry path is everything except the final tag name;
                // registry paths themselves can contain a slash (worldgen/biome).
                let Some((registry_path, tag_name)) = split_registry_and_tag(&rel, registries, dynamic) else {
                    continue;
                };
                let text = std::fs::read_to_string(&file)?;
                let json: serde_json::Value = serde_json::from_str(&text)?;
                let values = json
                    .get("values")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(raw_tag_value).collect())
                    .unwrap_or_default();
                per_registry
                    .entry(registry_path.to_owned())
                    .or_default()
                    .insert(format!("{namespace}:{tag_name}"), RawTag { values });
            }
        }

        let mut out = Vec::new();
        for (registry_path, tags) in &per_registry {
            let lookup = |name: &str| -> Option<i32> {
                registries
                    .id_of(registry_path, name)
                    .or_else(|| dynamic.id_of(registry_path, name))
            };
            let mut resolved: Vec<(Identifier, Vec<i32>)> = Vec::new();
            let mut cache: HashMap<String, Vec<i32>> = HashMap::new();
            for tag_name in tags.keys() {
                let mut visiting = HashSet::new();
                let ids = resolve_tag(tag_name, tags, &lookup, &mut cache, &mut visiting);
                if let Some(id) = Identifier::parse(tag_name) {
                    resolved.push((id, ids));
                }
            }
            out.push((Identifier::minecraft(registry_path), resolved));
        }
        Ok(Self { registries: out })
    }
}

struct RawTag {
    values: Vec<(String, bool)>, // (name or #tag, required)
}

fn raw_tag_value(v: &serde_json::Value) -> Option<(String, bool)> {
    match v {
        serde_json::Value::String(s) => Some((s.clone(), true)),
        serde_json::Value::Object(o) => Some((
            o.get("id")?.as_str()?.to_owned(),
            o.get("required").and_then(|r| r.as_bool()).unwrap_or(true),
        )),
        _ => None,
    }
}

fn resolve_tag(
    tag_name: &str,
    tags: &BTreeMap<String, RawTag>,
    lookup: &dyn Fn(&str) -> Option<i32>,
    cache: &mut HashMap<String, Vec<i32>>,
    visiting: &mut HashSet<String>,
) -> Vec<i32> {
    if let Some(ids) = cache.get(tag_name) {
        return ids.clone();
    }
    if !visiting.insert(tag_name.to_owned()) {
        return Vec::new(); // cycle
    }
    let mut ids = Vec::new();
    if let Some(tag) = tags.get(tag_name) {
        for (value, _required) in &tag.values {
            if let Some(nested) = value.strip_prefix('#') {
                let nested = with_namespace(nested);
                ids.extend(resolve_tag(&nested, tags, lookup, cache, visiting));
            } else if let Some(id) = lookup(value) {
                ids.push(id);
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    visiting.remove(tag_name);
    cache.insert(tag_name.to_owned(), ids.clone());
    ids
}

/// `worldgen/biome/is_forest` -> (`worldgen/biome`, `is_forest`);
/// `block/logs` -> (`block`, `logs`); `item/foo/bar` -> (`item`, `foo/bar`).
fn split_registry_and_tag<'a>(
    rel: &'a str,
    registries: &Registries,
    dynamic: &DynamicRegistries,
) -> Option<(&'a str, &'a str)> {
    // Try the longest registry prefix first so "worldgen/biome" wins over "worldgen".
    let mut split_points: Vec<usize> = rel.match_indices('/').map(|(i, _)| i).collect();
    split_points.reverse();
    for i in split_points {
        let (reg, tag) = (&rel[..i], &rel[i + 1..]);
        if registries.get(reg).is_some() || dynamic.get(reg).is_some() {
            return Some((reg, tag));
        }
    }
    None
}

fn with_namespace(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

fn read_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    Ok(dirs)
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "json").unwrap_or(false) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}
