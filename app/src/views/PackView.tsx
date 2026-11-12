import { Bot, Boxes, Check, Copy, Gauge, LoaderCircle, Play, RefreshCw, RotateCw, Square, SquareTerminal, Trash2, Upload } from "lucide-react";
import { type CSSProperties, type ReactNode, useMemo, useState } from "react";
import { backend, type InstanceState, type Pack, PHASES } from "../api";
import { Badge, Button, copy, Dot, fmt, IconButton, useNow } from "../components/ui";
import { launch, type PackTab, pendingRestart, profileOf, restartGame, setProfile, stateLabel, stop, useStore } from "../store";
import { HotReloadPanel } from "./HotReload";
import { AgentTab, ConsoleTab, ContentTab } from "./PackTabs";
import { OriginBadge, StatusIcon } from "./Pages";

const cap = (s: string) => s[0].toUpperCase() + s.slice(1);

export function PackView({ pack }: { pack: Pack }) {
  const tab = useStore((s) => s.tab);
  const setTab = useStore((s) => s.setTab);
  const count = useStore((s) => s.content[pack.id]?.length);
  const instance = useStore((s) => s.instances[pack.id]);

  const tabs: { id: PackTab; label: string; icon: ReactNode; n?: number }[] = [
    { id: "overview", label: "Overview", icon: <Gauge size={14} /> },
    { id: "content", label: "Content", icon: <Boxes size={14} />, n: count },
    { id: "console", label: "Console", icon: <SquareTerminal size={14} /> },
    { id: "agent", label: "Agent", icon: <Bot size={14} /> },
  ];

  return (
    <>
      <header className="page-head">
        <div className="page-title">
          <div className="grow">
            <h1 className="truncate">{pack.name}</h1>
            <div className="sub">
              <span>Minecraft {pack.minecraft}</span>
              <span className="sep">·</span>
              <span>
                {cap(pack.loader)} {pack.loaderVersion}
              </span>
              <span className="sep">·</span>
              <span className="mono selectable truncate">{pack.path}</span>
            </div>
          </div>
          <LaunchControl pack={pack} instance={instance} />
        </div>
        <div className="tabs" role="tablist" aria-label="Pack sections">
          {tabs.map((t) => (
            <button key={t.id} type="button" role="tab" className="tab" aria-selected={tab === t.id} onClick={() => setTab(t.id)}>
              {t.icon}
              {t.label}
              {t.n != null && <span className="n">{t.n}</span>}
            </button>
          ))}
        </div>
      </header>
      {tab === "overview" && <Overview pack={pack} instance={instance} />}
      {tab === "content" && <ContentTab pack={pack} />}
      {tab === "console" && <ConsoleTab pack={pack} />}
      {tab === "agent" && <AgentTab pack={pack} />}
    </>
  );
}

