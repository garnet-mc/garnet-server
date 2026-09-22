//! `garnet.toml`: every setting the server has, with sane defaults.
//!
//! On first start the file is written with comments so people can see what
//! exists. A file that fails to parse stops the server with a clear message
//! instead of silently running with defaults.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GarnetConfig {
    pub server: ServerSection,
    pub world: WorldSection,
    pub admin: AdminSection,
    pub rcon: RconSection,
    pub voice: VoiceSection,
    pub anticheat: AnticheatSection,
    pub backups: BackupsSection,
    pub client_mods: ClientModsSection,
}

/// Client-side mods this server wants players to have. The Garnet launcher
/// reads this from the server list ping and installs them before joining,
/// so nobody has to hunt for a modpack.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientModsSection {
    /// Kick clients that cannot prove they have every required mod.
    pub enforce: bool,
    /// Where to send people who need the launcher.
    pub launcher_url: String,
    pub required: Vec<ClientMod>,
    pub optional: Vec<ClientMod>,
    /// A shader pack to offer (same fields as a mod).
    pub shader_pack: Option<ClientMod>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientMod {
    /// Modrinth project slug or id, or any stable identifier for URL mods.
    pub id: String,
    pub name: String,
    /// Modrinth version number or the version string of a URL download.
    pub version: String,
    /// `modrinth` (resolved by the launcher) or `url`.
    pub source: String,
    pub url: String,
    pub sha512: String,
    /// Mod loader the file targets: `fabric` (default) or `neoforge`.
    pub loader: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSection {
    /// Shown in the server list and the panel.
    pub name: String,
    /// Message of the day; `&` colour codes work.
    pub motd: String,
    pub bind: String,
    pub port: u16,
    pub max_players: usize,
    /// Verify players with Mojang. Turn off only behind a proxy or for LAN.
    pub online_mode: bool,
    pub view_distance: i32,
    pub simulation_distance: i32,
    /// Packets at least this large are compressed; -1 disables compression.
    pub compression_threshold: i32,
    /// `latest` or an exact version like `26.3`.
    pub minecraft_version: String,
    /// Where downloaded game data and Java runtimes are kept.
    pub data_dir: String,
    /// `none`, `bungeecord` or `velocity`.
    pub proxy: String,
    /// Shared secret from Velocity's `forwarding.secret` file.
    pub velocity_secret: String,
    /// Whether players must be on the whitelist to join.
    pub whitelist: bool,
    /// Blocks around spawn only ops can edit; 0 disables.
    pub spawn_protection: i32,
    /// Whether players can hurt each other.
    pub pvp: bool,
    /// Message shown to players kicked for being AFK too long (0 = never).
    pub afk_kick_minutes: u64,
    /// Show a code of conduct players must accept before joining (empty = off).
    pub code_of_conduct: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldSection {
    pub name: String,
    /// 0 picks a random seed on world creation.
    pub seed: i64,
    /// `noise` (hills, oceans, trees) or `flat`.
    pub generator: String,
    /// Default game mode: survival, creative, adventure or spectator.
    pub gamemode: String,
    /// peaceful, easy, normal or hard.
    pub difficulty: String,
    pub hardcore: bool,
    pub autosave_minutes: u64,
    /// Chunks no player has seen for this long are unloaded.
    pub chunk_unload_seconds: u64,
    pub daylight_cycle: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminSection {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    /// Address players use to reach the panel (e.g. behind a reverse proxy).
    pub public_url: String,
    /// Set true when the panel is served over HTTPS.
    pub secure_cookies: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RconSection {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    pub password: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSection {
    pub enabled: bool,
    /// UDP port; defaults to the game port.
    pub port: u16,
    /// Distance in blocks at which players can hear each other.
    pub range: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AnticheatSection {
    pub enabled: bool,
    /// Violation points before a kick.
    pub kick_threshold: u32,
    /// Violation points before a temporary ban (0 = never ban).
    pub ban_threshold: u32,
    pub ban_hours: u64,
    /// Points decay by one every this many seconds.
    pub decay_seconds: u64,
    pub max_packets_per_second: u32,
    pub max_chat_per_second: u32,
    /// Extra tolerance on speed checks (1.0 = exact vanilla speeds).
    pub speed_tolerance: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupsSection {
    pub enabled: bool,
    pub interval_hours: u64,
    /// How many backups to keep; older ones are deleted.
    pub keep: usize,
    pub dir: String,
}

impl Default for GarnetConfig {
    fn default() -> Self {
        Self {
            server: ServerSection::default(),
            world: WorldSection::default(),
            admin: AdminSection::default(),
            rcon: RconSection::default(),
            voice: VoiceSection::default(),
            anticheat: AnticheatSection::default(),
            backups: BackupsSection::default(),
            client_mods: ClientModsSection::default(),
        }
    }
}

impl Default for ClientModsSection {
    fn default() -> Self {
        Self {
            enforce: false,
            launcher_url: "https://github.com/garnet-mc/garnet-launcher".into(),
            required: Vec::new(),
            optional: Vec::new(),
            shader_pack: None,
        }
    }
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            name: "A Garnet server".into(),
            motd: "&cA Garnet server".into(),
            bind: "0.0.0.0".into(),
            port: 25565,
            max_players: 100,
            online_mode: true,
            view_distance: 10,
            simulation_distance: 8,
            compression_threshold: 256,
            minecraft_version: "latest".into(),
            data_dir: "data".into(),
            proxy: "none".into(),
            velocity_secret: String::new(),
            whitelist: false,
            spawn_protection: 16,
            pvp: true,
            afk_kick_minutes: 0,
            code_of_conduct: String::new(),
        }
    }
}

impl Default for WorldSection {
    fn default() -> Self {
        Self {
            name: "world".into(),
            seed: 0,
            generator: "noise".into(),
            gamemode: "survival".into(),
            difficulty: "normal".into(),
            hardcore: false,
            autosave_minutes: 5,
            chunk_unload_seconds: 60,
            daylight_cycle: true,
        }
    }
}

impl Default for AdminSection {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: "0.0.0.0".into(),
            port: 8080,
            public_url: String::new(),
            secure_cookies: false,
        }
    }
}

impl Default for RconSection {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "0.0.0.0".into(),
            port: 25575,
            password: String::new(),
        }
    }
}

impl Default for VoiceSection {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 25565,
            range: 48.0,
        }
    }
}

