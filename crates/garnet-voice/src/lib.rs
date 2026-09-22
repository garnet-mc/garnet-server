//! Proximity voice chat relay.
//!
//! Voice runs over UDP next to the game connection. The game server hands
//! each player a random secret when they join (over the `garnet:voice`
//! plugin channel); the client mod uses it to authenticate its UDP endpoint.
//! After that the client streams Opus frames and the relay forwards each
//! frame to every authenticated listener in the same dimension within range,
//! tagged with the speaker's position.
//!
//! The relay deliberately does no audio processing: 3D panning, distance
//! attenuation, occlusion (blocks between speaker and listener) and cave
//! reverb all happen in the client mod, which already has the world loaded.
//! That keeps the server side a cheap routing job that scales to thousands
//! of players.
//!
//! Packet layout (all integers big-endian, first byte is the packet type):
//!
//! ```text
//! client -> server
//!   0x01 auth       uuid[16] secret[16]
//!   0x02 audio      seq u32, flags u8 (bit0 = whisper), len u16, opus[len]
//!   0x03 ping       nonce u64
//!   0x04 leave
//! server -> client
//!   0x81 auth ok    range f32
//!   0x82 audio      speaker uuid[16], seq u32, flags u8, x f32, y f32, z f32, len u16, opus[len]
//!   0x83 pong       nonce u64
//!   0x84 gone       speaker uuid[16]
//!   0x8F error      reason string (u16 len + utf8)
//! ```

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u8 = 1;

const AUTH: u8 = 0x01;
const AUDIO: u8 = 0x02;
const PING: u8 = 0x03;
const LEAVE: u8 = 0x04;
const AUTH_OK: u8 = 0x81;
const AUDIO_OUT: u8 = 0x82;
const PONG: u8 = 0x83;
const GONE: u8 = 0x84;
const ERROR: u8 = 0x8F;

/// Largest datagram we accept; Opus frames are far smaller.
const MAX_DATAGRAM: usize = 1500;
/// Listeners farther than this are not sent the frame at all.
const DEFAULT_RANGE: f32 = 48.0;
/// Whispering halves the range.
const WHISPER_FACTOR: f32 = 0.5;
/// Sessions silent for this long are dropped.
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
/// Spatial grid cell size in blocks; should be >= the maximum range.
const CELL: f32 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub dimension: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

struct Session {
    addr: SocketAddr,
    last_seen: Instant,
    position: Position,
}

#[derive(Default)]
struct State {
    /// Secrets handed out by the game server, waiting for the UDP auth.
    pending: HashMap<Uuid, [u8; 16]>,
    sessions: HashMap<Uuid, Session>,
    by_addr: HashMap<SocketAddr, Uuid>,
    /// Spatial index: (dimension, cell x, cell z) -> players in that cell.
    grid: HashMap<(u32, i32, i32), HashSet<Uuid>>,
    /// Players who muted themselves or were muted by an admin.
    muted: HashSet<Uuid>,
}

impl State {
    fn cell_of(p: Position) -> (u32, i32, i32) {
        (p.dimension, (p.x / CELL).floor() as i32, (p.z / CELL).floor() as i32)
    }

    fn set_position(&mut self, uuid: Uuid, position: Position) {
        if let Some(session) = self.sessions.get_mut(&uuid) {
            let old = Self::cell_of(session.position);
            let new = Self::cell_of(position);
            session.position = position;
            if old != new {
                if let Some(set) = self.grid.get_mut(&old) {
                    set.remove(&uuid);
                    if set.is_empty() {
                        self.grid.remove(&old);
                    }
                }
                self.grid.entry(new).or_default().insert(uuid);
            }
        }
    }

    fn remove(&mut self, uuid: Uuid) -> Option<Session> {
        let session = self.sessions.remove(&uuid)?;
        self.by_addr.remove(&session.addr);
        let cell = Self::cell_of(session.position);
        if let Some(set) = self.grid.get_mut(&cell) {
            set.remove(&uuid);
            if set.is_empty() {
                self.grid.remove(&cell);
            }
        }
        Some(session)
    }

