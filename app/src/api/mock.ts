// Simulated backend. Lets the whole UI run (and be designed) before mcpack-core's tool
// surface is stable. It behaves like the real thing: async calls, pushed events, stable
// error codes, and an activity record for every action.

import {
  type ActivityEvent,
  type AgentStatus,
  type Backend,
  type BackendEvent,
  BackendError,
  type ContentEntry,
  type ContentKind,
  type GlobalConfig,
  type InstanceState,
  type LogLevel,
  type Pack,
  type SearchHit,
} from "./types";

const listeners = new Set<(e: BackendEvent) => void>();
const emit = (e: BackendEvent) => listeners.forEach((f) => f(e));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
let seq = 0;
const uid = () => `a${(seq++).toString(36)}${Math.random().toString(36).slice(2, 6)}`;

// ---------------------------------------------------------------- seed data

const packs: Pack[] = [
  {
    id: "create-lite",
    name: "Create Lite",
    path: "C:\\packs\\create-lite",
    minecraft: "1.21.1",
    loader: "fabric",
    loaderVersion: "0.16.9",
    profiles: ["client", "test"],
    lock: "synced",
    pendingChanges: 0,
    watch: true,
  },
  {
    id: "vanilla-plus",
    name: "Vanilla Plus",
    path: "C:\\packs\\vanilla-plus",
    minecraft: "1.20.1",
    loader: "fabric",
    loaderVersion: "0.16.9",
    profiles: ["client"],
    lock: "stale",
    pendingChanges: 2,
    watch: false,
  },
  {
    id: "shader-lab",
    name: "Shader Lab",
    path: "C:\\packs\\shader-lab",
    minecraft: "1.21.1",
    loader: "fabric",
    loaderVersion: "0.16.9",
    profiles: ["client", "bench"],
    lock: "synced",
    pendingChanges: 0,
    watch: false,
  },
];

const e = (
  slug: string,
  name: string,
  version: string,
  size: number,
  o: Partial<ContentEntry> = {},
): ContentEntry => ({
  slug,
  name,
  version,
  size,
  kind: "mod",
  source: "modrinth",
  side: "client",
  dependency: false,
  ...o,
});

const content: Record<string, ContentEntry[]> = {
  "create-lite": [
    e("fabric-api", "Fabric API", "0.115.0+1.21.1", 2_310_000, { side: "both", dependency: true }),
    e("sodium", "Sodium", "0.6.5", 1_240_000, { update: "0.6.6" }),
    e("lithium", "Lithium", "0.14.3", 720_000, { side: "both" }),
    e("iris", "Iris Shaders", "1.8.1", 2_870_000),
    e("modmenu", "Mod Menu", "11.0.3", 790_000),
    e("ferrite-core", "FerriteCore", "7.0.2", 130_000, { side: "both" }),
    e("immediatelyfast", "ImmediatelyFast", "1.3.3", 610_000),
    e("entityculling", "Entity Culling", "1.7.2", 240_000),
    e("jei", "Just Enough Items", "19.21.0.247", 1_120_000, { source: "curseforge", side: "both" }),
    e("xaeros-minimap", "Xaero's Minimap", "24.6.1", 980_000, { source: "curseforge", update: "24.7.0" }),
    e("faithful-32x", "Faithful 32x", "1.21.1-r4", 18_400_000, { kind: "resourcepack" }),
    e("complementary-reimagined", "Complementary Reimagined", "5.4", 640_000, { kind: "shaderpack" }),
  ],
  "vanilla-plus": [
    e("fabric-api", "Fabric API", "0.92.2+1.20.1", 2_100_000, { side: "both", dependency: true }),
    e("sodium", "Sodium", "0.5.11", 1_010_000),
    e("lithium", "Lithium", "0.11.2", 640_000, { side: "both" }),
    e("continuity", "Continuity", "3.0.0", 410_000),
    e("terralith", "Terralith", "2.5.4", 3_200_000, { kind: "datapack", side: "server" }),
  ],
  "shader-lab": [
    e("fabric-api", "Fabric API", "0.115.0+1.21.1", 2_310_000, { side: "both", dependency: true }),
    e("sodium", "Sodium", "0.6.6", 1_250_000),
    e("iris", "Iris Shaders", "1.8.1", 2_870_000),
    e("complementary-reimagined", "Complementary Reimagined", "5.4", 640_000, { kind: "shaderpack" }),
    e("bsl-shaders", "BSL Shaders", "8.4.01", 520_000, { kind: "shaderpack" }),
  ],
};

