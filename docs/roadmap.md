# Roadmap

Garnet already handles login (online mode, Velocity and BungeeCord), the
configuration handshake, chunk streaming, movement, chat, the vanilla
command set, block breaking and placing, sky and block lighting, item
entities, health, hunger and combat, saves, mods, the admin panel, voice
relaying and anti-cheat.

What comes next, roughly in order:

1. **Containers and crafting** – chests and other block containers, the crafting grid and furnaces (dropped items, inventories, placing and block drops are done).
2. **Block behaviours** – gravity blocks, doors, beds, torches, fluids.
3. **Entities** – passive mobs with simple AI, then hostile mobs (items on the ground are done).
4. **Vanilla-compatible terrain** – reproduce Mojang's noise generator from the datapack settings so seeds match.
6. **Redstone**.
7. **Typed mod API** – a WebAssembly component (WIT) world next to the JSON ABI so language SDKs can be generated.
8. **Java plugin bridge** – run Paper/Fabric-style plugins in a sidecar JVM against a shim API.
9. **Minecraft Server Management Protocol** – the vanilla JSON-RPC admin API, so existing tooling works unchanged.

## Lessons taken from other servers

Things other Rust servers have struggled with that Garnet is built to avoid:

- silent config failures → the server refuses to start with the parse error printed
- half-written saves → every file is written to a temp path and renamed
- memory growth on an idle server → idle chunks unload and region files close
- proxy forwarding that breaks → Velocity modern and BungeeCord legacy forwarding are built in
- `ops.json` ignored in offline mode → ops are matched by the offline UUID
- VarInt decoders accepting overflow → rejected as malformed
- plugin APIs that panic when a command is registered during load → registration works at any time
- stale Docker images → images are built per tag and for amd64 and arm64
