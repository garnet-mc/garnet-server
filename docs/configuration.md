# Configuration

`garnet.toml` sits next to the server binary (or in `/data` in Docker). It is
created with comments on first start; `garnet default-config` prints a fresh
copy. A file that does not parse stops the server with the reason printed.

## `[server]`

| Key | Default | Notes |
|---|---|---|
| `name` | A Garnet server | Shown in the panel |
| `motd` | &cA Garnet server | Server list text, `&` colour codes |
| `bind` / `port` | 0.0.0.0 / 25565 | |
| `max_players` | 100 | Ops can always join |
| `online_mode` | true | Verify accounts with Mojang. Must be `false` behind a proxy |
| `view_distance` | 10 | 2–32, chunks |
| `simulation_distance` | 8 | |
| `compression_threshold` | 256 | -1 disables compression |
| `minecraft_version` | latest | `latest` or an exact version such as `26.3` |
| `data_dir` | data | Downloaded jars, Java runtimes and extracted data |
| `proxy` | none | `none`, `bungeecord` or `velocity` |
| `velocity_secret` | | Required with `proxy = "velocity"` |
| `whitelist` | false | |
| `spawn_protection` | 16 | Radius only ops can edit; 0 disables |
| `afk_kick_minutes` | 0 | 0 never kicks |
| `code_of_conduct` | | Text players must accept before joining |

## `[world]`

| Key | Default | Notes |
|---|---|---|
| `name` | world | Folder name |
| `seed` | 0 | 0 picks a random seed when the world is created |
| `generator` | noise | `noise` (hills, oceans, trees) or `flat` |
| `gamemode` | survival | Default for new players |
| `difficulty` | normal | |
| `hardcore` | false | |
| `autosave_minutes` | 5 | |
| `chunk_unload_seconds` | 60 | Idle chunks are saved and dropped |
| `daylight_cycle` | true | |

## `[admin]`

The web panel. `public_url` is the address players reach it on (used in the
`/panel` login link and the setup link); set it when the panel sits behind a
reverse proxy, and set `secure_cookies = true` when that proxy speaks HTTPS.

Panel accounts are stored in `panel-users.json` (argon2 hashes). Roles:

- **viewer** – read everything
- **moderator** – kick, ban, message players, mute voice
- **admin** – run commands, manage mods, whitelist, game modes, backups
- **owner** – edit the config, manage panel users, stop the server

## `[rcon]`, `[voice]`, `[anticheat]`, `[backups]`

See the comments in the generated file. Voice uses UDP on `voice.port`
(defaults to the game port). Anti-cheat thresholds are violation points;
`garnet.anticheat.bypass` in `permissions.toml` exempts a player.

## Other files

| File | Format |
|---|---|
| `ops.json`, `whitelist.json`, `banned-players.json`, `banned-ips.json` | vanilla |
| `permissions.toml` | groups, inheritance, prefixes, per-user nodes |
| `server-icon.png` | 64×64 server list icon |
| `logs/audit.jsonl` | one JSON object per admin action |
| `backups/` | zipped worlds |
