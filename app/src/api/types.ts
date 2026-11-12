// Domain types for the UI. These mirror what mcpack-core exposes through its tools
// (`mcpack call <tool>` / MCP `tools/call`). Field names follow the backend's JSON.

export type Loader = "fabric" | "quilt" | "forge" | "neoforge" | "vanilla";

export interface Pack {
  /** Stable id — the pack directory. */
  id: string;
  name: string;
  path: string;
  minecraft: string;
  loader: Loader;
  loaderVersion: string;
  profiles: string[];
  /** pack.lock matches pack.toml? */
  lock: "synced" | "stale";
  pendingChanges: number;
  /** File watcher (hot reload) active. */
  watch: boolean;
  /** pack.toml `dev.hotswap`: launch with the debug agent so changed mod code can be hot-swapped. */
  hotswap?: boolean;
  /** pack.toml `dev.restart`: what the watcher does when a change needs a restart. */
  restartPolicy?: RestartPolicy;
  /** pack.toml `dev.config_reload`: how config changes reach a running game. */
  configReload?: ConfigReload;
}

export type RestartPolicy = "ask" | "auto";
export type ConfigReload = "restart" | "resources" | "none";
export type DevSetting = { key: "hotswap"; value: boolean } | { key: "restart"; value: RestartPolicy } | { key: "config_reload"; value: ConfigReload };

/** What the hot-reload watcher did with one batch of file changes (core `WatchEvent`). */
export interface WatchEvent {
  seq: number;
  at_ms: number;
  pack: string;
  profile: string;
  /** Changed files, relative to the pack (absolute when outside it). */
  paths: string[];
  action: "applying" | "no_change" | "synced" | "live_reload" | "hotswapped" | "restart_needed" | "restarted" | "error";
  session?: string;
  live_actions?: ("reload_resources" | "reload_datapacks")[];
  restart_reasons?: string[];
  hotswap?: { mods: string[]; classes: string[]; not_loaded?: string[]; pending_resources?: string[] };
  hotswap_failed?: string;
  error?: string;
  sync?: { added?: string[]; removed?: string[]; updated?: string[]; config_changed?: string[]; overrides_changed?: string[]; held?: string[]; warnings?: string[] };
}

export type ContentKind = "mod" | "resourcepack" | "shaderpack" | "datapack";
export type ContentSource = "modrinth" | "curseforge" | "local";

export interface ContentEntry {
  slug: string;
  name: string;
  kind: ContentKind;
  source: ContentSource;
  version: string;
  side: "client" | "server" | "both";
  /** Pulled in by another entry rather than declared in pack.toml. */
  dependency: boolean;
  /** Newer compatible version, if any. */
  update?: string;
  size: number;
}

export interface SearchHit {
  slug: string;
  name: string;
  kind: ContentKind;
  source: ContentSource;
  description: string;
  downloads: number;
  latest: string;
  author?: string | null;
  /** Project page on Modrinth / CurseForge. */
  url?: string | null;
}

/** Enough to name a project when adding it or listing its versions (a search hit or an installed entry). */
export interface ContentRef {
  slug: string;
  name?: string;
  kind: ContentKind;
  source: ContentSource;
}

/** One published version of a project (core `VersionSummary`). */
export interface VersionSummary {
  id: string;
  version_number: string;
  /** release | beta | alpha */
  version_type: string;
  game_versions: string[];
  loaders: string[];
  date: string;
  filename?: string | null;
}

/** A mod (or resource pack, ...) that is not from Modrinth/CurseForge: a local file or a direct download URL. */
export interface CustomContent {
  kind: ContentKind;
  path?: string;
  url?: string;
}

export type InstancePhase = "resolve" | "download" | "java" | "launch" | "agent";
export const PHASES: { id: InstancePhase; label: string }[] = [
  { id: "resolve", label: "Resolve" },
  { id: "download", label: "Download" },
  { id: "java", label: "Java" },
  { id: "launch", label: "Start game" },
  { id: "agent", label: "Agent" },
];

export type PackFormat = "mrpack" | "curseforge";
export const PACK_FORMATS: { value: PackFormat; label: string; ext: string }[] = [
  { value: "mrpack", label: "Modrinth (.mrpack)", ext: "mrpack" },
  { value: "curseforge", label: "CurseForge (.zip)", ext: "zip" },
];

export interface ExportReport {
  path: string;
  files: number;
  overrides: number;
  warnings: string[];
}

export interface ImportResult {
  pack: Pack;
  warnings: string[];
}

export interface InstanceState {
  /** "preparing" covers everything from clicking Launch to the title screen; "stopping" is set while a stop is in flight. */
  status: "stopped" | "preparing" | "running" | "stopping" | "crashed";
  profile: string;
  phase?: InstancePhase;
  /** 0..1 within the current phase, when known. */
  progress?: number;
  detail?: string;
  startedAt?: number;
  pid?: number;
  agentPort?: number;
  /** Set when the session was launched with hot-swap (JVM debug agent port). */
  hotswapPort?: number;
  exitCode?: number;
}

