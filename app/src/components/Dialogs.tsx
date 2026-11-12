import { type FormEvent, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import {
  Activity,
  Bot,
  Boxes,
  Camera,
  ChevronDown,
  CornerDownLeft,
  Download,
  ExternalLink,
  Trash2,
  Gauge,
  LoaderCircle,
  Monitor,
  Moon,
  Package,
  Play,
  Plus,
  RefreshCw,
  Search,
  Settings,
  Square,
  SquareTerminal,
  Sun,
  Upload,
} from "lucide-react";
import { backend, type ContentKind, type Loader, PACK_FORMATS, type PackFormat, type SearchHit, type VersionSummary } from "../api";
import { agentAction, importPack, isActive, launch, openUrl, pageUrl, profileOf, stop, usePack, useStore } from "../store";
import { CustomContentForm, versionArg, VersionList } from "./ContentDialogs";
import { Badge, Button, fmt, IconButton, Kbd, Modal, Segmented } from "./ui";

// ---------------------------------------------------------- command palette

interface Command {
  id: string;
  group: string;
  label: string;
  icon: ReactNode;
  keys?: string[];
  run(): void;
}

function useCommands(): Command[] {
  const pack = usePack();
  const packs = useStore((s) => s.packs);
  const instance = useStore((s) => (pack ? s.instances[pack.id] : undefined));
  const connected = useStore((s) => (pack ? !!s.agent[pack.id] : false));

  return useMemo(() => {
    const s = useStore.getState();
    const out: Command[] = [];
    if (pack) {
      const g = pack.name;
      if (!isActive(instance)) out.push({ id: "launch", group: g, label: "Launch", icon: <Play size={15} />, keys: ["Ctrl", "↵"], run: () => launch(pack) });
      out.push({ id: "add", group: g, label: "Add content…", icon: <Plus size={15} />, run: () => s.open("add-content") });
      if (pack.lock === "stale")
        out.push({ id: "resolve", group: g, label: "Resolve lockfile", icon: <RefreshCw size={15} />, run: () => void s.run(backend.resolve(pack.id), "Lockfile resolved") });
      out.push({ id: "export", group: g, label: "Export modpack…", icon: <Upload size={15} />, run: () => s.open("export-pack") });
      if (connected) {
        out.push({ id: "shot", group: g, label: "Take screenshot", icon: <Camera size={15} />, run: () => { s.setTab("agent"); void s.screenshot(pack.id); } });
        out.push({ id: "reload", group: g, label: "Reload resources", icon: <RefreshCw size={15} />, run: () => void agentAction(pack, "reload_resources", {}, "Resources reloaded") });
      }
      // Stop is never the default (first) item: Ctrl+K, Enter must not kill a running game.
      if (isActive(instance)) out.push({ id: "stop", group: g, label: "Stop session", icon: <Square size={15} />, run: () => stop(pack) });
      else out.push({ id: "delete", group: g, label: "Delete pack…", icon: <Trash2 size={15} />, run: () => s.open("delete-pack") });
      out.push(
        { id: "t1", group: "Go to", label: "Overview", icon: <Gauge size={15} />, keys: ["Ctrl", "1"], run: () => s.setTab("overview") },
        { id: "t2", group: "Go to", label: "Content", icon: <Boxes size={15} />, keys: ["Ctrl", "2"], run: () => s.setTab("content") },
        { id: "t3", group: "Go to", label: "Console", icon: <SquareTerminal size={15} />, keys: ["Ctrl", "3"], run: () => s.setTab("console") },
        { id: "t4", group: "Go to", label: "Agent", icon: <Bot size={15} />, keys: ["Ctrl", "4"], run: () => s.setTab("agent") },
      );
    }
    out.push(
      { id: "activity", group: "Go to", label: "Activity", icon: <Activity size={15} />, run: () => s.setView("activity") },
      { id: "settings", group: "Go to", label: "Settings", icon: <Settings size={15} />, keys: ["Ctrl", ","], run: () => s.setView("settings") },
    );
    for (const p of packs)
      if (p.id !== pack?.id) out.push({ id: `p:${p.id}`, group: "Packs", label: p.name, icon: <Package size={15} />, run: () => s.selectPack(p.id) });
    out.push(
      { id: "new", group: "Packs", label: "New pack…", icon: <Plus size={15} />, run: () => s.open("new-pack") },
      { id: "import", group: "Packs", label: "Import modpack…", icon: <Download size={15} />, run: () => void importPack() },
      { id: "dark", group: "Theme", label: "Dark theme", icon: <Moon size={15} />, run: () => s.setTheme("dark") },
      { id: "light", group: "Theme", label: "Light theme", icon: <Sun size={15} />, run: () => s.setTheme("light") },
      { id: "system", group: "Theme", label: "Match system theme", icon: <Monitor size={15} />, run: () => s.setTheme("system") },
    );
    return out;
  }, [pack, packs, instance, connected]);
}

export function CommandPalette() {
  const isOpen = useStore((s) => s.dialog === "palette");
  const open = useStore((s) => s.open);
  const commands = useCommands();
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (isOpen) {
      setQuery("");
      setIndex(0);
    }
  }, [isOpen]);

  const results = useMemo(() => {
    const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
    return commands.filter((c) => tokens.every((t) => `${c.label} ${c.group}`.toLowerCase().includes(t)));
  }, [commands, query]);

  useEffect(() => {
    listRef.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  }, [index, results]);

  const exec = (c?: Command) => {
    if (!c) return;
    open(null);
    c.run();
  };

  return (
    <Modal open={isOpen} onClose={() => useStore.getState().dialog === "palette" && open(null)} label="Command palette">
      <div className="pal-input">
        <Search size={15} />
        <input
          autoFocus
          placeholder="Type a command or pack name"
          value={query}
          role="combobox"
          aria-expanded
          aria-controls="pal-list"
          aria-activedescendant={results[index] ? `cmd-${results[index].id}` : undefined}
          onChange={(e) => {
            setQuery(e.target.value);
            setIndex(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setIndex((i) => (results.length ? (i + 1) % results.length : 0));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setIndex((i) => (results.length ? (i - 1 + results.length) % results.length : 0));
            } else if (e.key === "Enter") {
              e.preventDefault();
              exec(results[index]);
            }
          }}
        />
      </div>
      <div className="pal-list" id="pal-list" role="listbox" ref={listRef}>
        {results.length === 0 && <div className="pal-empty">No matching commands</div>}
        {results.map((c, i) => (
          <div key={c.id}>
            {(i === 0 || results[i - 1].group !== c.group) && <div className="pal-group">{c.group}</div>}
            <button
              type="button"
              id={`cmd-${c.id}`}
              role="option"
              tabIndex={-1}
              aria-selected={i === index}
              className="pal-item"
              onMouseMove={() => i !== index && setIndex(i)}
              onClick={() => exec(c)}
            >
              {c.icon}
              {c.label}
              <span className="hint">
                {i === index && !c.keys ? <CornerDownLeft size={13} /> : c.keys?.map((k) => <Kbd key={k}>{k}</Kbd>)}
              </span>
            </button>
          </div>
        ))}
      </div>
    </Modal>
  );
}

