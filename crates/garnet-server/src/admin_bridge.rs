//! Answers the web panel's requests. Runs as its own task and only ever
//! touches server state through the same methods commands use.

use crate::commands::{self, CommandSender};
use crate::config::GarnetConfig;
use crate::player::Player;
use crate::server::Server;
use garnet_admin::api::{AdminRequest, AdminResponse, BanEntry, ModSummary, PlayerSummary, ServerStatus};
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{GameProfile, Text};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub async fn run(server: Arc<Server>, mut requests: mpsc::Receiver<(AdminRequest, oneshot::Sender<AdminResponse>)>) {
    while let Some((request, reply)) = requests.recv().await {
        let server = Arc::clone(&server);
        // Backups and saves block; everything else is quick.
        let response = match request {
            AdminRequest::CreateBackup { actor } => {
                tokio::task::spawn_blocking(move || match crate::backup::create(&server, &format!("panel:{actor}")) {
                    Ok(_) => AdminResponse::Ok,
                    Err(err) => AdminResponse::error(format!("{err:#}")),
                })
                .await
                .unwrap_or_else(|_| AdminResponse::error("backup task failed"))
            }
            other => tokio::task::spawn_blocking(move || handle(&server, other))
                .await
                .unwrap_or_else(|_| AdminResponse::error("request failed")),
        };
        let _ = reply.send(response);
    }
}

fn player_summary(server: &Server, p: &Player) -> PlayerSummary {
    let s = p.lock();
    PlayerSummary {
        uuid: p.uuid,
        name: p.name().to_owned(),
        ip: p.ip.clone(),
        game_mode: s.game_mode.name().to_owned(),
        latency_ms: s.latency_ms,
        x: s.x,
        y: s.y,
        z: s.z,
        dimension: s.dimension.clone(),
        is_op: server.is_op(p.uuid),
        online_seconds: p.joined_at.elapsed().as_secs(),
        voice_connected: server.voice.as_ref().map(|v| v.is_connected(p.uuid)).unwrap_or(false),
        violations: s.checks.points,
    }
}

fn resolve(server: &Server, target: &str) -> (Uuid, String) {
    if let Ok(uuid) = Uuid::parse_str(target) {
        if let Some(p) = server.player(uuid) {
            return (uuid, p.name().to_owned());
        }
        return (uuid, target.to_owned());
    }
    match server.player_by_name(target) {
        Some(p) => (p.uuid, p.name().to_owned()),
        None => (GameProfile::offline(target).id, target.to_owned()),
    }
}

