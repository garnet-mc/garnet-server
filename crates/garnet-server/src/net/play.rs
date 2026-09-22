//! Play state: joining, leaving and everything the client sends in between.

use crate::anticheat::{Escalation, Surroundings, Violation};
use crate::commands::{self, CommandSender};
use crate::entities;
use crate::player::Player;
use crate::server::Server;
use garnet_api::Event;
use garnet_protocol::packets::config::ClientInformation;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::serverbound as sb;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{BlockPos, Identifier, PacketReader, ServerboundPacket, Text};
use std::sync::Arc;
use std::time::Instant;

/// The full join sequence, in the order the vanilla server uses.
pub async fn on_join(server: &Arc<Server>, player: &Arc<Player>) {
    let config = server.config();
    let (dimension_names, dimension_type, seed, spawn, sea_level, time_of_day, world_age, daylight) = {
        let world = server.world();
        (
            vec![Identifier::minecraft("overworld")],
            server.data.dynamic.id_of("dimension_type", "overworld").unwrap_or(0),
            world.settings.seed,
            world.settings.spawn,
            garnet_world::generator::SEA_LEVEL,
            world.settings.time_of_day,
            world.settings.age,
            world.settings.daylight_cycle,
        )
    };
    let (game_mode, x, y, z, yaw, pitch, view) = {
        let s = player.lock();
        (s.game_mode, s.x, s.y, s.z, s.yaw, s.pitch, s.view_distance)
    };

    player.send(&cb::Login {
        entity_id: player.entity_id,
        is_hardcore: config.world.hardcore,
        dimension_names,
        max_players: config.server.max_players as i32,
        view_distance: config.server.view_distance,
        simulation_distance: config.server.simulation_distance,
        reduced_debug_info: false,
        enable_respawn_screen: true,
        do_limited_crafting: false,
        spawn: cb::SpawnInfo {
            dimension_type,
            dimension_name: Identifier::minecraft("overworld"),
            hashed_seed: hashed_seed(seed),
            game_mode,
            previous_game_mode: None,
            is_debug: false,
            is_flat: config.world.generator == "flat",
            death_location: None,
            portal_cooldown: 0,
            sea_level,
        },
        online_mode: config.server.online_mode,
        enforces_secure_chat: false,
    });
    let (difficulty, locked) = {
        let rules = server.rules.read().unwrap_or_else(|e| e.into_inner());
        (rules.difficulty_id(), rules.difficulty_locked)
    };
    player.send(&cb::ChangeDifficulty { difficulty, locked });
    player.send(&cb::PlayerAbilities::for_game_mode(game_mode));
    player.send(&cb::SetHeldSlot { slot: 0 });
    let op_level = server.lists.op_level(player.uuid);
    player.send(&cb::EntityEvent {
        entity_id: player.entity_id,
        status: 24 + op_level.min(4),
    });
    player.send(&server.commands.tree_packet(server, &CommandSender::Player(Arc::clone(player))));
    drop(config);

    commands::teleport(player, x, y, z, yaw, pitch);

    // Tab list: the newcomer gets everyone, everyone gets the newcomer.
    let mut entries = Vec::new();
    for other in server.online_players() {
        let mode = other.lock().game_mode;
        entries.push(cb::PlayerInfoEntry {
            uuid: other.uuid,
            profile: Some(other.profile.clone()),
            game_mode: Some(mode),
            listed: Some(true),
            latency_ms: Some(other.lock().latency_ms),
            ..Default::default()
        });
    }
    use cb::player_info_action::*;
    player.send(&cb::PlayerInfoUpdate {
        actions: ADD_PLAYER | UPDATE_GAME_MODE | UPDATE_LISTED | UPDATE_LATENCY,
        entries,
    });
    server.broadcast_except(&cb::PlayerInfoUpdate::add(player.profile.clone(), game_mode, 0), player.uuid);

    player.send(&cb::SetTime {
        world_age,
        time_of_day,
        advancing: daylight,
    });
    player.send(&cb::SetDefaultSpawnPosition {
        dimension: Identifier::minecraft("overworld"),
        position: spawn,
        yaw: 0.0,
        pitch: 0.0,
    });
    player.send(&cb::GameEvent {
        event: cb::GameEventKind::StartWaitingForChunks,
        value: 0.0,
    });
    let center = player.lock().chunk();
    player.send(&cb::SetCenterChunk {
        chunk_x: center.x,
        chunk_z: center.z,
    });
    player.send(&cb::SetRenderDistance { distance: view });
    player.send(&cb::SetSimulationDistance {
        distance: server.config().server.simulation_distance,
    });
    let (health, food) = {
        let s = player.lock();
        (s.health, s.food)
    };
    player.send(&cb::SetHealth {
        health,
        food,
        saturation: 5.0,
    });
    let (level, total) = {
        let s = player.lock();
        (s.xp_level, s.xp_total)
    };
    player.send(&cb::SetExperience { bar: 0.0, level, total });
    crate::items::sync_inventory(player);
    let (effects, attributes) = {
        let s = player.lock();
        (s.effects.clone(), s.attributes.clone())
    };
    let now = server.current_tick();
    for effect in effects {
        player.send(&cb::UpdateMobEffect {
            entity_id: player.entity_id,
            effect: effect.id,
            amplifier: effect.amplifier,
            duration: effect.expires_tick.map(|t| t.saturating_sub(now) as i32).unwrap_or(-1),
            ambient: false,
            show_particles: effect.particles,
            show_icon: true,
        });
    }
    if !attributes.is_empty() {
        let snapshots = attributes
            .iter()
            .filter_map(|(name, base)| {
                Some(cb::AttributeSnapshot {
                    attribute: server.data.registries.id_of("attribute", name)?,
                    base: *base,
                    modifiers: Vec::new(),
                })
            })
            .collect();
        player.send(&cb::UpdateAttributes {
            entity_id: player.entity_id,
            attributes: snapshots,
        });
    }
    player.send(&crate::vanilla_commands::border_packet(server));
    player.send(&crate::vanilla_commands::tick_packet(server));
    player.send(&crate::vanilla_commands::game_rule_packet(server));
    crate::vanilla_commands::send_weather(server, player);
    crate::board_commands::send_boards(server, player);

    // Voice chat: hand the client its UDP secret over our plugin channel.
    if let Some(voice) = &server.voice {
        let secret = voice.register(player.uuid);
        let mut w = garnet_protocol::PacketWriter::new();
        w.write_u8(garnet_voice::PROTOCOL_VERSION);
        w.write_u16(voice.port);
        w.write_bytes(&secret);
        player.send(&cb::ClientboundPluginMessage {
            channel: Identifier::parse("garnet:voice").unwrap(),
            data: w.into_inner(),
        });
    }

    server.broadcast_chat(Text::new(format!("{} joined the game", player.name())).color("yellow"));
    welcome(server, player);
    tracing::info!("{} joined ({} online)", player.name(), server.online_count());
    let event = Event::PlayerJoin {
        uuid: player.uuid,
        name: player.name().to_owned(),
    };
    server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
}

