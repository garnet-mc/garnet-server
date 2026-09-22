//! RCON: the classic remote console protocol, so existing hosting panels
//! and tools can run commands.
//!
//! Every packet is `length i32 LE, request id i32 LE, type i32 LE, payload,
//! two NUL bytes`. Type 3 logs in, type 2 runs a command, type 0 is our
//! reply. A failed login is answered with request id -1.

use crate::commands::CommandSender;
use crate::server::Server;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const LOGIN: i32 = 3;
const COMMAND: i32 = 2;
const RESPONSE: i32 = 0;

pub async fn start(server: Arc<Server>) -> anyhow::Result<()> {
    let (bind, port, password) = {
        let c = server.config();
        (c.rcon.bind.clone(), c.rcon.port, c.rcon.password.clone())
    };
    let listener = TcpListener::bind((bind.as_str(), port)).await?;
    tracing::info!("RCON listening on {bind}:{port}");
    tokio::spawn(async move {
        loop {
            let Ok((socket, addr)) = listener.accept().await else { continue };
            let server = Arc::clone(&server);
            let password = password.clone();
            tokio::spawn(async move {
                if let Err(err) = handle(server, socket, &password).await {
                    tracing::debug!("rcon {addr}: {err}");
                }
            });
        }
    });
    Ok(())
}

async fn read_packet(socket: &mut TcpStream) -> anyhow::Result<(i32, i32, String)> {
    let length = socket.read_i32_le().await?;
    if !(10..=4096).contains(&length) {
        anyhow::bail!("bad rcon packet length {length}");
    }
    let id = socket.read_i32_le().await?;
    let kind = socket.read_i32_le().await?;
    let mut payload = vec![0u8; length as usize - 10];
    socket.read_exact(&mut payload).await?;
    let mut pad = [0u8; 2];
    socket.read_exact(&mut pad).await?;
    Ok((id, kind, String::from_utf8_lossy(&payload).into_owned()))
}

async fn write_packet(socket: &mut TcpStream, id: i32, kind: i32, payload: &str) -> anyhow::Result<()> {
    let bytes = payload.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + 14);
    out.extend_from_slice(&((bytes.len() + 10) as i32).to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(&[0, 0]);
    socket.write_all(&out).await?;
    Ok(())
}

async fn handle(server: Arc<Server>, mut socket: TcpStream, password: &str) -> anyhow::Result<()> {
    let mut authenticated = false;
    loop {
        let (id, kind, payload) = read_packet(&mut socket).await?;
        match kind {
            LOGIN => {
                authenticated = payload == password;
                write_packet(&mut socket, if authenticated { id } else { -1 }, COMMAND, "").await?;
                if !authenticated {
                    tracing::warn!("rcon: failed login from {}", socket.peer_addr()?);
                    return Ok(());
                }
            }
            COMMAND if authenticated => {
                let replies = Arc::new(Mutex::new(Vec::new()));
                let sender = CommandSender::Rcon {
                    replies: Arc::clone(&replies),
                };
                server.commands.execute(&server, &sender, &payload);
                let text = replies.lock().unwrap_or_else(|e| e.into_inner()).join("\n");
                write_packet(&mut socket, id, RESPONSE, &text).await?;
            }
            _ => {
                write_packet(&mut socket, -1, RESPONSE, "not authenticated").await?;
                return Ok(());
            }
        }
    }
}
