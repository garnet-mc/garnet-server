//! Telling clients which mods this server wants.
//!
//! The same manifest goes out in two places: the server list ping (so the
//! launcher can prepare an instance before the game even starts) and a
//! `garnet:mods` plugin message during the configuration phase (so a Garnet
//! client can check itself and the server can enforce the list).

use crate::config::{ClientMod, GarnetConfig};
use serde::Deserialize;
use serde_json::{json, Value};

pub const CHANNEL: &str = "garnet:mods";

fn mod_json(m: &ClientMod) -> Value {
    json!({
        "id": m.id,
        "name": if m.name.is_empty() { m.id.clone() } else { m.name.clone() },
        "version": m.version,
        "source": if m.source.is_empty() { "modrinth" } else { m.source.as_str() },
        "url": m.url,
        "sha512": m.sha512,
        "loader": if m.loader.is_empty() { "fabric" } else { m.loader.as_str() },
    })
}

/// The `garnet` object in the status response and the config-phase message.
pub fn manifest(config: &GarnetConfig, minecraft_version: &str) -> Value {
    let c = &config.client_mods;
    json!({
        "server": env!("CARGO_PKG_VERSION"),
        "minecraft": minecraft_version,
        "launcher_url": c.launcher_url,
        "enforce": c.enforce,
        "required_mods": c.required.iter().map(mod_json).collect::<Vec<_>>(),
        "optional_mods": c.optional.iter().map(mod_json).collect::<Vec<_>>(),
        "shader_pack": c.shader_pack.as_ref().map(mod_json),
        "voice": config.voice.enabled,
    })
}

/// What a Garnet client answers with on the same channel.
#[derive(Debug, Default, Deserialize)]
pub struct ClientReport {
    #[serde(default)]
    pub installed: Vec<InstalledMod>,
}

#[derive(Debug, Deserialize)]
pub struct InstalledMod {
    pub id: String,
    #[serde(default)]
    pub version: String,
}

/// Names of required mods the report does not cover. A required mod with an
/// empty version matches any installed version.
pub fn missing_required(config: &GarnetConfig, report: Option<&ClientReport>) -> Vec<String> {
    config
        .client_mods
        .required
        .iter()
        .filter(|wanted| {
            !report
                .map(|r| {
                    r.installed
                        .iter()
                        .any(|have| have.id == wanted.id && (wanted.version.is_empty() || have.version == wanted.version))
                })
                .unwrap_or(false)
        })
        .map(|m| if m.name.is_empty() { m.id.clone() } else { m.name.clone() })
        .collect()
}
