//! Bans, IP bans, ops and the whitelist, in the same JSON files vanilla uses
//! (`banned-players.json`, `banned-ips.json`, `ops.json`, `whitelist.json`)
//! so existing tools and migrations from other servers keep working.
//!
//! Files are written atomically (temp file + rename).

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use uuid::Uuid;

/// Vanilla's timestamp format: `2026-09-22 14:20:00 +0000`.
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S %z";

fn now_string() -> String {
    Utc::now().format(TIME_FORMAT).to_string()
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(s, TIME_FORMAT).ok().map(|t| t.with_timezone(&Utc))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlayerBan {
    #[serde(with = "uuid::serde::simple", default = "Uuid::nil")]
    pub uuid: Uuid,
    pub name: String,
    #[serde(default = "now_string")]
    pub created: String,
    #[serde(default)]
    pub source: String,
    /// `forever` or a timestamp.
    #[serde(default = "forever")]
    pub expires: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IpBan {
    pub ip: String,
    #[serde(default = "now_string")]
    pub created: String,
    #[serde(default)]
    pub source: String,
    #[serde(default = "forever")]
    pub expires: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpEntry {
    #[serde(with = "uuid::serde::simple")]
    pub uuid: Uuid,
    pub name: String,
    #[serde(default = "default_op_level")]
    pub level: u8,
    #[serde(rename = "bypassesPlayerLimit", default)]
    pub bypasses_player_limit: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WhitelistEntry {
    #[serde(with = "uuid::serde::simple", default = "Uuid::nil")]
    pub uuid: Uuid,
    pub name: String,
}

fn forever() -> String {
    "forever".into()
}

fn default_op_level() -> u8 {
    4
}

/// Whether a ban is still active; expired bans are pruned on save.
fn active(expires: &str) -> bool {
    if expires == "forever" {
        return true;
    }
    parse_time(expires).map(|t| t > Utc::now()).unwrap_or(true)
}

pub fn expiry_in(hours: Option<u64>) -> String {
    match hours {
        Some(h) => (Utc::now() + Duration::hours(h as i64)).format(TIME_FORMAT).to_string(),
        None => forever(),
    }
}

struct JsonFile<T> {
    path: PathBuf,
    entries: Vec<T>,
}

impl<T: Serialize + DeserializeOwned + Clone> JsonFile<T> {
    fn load(path: PathBuf) -> Result<Self> {
        let entries = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            if text.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
            }
        } else {
            Vec::new()
        };
        Ok(Self { path, entries })
    }

    fn save(&self) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.entries)?)?;
        std::fs::rename(&tmp, &self.path).with_context(|| format!("saving {}", self.path.display()))?;
        Ok(())
    }
}

/// All four lists behind one lock; they are tiny and rarely change.
pub struct Lists {
    inner: RwLock<Inner>,
}

struct Inner {
    bans: JsonFile<PlayerBan>,
    ip_bans: JsonFile<IpBan>,
    ops: JsonFile<OpEntry>,
    whitelist: JsonFile<WhitelistEntry>,
}

