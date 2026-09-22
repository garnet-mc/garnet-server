//! One client connection, from the first byte to the last.

use crate::player::{Outbound, Player};
use crate::server::Server;
use garnet_protocol::packets::config as config_packets;
use garnet_protocol::packets::handshake::{Handshake, Intent};
use garnet_protocol::packets::login as login_packets;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::{ClientboundPacket, Codec, GameProfile, PacketReader, ProtocolError, State, Text};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Clients that send nothing useful for this long are dropped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Connection {
    pub server: Arc<Server>,
    pub addr: SocketAddr,
    /// The address we treat as the player's, possibly from a proxy.
    pub ip: String,
    reader: OwnedReadHalf,
    writer: OwnedWriteHalf,
    pub codec: Codec,
    pub state: State,
    pub protocol_version: i32,
    pub handshake: Option<Handshake>,
    pub verify_token: [u8; 4],
    /// Name the client claimed in Login Start, before verification.
    pub claimed_name: String,
    pub profile: Option<GameProfile>,
    /// Behind a proxy the real UUID and skin come from the handshake.
    pub proxied_profile: Option<GameProfile>,
    pub client_info: Option<config_packets::ClientInformation>,
    /// What the client told us on `garnet:mods`, if it is a Garnet client.
    pub mod_report: Option<crate::client_mods::ClientReport>,
    pub player: Option<Arc<Player>>,
    outbound_rx: Option<mpsc::UnboundedReceiver<Outbound>>,
    pub velocity_query_id: Option<i32>,
    last_packet: Instant,
}

impl Connection {
    pub fn new(server: Arc<Server>, socket: TcpStream, addr: SocketAddr) -> Self {
        let (reader, writer) = socket.into_split();
        Self {
            server,
            addr,
            ip: addr.ip().to_string(),
            reader,
            writer,
            codec: Codec::new(),
            state: State::Handshake,
            protocol_version: 0,
            handshake: None,
            verify_token: rand::random(),
            claimed_name: String::new(),
            profile: None,
            proxied_profile: None,
            client_info: None,
            mod_report: None,
            player: None,
            outbound_rx: None,
            velocity_query_id: None,
            last_packet: Instant::now(),
        }
    }

    /// Runs the connection to completion. Errors end the connection quietly;
    /// they are almost always a client going away or misbehaving.
    pub async fn run(&mut self) {
        let result = self.run_inner().await;
        if let Some(player) = self.player.take() {
            crate::net::play::on_quit(&self.server, &player).await;
        }
        match result {
            Ok(()) => {}
            Err(err) => match err.downcast_ref::<std::io::Error>() {
                Some(io) if matches!(io.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof) => {}
                _ => {
                    if self.state == State::Play {
                        tracing::info!("{} disconnected: {err}", self.display_name());
                    } else {
                        tracing::debug!("{} ({:?}) dropped: {err}", self.addr, self.state);
                    }
                }
            },
        }
    }

