import { create } from "zustand";
import {
  type ActivityEvent,
  type AgentStatus,
  backend,
  type BackendInfo,
  BackendError,
  type ContentEntry,
  type GlobalConfig,
  type InstanceState,
  type LogLine,
  type Pack,
  type WatchEvent,
} from "./api";

export type View = "pack" | "activity" | "settings";
export type PackTab = "overview" | "content" | "console" | "agent";
export type Theme = "system" | "dark" | "light";
export type Dialog = null | "palette" | "add-content" | "new-pack" | "export-pack" | "versions" | "delete-pack";

export interface Toast {
  id: number;
  kind: "ok" | "error" | "info";
  text: string;
  code?: string;
}

export interface ReplEntry {
  id: number;
  command: string;
  output: string[];
  error: boolean;
  pending: boolean;
}

export interface Shot {
  image?: string;
  path: string;
  t: number;
}

const MAX_LOG = 2000;
const MAX_WATCH = 40;

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** One-line summary of a watcher event (the change feed and toasts use it). */
export function describeWatch(e: WatchEvent): string {
  switch (e.action) {
    case "applying":
      return "Applying changes…";
    case "no_change":
      return "No effective change";
    case "synced":
      return e.session ? "Files updated" : "Files updated for the next launch";
    case "live_reload": {
      const what = (e.live_actions ?? []).map((a) => (a === "reload_resources" ? "resource packs" : "data packs"));
      return `Reloaded ${what.join(" and ")} in the game`;
    }
    case "hotswapped": {
      const h = e.hotswap;
      return h ? `Hot-swapped ${plural(h.classes.length, "class")} from ${h.mods.join(", ")}` : "Hot-swapped mod code";
    }
    case "restart_needed":
      return "Restart needed to apply";
    case "restarted":
      return "Restarted the game to apply";
    case "error":
      return e.error ?? "Hot reload failed";
  }
}

function watchToast(e: WatchEvent): { kind: Toast["kind"]; text: string } | null {
  switch (e.action) {
    case "hotswapped":
    case "live_reload":
      return { kind: "ok", text: describeWatch(e) };
    case "restart_needed":
      return { kind: "info", text: `Restart needed: ${e.restart_reasons?.[0] ?? "changes need a restart"}` };
    case "restarted":
      return { kind: "info", text: `Restarted the game: ${e.restart_reasons?.[0] ?? "changes needed a restart"}` };
    case "error":
      return { kind: "error", text: `Hot reload: ${e.error ?? "failed"}` };
    default:
      return null;
  }
}

const saved = <T,>(key: string, fallback: T): T => {
  try {
    const v = localStorage.getItem(`mcpack.${key}`);
    return v == null ? fallback : (JSON.parse(v) as T);
  } catch {
    return fallback;
  }
};
const save = (key: string, value: unknown) => {
  try {
    localStorage.setItem(`mcpack.${key}`, JSON.stringify(value));
  } catch {
    /* storage unavailable — preferences just won't persist */
  }
};

interface State {
  ready: boolean;
  info: BackendInfo | null;
  packs: Pack[];
  packId: string | null;
  view: View;
  tab: PackTab;
  theme: Theme;
  dialog: Dialog;
  content: Record<string, ContentEntry[] | undefined>;
  instances: Record<string, InstanceState | undefined>;
  logs: Record<string, LogLine[] | undefined>;
  agent: Record<string, AgentStatus | null | undefined>;
  /** Hot-reload watcher events per pack, newest first. */
  watchEvents: Record<string, WatchEvent[] | undefined>;
  repl: Record<string, ReplEntry[] | undefined>;
  shots: Record<string, Shot | undefined>;
  activity: ActivityEvent[];
  config: GlobalConfig | null;
  toasts: Toast[];
  /** A modpack import is in flight. */
  importing: boolean;
  /** Installed entry whose version is being chosen (dialog "versions"). */
  versionTarget: ContentEntry | null;
  pickVersion(entry: ContentEntry): void;

