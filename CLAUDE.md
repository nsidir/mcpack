# mcpack

Automation-first Minecraft launcher. Rust workspace (`crates/`), in-game agent (`agent-mod/`, a Java agent, Gradle),
desktop UI (`app/`, Tauri 2 + React; its `src-tauri` is a standalone crate).

- All front ends go through `crates/mcpack-core/src/tools.rs` (`all_definitions()` + `call()`); add new actions there.
  Put each new tool in a `guide::GROUPS` group (a test enforces it). Its description's first sentence is all
  `tools/list` shows agents; the rest is served by `help tool=<name>`.
- Errors: `mcpack_core::Error` with stable `code()`; tool results are JSON.
- `cargo test` (core unit tests), `cargo build`; app: `cd app && pnpm tauri dev`.
- Agent: `cd agent-mod && ./gradlew dist` (Gradle 8.14, JDK 21, emits Java 8 bytecode; `dist/mcpack-agent.jar` is embedded
  by core's build.rs). It is a `-javaagent` driven by reflection with Mojang names; `agent/mappings.rs` writes the
  per-version name mappings. Never touch game classes before `Game.startedUp` (Forge loads them untransformed).
- Versions: never edit by hand; `python scripts/version.py bump patch|minor|major|X.Y.Z` updates all
  eight places (both Cargo.toml, package.json, tauri.conf.json, both Cargo.lock). The agent mod is versioned separately.
- Git hooks (prek, `.pre-commit-config.yaml`; `prek install` once per clone): pre-commit checks version sync,
  API keys, stray archives/binaries, cargo check (workspace + app), app tsc; pre-push also runs `cargo test`.
  Never bypass with `--no-verify`; fix the cause. A CurseForge key never goes in the repo (env `MCPACK_CURSEFORGE_API_KEY`).
- Supported: vanilla, Fabric, Quilt, Forge (1.6+, official installer incl. processors), NeoForge; agent on all loaders
  for MC 1.14.4+ and vanilla/Forge 1.7.10 – 1.12.2. Forge before 1.6 is refused (FML ignores the game dir). New worlds are written by `world.rs` and joined through the agent.
