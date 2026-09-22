//! Keeping old saves loadable.
//!
//! Chunks store block *names*, so a numeric-id reshuffle between Minecraft
//! versions never matters. What does change occasionally is the name itself.
//! This table maps names from older versions to their current spelling; an
//! unknown name falls back to air (with one warning per name) rather than
//! refusing to load the chunk.

use std::collections::HashSet;
use std::sync::Mutex;

/// `(old name, new name)`, all in the `minecraft` namespace.
const BLOCK_RENAMES: &[(&str, &str)] = &[
    ("grass", "short_grass"),                  // 1.20.3
    ("grass_path", "dirt_path"),               // 1.17
    ("zombie_pigman_spawn_egg", "zombified_piglin_spawn_egg"),
    ("cauldron_water", "water_cauldron"),
    ("sign", "oak_sign"),                      // 1.14
    ("wall_sign", "oak_wall_sign"),
    ("stone_slab", "smooth_stone_slab"),       // 1.14 (the old double slab)
];

const BIOME_RENAMES: &[(&str, &str)] = &[
    ("mountains", "windswept_hills"),          // 1.18
    ("wooded_mountains", "windswept_forest"),
    ("gravelly_mountains", "windswept_gravelly_hills"),
    ("snowy_tundra", "snowy_plains"),
    ("jungle_edge", "sparse_jungle"),
    ("stone_shore", "stony_shore"),
    ("giant_tree_taiga", "old_growth_pine_taiga"),
    ("giant_spruce_taiga", "old_growth_spruce_taiga"),
    ("tall_birch_forest", "old_growth_birch_forest"),
    ("shattered_savanna", "windswept_savanna"),
    ("mushroom_field_shore", "mushroom_fields"),
    ("snowy_mountains", "snowy_plains"),
    ("nether", "nether_wastes"),               // 1.16
];

pub fn current_block_name(name: &str) -> String {
    rename(name, BLOCK_RENAMES)
}

pub fn current_biome_name(name: &str) -> String {
    rename(name, BIOME_RENAMES)
}

fn rename(name: &str, table: &[(&str, &str)]) -> String {
    let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
    if namespace != "minecraft" {
        return format!("{namespace}:{path}");
    }
    let path = table.iter().find(|(old, _)| *old == path).map(|(_, new)| *new).unwrap_or(path);
    format!("minecraft:{path}")
}

static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Logs "unknown block X" once per name so a corrupt region does not flood
/// the console.
pub fn warn_unknown_once(kind: &str, name: &str) {
    let mut guard = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    if set.insert(format!("{kind}:{name}")) {
        tracing::warn!("unknown {kind} '{name}' in saved chunk; loading it as air (a rename table entry in compat.rs would fix this)");
    }
}