/// A short Garnet-styled hello so players know what they joined and what
/// the client can do here.
fn welcome(server: &Arc<Server>, player: &Arc<Player>) {
    let config = server.config.read().unwrap_or_else(|e| e.into_inner());
    let name = config.server.name.clone();
    let voice = config.voice.enabled;
    drop(config);
    let mut line = Text::new("◆ ").color("#e04060").append(Text::new("Garnet").color("#e04060").bold()).append(Text::new(" · ").color("gray")).append(Text::new(name).color("white"));
    if voice {
        line = line.append(Text::new("  ·  voice chat on (hold V)").color("gray"));
    }
    player.send(&cb::SystemChat { content: line, overlay: false });
}

pub async fn on_quit(server: &Arc<Server>, player: &Arc<Player>) {
    if server.remove_player(player.uuid).is_none() {
        return;
    }
    crate::playerdata::save(server, player);
    if let Some(voice) = &server.voice {
        voice.unregister(player.uuid);
    }
    server.broadcast(&cb::PlayerInfoRemove {
        uuids: vec![player.uuid],
    });
    // Anyone who could see the entity forgets it.
    for other in server.online_players() {
        let saw = other.lock().visible_players.remove(&player.uuid);
        if saw {
            other.send(&cb::RemoveEntities {
                entity_ids: vec![player.entity_id],
            });
        }
    }
    server.broadcast_chat(Text::new(format!("{} left the game", player.name())).color("yellow"));
    tracing::info!("{} left ({} online)", player.name(), server.online_count());
    let event = Event::PlayerQuit {
        uuid: player.uuid,
        name: player.name().to_owned(),
    };
    server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
}