impl Default for AnticheatSection {
    fn default() -> Self {
        Self {
            enabled: true,
            kick_threshold: 20,
            ban_threshold: 60,
            ban_hours: 24,
            decay_seconds: 10,
            max_packets_per_second: 400,
            max_chat_per_second: 3,
            speed_tolerance: 1.15,
        }
    }
}

impl Default for BackupsSection {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_hours: 6,
            keep: 10,
            dir: "backups".into(),
        }
    }
}

impl GarnetConfig {
    /// Loads the config, writing a commented default file if none exists.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if !path.exists() {
            std::fs::write(path, default_config_text())?;
            tracing::info!("wrote default config to {}", path.display());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let config = Self::parse(&text).with_context(|| format!("{} is invalid", path.display()))?;
        Ok(config)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: GarnetConfig = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let s = &self.server;
        if s.max_players == 0 {
            bail!("server.max_players must be at least 1");
        }
        if !(2..=32).contains(&s.view_distance) {
            bail!("server.view_distance must be between 2 and 32");
        }
        if !(2..=32).contains(&s.simulation_distance) {
            bail!("server.simulation_distance must be between 2 and 32");
        }
        if !matches!(s.proxy.as_str(), "none" | "bungeecord" | "velocity") {
            bail!("server.proxy must be none, bungeecord or velocity");
        }
        if s.proxy == "velocity" && s.velocity_secret.is_empty() {
            bail!("server.velocity_secret is required when server.proxy = \"velocity\"");
        }
        if s.proxy != "none" && s.online_mode {
            bail!("server.online_mode must be false behind a proxy; the proxy does the authentication");
        }
        if !matches!(self.world.generator.as_str(), "noise" | "flat") {
            bail!("world.generator must be noise or flat");
        }
        if garnet_protocol::packets::play::GameMode::parse(&self.world.gamemode).is_none() {
            bail!("world.gamemode must be survival, creative, adventure or spectator");
        }
        if !matches!(self.world.difficulty.as_str(), "peaceful" | "easy" | "normal" | "hard") {
            bail!("world.difficulty must be peaceful, easy, normal or hard");
        }
        if self.rcon.enabled && self.rcon.password.is_empty() {
            bail!("rcon.password must be set when RCON is enabled");
        }
        let all_mods = self.client_mods.required.iter().chain(&self.client_mods.optional).chain(self.client_mods.shader_pack.iter());
        for m in all_mods {
            if m.id.is_empty() {
                bail!("every entry in [client_mods] needs an id");
            }
            match m.source.as_str() {
                "" | "modrinth" => {}
                "url" if !m.url.is_empty() => {}
                "url" => bail!("client mod {} has source = \"url\" but no url", m.id),
                other => bail!("client mod {} has unknown source {other:?} (use modrinth or url)", m.id),
            }
        }
        Ok(())
    }
}

