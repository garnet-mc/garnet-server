//! Messages between the web panel and the game server.
//!
//! The panel never touches game state directly. Every panel action becomes an
//! [`AdminRequest`] sent over a channel; the server answers with an
//! [`AdminResponse`]. This keeps the HTTP side simple and means the game
//! loop decides when it is safe to act.

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

/// What the server hands the panel at startup.
#[derive(Clone)]
pub struct AdminHandle {
    pub requests: mpsc::Sender<(AdminRequest, oneshot::Sender<AdminResponse>)>,
    /// Every console line, as it is logged.
    pub logs: broadcast::Sender<LogLine>,
}

impl AdminHandle {
    pub async fn call(&self, request: AdminRequest) -> AdminResponse {
        let (tx, rx) = oneshot::channel();
        if self.requests.send((request, tx)).await.is_err() {
            return AdminResponse::Error("server is shutting down".into());
        }
        rx.await.unwrap_or_else(|_| AdminResponse::Error("server did not answer".into()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogLine {
    pub time: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerStatus {
    pub name: String,
    pub version: String,
    pub minecraft_version: String,
    pub protocol_version: i32,
    pub online: usize,
    pub max_players: usize,
    pub tps: f32,
    pub tick_ms: f32,
    pub uptime_seconds: u64,
    pub loaded_chunks: usize,
    pub memory_mb: u64,
    pub world_name: String,
    pub world_time: i64,
    pub online_mode: bool,
    pub voice_connected: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlayerSummary {
    pub uuid: Uuid,
    pub name: String,
    pub ip: String,
    pub game_mode: String,
    pub latency_ms: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub dimension: String,
    pub is_op: bool,
    pub online_seconds: u64,
    pub voice_connected: bool,
    pub violations: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BanEntry {
    pub name: String,
    pub uuid: Option<Uuid>,
    pub reason: String,
    pub source: String,
    pub created: String,
    pub expires: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub time: String,
    pub actor: String,
    pub action: String,
    pub details: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ViolationEntry {
    pub time: String,
    pub player: String,
    pub check: String,
    pub details: String,
    pub level: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupInfo {
    pub file: String,
    pub size_bytes: u64,
    pub created: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub authors: Vec<String>,
    pub description: String,
    pub kind: String,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub enum AdminRequest {
    Status,
    Players,
    Kick { uuid: Uuid, reason: String, actor: String },
    Ban { target: String, reason: String, hours: Option<u64>, actor: String },
    BanIp { ip: String, reason: String, actor: String },
    Pardon { target: String, actor: String },
    Bans,
    Whitelist,
    WhitelistAdd { name: String, actor: String },
    WhitelistRemove { name: String, actor: String },
    SetGameMode { uuid: Uuid, mode: String, actor: String },
    SetOp { uuid: Uuid, op: bool, actor: String },
    Message { uuid: Option<Uuid>, text: String, actor: String },
    Teleport { uuid: Uuid, x: f64, y: f64, z: f64, actor: String },
    Command { command: String, actor: String },
    ConsoleHistory,
    Mods,
    SetModEnabled { id: String, enabled: bool, actor: String },
    ReloadMods { actor: String },
    ConfigText,
    SaveConfigText { text: String, actor: String },
    Audit,
    Violations,
    Backups,
    CreateBackup { actor: String },
    VoiceMute { uuid: Uuid, muted: bool, actor: String },
    SaveWorld { actor: String },
    Stop { actor: String },
    /// A player typed `/panel`; the server asks the panel for a login link.
    /// (Handled inside the panel, listed here for completeness.)
    Noop,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AdminResponse {
    Status(ServerStatus),
    Players(Vec<PlayerSummary>),
    Bans(Vec<BanEntry>),
    Names(Vec<String>),
    Lines(Vec<LogLine>),
    Mods(Vec<ModSummary>),
    Text(String),
    Audit(Vec<AuditEntry>),
    Violations(Vec<ViolationEntry>),
    Backups(Vec<BackupInfo>),
    Ok,
    Error(String),
}

impl AdminResponse {
    pub fn error(message: impl Into<String>) -> Self {
        AdminResponse::Error(message.into())
    }
}
