// Native transport: forwards to mcpack-core through the Tauri shell (app/src-tauri/src/main.rs).
//
//   invoke("mcpack_call", { tool, args })  -> result JSON, or rejects with { code, message }
//   event  "mcpack://event"                -> BackendEvent payloads (logs, session state, activity)
//
// `tool` uses the same names as `mcpack call <tool>` / MCP tools, so the UI, the CLI and
// AI agents all drive one surface. This file only adapts result shapes to ./types.ts.

import {
  type ActivityEvent,
  type Backend,
  type BackendEvent,
  type ContentEntry,
  type ExportReport,
  type GlobalConfig,
  type InstanceState,
  type Pack,
  type SearchHit,
  type VersionSummary,
  BackendError,
  PACK_FORMATS,
} from "./types";

interface TauriGlobal {
  core: { invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T>(name: string, fn: (e: { payload: T }) => void): Promise<() => void> };
  /** tauri-plugin-opener */
  opener: { openUrl(url: string): Promise<void> };
  /** tauri-plugin-dialog */
  dialog: {
    open(opts: { title?: string; multiple?: boolean; directory?: boolean; filters?: { name: string; extensions: string[] }[] }): Promise<string | string[] | null>;
    save(opts: { title?: string; defaultPath?: string; filters?: { name: string; extensions: string[] }[] }): Promise<string | null>;
  };
}
const tauri = () => (window as unknown as { __TAURI__: TauriGlobal }).__TAURI__;

async function call<T>(tool: string, args: Record<string, unknown> = {}): Promise<T> {
  try {
    return await tauri().core.invoke<T>("mcpack_call", { tool, args });
  } catch (err) {
    const e = err as { code?: string; message?: string };
    throw new BackendError(e?.code ?? "other", e?.message ?? String(err));
  }
}

/** Raw activity journal entry (core `ActivityEntry`). */
interface RawActivity {
  id: string;
  ts: number;
  origin: ActivityEvent["origin"];
  client?: string;
  tool: string;
  pack?: string;
  args: Record<string, unknown>;
  status: ActivityEvent["status"];
  error_code?: string;
  error?: string;
  duration_ms?: number;
  result?: unknown;
}

function summarize(a: RawActivity): string {
  if (a.error) return a.error.slice(0, 160);
  const arg = (k: string) => (typeof a.args?.[k] === "string" ? (a.args[k] as string) : "");
  const packName = a.pack ? a.pack.split(/[\\/]/).pop() : undefined;
  let s: string;
  switch (a.tool) {
    case "pack_add":
      s = `Add ${arg("id") || arg("slug") || arg("path") || arg("url")}`;
      break;
    case "pack_remove":
      s = `Remove ${arg("id") || arg("slug")}`;
      break;
    case "pack_set":
      s = `Set ${arg("path")} = ${JSON.stringify(a.args?.value)}`;
      break;
    case "launch":
      s = `Launch profile ${arg("profile") || "default"}`;
      break;
    case "game_command":
      s = `/${arg("command").replace(/^\//, "")}`;
      break;
    case "agent_call":
      s = `Agent ${arg("method")}`;
      break;
    default:
      s = a.tool.replace(/_/g, " ");
  }
  return packName ? `${s} · ${packName}` : s;
}

function toActivity(a: RawActivity): ActivityEvent {
  return {
    id: a.id,
    t: a.ts,
    origin: a.origin,
    client: a.client,
    tool: a.tool,
    packId: a.pack,
    summary: summarize(a),
    status: a.status,
    durationMs: a.duration_ms,
    errorCode: a.error_code,
    params: a.args,
    result: a.result,
  };
}