export type LogLevel = "debug" | "info" | "warn" | "error";
export interface LogLine {
  id: number;
  t: number;
  level: LogLevel;
  source: string;
  text: string;
}

/** Shape returned by the agent mod's `status` method. */
export interface AgentStatus {
  minecraft: string;
  mods_loaded: number;
  screen: string | null;
  screen_title: string | null;
  title_screen: boolean;
  loading: boolean;
  in_world: boolean;
  singleplayer: boolean;
  fps: number;
  world_name?: string;
  level_id?: string;
  position?: { x: number; y: number; z: number };
  dimension?: string;
  health?: number;
}

export type Origin = "mcp" | "cli" | "app";
export interface ActivityEvent {
  id: string;
  t: number;
  origin: Origin;
  /** e.g. "claude-code" for MCP clients. */
  client?: string;
  tool: string;
  packId?: string;
  summary: string;
  status: "running" | "ok" | "error";
  durationMs?: number;
  /** Stable code from mcpack-core's Error::code(). */
  errorCode?: string;
  params?: unknown;
  result?: unknown;
}

export interface GlobalConfig {
  curseforge_api_key: string | null;
  username: string;
  download_concurrency: number;
  java_path: string | null;
}

export interface BackendInfo {
  version: string;
  home: string;
  /** "mock" while the UI runs without the real core. */
  mode: "mock" | "native";
}

export type BackendEvent =
  | { type: "packs"; packs: Pack[] }
  | { type: "watch"; packId: string; event: WatchEvent }
  | { type: "content"; packId: string; content: ContentEntry[] }
  | { type: "instance"; packId: string; state: InstanceState }
  | { type: "log"; packId: string; lines: LogLine[] }
  | { type: "agent"; packId: string; status: AgentStatus | null }
  | { type: "activity"; event: ActivityEvent };

export class BackendError extends Error {
  constructor(
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

/** Everything the UI needs from the launcher. One implementation per transport. */
export interface Backend {
  info(): Promise<BackendInfo>;
  listPacks(): Promise<Pack[]>;
  createPack(input: { name: string; minecraft: string; loader: Loader }): Promise<Pack>;
  content(packId: string): Promise<ContentEntry[]>;
  search(packId: string, query: string, kind: ContentKind): Promise<SearchHit[]>;
  /** Add a project, or re-pin one that is already in the pack. Without `version`: latest compatible. */
  addContent(packId: string, item: ContentRef, version?: string): Promise<void>;
  /** Versions compatible with the pack's Minecraft version and loader, newest first. */
  versions(packId: string, item: ContentRef): Promise<VersionSummary[]>;
  /** Ask for a local .jar / .zip to add. null = cancelled. */
  pickContentFile(): Promise<string | null>;
  addCustom(packId: string, input: CustomContent): Promise<void>;
  /** Remove a pack from the launcher; `files` also deletes its folder (only for packs under <home>/packs). */
  deletePack(packId: string, files: boolean): Promise<void>;
  /** Open a web page in the default browser. */
  openUrl(url: string): Promise<void>;
  removeContent(packId: string, slug: string): Promise<void>;
  updateContent(packId: string, slug: string): Promise<void>;
  resolve(packId: string): Promise<void>;
  setWatch(packId: string, on: boolean, profile?: string): Promise<void>;
  /** Change a pack.toml `[dev]` setting. */
  setDev(packId: string, setting: DevSetting): Promise<void>;
  /** Restart the running game with re-synced files, rejoining the current world. */
  restart(packId: string, profile: string): Promise<void>;
  /** Ask for a modpack archive to import. null = cancelled. */
  pickImportFile(): Promise<string | null>;
  importPack(file: string): Promise<ImportResult>;
  /** Ask where to save an export. null = cancelled. */
  pickExportPath(defaultName: string, format: PackFormat): Promise<string | null>;
  exportPack(packId: string, profile: string, format: PackFormat, out: string): Promise<ExportReport>;
  /** Current state of every pack that has (or had) a session — includes games started by other processes. */
  instances(): Promise<Record<string, InstanceState>>;
  launch(packId: string, profile: string): Promise<void>;
  stop(packId: string): Promise<void>;
  /** Call the in-game agent (status, command, screenshot, create_world, ...). */
  agent<T = unknown>(packId: string, method: string, params?: Record<string, unknown>): Promise<T>;
  activity(): Promise<ActivityEvent[]>;
  getConfig(): Promise<GlobalConfig>;
  saveConfig(cfg: GlobalConfig): Promise<void>;
  subscribe(fn: (e: BackendEvent) => void): () => void;
}
