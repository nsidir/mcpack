// Dialog pieces for pack content and pack removal: version lists, the version picker for
// installed entries, the custom file/URL form, and the delete-pack confirmation.

import { type FormEvent, useEffect, useState } from "react";
import { ExternalLink, FolderOpen, LoaderCircle, Plus, Trash2 } from "lucide-react";
import { backend, type ContentKind, type ContentRef, type Pack, type VersionSummary } from "../api";
import { deletePack, isActive, isManaged, openUrl, pageUrl, usePack, useStore } from "../store";
import { Badge, Button, Modal } from "./ui";

// ----------------------------------------------------------------- versions

/** What `pack_add` wants as `version`: the version number on Modrinth, the file id on CurseForge. */
export const versionArg = (item: ContentRef, v: VersionSummary) => (item.source === "curseforge" ? v.id : v.version_number);

export function VersionList({
  pack,
  item,
  current,
  busy,
  action,
  onPick,
}: {
  pack: Pack;
  item: ContentRef;
  /** Installed version number, if any. */
  current?: string;
  busy: boolean;
  action: string;
  onPick: (v: VersionSummary) => void;
}) {
  const [versions, setVersions] = useState<VersionSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let stale = false;
    setVersions(null);
    setError(null);
    backend
      .versions(pack.id, { slug: item.slug, kind: item.kind, source: item.source })
      .then((v) => !stale && setVersions(v))
      .catch((e) => !stale && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      stale = true;
    };
  }, [pack.id, item.slug, item.kind, item.source]);

  if (error) return <div className="versions-note error">{error}</div>;
  if (!versions) return <div className="versions-note">Loading versions…</div>;
  if (versions.length === 0)
    return (
      <div className="versions-note">
        No versions for Minecraft {pack.minecraft} ({pack.loader}).
      </div>
    );

  return (
    <div className="versions" role="list" aria-label="Versions">
      {versions.map((v) => {
        const installed = current != null && (current === v.version_number || current === v.id);
        const date = new Date(v.date);
        const mc = v.game_versions.filter((g) => /^\d/.test(g));
        return (
          <div key={v.id} className="version" role="listitem">
            <span className="mono truncate" title={v.filename ?? undefined}>
              {v.version_number}
            </span>
            {v.version_type !== "release" && <Badge tone="warn">{v.version_type}</Badge>}
            {installed && <Badge tone="ok">Installed</Badge>}
            <span className="grow" />
            <span className="faint truncate" title={mc.join(", ")}>
              {mc.length > 2 ? `${mc[0]} – ${mc[mc.length - 1]}` : mc.join(", ")}
            </span>
            <span className="faint date">{Number.isNaN(date.getTime()) ? "" : date.toLocaleDateString()}</span>
            <Button size="sm" disabled={busy || installed} onClick={() => onPick(v)}>
              {action}
            </Button>
          </div>
        );
      })}
    </div>
  );
}

/** Change the version of something already in the pack. */
export function VersionDialog() {
  const isOpen = useStore((s) => s.dialog === "versions");
  const open = useStore((s) => s.open);
  const entry = useStore((s) => s.versionTarget);
  const pack = usePack();
  const [busy, setBusy] = useState(false);

  if (!pack || !entry) return null;
  const url = pageUrl(entry);

  const apply = async (action: Promise<void>, ok: string) => {
    setBusy(true);
    const s = useStore.getState();
    const done = await s.run(
      action.then(() => true),
      ok,
    );
    setBusy(false);
    if (done) s.open(null);
  };

  return (
    <Modal
      open={isOpen}
      onClose={() => useStore.getState().dialog === "versions" && open(null)}
      label="Choose version"
      title={`Version of ${entry.name}`}
      style={{ width: "min(560px, calc(100vw - 48px))" }}
    >
      <div className="modal-body" style={{ paddingBottom: 12 }}>
        <div className="row">
          <span className="muted">
            Installed: <span className="mono">{entry.version}</span>
          </span>
          <span className="grow" />
          {url && (
            <Button size="sm" variant="ghost" onClick={() => openUrl(url)}>
              <ExternalLink size={13} />
              Project page
            </Button>
          )}
        </div>
        {entry.dependency && (
          <p className="faint" style={{ fontSize: 12 }}>
            This was pulled in as a dependency. Choosing a version adds it to pack.toml as its own entry.
          </p>
        )}
        {isOpen && (
          <VersionList
            pack={pack}
            item={entry}
            current={entry.version}
            busy={busy}
            action="Use"
            onPick={(v) => void apply(backend.addContent(pack.id, entry, versionArg(entry, v)), `${entry.name} set to ${v.version_number}`)}
          />
        )}
      </div>
      <div className="modal-foot">
        <Button variant="ghost" onClick={() => open(null)}>
          Close
        </Button>
        <Button
          disabled={busy}
          title="Remove the pinned version and follow the newest compatible one"
          onClick={() => void apply(backend.updateContent(pack.id, entry.slug), `${entry.name} follows the latest version`)}
        >
          {busy && <LoaderCircle size={13} className="spin" />}
          Use latest compatible
        </Button>
      </div>
    </Modal>
  );
}