/// First eight bytes of SHA-256(seed), which the client uses for biome noise.
fn hashed_seed(seed: i64) -> i64 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(seed.to_be_bytes());
    i64::from_be_bytes(digest[..8].try_into().unwrap())
}

pub async fn handle(server: &Arc<Server>, player: &Arc<Player>, name: &str, r: &mut PacketReader<'_>) -> anyhow::Result<()> {
    // Every packet counts against the flood limit first.
    let flood = {
        let mut s = player.lock();
        s.last_activity = Instant::now();
        server.anticheat.count_packet(&mut s.checks)
    };
    if let Some(v) = flood {
        punish(server, player, &v);
        return Ok(());
    }

    match name {
        "accept_teleportation" => {
            let p = sb::ConfirmTeleport::read(r)?;
            let mut s = player.lock();
            if s.pending_teleport == Some(p.teleport_id) {
                s.pending_teleport = None;
                s.checks.awaiting_teleport = false;
            }
        }
        "chat" => {
            let p = sb::ChatMessage::read(r)?;
            handle_chat(server, player, p.message);
        }
        "chat_command" => {
            let p = sb::ChatCommand::read(r)?;
            server.commands.execute(server, &CommandSender::Player(Arc::clone(player)), &p.command);
        }
        "chat_command_signed" => {
            let p = sb::SignedChatCommand::read(r)?;
            server.commands.execute(server, &CommandSender::Player(Arc::clone(player)), &p.command);
        }
        "command_suggestion" => {
            let p = sb::CommandSuggestionRequest::read(r)?;
            handle_suggestions(server, player, p);
        }
        "chunk_batch_received" => {
            let p = sb::ChunkBatchReceived::read(r)?;
            let mut s = player.lock();
            s.unacked_batches = s.unacked_batches.saturating_sub(1);
            if p.chunks_per_tick.is_finite() {
                s.chunks_per_tick = p.chunks_per_tick.clamp(1.0, 64.0);
            }
        }
        "keep_alive" => {
            let p = sb::KeepAlive::read(r)?;
            let mut s = player.lock();
            if s.awaiting_keepalive && s.keepalive_id == p.id {
                s.awaiting_keepalive = false;
                s.latency_ms = s.keepalive_sent.elapsed().as_millis() as i32;
                let latency = s.latency_ms;
                drop(s);
                server.broadcast(&cb::PlayerInfoUpdate {
                    actions: cb::player_info_action::UPDATE_LATENCY,
                    entries: vec![cb::PlayerInfoEntry {
                        uuid: player.uuid,
                        latency_ms: Some(latency),
                        ..Default::default()
                    }],
                });
            }
        }
        "player_loaded" => {
            player.lock().loaded = true;
            entities::update_visibility(server, player);
        }
        "client_information" => {
            let p = sb::PlayClientInformation::read(r)?;
            apply_client_info(server, player, &p.0);
        }
        "move_player_pos" => {
            let p = sb::MovePlayerPos::read(r)?;
            handle_move(server, player, Some((p.x, p.y, p.z)), None, p.on_ground);
        }
        "move_player_pos_rot" => {
            let p = sb::MovePlayerPosRot::read(r)?;
            handle_move(server, player, Some((p.x, p.y, p.z)), Some((p.yaw, p.pitch)), p.on_ground);
        }
        "move_player_rot" => {
            let p = sb::MovePlayerRot::read(r)?;
            handle_move(server, player, None, Some((p.yaw, p.pitch)), p.on_ground);
        }
        "move_player_status_only" => {
            let p = sb::MovePlayerStatusOnly::read(r)?;
            handle_move(server, player, None, None, p.on_ground);
        }
        "player_command" => {
            let p = sb::PlayerCommand::read(r)?;
            let mut s = player.lock();
            match p.action {
                sb::PlayerCommandAction::StartSprinting => s.sprinting = true,
                sb::PlayerCommandAction::StopSprinting => s.sprinting = false,
                _ => {}
            }
        }
        "player_input" => {
            let p = sb::PlayerInput::read(r)?;
            let changed = {
                let mut s = player.lock();
                let was = s.sneaking;
                s.sneaking = p.sneaking();
                was != s.sneaking
            };
            if changed {
                let sneaking = player.lock().sneaking;
                let chunk = player.lock().chunk();
                server.broadcast_near(chunk, &entities::pose_packet(player, sneaking), Some(player.uuid));
            }
        }
        "player_action" => {
            let p = sb::PlayerAction::read(r)?;
            handle_dig(server, player, p);
        }
        "use_item_on" => {
            let p = sb::UseItemOn::read(r)?;
            handle_use_item_on(server, player, p);
        }
        "use_item" => {
            let _ = sb::UseItem::read(r)?;
        }
        "punch" => {
            let chunk = player.lock().chunk();
            server.broadcast_near(
                chunk,
                &cb::SwingAnimation {
                    entity_id: player.entity_id,
                    off_hand: false,
                },
                Some(player.uuid),
            );
        }
        "set_carried_item" => {
            let p = sb::SetCarriedItem::read(r)?;
            if (0..9).contains(&p.slot) {
                player.lock().held_slot = p.slot as i32;
            }
        }
        "set_creative_mode_slot" => {
            let p = sb::SetCreativeModeSlot::read(r)?;
            crate::items::handle_creative_slot(player, p.slot, p.item);
        }
        "container_click" => {
            let p = sb::ContainerClick::read(r)?;
            crate::items::handle_click(server, player, &p);
        }
        "container_close" => {
            let _ = sb::ContainerClose::read(r)?;
            crate::items::sync_inventory(player);
        }
        "player_abilities" => {
            let p = sb::ServerboundPlayerAbilities::read(r)?;
            let allowed = matches!(player.lock().game_mode, GameMode::Creative | GameMode::Spectator);
            if p.flying && !allowed {
                // The client claims to fly in survival: put it back on the ground.
                player.send(&cb::PlayerAbilities::for_game_mode(GameMode::Survival));
                let v = Violation {
                    check: crate::anticheat::Check::Fly,
                    details: "enabled flying in survival".into(),
                };
                punish(server, player, &v);
            } else {
                player.lock().flying = p.flying;
            }
        }
        "custom_payload" => {
            let p = sb::ServerboundPluginMessage::read(r)?;
            if p.channel.namespace == "garnet" {
                let event = Event::PluginMessage {
                    uuid: player.uuid,
                    channel: p.channel.to_string(),
                    data: p.data,
                };
                server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
            }
        }
        "ping_request" => {
            let p = sb::PingRequest::read(r)?;
            player.send(&cb::PingResponse { payload: p.payload });
        }
        "client_command" => {
            let p = sb::ClientCommand::read(r)?;
            if p.action == 0 {
                respawn(server, player);
            }
        }
        "interact" | "attack" | "chat_ack" | "chat_session_update" | "client_tick_end" | "pong"
        | "configuration_acknowledged" | "cookie_response" => {}
        _ => {}
    }
    Ok(())
}

