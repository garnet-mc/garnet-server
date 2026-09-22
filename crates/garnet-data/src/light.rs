//! How much light each block state gives off and how much it blocks.
//!
//! Mojang's data generator does not export this, so the rules live here,
//! by block name and properties, and are turned into two flat tables (one
//! byte per state) when the game data loads. Unknown blocks block light
//! fully, which is right for the great majority of blocks.

use crate::blocks::BlockRegistry;

/// Light blocked by a full opaque block.
pub const OPAQUE: u8 = 15;

pub struct LightTable {
    emission: Vec<u8>,
    opacity: Vec<u8>,
}

impl LightTable {
    pub fn build(blocks: &BlockRegistry) -> Self {
        let count = blocks.state_count();
        let mut emission = vec![0u8; count];
        let mut opacity = vec![OPAQUE; count];
        for block in &blocks.blocks {
            let short = block.name.strip_prefix("minecraft:").unwrap_or(&block.name);
            for id in block.first_state..=block.last_state {
                let Some(state) = blocks.state(id) else { continue };
                emission[id as usize] = emission_for(short, &state.properties);
                opacity[id as usize] = opacity_for(short, &state.properties);
            }
        }
        Self { emission, opacity }
    }

    /// 0-15 light given off by a state.
    pub fn emission(&self, state: u32) -> u8 {
        self.emission.get(state as usize).copied().unwrap_or(0)
    }

    /// 0-15 light blocked by a state: 0 lets light through untouched, 15
    /// stops it.
    pub fn opacity(&self, state: u32) -> u8 {
        self.opacity.get(state as usize).copied().unwrap_or(OPAQUE)
    }
}

type Props = std::collections::BTreeMap<String, String>;

fn lit(props: &Props) -> bool {
    props.get("lit").map(String::as_str) == Some("true")
}

