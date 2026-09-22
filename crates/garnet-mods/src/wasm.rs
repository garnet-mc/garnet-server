//! The WebAssembly side of the mod runtime.
//!
//! Boundary contract (module name `garnet`), all strings are UTF-8 JSON:
//!
//! Host functions the mod imports:
//! - `log(level: i32, ptr: i32, len: i32)`            0 debug, 1 info, 2 warn, 3 error
//! - `act(ptr: i32, len: i32)`                        a `garnet_api::Action`
//! - `query(ptr: i32, len: i32) -> i64`               a `garnet_api::Query`; the
//!   answer is written into guest memory (allocated with `garnet_alloc`) and
//!   returned as `(ptr << 32) | len`
//! - `set_result(ptr: i32, len: i32)`                 a `garnet_api::EventResult`
//!   for the event currently being handled
//!
//! Functions the mod exports:
//! - `memory`
//! - `garnet_alloc(size: i32) -> i32`                 give the host a buffer
//! - `garnet_init()`                                  optional, after load
//! - `garnet_event(ptr: i32, len: i32)`               a `garnet_api::Event`
//!
//! Keeping it this small means a mod can be written in Rust, C, Zig,
//! AssemblyScript or anything else with a JSON library.

use crate::ModHost;
use anyhow::{bail, Context, Result};
use garnet_api::{Action, Event, EventResult, LogLevel, ModInfo, Query};
use std::path::Path;
use std::sync::Arc;
use wasmtime::{Caller, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

/// How much work one call into a mod may do before it is cut off.
const FUEL_PER_CALL: u64 = 200_000_000;

struct ModState {
    mod_id: String,
    host: Arc<dyn ModHost>,
    /// Filled by `set_result` during `garnet_event`.
    result: Option<EventResult>,
}

pub struct WasmMod {
    store: Store<ModState>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    init: Option<TypedFunc<(), ()>>,
    event: TypedFunc<(i32, i32), ()>,
}

impl WasmMod {
    pub fn load(engine: &Engine, path: &Path, info: ModInfo, host: Arc<dyn ModHost>) -> Result<Self> {
        let module = Module::from_file(engine, path)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("compiling {}", path.display()))?;
        let mut linker: Linker<ModState> = Linker::new(engine);
        link_host_functions(&mut linker)?;

        let mut store = Store::new(
            engine,
            ModState {
                mod_id: info.id.clone(),
                host,
                result: None,
            },
        );
        store.set_fuel(FUEL_PER_CALL)?;
        let instance: Instance = linker
            .instantiate(&mut store, &module)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("instantiating {}", info.id))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .context("mod does not export `memory`")?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "garnet_alloc")
            .map_err(anyhow::Error::from)
            .context("mod does not export `garnet_alloc(size: i32) -> i32`")?;
        let event = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "garnet_event")
            .map_err(anyhow::Error::from)
            .context("mod does not export `garnet_event(ptr: i32, len: i32)`")?;
        let init = instance.get_typed_func::<(), ()>(&mut store, "garnet_init").ok();

        Ok(Self {
            store,
            memory,
            alloc,
            init,
            event,
        })
    }

    pub fn init(&mut self) -> Result<()> {
        if let Some(init) = &self.init {
            self.store.set_fuel(FUEL_PER_CALL)?;
            init.call(&mut self.store, ())
                .map_err(anyhow::Error::from)
                .context("garnet_init trapped")?;
        }
        Ok(())
    }

    pub fn event(&mut self, event: &Event) -> Result<EventResult> {
        let json = serde_json::to_vec(event)?;
        self.store.set_fuel(FUEL_PER_CALL)?;
        let ptr = self.alloc.call(&mut self.store, json.len() as i32)?;
        self.memory
            .write(&mut self.store, ptr as usize, &json)
            .context("mod returned a bad buffer from garnet_alloc")?;
        self.store.data_mut().result = None;
        self.event
            .call(&mut self.store, (ptr, json.len() as i32))
            .map_err(anyhow::Error::from)
            .context("garnet_event trapped (out of fuel or a panic in the mod)")?;
        Ok(self.store.data_mut().result.take().unwrap_or_default())
    }
}