fn apply_client_info(server: &Arc<Server>, player: &Arc<Player>, info: &ClientInformation) {
    let max = server.config().server.view_distance;
    let mut s = player.lock();
    s.locale = info.locale.clone();
    s.view_distance = (info.view_distance as i32).clamp(2, max);
}

fn handle_chat(server: &Arc<Server>, player: &Arc<Player>, message: String) {
    let message: String = message.chars().filter(|c| !c.is_control()).take(256).collect();
    if message.trim().is_empty() {
        return;
    }
    let flood = server.anticheat.count_chat(&mut player.lock().checks);
    if let Some(v) = flood {
        punish(server, player, &v);
        return;
    }
    let event = Event::Chat {
        uuid: player.uuid,
        name: player.name().to_owned(),
        message: message.clone(),
    };
    let verdict = server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
    if verdict.cancel {
        return;
    }
    let message = verdict.message.unwrap_or(message);
    let prefix = server
        .permissions
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .prefix(&player.uuid.to_string())
        .map(|p| Text::legacy(p));
    let mut line = Text::empty();
    if let Some(prefix) = prefix {
        line = line.append(prefix).append(Text::new(" "));
    }
    line = line
        .append(Text::new(format!("<{}> ", player.name())))
        .append(Text::new(message));
    server.broadcast_chat(line);
}

