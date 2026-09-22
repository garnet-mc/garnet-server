# Roadmap

Garnet already handles login (online mode, Velocity and BungeeCord), the
configuration handshake, chunk streaming, movement, chat, commands, block
breaking, saves, mods, the admin panel, voice relaying and anti-cheat.

What comes next, roughly in order:

1. **Inventories and items** – container packets, creative slot, block placing with the held item, item drops.
2. **Health and damage** – fall damage, hunger, respawn flow, combat between players.
3. **Block behaviours** – gravity blocks, doors, beds, torches, fluids.
4. **Entities** – item entities, then passive mobs with simple AI, then hostile mobs.
5. **Lighting** – real sky and block light propagation (chunks are currently fully lit).
6. **Vanilla-compatible terrain** – reproduce Mojang's noise generator from the datapack settings so seeds match.
7. **Redstone**.
8. **Typed mod API** – a WebAssembly component (WIT) world next to the JSON ABI so language SDKs can be generated.
9. **Java plugin bridge** – run Paper/Fabric-style plugins in a sidecar JVM against a shim API.
10. **Minecraft Server Management Protocol** – the vanilla JSON-RPC admin API, so existing tooling works unchanged.

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
