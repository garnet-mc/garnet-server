//! Login: who is this, are they allowed in, and (online mode) proving it to
//! Mojang. Also proxy forwarding for BungeeCord and Velocity.

use crate::net::connection::Connection;
use garnet_protocol::packets::login::*;
use garnet_protocol::{GameProfile, Identifier, PacketReader, ProfileProperty, ServerboundPacket, State, Text};
use hmac::{Hmac, KeyInit, Mac};
use num_bigint::BigInt;
use rsa::Pkcs1v15Encrypt;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::sync::Arc;
use uuid::Uuid;

const VELOCITY_CHANNEL: &str = "velocity:player_info";
const VELOCITY_MAX_VERSION: i32 = 1;

pub async fn handle(conn: &mut Connection, name: &str, reader: &mut PacketReader<'_>) -> anyhow::Result<()> {
    match name {
        "hello" => {
            let start = LoginStart::read(reader)?;
            handle_login_start(conn, start).await
        }
        "key" => {
            let response = EncryptionResponse::read(reader)?;
            handle_encryption_response(conn, response).await
        }
        "custom_query_answer" => {
            let response = LoginPluginResponse::read(reader)?;
            handle_velocity_response(conn, response).await
        }
        "login_acknowledged" => {
            conn.state = State::Config;
            crate::net::config_phase::begin(conn).await
        }
        _ => Ok(()),
    }
}

