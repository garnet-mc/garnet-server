//! Append-only record of who did what: panel actions, commands, bans, kicks.
//! Written as one JSON object per line to `logs/audit.jsonl` and kept in
//! memory for the panel.

use garnet_admin::api::AuditEntry;
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const KEEP_IN_MEMORY: usize = 1000;

pub struct Audit {
    path: PathBuf,
    recent: Mutex<VecDeque<AuditEntry>>,
}

impl Audit {
    pub fn new(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut recent = VecDeque::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines().rev().take(KEEP_IN_MEMORY) {
                if let Ok(entry) = serde_json::from_str::<AuditEntry>(line) {
                    recent.push_front(entry);
                }
            }
        }
        Self {
            path,
            recent: Mutex::new(recent),
        }
    }

    pub fn record(&self, actor: &str, action: &str, details: impl Into<String>) {
        let entry = AuditEntry {
            time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            actor: actor.to_owned(),
            action: action.to_owned(),
            details: details.into(),
        };
        tracing::info!(target: "audit", "{} {} {}", entry.actor, entry.action, entry.details);
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(file, "{}", serde_json::to_string(&entry).unwrap_or_default());
        }
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        if recent.len() >= KEEP_IN_MEMORY {
            recent.pop_front();
        }
        recent.push_back(entry);
    }

    /// Newest first.
    pub fn recent(&self) -> Vec<AuditEntry> {
        self.recent.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().cloned().collect()
    }
}