impl Lists {
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            inner: RwLock::new(Inner {
                bans: JsonFile::load(dir.join("banned-players.json"))?,
                ip_bans: JsonFile::load(dir.join("banned-ips.json"))?,
                ops: JsonFile::load(dir.join("ops.json"))?,
                whitelist: JsonFile::load(dir.join("whitelist.json"))?,
            }),
        })
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    // ---- bans ----

    /// The active ban for a player, matched by UUID or (case-insensitive) name.
    pub fn ban_for(&self, uuid: Uuid, name: &str) -> Option<PlayerBan> {
        self.read()
            .bans
            .entries
            .iter()
            .find(|b| active(&b.expires) && (b.uuid == uuid || b.name.eq_ignore_ascii_case(name)))
            .cloned()
    }

    pub fn ip_ban_for(&self, ip: &str) -> Option<IpBan> {
        self.read().ip_bans.entries.iter().find(|b| active(&b.expires) && b.ip == ip).cloned()
    }

    pub fn ban(&self, uuid: Uuid, name: &str, reason: &str, source: &str, hours: Option<u64>) -> Result<()> {
        let mut inner = self.write();
        inner.bans.entries.retain(|b| b.uuid != uuid && !b.name.eq_ignore_ascii_case(name));
        inner.bans.entries.push(PlayerBan {
            uuid,
            name: name.to_owned(),
            created: now_string(),
            source: source.to_owned(),
            expires: expiry_in(hours),
            reason: if reason.is_empty() { "Banned by an operator.".into() } else { reason.to_owned() },
        });
        inner.bans.save()
    }

    pub fn ban_ip(&self, ip: &str, reason: &str, source: &str, hours: Option<u64>) -> Result<()> {
        let mut inner = self.write();
        inner.ip_bans.entries.retain(|b| b.ip != ip);
        inner.ip_bans.entries.push(IpBan {
            ip: ip.to_owned(),
            created: now_string(),
            source: source.to_owned(),
            expires: expiry_in(hours),
            reason: if reason.is_empty() { "Banned by an operator.".into() } else { reason.to_owned() },
        });
        inner.ip_bans.save()
    }

    /// Removes a player ban by name or UUID, or an IP ban. Returns whether
    /// anything was removed.
    pub fn pardon(&self, target: &str) -> Result<bool> {
        let mut inner = self.write();
        let uuid = Uuid::parse_str(target).ok();
        let before = inner.bans.entries.len() + inner.ip_bans.entries.len();
        inner
            .bans
            .entries
            .retain(|b| !(b.name.eq_ignore_ascii_case(target) || Some(b.uuid) == uuid));
        inner.ip_bans.entries.retain(|b| b.ip != target);
        let removed = before != inner.bans.entries.len() + inner.ip_bans.entries.len();
        if removed {
            inner.bans.save()?;
            inner.ip_bans.save()?;
        }
        Ok(removed)
    }

    pub fn bans(&self) -> (Vec<PlayerBan>, Vec<IpBan>) {
        let inner = self.read();
        (
            inner.bans.entries.iter().filter(|b| active(&b.expires)).cloned().collect(),
            inner.ip_bans.entries.iter().filter(|b| active(&b.expires)).cloned().collect(),
        )
    }

    // ---- ops ----

    pub fn is_op(&self, uuid: Uuid) -> bool {
        self.read().ops.entries.iter().any(|o| o.uuid == uuid)
    }

    pub fn op_level(&self, uuid: Uuid) -> u8 {
        self.read().ops.entries.iter().find(|o| o.uuid == uuid).map(|o| o.level).unwrap_or(0)
    }

    pub fn set_op(&self, uuid: Uuid, name: &str, op: bool) -> Result<bool> {
        let mut inner = self.write();
        let was = inner.ops.entries.iter().any(|o| o.uuid == uuid);
        if op == was {
            return Ok(false);
        }
        if op {
            inner.ops.entries.push(OpEntry {
                uuid,
                name: name.to_owned(),
                level: 4,
                bypasses_player_limit: false,
            });
        } else {
            inner.ops.entries.retain(|o| o.uuid != uuid);
        }
        inner.ops.save()?;
        Ok(true)
    }

    // ---- whitelist ----

    pub fn is_whitelisted(&self, uuid: Uuid, name: &str) -> bool {
        self.read()
            .whitelist
            .entries
            .iter()
            .any(|w| w.uuid == uuid || w.name.eq_ignore_ascii_case(name))
    }

    pub fn whitelist_add(&self, uuid: Uuid, name: &str) -> Result<bool> {
        let mut inner = self.write();
        if inner.whitelist.entries.iter().any(|w| w.name.eq_ignore_ascii_case(name)) {
            return Ok(false);
        }
        inner.whitelist.entries.push(WhitelistEntry {
            uuid,
            name: name.to_owned(),
        });
        inner.whitelist.save()?;
        Ok(true)
    }

    pub fn whitelist_remove(&self, name: &str) -> Result<bool> {
        let mut inner = self.write();
        let before = inner.whitelist.entries.len();
        inner.whitelist.entries.retain(|w| !w.name.eq_ignore_ascii_case(name));
        let removed = before != inner.whitelist.entries.len();
        if removed {
            inner.whitelist.save()?;
        }
        Ok(removed)
    }

    pub fn whitelist(&self) -> Vec<String> {
        self.read().whitelist.entries.iter().map(|w| w.name.clone()).collect()
    }
}
