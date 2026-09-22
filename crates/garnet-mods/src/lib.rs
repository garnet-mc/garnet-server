//! Loads and runs mods.
//!
//! Two kinds of mod exist:
//! - **WASM mods** in `mods/<id>/mod.wasm` (+ `mod.toml`), sandboxed and
//!   written in any language that compiles to WebAssembly. They talk to the
//!   server over a tiny JSON boundary described in `wasm.rs`.
//! - **Native plugins** implementing `garnet_api::Plugin`, compiled into the
//!   server binary (used for built-in features and by people embedding
//!   Garnet in their own Rust project).
//!
//! Both receive the same [`Event`]s and use the same [`ModHost`] to act.

pub mod wasm;

use anyhow::{Context, Result};
use garnet_api::{Action, Event, EventResult, Host, LogLevel, ModInfo, Plugin, Query, QueryResult};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What the server offers to mods. Implemented in `garnet-server`.
///
/// `act` must be cheap and must never call back into the mod runtime
/// (queue the action and apply it on the next tick), otherwise a mod that
/// runs a command from inside an event handler would deadlock.
pub trait ModHost: Send + Sync {
    fn act(&self, mod_id: &str, action: Action);
    fn query(&self, mod_id: &str, query: Query) -> QueryResult;
}

/// One loaded mod of either kind.
pub struct LoadedMod {
    pub info: ModInfo,
    pub enabled: bool,
    pub path: Option<PathBuf>,
    kind: ModKind,
    /// Whether the manifest asked for the (expensive) tick event.
    pub wants_ticks: bool,
}

enum ModKind {
    Wasm(wasm::WasmMod),
    Native(Box<dyn Plugin>),
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ModSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub authors: Vec<String>,
    pub description: String,
    pub kind: &'static str,
    pub enabled: bool,
}

pub struct ModRuntime {
    host: Arc<dyn ModHost>,
    engine: wasmtime::Engine,
    mods: Vec<LoadedMod>,
    mods_dir: PathBuf,
}

