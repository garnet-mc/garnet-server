//! What mods see of the server: answers their queries right away and
//! queues their actions for the next tick (see `Server::apply_mod_actions`).

use crate::commands::{self, CommandSender};
use crate::player::Player;
use crate::server::{ScheduledTask, Server};
use garnet_api::{Action, GameMode as ApiMode, Location, PlayerInfo, Query, QueryResult};
use garnet_mods::ModHost;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{BlockPos, Identifier, Text};
use std::sync::{Arc, Weak};

/// Created before the server exists (the runtime needs a host, the server
/// owns the runtime), so the pointer is filled in once the server is built.
pub struct ServerHost {
    server: std::sync::OnceLock<Weak<Server>>,
}

impl ServerHost {
    pub fn new() -> Self {
        Self {
            server: std::sync::OnceLock::new(),
        }
    }

    pub fn attach(&self, server: &Arc<Server>) {
        let _ = self.server.set(Arc::downgrade(server));
    }

    fn server(&self) -> Option<Arc<Server>> {
        self.server.get().and_then(Weak::upgrade)
    }
}

pub fn to_api_mode(mode: GameMode) -> ApiMode {
    match mode {
        GameMode::Survival => ApiMode::Survival,
        GameMode::Creative => ApiMode::Creative,
        GameMode::Adventure => ApiMode::Adventure,
        GameMode::Spectator => ApiMode::Spectator,
    }
}

pub fn from_api_mode(mode: ApiMode) -> GameMode {
    match mode {
        ApiMode::Survival => GameMode::Survival,
        ApiMode::Creative => GameMode::Creative,
        ApiMode::Adventure => GameMode::Adventure,
        ApiMode::Spectator => GameMode::Spectator,
    }
}

pub fn player_info(server: &Server, player: &Player) -> PlayerInfo {
    let s = player.lock();
    PlayerInfo {
        uuid: player.uuid,
        name: player.name().to_owned(),
        location: Location {
            x: s.x,
            y: s.y,
            z: s.z,
            yaw: s.yaw,
            pitch: s.pitch,
        },
        game_mode: to_api_mode(s.game_mode),
        latency_ms: s.latency_ms,
        is_op: server.is_op(player.uuid),
    }
}

fn data_path(server: &Server, mod_id: &str) -> std::path::PathBuf {
    server.root.join("mods").join(mod_id).join("data.json")
}

impl ModHost for ServerHost {
    fn act(&self, mod_id: &str, action: Action) {
        let Some(server) = self.server() else { return };
        // Logging and command registration are safe to do immediately and
        // mods expect them to work during init; everything else waits a tick.
        match action {
            Action::Log { level, message } => garnet_mods::log_action(mod_id, level, &message),
            Action::RegisterCommand {
                name,
                description,
                permission,
            } => server.commands.register_mod_command(mod_id, &name, &description, permission),
            other => server
                .mod_actions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((mod_id.to_owned(), other)),
        }
    }

    fn query(&self, mod_id: &str, query: Query) -> QueryResult {
        let Some(server) = self.server() else {
            return QueryResult::Error {
                message: "server is shutting down".into(),
            };
        };
        match query {
            Query::Players => QueryResult::Players {
                players: server.online_players().iter().map(|p| player_info(&server, p)).collect(),
            },
            Query::Player { uuid } => QueryResult::Player {
                player: server.player(uuid).map(|p| player_info(&server, &p)),
            },
            Query::PlayerByName { name } => QueryResult::Player {
                player: server.player_by_name(&name).map(|p| player_info(&server, &p)),
            },
            Query::Block { pos } => {
                let state = server.world().get_block(BlockPos::new(pos.x, pos.y, pos.z)).unwrap_or(0);
                let name = server
                    .data
                    .blocks
                    .block_of_state(state as i32)
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| "minecraft:air".into());
                QueryResult::Block { block: name }
            }
            Query::Time => {
                let world = server.world();
                QueryResult::Time {
                    world_age: world.settings.age,
                    time_of_day: world.settings.time_of_day,
                }
            }
            Query::HasPermission { player, permission } => QueryResult::Bool {
                value: server.has_permission(player, &permission),
            },
            Query::LoadData { key } => {
                let value = std::fs::read_to_string(data_path(&server, mod_id))
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|v| v.get(&key).cloned());
                QueryResult::Data { value }
            }
            Query::ServerInfo => {
                let (tps, _) = server.tps();
                QueryResult::ServerInfo {
                    version: env!("CARGO_PKG_VERSION").into(),
                    minecraft_version: server.data.version.clone(),
                    online: server.online_count(),
                    max_players: server.config().server.max_players,
                    tps,
                }
            }
        }
    }
}