/** The launch button doubles as the state display: Launch → Launching (phase) → Running → Stopping. */
function LaunchControl({ pack, instance }: { pack: Pack; instance?: InstanceState }) {
  const now = useNow(1000, instance?.status === "running");
  const events = useStore((s) => s.watchEvents[pack.id]);
  const pending = pendingRestart(events, instance);
  const exportButton = (
    <>
      <IconButton label="Delete pack…" onClick={() => useStore.getState().open("delete-pack")}>
        <Trash2 size={15} />
      </IconButton>
      <IconButton label="Export modpack…" onClick={() => useStore.getState().open("export-pack")}>
        <Upload size={15} />
      </IconButton>
    </>
  );

  if (instance?.status === "preparing") {
    const phase = PHASES.find((p) => p.id === instance.phase);
    return (
      <div className="launch">
        {exportButton}
        <div className="progress-chip" style={{ "--p": instance.progress ?? 0 } as CSSProperties} role="status" title={instance.detail}>
          <LoaderCircle size={14} className="spin" />
          <span>Launching{phase ? ` · ${phase.label}` : ""}</span>
          <span className="grow" />
          <span className="muted">{instance.progress != null ? `${Math.round(instance.progress * 100)}%` : ""}</span>
        </div>
        <Button size="lg" onClick={() => stop(pack)}>
          Cancel
        </Button>
      </div>
    );
  }

  if (instance?.status === "running" || instance?.status === "stopping") {
    const stopping = instance.status === "stopping";
    return (
      <div className="launch">
        {exportButton}
        {pending && !stopping && (
          <Button size="lg" className="restart-needed" title={(pending.restart_reasons ?? []).join("\n")} onClick={() => void restartGame(pack)}>
            <RotateCw size={13} />
            Restart to apply
          </Button>
        )}
        <Button size="lg" variant="primary" className="state" disabled role="status" title={`Profile ${instance.profile}${instance.pid ? ` · pid ${instance.pid}` : ""}`}>
          {stopping ? <LoaderCircle size={13} className="spin" /> : <Dot tone="ok" />}
          {stopping ? "Stopping…" : `Running · ${fmt.uptime(now - (instance.startedAt ?? now))}`}
        </Button>
        <Button size="lg" variant="danger" disabled={stopping} onClick={() => stop(pack)}>
          <Square size={12} fill="currentColor" />
          Stop
        </Button>
      </div>
    );
  }

  return (
    <div className="launch">
      {exportButton}
      {instance?.status === "crashed" && (
        <button type="button" className="crash-note" onClick={() => useStore.getState().setTab("console")} title="Open the console">
          <Dot tone="error" />
          Crashed{instance.exitCode != null ? ` (code ${instance.exitCode})` : ""}
        </button>
      )}
      {pack.profiles.length > 1 && (
        <select className="select" style={{ height: 32 }} aria-label="Profile" value={profileOf(pack)} onChange={(e) => setProfile(pack, e.target.value)}>
          {pack.profiles.map((p) => (
            <option key={p}>{p}</option>
          ))}
        </select>
      )}
      <Button size="lg" variant="primary" onClick={() => launch(pack)} title="Launch (Ctrl+Enter)">
        <Play size={13} fill="currentColor" />
        Launch
      </Button>
    </div>
  );
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const [done, setDone] = useState(false);
  return (
    <IconButton
      size="sm"
      label={label}
      onClick={async () => {
        if (await copy(text)) {
          setDone(true);
          setTimeout(() => setDone(false), 1200);
        }
      }}
    >
      {done ? <Check size={13} /> : <Copy size={13} />}
    </IconButton>
  );
}

