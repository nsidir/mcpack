// Hot reload panel (pack Overview): the watcher switch, what happens on changes that need a
// restart, mod-code hot-swap, and a live feed of what the watcher detected and did.

import { CircleAlert, CircleCheck, Flame, LoaderCircle, RefreshCw, RotateCw } from "lucide-react";
import { useState } from "react";
import { backend, type ConfigReload, type InstanceState, type Pack, type RestartPolicy, type WatchEvent } from "../api";
import { Badge, Button, fmt, Segmented, Switch, useNow } from "../components/ui";
import { describeWatch, pendingRestart, profileOf, restartGame, useStore } from "../store";

export function HotReloadPanel({ pack, instance }: { pack: Pack; instance?: InstanceState }) {
  const events = useStore((s) => s.watchEvents[pack.id]);
  const { run } = useStore.getState();
  const now = useNow(15_000);
  const [open, setOpen] = useState<string | null>(null);
  const running = instance?.status === "running";
  const pending = pendingRestart(events, instance);
  const shown = (events ?? []).slice(0, 12);
  // Hot-swap is on in pack.toml but this session was launched without it.
  const hotswapNeedsRelaunch = !!pack.hotswap && running && instance?.hotswapPort == null;

  return (
    <section className="panel" aria-label="Hot reload">
      <div className="setting-row">
        <div className="grow">
          <div className="label row">
            Hot reload
            {pack.watch && <Badge tone="ok">Watching</Badge>}
          </div>
          <div className="desc">Watches the pack folder and local mod files, and applies changes to the running game.</div>
        </div>
        <Switch checked={pack.watch} label="Hot reload" onChange={(on) => void run(backend.setWatch(pack.id, on, profileOf(pack)))} />
      </div>

      <div className="setting-row">
        <div className="grow">
          <div className="label">When a change needs a restart</div>
          <div className="desc">New or removed mods, mixins, JVM and loader settings, options.txt, and most configs.</div>
        </div>
        <Segmented<RestartPolicy>
          label="When a change needs a restart"
          value={pack.restartPolicy ?? "ask"}
          onChange={(value) => void run(backend.setDev(pack.id, { key: "restart", value }))}
          options={[
            { value: "ask", label: "Ask me" },
            { value: "auto", label: "Restart automatically" },
          ]}
        />
      </div>

      <div className="setting-row">
        <div className="grow">
          <div className="label">Config changes</div>
          <div className="desc">Some mods reload their configs with resources (F3+T); most only read them at startup.</div>
        </div>
        <Segmented<ConfigReload>
          label="Config changes"
          value={pack.configReload ?? "restart"}
          onChange={(value) => void run(backend.setDev(pack.id, { key: "config_reload", value }))}
          options={[
            { value: "restart", label: "Restart" },
            { value: "resources", label: "Reload resources" },
            { value: "none", label: "Files only" },
          ]}
        />
      </div>

      <div className="setting-row">
        <div className="grow">
          <div className="label row">
            Hot-swap mod code
            <Badge tone="warn">Experimental</Badge>
            {running && instance?.hotswapPort != null && <Badge tone="ok">Active</Badge>}
          </div>
          <div className="desc">
            Swaps the changed code of a rebuilt mod into the running game instead of restarting. Works for edits inside methods; new classes, mixins
            and added mods still need a restart. Starts the game with a local Java debug port.
            {hotswapNeedsRelaunch && <span className="warn-text"> Takes effect the next time the game starts.</span>}
          </div>
        </div>
        <Switch checked={!!pack.hotswap} label="Hot-swap mod code" onChange={(value) => void run(backend.setDev(pack.id, { key: "hotswap", value }))} />
      </div>

      {pending && (
        <div className="restart-banner" role="status">
          <CircleAlert size={15} />
          <div className="grow">
            <div style={{ fontWeight: 500 }}>Restart needed to apply changes</div>
            <ul>
              {(pending.restart_reasons ?? []).map((r) => (
                <li key={r}>{r}</li>
              ))}
            </ul>
            {pending.hotswap_failed && <div className="faint">Not hot-swapped: {pending.hotswap_failed}</div>}
          </div>
          <Button variant="primary" onClick={() => void restartGame(pack)}>
            <RotateCw size={13} />
            Restart now
          </Button>
        </div>
      )}

      <div className="panel-head" style={{ borderTop: "1px solid var(--border)" }}>
        <h2>Detected changes</h2>
      </div>
      {shown.length === 0 ? (
        <div className="panel-body faint">
          {pack.watch ? "Nothing yet. Save a file in the pack folder (configs, resource packs, mods…) and it shows up here." : "Turn on hot reload to see file changes here."}
        </div>
      ) : (
        <ul className="watch-feed">
          {shown.map((e) => {
            const key = `${e.seq}-${e.at_ms}`;
            const detail = details(e);
            const expanded = open === key;
            return (
              <li key={key} className={`watch-item ${e.action}`}>
                <button type="button" className="watch-row" aria-expanded={detail.length ? expanded : undefined} onClick={() => setOpen(expanded ? null : key)}>
                  <WatchIcon action={e.action} />
                  <span className="grow" style={{ minWidth: 0 }}>
                    <span className="truncate" style={{ display: "block" }}>
                      {describeWatch(e)}
                    </span>
                    <span className="mono faint truncate" style={{ display: "block", fontSize: 11 }} title={e.paths.join("\n")}>
                      {e.paths.slice(0, 2).map(shortPath).join(", ")}
                      {e.paths.length > 2 && ` +${e.paths.length - 2}`}
                    </span>
                  </span>
                  <time className="faint">{fmt.ago(e.at_ms, now)}</time>
                </button>
                {expanded && detail.length > 0 && (
                  <ul className="watch-detail">
                    {detail.map((d) => (
                      <li key={d}>{d}</li>
                    ))}
                  </ul>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function WatchIcon({ action }: { action: WatchEvent["action"] }) {
  switch (action) {
    case "applying":
      return <LoaderCircle size={14} className="spin" />;
    case "hotswapped":
      return <Flame size={14} className="tone-ok" />;
    case "live_reload":
    case "synced":
    case "no_change":
      return <CircleCheck size={14} className="tone-ok" />;
    case "restarted":
      return <RefreshCw size={14} className="tone-warn" />;
    case "restart_needed":
      return <CircleAlert size={14} className="tone-warn" />;
    case "error":
      return <CircleAlert size={14} className="tone-error" />;
  }
}

/** The expandable lines under a feed entry. */
function details(e: WatchEvent): string[] {
  const out: string[] = [];
  for (const r of e.restart_reasons ?? []) out.push(`Needs a restart: ${r}`);
  if (e.hotswap_failed) out.push(`Not hot-swapped: ${e.hotswap_failed}`);
  if (e.hotswap) {
    const h = e.hotswap;
    if (h.classes.length) out.push(`Swapped: ${h.classes.join(", ")}`);
    if (h.not_loaded?.length) out.push(`Not loaded yet, old code until restart: ${h.not_loaded.join(", ")}`);
    if (h.pending_resources?.length) out.push(`Assets inside the jar apply at the next launch: ${h.pending_resources.join(", ")}`);
  }
  const s = e.sync;
  if (s) {
    if (s.added?.length) out.push(`Added: ${s.added.join(", ")}`);
    if (s.updated?.length) out.push(`Updated: ${s.updated.join(", ")}`);
    if (s.removed?.length) out.push(`Removed: ${s.removed.join(", ")}`);
    if (s.config_changed?.length) out.push(`Configs: ${s.config_changed.join(", ")}`);
    if (s.overrides_changed?.length) out.push(`Files: ${s.overrides_changed.join(", ")}`);
    if (s.held?.length) out.push(`Kept for the next launch: ${s.held.join(", ")}`);
    for (const w of s.warnings ?? []) out.push(`Warning: ${w}`);
  }
  if (e.error && e.action !== "error") out.push(`Error: ${e.error}`);
  return out;
}

function shortPath(p: string) {
  const parts = p.split(/[\\/]/);
  return parts.length > 3 ? `…/${parts.slice(-2).join("/")}` : p;
}