    /// Everyone (except the speaker) within `range` of `from`.
    fn listeners(&self, speaker: Uuid, from: Position, range: f32) -> Vec<SocketAddr> {
        let (dim, cx, cz) = Self::cell_of(from);
        let reach = (range / CELL).ceil() as i32;
        let mut out = Vec::new();
        for dx in -reach..=reach {
            for dz in -reach..=reach {
                let Some(cell) = self.grid.get(&(dim, cx + dx, cz + dz)) else { continue };
                for uuid in cell {
                    if *uuid == speaker {
                        continue;
                    }
                    let Some(s) = self.sessions.get(uuid) else { continue };
                    let (ddx, ddy, ddz) = (s.position.x - from.x, s.position.y - from.y, s.position.z - from.z);
                    if ddx * ddx + ddy * ddy + ddz * ddz <= range * range {
                        out.push(s.addr);
                    }
                }
            }
        }
        out
    }
}

/// Handle the game server keeps: hands out secrets and pushes positions.
#[derive(Clone)]
pub struct VoiceServer {
    state: Arc<Mutex<State>>,
    socket: Arc<UdpSocket>,
    range: f32,
    pub port: u16,
}

impl VoiceServer {
    pub async fn bind(bind: &str, port: u16, range: Option<f32>) -> std::io::Result<Self> {
        let socket = UdpSocket::bind((bind, port)).await?;
        let port = socket.local_addr()?.port();
        let server = Self {
            state: Arc::new(Mutex::new(State::default())),
            socket: Arc::new(socket),
            range: range.unwrap_or(DEFAULT_RANGE),
            port,
        };
        tokio::spawn(server.clone().receive_loop());
        tokio::spawn(server.clone().reaper_loop());
        tracing::info!("voice chat listening on {bind}:{port}/udp (range {} blocks)", server.range);
        Ok(server)
    }

    /// Creates a fresh secret for a joining player. Send it to the client
    /// over the `garnet:voice` plugin channel together with `port`.
    pub fn register(&self, uuid: Uuid) -> [u8; 16] {
        let secret: [u8; 16] = rand::random();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.remove(uuid);
        state.pending.insert(uuid, secret);
        secret
    }