const h = (
  slug: string,
  name: string,
  description: string,
  downloads: number,
  latest: string,
  o: Partial<SearchHit> = {},
): SearchHit => ({ slug, name, description, downloads, latest, kind: "mod", source: "modrinth", ...o });

const catalog: SearchHit[] = [
  h("sodium", "Sodium", "The fastest rendering optimization mod for Minecraft.", 98_400_000, "0.6.6"),
  h("lithium", "Lithium", "General-purpose optimization of game physics, AI and ticking.", 61_200_000, "0.14.3"),
  h("iris", "Iris Shaders", "Modern shader pack loader, compatible with existing packs.", 74_900_000, "1.8.1"),
  h("create-fabric", "Create Fabric", "Aesthetic technology that falls short of magic.", 22_100_000, "6.0.8"),
  h("modmenu", "Mod Menu", "Adds a screen to view and configure installed mods.", 70_300_000, "11.0.3"),
  h("worldedit", "WorldEdit", "In-game map editor for building and automation.", 12_700_000, "7.3.9"),
  h("carpet", "Carpet", "Take full control over what matters in vanilla mechanics.", 9_800_000, "1.4.147"),
  h("appleskin", "AppleSkin", "Food and hunger related HUD improvements.", 31_500_000, "3.0.6"),
  h("jei", "Just Enough Items", "View items and recipes.", 310_000_000, "19.21.0.247", { source: "curseforge" }),
  h("xaeros-world-map", "Xaero's World Map", "Self-writing full screen world map.", 140_000_000, "1.39.2", { source: "curseforge" }),
  h("faithful-32x", "Faithful 32x", "Vanilla textures at double the resolution.", 14_200_000, "1.21.1-r4", { kind: "resourcepack" }),
  h("fresh-animations", "Fresh Animations", "Animated, more expressive mobs.", 19_600_000, "1.9.2", { kind: "resourcepack" }),
  h("complementary-reimagined", "Complementary Reimagined", "Vanilla-faithful shaders with strong performance.", 27_300_000, "5.4", { kind: "shaderpack" }),
  h("bsl-shaders", "BSL Shaders", "Bright, colourful shader pack with deep customisation.", 18_900_000, "8.4.01", { kind: "shaderpack" }),
  h("terralith", "Terralith", "Overworld overhaul with nearly 100 new biomes.", 16_400_000, "2.5.8", { kind: "datapack" }),
];

let config: GlobalConfig = {
  curseforge_api_key: null,
  username: "Player",
  download_concurrency: 24,
  java_path: null,
};