  init(): Promise<void>;
  selectPack(id: string): void;
  setView(v: View): void;
  setTab(t: PackTab): void;
  setTheme(t: Theme): void;
  open(d: Dialog): void;
  toast(kind: Toast["kind"], text: string, code?: string): void;
  dismiss(id: number): void;
  clearLogs(packId: string): void;
  loadContent(packId: string): Promise<void>;
  saveConfig(cfg: GlobalConfig): Promise<void>;
  /** Run a backend action; failures surface as a toast with the stable error code. */
  run<T>(action: Promise<T>, ok?: string): Promise<T | undefined>;
  runCommand(packId: string, command: string): Promise<void>;
  screenshot(packId: string): Promise<void>;
}

let toastId = 0;
let replId = 0;

export const useStore = create<State>((set, get) => ({
  ready: false,
  info: null,
  packs: [],
  packId: saved<string | null>("pack", null),
  view: "pack",
  tab: saved<PackTab>("tab", "overview"),
  theme: saved<Theme>("theme", "system"),
  dialog: null,
  content: {},
  instances: {},
  logs: {},
  agent: {},
  watchEvents: {},
  repl: {},
  shots: {},
  activity: [],
  config: null,
  toasts: [],
  importing: false,
  versionTarget: null,
  pickVersion: (entry) => set({ versionTarget: entry, dialog: "versions" }),

  async init() {
    backend.subscribe((e) => {
      switch (e.type) {
        case "packs":
          set({ packs: e.packs });
          break;
        case "content":
          set((s) => ({ content: { ...s.content, [e.packId]: e.content } }));
          break;
        case "instance":
          set((s) => ({ instances: { ...s.instances, [e.packId]: e.state } }));
          break;
        case "watch": {
          set((s) => {
            const list = s.watchEvents[e.packId] ?? [];
            // The outcome replaces its own "applying" entry (seq restarts with each watch).
            const i = list.findIndex((x) => x.seq === e.event.seq && x.action === "applying");
            const next = i >= 0 ? list.map((x, j) => (j === i ? e.event : x)) : [e.event, ...list];
            return { watchEvents: { ...s.watchEvents, [e.packId]: next.slice(0, MAX_WATCH) } };
          });
          const note = watchToast(e.event);
          if (note) get().toast(note.kind, note.text);
          break;
        }
        case "agent":
          set((s) => ({ agent: { ...s.agent, [e.packId]: e.status } }));
          break;
        case "log":
          set((s) => {
            const next = [...(s.logs[e.packId] ?? []), ...e.lines];
            return { logs: { ...s.logs, [e.packId]: next.length > MAX_LOG ? next.slice(-MAX_LOG) : next } };
          });
          break;
        case "activity":
          set((s) => {
            const i = s.activity.findIndex((a) => a.id === e.event.id);
            const activity = i >= 0 ? s.activity.map((a) => (a.id === e.event.id ? e.event : a)) : [e.event, ...s.activity];
            return { activity };
          });
          break;
      }
    });
    // Games that were already running before this window opened (started earlier, by the CLI or by
    // an agent). Events that arrived in the meantime are newer, so they win.
    void backend
      .instances()
      .then((snapshot) => set((s) => ({ instances: { ...snapshot, ...s.instances } })))
      .catch(() => undefined);
    try {
      const [info, packs, activity, config] = await Promise.all([
        backend.info(),
        backend.listPacks(),
        backend.activity(),
        backend.getConfig(),
      ]);
      const current = get().packId;
      const packId = packs.some((p) => p.id === current) ? current : (packs[0]?.id ?? null);
      set({ ready: true, info, packs, activity, config, packId });
      if (packId) void get().loadContent(packId);
    } catch (err) {
      set({ ready: true });
      get().toast("error", err instanceof Error ? err.message : String(err));
    }
  },

  selectPack(id) {
    save("pack", id);
    set({ packId: id, view: "pack" });
    if (!get().content[id]) void get().loadContent(id);
  },
  setView: (view) => set({ view }),
  setTab(tab) {
    save("tab", tab);
    set({ tab, view: "pack" });
  },
  setTheme(theme) {
    save("theme", theme);
    set({ theme });
  },
  open: (dialog) => set({ dialog }),

  toast(kind, text, code) {
    const id = ++toastId;
    set((s) => ({ toasts: [...s.toasts.slice(-3), { id, kind, text, code }] }));
    setTimeout(() => get().dismiss(id), kind === "error" ? 8000 : 3500);
  },
  dismiss: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),

  clearLogs: (packId) => set((s) => ({ logs: { ...s.logs, [packId]: [] } })),

  async loadContent(packId) {
    const content = await get().run(backend.content(packId));
    if (content) set((s) => ({ content: { ...s.content, [packId]: content } }));
  },

  async saveConfig(cfg) {
    const prev = get().config;
    set({ config: cfg });
    try {
      await backend.saveConfig(cfg);
    } catch (err) {
      set({ config: prev });
      get().toast("error", err instanceof Error ? err.message : String(err));
    }
  },

  async run(action, ok) {
    try {
      const r = await action;
      if (ok) get().toast("ok", ok);
      return r;
    } catch (err) {
      if (err instanceof BackendError) get().toast("error", err.message, err.code);
      else get().toast("error", err instanceof Error ? err.message : String(err));
      return undefined;
    }
  },

  async runCommand(packId, command) {
    const id = ++replId;
    const patch = (p: Partial<ReplEntry>) =>
      set((s) => ({ repl: { ...s.repl, [packId]: (s.repl[packId] ?? []).map((r) => (r.id === id ? { ...r, ...p } : r)) } }));
    set((s) => ({
      repl: { ...s.repl, [packId]: [...(s.repl[packId] ?? []).slice(-99), { id, command, output: [], error: false, pending: true }] },
    }));
    try {
      const r = await backend.agent<{ output: string[] }>(packId, "command", { command });
      patch({ output: r.output, pending: false });
    } catch (err) {
      patch({ output: [err instanceof Error ? err.message : String(err)], error: true, pending: false });
    }
  },

  async screenshot(packId) {
    const r = await get().run(backend.agent<{ path: string; image?: string }>(packId, "screenshot", {}));
    if (r) set((s) => ({ shots: { ...s.shots, [packId]: { ...r, t: Date.now() } } }));
  },
}));

