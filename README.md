# mcpack

A minimal, automation-first Minecraft launcher. Every action — creating packs and profiles,
adding mods, tweaking settings, launching, testing, hot reloading — is a plain call, so the
same operations work from the **desktop app**, the **`mcpack` CLI**, scripts, or (optionally)
**AI agents over MCP**. Nothing requires AI.

- **Declarative packs**: `pack.toml` is the source of truth, `pack.lock` pins exact files.
- **Automatic dependencies**: adding a mod pulls in its required dependencies (transitively),
  plus dependencies that jars declare but Modrinth/CurseForge metadata misses.
- **Error catching**: `check` validates every jar (missing deps, wrong Minecraft/Java version,
  incompatibilities, duplicates); `diagnose` turns crashes into structured problems;
  `doctor` fixes what it can automatically and repeats until clean.
- **Testing**: `smoke_test` launches the game, waits for the title screen, can create a world,
  run commands and take a screenshot, and returns pass/fail with a diagnosis.
- **Hot reload**: edit the pack while the game runs — resource packs/datapacks reload live,
  rebuilt mods can have their code hot-swapped into the running game (`dev.hotswap`), and
  changes that need a restart are reported with the reason (or restart the game and rejoin
  your world automatically with `dev.restart = "auto"`).
- Fabric and Quilt on Minecraft 1.20.1 and 1.21.1 (in-game agent); vanilla on any version.
  Modrinth and CurseForge (needs an API key). Offline accounts.

## Layout

| Path | What |
| --- | --- |
| `crates/mcpack-core` | The launcher library. `tools.rs` is the single tool surface for all front ends |
| `crates/mcpack-mcp` | The `mcpack` binary: CLI (`call`, `dev`, `tools`) and MCP server (`serve`) |
| `agent-mod/` | Tiny Fabric mod injected into test launches: status, commands, screenshots, worlds, live reload |
| `app/` | Desktop UI (Tauri 2 + React) linking mcpack-core in-process |
| `examples/` | Example `pack.toml` |

## Run it

Prerequisites: Rust (stable), Node.js + pnpm (for the app). Java is downloaded automatically.

```sh
# Desktop app
cd app
pnpm install
pnpm tauri dev

# CLI
cargo build --release            # -> target/release/mcpack(.exe)
mcpack call pack_init dir=./mypack minecraft=1.21.1 loader=fabric
mcpack call pack_add pack=./mypack id=sodium
mcpack call smoke_test pack=./mypack profile=test
mcpack dev ./mypack              # launch + hot reload + live logs
mcpack tools                     # list every tool
```

### Optional: AI agents (MCP)

`mcpack serve` is an MCP server over stdio exposing every tool. For Claude Code, this repo's
`.mcp.json` registers it (run `cargo build --release` first). Anything an agent does shows up
live in the app's Activity view, and games it launches appear in the app too.

## pack.toml

```toml
[pack]
name = "my-pack"
minecraft = "1.21.1"
loader = { kind = "fabric", version = "latest" }

[jvm]
memory = "4G"

[options]                       # options.txt
renderDistance = 12

[config."config/sodium-options.json"]   # patch mod configs (json/toml/properties/txt)
"quality.weather_quality" = "FAST"

[[mods]]
id = "sodium"                   # Modrinth slug (default source)

[[mods]]
source = "curseforge"
id = "jei"

[[mods]]
source = "file"                 # a mod you're developing: hot-reloads when rebuilt
path = "../mymod/build/libs/mymod.jar"

[profiles.test]                 # profiles overlay the base pack
agent = true
options = { renderDistance = 4 }
disable = ["iris"]

[dev]                           # hot reload (watch_start / mcpack dev / the app's Hot reload switch)
restart = "ask"                 # "ask" (report restart_needed) or "auto" (restart + rejoin the world)
config_reload = "restart"       # or "resources" for mods that reload configs with F3+T, or "none"
hotswap = true                  # launch with a local JVM debug port; rebuilt mods' changed method
                                # bodies are swapped in without restarting
```

### Hot reload: what applies how

| Change | Running game |
| --- | --- |
| Resource packs, data packs (`overrides/`, content) | Reloaded live (`F3+T` / `/reload` via the agent) |
| Configs | Restart, or a resource reload with `config_reload = "resources"` |
| Rebuilt mod, code inside existing methods | Hot-swapped with `hotswap = true` (JDWP class redefinition) |
| Rebuilt mod adding methods/fields/classes, mixins, `fabric.mod.json` | Restart (the JetBrains Runtime as `jvm.java` allows more to be hot-swapped) |
| Mods added or removed, JVM/loader/Minecraft settings, `options.txt` | Restart: Fabric loads mods and applies mixins only at startup |

Hot-swapped mod jars are written to the instance at the next launch (a running game keeps
its mod jars open). Classes the game has not loaded yet and assets inside a swapped jar keep
their old version until then.

Files in `overrides/` (and `profiles/<name>/overrides/`) are copied into the instance.
Instances live in `<pack>/run/<profile>`.

## Data

`MCPACK_HOME` (default `%APPDATA%\mcpack` / `~/.local/share/mcpack`): shared caches,
`config.toml`, `sessions/` (running games, visible across processes), `activity.jsonl`
(journal of every tool call), `packs.json` (known packs).

## Rebuilding the agent mod

```sh
cd agent-mod && ./gradlew dist    # writes dist/mcpack-agent-<mc>.jar, embedded at cargo build
```