// -------------------------------------------------------------- add content

const KINDS: { value: ContentKind; label: string }[] = [
  { value: "mod", label: "Mods" },
  { value: "resourcepack", label: "Resource packs" },
  { value: "shaderpack", label: "Shaders" },
  { value: "datapack", label: "Data packs" },
];

export function AddContentDialog() {
  const isOpen = useStore((s) => s.dialog === "add-content");
  const open = useStore((s) => s.open);
  const pack = usePack();
  const owned = useStore((s) => (pack ? s.content[pack.id]?.length : 0));
  const [kind, setKind] = useState<ContentKind>("mod");
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [adding, setAdding] = useState<string | null>(null);
  const [mode, setMode] = useState<"search" | "custom">("search");
  const [expanded, setExpanded] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen || !pack || mode !== "search") return;
    let stale = false;
    const t = setTimeout(async () => {
      try {
        const r = await backend.search(pack.id, query, kind);
        if (!stale) setHits(r);
      } catch {
        if (!stale) setHits([]);
      }
    }, 180);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [isOpen, pack, query, kind, owned, mode]);

  useEffect(() => {
    if (!isOpen) {
      setQuery("");
      setHits(null);
      setMode("search");
      setExpanded(null);
    }
  }, [isOpen]);

  if (!pack) return null;

  const add = async (hit: SearchHit, version?: VersionSummary) => {
    setAdding(hit.slug);
    await useStore.getState().run(backend.addContent(pack.id, hit, version && versionArg(hit, version)), `Added ${hit.name}${version ? ` ${version.version_number}` : ""}`);
    setAdding(null);
  };

  return (
    <Modal
      open={isOpen}
      onClose={() => useStore.getState().dialog === "add-content" && open(null)}
      label="Add content"
      title={`Add to ${pack.name}`}
      style={{ width: "min(620px, calc(100vw - 48px))" }}
    >
      <div className="modal-body" style={{ paddingBottom: 12 }}>
        <Segmented
          value={mode}
          onChange={setMode}
          label="Where to add from"
          options={[
            { value: "search", label: "Browse" },
            { value: "custom", label: "Custom file or URL" },
          ]}
        />
        {mode === "search" && (
          <div className="search">
            <Search size={14} />
            <input className="input" autoFocus placeholder="Search Modrinth and CurseForge" value={query} onChange={(e) => setQuery(e.target.value)} />
          </div>
        )}
        <div className="row">
          <Segmented value={kind} onChange={setKind} options={KINDS} label="Content type" />
          <span className="grow" />
          <span className="faint" style={{ fontSize: 12, textTransform: "capitalize" }}>
            {pack.minecraft} · {pack.loader}
          </span>
        </div>
      </div>
      {mode === "custom" ? (
        <CustomContentForm pack={pack} kind={kind} />
      ) : (
        <div className="hits">
          {hits === null && <div className="pal-empty">Searching…</div>}
          {hits?.length === 0 && <div className="pal-empty">No compatible results{query && ` for “${query}”`}</div>}
          {hits?.map((h) => {
            const url = h.url ?? pageUrl(h);
            const isOpenRow = expanded === h.slug;
            return (
              <div key={h.slug} className="hit-wrap">
                <div className="hit">
                  <span className="avatar">{h.name[0]}</span>
                  <div className="grow">
                    <div className="row">
                      <span style={{ fontWeight: 500 }}>{h.name}</span>
                      <Badge>{h.source === "modrinth" ? "Modrinth" : "CurseForge"}</Badge>
                      <span className="faint" style={{ fontSize: 12 }}>
                        {fmt.count(h.downloads)} downloads
                      </span>
                    </div>
                    <div className="desc truncate">{h.description}</div>
                  </div>
                  {h.latest && <span className="mono faint">{h.latest}</span>}
                  {url && (
                    <IconButton size="sm" label={`Open ${h.name} on ${h.source === "modrinth" ? "Modrinth" : "CurseForge"}`} onClick={() => openUrl(url)}>
                      <ExternalLink size={13} />
                    </IconButton>
                  )}
                  <Button size="sm" variant="ghost" aria-expanded={isOpenRow} title="More info and versions" onClick={() => setExpanded(isOpenRow ? null : h.slug)}>
                    Versions
                    <ChevronDown size={13} style={{ transform: isOpenRow ? "rotate(180deg)" : undefined }} />
                  </Button>
                  <Button size="sm" disabled={adding !== null} onClick={() => void add(h)} aria-label={`Add latest ${h.name}`} title="Add the latest compatible version">
                    {adding === h.slug ? <LoaderCircle size={13} className="spin" /> : <Plus size={13} />}
                    Add
                  </Button>
                </div>
                {isOpenRow && (
                  <div className="hit-more">
                    <p>{h.description}</p>
                    <div className="row faint" style={{ fontSize: 12 }}>
                      {h.author && <span>by {h.author}</span>}
                      {url && (
                        <button type="button" className="link" onClick={() => openUrl(url)}>
                          {url.replace(/^https?:\/\/(www\.)?/, "")}
                        </button>
                      )}
                    </div>
                    <VersionList pack={pack} item={h} busy={adding !== null} action="Add" onPick={(v) => void add(h, v)} />
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </Modal>
  );
}

// ----------------------------------------------------------------- new pack

const MC_VERSIONS = ["1.21.4", "1.21.1", "1.20.1"];
const LOADERS: Loader[] = ["fabric", "quilt", "neoforge", "forge", "vanilla"];

export function NewPackDialog() {
  const isOpen = useStore((s) => s.dialog === "new-pack");
  const open = useStore((s) => s.open);
  const [name, setName] = useState("");
  const [minecraft, setMinecraft] = useState(MC_VERSIONS[1]);
  const [loader, setLoader] = useState<Loader>("fabric");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (isOpen) setName("");
  }, [isOpen]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim() || busy) return;
    setBusy(true);
    const s = useStore.getState();
    const pack = await s.run(backend.createPack({ name, minecraft, loader }));
    setBusy(false);
    if (pack) {
      s.open(null);
      s.selectPack(pack.id);
    }
  };

  return (
    <Modal open={isOpen} onClose={() => useStore.getState().dialog === "new-pack" && open(null)} label="New pack" title="New pack" style={{ width: 420 }}>
      <form onSubmit={submit}>
        <div className="modal-body">
          <div className="field">
            <label htmlFor="np-name">Name</label>
            <input id="np-name" className="input" autoFocus placeholder="My pack" value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <div className="field-row">
            <div className="field">
              <label htmlFor="np-mc">Minecraft</label>
              <select id="np-mc" className="select" value={minecraft} onChange={(e) => setMinecraft(e.target.value)}>
                {MC_VERSIONS.map((v) => (
                  <option key={v}>{v}</option>
                ))}
              </select>
            </div>
            <div className="field">
              <label htmlFor="np-loader">Loader</label>
              <select id="np-loader" className="select" value={loader} onChange={(e) => setLoader(e.target.value as Loader)}>
                {LOADERS.map((l) => (
                  <option key={l} value={l}>
                    {l[0].toUpperCase() + l.slice(1)}
                  </option>
                ))}
              </select>
            </div>
          </div>
          <p className="faint" style={{ fontSize: 12 }}>
            Creates a folder with a <span className="mono">pack.toml</span> you, the CLI and AI agents can all edit.
          </p>
        </div>
        <div className="modal-foot">
          <Button variant="ghost" onClick={() => open(null)}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" disabled={!name.trim() || busy}>
            {busy && <LoaderCircle size={13} className="spin" />}
            Create pack
          </Button>
        </div>
      </form>
    </Modal>
  );
}

// -------------------------------------------------------------- export pack

const FORMAT_NOTES: Record<PackFormat, string> = {
  mrpack: "The Modrinth format. Also opens in other launchers that support .mrpack, and imports back into mcpack.",
  curseforge: "A CurseForge modpack zip with a manifest.json.",
};

export function ExportPackDialog() {
  const isOpen = useStore((s) => s.dialog === "export-pack");
  const open = useStore((s) => s.open);
  const pack = usePack();
  const [format, setFormat] = useState<PackFormat>("mrpack");
  const [profile, setProfile] = useState("");
  const [busy, setBusy] = useState(false);
  const packId = pack?.id;

  useEffect(() => {
    const p = useStore.getState().packs.find((x) => x.id === packId);
    if (isOpen && p) setProfile(profileOf(p));
  }, [isOpen, packId]);

  if (!pack) return null;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const s = useStore.getState();
    const name = pack.name.replace(/[<>:"/\\|?*]+/g, "").trim() || "modpack";
    const out = await s.run(backend.pickExportPath(name, format));
    if (!out) return;
    setBusy(true);
    const r = await s.run(backend.exportPack(pack.id, profile || profileOf(pack), format, out));
    setBusy(false);
    if (!r) return;
    s.open(null);
    const n = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`;
    s.toast("ok", `Exported ${n(r.files, "file")}${r.overrides ? ` and ${n(r.overrides, "override")}` : ""} to ${r.path}`);
    if (r.warnings.length) s.toast("info", r.warnings.length === 1 ? r.warnings[0] : `${r.warnings[0]} (+${r.warnings.length - 1} more warnings, see Activity)`);
  };

  return (
    <Modal
      open={isOpen}
      onClose={() => useStore.getState().dialog === "export-pack" && open(null)}
      label="Export modpack"
      title={`Export ${pack.name}`}
      style={{ width: 440 }}
    >
      <form onSubmit={submit}>
        <div className="modal-body">
          <div className="field">
            <label>Format</label>
            <Segmented value={format} onChange={setFormat} options={PACK_FORMATS} label="Format" />
          </div>
          {pack.profiles.length > 1 && (
            <div className="field">
              <label htmlFor="ex-profile">Profile</label>
              <select id="ex-profile" className="select" value={profile} onChange={(e) => setProfile(e.target.value)}>
                {pack.profiles.map((p) => (
                  <option key={p}>{p}</option>
                ))}
              </select>
            </div>
          )}
          <p className="faint" style={{ fontSize: 12 }}>
            {FORMAT_NOTES[format]}
          </p>
        </div>
        <div className="modal-foot">
          <Button variant="ghost" onClick={() => open(null)}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" disabled={busy}>
            {busy ? <LoaderCircle size={13} className="spin" /> : <Upload size={13} />}
            {busy ? "Exporting…" : "Choose location…"}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
