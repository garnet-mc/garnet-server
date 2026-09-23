# Roadmap

Garnet already handles login (online mode, Velocity and BungeeCord), the
configuration handshake, chunk streaming, movement, chat, the vanilla
command set, block breaking and placing, sky and block lighting, item
entities, health, hunger and combat, chests, crafting and furnaces,
mobs that spawn, wander and fight back, doors, beds, falling blocks,
growing crops, flowing water and lava, redstone from wire and torches
through comparators, observers, pistons and TNT, hoppers, experience
and tools that wear out, anvils, enchanting tables and brewing stands,
bows and the rest of what flies, skeletons, creepers, mobs that find
their way round what is in it, a dawn that burns the undead, animals
that breed and shear and follow the food, saves, mods, the admin panel,
voice relaying and anti-cheat.

What comes next, roughly in order:

1. **The rest of the cast** – endermen, witches, drowned, husks and strays, and the animals beyond the four farm ones (breeding, shearing, milking and following are done for cows, sheep, pigs and chickens).
2. **The rest of the workbenches** – the stonecutter, the grindstone and the smithing table (chests, the crafting grid, furnaces, anvils, enchanting tables and brewing stands are done).
3. **Rails and minecarts** – and the rest of what rides them (hoppers and the rest of redstone are done: wire, torches, levers, buttons, plates, repeaters, comparators, observers, pistons, dispensers, droppers, note blocks, lamps, doors and TNT).
3. **More mobs** – skeletons and creepers want arrows and explosions first, and everything wants proper pathfinding, sounds and experience orbs.
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
