# Client mods, without modpacks

A Garnet server can declare the client-side mods it wants players to have.
The Garnet launcher reads that list when someone joins the server and
installs whatever is missing into the instance first, so a player never has
to find, download or update a modpack.

## Declaring mods

In `garnet.toml`:

```toml
[client_mods]
enforce = true                       # kick clients without the required mods
launcher_url = "https://github.com/garnet-mc/garnet-launcher"

[[client_mods.required]]
id = "sodium"                        # Modrinth project slug
name = "Sodium"
version = ""                         # blank = newest version for this Minecraft release

[[client_mods.required]]
id = "my-server-mod"
name = "My Server Mod"
version = "1.4.0"
source = "url"
url = "https://example.com/my-server-mod-1.4.0.jar"
sha512 = "…"                         # verified after download

[[client_mods.optional]]
id = "iris"
name = "Iris Shaders"

[client_mods.shader_pack]
id = "garnet-shaders"
name = "Garnet Shaders"
source = "url"
url = "https://example.com/garnet-shaders.zip"
```

## How it reaches the client

1. **Server list ping** – the status JSON gets a `garnet` object with the lists above. Vanilla clients ignore it; the launcher uses it before the game starts.
2. **Configuration phase** – the server sends the same manifest on the `garnet:mods` plugin channel. A Garnet client answers with what it has installed. With `enforce = true`, a client that cannot show every required mod is kicked with a message naming the mods and linking the launcher.

Optional mods and the shader pack are offered, never enforced.
