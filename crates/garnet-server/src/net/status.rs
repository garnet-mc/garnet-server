//! Server list ping.

use crate::net::connection::Connection;
use garnet_protocol::packets::status::*;
use garnet_protocol::{PacketReader, Text};

pub async fn handle(conn: &mut Connection, name: &str, reader: &mut PacketReader<'_>) -> anyhow::Result<()> {
    match name {
        "status_request" => {
            let json = build_status_json(conn);
            conn.send(&StatusResponse { json }).await
        }
        "ping_request" => {
            let ping = PingRequest::read(reader)?;
            conn.send(&PongResponse { payload: ping.payload }).await
        }
        _ => Ok(()),
    }
}

fn build_status_json(conn: &Connection) -> String {
    let server = &conn.server;
    let config = server.config();
    let players = server.online_players();
    let sample: Vec<StatusPlayerSample> = players
        .iter()
        .take(12)
        .map(|p| StatusPlayerSample {
            name: p.name().to_owned(),
            id: p.uuid.to_string(),
        })
        .collect();
    let status = StatusJson {
        version: StatusVersion {
            name: format!("Garnet {}", server.data.version),
            protocol: server.data.protocol_version,
        },
        players: StatusPlayers {
            max: config.server.max_players as i32,
            online: players.len() as i32,
            sample,
        },
        description: Text::legacy(&config.server.motd).to_json(),
        favicon: load_favicon(&server.root),
        enforces_secure_chat: false,
    };
    // Vanilla clients ignore unknown keys; the Garnet launcher reads this
    // block to install the server's client mods before joining.
    let mut json = serde_json::to_value(&status).unwrap_or_default();
    if let Some(obj) = json.as_object_mut() {
        obj.insert("garnet".into(), crate::client_mods::manifest(&config, &server.data.version));
    }
    json.to_string()
}

/// The Garnet mark, shown in the server list unless the owner drops their
/// own `server-icon.png` next to the config.
const DEFAULT_ICON: &[u8] = include_bytes!("server-icon.png");

/// `server-icon.png` next to the config (or the Garnet icon), as a data URL.
/// Read on every ping; it is tiny and this keeps changes instant.
fn load_favicon(root: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(root.join("server-icon.png")).unwrap_or_else(|_| DEFAULT_ICON.to_vec());
    Some(format!("data:image/png;base64,{}", base64_encode(&bytes)))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

use garnet_protocol::ServerboundPacket;