/// The file people see on first start. Kept in sync with the defaults above.
pub fn default_config_text() -> String {
    let d = GarnetConfig::default();
    format!(
        r#"# Garnet server configuration.
# Every setting has a sensible default; delete a line to go back to it.

[server]
name = "{name}"
motd = "{motd}"                 # & colour codes work
bind = "{bind}"
port = {port}
max_players = {max_players}
online_mode = {online_mode}                # verify players with Mojang; keep on unless behind a proxy
view_distance = {view_distance}
simulation_distance = {simulation_distance}
compression_threshold = {compression_threshold}
minecraft_version = "{minecraft_version}"      # "latest" or e.g. "26.3"; the game data is downloaded automatically
data_dir = "{data_dir}"
proxy = "{proxy}"                    # none | bungeecord | velocity
velocity_secret = ""
whitelist = {whitelist}
spawn_protection = {spawn_protection}
# Whether players can hurt each other.
pvp = {pvp}
afk_kick_minutes = {afk_kick_minutes}
code_of_conduct = ""

[world]
name = "{wname}"
seed = {seed}                         # 0 = random
generator = "{generator}"             # noise | flat
gamemode = "{gamemode}"
difficulty = "{difficulty}"
hardcore = {hardcore}
autosave_minutes = {autosave_minutes}
chunk_unload_seconds = {chunk_unload_seconds}
daylight_cycle = {daylight_cycle}

[admin]                          # web admin panel
enabled = {aenabled}
bind = "{abind}"
port = {aport}
public_url = ""                  # e.g. "https://panel.example.com" when behind a reverse proxy
secure_cookies = {secure_cookies}

[rcon]
enabled = {renabled}
bind = "{rbind}"
port = {rport}
password = ""

[voice]                          # proximity voice chat (UDP)
enabled = {venabled}
port = {vport}
range = {vrange}

[anticheat]
enabled = {acenabled}
kick_threshold = {kick_threshold}
ban_threshold = {ban_threshold}
ban_hours = {ban_hours}
decay_seconds = {decay_seconds}
max_packets_per_second = {max_packets_per_second}
max_chat_per_second = {max_chat_per_second}
speed_tolerance = {speed_tolerance}

[backups]
enabled = {benabled}
interval_hours = {interval_hours}
keep = {keep}
dir = "{bdir}"

[client_mods]                    # client mods the Garnet launcher installs automatically before joining
enforce = false                  # true kicks clients that lack a required mod
launcher_url = "https://github.com/garnet-mc/garnet-launcher"
required = []
optional = []
# Example:
# [[client_mods.required]]
# id = "sodium"                  # Modrinth slug
# name = "Sodium"
# version = ""                   # Modrinth version number; blank = newest for this Minecraft version
# source = "modrinth"
# [[client_mods.optional]]
# id = "my-hud"
# name = "My HUD"
# version = "1.2.0"
# source = "url"
# url = "https://example.com/my-hud-1.2.0.jar"
# sha512 = ""
"#,
        name = d.server.name,
        motd = d.server.motd,
        bind = d.server.bind,
        port = d.server.port,
        max_players = d.server.max_players,
        online_mode = d.server.online_mode,
        view_distance = d.server.view_distance,
        simulation_distance = d.server.simulation_distance,
        compression_threshold = d.server.compression_threshold,
        minecraft_version = d.server.minecraft_version,
        data_dir = d.server.data_dir,
        proxy = d.server.proxy,
        whitelist = d.server.whitelist,
        spawn_protection = d.server.spawn_protection,
        pvp = d.server.pvp,
        afk_kick_minutes = d.server.afk_kick_minutes,
        wname = d.world.name,
        seed = d.world.seed,
        generator = d.world.generator,
        gamemode = d.world.gamemode,
        difficulty = d.world.difficulty,
        hardcore = d.world.hardcore,
        autosave_minutes = d.world.autosave_minutes,
        chunk_unload_seconds = d.world.chunk_unload_seconds,
        daylight_cycle = d.world.daylight_cycle,
        aenabled = d.admin.enabled,
        abind = d.admin.bind,
        aport = d.admin.port,
        secure_cookies = d.admin.secure_cookies,
        renabled = d.rcon.enabled,
        rbind = d.rcon.bind,
        rport = d.rcon.port,
        venabled = d.voice.enabled,
        vport = d.voice.port,
        vrange = d.voice.range,
        acenabled = d.anticheat.enabled,
        kick_threshold = d.anticheat.kick_threshold,
        ban_threshold = d.anticheat.ban_threshold,
        ban_hours = d.anticheat.ban_hours,
        decay_seconds = d.anticheat.decay_seconds,
        max_packets_per_second = d.anticheat.max_packets_per_second,
        max_chat_per_second = d.anticheat.max_chat_per_second,
        speed_tolerance = d.anticheat.speed_tolerance,
        benabled = d.backups.enabled,
        interval_hours = d.backups.interval_hours,
        keep = d.backups.keep,
        bdir = d.backups.dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_text_parses_to_defaults() {
        let parsed = GarnetConfig::parse(&default_config_text()).unwrap();
        assert_eq!(parsed.server.port, 25565);
        assert_eq!(parsed.world.generator, "noise");
        assert!(parsed.admin.enabled);
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(GarnetConfig::parse("[server]\nview_distance = 99").is_err());
        assert!(GarnetConfig::parse("[server]\nproxy = \"velocity\"\nonline_mode = false").is_err());
    }
}
