//! Events the server raises for mods.

use crate::{GameMode, Location, Pos};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The server finished starting and is accepting connections.
    ServerStarted,
    /// The server is about to shut down; last chance to save.
    ServerStopping,
    /// Fired every game tick (20 times per second). Only sent to mods that
    /// asked for it in their manifest, because it is expensive.
    Tick { tick: u64 },
    /// A player passed login and is about to join. Cancelling kicks them
    /// with `EventResult::message` as the reason.
    PlayerLogin { uuid: Uuid, name: String, ip: String },
    PlayerJoin { uuid: Uuid, name: String },
    PlayerQuit { uuid: Uuid, name: String },
    /// Chat message. Cancel to swallow it; set `message` to rewrite it.
    Chat { uuid: Uuid, name: String, message: String },
    /// A `/command` from a player. `command` has no leading slash.
    /// Cancel if you handled it (e.g. a command you registered).
    Command { uuid: Option<Uuid>, name: String, command: String },
    PlayerMove { uuid: Uuid, from: Location, to: Location },
    BlockBreak { uuid: Uuid, pos: Pos, block: String },
    BlockPlace { uuid: Uuid, pos: Pos, block: String },
    /// Right-click on a block (`block` is what was clicked).
    BlockInteract { uuid: Uuid, pos: Pos, block: String, face: u8 },
    GameModeChange { uuid: Uuid, from: GameMode, to: GameMode },
    /// A timer created with `Action::Schedule` went off.
    TimerFired { id: String },
    /// Data from a client-side mod on the `garnet:` plugin channel.
    PluginMessage { uuid: Uuid, channel: String, data: Vec<u8> },
}

impl Event {
    /// Whether mods are allowed to cancel this event.
    pub fn is_cancellable(&self) -> bool {
        matches!(
            self,
            Event::PlayerLogin { .. }
                | Event::Chat { .. }
                | Event::Command { .. }
                | Event::PlayerMove { .. }
                | Event::BlockBreak { .. }
                | Event::BlockPlace { .. }
                | Event::BlockInteract { .. }
        )
    }
}

/// A mod's answer to an event.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EventResult {
    /// Stop the default behaviour (and later mods from seeing the event).
    #[serde(default)]
    pub cancel: bool,
    /// For `Chat`: the rewritten message. For `PlayerLogin`: the kick reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl EventResult {
    pub fn cancelled() -> Self {
        Self {
            cancel: true,
            message: None,
        }
    }

    pub fn cancelled_with(message: impl Into<String>) -> Self {
        Self {
            cancel: true,
            message: Some(message.into()),
        }
    }

    pub fn rewrite(message: impl Into<String>) -> Self {
        Self {
            cancel: false,
            message: Some(message.into()),
        }
    }
}