fn link_host_functions(linker: &mut Linker<ModState>) -> Result<()> {
    linker.func_wrap("garnet", "log", |mut caller: Caller<'_, ModState>, level: i32, ptr: i32, len: i32| {
        let text = read_string(&mut caller, ptr, len).unwrap_or_default();
        let level = match level {
            0 => LogLevel::Debug,
            1 => LogLevel::Info,
            2 => LogLevel::Warn,
            _ => LogLevel::Error,
        };
        crate::log_action(&caller.data().mod_id, level, &text);
    })?;

    linker.func_wrap("garnet", "act", |mut caller: Caller<'_, ModState>, ptr: i32, len: i32| {
        let text = read_string(&mut caller, ptr, len).unwrap_or_default();
        match serde_json::from_str::<Action>(&text) {
            Ok(action) => {
                let state = caller.data();
                state.host.act(&state.mod_id, action);
            }
            Err(err) => tracing::warn!("mod {} sent an invalid action: {err} ({text})", caller.data().mod_id),
        }
    })?;

    linker.func_wrap("garnet", "query", |mut caller: Caller<'_, ModState>, ptr: i32, len: i32| -> i64 {
        let text = read_string(&mut caller, ptr, len).unwrap_or_default();
        let result = match serde_json::from_str::<Query>(&text) {
            Ok(query) => {
                let state = caller.data();
                state.host.query(&state.mod_id, query)
            }
            Err(err) => garnet_api::QueryResult::Error {
                message: format!("invalid query: {err}"),
            },
        };
        let json = serde_json::to_vec(&result).unwrap_or_default();
        match write_to_guest(&mut caller, &json) {
            Ok((ptr, len)) => ((ptr as i64) << 32) | (len as i64 & 0xFFFF_FFFF),
            Err(err) => {
                tracing::warn!("could not return query result to mod {}: {err}", caller.data().mod_id);
                0
            }
        }
    })?;

    linker.func_wrap("garnet", "set_result", |mut caller: Caller<'_, ModState>, ptr: i32, len: i32| {
        let text = read_string(&mut caller, ptr, len).unwrap_or_default();
        match serde_json::from_str::<EventResult>(&text) {
            Ok(result) => caller.data_mut().result = Some(result),
            Err(err) => tracing::warn!("mod {} sent an invalid event result: {err}", caller.data().mod_id),
        }
    })?;
    Ok(())
}

fn guest_memory(caller: &mut Caller<'_, ModState>) -> Result<Memory> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .context("mod has no memory export")
}

fn read_string(caller: &mut Caller<'_, ModState>, ptr: i32, len: i32) -> Result<String> {
    let memory = guest_memory(caller)?;
    let (ptr, len) = (ptr as usize, len as usize);
    let data = memory.data(&caller);
    let bytes = data.get(ptr..ptr + len).context("mod passed an out-of-bounds buffer")?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// Allocates in the guest with its `garnet_alloc` and copies `bytes` there.
fn write_to_guest(caller: &mut Caller<'_, ModState>, bytes: &[u8]) -> Result<(i32, i32)> {
    let alloc = caller
        .get_export("garnet_alloc")
        .and_then(|e| e.into_func())
        .context("mod has no garnet_alloc export")?
        .typed::<i32, i32>(&caller)?;
    let ptr = alloc.call(&mut *caller, bytes.len() as i32)?;
    if ptr < 0 {
        bail!("garnet_alloc returned a negative pointer");
    }
    let memory = guest_memory(caller)?;
    memory.write(&mut *caller, ptr as usize, bytes)?;
    Ok((ptr, bytes.len() as i32))
}