impl ModRuntime {
    pub fn new(mods_dir: &Path, host: Arc<dyn ModHost>) -> Result<Self> {
        let mut config = wasmtime::Config::new();
        // Fuel lets us stop a mod that loops forever instead of freezing the server.
        config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&config)?;
        std::fs::create_dir_all(mods_dir)?;
        Ok(Self {
            host,
            engine,
            mods: Vec::new(),
            mods_dir: mods_dir.to_owned(),
        })
    }

    /// Registers a plugin compiled into the server.
    pub fn add_native(&mut self, plugin: Box<dyn Plugin>) {
        let info = plugin.info();
        tracing::info!("loaded built-in plugin {} {}", info.name, info.version);
        self.mods.push(LoadedMod {
            wants_ticks: false,
            info,
            enabled: true,
            path: None,
            kind: ModKind::Native(plugin),
        });
    }

    /// Loads every WASM mod under the mods directory. A broken mod is logged
    /// and skipped; it never stops the server from starting.
    pub fn load_wasm_mods(&mut self) -> Result<()> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&self.mods_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        entries.sort();
        for entry in entries {
            let (wasm_path, manifest_path) = if entry.is_dir() {
                (entry.join("mod.wasm"), Some(entry.join("mod.toml")))
            } else if entry.extension().map(|e| e == "wasm").unwrap_or(false) {
                (entry.clone(), None)
            } else {
                continue;
            };
            if !wasm_path.exists() {
                continue;
            }
            match self.load_one_wasm(&wasm_path, manifest_path.as_deref()) {
                Ok(()) => {}
                Err(err) => tracing::error!("failed to load mod {}: {err:#}", wasm_path.display()),
            }
        }
        Ok(())
    }

    fn load_one_wasm(&mut self, wasm_path: &Path, manifest_path: Option<&Path>) -> Result<()> {
        let info = match manifest_path.filter(|p| p.exists()) {
            Some(p) => {
                let text = std::fs::read_to_string(p)?;
                toml::from_str::<ModInfo>(&text).with_context(|| format!("parsing {}", p.display()))?
            }
            None => {
                let id = wasm_path.file_stem().unwrap().to_string_lossy().to_string();
                ModInfo {
                    id: id.clone(),
                    name: id,
                    version: "0.0.0".into(),
                    authors: Vec::new(),
                    description: String::new(),
                    api_version: garnet_api::API_VERSION,
                    depends: Vec::new(),
                }
            }
        };
        if info.api_version > garnet_api::API_VERSION {
            anyhow::bail!(
                "mod {} needs API version {} but this server provides {}",
                info.id,
                info.api_version,
                garnet_api::API_VERSION
            );
        }
        if self.mods.iter().any(|m| m.info.id == info.id) {
            anyhow::bail!("a mod with id '{}' is already loaded", info.id);
        }
        let wants_ticks = manifest_path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.contains("ticks = true"))
            .unwrap_or(false);
        let module = wasm::WasmMod::load(&self.engine, wasm_path, info.clone(), Arc::clone(&self.host))?;
        tracing::info!("loaded mod {} {} ({})", info.name, info.version, wasm_path.display());
        self.mods.push(LoadedMod {
            info,
            enabled: true,
            path: Some(wasm_path.to_owned()),
            kind: ModKind::Wasm(module),
            wants_ticks,
        });
        Ok(())
    }

    /// Calls every mod's init/enable hook. Done once the world is ready.
    pub fn enable_all(&mut self) {
        let host = Arc::clone(&self.host);
        for m in &mut self.mods {
            let result = match &mut m.kind {
                ModKind::Wasm(w) => w.init(),
                ModKind::Native(p) => {
                    let mut adapter = NativeHost {
                        host: &*host,
                        mod_id: &m.info.id,
                    };
                    p.on_enable(&mut adapter);
                    Ok(())
                }
            };
            if let Err(err) = result {
                tracing::error!("mod {} failed to initialise and was disabled: {err:#}", m.info.id);
                m.enabled = false;
            }
        }
    }

    /// Delivers an event to every enabled mod in load order. The first mod to
    /// cancel wins; a rewritten message from one mod is passed on to the next.
    pub fn dispatch(&mut self, event: &Event) -> EventResult {
        let mut current = event.clone();
        let mut combined = EventResult::default();
        let host = Arc::clone(&self.host);
        for m in &mut self.mods {
            if !m.enabled {
                continue;
            }
            if matches!(current, Event::Tick { .. }) && !m.wants_ticks {
                continue;
            }
            let result = match &mut m.kind {
                ModKind::Wasm(w) => match w.event(&current) {
                    Ok(r) => r,
                    Err(err) => {
                        tracing::error!("mod {} crashed handling {}: {err:#}; disabling it", m.info.id, event_name(&current));
                        m.enabled = false;
                        continue;
                    }
                },
                ModKind::Native(p) => {
                    let mut adapter = NativeHost {
                        host: &*host,
                        mod_id: &m.info.id,
                    };
                    p.on_event(&current, &mut adapter)
                }
            };
            if let Some(message) = &result.message {
                if let Event::Chat { message: m, .. } = &mut current {
                    *m = message.clone();
                }
                combined.message = Some(message.clone());
            }
            if result.cancel && current.is_cancellable() {
                combined.cancel = true;
                break;
            }
        }
        combined
    }

    pub fn list(&self) -> Vec<ModSummary> {
        self.mods
            .iter()
            .map(|m| ModSummary {
                id: m.info.id.clone(),
                name: m.info.name.clone(),
                version: m.info.version.clone(),
                authors: m.info.authors.clone(),
                description: m.info.description.clone(),
                kind: match m.kind {
                    ModKind::Wasm(_) => "wasm",
                    ModKind::Native(_) => "native",
                },
                enabled: m.enabled,
            })
            .collect()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        match self.mods.iter_mut().find(|m| m.info.id == id) {
            Some(m) => {
                m.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// Drops all WASM mods and loads them again from disk. Native plugins
    /// are kept.
    pub fn reload(&mut self) -> Result<()> {
        self.mods.retain(|m| matches!(m.kind, ModKind::Native(_)));
        self.load_wasm_mods()?;
        self.enable_all();
        Ok(())
    }

    pub fn shutdown(&mut self) {
        let host = Arc::clone(&self.host);
        for m in &mut self.mods {
            if let ModKind::Native(p) = &mut m.kind {
                let mut adapter = NativeHost {
                    host: &*host,
                    mod_id: &m.info.id,
                };
                p.on_disable(&mut adapter);
            }
        }
    }
}

fn event_name(event: &Event) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "event".into())
}

/// Adapts the shared [`ModHost`] to the per-plugin [`Host`] trait.
struct NativeHost<'a> {
    host: &'a dyn ModHost,
    mod_id: &'a str,
}

impl Host for NativeHost<'_> {
    fn act(&mut self, action: Action) {
        self.host.act(self.mod_id, action);
    }
    fn query(&mut self, query: Query) -> QueryResult {
        self.host.query(self.mod_id, query)
    }
}

/// Convenience for hosts: turn a `Log` action into a tracing call.
pub fn log_action(mod_id: &str, level: LogLevel, message: &str) {
    match level {
        LogLevel::Debug => tracing::debug!(target: "mod", "[{mod_id}] {message}"),
        LogLevel::Info => tracing::info!(target: "mod", "[{mod_id}] {message}"),
        LogLevel::Warn => tracing::warn!(target: "mod", "[{mod_id}] {message}"),
        LogLevel::Error => tracing::error!(target: "mod", "[{mod_id}] {message}"),
    }
}