function Overview({ pack, instance }: { pack: Pack; instance?: InstanceState }) {
  const content = useStore((s) => s.content[pack.id]);
  const activity = useStore((s) => s.activity);
  const recent = useMemo(() => activity.filter((a) => a.packId === pack.id).slice(0, 6), [activity, pack.id]);
  const { run, setView, setTab } = useStore.getState();
  const [resolving, setResolving] = useState(false);
  const now = useNow(15_000);
  const status = instance?.status ?? "stopped";
  const phaseIndex = status === "running" || status === "stopping" ? PHASES.length : PHASES.findIndex((p) => p.id === instance?.phase);
  const kinds = (["mod", "resourcepack", "shaderpack", "datapack"] as const)
    .map((k) => [content?.filter((c) => c.kind === k).length ?? 0, { mod: "mod", resourcepack: "resource pack", shaderpack: "shader", datapack: "data pack" }[k]] as const)
    .filter(([n]) => n > 0)
    .map(([n, label]) => `${n} ${label}${n === 1 ? "" : "s"}`);
  const updates = content?.filter((c) => c.update).length ?? 0;
  const instanceDir = `${pack.path}\\run\\${instance?.profile ?? pack.profiles[0]}`;

  return (
    <div className="page-body">
      <div className="cols">
        <div className="stack">
          <section className="panel" aria-label="Session">
            <div className="panel-body">
              <div className="state-line">
                <Dot
                  tone={status === "running" ? "ok" : status === "preparing" || status === "stopping" ? "warn" : status === "crashed" ? "error" : undefined}
                  pulse={status === "preparing" || status === "stopping"}
                />
                {stateLabel(instance)}
                {status === "preparing" && phaseIndex >= 0 && <span className="muted"> · {PHASES[phaseIndex].label}</span>}
              </div>
              <div className="state-detail">
                {status === "preparing" && (instance?.detail ?? "Preparing to launch")}
                {status === "running" && (instance?.agentPort ? "Game is up and the agent is accepting commands." : "Game is up.")}
                {status === "stopping" && "Closing the game."}
                {status === "stopped" && (instance?.exitCode != null ? `Last session exited with code ${instance.exitCode}.` : "Launch to download what's missing and start the game.")}
                {status === "crashed" && `Exited with code ${instance?.exitCode}. Check the console.`}
              </div>
              <div className="steps" aria-hidden={status === "stopped"}>
                {PHASES.map((p, i) => {
                  const done = i < phaseIndex;
                  const cur = i === phaseIndex;
                  return (
                    <div key={p.id} className={`step ${done ? "done" : cur ? "now" : ""}`}>
                      <div className={`bar ${cur && instance?.progress == null ? "indet" : ""}`}>
                        <i style={cur && instance?.progress != null ? { width: `${instance.progress * 100}%` } : undefined} />
                      </div>
                      <span>{p.label}</span>
                    </div>
                  );
                })}
              </div>
            </div>
            {(status === "running" || status === "stopping") && instance && (
              <div className="panel-body" style={{ borderTop: "1px solid var(--border)" }}>
                <dl className="kv">
                  <dt>Profile</dt>
                  <dd>{instance.profile}</dd>
                  {instance.pid != null && (
                    <>
                      <dt>Process</dt>
                      <dd className="mono">pid {instance.pid}</dd>
                    </>
                  )}
                  {instance.agentPort != null && (
                    <>
                      <dt>Agent</dt>
                      <dd>
                        <span className="mono selectable">127.0.0.1:{instance.agentPort}</span>
                        <Button size="sm" variant="ghost" onClick={() => setTab("agent")}>
                          Open
                        </Button>
                      </dd>
                    </>
                  )}
                </dl>
              </div>
            )}
          </section>

          <section className="panel">
            <div className="panel-head">
              <h2>Pack</h2>
            </div>
            <div className="panel-body">
              <dl className="kv">
                <dt>Minecraft</dt>
                <dd>{pack.minecraft}</dd>
                <dt>Loader</dt>
                <dd>
                  {cap(pack.loader)} {pack.loaderVersion}
                </dd>
                <dt>Content</dt>
                <dd>
                  <span className="truncate">{content ? kinds.join(", ") || "Empty" : "…"}</span>
                  {updates > 0 && (
                    <button type="button" className="update-pill" style={{ fontFamily: "var(--font)" }} onClick={() => setTab("content")}>
                      {updates} update{updates > 1 ? "s" : ""}
                    </button>
                  )}
                </dd>
                <dt>Profiles</dt>
                <dd>{pack.profiles.join(", ")}</dd>
                <dt>Location</dt>
                <dd>
                  <span className="mono selectable truncate">{pack.path}</span>
                  <CopyButton text={pack.path} label="Copy path" />
                </dd>
                <dt>Instance</dt>
                <dd>
                  <span className="mono selectable truncate">{instanceDir}</span>
                  <CopyButton text={instanceDir} label="Copy instance path" />
                </dd>
              </dl>
            </div>
          </section>
        </div>

        <div className="stack">
          <section className="panel">
            <div className="setting-row">
              <div className="grow">
                <div className="label row">
                  Lockfile
                  {pack.lock === "synced" ? <Badge tone="ok">In sync</Badge> : <Badge tone="warn">{pack.pendingChanges} unresolved</Badge>}
                </div>
                <div className="desc">
                  {pack.lock === "synced" ? "pack.lock matches pack.toml." : "pack.toml changed since the last resolve."}
                </div>
              </div>
              {pack.lock === "stale" && (
                <Button
                  disabled={resolving}
                  onClick={async () => {
                    setResolving(true);
                    await run(backend.resolve(pack.id), "Lockfile resolved");
                    setResolving(false);
                  }}
                >
                  <RefreshCw size={13} className={resolving ? "spin" : ""} />
                  Resolve
                </Button>
              )}
            </div>
          </section>

          <HotReloadPanel pack={pack} instance={instance} />

          <section className="panel">
            <div className="panel-head">
              <h2>Recent activity</h2>
              <Button size="sm" variant="ghost" onClick={() => setView("activity")}>
                View all
              </Button>
            </div>
            {recent.length === 0 ? (
              <div className="panel-body faint">Nothing has touched this pack yet.</div>
            ) : (
              <ul className="mini-list">
                {recent.map((a) => (
                  <li key={a.id}>
                    <StatusIcon status={a.status} />
                    <span className="truncate" title={a.summary}>
                      {a.summary}
                    </span>
                    <OriginBadge event={a} compact />
                    <time>{a.status === "running" ? "running" : fmt.ago(a.t, now)}</time>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}
