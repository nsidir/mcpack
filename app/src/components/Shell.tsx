import { Activity, CircleAlert, CircleCheck, Download, Info, LoaderCircle, Minus, Plus, Search, Settings, Square, X } from "lucide-react";
import { isDesktop } from "../api";
import { importPack, isActive, stateLabel, useStore } from "../store";
import { Dot, fmt, IconButton, Kbd, useNow } from "./ui";

interface TauriWindow {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
}
const appWindow = (): TauriWindow =>
  (window as unknown as { __TAURI__: { window: { getCurrentWindow(): TauriWindow } } }).__TAURI__.window.getCurrentWindow();

export function Logo({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden>
      <rect width="16" height="16" rx="4" fill="currentColor" />
      <path d="M4.5 5.2 7.3 8l-2.8 2.8M8.8 10.9h2.9" stroke="var(--accent-fg)" strokeWidth="1.7" fill="none" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function Titlebar() {
  const open = useStore((s) => s.open);
  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="brand">
        <Logo />
        mcpack
      </div>
      <button type="button" className="palette-trigger" onClick={() => open("palette")}>
        <Search size={13} />
        <span className="grow" style={{ textAlign: "left" }}>
          Search or run a command
        </span>
        <Kbd>Ctrl</Kbd>
        <Kbd>K</Kbd>
      </button>
      <div className="win" data-tauri-drag-region>
        {isDesktop && (
          <>
            <button type="button" aria-label="Minimize" onClick={() => void appWindow().minimize()}>
              <Minus size={15} />
            </button>
            <button type="button" aria-label="Maximize" onClick={() => void appWindow().toggleMaximize()}>
              <Square size={11} />
            </button>
            <button type="button" className="close" aria-label="Close" onClick={() => void appWindow().close()}>
              <X size={15} />
            </button>
          </>
        )}
      </div>
    </header>
  );
}

export function Sidebar() {
  const packs = useStore((s) => s.packs);
  const packId = useStore((s) => s.packId);
  const view = useStore((s) => s.view);
  const instances = useStore((s) => s.instances);
  const running = useStore((s) => s.activity.filter((a) => a.status === "running").length);
  const importing = useStore((s) => s.importing);
  const { selectPack, setView, open } = useStore.getState();

  return (
    <nav className="sidebar" aria-label="Primary">
      <div className="side-head">
        <span className="eyebrow">Packs</span>
        <span style={{ display: "flex", gap: 2 }}>
          <IconButton label={importing ? "Importing…" : "Import modpack (.mrpack, CurseForge .zip)"} size="sm" disabled={importing} onClick={() => void importPack()}>
            {importing ? <LoaderCircle size={14} className="spin" /> : <Download size={14} />}
          </IconButton>
          <IconButton label="New pack" size="sm" onClick={() => open("new-pack")}>
            <Plus size={14} />
          </IconButton>
        </span>
      </div>
      <div className="side-list">
        {packs.map((p) => {
          const st = instances[p.id]?.status;
          return (
            <button
              key={p.id}
              type="button"
              className="nav"
              title={st && st !== "stopped" ? `${p.name} · ${stateLabel(instances[p.id])}` : undefined}
              aria-current={view === "pack" && p.id === packId ? "page" : undefined}
              onClick={() => selectPack(p.id)}
            >
              <span className="grow">
                <span className="name truncate" style={{ display: "block" }}>
                  {p.name}
                </span>
                <span className="meta">
                  {p.minecraft} · {p.loader}
                </span>
              </span>
              {st === "running" && <Dot tone="ok" />}
              {(st === "preparing" || st === "stopping") && <Dot tone="warn" pulse />}
              {st === "crashed" && <Dot tone="error" />}
            </button>
          );
        })}
      </div>
      <div className="side-foot">
        <button type="button" className="nav" aria-current={view === "activity" ? "page" : undefined} onClick={() => setView("activity")}>
          <Activity size={15} />
          Activity
          {running > 0 && <span className="count">{running} running</span>}
        </button>
        <button type="button" className="nav" aria-current={view === "settings" ? "page" : undefined} onClick={() => setView("settings")}>
          <Settings size={15} />
          Settings
        </button>
      </div>
    </nav>
  );
}

export function StatusBar() {
  const info = useStore((s) => s.info);
  const packs = useStore((s) => s.packs);
  const instances = useStore((s) => s.instances);
  const last = useStore((s) => s.activity[0]);
  const { selectPack, setView } = useStore.getState();
  const now = useNow(15_000);
  const live = packs.filter((p) => isActive(instances[p.id]));

  return (
    <footer className="statusbar">
      {info?.mode === "mock" ? (
        <span className="item" title="The UI is running against a simulated backend. Nothing is downloaded or launched.">
          <Dot tone="warn" />
          Simulated backend
        </span>
      ) : (
        <span className="item">
          <Dot tone="ok" />
          mcpack {info?.version}
        </span>
      )}
      {live.map((p) => {
        const i = instances[p.id]!;
        return (
          <button key={p.id} type="button" onClick={() => selectPack(p.id)}>
            <Dot tone={i.status === "running" ? "ok" : "warn"} pulse={i.status !== "running"} />
            <span className="truncate">
              {p.name} · {stateLabel(i)}
              {i.status === "running" && i.agentPort != null && ` · agent :${i.agentPort}`}
              {i.status === "preparing" && i.detail && ` · ${i.detail}`}
            </span>
          </button>
        );
      })}
      <span className="grow" />
      {last && (
        <button type="button" onClick={() => setView("activity")} title="Open activity">
          <span className="truncate">
            {last.origin === "mcp" ? (last.client ?? "mcp") : last.origin === "cli" ? "cli" : "you"} · <span className="mono" style={{ fontSize: 11 }}>{last.tool}</span> ·{" "}
            {last.status === "running" ? "running" : fmt.ago(last.t, now)}
          </span>
        </button>
      )}
    </footer>
  );
}

export function Toasts() {
  const toasts = useStore((s) => s.toasts);
  const dismiss = useStore((s) => s.dismiss);
  return (
    <div className="toasts" role="status" aria-live="polite">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`}>
          {t.kind === "ok" ? <CircleCheck className="lead" size={15} /> : t.kind === "error" ? <CircleAlert className="lead" size={15} /> : <Info className="lead" size={15} />}
          <div className="text">
            {t.text}
            {t.code && (
              <div className="mono faint" style={{ marginTop: 2 }}>
                code: {t.code}
              </div>
            )}
          </div>
          <IconButton label="Dismiss" size="sm" onClick={() => dismiss(t.id)}>
            <X size={13} />
          </IconButton>
        </div>
      ))}
    </div>
  );
}