fn handle_suggestions(server: &Arc<Server>, player: &Arc<Player>, request: sb::CommandSuggestionRequest) {
    // Only the command name itself is completed; arguments are free text.
    let text = request.text.trim_start_matches('/');
    if text.contains(' ') {
        player.send(&cb::CommandSuggestions {
            transaction_id: request.transaction_id,
            start: request.text.len() as i32,
            length: 0,
            matches: Vec::new(),
        });
        return;
    }
    let sender = CommandSender::Player(Arc::clone(player));
    let matches: Vec<(String, Option<Text>)> = server
        .commands
        .list()
        .into_iter()
        .filter(|c| c.name.starts_with(text))
        .filter(|c| sender.has_permission(server, &c.permission.clone().unwrap_or_else(|| format!("garnet.command.{}", c.name))))
        .map(|c| (c.name, Some(Text::new(c.description))))
        .collect();
    player.send(&cb::CommandSuggestions {
        transaction_id: request.transaction_id,
        start: 1,
        length: text.len() as i32,
        matches,
    });
}

fn handle_move(server: &Arc<Server>, player: &Arc<Player>, pos: Option<(f64, f64, f64)>, rot: Option<(f32, f32)>, on_ground: bool) {
    let (from, from_chunk, game_mode, flying, pending) = {
        let s = player.lock();
        ((s.x, s.y, s.z), s.chunk(), s.game_mode, s.flying, s.pending_teleport.is_some())
    };
    if pending {
        // Movement before the teleport confirmation refers to the old spot.
        return;
    }
    let to = pos.unwrap_or(from);
    if let Some((yaw, pitch)) = rot {
        if !yaw.is_finite() || !pitch.is_finite() {
            player.disconnect(Text::translate("multiplayer.disconnect.invalid_player_movement", vec![]));
            return;
        }
    }

    if pos.is_some() {
        let around = surroundings(server, to);
        let can_fly = matches!(game_mode, GameMode::Creative | GameMode::Spectator) && flying || game_mode == GameMode::Spectator;
        let verdict = server
            .anticheat
            .check_movement(&mut player.lock().checks, from, to, on_ground, can_fly, around);
        if let Some(v) = verdict {
            punish(server, player, &v);
            if player.is_connected() {
                // Rubber-band: put them back where the server last saw them.
                let (yaw, pitch) = rot.unwrap_or_else(|| {
                    let s = player.lock();
                    (s.yaw, s.pitch)
                });
                commands::teleport(player, from.0, from.1, from.2, yaw, pitch);
            }
            return;
        }
        let event = Event::PlayerMove {
            uuid: player.uuid,
            from: garnet_api::Location {
                x: from.0,
                y: from.1,
                z: from.2,
                yaw: 0.0,
                pitch: 0.0,
            },
            to: garnet_api::Location {
                x: to.0,
                y: to.1,
                z: to.2,
                yaw: rot.map(|r| r.0).unwrap_or(0.0),
                pitch: rot.map(|r| r.1).unwrap_or(0.0),
            },
        };
        let verdict = server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
        if verdict.cancel {
            let (yaw, pitch) = {
                let s = player.lock();
                (s.yaw, s.pitch)
            };
            commands::teleport(player, from.0, from.1, from.2, yaw, pitch);
            return;
        }
    }

    let to_chunk = {
        let mut s = player.lock();
        s.x = to.0;
        s.y = to.1;
        s.z = to.2;
        if let Some((yaw, pitch)) = rot {
            s.yaw = yaw;
            s.pitch = pitch;
        }
        s.on_ground = on_ground;
        s.chunk()
    };
    if to_chunk != from_chunk {
        server.move_player_chunk(player.uuid, from_chunk, to_chunk);
    }
    if pos.is_some() || rot.is_some() {
        entities::broadcast_movement(server, player, from, rot.is_some());
    }
}

