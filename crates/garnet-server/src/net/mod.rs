//! Networking: accepting connections and driving each one through the
//! handshake, status, login, configuration and play states.

pub mod config_phase;
pub mod connection;
pub mod login;
pub mod play;
pub mod status;

use crate::server::Server;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

/// Connections per IP within the throttle window before we start refusing.
const MAX_CONNECTIONS_PER_IP_PER_10S: u32 = 30;

/// Accepts connections until the server shuts down.
pub async fn listen(server: Arc<Server>) -> anyhow::Result<()> {
    let (bind, port) = {
        let c = server.config();
        (c.server.bind.clone(), c.server.port)
    };
    let listener = TcpListener::bind((bind.as_str(), port)).await?;
    tracing::info!("listening for players on {bind}:{port}");
    let throttle: Arc<Mutex<HashMap<IpAddr, (u32, Instant)>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut shutdown = server.shutdown.subscribe();
    loop {
        let accepted = tokio::select! {
            r = listener.accept() => r,
            _ = shutdown.changed() => break,
        };
        let (socket, addr) = match accepted {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("accept failed: {err}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        if !allow_connection(&throttle, addr.ip()) {
            tracing::debug!("throttled connection from {}", addr.ip());
            continue;
        }
        let _ = socket.set_nodelay(true);
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            let mut connection = connection::Connection::new(server, socket, addr);
            connection.run().await;
        });
    }
    Ok(())
}

/// Simple per-IP connection throttle against connection floods. Bans are
/// checked later, once we know who is connecting.
fn allow_connection(throttle: &Mutex<HashMap<IpAddr, (u32, Instant)>>, ip: IpAddr) -> bool {
    let mut map = throttle.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if map.len() > 10_000 {
        map.retain(|_, (_, since)| now.duration_since(*since) < Duration::from_secs(10));
    }
    let entry = map.entry(ip).or_insert((0, now));
    if now.duration_since(entry.1) >= Duration::from_secs(10) {
        *entry = (0, now);
    }
    entry.0 += 1;
    entry.0 <= MAX_CONNECTIONS_PER_IP_PER_10S
}