export const usePack = () => useStore((s) => s.packs.find((p) => p.id === s.packId) ?? null);

// ---- shared actions (used by buttons, the command palette and keyboard shortcuts)

export function profileOf(pack: Pack): string {
  return useStore.getState().instances[pack.id]?.profile ?? pack.profiles[0];
}

export function setProfile(pack: Pack, profile: string) {
  useStore.setState((s) => ({ instances: { ...s.instances, [pack.id]: { status: "stopped", ...s.instances[pack.id], profile } } }));
}

export const isActive = (i?: InstanceState) => i?.status === "running" || i?.status === "preparing" || i?.status === "stopping";

/** Short label for an instance's state, used wherever the state is shown. */
export function stateLabel(i?: InstanceState): string {
  switch (i?.status) {
    case "preparing":
      return "Launching";
    case "running":
      return "Running";
    case "stopping":
      return "Stopping";
    case "crashed":
      return "Crashed";
    default:
      return "Not running";
  }
}

const setInstance = (packId: string, state: InstanceState) => useStore.setState((s) => ({ instances: { ...s.instances, [packId]: state } }));

export async function launch(pack: Pack) {
  const s = useStore.getState();
  if (isActive(s.instances[pack.id])) return;
  const profile = profileOf(pack);
  // Show "Launching" right away; the backend's progress events take over from here.
  setInstance(pack.id, { status: "preparing", profile, phase: "resolve", detail: "Preparing to launch" });
  try {
    await backend.launch(pack.id, profile);
  } catch (err) {
    const cur = useStore.getState().instances[pack.id];
    if (cur?.status === "preparing" && cur.pid == null) setInstance(pack.id, { status: "stopped", profile });
    if (err instanceof BackendError) s.toast("error", err.message, err.code);
    else s.toast("error", err instanceof Error ? err.message : String(err));
  }
}

export async function stop(pack: Pack) {
  const s = useStore.getState();
  const prev = s.instances[pack.id];
  if (!prev || prev.status === "stopping") return;
  setInstance(pack.id, { ...prev, status: "stopping" });
  try {
    await backend.stop(pack.id);
  } catch (err) {
    if (useStore.getState().instances[pack.id]?.status === "stopping") setInstance(pack.id, prev);
    if (err instanceof BackendError) s.toast("error", err.message, err.code);
    else s.toast("error", err instanceof Error ? err.message : String(err));
  }
}