fn handle(server: &Arc<Server>, request: AdminRequest) -> AdminResponse {
    match request {
        AdminRequest::Status => {
            let (tps, tick_ms) = server.tps();
            let config = server.config();
            let world = server.world();
            AdminResponse::Status(ServerStatus {
                name: config.server.name.clone(),
                version: env!("CARGO_PKG_VERSION").into(),
                minecraft_version: server.data.version.clone(),
                protocol_version: server.data.protocol_version,
                online: server.online_count(),
                max_players: config.server.max_players,
                tps,
                tick_ms,
                uptime_seconds: server.started.elapsed().as_secs(),
                loaded_chunks: world.loaded_chunk_count(),
                memory_mb: Server::memory_mb(),
                world_name: world.settings.name.clone(),
                world_time: world.settings.time_of_day,
                online_mode: config.server.online_mode,
                voice_connected: server.voice.as_ref().map(|v| v.connected_count()).unwrap_or(0),
            })
        }
        AdminRequest::Players => AdminResponse::Players(
            server.online_players().iter().map(|p| player_summary(server, p)).collect(),
        ),
        AdminRequest::Kick { uuid, reason, actor } => match server.player(uuid) {
            Some(p) => {
                let reason = if reason.is_empty() { "Kicked by an admin.".to_owned() } else { reason };
                server.audit.record(&actor, "kick", format!("{} ({reason})", p.name()));
                p.disconnect(Text::new(reason.clone()));
                server.broadcast_chat(Text::new(format!("{} was kicked: {reason}", p.name())).color("yellow"));
                AdminResponse::Ok
            }
            None => AdminResponse::error("player is not online"),
        },
        AdminRequest::Ban {
            target,
            reason,
            hours,
            actor,
        } => {
            let (uuid, name) = resolve(server, &target);
            if let Err(err) = server.lists.ban(uuid, &name, &reason, &actor, hours) {
                return AdminResponse::error(err.to_string());
            }
            server.audit.record(&actor, "ban", format!("{name} ({reason}) {}", hours.map(|h| format!("{h}h")).unwrap_or_else(|| "permanent".into())));
            if let Some(p) = server.player(uuid) {
                p.disconnect(Text::new(format!("You are banned from this server.\n{reason}")));
            }
            AdminResponse::Ok
        }
        AdminRequest::BanIp { ip, reason, actor } => {
            if let Err(err) = server.lists.ban_ip(&ip, &reason, &actor, None) {
                return AdminResponse::error(err.to_string());
            }
            server.audit.record(&actor, "ban-ip", format!("{ip} ({reason})"));
            for p in server.online_players() {
                if p.ip == ip {
                    p.disconnect(Text::new(format!("Your IP address is banned from this server.\n{reason}")));
                }
            }
            AdminResponse::Ok
        }
        AdminRequest::Pardon { target, actor } => match server.lists.pardon(&target) {
            Ok(true) => {
                server.audit.record(&actor, "pardon", target);
                AdminResponse::Ok
            }
            Ok(false) => AdminResponse::error("no such ban"),
            Err(err) => AdminResponse::error(err.to_string()),
        },
        AdminRequest::Bans => {
            let (players, ips) = server.lists.bans();
            let mut out: Vec<BanEntry> = players
                .into_iter()
                .map(|b| BanEntry {
                    name: b.name,
                    uuid: Some(b.uuid),
                    reason: b.reason,
                    source: b.source,
                    created: b.created,
                    expires: if b.expires == "forever" { None } else { Some(b.expires) },
                })
                .collect();
            out.extend(ips.into_iter().map(|b| BanEntry {
                name: b.ip,
                uuid: None,
                reason: b.reason,
                source: b.source,
                created: b.created,
                expires: if b.expires == "forever" { None } else { Some(b.expires) },
            }));
            AdminResponse::Bans(out)
        }
        AdminRequest::Whitelist => AdminResponse::Names(server.lists.whitelist()),
        AdminRequest::WhitelistAdd { name, actor } => {
            let (uuid, name) = resolve(server, &name);
            match server.lists.whitelist_add(uuid, &name) {
                Ok(_) => {
                    server.audit.record(&actor, "whitelist-add", name);
                    AdminResponse::Ok
                }
                Err(err) => AdminResponse::error(err.to_string()),
            }
        }
        AdminRequest::WhitelistRemove { name, actor } => match server.lists.whitelist_remove(&name) {
            Ok(_) => {
                server.audit.record(&actor, "whitelist-remove", name);
                AdminResponse::Ok
            }
            Err(err) => AdminResponse::error(err.to_string()),
        },
        AdminRequest::SetGameMode { uuid, mode, actor } => {
            let Some(mode) = GameMode::parse(&mode) else {
                return AdminResponse::error("unknown game mode");
            };
            match server.player(uuid) {
                Some(p) => {
                    commands::set_game_mode(server, &p, mode);
                    server.audit.record(&actor, "gamemode", format!("{} -> {}", p.name(), mode.name()));
                    AdminResponse::Ok
                }
                None => AdminResponse::error("player is not online"),
            }
        }
        AdminRequest::SetOp { uuid, op, actor } => {
            let name = server.player(uuid).map(|p| p.name().to_owned()).unwrap_or_else(|| uuid.to_string());
            match server.lists.set_op(uuid, &name, op) {
                Ok(_) => {
                    server.audit.record(&actor, if op { "op" } else { "deop" }, name.clone());
                    if let Some(p) = server.player(uuid) {
                        p.send(&cb::EntityEvent {
                            entity_id: p.entity_id,
                            status: if op { 28 } else { 24 },
                        });
                        p.send(&server.commands.tree_packet(server, &CommandSender::Player(Arc::clone(&p))));
                    }
                    AdminResponse::Ok
                }
                Err(err) => AdminResponse::error(err.to_string()),
            }
        }
        AdminRequest::Message { uuid, text, actor } => {
            let content = Text::legacy(&text);
            match uuid {
                Some(uuid) => match server.player(uuid) {
                    Some(p) => {
                        p.send(&cb::SystemChat {
                            content: Text::new(format!("[{actor}] ")).color("light_purple").append(content),
                            overlay: false,
                        });
                        AdminResponse::Ok
                    }
                    None => AdminResponse::error("player is not online"),
                },
                None => {
                    server.broadcast_chat(Text::new(format!("[{actor}] ")).color("light_purple").append(content));
                    AdminResponse::Ok
                }
            }
        }
        AdminRequest::Teleport { uuid, x, y, z, actor } => match server.player(uuid) {
            Some(p) => {
                let (yaw, pitch) = {
                    let s = p.lock();
                    (s.yaw, s.pitch)
                };
                commands::teleport(&p, x, y, z, yaw, pitch);
                server.audit.record(&actor, "teleport", format!("{} -> {x:.0} {y:.0} {z:.0}", p.name()));
                AdminResponse::Ok
            }
            None => AdminResponse::error("player is not online"),
        },
        AdminRequest::Command { command, actor } => {
            let replies = Arc::new(Mutex::new(Vec::new()));
            let sender = CommandSender::Panel {
                username: actor,
                replies: Arc::clone(&replies),
            };
            server.commands.execute(server, &sender, &command);
            let lines = replies.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for line in &lines {
                tracing::info!("{line}");
            }
            AdminResponse::Text(lines.join("\n"))
        }
        AdminRequest::ConsoleHistory => AdminResponse::Lines(server.logs.history()),
        AdminRequest::Mods => AdminResponse::Mods(
            server
                .mods
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .list()
                .into_iter()
                .map(|m| ModSummary {
                    id: m.id,
                    name: m.name,
                    version: m.version,
                    authors: m.authors,
                    description: m.description,
                    kind: m.kind.to_owned(),
                    enabled: m.enabled,
                })
                .collect(),
        ),
        AdminRequest::SetModEnabled { id, enabled, actor } => {
            let ok = server.mods.lock().unwrap_or_else(|e| e.into_inner()).set_enabled(&id, enabled);
            if ok {
                server.audit.record(&actor, if enabled { "mod-enable" } else { "mod-disable" }, id);
                AdminResponse::Ok
            } else {
                AdminResponse::error("no such mod")
            }
        }
        AdminRequest::ReloadMods { actor } => {
            server.commands.remove_mod_commands();
            let result = server.mods.lock().unwrap_or_else(|e| e.into_inner()).reload();
            match result {
                Ok(()) => {
                    server.audit.record(&actor, "mods-reload", "");
                    AdminResponse::Ok
                }
                Err(err) => AdminResponse::error(format!("{err:#}")),
            }
        }
        AdminRequest::ConfigText => match std::fs::read_to_string(&server.config_path) {
            Ok(text) => AdminResponse::Text(text),
            Err(err) => AdminResponse::error(err.to_string()),
        },
        AdminRequest::SaveConfigText { text, actor } => match GarnetConfig::parse(&text) {
            Ok(parsed) => {
                let tmp = server.config_path.with_extension("toml.tmp");
                if let Err(err) = std::fs::write(&tmp, &text).and_then(|_| std::fs::rename(&tmp, &server.config_path)) {
                    return AdminResponse::error(err.to_string());
                }
                *server.config.write().unwrap_or_else(|e| e.into_inner()) = parsed;
                server.audit.record(&actor, "config-save", "garnet.toml updated");
                AdminResponse::Ok
            }
            Err(err) => AdminResponse::error(format!("config rejected: {err:#}")),
        },
        AdminRequest::Audit => AdminResponse::Audit(server.audit.recent()),
        AdminRequest::Violations => AdminResponse::Violations(server.anticheat.recent()),
        AdminRequest::Backups => match crate::backup::list(server) {
            Ok(list) => AdminResponse::Backups(list),
            Err(err) => AdminResponse::error(format!("{err:#}")),
        },
        AdminRequest::CreateBackup { .. } => AdminResponse::error("handled elsewhere"),
        AdminRequest::VoiceMute { uuid, muted, actor } => match &server.voice {
            Some(voice) => {
                voice.set_muted(uuid, muted);
                server.audit.record(&actor, if muted { "voice-mute" } else { "voice-unmute" }, uuid.to_string());
                AdminResponse::Ok
            }
            None => AdminResponse::error("voice chat is disabled"),
        },
        AdminRequest::SaveWorld { actor } => {
            server.save_everything(&format!("panel:{actor}"));
            AdminResponse::Ok
        }
        AdminRequest::Stop { actor } => {
            server.audit.record(&actor, "stop", "server stop requested from the panel");
            server.request_stop();
            AdminResponse::Ok
        }
        AdminRequest::Noop => AdminResponse::Ok,
    }
}
