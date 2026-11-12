import { Activity, Check, CircleAlert, CircleCheck, Copy, Eye, EyeOff, LoaderCircle } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import type { ActivityEvent, GlobalConfig, Origin } from "../api";
import { Badge, Button, copy, Empty, fmt, IconButton, Segmented, Switch, useNow } from "../components/ui";
import { type Theme, useStore } from "../store";

// ---------------------------------------------------------------- activity

export function StatusIcon({ status }: { status: ActivityEvent["status"] }) {
  if (status === "running") return <LoaderCircle size={14} className="spin" style={{ color: "var(--text-2)", flex: "none" }} aria-label="Running" />;
  if (status === "error") return <CircleAlert size={14} style={{ color: "var(--danger)", flex: "none" }} aria-label="Failed" />;
  return <CircleCheck size={14} style={{ color: "var(--accent)", flex: "none" }} aria-label="Succeeded" />;
}

/** Who did it. AI agents get their own colour so automated actions stand out at a glance. */
export function OriginBadge({ event, compact }: { event: ActivityEvent; compact?: boolean }) {
  if (event.origin === "mcp") return <Badge tone="ai">{compact ? "AI" : (event.client ?? "MCP")}</Badge>;
  return <Badge>{event.origin === "cli" ? "CLI" : "You"}</Badge>;
}

type OriginFilter = "all" | Origin;