/// Performs one queued mod action. Runs on the tick loop with no locks held.
pub fn apply_action(server: &Arc<Server>, mod_id: &str, action: Action) {
    match action {
        Action::Log { level, message } => garnet_mods::log_action(mod_id, level, &message),
        Action::SendMessage { player, text } => {
            if let Some(p) = server.player(player) {
                p.send(&cb::SystemChat {
                    content: Text::legacy(&text),
                    overlay: false,
                });
            }
        }
        Action::Broadcast { text } => server.broadcast_chat(Text::legacy(&text)),
        Action::ActionBar { player, text } => {
            if let Some(p) = server.player(player) {
                p.send(&cb::SetActionBarText { text: Text::legacy(&text) });
            }
        }
        Action::Title {
            player,
            title,
            subtitle,
            fade_in,
            stay,
            fade_out,
        } => {
            if let Some(p) = server.player(player) {
                p.send(&cb::SetTitleAnimation { fade_in, stay, fade_out });
                p.send(&cb::SetSubtitleText { text: Text::legacy(&subtitle) });
                p.send(&cb::SetTitleText { text: Text::legacy(&title) });
            }
        }
        Action::Kick { player, reason } => {
            if let Some(p) = server.player(player) {
                server.audit.record(&format!("mod:{mod_id}"), "kick", format!("{} ({reason})", p.name()));
                p.disconnect(Text::legacy(&reason));
            }
        }
        Action::Teleport { player, location } => {
            if let Some(p) = server.player(player) {
                commands::teleport(&p, location.x, location.y, location.z, location.yaw, location.pitch);
            }
        }
        Action::SetGameMode { player, game_mode } => {
            if let Some(p) = server.player(player) {
                commands::set_game_mode(server, &p, from_api_mode(game_mode));
            }
        }
        Action::SetBlock { pos, block } => match server.data.blocks.default_state(&block) {
            Some(state) => {
                server.set_block(BlockPos::new(pos.x, pos.y, pos.z), state as u32);
            }
            None => tracing::warn!("mod {mod_id}: unknown block '{block}'"),
        },
        Action::SetTime { time_of_day } => {
            let (age, daylight) = {
                let mut world = server.world();
                world.settings.time_of_day = time_of_day.rem_euclid(24000);
                (world.settings.age, world.settings.daylight_cycle)
            };
            server.broadcast(&cb::SetTime {
                world_age: age,
                time_of_day: time_of_day.rem_euclid(24000),
                advancing: daylight,
            });
        }
        Action::RunCommand { command } => {
            server.commands.execute(
                server,
                &CommandSender::Mod {
                    mod_id: mod_id.to_owned(),
                },
                &command,
            );
        }
        Action::RegisterCommand {
            name,
            description,
            permission,
        } => server.commands.register_mod_command(mod_id, &name, &description, permission),
        Action::Schedule { id, delay_ticks, repeat } => {
            let now = server.current_tick();
            let mut scheduled = server.scheduled.lock().unwrap_or_else(|e| e.into_inner());
            scheduled.retain(|t| !(t.mod_id == mod_id && t.id == id));
            scheduled.push(ScheduledTask {
                mod_id: mod_id.to_owned(),
                id,
                due_tick: now + delay_ticks.max(1) as u64,
                repeat_every: if repeat { Some(delay_ticks.max(1) as u64) } else { None },
            });
        }
        Action::CancelSchedule { id } => {
            server
                .scheduled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|t| !(t.mod_id == mod_id && t.id == id));
        }
        Action::PluginMessage { player, channel, data } => {
            if let (Some(p), Some(channel)) = (server.player(player), Identifier::parse(&channel)) {
                p.send(&cb::ClientboundPluginMessage { channel, data });
            }
        }
        Action::StoreData { key, value } => {
            let path = data_path(server, mod_id);
            let mut doc = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            if let Some(obj) = doc.as_object_mut() {
                obj.insert(key, value);
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap_or_default())
                .and_then(|_| std::fs::rename(&tmp, &path))
                .is_err()
            {
                tracing::warn!("mod {mod_id}: could not save data");
            }
        }
    }
}
