//! Garnet: a Minecraft server.
//!
//! `garnet` with no arguments starts a server in the current directory,
//! creating the config, downloading the game data and generating a world on
//! first run.

mod admin_bridge;
mod anticheat;
mod audit;
mod anvil;
mod backup;
mod blocks;
mod brewing;
mod board_commands;
mod boards;
mod chunks;
mod client_mods;
mod commands;
mod containers;
mod config;
mod durability;
mod enchanting;
mod entities;
mod entity_commands;
mod experience;
mod explosions;
mod fluids;
mod functions;
mod hoppers;
mod furnaces;
mod inventory;
mod items;
mod lists;
mod logging;
mod loot;
mod mod_host;
mod mobs;
mod net;
mod pathfinding;
mod player;
mod potions;
mod projectiles;
mod playerdata;
mod rcon;
mod recipes;
mod redstone;
mod rules;
mod server;
mod survival;
mod vanilla_commands;
mod world_entities;

use crate::config::GarnetConfig;
use crate::server::Server;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use garnet_admin::api::AdminHandle;
use garnet_data::{GameData, VersionChoice};
use garnet_world::world::{GeneratorKind, WorldSettings};
use garnet_world::World;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

#[derive(Parser)]
#[command(name = "garnet", version, about = "A Minecraft server written in Rust")]
struct Cli {
    /// Directory holding garnet.toml, the world and everything else.
    #[arg(long, default_value = ".")]
    dir: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the server (the default).
    Run,
    /// Download and prepare the game data without starting.
    Prepare,
    /// Print the default configuration.
    DefaultConfig,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::DefaultConfig)) {
        print!("{}", config::default_config_text());
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    let logs = logging::init();
    let root = std::fs::canonicalize(&cli.dir).unwrap_or(cli.dir.clone());
    std::fs::create_dir_all(&root)?;
    tracing::info!("Garnet {} starting in {}", env!("CARGO_PKG_VERSION"), root.display());

    let config_path = root.join("garnet.toml");
    let config = match GarnetConfig::load_or_create(&config_path) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("{err:#}");
            tracing::error!("fix garnet.toml and start again (or delete it to get the defaults)");
            std::process::exit(2);
        }
    };

    let data_dir = root.join(&config.server.data_dir);
    let data = GameData::load(&data_dir, &VersionChoice::parse(&config.server.minecraft_version))
        .await
        .context("preparing game data")?;
    let data = Arc::new(data);
    if matches!(cli.command, Some(Command::Prepare)) {
        tracing::info!("game data for Minecraft {} is ready", data.version);
        return Ok(());
    }

    let world = World::open(
        &root.join(&config.world.name),
        Arc::clone(&data),
        WorldSettings {
            name: config.world.name.clone(),
            seed: if config.world.seed == 0 { rand::random::<i64>() } else { config.world.seed },
            generator: GeneratorKind::parse(&config.world.generator).unwrap_or(GeneratorKind::Noise),
            spawn: garnet_protocol::BlockPos::new(0, 70, 0),
            age: 0,
            time_of_day: 1000,
            daylight_cycle: config.world.daylight_cycle,
        },
    )
    .context("opening the world")?;

    let lists = lists::Lists::load(&root).context("loading bans/ops/whitelist")?;
    let permissions = load_permissions(&root)?;
    let audit = audit::Audit::new(root.join("logs").join("audit.jsonl"));
    let anticheat = anticheat::AntiCheat::new(config.anticheat.clone());
    let keys = server::generate_keys().context("generating the encryption key")?;

    let voice = if config.voice.enabled {
        match garnet_voice::VoiceServer::bind(&config.server.bind, config.voice.port, Some(config.voice.range)).await {
            Ok(v) => Some(v),
            Err(err) => {
                tracing::error!("voice chat could not bind UDP port {}: {err}; voice is disabled", config.voice.port);
                None
            }
        }
    } else {
        None
    };

    let (shutdown, _) = tokio::sync::watch::channel(false);
    let (admin_tx, admin_rx) = tokio::sync::mpsc::channel(64);

    let commands = commands::CommandRegistry::default();
    commands::register_builtins(&commands);
    vanilla_commands::register(&commands);
    functions::register(&commands);
    entity_commands::register(&commands);

    // The mod runtime needs a handle to the server and the server owns the
    // runtime, so the host is attached to the server right after it is built.
    let host = Arc::new(mod_host::ServerHost::new());
    let mods = garnet_mods::ModRuntime::new(&root.join("mods"), Arc::clone(&host) as Arc<dyn garnet_mods::ModHost>)
        .context("starting the mod runtime")?;

    let server = Arc::new({
        Server {
            config: RwLock::new(config.clone()),
            config_path: config_path.clone(),
            root: root.clone(),
            data: Arc::clone(&data),
            world: Mutex::new(world),
            players: RwLock::new(Default::default()),
            chunk_watchers: Mutex::new(Default::default()),
            players_by_chunk: Mutex::new(Default::default()),
            chunk_cache: Mutex::new(Default::default()),
            generating: Mutex::new(Default::default()),
            lists,
            permissions: RwLock::new(permissions),
            audit,
            anticheat,
            mods: Mutex::new(mods),
            mod_actions: Mutex::new(Vec::new()),
            scheduled: Mutex::new(Vec::new()),
            commands,
            rules: RwLock::new(rules::WorldRules::load(&root.join(&config.world.name), &config.world.difficulty)),
            loot: loot::LootTables::new(&data),
            recipes: recipes::Recipes::load(&data),
            enchantments: enchanting::Enchantments::load(&data),
            brewing: brewing::Brewing::load(&data),
            stands: brewing::Stands::new(),
            mob_loot: loot::LootTables::entities(&data),
            furnaces: furnaces::Furnaces::new(),
            block_ticks: Mutex::new(Vec::new()),
            fuses: Mutex::new(Vec::new()),
            changed_blocks: Mutex::new(Vec::new()),
            stage: std::sync::atomic::AtomicUsize::new(0),
            stage_len: std::sync::atomic::AtomicUsize::new(0),
            entities: Mutex::new(world_entities::Entities::new(&root.join(&config.world.name))),
            boards: Mutex::new(boards::Boards::load(&root.join(&config.world.name))),
            voice,
            panel: Mutex::new(None),
            logs: logs.clone(),
            keys,
            started: Instant::now(),
            tick: AtomicU64::new(0),
            tick_times: Mutex::new(Default::default()),
            next_entity_id: AtomicI32::new(1),
            shutdown,
            stopping: AtomicBool::new(false),
            datapacks_present: AtomicBool::new(false),
            http: reqwest::Client::builder()
                .user_agent(concat!("garnet/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client"),
        }
    });
    host.attach(&server);
    functions::refresh_presence(&server);
    functions::run_tag(&server, "minecraft:load");

    // Mods: load from disk, then enable once the world is open.
    {
        let mut mods = server.mods.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = mods.load_wasm_mods() {
            tracing::error!("loading mods: {err:#}");
        }
        mods.enable_all();
    }

    // Admin panel.
    if config.admin.enabled {
        let panel = garnet_admin::start(
            garnet_admin::AdminConfig {
                bind: config.admin.bind.clone(),
                port: config.admin.port,
                public_url: if config.admin.public_url.is_empty() { None } else { Some(config.admin.public_url.clone()) },
                secure_cookies: config.admin.secure_cookies,
            },
            AdminHandle {
                requests: admin_tx,
                logs: logs.sender.clone(),
            },
            &root.join("panel-users.json"),
        )
        .await
        .context("starting the admin panel")?;
        if let Some(link) = panel.setup_link() {
            tracing::info!("");
            tracing::info!("  Admin panel setup: open this link to create the owner account");
            tracing::info!("  {link}");
            tracing::info!("");
        } else {
            tracing::info!("admin panel: {}", panel.public_url());
        }
        *server.panel.lock().unwrap_or_else(|e| e.into_inner()) = Some(panel);
        tokio::spawn(admin_bridge::run(Arc::clone(&server), admin_rx));
    }

    if config.rcon.enabled {
        rcon::start(Arc::clone(&server)).await.context("starting RCON")?;
    }

    server.watch_ticks();
    tokio::spawn(Arc::clone(&server).run_ticks());
    tokio::spawn(console_input(Arc::clone(&server)));
    tokio::spawn(shutdown_signal(Arc::clone(&server)));

    {
        let mut mods = server.mods.lock().unwrap_or_else(|e| e.into_inner());
        mods.dispatch(&garnet_api::Event::ServerStarted);
    }
    tracing::info!(
        "ready: Minecraft {} on port {} ({} mode)",
        data.version,
        config.server.port,
        if config.server.online_mode { "online" } else { "offline" }
    );

    net::listen(Arc::clone(&server)).await?;

    // Shutting down.
    tracing::info!("stopping...");
    {
        let mut mods = server.mods.lock().unwrap_or_else(|e| e.into_inner());
        mods.dispatch(&garnet_api::Event::ServerStopping);
        mods.shutdown();
    }
    for player in server.online_players() {
        player.disconnect(garnet_protocol::Text::new("Server closed"));
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    for player in server.online_players() {
        playerdata::save(&server, &player);
    }
    server.save_everything("shutdown");
    tracing::info!("goodbye");
    std::process::exit(0);
}

fn load_permissions(root: &std::path::Path) -> Result<garnet_api::Permissions> {
    let path = root.join("permissions.toml");
    if !path.exists() {
        std::fs::write(
            &path,
            r#"# Permission groups and users. Ops have every permission unless a node
# starting with "-" takes it away. Wildcards: garnet.command.*

[groups.default]
permissions = ["garnet.command.help", "garnet.command.list", "garnet.command.msg", "garnet.command.tps", "garnet.command.spawn", "garnet.command.seed"]
prefix = ""

[groups.moderator]
inherits = ["default"]
permissions = ["garnet.command.kick", "garnet.command.ban", "garnet.command.pardon", "garnet.command.tp"]
prefix = "&9[Mod]"

# [users."00000000-0000-0000-0000-000000000000"]
# groups = ["moderator"]
# permissions = ["garnet.command.gamemode"]
"#,
        )?;
    }
    let text = std::fs::read_to_string(&path)?;
    toml::from_str(&text).with_context(|| format!("{} is invalid", path.display()))
}

/// Reads commands typed into the terminal.
async fn console_input(server: Arc<Server>) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        server.commands.execute(&server, &commands::CommandSender::Console, &line);
    }
}

async fn shutdown_signal(server: Arc<Server>) {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("received Ctrl-C");
    server.request_stop();
}
