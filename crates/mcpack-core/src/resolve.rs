//! Turn `pack.toml` specs into a fully pinned `pack.lock` (with dependency closure).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::http::DownloadItem;
use crate::meta::loader::{self, LoaderKind};
use crate::pack::lock::{LockEntry, LockFile};
use crate::pack::schema::{ModKind, ModSpec, Source};
use crate::pack::Pack;
use crate::source::curseforge::CurseForge;
use crate::source::modrinth::Modrinth;
use crate::source::{DepRef, ResolvedVersion};
use crate::util;
use crate::Ctx;

/// Which entries to re-resolve instead of reusing the existing lock.
#[derive(Debug, Clone, Default)]
pub struct UpdatePolicy {
    pub all: bool,
    /// Mod identifiers (slug/id/filename) to update.
    pub only: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManualDownload {
    pub name: String,
    pub filename: String,
    pub page_url: Option<String>,
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryChange {
    pub key: String,
    pub name: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveReport {
    pub minecraft: String,
    pub loader: LoaderKind,
    pub loader_version: String,
    pub entries: usize,
    pub added: Vec<EntryChange>,
    pub removed: Vec<EntryChange>,
    pub changed: Vec<EntryChange>,
    pub warnings: Vec<String>,
    pub manual_downloads: Vec<ManualDownload>,
    pub lock_written: bool,
}

struct Resolver<'a> {
    ctx: &'a Ctx,
    pack_dir: &'a Path,
    mc: String,
    loader: LoaderKind,
    old: Option<&'a LockFile>,
    policy: UpdatePolicy,
    mr: Modrinth,
    cf: Option<CurseForge>,
    /// Exact versions pinned by top-level specs (`source:id` -> version), so a dependency
    /// that is also pinned (e.g. by an imported CurseForge manifest) gets that version, not latest.
    pins: HashMap<String, String>,
    entries: BTreeMap<String, LockEntry>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

pub async fn resolve(ctx: &Ctx, pack: &Pack, old: Option<&LockFile>, policy: UpdatePolicy) -> Result<(LockFile, ResolveReport)> {
    let mc = pack.file.pack.minecraft.clone();
    let loader_spec = &pack.file.pack.loader;
    let compatible_old = old.filter(|o| o.minecraft == mc && o.loader == loader_spec.kind);

    let loader_version = if loader_spec.kind == LoaderKind::Vanilla {
        String::new()
    } else if (loader_spec.version == "latest" || loader_spec.version.is_empty())
        && !policy.all
        && compatible_old.is_some_and(|o| !o.loader_version.is_empty())
    {
        compatible_old.map(|o| o.loader_version.clone()).unwrap_or_default()
    } else {
        loader::resolve_loader_version(&ctx.http, loader_spec.kind, &mc, &loader_spec.version).await?
    };

    let needs_cf = pack.all_specs().iter().any(|s| s.source == Source::Curseforge);
    let cf = if needs_cf { Some(CurseForge::new(ctx.http.clone(), ctx.cfg().curseforge_api_key)?) } else { None };
    let mut pins = HashMap::new();
    for s in pack.all_specs() {
        if let (Source::Modrinth | Source::Curseforge, Some(id)) = (s.source, &s.id) {
            let v = s.version_spec();
            if !v.is_empty() && v != "latest" {
                pins.insert(format!("{}:{id}", s.source.as_str()), v.to_string());
            }
        }
    }
    if let Some(cf) = &cf {
        // Batch-fetch every pinned CurseForge file (modpack imports pin hundreds).
        let ids: Vec<u64> = pack
            .all_specs()
            .iter()
            .filter(|s| s.source == Source::Curseforge)
            .filter_map(|s| s.version.as_deref()?.parse().ok())
            .collect();
        if !ids.is_empty() {
            if let Err(e) = cf.prefetch(&ids).await {
                tracing::debug!("CurseForge batch prefetch failed, falling back to per-file requests: {e}");
            }
        }
    }

    let mut r = Resolver {
        ctx,
        pack_dir: &pack.dir,
        mc: mc.clone(),
        loader: loader_spec.kind,
        old: compatible_old,
        policy,
        mr: Modrinth::new(ctx.http.clone()),
        cf,
        pins,
        entries: BTreeMap::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    if loader_spec.kind == LoaderKind::Vanilla
        && pack.all_specs().iter().any(|s| s.kind.unwrap_or_default() == ModKind::Mod && s.source != Source::Modrinth)
    {
        r.warnings.push("pack is vanilla but contains mods; set pack.loader.kind = \"fabric\"".into());
    }

    for spec in pack.all_specs() {
        if let Err(e) = r.top_level(&spec).await {
            r.errors.push(format!("{}: {e}", describe(&spec)));
        }
    }

    if !r.errors.is_empty() {
        return Err(Error::Resolve(r.errors.join("\n")));
    }

    r.fill_manual_from_modrinth().await;

    // Incompatibility report.
    let keys: BTreeSet<String> = r.entries.keys().cloned().collect();
    for e in r.entries.values() {
        for inc in &e.incompatible {
            if keys.contains(inc) {
                let other = r.entries.get(inc).map(|o| o.name.clone()).unwrap_or_else(|| inc.clone());
                r.warnings.push(format!("{} declares itself incompatible with {}", e.name, other));
            }
        }
    }

    let lock = LockFile {
        lock_version: 1,
        inputs: pack.resolution_inputs(),
        minecraft: mc.clone(),
        loader: loader_spec.kind,
        loader_version: loader_version.clone(),
        entries: r.entries.values().cloned().collect(),
    };

    let (added, removed, changed) = diff(old, &lock);
    let manual_downloads = lock
        .entries
        .iter()
        .filter(|e| e.manual)
        .map(|e| ManualDownload {
            name: e.name.clone(),
            filename: e.filename.clone(),
            page_url: e.page_url.clone(),
            sha1: e.sha1.clone(),
        })
        .collect();
    let report = ResolveReport {
        minecraft: mc,
        loader: loader_spec.kind,
        loader_version,
        entries: lock.entries.len(),
        added,
        removed,
        changed,
        warnings: r.warnings,
        manual_downloads,
        lock_written: false,
    };
    Ok((lock, report))
}

fn describe(s: &ModSpec) -> String {
    format!("{}:{}", s.source.as_str(), s.ident())
}

fn diff(old: Option<&LockFile>, new: &LockFile) -> (Vec<EntryChange>, Vec<EntryChange>, Vec<EntryChange>) {
    let empty = LockFile::default();
    let old = old.unwrap_or(&empty);
    let om = old.by_key();
    let nm = new.by_key();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (k, e) in &nm {
        match om.get(k) {
            None => added.push(EntryChange { key: k.to_string(), name: e.name.clone(), from: None, to: Some(e.version_number.clone()) }),
            Some(o) if o.version_id != e.version_id || o.sha1 != e.sha1 => changed.push(EntryChange {
                key: k.to_string(),
                name: e.name.clone(),
                from: Some(o.version_number.clone()),
                to: Some(e.version_number.clone()),
            }),
            _ => {}
        }
    }
    for (k, o) in &om {
        if !nm.contains_key(k) {
            removed.push(EntryChange { key: k.to_string(), name: o.name.clone(), from: Some(o.version_number.clone()), to: None });
        }
    }
    (added, removed, changed)
}

impl<'a> Resolver<'a> {
    fn wants_update(&self, spec: &ModSpec) -> bool {
        self.policy.all || self.policy.only.iter().any(|i| spec.matches(i))
    }

    async fn top_level(&mut self, spec: &ModSpec) -> Result<()> {
        let fp = spec.resolution_fingerprint();
        // Reuse the previous pin when inputs are unchanged.
        if !self.wants_update(spec) && spec.source != Source::File {
            if let Some(old) = self.old {
                if let Some(e) = old.entries.iter().find(|e| e.requested_by.contains(&fp)) {
                    let key = e.key.clone();
                    self.adopt_old(&key).await?;
                    self.mark_requested(&key, &fp);
                    return Ok(());
                }
            }
        }
        let key = match spec.source {
            Source::Modrinth | Source::Curseforge => {
                let id = spec.id.clone().unwrap_or_default();
                let rv = match spec.source {
                    Source::Modrinth => {
                        self.mr
                            .resolve(&id, spec.version_spec(), spec.kind, spec.channel, &self.mc, self.loader, &mut self.warnings)
                            .await?
                    }
                    _ => {
                        let cf = self.cf.clone().ok_or_else(|| Error::Config("CurseForge client unavailable".into()))?;
                        cf.resolve(&id, spec.version_spec(), spec.kind, spec.channel, &self.mc, self.loader, &mut self.warnings)
                            .await?
                    }
                };
                let with_optional = spec.optional_deps.unwrap_or(false);
                self.insert_resolved(rv, with_optional).await?
            }
            Source::Url => self.url_entry(spec).await?,
            Source::File => self.file_entry(spec)?,
        };
        self.mark_requested(&key, &fp);
        Ok(())
    }

    fn mark_requested(&mut self, key: &str, fp: &str) {
        if let Some(e) = self.entries.get_mut(key) {
            if !e.requested_by.iter().any(|f| f == fp) {
                e.requested_by.push(fp.to_string());
            }
        }
    }

    /// Copy an entry (and its dependency closure) from the old lock.
    async fn adopt_old(&mut self, key: &str) -> Result<()> {
        let old = match self.old {
            Some(o) => o,
            None => return Ok(()),
        };
        let mut queue = VecDeque::from([key.to_string()]);
        while let Some(k) = queue.pop_front() {
            if self.entries.contains_key(&k) {
                continue;
            }
            match old.get(&k) {
                Some(e) => {
                    let mut e = e.clone();
                    e.requested_by.clear();
                    for d in &e.deps {
                        queue.push_back(d.clone());
                    }
                    self.entries.insert(k, e);
                }
                None => {
                    // Dep missing from old lock: resolve fresh.
                    let (src, pid) = k.split_once(':').unwrap_or(("modrinth", k.as_str()));
                    let dep = DepRef { project_id: Some(pid.to_string()), version_id: None };
                    let source = Source::parse(src).unwrap_or(Source::Modrinth);
                    let rv = self.fetch_dep(source, &dep).await?;
                    Box::pin(self.insert_resolved(rv, false)).await?;
                }
            }
        }
        Ok(())
    }

    async fn fetch_dep(&mut self, source: Source, dep: &DepRef) -> Result<ResolvedVersion> {
        if let Some(pid) = &dep.project_id {
            if let Some(v) = self.pins.get(&format!("{}:{pid}", source.as_str())).cloned() {
                return match source {
                    Source::Curseforge => {
                        let cf = self.cf.clone().ok_or_else(|| Error::Config("CurseForge client unavailable".into()))?;
                        cf.resolve(pid, &v, None, None, &self.mc, self.loader, &mut self.warnings).await
                    }
                    _ => self.mr.resolve(pid, &v, None, None, &self.mc, self.loader, &mut self.warnings).await,
                };
            }
        }
        match source {
            Source::Curseforge => {
                let cf = self.cf.clone().ok_or_else(|| Error::Config("CurseForge client unavailable".into()))?;
                cf.resolve_dep(dep, &self.mc, self.loader, &mut self.warnings).await
            }
            _ => self.mr.resolve_dep(dep, &self.mc, self.loader, &mut self.warnings).await,
        }
    }

    /// Insert a resolved version and walk its required (and optionally optional) dependencies.
    async fn insert_resolved(&mut self, rv: ResolvedVersion, with_optional: bool) -> Result<String> {
        let root_key = format!("{}:{}", rv.source.as_str(), rv.project_id);
        let mut queue: VecDeque<(ResolvedVersion, bool)> = VecDeque::from([(rv, with_optional)]);
        while let Some((rv, opt)) = queue.pop_front() {
            let key = format!("{}:{}", rv.source.as_str(), rv.project_id);
            if self.entries.contains_key(&key) {
                continue;
            }
            let source = rv.source;
            let mut deps = rv.required.clone();
            if opt {
                deps.extend(rv.optional.iter().cloned());
            }
            let mut entry = to_entry(&rv);
            entry.incompatible = rv
                .incompatible
                .iter()
                .filter_map(|d| d.project_id.as_ref().map(|p| format!("{}:{p}", source.as_str())))
                .collect();
            self.entries.insert(key.clone(), entry);
            let mut dep_keys = Vec::new();
            for dep in deps {
                // Resolve project id for version-only deps.
                let dep_key = match &dep.project_id {
                    Some(p) => format!("{}:{p}", source.as_str()),
                    None => {
                        match self.fetch_dep(source, &dep).await {
                            Ok(d) => {
                                let k = format!("{}:{}", d.source.as_str(), d.project_id);
                                if !self.entries.contains_key(&k) {
                                    queue.push_back((d, false));
                                }
                                dep_keys.push(k);
                                continue;
                            }
                            Err(e) => {
                                self.errors.push(format!("dependency of {}: {e}", rv.name));
                                continue;
                            }
                        }
                    }
                };
                dep_keys.push(dep_key.clone());
                if self.entries.contains_key(&dep_key) || queue.iter().any(|(q, _)| format!("{}:{}", q.source.as_str(), q.project_id) == dep_key) {
                    continue;
                }
                // Prefer the previous pin for deps unless updating everything.
                if !self.policy.all {
                    if let Some(old) = self.old {
                        if old.get(&dep_key).is_some() {
                            Box::pin(self.adopt_old(&dep_key)).await?;
                            continue;
                        }
                    }
                }
                match self.fetch_dep(source, &dep).await {
                    Ok(d) => queue.push_back((d, false)),
                    Err(e) => self.errors.push(format!("required dependency {dep_key} of {}: {e}", rv.name)),
                }
            }
            if let Some(e) = self.entries.get_mut(&key) {
                dep_keys.sort();
                dep_keys.dedup();
                e.deps = dep_keys;
            }
        }
        Ok(root_key)
    }

    /// CurseForge files whose authors disallow third-party downloads have no URL. Many are
    /// mirrored byte-for-byte on Modrinth, so look them up by SHA-1 and download from there;
    /// whatever is still missing must be downloaded manually.
    async fn fill_manual_from_modrinth(&mut self) {
        let hashes: Vec<String> = self
            .entries
            .values()
            .filter(|e| e.manual && e.source == Source::Curseforge)
            .filter_map(|e| e.sha1.clone())
            .collect();
        if !hashes.is_empty() {
            match self.mr.files_by_sha1(&hashes).await {
                Ok(found) => {
                    for e in self.entries.values_mut().filter(|e| e.manual && e.source == Source::Curseforge) {
                        if let Some(f) = e.sha1.as_ref().and_then(|h| found.get(&h.to_ascii_lowercase())) {
                            e.url = Some(f.url.clone());
                            e.size = e.size.or(f.size);
                            e.manual = false;
                            self.warnings.push(format!("{} ({}): CurseForge disallows API downloads; using the identical file from Modrinth", e.name, e.filename));
                        }
                    }
                }
                Err(err) => self.warnings.push(format!("Modrinth hash lookup for CurseForge-restricted files failed: {err}")),
            }
        }
        for e in self.entries.values().filter(|e| e.manual && e.source == Source::Curseforge) {
            self.warnings.push(format!(
                "{} ({}) disallows third-party downloads; download it manually from {} and place it in <pack>/manual/ or your Downloads folder",
                e.name,
                e.filename,
                e.page_url.as_deref().unwrap_or("CurseForge")
            ));
        }
    }

    async fn url_entry(&mut self, spec: &ModSpec) -> Result<String> {
        let url = spec.url.clone().unwrap_or_default();
        let filename = spec.filename.clone().unwrap_or_else(|| {
            let last = url.split('?').next().unwrap_or(&url).rsplit('/').next().unwrap_or("download.jar");
            util::safe_component(last)
        });
        let key = format!("url:{filename}");
        // Reuse hash from old lock when URL unchanged.
        let old_sha = self
            .old
            .and_then(|o| o.get(&key))
            .filter(|e| e.url.as_deref() == Some(url.as_str()))
            .and_then(|e| e.sha1.clone());
        let sha1 = match spec.sha1.clone().or(old_sha) {
            Some(s) => s,
            None => {
                // Download once to learn the hash, then keep it in the store.
                let tmp = self.ctx.paths.cache().join("tmp").join(&filename);
                self.ctx
                    .http
                    .download(&DownloadItem { url: url.clone(), dest: tmp.clone(), sha1: None, size: None })
                    .await?;
                let sha = util::sha1_file(&tmp)?;
                let dest = self.ctx.paths.stored_file(&sha, &filename);
                util::ensure_parent(&dest)?;
                util::replace_file(&tmp, &dest)?;
                sha
            }
        };
        let size = std::fs::metadata(self.ctx.paths.stored_file(&sha1, &filename)).ok().map(|m| m.len());
        let entry = LockEntry {
            key: key.clone(),
            source: Source::Url,
            kind: spec.kind.unwrap_or_default(),
            project_id: filename.clone(),
            slug: None,
            name: filename.clone(),
            version_id: sha1.clone(),
            version_number: spec.version.clone().unwrap_or_else(|| "url".into()),
            filename,
            url: Some(url),
            sha1: Some(sha1),
            size,
            ..Default::default()
        };
        self.entries.insert(key.clone(), entry);
        Ok(key)
    }

    fn file_entry(&mut self, spec: &ModSpec) -> Result<String> {
        let rel = spec.path.clone().unwrap_or_default();
        let abs = util::resolve_rel(self.pack_dir, &rel);
        if !abs.exists() {
            self.warnings.push(format!("local file {} does not exist yet", abs.display()));
        }
        let filename = spec
            .filename
            .clone()
            .or_else(|| abs.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "local.jar".into());
        let key = format!("file:{rel}");
        let entry = LockEntry {
            key: key.clone(),
            source: Source::File,
            kind: spec.kind.unwrap_or_default(),
            project_id: rel.clone(),
            name: filename.clone(),
            version_number: "local".into(),
            filename,
            path: Some(rel),
            ..Default::default()
        };
        self.entries.insert(key.clone(), entry);
        Ok(key)
    }
}

fn to_entry(rv: &ResolvedVersion) -> LockEntry {
    LockEntry {
        key: format!("{}:{}", rv.source.as_str(), rv.project_id),
        source: rv.source,
        kind: rv.kind,
        project_id: rv.project_id.clone(),
        slug: rv.slug.clone(),
        name: rv.name.clone(),
        version_id: rv.version_id.clone(),
        version_number: rv.version_number.clone(),
        filename: util::safe_component(&rv.filename),
        url: rv.url.clone(),
        sha1: rv.sha1.clone(),
        sha512: rv.sha512.clone(),
        size: rv.size,
        path: None,
        page_url: rv.page_url.clone(),
        manual: rv.url.is_none(),
        deps: vec![],
        incompatible: vec![],
        requested_by: vec![],
    }
}
