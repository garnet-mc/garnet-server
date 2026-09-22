//! Write Garnet mods in Rust.
//!
//! ```ignore
//! use garnet_sdk::*;
//!
//! struct MyMod;
//!
//! impl Mod for MyMod {
//!     fn init(&mut self) {
//!         register_command("hello", "Says hello", None);
//!     }
//!     fn on_event(&mut self, event: Event) -> EventResult {
//!         if let Event::PlayerJoin { uuid, name } = event {
//!             send_message(uuid, &format!("&aWelcome, {name}!"));
//!         }
//!         EventResult::default()
//!     }
//! }
//!
//! garnet_mod!(MyMod);
//! ```
//!
//! Build with `cargo build --release --target wasm32-unknown-unknown` and
//! copy the `.wasm` next to a `mod.toml` into the server's `mods/` folder.

pub use garnet_api::*;
use std::cell::RefCell;

/// Implemented by the mod's main type.
pub trait Mod {
    /// Called once after the mod is loaded. Register commands here.
    fn init(&mut self) {}
    fn on_event(&mut self, event: Event) -> EventResult;
}

// ---- host functions the server provides ----

#[link(wasm_import_module = "garnet")]
extern "C" {
    fn log(level: i32, ptr: i32, len: i32);
    fn act(ptr: i32, len: i32);
    fn query(ptr: i32, len: i32) -> i64;
    fn set_result(ptr: i32, len: i32);
}

fn host_log(level: i32, message: &str) {
    unsafe { log(level, message.as_ptr() as i32, message.len() as i32) }
}

/// Sends an [`Action`] to the server.
pub fn perform(action: Action) {
    let json = serde_json::to_vec(&action).expect("action serialises");
    unsafe { act(json.as_ptr() as i32, json.len() as i32) }
}

/// Asks the server a [`Query`].
pub fn ask(q: Query) -> QueryResult {
    let json = serde_json::to_vec(&q).expect("query serialises");
    let packed = unsafe { query(json.as_ptr() as i32, json.len() as i32) };
    if packed == 0 {
        return QueryResult::Error {
            message: "no answer from host".into(),
        };
    }
    let (ptr, len) = ((packed >> 32) as usize, (packed & 0xFFFF_FFFF) as usize);
    let bytes = unsafe { Vec::from_raw_parts(ptr as *mut u8, len, len) };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| QueryResult::Error {
        message: format!("bad answer: {e}"),
    })
}

// ---- convenience wrappers ----

pub fn info(message: &str) {
    host_log(1, message);
}

pub fn warn(message: &str) {
    host_log(2, message);
}

pub fn send_message(player: uuid::Uuid, text: &str) {
    perform(Action::SendMessage {
        player,
        text: text.to_owned(),
    });
}

pub fn broadcast(text: &str) {
    perform(Action::Broadcast { text: text.to_owned() });
}

pub fn register_command(name: &str, description: &str, permission: Option<&str>) {
    perform(Action::RegisterCommand {
        name: name.to_owned(),
        description: description.to_owned(),
        permission: permission.map(str::to_owned),
    });
}

pub fn kick(player: uuid::Uuid, reason: &str) {
    perform(Action::Kick {
        player,
        reason: reason.to_owned(),
    });
}

pub fn players() -> Vec<PlayerInfo> {
    match ask(Query::Players) {
        QueryResult::Players { players } => players,
        _ => Vec::new(),
    }
}

pub fn schedule(id: &str, delay_ticks: u32, repeat: bool) {
    perform(Action::Schedule {
        id: id.to_owned(),
        delay_ticks,
        repeat,
    });
}

// ---- glue between the host ABI and the `Mod` trait ----

thread_local! {
    static INSTANCE: RefCell<Option<Box<dyn Mod>>> = const { RefCell::new(None) };
}

#[doc(hidden)]
pub fn __install(m: Box<dyn Mod>) {
    INSTANCE.with(|slot| *slot.borrow_mut() = Some(m));
}

#[doc(hidden)]
pub fn __init() {
    INSTANCE.with(|slot| {
        if let Some(m) = slot.borrow_mut().as_mut() {
            m.init();
        }
    });
}

#[doc(hidden)]
pub fn __event(ptr: i32, len: i32) {
    let bytes = unsafe { Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize) };
    let Ok(event) = serde_json::from_slice::<Event>(&bytes) else {
        warn("received an event this SDK version does not understand");
        return;
    };
    let result = INSTANCE.with(|slot| slot.borrow_mut().as_mut().map(|m| m.on_event(event)));
    if let Some(result) = result {
        if result.cancel || result.message.is_some() {
            let json = serde_json::to_vec(&result).expect("result serialises");
            unsafe { set_result(json.as_ptr() as i32, json.len() as i32) }
        }
    }
}

/// Buffer the host writes into; it is handed to us and freed by `__event`
/// or `ask` when they take ownership of it.
#[doc(hidden)]
pub fn __alloc(size: i32) -> i32 {
    let mut buf = Vec::<u8>::with_capacity(size as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr as i32
}

/// Declares the mod's entry points. Use once per mod.
#[macro_export]
macro_rules! garnet_mod {
    ($ty:expr) => {
        #[no_mangle]
        pub extern "C" fn garnet_alloc(size: i32) -> i32 {
            $crate::__alloc(size)
        }
        #[no_mangle]
        pub extern "C" fn garnet_init() {
            $crate::__install(Box::new($ty));
            $crate::__init();
        }
        #[no_mangle]
        pub extern "C" fn garnet_event(ptr: i32, len: i32) {
            $crate::__event(ptr, len)
        }
    };
}
