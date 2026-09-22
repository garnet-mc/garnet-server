# Commands

Garnet speaks the vanilla command set. Everything below works from the
in-game chat (ops, or anyone given the `garnet.command.<name>` permission),
the console, the web panel and RCON. Selectors `@a`, `@p`, `@r` and `@s`
work everywhere a player is expected; coordinates accept `~` offsets.

## Chat

| Command | What it does |
| --- | --- |
| `/say <message>` | Broadcast as the server |
| `/me <action>` | `* name action` |
| `/msg`, `/tell`, `/w <player> <message>` | Private message |
| `/teammsg`, `/tm <message>` | Message your team |
| `/tellraw <targets> <json>` | JSON text |
| `/title <targets> title\|subtitle\|actionbar\|times\|clear\|reset ...` | Titles; text can be `&c` legacy or JSON |
| `/playsound <sound> [source] [targets] [x y z] [volume] [pitch]` | Any sound event, including resource pack ones |
| `/stopsound <targets> [source] [sound]` | |
| `/particle <name> [x y z] [dx dy dz] [speed] [count]` | Particles without extra data |

## World

| Command | What it does |
| --- | --- |
| `/time set <day\|night\|noon\|midnight\|ticks>` | |
| `/weather clear\|rain\|thunder [seconds]` | Weather also cycles on its own (`doWeatherCycle`) |
| `/difficulty [level]` | |
| `/defaultgamemode <mode>` | Game mode for players who have never joined |
| `/gamerule [rule] [value]` | Every vanilla rule is stored and sent to clients; the server acts on `doDaylightCycle`, `doWeatherCycle`, `showDeathMessages` today |
| `/setworldspawn [x y z] [yaw]` | |
| `/spawnpoint [targets] [x y z]` | Per-player respawn point, saved with the player |
| `/worldborder get\|set\|add\|center\|damage\|warning ...` | |
| `/tick query\|rate\|freeze\|unfreeze\|step` | Freezing stops the clock and weather |
| `/setblock <x y z> <block[props]>` | |
| `/fill <from> <to> <block> [replace [filter]\|hollow\|outline\|keep]` | Up to 131072 blocks |
| `/clone <from> <to> <dest>` | |
| `/spreadplayers <x> <z> <distance> <range> <targets>` | |
| `/seed`, `/random roll <min..max>` | |
| `/save-all`, `/save-off`, `/save-on`, `/reload`, `/stop` | |
| `/setidletimeout <minutes>` | |

## Players

| Command | What it does |
| --- | --- |
| `/tp`, `/teleport` | `/tp <player> <x> <y> <z> [yaw pitch]` and the usual forms |
| `/rotate <player> <yaw> <pitch>` | |
| `/gamemode <mode> [player]` | |
| `/xp`, `/experience add\|set\|query <targets> [amount] [levels\|points]` | |
| `/kill [targets]` | Death screen and respawn |
| `/damage <target> <amount>` | |
| `/effect give <targets> <effect> [seconds\|infinite] [amplifier] [hideParticles]`, `/effect clear` | Effects expire on the server and survive a rejoin |
| `/attribute <target> <attribute> get\|base get\|base set <value>` | |
| `/spectate [target] [player]` | |
| `/transfer <host> [port] [targets]` | Send players to another server |
| `/kick`, `/ban`, `/ban-ip`, `/pardon`, `/pardon-ip`, `/banlist`, `/op`, `/deop`, `/whitelist` | |
| `/list`, `/tps`, `/spawn`, `/panel`, `/mods` | Garnet extras: `/panel` gives ops a one-time web panel login |

## Scoreboard, teams, boss bars

`/scoreboard objectives add|remove|list|modify|setdisplay`,
`/scoreboard players set|add|remove|reset|get|list`, `/trigger`,
`/team add|remove|join|leave|empty|list|modify`, `/tag <targets> add|remove|list`,
`/bossbar add|remove|set|get|list`. All of it is saved in
`world/garnet-boards.json`; world rules live in `world/garnet-rules.json`.

## `/execute`

`/execute [as <targets>] [at <target>] [positioned <x y z>] [if|unless entity <targets>] run <command>`.
`as` runs the command as each player with the *original* sender's permissions,
like vanilla.

## Items

| Command | What it does |
| --- | --- |
| `/give <targets> <item> [count]` | |
| `/clear [targets] [item] [maxCount]` | `maxCount 0` only counts |
| `/item replace <targets> <slot> with <item> [count]` | Slots: `weapon.mainhand`, `weapon.offhand`, `armor.head/chest/legs/feet`, `hotbar.N`, `inventory.N`, `container.N` |
| `/enchant <targets> <enchantment> [level]` | On the held item |

Inventories are real: survival players pick up what blocks drop (from
Mojang's loot tables, with tool and silk touch/fortune rules), place what they
hold, and keep their inventory across rejoins.

## Data packs

Put packs in `world/datapacks/<name>/` (a folder with `pack.mcmeta`).
`/datapack list|enable|disable`, `/function <ns:name>` (or `#ns:tag`),
`/schedule function <name> <time> [append|replace]`, `/schedule clear`,
`/return <value>|fail`. `#minecraft:load` runs at start and when a pack is
enabled; `#minecraft:tick` runs every tick. Macro lines (`$`) are skipped for
now.

## Not there yet

`loot`, `recipe`, `advancement`, `summon`, `ride`, `locate`, `place`,
`fillbiome`, `data`, `debug`, `jfr`, `perf` are registered so tab completion and `/help` are complete, and
each one says which system it is waiting on (entities, structures, data
packs). They arrive with those systems.
