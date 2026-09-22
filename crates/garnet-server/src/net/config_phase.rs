//! Configuration state: hand the client the registries and tags it needs,
//! then move to play.

use crate::net::connection::Connection;
use crate::player::{Outbound, Player};
use garnet_protocol::packets::config::*;
use garnet_protocol::{Identifier, PacketReader, ServerboundPacket, Text};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Sent right after the client acknowledges login.
pub async fn begin(conn: &mut Connection) -> anyhow::Result<()> {
    conn.send(&ClientboundPluginMessage {
        channel: Identifier::minecraft("brand"),
        data: crate::net::login::brand_payload("garnet"),
    })
    .await?;
    // The client mod manifest goes out before anything else so a Garnet
    // client can answer while the registries are still being sent.
    let manifest = {
        let config = conn.server.config();
        crate::client_mods::manifest(&config, &conn.server.data.version)
    };
    conn.send(&ClientboundPluginMessage {
        channel: Identifier::parse(crate::client_mods::CHANNEL).unwrap(),
        data: manifest.to_string().into_bytes(),
    })
    .await?;
    conn.send(&FeatureFlags {
        features: vec![Identifier::minecraft("vanilla")],
    })
    .await?;
    // We always send full registry data, so we do not need the client to
    // know any packs; an empty list keeps the exchange simple and version-proof.
    conn.send(&ClientboundKnownPacks { packs: Vec::new() }).await
}

pub async fn handle(conn: &mut Connection, name: &str, reader: &mut PacketReader<'_>) -> anyhow::Result<()> {
    match name {
        "client_information" => {
            conn.client_info = Some(ClientInformation::read(reader)?);
            Ok(())
        }
        "select_known_packs" => {
            let _ = ServerboundKnownPacks::read(reader)?;
            send_registries(conn).await
        }
        "accept_code_of_conduct" => conn.send(&FinishConfiguration).await,
        "finish_configuration" => enter_play(conn).await,
        "custom_payload" => {
            let p = ServerboundPluginMessage::read(reader)?;
            if p.channel.to_string() == crate::client_mods::CHANNEL {
                match serde_json::from_slice(&p.data) {
                    Ok(report) => conn.mod_report = Some(report),
                    Err(err) => tracing::debug!("{}: bad garnet:mods report: {err}", conn.claimed_name),
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

async fn send_registries(conn: &mut Connection) -> anyhow::Result<()> {
    let data = Arc::clone(&conn.server.data);
    for (registry, entries) in data.registry_packets() {
        conn.send(&RegistryData { registry, entries }).await?;
    }
    conn.send(&UpdateTags {
        registries: data.tag_packets(),
    })
    .await?;
    let code_of_conduct = conn.server.config().server.code_of_conduct.clone();
    if !code_of_conduct.is_empty() {
        // The client answers with accept_code_of_conduct, which finishes.
        return conn.send(&CodeOfConduct { text: code_of_conduct }).await;
    }
    conn.send(&FinishConfiguration).await
}

/// The client acknowledged the end of configuration: create the player and
/// start the play state.
async fn enter_play(conn: &mut Connection) -> anyhow::Result<()> {
    let server = Arc::clone(&conn.server);
    let Some(profile) = conn.profile.clone() else {
        conn.kick(Text::new("Login did not complete")).await;
        anyhow::bail!("no profile at end of configuration");
    };

    // Required client mods: kick with a helpful message when enforced.
    let missing_mods = {
        let config = server.config();
        if config.client_mods.enforce {
            let missing = crate::client_mods::missing_required(&config, conn.mod_report.as_ref());
            (!missing.is_empty()).then(|| (missing, config.client_mods.launcher_url.clone()))
        } else {
            None
        }
    };
    if let Some((missing, launcher_url)) = missing_mods {
        let text = format!(
            "This server needs these client mods:
{}

Join with the Garnet launcher and they install automatically:
{launcher_url}",
            missing.join(", ")
        );
        conn.kick(Text::new(text)).await;
        anyhow::bail!("missing required client mods");
    }

    let saved = crate::playerdata::load(&server, profile.id);
    let (spawn, game_mode, view_distance) = {
        let config = server.config();
        let world = server.world();
        let spawn = world.settings.spawn;
        let spawn_pos = saved
            .as_ref()
            .map(|s| s.position)
            .unwrap_or((spawn.x as f64 + 0.5, spawn.y as f64, spawn.z as f64 + 0.5));
        let mode = saved
            .as_ref()
            .and_then(|s| s.game_mode)
            .unwrap_or_else(|| server.default_game_mode());
        let client_view = conn.client_info.as_ref().map(|c| c.view_distance as i32).unwrap_or(config.server.view_distance);
        (spawn_pos, mode, client_view.clamp(2, config.server.view_distance))
    };

    let (tx, rx) = mpsc::unbounded_channel::<Outbound>();
    let player = Arc::new(Player::new(
        profile,
        server.allocate_entity_id(),
        conn.ip.clone(),
        Arc::clone(&server.data),
        tx,
        spawn,
        game_mode,
        view_distance,
    ));
    if let Some(saved) = saved {
        let now = server.current_tick();
        let mut state = player.lock();
        state.yaw = saved.yaw;
        state.pitch = saved.pitch;
        state.health = saved.health;
        state.food = saved.food;
        state.saturation = saved.saturation;
        state.exhaustion = saved.exhaustion;
        state.air = saved.air;
        state.fire_ticks = saved.fire_ticks;
        state.xp_level = saved.xp_level;
        state.xp_progress = saved.xp_progress;
        state.xp_total = saved.xp_total;
        state.spawn_point = saved.spawn_point;
        state.tags = saved.tags.into_iter().collect();
        // Saved durations are ticks left; turn them back into end ticks.
        state.effects = saved
            .effects
            .into_iter()
            .map(|mut e| {
                e.expires_tick = e.expires_tick.map(|left| now + left);
                e
            })
            .collect();
        state.attributes = saved.attributes.into_iter().collect();
        for (slot, name, count, patch) in saved.inventory {
            if let Some(item) = crate::items::item_id(&server, &name) {
                state.inventory.set(
                    slot,
                    garnet_protocol::packets::play::items::ItemStack {
                        item,
                        count,
                        patch,
                    },
                );
            }
        }
    }
    if let Some(info) = &conn.client_info {
        player.lock().locale = info.locale.clone();
    }

    server.add_player(Arc::clone(&player));
    conn.enter_play(Arc::clone(&player), rx);
    crate::net::play::on_join(&server, &player).await;
    Ok(())
}
