//! Everything Garnet needs to know about a specific Minecraft version,
//! obtained from Mojang's own files rather than hand-copied tables.
//!
//! On first start for a version we:
//! 1. download the vanilla server jar from Mojang's version manifest,
//! 2. make sure a suitable Java runtime exists (downloading Mojang's if not),
//! 3. run the jar's built-in data generator to produce the `reports/` JSON
//!    files (block states, registries, packet ids),
//! 4. pull the data-pack registries (dimension types, biomes, ...) and tags
//!    straight out of the jar.
//!
//! The results are cached under the data directory so later starts are
//! instant and need no network or Java.

pub mod blocks;
pub mod datapack;
pub mod generator;
pub mod item_components;
pub mod java;
pub mod light;
pub mod mojang;
pub mod registries;

use anyhow::{Context, Result};
use garnet_protocol::packets::config::{RegistryEntry, TagRegistry};
use garnet_protocol::{Identifier, PacketIds};
use std::path::{Path, PathBuf};

pub use blocks::{BlockRegistry, BlockState};
pub use datapack::{DynamicRegistries, DynamicRegistry, Tags};
pub use item_components::{ItemComponents, ItemDefaults};
pub use registries::Registries;

/// Which Minecraft version to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionChoice {
    /// The newest full release according to Mojang's manifest.
    LatestRelease,
    /// A specific version id, for example `26.3`.
    Exact(String),
}

impl VersionChoice {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "" | "latest" | "latest-release" => Self::LatestRelease,
            other => Self::Exact(other.to_owned()),
        }
    }
}

/// All version-specific game data, loaded once at startup.
pub struct GameData {
    pub version: String,
    pub protocol_version: i32,
    pub data_version: i32,
    pub packet_ids: PacketIds,
    pub blocks: BlockRegistry,
    pub registries: Registries,
    pub dynamic: DynamicRegistries,
    pub tags: Tags,
    /// What a fresh stack of each item carries: wear, stack size and the rest.
    pub item_components: ItemComponents,
    /// Light given off and blocked by each block state.
    pub light: light::LightTable,
    /// Where this version's files live (jar, reports, extracted data).
    pub version_dir: PathBuf,
}

impl GameData {
    /// Loads cached data for the chosen version, preparing it first if needed.
    pub async fn load(data_dir: &Path, choice: &VersionChoice) -> Result<Self> {
        let version_id = match choice {
            VersionChoice::Exact(v) => v.clone(),
            VersionChoice::LatestRelease => mojang::latest_release(data_dir).await?,
        };
        let version_dir = data_dir.join("versions").join(&version_id);
        let reports_dir = version_dir.join("generated").join("reports");

        if !reports_dir.join("packets.json").exists() {
            tracing::info!("preparing game data for Minecraft {version_id} (first run for this version)");
            prepare(data_dir, &version_dir, &version_id).await?;
        } else if !version_dir.join("datapack").join("minecraft").join("loot_table").join("entities").exists() {
            // Data prepared by an older Garnet: pull the newer files out of the jar.
            let inner_jar = version_dir.join("server-inner.jar");
            if inner_jar.exists() {
                datapack::extract_datapack(&inner_jar, &version_dir.join("datapack"))?;
            }
        }

        Self::load_cached(&version_dir, &version_id)
    }

    fn load_cached(version_dir: &Path, version_id: &str) -> Result<Self> {
        let reports_dir = version_dir.join("generated").join("reports");
        let read_json = |name: &str| -> Result<serde_json::Value> {
            let path = reports_dir.join(name);
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        };

        let meta: serde_json::Value = {
            let path = version_dir.join("version.json");
            serde_json::from_str(&std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?)?
        };
        let protocol_version = meta["protocol_version"].as_i64().context("version.json: protocol_version")? as i32;
        let data_version = meta["world_version"].as_i64().unwrap_or(0) as i32;

        let packet_ids = PacketIds::from_report(&read_json("packets.json")?, protocol_version)?;
        let blocks = BlockRegistry::from_report(&read_json("blocks.json")?)?;
        let registries = Registries::from_report(&read_json("registries.json")?)?;
        let dynamic = DynamicRegistries::load(&version_dir.join("datapack"))?;
        let tags = Tags::load(&version_dir.join("datapack").join("tags"), &registries, &dynamic)?;
        let item_components = ItemComponents::load(&reports_dir)?;

        tracing::info!(
            "loaded Minecraft {version_id}: protocol {protocol_version}, {} block states, {} registries, {} tag registries",
            blocks.state_count(),
            registries.len(),
            tags.registries.len()
        );

        let light = light::LightTable::build(&blocks);
        Ok(Self {
            version: version_id.to_owned(),
            protocol_version,
            data_version,
            packet_ids,
            blocks,
            registries,
            dynamic,
            light,
            tags,
            item_components,
            version_dir: version_dir.to_owned(),
        })
    }

    /// The numeric id of an entry in any registry, static or data-driven.
    /// Enchantments and damage types live in the data pack, so a plain
    /// `registries` lookup misses them.
    pub fn id_of(&self, registry: &str, entry: &str) -> Option<i32> {
        self.registries.id_of(registry, entry).or_else(|| self.dynamic.id_of(registry, entry))
    }

    /// The `Registry Data` packets to send during configuration, in order.
    pub fn registry_packets(&self) -> Vec<(Identifier, Vec<RegistryEntry>)> {
        self.dynamic
            .registries
            .iter()
            .map(|reg| {
                let entries = reg
                    .entries
                    .iter()
                    .map(|(id, nbt)| RegistryEntry {
                        id: id.clone(),
                        data: Some(nbt.clone()),
                    })
                    .collect();
                (reg.id.clone(), entries)
            })
            .collect()
    }

    /// The tag registries for the `Update Tags` packet.
    pub fn tag_packets(&self) -> Vec<TagRegistry> {
        self.tags
            .registries
            .iter()
            .map(|(registry, tags)| TagRegistry {
                registry: registry.clone(),
                tags: tags.clone(),
            })
            .collect()
    }
}

/// Runs the whole first-time pipeline for one version.
async fn prepare(data_dir: &Path, version_dir: &Path, version_id: &str) -> Result<()> {
    std::fs::create_dir_all(version_dir)?;
    let version_meta = mojang::fetch_version(data_dir, version_id).await?;

    let jar_path = version_dir.join("server.jar");
    mojang::download_server_jar(&version_meta, &jar_path).await?;

    let inner_jar = datapack::extract_inner_jar(&jar_path, version_dir)?;
    datapack::extract_version_json(&inner_jar, &version_dir.join("version.json"))?;
    datapack::extract_datapack(&inner_jar, &version_dir.join("datapack"))?;

    let java = java::ensure_runtime(data_dir, &version_meta).await?;
    generator::run(&java, &jar_path, &version_dir.join("generated")).await?;
    Ok(())
}