fn valid_name(name: &str) -> bool {
    (3..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

async fn handle_login_start(conn: &mut Connection, start: LoginStart) -> anyhow::Result<()> {
    if conn.protocol_version != conn.server.data.protocol_version {
        let wanted = conn.server.data.version.clone();
        let text = if conn.protocol_version < conn.server.data.protocol_version {
            format!("Outdated client! Please use {wanted}")
        } else {
            format!("Outdated server! I'm still on {wanted}")
        };
        conn.kick(Text::new(text)).await;
        anyhow::bail!("protocol {} != {}", conn.protocol_version, conn.server.data.protocol_version);
    }
    if !valid_name(&start.name) {
        conn.kick(Text::new("Invalid username")).await;
        anyhow::bail!("invalid username {:?}", start.name);
    }
    conn.claimed_name = start.name.clone();

    let (online_mode, proxy) = {
        let c = conn.server.config();
        (c.server.online_mode, c.server.proxy.clone())
    };

    // Proxies pass the real address (and identity) in the handshake or a
    // plugin message; without one, whatever connected to us is the player.
    if proxy == "bungeecord" {
        match parse_bungeecord(conn.handshake.as_ref().map(|h| h.server_address.as_str()).unwrap_or("")) {
            Some((ip, profile)) => {
                conn.ip = ip;
                conn.proxied_profile = Some(profile);
            }
            None => {
                conn.kick(Text::new("This server only accepts connections through the proxy.")).await;
                anyhow::bail!("bungeecord forwarding data missing");
            }
        }
    }

    if let Some(ban) = conn.server.lists.ip_ban_for(&conn.ip) {
        conn.kick(Text::new(format!("Your IP address is banned from this server.\nReason: {}", ban.reason))).await;
        anyhow::bail!("ip banned");
    }

    if online_mode {
        let request = EncryptionRequest {
            server_id: String::new(),
            public_key: conn.server.keys.public_der.clone(),
            verify_token: conn.verify_token.to_vec(),
            should_authenticate: true,
        };
        conn.send(&request).await
    } else if proxy == "velocity" {
        conn.velocity_query_id = Some(0);
        conn.send(&LoginPluginRequest {
            message_id: 0,
            channel: Identifier::parse(VELOCITY_CHANNEL).unwrap(),
            data: vec![VELOCITY_MAX_VERSION as u8],
        })
        .await
    } else {
        let profile = conn
            .proxied_profile
            .take()
            .unwrap_or_else(|| GameProfile::offline(&start.name));
        finish_login(conn, profile).await
    }
}

/// BungeeCord puts `hostname\0ip\0uuid\0properties-json` in the handshake.
fn parse_bungeecord(address: &str) -> Option<(String, GameProfile)> {
    let parts: Vec<&str> = address.split('\0').collect();
    if parts.len() < 3 {
        return None;
    }
    let ip = parts[1].to_owned();
    let id = Uuid::parse_str(parts[2]).ok()?;
    let properties: Vec<ProfileProperty> = parts
        .get(3)
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    Some((
        ip,
        GameProfile {
            id,
            name: String::new(), // filled from Login Start
            properties,
        },
    ))
}

async fn handle_velocity_response(conn: &mut Connection, response: LoginPluginResponse) -> anyhow::Result<()> {
    if conn.velocity_query_id != Some(response.message_id) {
        return Ok(());
    }
    let Some(data) = response.data else {
        conn.kick(Text::new("This server only accepts connections through the proxy.")).await;
        anyhow::bail!("velocity forwarding not supported by client");
    };
    let secret = conn.server.config().server.velocity_secret.clone();
    match parse_velocity(&data, &secret) {
        Ok((ip, profile)) => {
            conn.ip = ip;
            finish_login(conn, profile).await
        }
        Err(err) => {
            conn.kick(Text::new("Unable to verify player details")).await;
            anyhow::bail!("velocity forwarding rejected: {err}");
        }
    }
}

/// Velocity modern forwarding: `HMAC-SHA256(secret, rest)` followed by
/// version, address, uuid, name and profile properties.
fn parse_velocity(data: &[u8], secret: &str) -> anyhow::Result<(String, GameProfile)> {
    if data.len() < 33 {
        anyhow::bail!("payload too short");
    }
    let (signature, rest) = data.split_at(32);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
    mac.update(rest);
    mac.verify_slice(signature).map_err(|_| anyhow::anyhow!("bad signature (wrong velocity_secret?)"))?;

    let mut r = PacketReader::new(rest);
    let _version = r.read_varint()?;
    let address = r.read_string()?;
    let id = r.read_uuid()?;
    let name = r.read_string_max(16)?;
    let properties = r.read_list(|r| {
        Ok(ProfileProperty {
            name: r.read_string()?,
            value: r.read_string()?,
            signature: r.read_option(|r| r.read_string())?,
        })
    })?;
    Ok((address, GameProfile { id, name, properties }))
}

async fn handle_encryption_response(conn: &mut Connection, response: EncryptionResponse) -> anyhow::Result<()> {
    let private = &conn.server.keys.private;
    let secret = private
        .decrypt(Pkcs1v15Encrypt, &response.shared_secret)
        .map_err(|_| anyhow::anyhow!("could not decrypt shared secret"))?;
    let token = private
        .decrypt(Pkcs1v15Encrypt, &response.verify_token)
        .map_err(|_| anyhow::anyhow!("could not decrypt verify token"))?;
    if token != conn.verify_token {
        conn.kick(Text::new("Invalid verify token")).await;
        anyhow::bail!("verify token mismatch");
    }
    let secret: [u8; 16] = secret
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("shared secret is not 16 bytes"))?;
    conn.codec.enable_encryption(&secret);

    let hash = server_hash(&secret, &conn.server.keys.public_der);
    let name = conn.claimed_name.clone();
    match authenticate(&conn.server.http, &name, &hash, &conn.ip).await {
        Ok(Some(profile)) => finish_login(conn, profile).await,
        Ok(None) => {
            conn.kick(Text::new("Failed to verify username!")).await;
            anyhow::bail!("session server rejected {name}");
        }
        Err(err) => {
            conn.kick(Text::new("Authentication servers are down. Please try again later.")).await;
            anyhow::bail!("session server error: {err}");
        }
    }
}

/// Minecraft's odd "signed hex" SHA-1 of the shared secret and public key.
fn server_hash(secret: &[u8], public_key: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(secret);
    hasher.update(public_key);
    let digest = hasher.finalize();
    BigInt::from_signed_bytes_be(&digest).to_str_radix(16)
}

#[derive(serde::Deserialize)]
struct SessionProfile {
    id: String,
    name: String,
    #[serde(default)]
    properties: Vec<ProfileProperty>,
}

async fn authenticate(http: &reqwest::Client, name: &str, hash: &str, ip: &str) -> anyhow::Result<Option<GameProfile>> {
    let mut url = format!("https://sessionserver.mojang.com/session/minecraft/hasJoined?username={name}&serverId={hash}");
    if let Ok(parsed) = ip.parse::<std::net::IpAddr>() {
        if !parsed.is_loopback() {
            url.push_str(&format!("&ip={parsed}"));
        }
    }
    let response = http.get(&url).send().await?;
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    let session: SessionProfile = response.error_for_status()?.json().await?;
    let id = Uuid::parse_str(&session.id)?;
    Ok(Some(GameProfile {
        id,
        name: session.name,
        properties: session.properties,
    }))
}

/// Everything that happens once we know who the player is.
async fn finish_login(conn: &mut Connection, mut profile: GameProfile) -> anyhow::Result<()> {
    if profile.name.is_empty() {
        profile.name = conn.claimed_name.clone();
    }
    let server = Arc::clone(&conn.server);

    if let Some(ban) = server.lists.ban_for(profile.id, &profile.name) {
        let until = if ban.expires == "forever" { String::new() } else { format!("\nUntil: {}", ban.expires) };
        conn.kick(Text::new(format!("You are banned from this server.\nReason: {}{until}", ban.reason))).await;
        anyhow::bail!("banned");
    }
    let (whitelist_on, max_players) = {
        let c = server.config();
        (c.server.whitelist, c.server.max_players)
    };
    if whitelist_on && !server.lists.is_whitelisted(profile.id, &profile.name) && !server.is_op(profile.id) {
        conn.kick(Text::new("You are not white-listed on this server!")).await;
        anyhow::bail!("not whitelisted");
    }
    if server.online_count() >= max_players && !server.is_op(profile.id) {
        conn.kick(Text::new("The server is full!")).await;
        anyhow::bail!("server full");
    }

    // Mods get a say (for example a maintenance mode or extra whitelist).
    let event = garnet_api::Event::PlayerLogin {
        uuid: profile.id,
        name: profile.name.clone(),
        ip: conn.ip.clone(),
    };
    let verdict = server.mods.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&event);
    if verdict.cancel {
        conn.kick(Text::legacy(&verdict.message.unwrap_or_else(|| "You are not allowed to join.".into()))).await;
        anyhow::bail!("login cancelled by a mod");
    }

    // A second login with the same account replaces the first.
    if let Some(existing) = server.player(profile.id) {
        existing.disconnect(Text::new("You logged in from another location"));
        // Give the old connection a moment to leave so the join is clean.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let threshold = server.config().server.compression_threshold;
    if threshold >= 0 {
        conn.send(&SetCompression { threshold }).await?;
        conn.codec.enable_compression(threshold as usize);
    }

    conn.send(&LoginSuccess {
        uuid: profile.id,
        username: profile.name.clone(),
        properties: profile.properties.clone(),
        session_id: Uuid::new_v4(),
    })
    .await?;
    tracing::info!("{} [{}] logged in as {}", profile.name, conn.ip, profile.id);
    conn.profile = Some(profile);
    Ok(())
}

pub fn brand_payload(brand: &str) -> Vec<u8> {
    let mut w = garnet_protocol::PacketWriter::new();
    w.write_string(brand);
    w.into_inner()
}