const now = Date.now();
const min = 60_000;
const activity: ActivityEvent[] = [
  {
    id: uid(), t: now - 2 * min, origin: "mcp", client: "claude-code", tool: "stop", packId: "create-lite",
    summary: "Stopped session client", status: "ok", durationMs: 412,
    params: { pack: "C:\\packs\\create-lite", profile: "client" }, result: { exit_code: 0 },
  },
  {
    id: uid(), t: now - 3 * min, origin: "mcp", client: "claude-code", tool: "screenshot", packId: "create-lite",
    summary: "Captured after-reload.png", status: "ok", durationMs: 188,
    params: { name: "after-reload" }, result: { path: "C:\\packs\\create-lite\\run\\client\\screenshots\\after-reload.png" },
  },
  {
    id: uid(), t: now - 4 * min, origin: "mcp", client: "claude-code", tool: "game_command", packId: "create-lite",
    summary: "/time set noon", status: "ok", durationMs: 36,
    params: { command: "time set noon" }, result: { output: ["Set the time to 6000"] },
  },
  {
    id: uid(), t: now - 5 * min, origin: "mcp", client: "claude-code", tool: "create_world", packId: "create-lite",
    summary: "Created flat creative world mcpack-test", status: "ok", durationMs: 6240,
    params: { level_id: "mcpack-test", flat: true, game_mode: "creative" }, result: { ok: true },
  },
  {
    id: uid(), t: now - 6 * min, origin: "mcp", client: "claude-code", tool: "launch", packId: "create-lite",
    summary: "Launched profile client", status: "ok", durationMs: 21_480,
    params: { pack: "C:\\packs\\create-lite", profile: "client" }, result: { pid: 18244, agent_port: 51877 },
  },
  {
    id: uid(), t: now - 8 * min, origin: "mcp", client: "claude-code", tool: "pack_add", packId: "create-lite",
    summary: "optifine — no project with that slug", status: "error", durationMs: 640, errorCode: "not_found",
    params: { slug: "optifine", source: "modrinth" },
    result: { error: { code: "not_found", message: "not found: modrinth project 'optifine'" } },
  },
  {
    id: uid(), t: now - 9 * min, origin: "mcp", client: "claude-code", tool: "pack_add", packId: "create-lite",
    summary: "Added entityculling 1.7.2", status: "ok", durationMs: 910,
    params: { slug: "entityculling", source: "modrinth" }, result: { version: "1.7.2", dependencies: [] },
  },
  {
    id: uid(), t: now - 47 * min, origin: "cli", tool: "sync", packId: "shader-lab",
    summary: "Synced 5 files into run/client", status: "ok", durationMs: 3110,
    params: { profile: "client" }, result: { linked: 5, removed: 0 },
  },
  {
    id: uid(), t: now - 26 * 60 * min, origin: "mcp", client: "claude-code", tool: "smoke_test", packId: "vanilla-plus",
    summary: "Game exited before the agent connected", status: "error", durationMs: 48_300, errorCode: "launch",
    params: { profile: "client", timeout_secs: 180 },
    result: { error: { code: "launch", message: "launch failed: process exited with code 1 (see logs)" } },
  },
];

// ---------------------------------------------------------------- sessions

interface Session {
  state: InstanceState;
  agent: AgentStatus | null;
  token: number;
  ticker?: number;
  logId: number;
}
const sessions = new Map<string, Session>();
const session = (packId: string): Session => {
  let s = sessions.get(packId);
  if (!s) {
    const pack = findPack(packId);
    s = { state: { status: "stopped", profile: pack.profiles[0] }, agent: null, token: 0, logId: 0 };
    sessions.set(packId, s);
  }
  return s;
};

function findPack(packId: string): Pack {
  const p = packs.find((x) => x.id === packId);
  if (!p) throw new BackendError("not_found", `not found: pack '${packId}'`);
  return p;
}

function log(packId: string, level: LogLevel, source: string, text: string) {
  const s = session(packId);
  emit({ type: "log", packId, lines: [{ id: s.logId++, t: Date.now(), level, source, text }] });
}

function setState(packId: string, patch: Partial<InstanceState>) {
  const s = session(packId);
  s.state = { ...s.state, ...patch };
  emit({ type: "instance", packId, state: s.state });
}

function record(ev: Omit<ActivityEvent, "id" | "t"> & { id?: string }): ActivityEvent {
  const full: ActivityEvent = { id: ev.id ?? uid(), t: Date.now(), ...ev };
  const i = activity.findIndex((a) => a.id === full.id);
  if (i >= 0) {
    full.t = activity[i].t;
    activity[i] = full;
  } else activity.unshift(full);
  emit({ type: "activity", event: full });
  return full;
}

