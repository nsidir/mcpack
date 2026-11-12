# mcpack app

Desktop UI for mcpack. Vite + React + TypeScript, wrapped in a Tauri 2 shell.

```sh
pnpm install
pnpm dev          # UI in a browser at http://localhost:5173 (simulated backend)
pnpm tauri dev    # the desktop app (real launcher)
pnpm tauri build  # installer
pnpm build        # type-check + production bundle only
```

## Layout

| Path | What |
| --- | --- |
| `src/api/types.ts` | Domain types and the `Backend` interface — the only thing the UI knows about the launcher |
| `src/api/mock.ts` | Simulated backend (default). Fake packs, a timed launch, a fake in-game agent |
| `src/api/native.ts` | Real transport: Tauri `invoke` + events |
| `src/store.ts` | App state (zustand) and the shared actions used by buttons, palette and shortcuts |
| `src/components/` | Primitives (`ui.tsx`), window chrome (`Shell.tsx`), palette and dialogs (`Dialogs.tsx`) |
| `src/views/` | Pack view and its tabs, Activity, Settings |
| `src/styles.css` | Design tokens (dark + light) and all styles |
| `src-tauri/` | Frameless window shell. Standalone crate, not part of the root workspace |

## Backend

Inside the desktop shell the UI talks to the real launcher: `src-tauri/src/main.rs` links
`mcpack-core` and exposes `mcpack_call(tool, args)` (same tool names as `mcpack call` and MCP;
errors reject with `{ code, message }`) plus the `mcpack://event` stream (instance state with
launch phases and download progress, batched logs, live agent status, pack/content refreshes,
activity). Games and tool calls from other processes, such as an AI agent's `mcpack serve`,
are picked up from `<MCPACK_HOME>/sessions/` and `activity.jsonl`, so they appear here too.

`pnpm dev` in a plain browser tab uses the simulated backend (`src/api/mock.ts`). To force the
simulation inside the shell, set `VITE_MCPACK_BACKEND=mock`.

## Keyboard

`Ctrl+K` command palette · `Ctrl+Enter` launch · `Ctrl+1…4` pack tabs · `Ctrl+,` settings

## Icon

`node scripts/make-icon.mjs && pnpm tauri icon src-tauri/app-icon.png`