/// Liquids and ladders explain vertical movement the fly check would flag.
fn surroundings(server: &Server, at: (f64, f64, f64)) -> Surroundings {
    let feet = BlockPos::new(at.0.floor() as i32, at.1.floor() as i32, at.2.floor() as i32);
    let world = server.world();
    let names: Vec<String> = [feet, feet.offset(0, 1, 0), feet.offset(0, -1, 0)]
        .iter()
        .filter_map(|p| world.chunk(p.chunk()).and_then(|c| c.get_block(p.x, p.y, p.z)))
        .filter_map(|s| server.data.blocks.block_of_state(s as i32).map(|b| b.name.clone()))
        .collect();
    let liquid = names.iter().any(|n| n.ends_with(":water") || n.ends_with(":lava") || n.ends_with("kelp") || n.ends_with("seagrass"));
    let climbable = names.iter().any(|n| n.ends_with(":ladder") || n.ends_with("vine") || n.ends_with(":scaffolding") || n.contains("weeping_vines") || n.contains("twisting_vines"));
    Surroundings {
        in_liquid: liquid,
        on_climbable: climbable,
    }
}

fn punish(server: &Arc<Server>, player: &Arc<Player>, violation: &Violation) {
    if server.has_permission(player.uuid, "garnet.anticheat.bypass") {
        return;
    }
    let escalation = server
        .anticheat
        .flag(player.name(), &mut player.lock().checks, violation);
    match escalation {
        Escalation::None => {}
        Escalation::Kick => {
            server.audit.record("anticheat", "kick", format!("{} ({})", player.name(), violation.check.name()));
            player.disconnect(Text::new(format!("Kicked by anti-cheat: {}", violation.check.name())));
        }
        Escalation::Ban => {
            let hours = server.config().anticheat.ban_hours;
            let _ = server.lists.ban(
                player.uuid,
                player.name(),
                &format!("Anti-cheat: {}", violation.check.name()),
                "anticheat",
                Some(hours),
            );
            server.audit.record("anticheat", "ban", format!("{} ({}) for {hours}h", player.name(), violation.check.name()));
            player.disconnect(Text::new(format!("Banned by anti-cheat: {}", violation.check.name())));
        }
    }
}

fn protected(server: &Server, player: &Player, pos: BlockPos) -> bool {
    let radius = server.config().server.spawn_protection;
    if radius <= 0 || server.is_op(player.uuid) {
        return false;
    }
    let spawn = server.world().settings.spawn;
    (pos.x - spawn.x).abs() <= radius && (pos.z - spawn.z).abs() <= radius
}

fn handle_dig(server: &Arc<Server>, player: &Arc<Player>, action: sb::PlayerAction) {
    use sb::DigStatus::*;
    let game_mode = player.lock().game_mode;
    match action.status {
        SwapItemWithOffhand => {
            crate::items::swap_hands(player);
            return;
        }
        DropItem | DropItemStack => {
            // Item entities are not here yet: the item stays in the inventory.
            crate::items::sync_inventory(player);
            return;
        }
        _ => {}
    }
    let breaks = match (action.status, game_mode) {
        (StartDigging, GameMode::Creative) => true,
        (FinishDigging, GameMode::Survival | GameMode::Adventure) => game_mode == GameMode::Survival,
        _ => false,
    };
    let done = || player.send(&cb::AcknowledgeBlockChange { sequence: action.sequence });
    if !breaks {
        done();
        return;
    }
    let eyes = player.lock().eye_position();
    if let Some(v) = server.anticheat.check_reach(eyes, (action.position.x, action.position.y, action.position.z), game_mode == GameMode::Creative) {
        punish(server, player, &v);
        restore_block(server, player, action.position);
        done();
        return;
    }
    if game_mode == GameMode::Spectator || protected(server, player, action.position) {
        restore_block(server, player, action.position);
        done();
        return;
    }
    let current = server.world().get_block(action.position).unwrap_or(0);
    let block_name = server
        .data
        .blocks
        .block_of_state(current as i32)
        .map(|b| b.name.clone())
        .unwrap_or_default();
    if server.data.blocks.is_air(current as i32) || block_name.ends_with(":bedrock") && game_mode != GameMode::Creative {
        restore_block(server, player, action.position);
        done();
        return;
    }
    let event = Event::BlockBreak {
        uuid: player.uuid,
        pos: garnet_api::Pos {
            x: action.position.x,
            y: action.position.y,
            z: action.position.z,
        },
        block: block_name,
    };
    let verdict = server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
    if verdict.cancel {
        restore_block(server, player, action.position);
    } else {
        let air = server.data.blocks.default_state("air").unwrap_or(0) as u32;
        server.set_block(action.position, air);
        crate::items::collect_drops(server, player, current);
    }
    done();
}