/** Run an action and mirror it into the activity log, the way the core reports tool calls. */
async function track<T>(
  tool: string,
  packId: string | undefined,
  summary: string,
  params: unknown,
  fn: () => Promise<T>,
  done?: (r: T) => string,
): Promise<T> {
  const started = performance.now();
  const base = { origin: "app" as const, tool, packId, params };
  const ev = record({ ...base, summary, status: "running" });
  try {
    const result = await fn();
    record({
      ...base, id: ev.id, status: "ok", result: result ?? { ok: true },
      summary: done ? done(result) : summary, durationMs: Math.round(performance.now() - started),
    });
    return result;
  } catch (err) {
    const be = err instanceof BackendError ? err : new BackendError("other", String(err));
    record({
      ...base, id: ev.id, status: "error", summary: be.message, errorCode: be.code,
      result: { error: { code: be.code, message: be.message } },
      durationMs: Math.round(performance.now() - started),
    });
    throw be;
  }
}

async function runLaunch(pack: Pack, profile: string) {
  const s = session(pack.id);
  const token = ++s.token;
  const alive = () => s.token === token;
  const id = pack.id;
  const items = content[id]?.length ?? 0;

  setState(id, { status: "preparing", profile, phase: "resolve", progress: undefined, detail: "Checking pack.lock", exitCode: undefined });
  log(id, "info", "mcpack", `Launching ${pack.name} (${profile}) — Minecraft ${pack.minecraft}, Fabric ${pack.loaderVersion}`);
  await sleep(600);
  if (!alive()) return;
  log(id, "info", "mcpack", `Lockfile up to date: ${items} entries`);

  const total = 412;
  for (let i = 0; i <= 20; i++) {
    if (!alive()) return;
    setState(id, { phase: "download", progress: i / 20, detail: `${Math.round((i / 20) * total)} / ${total} files` });
    await sleep(90);
  }
  log(id, "info", "mcpack", `Verified ${total} files (libraries, assets, ${items} pack files)`);

  setState(id, { phase: "java", progress: undefined, detail: "Java 21 (Mojang runtime)" });
  await sleep(500);
  if (!alive()) return;
  log(id, "info", "mcpack", "Using Java 21.0.5 from cache/java/java-runtime-delta");

  setState(id, { phase: "launch", detail: "Starting Minecraft", pid: 10_000 + Math.floor(Math.random() * 20_000), startedAt: Date.now() });
  const boot: [LogLevel, string, string][] = [
    ["info", "main", `Loading Minecraft ${pack.minecraft} with Fabric Loader ${pack.loaderVersion}`],
    ["info", "main", `Loading ${items + 48} mods`],
    ["warn", "main", "Mod 'sodium' recommends a newer version of 'fabric-api'"],
    ["info", "main", "SpongePowered MIXIN Subsystem Version=0.8.7"],
    ["info", "Render thread", "Setting user: " + config.username],
    ["info", "Render thread", "Backend library: LWJGL version 3.3.3"],
    ["info", "Render thread", "OpenGL 4.6 — reloading ResourceManager: vanilla, fabric"],
    ["info", "Render thread", "Sound engine started"],
    ["info", "Render thread", "Created: 1024x512x4 minecraft:textures/atlas/blocks.png-atlas"],
  ];
  for (const [lvl, src, text] of boot) {
    await sleep(210);
    if (!alive()) return;
    log(id, lvl, src, text);
  }

  const port = 49_152 + Math.floor(Math.random() * 10_000);
  setState(id, { phase: "agent", detail: "Waiting for agent" });
  await sleep(700);
  if (!alive()) return;
  log(id, "info", "mcpack-agent", `listening on 127.0.0.1:${port}`);

  s.agent = {
    minecraft: pack.minecraft,
    mods_loaded: items + 48,
    screen: "net.minecraft.client.gui.screens.TitleScreen",
    screen_title: "Title Screen",
    title_screen: true,
    loading: false,
    in_world: false,
    singleplayer: false,
    fps: 60,
  };
  setState(id, { status: "running", phase: undefined, progress: undefined, detail: undefined, agentPort: port });
  emit({ type: "agent", packId: id, status: s.agent });

  s.ticker = window.setInterval(() => {
    if (!s.agent) return;
    const a = { ...s.agent, fps: (s.agent.in_world ? 138 : 60) + Math.round((Math.random() - 0.5) * 8) };
    if (a.position) a.position = { ...a.position, x: a.position.x + (Math.random() - 0.5) * 0.4, z: a.position.z + (Math.random() - 0.5) * 0.4 };
    s.agent = a;
    emit({ type: "agent", packId: id, status: a });
  }, 1000);
}