fn emission_for(name: &str, props: &Props) -> u8 {
    let prop = |key: &str| props.get(key).map(String::as_str);
    let count = |key: &str| prop(key).and_then(|v| v.parse::<u8>().ok()).unwrap_or(1);
    match name {
        "glowstone" | "sea_lantern" | "lava" | "jack_o_lantern" | "beacon" | "conduit" | "end_gateway" | "end_portal" | "fire"
        | "lantern" | "shroomlight" | "ochre_froglight" | "verdant_froglight" | "pearlescent_froglight" | "lava_cauldron" => 15,
        "campfire" | "redstone_lamp" | "copper_bulb" | "waxed_copper_bulb" => {
            if lit(props) {
                15
            } else {
                0
            }
        }
        "exposed_copper_bulb" | "waxed_exposed_copper_bulb" => {
            if lit(props) {
                12
            } else {
                0
            }
        }
        "weathered_copper_bulb" | "waxed_weathered_copper_bulb" => {
            if lit(props) {
                8
            } else {
                0
            }
        }
        "oxidized_copper_bulb" | "waxed_oxidized_copper_bulb" => {
            if lit(props) {
                4
            } else {
                0
            }
        }
        "cave_vines" | "cave_vines_plant" => {
            if prop("berries") == Some("true") {
                14
            } else {
                0
            }
        }
        "torch" | "wall_torch" | "end_rod" => 14,
        "furnace" | "blast_furnace" | "smoker" => {
            if lit(props) {
                13
            } else {
                0
            }
        }
        "nether_portal" => 11,
        "soul_torch" | "soul_wall_torch" | "soul_lantern" | "soul_fire" | "crying_obsidian" => 10,
        "soul_campfire" => {
            if lit(props) {
                10
            } else {
                0
            }
        }
        "redstone_ore" | "deepslate_redstone_ore" => {
            if lit(props) {
                9
            } else {
                0
            }
        }
        "redstone_torch" | "redstone_wall_torch" => {
            if lit(props) {
                7
            } else {
                0
            }
        }
        "enchanting_table" | "ender_chest" | "glow_lichen" => 7,
        "sea_pickle" => {
            if prop("waterlogged") == Some("true") {
                6 + 3 * (count("pickles").saturating_sub(1))
            } else {
                0
            }
        }
        "sculk_catalyst" | "vault" => 6,
        "amethyst_cluster" => 5,
        "large_amethyst_bud" | "trial_spawner" => 4,
        "magma_block" => 3,
        "medium_amethyst_bud" | "firefly_bush" => 2,
        "brown_mushroom" | "small_amethyst_bud" | "brewing_stand" | "dragon_egg" | "end_portal_frame" | "sculk_sensor"
        | "calibrated_sculk_sensor" => 1,
        "respawn_anchor" => match prop("charges") {
            Some("1") => 3,
            Some("2") => 7,
            Some("3") => 11,
            Some("4") => 15,
            _ => 0,
        },
        "light" => prop("level").and_then(|v| v.parse().ok()).unwrap_or(15),
        _ if name.ends_with("candle") || name.ends_with("candle_cake") => {
            if lit(props) {
                if name.ends_with("candle") {
                    3 * count("candles")
                } else {
                    3
                }
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// Blocks that are not full opaque cubes. Everything else stops light.
fn opacity_for(name: &str, props: &Props) -> u8 {
    let prop = |key: &str| props.get(key).map(String::as_str);
    // Full-cube shapes that do not occlude still cost a little (leaves,
    // ice, honey), like vanilla's "does not propagate sky light" rule.
    const SOFT: &[&str] = &[
        "ice", "frosted_ice", "slime_block", "honey_block", "spawner", "trial_spawner", "vault", "beacon", "mangrove_roots", "powder_snow",
        "bubble_column",
    ];
    const CLEAR: &[&str] = &[
        "air", "cave_air", "void_air", "light", "structure_void", "barrier", "chain", "iron_bars", "ladder", "vine", "glow_lichen",
        "sculk_vein", "lever", "tripwire", "tripwire_hook", "redstone_wire", "repeater", "comparator", "daylight_detector", "hopper",
        "composter", "grindstone", "bell", "lectern", "stonecutter", "enchanting_table", "end_portal_frame", "end_rod", "lightning_rod",
        "brewing_stand", "scaffolding", "chest", "trapped_chest", "ender_chest", "conduit", "pointed_dripstone", "decorated_pot", "cake",
        "flower_pot", "cactus", "farmland", "dirt_path", "cobweb", "sea_pickle", "turtle_egg", "sniffer_egg", "dragon_egg",
        "frogspawn", "lily_pad", "bamboo", "bamboo_sapling", "sugar_cane", "kelp", "kelp_plant", "seagrass", "tall_seagrass",
        "nether_wart", "sweet_berry_bush", "cocoa", "chorus_plant", "chorus_flower", "moss_carpet", "pale_moss_carpet",
        "pale_hanging_moss", "hanging_roots", "spore_blossom", "small_dripleaf", "big_dripleaf", "big_dripleaf_stem", "azalea",
        "flowering_azalea", "mangrove_propagule", "pink_petals", "wildflowers", "leaf_litter", "bush", "firefly_bush", "cactus_flower",
        "short_dry_grass", "tall_dry_grass", "short_grass", "tall_grass", "fern", "large_fern", "dead_bush", "dandelion", "poppy",
        "blue_orchid", "allium", "azure_bluet", "red_tulip", "orange_tulip", "white_tulip", "pink_tulip", "oxeye_daisy", "cornflower",
        "lily_of_the_valley", "wither_rose", "torchflower", "pitcher_plant", "pitcher_crop", "torchflower_crop", "sunflower", "lilac",
        "rose_bush", "peony", "open_eyeblossom", "closed_eyeblossom", "brown_mushroom", "red_mushroom", "crimson_fungus",
        "warped_fungus", "crimson_roots", "warped_roots", "nether_sprouts", "weeping_vines", "weeping_vines_plant", "twisting_vines",
        "twisting_vines_plant", "cave_vines", "cave_vines_plant", "wheat", "carrots", "potatoes", "beetroots", "melon_stem",
        "pumpkin_stem", "attached_melon_stem", "attached_pumpkin_stem", "sculk_sensor", "calibrated_sculk_sensor", "sculk_shrieker",
        "piston_head", "moving_piston", "heavy_core", "resin_clump", "anvil", "chipped_anvil", "damaged_anvil", "campfire",
        "soul_campfire", "lantern", "soul_lantern", "torch", "wall_torch", "soul_torch", "soul_wall_torch", "redstone_torch",
        "redstone_wall_torch", "rail", "powered_rail", "detector_rail", "activator_rail", "water", "lava",
    ];
    if CLEAR.contains(&name) {
        return match name {
            "water" | "bubble_column" => 1,
            _ => 0,
        };
    }
    if SOFT.contains(&name) {
        return 1;
    }
    let suffixes_clear = [
        "_stairs", "_slab", "_fence", "_fence_gate", "_wall", "_door", "_trapdoor", "_button", "_pressure_plate", "_pane", "_sign",
        "_banner", "_bed", "_carpet", "_sapling", "_coral", "_coral_fan", "_coral_wall_fan", "_head", "_skull", "_candle",
        "_candle_cake", "_cauldron", "_amethyst_bud", "_egg", "_bars",
    ];
    if suffixes_clear.iter().any(|s| name.ends_with(s)) || name.starts_with("potted_") || name.starts_with("dead_") && name.contains("coral") {
        return 0;
    }
    if name == "amethyst_cluster" || name == "cauldron" || name == "candle" || name == "candle_cake" {
        return 0;
    }
    if name.ends_with("_leaves") || name.ends_with("_copper_grate") || name == "copper_grate" || name.ends_with("shulker_box") {
        return 1;
    }
    if name.ends_with("glass") {
        // Tinted glass is the one glass that stops light.
        return if name == "tinted_glass" { OPAQUE } else { 0 };
    }
    // Snow layers thinner than a block let light past.
    if name == "snow" {
        return if prop("layers") == Some("8") { OPAQUE } else { 0 };
    }
    let _ = prop;
    OPAQUE
}