/// Tells one client what a block really is (undoing its prediction).
fn restore_block(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos) {
    let state = server.world().get_block(pos).unwrap_or(0);
    player.send(&cb::BlockUpdate {
        position: pos,
        state_id: state as i32,
    });
}

fn handle_use_item_on(server: &Arc<Server>, player: &Arc<Player>, use_on: sb::UseItemOn) {
    let target = use_on.position;
    let clicked = server.world().get_block(target).unwrap_or(0);
    let block_name = server
        .data
        .blocks
        .block_of_state(clicked as i32)
        .map(|b| b.name.clone())
        .unwrap_or_default();
    let event = Event::BlockInteract {
        uuid: player.uuid,
        pos: garnet_api::Pos {
            x: target.x,
            y: target.y,
            z: target.z,
        },
        block: block_name,
        face: use_on.face as u8,
    };
    server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);

    let eyes = player.lock().eye_position();
    let creative = player.lock().game_mode == GameMode::Creative;
    let too_far = server.anticheat.check_reach(eyes, (target.x, target.y, target.z), creative).is_some();
    let placed = !too_far
        && !protected(server, player, target)
        && crate::items::place_held(server, player, target, use_on.face, (use_on.cursor_x, use_on.cursor_y, use_on.cursor_z), use_on.hand);
    if !placed {
        // Undo the client's prediction so it never desyncs.
        if let Some(dir) = garnet_protocol::Direction::from_id(use_on.face) {
            let (dx, dy, dz) = dir.offset();
            restore_block(server, player, target.offset(dx, dy, dz));
        }
        restore_block(server, player, target);
    }
    player.send(&cb::AcknowledgeBlockChange {
        sequence: use_on.sequence,
    });
}

fn respawn(server: &Arc<Server>, player: &Arc<Player>) {
    let (spawn, dimension_type, seed, sea_level, flat) = {
        let world = server.world();
        (
            world.settings.spawn,
            server.data.dynamic.id_of("dimension_type", "overworld").unwrap_or(0),
            world.settings.seed,
            garnet_world::generator::SEA_LEVEL,
            server.config().world.generator == "flat",
        )
    };
    let (game_mode, spawn) = {
        let mut s = player.lock();
        s.health = 20.0;
        s.food = 20;
        (s.game_mode, s.spawn_point.unwrap_or(spawn))
    };
    player.send(&cb::Respawn {
        spawn: cb::SpawnInfo {
            dimension_type,
            dimension_name: Identifier::minecraft("overworld"),
            hashed_seed: hashed_seed(seed),
            game_mode,
            previous_game_mode: Some(game_mode),
            is_debug: false,
            is_flat: flat,
            death_location: None,
            portal_cooldown: 0,
            sea_level,
        },
        data_kept: 0,
    });
    commands::teleport(player, spawn.x as f64 + 0.5, spawn.y as f64, spawn.z as f64 + 0.5, 0.0, 0.0);
    player.send(&cb::SetHealth {
        health: 20.0,
        food: 20,
        saturation: 5.0,
    });
}