function endSession(packId: string, exitCode: number) {
  const s = session(packId);
  s.token++;
  if (s.ticker) clearInterval(s.ticker);
  s.ticker = undefined;
  s.agent = null;
  const wasUp = s.state.status !== "stopped";
  setState(packId, { status: "stopped", phase: undefined, progress: undefined, detail: undefined, pid: undefined, agentPort: undefined, startedAt: undefined, exitCode });
  emit({ type: "agent", packId, status: null });
  if (wasUp) log(packId, "info", "mcpack", `Process exited with code ${exitCode}`);
}

function requireAgent(packId: string): AgentStatus {
  const a = session(packId).agent;
  if (!a) throw new BackendError("agent", "agent error: no running session for this pack");
  return a;
}

const COMMANDS: Partial<Record<string, (args: string) => string>> = {
  time: (a) => `Set the time to ${{ day: 1000, noon: 6000, night: 13000, midnight: 18000 }[a.split(" ")[1]] ?? a.split(" ")[1] ?? 0}`,
  gamemode: (a) => `Set own game mode to ${a.charAt(0).toUpperCase() + a.slice(1)} Mode`,
  weather: (a) => `Set the weather to ${a}`,
  say: (a) => `[${config.username}] ${a}`,
  list: () => `There are 1 of a max of 8 players online: ${config.username}`,
  seed: () => "Seed: [-4172144997902289642]",
  give: (a) => `Gave 1 [${a.split(" ")[1] ?? "item"}] to ${config.username}`,
  tp: (a) => `Teleported ${config.username} to ${a}`,
  setblock: (a) => `Changed the block at ${a.split(" ").slice(0, 3).join(", ")}`,
  fill: () => "Successfully filled 64 block(s)",
  summon: (a) => `Summoned new ${a.split(" ")[0].replace("minecraft:", "")}`,
  kill: () => "Killed 3 entities",
  reload: () => "Reloading!",
};

function drawScreenshot(a: AgentStatus): string {
  const c = document.createElement("canvas");
  c.width = 480;
  c.height = 270;
  const g = c.getContext("2d")!;
  if (!a.in_world) {
    g.fillStyle = "#1b1410";
    g.fillRect(0, 0, 480, 270);
    for (let y = 0; y < 270; y += 30) for (let x = 0; x < 480; x += 30) {
      g.fillStyle = (x + y) % 60 ? "#2a1f18" : "#33261d";
      g.fillRect(x, y, 29, 29);
    }
    g.fillStyle = "rgba(0,0,0,.45)";
    g.fillRect(0, 0, 480, 270);
    g.fillStyle = "#d8d8d8";
    for (let i = 0; i < 3; i++) g.fillRect(150, 120 + i * 34, 180, 24);
    return c.toDataURL("image/png");
  }
  const sky = g.createLinearGradient(0, 0, 0, 170);
  sky.addColorStop(0, "#6aa7ee");
  sky.addColorStop(1, "#b8d7f5");
  g.fillStyle = sky;
  g.fillRect(0, 0, 480, 270);
  const B = 15;
  for (let x = 0; x < 480; x += B) {
    const hgt = 150 + Math.round((Math.sin(x / 70) * 22 + Math.sin(x / 23) * 8) / B) * B;
    for (let y = hgt; y < 270; y += B) {
      const d = (y - hgt) / B;
      g.fillStyle = d === 0 ? ((x / B) % 2 ? "#5fa043" : "#69ad4a") : d < 4 ? ((x / B + d) % 2 ? "#7a5536" : "#84603e") : (x / B + d) % 3 ? "#7b7b7b" : "#868686";
      g.fillRect(x, y, B, B);
    }
  }
  g.fillStyle = "#fff";
  g.fillRect(238, 133, 4, 4);
  return c.toDataURL("image/png");
}