    fn display_name(&self) -> String {
        self.profile
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| self.addr.to_string())
    }

    async fn run_inner(&mut self) -> anyhow::Result<()> {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let idle = tokio::time::sleep_until(tokio::time::Instant::from_std(self.last_packet + IDLE_TIMEOUT));
            tokio::select! {
                read = self.reader.read(&mut buf) => {
                    let n = read?;
                    if n == 0 {
                        return Ok(());
                    }
                    self.codec.push_received(&buf[..n]);
                    loop {
                        let packet = match self.codec.next_packet() {
                            Ok(Some(p)) => p,
                            Ok(None) => break,
                            Err(err) => {
                                self.kick(Text::new("Malformed packet")).await;
                                return Err(err.into());
                            }
                        };
                        self.last_packet = Instant::now();
                        if let Err(err) = self.handle_packet(&packet).await {
                            match err.downcast_ref::<ProtocolError>() {
                                Some(ProtocolError::UnknownPacket { .. }) => {
                                    // Unknown or unhandled packets are ignored; the
                                    // client sends plenty we do not care about.
                                }
                                _ => {
                                    self.kick(Text::new("Invalid packet")).await;
                                    return Err(err);
                                }
                            }
                        }
                        if self.state == State::Play && self.player.as_ref().map(|p| !p.is_connected()).unwrap_or(false) {
                            return Ok(());
                        }
                    }
                }
                message = recv_outbound(&mut self.outbound_rx) => {
                    match message {
                        Some(Outbound::Packet(bytes)) => {
                            let frame = self.codec.encode(&bytes);
                            self.writer.write_all(&frame).await?;
                        }
                        Some(Outbound::Disconnect(reason)) => {
                            self.kick(reason).await;
                            return Ok(());
                        }
                        None => return Ok(()),
                    }
                }
                _ = idle => {
                    if self.state == State::Play {
                        self.kick(Text::translate("disconnect.timeout", vec![])).await;
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Encodes and writes a packet right away. Used before the play state;
    /// during play, packets go through the player's queue instead.
    pub async fn send<P: ClientboundPacket>(&mut self, packet: &P) -> anyhow::Result<()> {
        let payload = self.server.data.packet_ids.encode(packet)?;
        let frame = self.codec.encode(&payload);
        self.writer.write_all(&frame).await?;
        Ok(())
    }

    /// Sends the right disconnect packet for the current state and flushes.
    pub async fn kick(&mut self, reason: Text) {
        let result = match self.state {
            State::Login => self.send(&login_packets::LoginDisconnect { reason }).await,
            State::Config => self.send(&config_packets::ConfigDisconnect { reason }).await,
            State::Play => self.send(&cb::Disconnect { reason }).await,
            _ => Ok(()),
        };
        let _ = result;
        let _ = self.writer.flush().await;
        let _ = self.writer.shutdown().await;
    }

    async fn handle_packet(&mut self, payload: &[u8]) -> anyhow::Result<()> {
        let data = Arc::clone(&self.server.data);
        let ids = &data.packet_ids;
        match self.state {
            State::Handshake => {
                // The handshake has one packet; legacy pings start with 0xFE.
                if payload.first() == Some(&0xFE) {
                    return Ok(());
                }
                let mut reader = PacketReader::new(payload);
                let id = reader.read_varint()?;
                if id != 0 {
                    anyhow::bail!("expected handshake, got packet {id}");
                }
                let handshake = garnet_protocol::packets::read_exact::<Handshake>(&mut reader)?;
                self.protocol_version = handshake.protocol_version;
                self.state = match handshake.intent {
                    Intent::Status => State::Status,
                    Intent::Login | Intent::Transfer => State::Login,
                };
                self.handshake = Some(handshake);
                Ok(())
            }
            State::Status => {
                let (name, mut reader) = ids.decode(State::Status, payload)?;
                crate::net::status::handle(self, name, &mut reader).await
            }
            State::Login => {
                let (name, mut reader) = ids.decode(State::Login, payload)?;
                let name = name.to_owned();
                crate::net::login::handle(self, &name, &mut reader).await
            }
            State::Config => {
                let (name, mut reader) = ids.decode(State::Config, payload)?;
                let name = name.to_owned();
                crate::net::config_phase::handle(self, &name, &mut reader).await
            }
            State::Play => {
                let (name, mut reader) = ids.decode(State::Play, payload)?;
                let name = name.to_owned();
                let player = self.player.clone().expect("player exists in play state");
                crate::net::play::handle(&self.server, &player, &name, &mut reader).await
            }
        }
    }

    /// Switches to the play state: from here on packets are queued through
    /// the player rather than written directly.
    pub fn enter_play(&mut self, player: Arc<Player>, rx: mpsc::UnboundedReceiver<Outbound>) {
        self.state = State::Play;
        self.player = Some(player);
        self.outbound_rx = Some(rx);
    }
}

async fn recv_outbound(rx: &mut Option<mpsc::UnboundedReceiver<Outbound>>) -> Option<Outbound> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}
