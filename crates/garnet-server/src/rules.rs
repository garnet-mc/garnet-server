//! World-wide settings players can change with commands: game rules,
//! difficulty, weather, the world border and the tick rate. Saved as
//! `world/garnet-rules.json`, a plain file that stays readable between
//! versions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorldBorder {
    pub center_x: f64,
    pub center_z: f64,
    pub size: f64,
    /// Where the size is heading and when it gets there (ms since epoch).
    pub target_size: f64,
    pub lerp_end_ms: i64,
    pub warning_blocks: i32,
    pub warning_seconds: i32,
    pub damage_per_block: f64,
    pub damage_buffer: f64,
}

impl Default for WorldBorder {
    fn default() -> Self {
        Self {
            center_x: 0.0,
            center_z: 0.0,
            size: 59_999_968.0,
            target_size: 59_999_968.0,
            lerp_end_ms: 0,
            warning_blocks: 5,
            warning_seconds: 15,
            damage_per_block: 0.2,
            damage_buffer: 5.0,
        }
    }
}

impl WorldBorder {
    pub fn remaining_lerp_ms(&self) -> i64 {
        (self.lerp_end_ms - now_ms()).max(0)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Weather {
    pub raining: bool,
    pub thundering: bool,
    /// Ticks until the current weather changes on its own; 0 = never.
    pub ticks_left: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TickControl {
    pub rate: f32,
    pub frozen: bool,
    /// Steps still to run while frozen (`/tick step`).
    pub steps_left: i32,
}

impl Default for TickControl {
    fn default() -> Self {
        Self {
            rate: 20.0,
            frozen: false,
            steps_left: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldRules {
    pub game_rules: BTreeMap<String, String>,
    pub difficulty: String,
    pub difficulty_locked: bool,
    pub default_game_mode: Option<String>,
    pub weather: Weather,
    pub border: WorldBorder,
    pub tick: TickControl,
    /// Where new players start and respawn by default.
    pub spawn_yaw: f32,
    pub autosave: bool,
    pub disabled_datapacks: Vec<String>,
    pub scheduled_functions: Vec<crate::functions::ScheduledFunction>,
}

impl Default for WorldRules {
    fn default() -> Self {
        Self {
            game_rules: BTreeMap::new(),
            difficulty: "normal".into(),
            difficulty_locked: false,
            default_game_mode: None,
            weather: Weather::default(),
            border: WorldBorder::default(),
            tick: TickControl::default(),
            spawn_yaw: 0.0,
            autosave: true,
            disabled_datapacks: Vec::new(),
            scheduled_functions: Vec::new(),
        }
    }
}

/// Every vanilla game rule with its default, so `/gamerule` can list them
/// and validate values. The server honours the ones it implements; the rest
/// are stored and sent to clients that care.
pub const GAME_RULES: &[(&str, &str)] = &[
    ("announceAdvancements", "true"),
    ("blockExplosionDropDecay", "true"),
    ("commandBlockOutput", "true"),
    ("commandModificationBlockLimit", "32768"),
    ("disableElytraMovementCheck", "false"),
    ("disablePlayerMovementCheck", "false"),
    ("disableRaids", "false"),
    ("doDaylightCycle", "true"),
    ("doEntityDrops", "true"),
    ("doFireTick", "true"),
    ("doImmediateRespawn", "false"),
    ("doInsomnia", "true"),
    ("doLimitedCrafting", "false"),
    ("doMobLoot", "true"),
    ("doMobSpawning", "true"),
    ("doPatrolSpawning", "true"),
    ("doTileDrops", "true"),
    ("doTraderSpawning", "true"),
    ("doVinesSpread", "true"),
    ("doWardenSpawning", "true"),
    ("doWeatherCycle", "true"),
    ("drowningDamage", "true"),
    ("enderPearlsVanishOnDeath", "true"),
    ("fallDamage", "true"),
    ("fireDamage", "true"),
    ("forgiveDeadPlayers", "true"),
    ("freezeDamage", "true"),
    ("globalSoundEvents", "true"),
    ("keepInventory", "false"),
    ("lavaSourceConversion", "false"),
    ("logAdminCommands", "true"),
    ("maxCommandChainLength", "65536"),
    ("maxCommandForkCount", "65536"),
    ("maxEntityCramming", "24"),
    ("minecartMaxSpeed", "8"),
    ("mobExplosionDropDecay", "true"),
    ("mobGriefing", "true"),
    ("naturalRegeneration", "true"),
    ("playersNetherPortalCreativeDelay", "0"),
    ("playersNetherPortalDefaultDelay", "80"),
    ("playersSleepingPercentage", "100"),
    ("projectilesCanBreakBlocks", "true"),
    ("randomTickSpeed", "3"),
    ("reducedDebugInfo", "false"),
    ("sendCommandFeedback", "true"),
    ("showDeathMessages", "true"),
    ("snowAccumulationHeight", "1"),
    ("spawnChunkRadius", "2"),
    ("spawnRadius", "10"),
    ("spectatorsGenerateChunks", "true"),
    ("tntExplodes", "true"),
    ("tntExplosionDropDecay", "false"),
    ("universalAnger", "false"),
    ("waterSourceConversion", "true"),
];

impl WorldRules {
    /// Loads the saved rules, or starts from the config's difficulty the
    /// first time a world is opened.
    pub fn load(world_dir: &Path, config_difficulty: &str) -> Self {
        let path = world_dir.join("garnet-rules.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(rules) => rules,
                Err(err) => {
                    tracing::warn!("{} is not valid, using defaults: {err}", path.display());
                    Self::default()
                }
            },
            Err(_) => Self {
                difficulty: config_difficulty.to_owned(),
                ..Self::default()
            },
        }
    }

    pub fn save(&self, world_dir: &Path) {
        let path = world_dir.join("garnet-rules.json");
        let tmp = world_dir.join("garnet-rules.json.tmp");
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(world_dir)?;
            std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(err) = write() {
            tracing::error!("saving {}: {err}", path.display());
        }
    }

    pub fn game_rule(&self, name: &str) -> String {
        if let Some(v) = self.game_rules.get(name) {
            return v.clone();
        }
        GAME_RULES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, d)| (*d).to_owned())
            .unwrap_or_default()
    }

    pub fn game_rule_bool(&self, name: &str) -> bool {
        self.game_rule(name) == "true"
    }

    /// Whether `name` is a real rule, and whether `value` fits its type.
    pub fn validate_game_rule(name: &str, value: &str) -> Result<String, String> {
        let Some((_, default)) = GAME_RULES.iter().find(|(n, _)| *n == name) else {
            return Err(format!("Unknown game rule '{name}'."));
        };
        if *default == "true" || *default == "false" {
            match value {
                "true" | "false" => Ok(value.to_owned()),
                _ => Err(format!("{name} needs true or false.")),
            }
        } else {
            value
                .parse::<i64>()
                .map(|v| v.to_string())
                .map_err(|_| format!("{name} needs a whole number."))
        }
    }

    pub fn difficulty_id(&self) -> u8 {
        match self.difficulty.as_str() {
            "peaceful" => 0,
            "easy" => 1,
            "hard" => 3,
            _ => 2,
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
