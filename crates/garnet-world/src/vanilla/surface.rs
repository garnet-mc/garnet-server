//! What a biome looks like on top, until the real surface rules land.
//!
//! Vanilla decides the surface with a rule tree out of the data pack, which
//! is its own piece of work. Meanwhile the biome is known exactly -- that
//! part already matches -- so this at least puts the right ground under
//! the right sky: sand in a desert, snow on a peak, terracotta in the
//! badlands. It is a table of what each biome is made of, and nothing
//! more.

/// What a biome's ground is made of, from the top down.
#[derive(Clone, Copy, Debug)]
pub struct Ground {
    /// The block right at the top.
    pub top: &'static str,
    /// The few blocks under it.
    pub under: &'static str,
    /// What sits on the sea floor of this biome, where there is one.
    pub underwater: &'static str,
}

const GRASS: Ground = Ground {
    top: "minecraft:grass_block",
    under: "minecraft:dirt",
    underwater: "minecraft:gravel",
};
const SAND: Ground = Ground {
    top: "minecraft:sand",
    under: "minecraft:sandstone",
    underwater: "minecraft:sand",
};
const SNOW: Ground = Ground {
    top: "minecraft:snow_block",
    under: "minecraft:dirt",
    underwater: "minecraft:gravel",
};
const STONE: Ground = Ground {
    top: "minecraft:stone",
    under: "minecraft:stone",
    underwater: "minecraft:gravel",
};

/// The ground a biome is made of.
pub fn ground_of(biome: &str) -> Ground {
    let short = biome.strip_prefix("minecraft:").unwrap_or(biome);
    match short {
        "desert" => SAND,
        "beach" | "snowy_beach" => SAND,
        "badlands" | "eroded_badlands" | "wooded_badlands" => Ground {
            top: "minecraft:red_sand",
            under: "minecraft:terracotta",
            underwater: "minecraft:red_sand",
        },
        "snowy_plains" | "ice_spikes" | "snowy_taiga" | "snowy_slopes" | "frozen_peaks" | "frozen_river" | "frozen_ocean"
        | "deep_frozen_ocean" => SNOW,
        "jagged_peaks" | "stony_peaks" | "stony_shore" | "windswept_gravelly_hills" => STONE,
        "grove" => SNOW,
        "old_growth_pine_taiga" | "old_growth_spruce_taiga" => Ground {
            top: "minecraft:podzol",
            under: "minecraft:dirt",
            underwater: "minecraft:gravel",
        },
        "mushroom_fields" => Ground {
            top: "minecraft:mycelium",
            under: "minecraft:dirt",
            underwater: "minecraft:gravel",
        },
        "swamp" | "mangrove_swamp" => Ground {
            top: "minecraft:grass_block",
            under: "minecraft:dirt",
            underwater: "minecraft:mud",
        },
        "warm_ocean" => Ground {
            top: "minecraft:sand",
            under: "minecraft:sandstone",
            underwater: "minecraft:sand",
        },
        // Everything green, and everything we have no special word for.
        _ => GRASS,
    }
}

/// Whether this biome is cold enough for its water to freeze and its rain
/// to fall as snow.
pub fn is_frozen(biome: &str) -> bool {
    let short = biome.strip_prefix("minecraft:").unwrap_or(biome);
    short.starts_with("snowy_")
        || short.starts_with("frozen_")
        || matches!(
            short,
            "ice_spikes" | "jagged_peaks" | "frozen_peaks" | "grove" | "deep_frozen_ocean"
        )
}

/// Whether trees and grass belong here at all.
pub fn is_bare(biome: &str) -> bool {
    let short = biome.strip_prefix("minecraft:").unwrap_or(biome);
    matches!(
        short,
        "desert"
            | "badlands"
            | "eroded_badlands"
            | "wooded_badlands"
            | "beach"
            | "snowy_beach"
            | "stony_shore"
            | "jagged_peaks"
            | "frozen_peaks"
            | "stony_peaks"
            | "ice_spikes"
    ) || short.ends_with("_ocean")
        || short == "river"
        || short == "frozen_river"
}
