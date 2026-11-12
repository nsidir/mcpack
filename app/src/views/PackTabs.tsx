import { ArrowDownToLine, Bot, Boxes, Camera, ChevronDown, CircleArrowUp, Copy, ExternalLink, Globe, LoaderCircle, LogOut, Play, Plus, RefreshCw, Search, SquareTerminal, Trash2 } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { backend, type ContentKind, type Pack } from "../api";
import { Badge, Button, copy, Empty, fmt, IconButton, Segmented } from "../components/ui";
import { agentAction, isActive, launch, openUrl, pageUrl, useStore } from "../store";

// ------------------------------------------------------------------ content

type KindFilter = "all" | ContentKind;
const KIND_LABEL: Record<ContentKind, string> = { mod: "Mod", resourcepack: "Resource pack", shaderpack: "Shader", datapack: "Data pack" };

export function ContentTab({ pack }: { pack: Pack }) {
  const content = useStore((s) => s.content[pack.id]);
  const { open, run, pickVersion } = useStore.getState();
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<KindFilter>("all");
  const [busy, setBusy] = useState<string | null>(null);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return (content ?? [])
      .filter((c) => kind === "all" || c.kind === kind)
      .filter((c) => !q || c.name.toLowerCase().includes(q) || c.slug.includes(q))
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [content, query, kind]);

  const updates = (content ?? []).filter((c) => c.update);
  const act = async (slug: string, p: Promise<unknown>, ok: string) => {
    setBusy(slug);
    await run(p, ok);
    setBusy(null);
  };

  return (
    <div className="page-body flush">
      <div className="toolbar">
        <div className="search">
          <Search size={14} />
          <input className="input" placeholder="Filter content" aria-label="Filter content" value={query} onChange={(e) => setQuery(e.target.value)} />
        </div>
        <Segmented
          label="Content type"
          value={kind}
          onChange={setKind}
          options={[
            { value: "all", label: "All" },
            { value: "mod", label: "Mods" },
            { value: "resourcepack", label: "Resource packs" },
            { value: "shaderpack", label: "Shaders" },
            { value: "datapack", label: "Data packs" },
          ]}
        />
        <span className="grow" />
        {updates.length > 0 && (
          <Button
            disabled={busy !== null}
            onClick={async () => {
              setBusy("*");
              for (const u of updates) await run(backend.updateContent(pack.id, u.slug));
              setBusy(null);
            }}
          >
            <CircleArrowUp size={14} />
            Update {updates.length}
          </Button>
        )}
        <Button variant="primary" onClick={() => open("add-content")}>
          <Plus size={14} />
          Add content
        </Button>
      </div>

      {content && rows.length === 0 ? (
        <Empty
          icon={<Boxes size={28} strokeWidth={1.5} />}
          title={content.length === 0 ? "This pack is empty" : "Nothing matches"}
          action={content.length === 0 ? <Button onClick={() => open("add-content")}>Add content</Button> : undefined}
        >
          {content.length === 0 ? "Add mods, resource packs or shaders from Modrinth and CurseForge." : "Try a different filter."}
        </Empty>
      ) : (
        <div className="list" role="table" aria-label="Pack content">
          <div className="trow head content-grid" role="row">
            <span role="columnheader">Name</span>
            <span role="columnheader">Type</span>
            <span role="columnheader">Version</span>
            <span role="columnheader">Source</span>
            <span role="columnheader">Size</span>
            <span />
          </div>
          {rows.map((c) => (
            <div key={c.slug} className="trow content-grid" role="row">
              <div className="row" role="cell" style={{ minWidth: 0 }}>
                <span className="avatar">{c.name[0]}</span>
                <span className="truncate" style={{ fontWeight: 500 }}>
                  {c.name}
                </span>
                {c.dependency && <Badge>dependency</Badge>}
              </div>
              <span className="muted" role="cell">
                {KIND_LABEL[c.kind]}
              </span>
              <div className="row" role="cell" style={{ minWidth: 0 }}>
                {c.source === "local" ? (
                  <span className="mono muted truncate">{c.version}</span>
                ) : (
                  <button type="button" className="version-btn" title="Choose a version" disabled={busy !== null} onClick={() => pickVersion(c)}>
                    <span className="mono truncate">{c.version}</span>
                    <ChevronDown size={12} />
                  </button>
                )}
                {c.update && (
                  <button
                    type="button"
                    className="update-pill"
                    disabled={busy !== null}
                    title={`Update to ${c.update}`}
                    onClick={() => void act(c.slug, backend.updateContent(pack.id, c.slug), `Updated ${c.name}`)}
                  >
                    {busy === c.slug ? <LoaderCircle size={11} className="spin" /> : "→"} {c.update}
                  </button>
                )}
              </div>
              <span className="muted" role="cell">
                {c.source === "modrinth" ? "Modrinth" : c.source === "curseforge" ? "CurseForge" : "Local"}
              </span>
              <span className="muted" role="cell" style={{ fontVariantNumeric: "tabular-nums" }}>
                {fmt.bytes(c.size)}
              </span>
              <div className="actions" role="cell">
                {pageUrl(c) && (
                  <IconButton size="sm" label={`Open ${c.name} project page`} onClick={() => openUrl(pageUrl(c)!)}>
                    <ExternalLink size={13} />
                  </IconButton>
                )}
                <IconButton
                  size="sm"
                  variant="danger"
                  label={`Remove ${c.name}`}
                  disabled={busy !== null}
                  onClick={() => void act(c.slug, backend.removeContent(pack.id, c.slug), `Removed ${c.name}`)}
                >
                  <Trash2 size={13} />
                </IconButton>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ console

type LevelFilter = "all" | "warn" | "error";

export function ConsoleTab({ pack }: { pack: Pack }) {
  const logs = useStore((s) => s.logs[pack.id]);
  const { clearLogs, toast } = useStore.getState();
  const [level, setLevel] = useState<LevelFilter>("all");
  const [query, setQuery] = useState("");
  const [follow, setFollow] = useState(true);
  const ref = useRef<HTMLDivElement>(null);

  const lines = useMemo(() => {
    const q = query.toLowerCase();
    return (logs ?? []).filter(
      (l) => (level === "all" || l.level === "error" || (level === "warn" && l.level === "warn")) && (!q || l.text.toLowerCase().includes(q) || l.source.toLowerCase().includes(q)),
    );
  }, [logs, level, query]);

  useEffect(() => {
    if (follow && ref.current) ref.current.scrollTop = ref.current.scrollHeight;
  }, [lines, follow]);

  return (
    <div className="page-body flush">
      <div className="toolbar">
        <div className="search">
          <Search size={14} />
          <input className="input" placeholder="Filter output" aria-label="Filter output" value={query} onChange={(e) => setQuery(e.target.value)} />
        </div>
        <Segmented
          label="Log level"
          value={level}
          onChange={setLevel}
          options={[
            { value: "all", label: "All" },
            { value: "warn", label: "Warnings" },
            { value: "error", label: "Errors" },
          ]}
        />
        <span className="grow" />
        <span className="faint" style={{ fontSize: 12, fontVariantNumeric: "tabular-nums" }}>
          {lines.length} lines
        </span>
        <IconButton label={follow ? "Following output" : "Follow output"} aria-pressed={follow} style={follow ? { color: "var(--accent)" } : undefined} onClick={() => setFollow(!follow)}>
          <ArrowDownToLine size={14} />
        </IconButton>
        <IconButton
          label="Copy output"
          disabled={!lines.length}
          onClick={async () => {
            const ok = await copy(lines.map((l) => `[${fmt.clock(l.t)}] [${l.source}/${l.level.toUpperCase()}] ${l.text}`).join("\n"));
            toast(ok ? "ok" : "error", ok ? `Copied ${lines.length} lines` : "Couldn't access the clipboard");
          }}
        >
          <Copy size={14} />
        </IconButton>
        <IconButton label="Clear console" disabled={!logs?.length} onClick={() => clearLogs(pack.id)}>
          <Trash2 size={14} />
        </IconButton>
      </div>
      {!logs?.length ? (
        <Empty icon={<SquareTerminal size={28} strokeWidth={1.5} />} title="No output yet">
          Launcher and game logs stream here while a session is running.
        </Empty>
      ) : (
        <div
          className="console"
          ref={ref}
          role="log"
          aria-label="Game output"
          onScroll={(e) => {
            const el = e.currentTarget;
            const atEnd = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
            if (atEnd !== follow) setFollow(atEnd);
          }}
        >
          {lines.map((l) => (
            <div key={l.id} className={`logline ${l.level}`}>
              <time>{fmt.clock(l.t)}</time>
              <span className={`src ${l.source.startsWith("mcpack") ? "own" : ""}`}>{l.source}</span>
              <span className="txt">{l.text}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// -------------------------------------------------------------------- agent

function Stat({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="stat">
      <dt>{label}</dt>
      <dd className="truncate">{children}</dd>
    </div>
  );
}

export function AgentTab({ pack }: { pack: Pack }) {
  const agent = useStore((s) => s.agent[pack.id]);
  const instance = useStore((s) => s.instances[pack.id]);
  const shot = useStore((s) => s.shots[pack.id]);
  const repl = useStore((s) => s.repl[pack.id]);
  const { runCommand, screenshot } = useStore.getState();
  const [busy, setBusy] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [cursor, setCursor] = useState(-1);
  const outRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (outRef.current) outRef.current.scrollTop = outRef.current.scrollHeight;
  }, [repl]);

  if (!agent) {
    const starting = instance?.status === "preparing";
    return (
      <div className="page-body flush">
        <Empty
          icon={<Bot size={28} strokeWidth={1.5} />}
          title={starting ? "Waiting for the game" : isActive(instance) ? "Agent not connected" : "Agent offline"}
          action={
            isActive(instance) ? undefined : (
              <Button variant="primary" onClick={() => launch(pack)}>
                <Play size={13} fill="currentColor" />
                Launch
              </Button>
            )
          }
        >
          {starting
            ? "The agent connects as soon as Minecraft finishes loading."
            : isActive(instance)
              ? "The game is running, but its agent isn't answering."
              : "The in-game agent runs commands, creates worlds, reloads packs and takes screenshots. It starts with the game."}
        </Empty>
      </div>
    );
  }

  const doing = async (key: string, p: Promise<unknown> | undefined) => {
    setBusy(key);
    await p;
    setBusy(null);
  };
  const ico = (key: string, icon: ReactNode) => (busy === key ? <LoaderCircle size={14} className="spin" /> : icon);
  const history = (repl ?? []).map((r) => r.command);
  const where = agent.loading ? "Loading" : agent.in_world ? (agent.world_name ?? "World") : (agent.screen_title ?? "Menu");

  return (
    <div className="page-body">
      <div className="agent-grid">
        <div className="stack">
          <section className="panel" aria-label="Game state">
            <dl className="stats">
              <Stat label="Screen">{where}</Stat>
              <Stat label="Dimension">{agent.dimension?.replace("minecraft:", "") ?? "—"}</Stat>
              <Stat label="Position">
                {agent.position ? `${agent.position.x.toFixed(0)}, ${agent.position.y.toFixed(0)}, ${agent.position.z.toFixed(0)}` : "—"}
              </Stat>
              <Stat label="Health">{agent.health != null ? `${agent.health} / 20` : "—"}</Stat>
              <Stat label="FPS">{agent.fps}</Stat>
              <Stat label="Mods loaded">{agent.mods_loaded}</Stat>
              <Stat label="Minecraft">{agent.minecraft}</Stat>
              <Stat label="Agent port">{instance?.agentPort ?? "—"}</Stat>
            </dl>
          </section>

          <div className="actions-row">
            {agent.in_world ? (
              <Button disabled={busy !== null} onClick={() => void doing("leave", agentAction(pack, "leave_world"))}>
                {ico("leave", <LogOut size={14} />)}
                Leave world
              </Button>
            ) : (
              <Button
                disabled={busy !== null || agent.loading}
                onClick={() => void doing("world", agentAction(pack, "create_world", { level_id: "mcpack-test", flat: true, game_mode: "creative" }))}
              >
                {ico("world", <Globe size={14} />)}
                Create test world
              </Button>
            )}
            <Button disabled={busy !== null} onClick={() => void doing("res", agentAction(pack, "reload_resources", {}, "Resources reloaded"))}>
              {ico("res", <RefreshCw size={14} />)}
              Reload resources
            </Button>
            <Button
              disabled={busy !== null || !agent.in_world}
              title={agent.in_world ? undefined : "Requires a world"}
              onClick={() => void doing("data", agentAction(pack, "reload_datapacks", {}, "Data packs reloaded"))}
            >
              {ico("data", <RefreshCw size={14} />)}
              Reload data packs
            </Button>
          </div>

          <section className="panel repl" aria-label="Game commands">
            <div className="panel-head">
              <h2>Commands</h2>
              <span className="faint" style={{ fontSize: 12, paddingRight: 6 }}>
                Runs as operator in the open world
              </span>
            </div>
            <div className="repl-out" ref={outRef} role="log">
              {!repl?.length && <div className="faint">{agent.in_world ? "Try: time set day" : "Create or join a world to run commands."}</div>}
              {repl?.map((r) => (
                <div key={r.id}>
                  <div className="cmd">{r.command}</div>
                  {r.pending ? <div className="out faint">…</div> : r.output.map((o, i) => <div key={i} className={`out ${r.error ? "error" : ""}`}>{o}</div>)}
                </div>
              ))}
            </div>
            <form
              className="repl-in"
              onSubmit={(e) => {
                e.preventDefault();
                const cmd = input.trim().replace(/^\//, "");
                if (!cmd) return;
                setInput("");
                setCursor(-1);
                void runCommand(pack.id, cmd);
              }}
            >
              <span aria-hidden>/</span>
              <input
                value={input}
                aria-label="Game command"
                placeholder="gamemode creative"
                spellCheck={false}
                autoComplete="off"
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
                  e.preventDefault();
                  const next = e.key === "ArrowUp" ? Math.min(cursor + 1, history.length - 1) : Math.max(cursor - 1, -1);
                  setCursor(next);
                  setInput(next < 0 ? "" : history[history.length - 1 - next]);
                }}
              />
            </form>
          </section>
        </div>

        <section className="panel" aria-label="Screenshot">
          <div className="shot">{shot?.image ? <img src={shot.image} alt="Latest screenshot of the game" /> : <Camera size={24} strokeWidth={1.5} />}</div>
          <div className="setting-row">
            <div className="grow">
              <div className="label">{shot ? `Captured ${fmt.clock(shot.t)}` : "No screenshot yet"}</div>
              <div className={`desc truncate ${shot ? "mono selectable" : ""}`} title={shot?.path}>
                {shot ? shot.path.split("\\").slice(-2).join("\\") : "See what the game is showing."}
              </div>
            </div>
            <Button disabled={busy !== null} onClick={() => void doing("shot", screenshot(pack.id))}>
              {ico("shot", <Camera size={14} />)}
              Capture
            </Button>
          </div>
        </section>
      </div>
    </div>
  );
}