export function ActivityPage() {
  const activity = useStore((s) => s.activity);
  const packs = useStore((s) => s.packs);
  const [origin, setOrigin] = useState<OriginFilter>("all");
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [expanded, setExpanded] = useState<string | null>(null);
  const now = useNow(15_000);

  const rows = useMemo(
    () => activity.filter((a) => (origin === "all" || a.origin === origin) && (!errorsOnly || a.status === "error")),
    [activity, origin, errorsOnly],
  );
  const packName = (id?: string) => packs.find((p) => p.id === id)?.name ?? "—";

  return (
    <>
      <header className="page-head plain">
        <div className="page-title">
          <div className="grow">
            <h1>Activity</h1>
            <div className="sub">Every action on your packs — from this app, the CLI, and AI agents over MCP.</div>
          </div>
        </div>
      </header>
      <div className="page-body flush">
        <div className="toolbar">
          <Segmented
            label="Origin"
            value={origin}
            onChange={setOrigin}
            options={[
              { value: "all", label: "All" },
              { value: "mcp", label: "AI agents" },
              { value: "cli", label: "CLI" },
              { value: "app", label: "You" },
            ]}
          />
          <span className="grow" />
          <label className="row muted" style={{ fontSize: 12, cursor: "pointer" }}>
            Errors only
            <Switch checked={errorsOnly} onChange={setErrorsOnly} label="Errors only" />
          </label>
        </div>
        {rows.length === 0 ? (
          <Empty icon={<Activity size={28} strokeWidth={1.5} />} title="No activity">
            {activity.length ? "Nothing matches these filters." : "Actions will appear here as they happen."}
          </Empty>
        ) : (
          <div className="list">
            <div className="trow head act-grid" style={{ cursor: "default" }}>
              <span />
              <span>Tool</span>
              <span>Result</span>
              <span>Pack</span>
              <span>Took</span>
              <span>When</span>
            </div>
            {rows.map((a) => (
              <div key={a.id}>
                <button type="button" className="trow act-grid" aria-expanded={expanded === a.id} onClick={() => setExpanded(expanded === a.id ? null : a.id)}>
                  <StatusIcon status={a.status} />
                  <span className="mono truncate">{a.tool}</span>
                  <span className="row" style={{ minWidth: 0 }}>
                    <span className={`truncate ${a.status === "error" ? "" : "muted"}`}>{a.summary}</span>
                    {a.errorCode && <Badge tone="error">{a.errorCode}</Badge>}
                  </span>
                  <span className="row" style={{ minWidth: 0 }}>
                    <OriginBadge event={a} />
                    <span className="muted truncate">{packName(a.packId)}</span>
                  </span>
                  <span className="faint" style={{ fontVariantNumeric: "tabular-nums" }}>
                    {fmt.duration(a.durationMs)}
                  </span>
                  <time className="faint" title={new Date(a.t).toLocaleString()}>
                    {a.status === "running" ? "running" : fmt.ago(a.t, now)}
                  </time>
                </button>
                {expanded === a.id && (
                  <div className="act-detail">
                    <div>
                      <h4>Arguments</h4>
                      <pre className="code">{JSON.stringify(a.params ?? {}, null, 2)}</pre>
                    </div>
                    <div>
                      <h4>{a.status === "error" ? "Error" : "Result"}</h4>
                      <pre className="code">{a.status === "running" ? "…" : JSON.stringify(a.result ?? {}, null, 2)}</pre>
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </>
  );
}

// ---------------------------------------------------------------- settings

/** Text input that commits on blur or Enter, and reverts on Escape. */
function CommitInput({
  value,
  onCommit,
  ...rest
}: { value: string; onCommit: (v: string) => void } & Omit<React.InputHTMLAttributes<HTMLInputElement>, "value" | "onChange">) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return (
    <input
      className="input"
      spellCheck={false}
      autoComplete="off"
      {...rest}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => draft !== value && onCommit(draft)}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
        if (e.key === "Escape") setDraft(value);
      }}
    />
  );
}

function Snippet({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return (
    <div className="snippet">
      <code>{text}</code>
      <IconButton
        size="sm"
        label="Copy"
        onClick={async () => {
          if (await copy(text)) {
            setDone(true);
            setTimeout(() => setDone(false), 1200);
          }
        }}
      >
        {done ? <Check size={13} /> : <Copy size={13} />}
      </IconButton>
    </div>
  );
}

export function SettingsPage() {
  const config = useStore((s) => s.config);
  const info = useStore((s) => s.info);
  const theme = useStore((s) => s.theme);
  const { saveConfig, setTheme } = useStore.getState();
  const [reveal, setReveal] = useState(false);

  if (!config) return null;
  const set = (patch: Partial<GlobalConfig>) => void saveConfig({ ...config, ...patch });

  return (
    <>
      <header className="page-head plain">
        <div className="page-title">
          <div className="grow">
            <h1>Settings</h1>
            <div className="sub">
              Saved to <span className="mono selectable">{info?.home}\config.toml</span>
            </div>
          </div>
        </div>
      </header>
      <div className="page-body">
        <div className="settings">
          <section>
            <h2>General</h2>
            <div className="panel">
              <div className="setting-row">
                <div className="grow">
                  <label className="label" htmlFor="s-user">
                    Username
                  </label>
                  <div className="desc">Offline player name used when launching.</div>
                </div>
                <CommitInput id="s-user" value={config.username} onCommit={(v) => v.trim() && set({ username: v.trim() })} />
              </div>
              <div className="setting-row">
                <div className="grow">
                  <div className="label">Theme</div>
                </div>
                <Segmented<Theme>
                  label="Theme"
                  value={theme}
                  onChange={setTheme}
                  options={[
                    { value: "system", label: "System" },
                    { value: "dark", label: "Dark" },
                    { value: "light", label: "Light" },
                  ]}
                />
              </div>
            </div>
          </section>

          <section>
            <h2>AI agents</h2>
            <div className="panel">
              <div className="panel-body">
                <p className="muted">
                  mcpack exposes every launcher action as an MCP tool, so an agent can build packs, launch the game, run commands and take
                  screenshots. Anything it does shows up in Activity.
                </p>
                <div className="desc faint" style={{ marginTop: 12, fontSize: 12 }}>
                  Claude Code
                </div>
                <Snippet text="claude mcp add mcpack -- mcpack serve" />
                <div className="desc faint" style={{ marginTop: 12, fontSize: 12 }}>
                  Other MCP clients
                </div>
                <Snippet text='{ "mcpServers": { "mcpack": { "command": "mcpack", "args": ["serve"] } } }' />
              </div>
            </div>
          </section>

          <section>
            <h2>Game</h2>
            <div className="panel">
              <div className="setting-row">
                <div className="grow">
                  <label className="label" htmlFor="s-java">
                    Java executable
                  </label>
                  <div className="desc">Leave empty to use the runtime Mojang ships for each version.</div>
                </div>
                <CommitInput id="s-java" className="input mono" placeholder="Automatic" value={config.java_path ?? ""} onCommit={(v) => set({ java_path: v.trim() || null })} />
              </div>
              <div className="setting-row">
                <div className="grow">
                  <label className="label" htmlFor="s-conc">
                    Parallel downloads
                  </label>
                  <div className="desc">Between 1 and 64.</div>
                </div>
                <CommitInput
                  id="s-conc"
                  className="input num"
                  inputMode="numeric"
                  value={String(config.download_concurrency)}
                  onCommit={(v) => {
                    const n = Math.round(Number(v));
                    set({ download_concurrency: Number.isFinite(n) ? Math.min(64, Math.max(1, n)) : config.download_concurrency });
                  }}
                />
              </div>
            </div>
          </section>

          <section>
            <h2>Sources</h2>
            <div className="panel">
              <div className="setting-row">
                <div className="grow">
                  <label className="label row" htmlFor="s-cf">
                    CurseForge API key
                    {config.curseforge_api_key ? <Badge tone="ok">Set</Badge> : <Badge>Not set</Badge>}
                  </label>
                  <div className="desc">Required to add content from CurseForge. Modrinth needs no key.</div>
                </div>
                <div className="row" style={{ gap: 4 }}>
                  <CommitInput
                    id="s-cf"
                    className="input mono"
                    type={reveal ? "text" : "password"}
                    placeholder="Paste key"
                    value={config.curseforge_api_key ?? ""}
                    onCommit={(v) => set({ curseforge_api_key: v.trim() || null })}
                  />
                  <IconButton label={reveal ? "Hide key" : "Show key"} onClick={() => setReveal(!reveal)}>
                    {reveal ? <EyeOff size={14} /> : <Eye size={14} />}
                  </IconButton>
                </div>
              </div>
            </div>
          </section>

          <section>
            <h2>About</h2>
            <div className="panel">
              <div className="setting-row">
                <div className="grow">
                  <div className="label">mcpack {info?.version}</div>
                  <div className="desc mono selectable">{info?.home}</div>
                </div>
                {info?.mode === "mock" && <Badge tone="warn">Simulated backend</Badge>}
                <Button variant="ghost" onClick={() => info && void copy(info.home)}>
                  Copy path
                </Button>
              </div>
            </div>
          </section>
        </div>
      </div>
    </>
  );
}