    pub fn unregister(&self, uuid: Uuid) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.remove(&uuid);
        if state.remove(uuid).is_some() {
            // Tell everyone nearby the speaker is gone so clients stop their streams.
            let mut msg = vec![GONE];
            msg.extend_from_slice(uuid.as_bytes());
            let targets: Vec<SocketAddr> = state.sessions.values().map(|s| s.addr).collect();
            drop(state);
            for addr in targets {
                let _ = self.socket.try_send_to(&msg, addr);
            }
        }
    }

    /// Called by the game loop whenever a player moves (cheap; just a map write).
    pub fn update_position(&self, uuid: Uuid, position: Position) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.set_position(uuid, position);
    }

    pub fn set_muted(&self, uuid: Uuid, muted: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if muted {
            state.muted.insert(uuid);
        } else {
            state.muted.remove(&uuid);
        }
    }

    pub fn connected_count(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).sessions.len()
    }

    pub fn is_connected(&self, uuid: Uuid) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).sessions.contains_key(&uuid)
    }

    async fn receive_loop(self) {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            let Ok((len, addr)) = self.socket.recv_from(&mut buf).await else {
                continue;
            };
            let packet = &buf[..len];
            if packet.is_empty() {
                continue;
            }
            match packet[0] {
                AUTH => self.handle_auth(&packet[1..], addr).await,
                AUDIO => self.handle_audio(&packet[1..], addr).await,
                PING => {
                    let mut reply = vec![PONG];
                    reply.extend_from_slice(&packet[1..]);
                    let _ = self.socket.send_to(&reply, addr).await;
                    self.touch(addr);
                }
                LEAVE => {
                    let uuid = self.state.lock().unwrap_or_else(|e| e.into_inner()).by_addr.get(&addr).copied();
                    if let Some(uuid) = uuid {
                        self.unregister(uuid);
                    }
                }
                _ => {}
            }
        }
    }

    fn touch(&self, addr: SocketAddr) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(uuid) = state.by_addr.get(&addr).copied() {
            if let Some(s) = state.sessions.get_mut(&uuid) {
                s.last_seen = Instant::now();
            }
        }
    }

    async fn handle_auth(&self, body: &[u8], addr: SocketAddr) {
        if body.len() != 32 {
            self.send_error(addr, "bad auth packet").await;
            return;
        }
        let uuid = Uuid::from_slice(&body[..16]).unwrap();
        let secret: [u8; 16] = body[16..32].try_into().unwrap();
        let ok = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            // Constant-time compare is not needed: secrets are single-use and random.
            match state.pending.get(&uuid) {
                Some(expected) if *expected == secret => {
                    state.pending.remove(&uuid);
                    state.by_addr.insert(addr, uuid);
                    state.sessions.insert(
                        uuid,
                        Session {
                            addr,
                            last_seen: Instant::now(),
                            position: Position {
                                dimension: 0,
                                x: 0.0,
                                y: 0.0,
                                z: 0.0,
                            },
                        },
                    );
                    state.grid.entry((0, 0, 0)).or_default().insert(uuid);
                    true
                }
                _ => false,
            }
        };
        if ok {
            let mut reply = vec![AUTH_OK];
            reply.extend_from_slice(&self.range.to_be_bytes());
            let _ = self.socket.send_to(&reply, addr).await;
            tracing::debug!("voice: {uuid} authenticated from {addr}");
        } else {
            self.send_error(addr, "unknown player or wrong secret").await;
        }
    }

    async fn handle_audio(&self, body: &[u8], addr: SocketAddr) {
        // seq u32, flags u8, len u16, data
        if body.len() < 7 {
            return;
        }
        let flags = body[4];
        let len = u16::from_be_bytes([body[5], body[6]]) as usize;
        if body.len() < 7 + len {
            return;
        }
        let (speaker, position, targets) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(uuid) = state.by_addr.get(&addr).copied() else { return };
            if state.muted.contains(&uuid) {
                return;
            }
            let Some(session) = state.sessions.get_mut(&uuid) else { return };
            session.last_seen = Instant::now();
            let position = session.position;
            let range = if flags & 1 != 0 { self.range * WHISPER_FACTOR } else { self.range };
            (uuid, position, state.listeners(uuid, position, range))
        };
        if targets.is_empty() {
            return;
        }
        let mut out = Vec::with_capacity(1 + 16 + 4 + 1 + 12 + 2 + len);
        out.push(AUDIO_OUT);
        out.extend_from_slice(speaker.as_bytes());
        out.extend_from_slice(&body[..4]); // seq
        out.push(flags);
        out.extend_from_slice(&position.x.to_be_bytes());
        out.extend_from_slice(&position.y.to_be_bytes());
        out.extend_from_slice(&position.z.to_be_bytes());
        out.extend_from_slice(&body[5..7 + len]);
        for target in targets {
            // try_send_to never blocks; a full socket buffer just drops a frame.
            let _ = self.socket.try_send_to(&out, target);
        }
    }

    async fn send_error(&self, addr: SocketAddr, reason: &str) {
        let mut out = vec![ERROR];
        out.extend_from_slice(&(reason.len() as u16).to_be_bytes());
        out.extend_from_slice(reason.as_bytes());
        let _ = self.socket.send_to(&out, addr).await;
    }

    async fn reaper_loop(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let stale: Vec<Uuid> = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state
                    .sessions
                    .iter()
                    .filter(|(_, s)| s.last_seen.elapsed() > SESSION_TIMEOUT)
                    .map(|(u, _)| *u)
                    .collect()
            };
            for uuid in stale {
                tracing::debug!("voice: {uuid} timed out");
                self.unregister(uuid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: f32, z: f32) -> Position {
        Position { dimension: 0, x, y: 64.0, z }
    }

    #[test]
    fn listeners_respect_range_and_dimension() {
        let mut state = State::default();
        let addr = |port: u16| SocketAddr::from(([127, 0, 0, 1], port));
        for (i, p) in [pos(0.0, 0.0), pos(10.0, 0.0), pos(100.0, 0.0), pos(0.0, 20.0)].iter().enumerate() {
            let uuid = Uuid::from_u128(i as u128 + 1);
            state.sessions.insert(
                uuid,
                Session {
                    addr: addr(1000 + i as u16),
                    last_seen: Instant::now(),
                    position: *p,
                },
            );
            state.by_addr.insert(addr(1000 + i as u16), uuid);
            state.grid.entry(State::cell_of(*p)).or_default().insert(uuid);
        }
        let speaker = Uuid::from_u128(1);
        let mut heard = state.listeners(speaker, pos(0.0, 0.0), 48.0);
        heard.sort();
        assert_eq!(heard, vec![addr(1001), addr(1003)]);

        // Moving the far player into range updates the grid.
        state.set_position(Uuid::from_u128(3), pos(30.0, 0.0));
        assert_eq!(state.listeners(speaker, pos(0.0, 0.0), 48.0).len(), 3);
    }
}
