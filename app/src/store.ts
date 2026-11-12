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
} from "./api";

export type View = "pack" | "activity" | "settings";
export type PackTab = "overview" | "content" | "console" | "agent";
export type Theme = "system" | "dark" | "light";
export type Dialog = null | "palette" | "add-content" | "new-pack" | "export-pack";

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
  repl: Record<string, ReplEntry[] | undefined>;
  shots: Record<string, Shot | undefined>;
  activity: ActivityEvent[];
  config: GlobalConfig | null;
  toasts: Toast[];
  /** A modpack import is in flight. */
  importing: boolean;

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
  repl: {},
  shots: {},
  activity: [],
  config: null,
  toasts: [],
  importing: false,

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

export function agentAction(pack: Pack, method: string, params: Record<string, unknown> = {}, ok?: string) {
  return useStore.getState().run(backend.agent(pack.id, method, params), ok);
}
