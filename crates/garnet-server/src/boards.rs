//! The scoreboard, teams and boss bars: the state behind `/scoreboard`,
//! `/team`, `/bossbar`, `/tag` and `/trigger`. Saved as
//! `world/garnet-boards.json`.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Objective {
    pub name: String,
    pub criteria: String,
    pub display_name: String,
    /// "integer" or "hearts"
    pub render_type: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    pub display_name: String,
    pub prefix: String,
    pub suffix: String,
    /// A formatting colour name such as "red", or "reset".
    pub color: String,
    pub friendly_fire: bool,
    pub see_friendly_invisibles: bool,
    /// always | never | hideForOtherTeams | hideForOwnTeam
    pub name_tag_visibility: String,
    /// always | never | pushOtherTeams | pushOwnTeam
    pub collision_rule: String,
    pub members: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BossBar {
    pub id: String,
    pub uuid: Uuid,
    pub name: String,
    pub color: String,
    pub style: String,
    pub value: i32,
    pub max: i32,
    pub visible: bool,
    pub players: BTreeSet<Uuid>,
}

impl BossBar {
    pub fn progress(&self) -> f32 {
        if self.max <= 0 {
            0.0
        } else {
            (self.value as f32 / self.max as f32).clamp(0.0, 1.0)
        }
    }

    pub fn color_id(&self) -> i32 {
        match self.color.as_str() {
            "pink" => 0,
            "blue" => 1,
            "red" => 2,
            "green" => 3,
            "yellow" => 4,
            "purple" => 5,
            _ => 6,
        }
    }

    pub fn style_id(&self) -> i32 {
        match self.style.as_str() {
            "notched_6" => 1,
            "notched_10" => 2,
            "notched_12" => 3,
            "notched_20" => 4,
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Boards {
    pub objectives: BTreeMap<String, Objective>,
    /// objective -> owner -> score
    pub scores: BTreeMap<String, BTreeMap<String, i32>>,
    /// display slot ("sidebar", "list", "belowName") -> objective
    pub displays: BTreeMap<String, String>,
    pub teams: BTreeMap<String, Team>,
    pub boss_bars: BTreeMap<String, BossBar>,
}

impl Boards {
    pub fn load(world_dir: &Path) -> Self {
        let path = world_dir.join("garnet-boards.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!("{} is not valid, starting empty: {err}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, world_dir: &Path) {
        let path = world_dir.join("garnet-boards.json");
        let tmp = world_dir.join("garnet-boards.json.tmp");
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(world_dir)?;
            std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(err) = write() {
            tracing::error!("saving {}: {err}", path.display());
        }
    }

    pub fn team_of(&self, member: &str) -> Option<&Team> {
        self.teams.values().find(|t| t.members.contains(member))
    }
}

/// Formatting colour ids as the protocol numbers them.
pub fn color_id(name: &str) -> Option<i32> {
    Some(match name {
        "black" => 0,
        "dark_blue" => 1,
        "dark_green" => 2,
        "dark_aqua" => 3,
        "dark_red" => 4,
        "dark_purple" => 5,
        "gold" => 6,
        "gray" => 7,
        "dark_gray" => 8,
        "blue" => 9,
        "green" => 10,
        "aqua" => 11,
        "red" => 12,
        "light_purple" => 13,
        "yellow" => 14,
        "white" => 15,
        _ => return None,
    })
}

pub fn display_slot_id(name: &str) -> Option<i32> {
    Some(match name {
        "list" => 0,
        "sidebar" => 1,
        "belowName" | "below_name" => 2,
        other => {
            let color = other.strip_prefix("sidebar.team.")?;
            3 + color_id(color)?
        }
    })
}

pub fn visibility_id(name: &str) -> Option<i32> {
    Some(match name {
        "always" => 0,
        "never" => 1,
        "hideForOtherTeams" => 2,
        "hideForOwnTeam" => 3,
        _ => return None,
    })
}

pub fn collision_id(name: &str) -> Option<i32> {
    Some(match name {
        "always" => 0,
        "never" => 1,
        "pushOtherTeams" => 2,
        "pushOwnTeam" => 3,
        _ => return None,
    })
}
