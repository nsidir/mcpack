# mcpack

Automation-first Minecraft launcher. Rust workspace (`crates/`), Fabric agent mod (`agent-mod/`, Java/Gradle),
desktop UI (`app/`, Tauri 2 + React; its `src-tauri` is a standalone crate).

- All front ends go through `crates/mcpack-core/src/tools.rs` (`definitions()` + `call()`); add new actions there.
- Errors: `mcpack_core::Error` with stable `code()`; tool results are JSON.
- `cargo test` (core unit tests), `cargo build`; app: `cd app && pnpm tauri dev`.
- Agent mod: `cd agent-mod && ./gradlew dist` (Gradle 8.14 + Loom 1.11, Java 21; build output embedded by core's build.rs).
- Supported: Fabric/Quilt; agent for MC 1.20.1 and 1.21.1. Forge/NeoForge intentionally not implemented yet.
