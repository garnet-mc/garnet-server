//! The Garnet mod API.
//!
//! Everything a mod can see or do is expressed as plain data:
//!
//! - an [`Event`] is something that happened; the server hands it to mods and
//!   they answer with an [`EventResult`] (cancel it, or change it),
//! - an [`Action`] is something a mod asks the server to do,
//! - a [`Query`] asks the server a question and gets a [`QueryResult`].
//!
//! Because they are all serialisable, the same API is used by WASM mods
//! (over a JSON boundary, see `garnet-mods`) and by native Rust plugins that
//! implement [`Plugin`] directly. Mod authors never touch packets.

pub mod events;
pub mod permissions;

pub use events::*;
pub use permissions::*;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// API version mods are compiled against. Bumped only on breaking changes.
pub const API_VERSION: u32 = 1;

/// The `mod.toml` manifest every mod ships.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModInfo {
    /// Lowercase identifier, e.g. `economy`. Used for permissions and logs.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// API version the mod targets; the server refuses newer ones.
    #[serde(default = "default_api_version")]
    pub api_version: u32,
    /// Other mods that must be loaded first.
    #[serde(default)]
    pub depends: Vec<String>,
}

fn default_api_version() -> u32 {
    API_VERSION
}

/// A block position, kept separate from the protocol crate so the API has
/// no dependency on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

/// What a mod knows about an online player.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub uuid: Uuid,
    pub name: String,
    pub location: Location,
    pub game_mode: GameMode,
    pub latency_ms: i32,
    pub is_op: bool,
}

/// Things a mod can ask the server to do. Every variant is fire-and-forget.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Write to the server log.
    Log { level: LogLevel, message: String },
    /// Chat message to one player. `text` may use `&` colour codes.
    SendMessage { player: Uuid, text: String },
    /// Chat message to everyone.
    Broadcast { text: String },
    /// Text above the hotbar.
    ActionBar { player: Uuid, text: String },
    Title { player: Uuid, title: String, subtitle: String, fade_in: i32, stay: i32, fade_out: i32 },
    Kick { player: Uuid, reason: String },
    Teleport { player: Uuid, location: Location },
    SetGameMode { player: Uuid, game_mode: GameMode },
    SetBlock { pos: Pos, block: String },
    SetTime { time_of_day: i64 },
    /// Run a command as the console.
    RunCommand { command: String },
    /// Make `/name` available. The mod receives [`Event::Command`] when used.
    RegisterCommand { name: String, description: String, permission: Option<String> },
    /// Ask for a [`Event::TimerFired`] in `delay_ticks` ticks (20 per second).
    Schedule { id: String, delay_ticks: u32, repeat: bool },
    CancelSchedule { id: String },
    /// Send a plugin message to the player's client (for Garnet client mods).
    PluginMessage { player: Uuid, channel: String, data: Vec<u8> },
    /// Persist a small piece of mod state; survives restarts.
    StoreData { key: String, value: serde_json::Value },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Questions a mod can ask. Answered synchronously with a [`QueryResult`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Query {
    Players,
    Player { uuid: Uuid },
    PlayerByName { name: String },
    Block { pos: Pos },
    Time,
    HasPermission { player: Uuid, permission: String },
    LoadData { key: String },
    ServerInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueryResult {
    Players { players: Vec<PlayerInfo> },
    Player { player: Option<PlayerInfo> },
    Block { block: String },
    Time { world_age: i64, time_of_day: i64 },
    Bool { value: bool },
    Data { value: Option<serde_json::Value> },
    ServerInfo { version: String, minecraft_version: String, online: usize, max_players: usize, tps: f32 },
    Error { message: String },
}

/// The server, as seen from a native plugin.
pub trait Host {
    fn act(&mut self, action: Action);
    fn query(&mut self, query: Query) -> QueryResult;
}

/// A plugin compiled into the server (as opposed to a WASM mod).
pub trait Plugin: Send {
    fn info(&self) -> ModInfo;
    /// Called once after the server has loaded the world.
    fn on_enable(&mut self, _host: &mut dyn Host) {}
    fn on_disable(&mut self, _host: &mut dyn Host) {}
    fn on_event(&mut self, _event: &Event, _host: &mut dyn Host) -> EventResult {
        EventResult::default()
    }
}
