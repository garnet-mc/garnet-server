# Writing mods

Garnet mods are WebAssembly modules. They run sandboxed (a crashing or
looping mod is disabled, never the server) and talk to the server through a
small JSON API, so any language with a WebAssembly target and a JSON library
can be used.

## Layout

```
mods/
  my-mod/
    mod.toml     # manifest
    mod.wasm     # the module
    data.json    # written by the server for StoreData/LoadData
```

`mod.toml`:

```toml
id = "my-mod"           # lowercase, used in logs and permissions
name = "My Mod"
version = "1.0.0"
authors = ["you"]
description = "Does things"
api_version = 1         # optional, defaults to the current API
depends = []            # ids of mods that must load first
ticks = false           # true to receive Event::Tick (20 times a second)
```

Reload mods from the panel or with `/mods` after changing files.

## The API

Everything is defined in the `garnet-api` crate and serialised as JSON:

- **Events** (`Event`) arrive from the server: `server_started`, `server_stopping`, `tick`, `player_login`, `player_join`, `player_quit`, `chat`, `command`, `player_move`, `block_break`, `block_place`, `block_interact`, `game_mode_change`, `timer_fired`, `plugin_message`.
- **Actions** (`Action`) are things the mod asks the server to do: `send_message`, `broadcast`, `action_bar`, `title`, `kick`, `teleport`, `set_game_mode`, `set_block`, `set_time`, `run_command`, `register_command`, `schedule`, `cancel_schedule`, `plugin_message`, `store_data`, `log`.
- **Queries** (`Query`) return an answer immediately: `players`, `player`, `player_by_name`, `block`, `time`, `has_permission`, `load_data`, `server_info`.
- An **EventResult** `{ "cancel": bool, "message": string? }` cancels a cancellable event or rewrites a chat message / sets a kick reason.

Player login, chat, commands, movement, block break/place and block interaction can be cancelled.

## Rust

Add the SDK and implement `Mod`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
garnet-sdk = { git = "https://github.com/garnet-mc/garnet-server", path = "sdk/rust" }
```

```rust
use garnet_sdk::*;

struct Greeter;

impl Mod for Greeter {
    fn init(&mut self) {
        register_command("hello", "Say hello", None);
    }
    fn on_event(&mut self, event: Event) -> EventResult {
        match event {
            Event::PlayerJoin { uuid, name } => {
                send_message(uuid, &format!("&aWelcome, {name}!"));
                EventResult::default()
            }
            Event::Chat { message, .. } if message.contains("creeper") => {
                EventResult::rewrite(message.replace("creeper", "c*****r"))
            }
            Event::Command { command, .. } if command == "hello" => {
                broadcast("&dHello!");
                EventResult::cancelled()
            }
            _ => EventResult::default(),
        }
    }
}

garnet_mod!(Greeter);
```

```
cargo build --release --target wasm32-unknown-unknown
```

The example in `examples/mods/hello` is a complete, working mod.

## Other languages

A module must export:

| Export | Purpose |
|---|---|
| `memory` | linear memory |
| `garnet_alloc(size: i32) -> i32` | allocate a buffer the host writes into; the mod owns it afterwards |
| `garnet_init()` | optional, called once after loading |
| `garnet_event(ptr: i32, len: i32)` | receives a JSON `Event` |

and may import from module `garnet`:

| Import | Purpose |
|---|---|
| `log(level: i32, ptr: i32, len: i32)` | 0 debug, 1 info, 2 warn, 3 error |
| `act(ptr: i32, len: i32)` | a JSON `Action` |
| `query(ptr: i32, len: i32) -> i64` | a JSON `Query`; the answer is written to a buffer from `garnet_alloc` and returned as `(ptr << 32) \| len` |
| `set_result(ptr: i32, len: i32)` | a JSON `EventResult` for the event being handled |

That is the whole contract. Each call into a mod has a fuel budget; a mod that runs too long is stopped and disabled.

## Permissions

Commands registered with a permission node are checked against `permissions.toml`. Nodes are dotted strings with `*` wildcards and a leading `-` for denial. Mods can also ask `has_permission` directly.

## Built-in plugins

Plugins compiled into the server implement `garnet_api::Plugin` and receive the same events through the same host. This is how Garnet's own features are meant to grow.