/** Pick a .mrpack / CurseForge .zip and import it as a new pack. */
export async function importPack() {
  const s = useStore.getState();
  if (s.importing) return;
  const file = await s.run(backend.pickImportFile());
  if (!file) return;
  useStore.setState({ importing: true });
  s.toast("info", `Importing ${file.split(/[\\/]/).pop()}…`);
  const r = await s.run(backend.importPack(file));
  useStore.setState({ importing: false });
  if (!r) return;
  useStore.setState((st) => ({ packs: st.packs.some((p) => p.id === r.pack.id) ? st.packs : [...st.packs, r.pack] }));
  s.selectPack(r.pack.id);
  s.toast("ok", `Imported ${r.pack.name}`);
  if (r.warnings.length) s.toast("info", r.warnings.length === 1 ? r.warnings[0] : `${r.warnings[0]} (+${r.warnings.length - 1} more warnings, see Activity)`);
}

/** The latest watcher outcome says a restart is needed and the current session predates it. */
export function pendingRestart(events: WatchEvent[] | undefined, instance: InstanceState | undefined): WatchEvent | null {
  if (instance?.status !== "running") return null;
  const last = events?.find((e) => e.action !== "applying" && e.action !== "no_change");
  if (last?.action !== "restart_needed") return null;
  return instance.startedAt != null && last.at_ms < instance.startedAt ? null : last;
}

/** Restart the running game with re-synced files, rejoining the current world. */
export async function restartGame(pack: Pack) {
  const s = useStore.getState();
  const prev = s.instances[pack.id];
  if (prev) setInstance(pack.id, { ...prev, status: "stopping" });
  const ok = await s.run(backend.restart(pack.id, prev?.profile ?? profileOf(pack)).then(() => true));
  if (!ok && prev && useStore.getState().instances[pack.id]?.status === "stopping") setInstance(pack.id, prev);
}

export function agentAction(pack: Pack, method: string, params: Record<string, unknown> = {}, ok?: string) {
  return useStore.getState().run(backend.agent(pack.id, method, params), ok);
}

/** Can this pack's folder be deleted by the launcher? Only packs it created under <home>/packs. */
export function isManaged(pack: Pack): boolean {
  const home = useStore.getState().info?.home;
  if (!home) return false;
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
  return norm(pack.path).startsWith(`${norm(home)}/packs/`);
}

/** Remove a pack from the launcher (optionally with its folder) and move the selection elsewhere. */
export async function deletePack(pack: Pack, files: boolean): Promise<boolean> {
  const s = useStore.getState();
  try {
    await backend.deletePack(pack.id, files);
  } catch (err) {
    if (err instanceof BackendError) s.toast("error", err.message, err.code);
    else s.toast("error", err instanceof Error ? err.message : String(err));
    return false;
  }
  useStore.setState((st) => {
    const packs = st.packs.filter((p) => p.id !== pack.id);
    const { [pack.id]: _c, ...content } = st.content;
    const { [pack.id]: _i, ...instances } = st.instances;
    const { [pack.id]: _l, ...logs } = st.logs;
    return { packs, content, instances, logs, packId: st.packId === pack.id ? (packs[0]?.id ?? null) : st.packId };
  });
  const next = useStore.getState().packId;
  if (next) useStore.getState().selectPack(next);
  s.toast("ok", files ? `Deleted ${pack.name}` : `Removed ${pack.name} from the launcher`);
  return true;
}

/** Project page of a Modrinth / CurseForge entry, when it can be derived from the slug. */
export function pageUrl(item: { slug: string; kind: string; source: string }): string | null {
  if (item.source === "modrinth") return `https://modrinth.com/project/${encodeURIComponent(item.slug)}`;
  if (item.source === "curseforge" && !/^\d+$/.test(item.slug)) {
    const section = { mod: "mc-mods", resourcepack: "texture-packs", shaderpack: "shaders", datapack: "data-packs" }[item.kind] ?? "mc-mods";
    return `https://www.curseforge.com/minecraft/${section}/${encodeURIComponent(item.slug)}`;
  }
  return null;
}

export function openUrl(url: string) {
  void useStore.getState().run(backend.openUrl(url));
}