// ---------------------------------------------------------------- backend

export const mockBackend: Backend = {
  async info() {
    return { version: "0.1.0", home: "C:\\Users\\you\\AppData\\Roaming\\mcpack", mode: "mock" };
  },

  async listPacks() {
    await sleep(120);
    return packs.map((p) => ({ ...p }));
  },

  createPack(input) {
    return track("pack_init", undefined, `Created ${input.name}`, input, async () => {
      await sleep(400);
      const id = input.name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
      if (!id) throw new BackendError("invalid", "invalid input: pack name is empty");
      if (packs.some((p) => p.id === id)) throw new BackendError("invalid", `invalid input: a pack named '${id}' already exists`);
      const pack: Pack = {
        id, name: input.name.trim(), path: `C:\\packs\\${id}`, minecraft: input.minecraft, loader: input.loader,
        loaderVersion: input.loader === "vanilla" ? "" : "0.16.9", profiles: ["client"], lock: "synced", pendingChanges: 0, watch: false,
      };
      packs.push(pack);
      content[id] = input.loader === "fabric" ? [e("fabric-api", "Fabric API", "0.115.0+1.21.1", 2_310_000, { side: "both" })] : [];
      emit({ type: "packs", packs: packs.map((p) => ({ ...p })) });
      return pack;
    });
  },

  async content(packId) {
    findPack(packId);
    await sleep(100);
    return [...(content[packId] ?? [])];
  },

  async search(packId, query, kind: ContentKind) {
    await sleep(260);
    const q = query.trim().toLowerCase();
    const have = new Set((content[packId] ?? []).map((c) => c.slug));
    return catalog
      .filter((c) => c.kind === kind && !have.has(c.slug))
      .filter((c) => !q || c.name.toLowerCase().includes(q) || c.slug.includes(q) || c.description.toLowerCase().includes(q))
      .sort((a, b) => b.downloads - a.downloads);
  },

  addContent(packId, hit) {
    return track("pack_add", packId, `Added ${hit.slug} ${hit.latest}`, { slug: hit.slug, source: hit.source }, async () => {
      await sleep(700);
      if (hit.source === "curseforge" && !config.curseforge_api_key)
        throw new BackendError("config", "missing configuration: CurseForge API key (set it in Settings)");
      const list = content[packId] ?? (content[packId] = []);
      list.push(e(hit.slug, hit.name, hit.latest, 900_000, { kind: hit.kind, source: hit.source }));
      emit({ type: "content", packId, content: [...list] });
    });
  },

  removeContent(packId, slug) {
    return track("pack_remove", packId, `Removed ${slug}`, { slug }, async () => {
      await sleep(250);
      content[packId] = (content[packId] ?? []).filter((c) => c.slug !== slug);
      emit({ type: "content", packId, content: [...content[packId]] });
    });
  },

  updateContent(packId, slug) {
    const entry = (content[packId] ?? []).find((c) => c.slug === slug);
    return track("pack_set", packId, `Updated ${slug} to ${entry?.update}`, { slug, version: entry?.update }, async () => {
      await sleep(600);
      if (!entry?.update) throw new BackendError("invalid", `invalid input: no update available for '${slug}'`);
      content[packId] = content[packId].map((c) => (c.slug === slug ? { ...c, version: c.update!, update: undefined } : c));
      emit({ type: "content", packId, content: [...content[packId]] });
    });
  },

  resolve(packId) {
    const pack = findPack(packId);
    return track("resolve", packId, "Resolved pack.lock", { pack: pack.path }, async () => {
      await sleep(1100);
      pack.lock = "synced";
      pack.pendingChanges = 0;
      emit({ type: "packs", packs: packs.map((p) => ({ ...p })) });
    });
  },

  setWatch(packId, on) {
    const pack = findPack(packId);
    return track(on ? "watch_start" : "watch_stop", packId, on ? "Watching pack for changes" : "Stopped watching", { pack: pack.path }, async () => {
      await sleep(150);
      pack.watch = on;
      emit({ type: "packs", packs: packs.map((p) => ({ ...p })) });
    });
  },

  // No file pickers in a browser tab: pretend the user chose something.
  async pickImportFile() {
    return "C:\\Users\\you\\Downloads\\Fabulously Optimized 6.2.0.mrpack";
  },

  importPack(file) {
    return track("import_pack", undefined, "Imported pack", { file }, async () => {
      await sleep(1200);
      const name = (file.split(/[\\/]/).pop() ?? "Imported").replace(/\.(mrpack|zip)$/i, "");
      const base = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "imported";
      let id = base;
      for (let n = 2; packs.some((p) => p.id === id); n++) id = `${base}-${n}`;
      const pack: Pack = {
        id, name, path: `C:\\packs\\${id}`, minecraft: "1.21.1", loader: "fabric", loaderVersion: "0.16.9",
        profiles: ["client"], lock: "synced", pendingChanges: 0, watch: false,
      };
      packs.push(pack);
      content[id] = [
        e("fabric-api", "Fabric API", "0.115.0+1.21.1", 2_310_000, { side: "both", dependency: true }),
        e("sodium", "Sodium", "0.6.6", 1_250_000),
        e("lithium", "Lithium", "0.14.3", 720_000, { side: "both" }),
      ];
      emit({ type: "packs", packs: packs.map((p) => ({ ...p })) });
      return { pack: { ...pack }, warnings: [] as string[] };
    }, (r) => `Imported ${r.pack.name}`);
  },

  async pickExportPath(defaultName, format) {
    return `C:\\Users\\you\\Downloads\\${defaultName}.${format === "mrpack" ? "mrpack" : "zip"}`;
  },

  exportPack(packId, profile, format, out) {
    const pack = findPack(packId);
    return track("export_pack", packId, `Exported ${pack.name}`, { pack: pack.path, profile, format, out }, async () => {
      await sleep(900);
      const list = content[packId] ?? [];
      const local = list.filter((c) => c.source === "local").length;
      return { path: out, files: list.length - local, overrides: local, warnings: [] as string[] };
    });
  },

  async instances() {
    return Object.fromEntries([...sessions].map(([id, s]) => [id, s.state]));
  },

  launch(packId, profile) {
    const pack = findPack(packId);
    return track("launch", packId, `Launched profile ${profile}`, { pack: pack.path, profile }, async () => {
      const s = session(packId);
      if (s.state.status === "running" || s.state.status === "preparing" || s.state.status === "stopping")
        throw new BackendError("launch", "launch failed: a session is already active for this profile");
      await runLaunch(pack, profile);
      if (session(packId).state.status !== "running") return { cancelled: true };
      return { pid: s.state.pid, agent_port: s.state.agentPort };
    }, (r) => ("cancelled" in r ? "Launch cancelled" : `Launched profile ${profile}`)).then(() => undefined);
  },

  stop(packId) {
    return track("stop", packId, "Stopped session", { pack: findPack(packId).path }, async () => {
      await sleep(600);
      endSession(packId, 0);
    });
  },

  agent<T>(packId: string, method: string, params: Record<string, unknown> = {}) {
    const tool = { command: "game_command", reload_resources: "reload", reload_datapacks: "reload" }[method] ?? method;
    if (method === "status") return Promise.resolve(requireAgent(packId) as T);
    const summary =
      method === "command" ? `/${String(params.command ?? "").replace(/^\//, "")}`
      : method === "create_world" ? `Created world ${params.level_id ?? "mcpack-test"}`
      : method === "reload_resources" ? "Reloaded resource packs"
      : method === "reload_datapacks" ? "Reloaded data packs"
      : method === "screenshot" ? "Captured screenshot"
      : method === "leave_world" ? "Left world"
      : method;
    return track(tool, packId, summary, params, async () => {
      const s = session(packId);
      const a = requireAgent(packId);
      const set = (patch: Partial<AgentStatus>) => {
        s.agent = { ...requireAgent(packId), ...patch };
        emit({ type: "agent", packId, status: s.agent });
      };
      switch (method) {
        case "command": {
          await sleep(60);
          if (!a.in_world) throw new BackendError("agent", "agent error: command requires a singleplayer world (use create_world/join_world first)");
          const raw = String(params.command ?? "").trim().replace(/^\//, "");
          const [root, ...rest] = raw.split(" ");
          const fn = COMMANDS[root];
          const line = fn ? fn(rest.join(" ")) : `Unknown or incomplete command, see below for error\n${raw}<--[HERE]`;
          log(packId, fn ? "info" : "warn", "Server thread", `[${config.username}: ${line.split("\n")[0]}]`);
          return { output: line.split("\n") };
        }
        case "create_world": {
          const level = String(params.level_id ?? "mcpack-test");
          set({ loading: true, title_screen: false, screen: "net.minecraft.client.gui.screens.LevelLoadingScreen", screen_title: "Loading world" });
          log(packId, "info", "Server thread", `Starting integrated minecraft server, level '${level}'`);
          await sleep(1600);
          log(packId, "info", "Server thread", `${config.username} joined the game`);
          set({
            loading: false, in_world: true, singleplayer: true, screen: null, screen_title: null, world_name: level, level_id: level,
            position: { x: 0.5, y: params.flat ? -60 : 71, z: 0.5 }, dimension: "minecraft:overworld", health: 20,
          });
          return { ok: true };
        }
        case "leave_world":
          await sleep(500);
          log(packId, "info", "Server thread", "Stopping singleplayer server as player logged out");
          set({
            in_world: false, singleplayer: false, title_screen: true, screen: "net.minecraft.client.gui.screens.TitleScreen", screen_title: "Title Screen",
            world_name: undefined, level_id: undefined, position: undefined, dimension: undefined, health: undefined,
          });
          return { ok: true };
        case "reload_resources":
          set({ loading: true });
          await sleep(1300);
          set({ loading: false });
          log(packId, "info", "Render thread", "Reloading ResourceManager: vanilla, fabric, file/faithful-32x");
          return { ok: true, selected: ["vanilla", "fabric", "file/faithful-32x"] };
        case "reload_datapacks":
          await sleep(500);
          if (!a.in_world) throw new BackendError("agent", "agent error: command requires a singleplayer world (use create_world/join_world first)");
          log(packId, "info", "Server thread", "Reloading!");
          return { output: ["Reloading!"] };
        case "screenshot": {
          await sleep(180);
          const name = String(params.name ?? `mcpack-${Date.now()}`);
          return { ok: true, path: `${findPack(packId).path}\\run\\${s.state.profile}\\screenshots\\${name}.png`, image: drawScreenshot(a) };
        }
        default:
          throw new BackendError("agent", `agent error: unknown method: ${method}`);
      }
    }) as Promise<T>;
  },

  async activity() {
    return [...activity];
  },

  async getConfig() {
    return { ...config };
  },

  async saveConfig(cfg) {
    await sleep(150);
    config = { ...cfg };
  },

  subscribe(fn) {
    listeners.add(fn);
    return () => listeners.delete(fn);
  },
};