// ----------------------------------------------------------- custom content

/** Add something that is not on Modrinth/CurseForge: a local file or a direct download URL. */
export function CustomContentForm({ pack, kind }: { pack: Pack; kind: ContentKind }) {
  const [path, setPath] = useState("");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const from = url.trim() || path.trim();

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!from || busy) return;
    setBusy(true);
    const s = useStore.getState();
    const name = from.split(/[\\/?#]/).filter(Boolean).pop();
    const done = await s.run(
      backend.addCustom(pack.id, url.trim() ? { kind, url: url.trim() } : { kind, path: path.trim() }).then(() => true),
      `Added ${name}`,
    );
    setBusy(false);
    if (done) {
      setPath("");
      setUrl("");
    }
  };

  return (
    <form onSubmit={submit} style={{ borderTop: "1px solid var(--border)" }}>
      <div className="modal-body" style={{ paddingTop: 14 }}>
        <div className="field">
          <label htmlFor="cc-path">File on this computer</label>
          <div className="row">
            <input
              id="cc-path"
              className="input grow mono"
              placeholder="C:\path\to\my-mod.jar"
              value={path}
              disabled={!!url.trim()}
              onChange={(e) => setPath(e.target.value)}
            />
            <Button
              disabled={!!url.trim()}
              onClick={async () => {
                const f = await useStore.getState().run(backend.pickContentFile());
                if (f) setPath(f);
              }}
            >
              <FolderOpen size={13} />
              Browse…
            </Button>
          </div>
        </div>
        <div className="field">
          <label htmlFor="cc-url">Or a direct download URL</label>
          <input id="cc-url" className="input mono" placeholder="https://example.com/my-mod-1.0.jar" value={url} onChange={(e) => setUrl(e.target.value)} />
        </div>
        <p className="faint" style={{ fontSize: 12 }}>
          For mods you built yourself or that are not on Modrinth or CurseForge. Pick the content type above.
        </p>
      </div>
      <div className="modal-foot">
        <Button variant="primary" type="submit" disabled={!from || busy}>
          {busy ? <LoaderCircle size={13} className="spin" /> : <Plus size={13} />}
          Add to pack
        </Button>
      </div>
    </form>
  );
}

// -------------------------------------------------------------- delete pack

export function DeletePackDialog() {
  const isOpen = useStore((s) => s.dialog === "delete-pack");
  const open = useStore((s) => s.open);
  const pack = usePack();
  const instance = useStore((s) => (pack ? s.instances[pack.id] : undefined));
  const [files, setFiles] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (isOpen) setFiles(false);
  }, [isOpen]);

  if (!pack) return null;
  const managed = isManaged(pack);
  const active = isActive(instance);
  const withFiles = files && managed;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy || active) return;
    setBusy(true);
    const done = await deletePack(pack, withFiles);
    setBusy(false);
    if (done) useStore.getState().open(null);
  };

  return (
    <Modal open={isOpen} onClose={() => useStore.getState().dialog === "delete-pack" && open(null)} label="Delete pack" title={`Delete ${pack.name}?`} style={{ width: 460 }}>
      <form onSubmit={submit}>
        <div className="modal-body">
          <p>
            The pack is removed from the launcher.{" "}
            {withFiles ? "Its folder is deleted too. This cannot be undone." : "Its folder and files stay on disk."}
          </p>
          <label className={`check ${managed ? "" : "disabled"}`}>
            <input type="checkbox" checked={withFiles} disabled={!managed} onChange={(e) => setFiles(e.target.checked)} />
            <span>
              Also delete the pack folder
              <span className="desc">
                {managed
                  ? "Worlds, configs, screenshots and everything else in the folder below."
                  : "Not available: this pack is outside the launcher's own packs folder, so the launcher leaves its files alone."}
              </span>
              <span className="desc mono selectable">{pack.path}</span>
            </span>
          </label>
          {active && <p className="warn-text">The game is running for this pack. Stop it first.</p>}
        </div>
        <div className="modal-foot">
          <Button variant="ghost" onClick={() => open(null)}>
            Cancel
          </Button>
          <Button variant="danger" className="solid" type="submit" disabled={busy || active}>
            {busy ? <LoaderCircle size={13} className="spin" /> : <Trash2 size={13} />}
            {withFiles ? "Delete pack and files" : "Remove from launcher"}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