export const nativeBackend: Backend = {
  info: () => call("app_info"),
  listPacks: () => call<Pack[]>("pack_list"),
  async createPack(input) {
    const info = await call<{ dir: string }>("pack_init", { ...input });
    const packs = await call<Pack[]>("pack_list");
    const pack = packs.find((p) => p.id === info.dir);
    if (!pack) throw new BackendError("not_found", `created ${info.dir} but it is not in the pack list`);
    return pack;
  },
  async content(pack) {
    const info = await call<{ content: ContentEntry[] }>("pack_show", { pack });
    return info.content ?? [];
  },
  search: (pack, query, kind) => call<SearchHit[]>("search_mods", { pack, query, kind, limit: 20 }),
  async addContent(pack, item, version) {
    await call("pack_add", {
      pack,
      id: item.slug,
      source: item.source === "local" ? undefined : item.source,
      kind: item.kind,
      version,
    });
  },
  versions: (pack, item) => call<VersionSummary[]>("mod_versions", { pack, id: item.slug, source: item.source }),
  async pickContentFile() {
    const r = await tauri().dialog.open({
      title: "Add a file to the pack",
      multiple: false,
      directory: false,
      filters: [{ name: "Mods and packs (.jar, .zip)", extensions: ["jar", "zip"] }],
    });
    return typeof r === "string" ? r : null;
  },
  async addCustom(pack, input) {
    await call("pack_add", { pack, kind: input.kind, ...(input.url ? { source: "url", url: input.url } : { source: "file", path: input.path }) });
  },
  async deletePack(pack, files) {
    await call("pack_delete", { pack, files });
  },
  async openUrl(url) {
    // The opener capability (src-tauri/capabilities/default.json) only allows https pages.
    if (!/^https:\/\//i.test(url)) throw new BackendError("invalid", `only https pages can be opened: ${url}`);
    try {
      await tauri().opener.openUrl(url);
    } catch (err) {
      // Plugin errors arrive as plain strings.
      throw new BackendError("open_url", `could not open ${url}: ${err instanceof Error ? err.message : String(err)}`);
    }
  },
  async removeContent(pack, slug) {
    await call("pack_remove", { pack, id: slug });
  },
  async updateContent(pack, slug) {
    await call("update", { pack, ids: [slug] });
  },
  async resolve(pack) {
    await call("resolve", { pack });
  },
  async setWatch(pack, on, profile) {
    if (on) {
      await call("watch_start", { pack, profile });
      return;
    }
    // Stop every watch on this pack, whichever profile it was started for.
    const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/$/, "").toLowerCase();
    const watches = await call<{ id: string; pack_dir: string }[]>("watches");
    for (const w of watches.filter((x) => norm(x.pack_dir) === norm(pack))) await call("watch_stop", { watch: w.id });
  },
  async setDev(pack, { key, value }) {
    await call("pack_set", { pack, path: `dev.${key}`, value });
  },
  async restart(pack, profile) {
    await call("restart", { pack, profile });
  },
  async pickImportFile() {
    const r = await tauri().dialog.open({
      title: "Import modpack",
      multiple: false,
      directory: false,
      filters: [{ name: "Modpack (.mrpack, CurseForge .zip)", extensions: ["mrpack", "zip"] }],
    });
    return typeof r === "string" ? r : null;
  },
  async importPack(file) {
    const r = await call<{ pack_dir: string; warnings?: string[] }>("import_pack", { file });
    const packs = await call<Pack[]>("pack_list");
    const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/$/, "").toLowerCase();
    const pack = packs.find((p) => norm(p.id) === norm(r.pack_dir));
    if (!pack) throw new BackendError("not_found", `imported into ${r.pack_dir} but it is not in the pack list`);
    return { pack, warnings: r.warnings ?? [] };
  },
  async pickExportPath(defaultName, format) {
    const f = PACK_FORMATS.find((x) => x.value === format)!;
    return tauri().dialog.save({ title: "Export modpack", defaultPath: `${defaultName}.${f.ext}`, filters: [{ name: f.label, extensions: [f.ext] }] });
  },
  exportPack: (pack, profile, format, out) => call<ExportReport>("export_pack", { pack, profile, format, out }),
  instances: () => tauri().core.invoke<Record<string, InstanceState>>("mcpack_instances"),
  async launch(pack, profile) {
    await call("launch", { pack, profile });
  },
  async stop(pack) {
    await call("stop", { pack });
  },
  agent: (pack, method, params = {}) => call("agent_call", { pack, method, params }),
  async activity() {
    const raw = await call<RawActivity[]>("activity", { limit: 300 });
    return raw.map(toActivity);
  },
  getConfig: () => call<GlobalConfig>("config_get"),
  async saveConfig(config) {
    await call("config_set", { config });
  },
  subscribe(fn) {
    let off: (() => void) | undefined;
    let cancelled = false;
    void tauri()
      .event.listen<BackendEvent>("mcpack://event", (e) => fn(e.payload))
      .then((un) => (cancelled ? un() : (off = un)));
    return () => {
      cancelled = true;
      off?.();
    };
  },
};
