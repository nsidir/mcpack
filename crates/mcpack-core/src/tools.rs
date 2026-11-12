//! The tool surface: every launcher action as `call(tool, json_args) -> json`.
//!
//! One dispatcher serves every front end — the desktop UI (in-process), the `mcpack call` CLI,
//! and the optional MCP server for AI agents — so they all drive exactly the same operations.
//! Every call is recorded in the activity journal (`<home>/activity.jsonl`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::doctor::DoctorOptions;
use crate::guide;
use crate::error::{Error, Result};
use crate::events::Event;
use crate::launch::LaunchOptions;
use crate::pack::schema::{Channel, ModKind, ModSpec, Source};
use crate::pack::{Pack, DEFAULT_PROFILE};
use crate::registry::{self, ActivityEntry};
use crate::resolve::UpdatePolicy;
use crate::source::SearchFilters;
use crate::testing::{SmokeOptions, WorldOptions};
use crate::{util, Launcher, LoaderKind};

/// Who is calling (recorded in the activity journal).
#[derive(Debug, Clone)]
pub struct CallContext {
    /// `mcp` | `cli` | `app`
    pub origin: String,
    /// MCP client name (from `initialize`), if any.
    pub client: Option<String>,
}

impl CallContext {
    pub fn new(origin: &str) -> Self {
        Self { origin: origin.into(), client: None }
    }
}

/// Read-only tools that UIs poll; not journaled when called by the app.
const QUIET: &[&str] = &[
    "app_info", "config_get", "pack_list", "pack_show", "pack_get", "profile_show", "sessions", "session_status", "logs",
    "activity", "watch_events", "watches", "minecraft_versions", "loader_versions", "agent_status", "help", "overview",
];

// ---------------------------------------------------------------- definitions

fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": props, "required": required},
    })
}

fn p_pack() -> Value {
    json!({"type": "string", "description": "Pack directory (or path to pack.toml)"})
}
fn p_profile() -> Value {
    json!({"type": "string", "description": "Profile name (default: \"default\")"})
}
fn p_session() -> Value {
    json!({"type": "string", "description": "Session id. Optional: defaults to the running session of `pack`/`profile`, or the newest running session"})
}

/// What `tools/list` advertises: every tool except old aliases, with a one-sentence
/// description each (the full text is one `help tool=<name>` call away). Keeps the list an
/// agent loads up front small.
pub fn definitions() -> Vec<Value> {
    all_definitions()
        .into_iter()
        .filter(|d| !guide::ALIASES.iter().any(|(alias, _)| d["name"] == *alias))
        .map(|mut d| {
            let short = guide::summary(d["description"].as_str().unwrap_or_default());
            d["description"] = json!(short);
            // The pack/profile/session conventions are stated once in the server instructions
            // instead of in every schema (they were most of the list's size).
            if let Some(props) = d["inputSchema"]["properties"].as_object_mut() {
                for k in ["pack", "profile", "session"] {
                    if let Some(p) = props.get_mut(k).and_then(|p| p.as_object_mut()) {
                        p.remove("description");
                    }
                }
            }
            if d["inputSchema"]["required"].as_array().is_some_and(|r| r.is_empty()) {
                d["inputSchema"].as_object_mut().map(|s| s.remove("required"));
            }
            d
        })
        .collect()
}

/// Full JSON descriptions of every tool, aliases included (name, description, inputSchema).
pub fn all_definitions() -> Vec<Value> {
    let sess = || json!({"session": p_session(), "pack": p_pack(), "profile": p_profile()});
    let with_sess = |extra: Value| {
        let mut base = sess();
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        base
    };
    let spec_props = json!({
        "pack": p_pack(),
        "id": {"type": "string", "description": "Modrinth slug/id or CurseForge slug/numeric id (alias: slug)"},
        "source": {"type": "string", "enum": ["modrinth", "curseforge", "url", "file"], "description": "default modrinth"},
        "version": {"type": "string", "description": "\"latest\" (default) or exact version number / version id / CurseForge file id"},
        "kind": {"type": "string", "enum": ["mod", "resourcepack", "shaderpack", "datapack"], "description": "inferred when omitted"},
        "channel": {"type": "string", "enum": ["release", "beta", "alpha"]},
        "url": {"type": "string", "description": "for source=url"},
        "path": {"type": "string", "description": "for source=file (relative to the pack or absolute) — great for mods you are developing"},
        "filename": {"type": "string"},
        "sha1": {"type": "string"},
        "optional_deps": {"type": "boolean", "description": "also install optional dependencies"},
        "profile": {"type": "string", "description": "add to this profile only (default: base pack)"},
        "resolve": {"type": "boolean", "description": "resolve immediately (default true); the edit is rolled back if resolution fails"},
        "check": {"type": "boolean", "description": "after resolving, scan jars and auto-add dependencies they declare but the source did not (default true)"},
    });
    vec![
        tool(
            "overview",
            "Start here: packs, running games (with in-game state), hot-reload watches and pending restarts, recent errors, and suggested next calls — in one call.",
            json!({}),
            &[],
        ),
        tool(
            "help",
            "Map of all tools by group. help tool=<name> gives a tool's full description and arguments; help topic=<name> gives a guide (workflows, pack-settings, hot-reload, testing, errors, results).",
            json!({"tool": {"type": "string"}, "topic": {"type": "string", "enum": ["workflows", "pack-settings", "hot-reload", "testing", "errors", "results"]}}),
            &[],
        ),
        tool("app_info", "Launcher version, home directory, agent-supported Minecraft versions.", json!({}), &[]),
        tool("config_get", "Global launcher config (CurseForge key masked).", json!({}), &[]),
        tool(
            "config_set",
            "Set global config: key+value, or `config` object with any of curseforge_api_key, username, java_path, download_concurrency.",
            json!({"key": {"type": "string"}, "value": {"type": "string"}, "config": {"type": "object"}}),
            &[],
        ),
        tool("minecraft_versions", "List Minecraft versions.", json!({"snapshots": {"type": "boolean"}}), &[]),
        tool(
            "loader_versions",
            "List loader versions for a Minecraft version.",
            json!({"loader": {"type": "string", "enum": ["fabric", "quilt"]}, "minecraft": {"type": "string"}}),
            &["loader", "minecraft"],
        ),
        tool("pack_list", "Known packs with status (lock synced/stale, watch, running).", json!({}), &[]),
        tool(
            "pack_init",
            "Create a new pack (pack.toml with a `test` profile). Without `dir`, creates <home>/packs/<name>.",
            json!({
                "dir": {"type": "string"}, "name": {"type": "string"}, "minecraft": {"type": "string", "description": "e.g. 1.21.1"},
                "loader": {"type": "string", "enum": ["fabric", "quilt", "vanilla"], "description": "default fabric"},
                "loader_version": {"type": "string"},
            }),
            &["minecraft"],
        ),
        tool("pack_forget", "Remove a pack from the known-packs list (files are kept).", json!({"pack": p_pack()}), &["pack"]),
        tool(
            "pack_delete",
            "Delete a pack: stops its watches and removes it from the known-packs list. files=true also deletes the pack folder (worlds, configs, everything) — only for packs inside <home>/packs. Refused while the game is running for the pack.",
            json!({"pack": p_pack(), "files": {"type": "boolean", "description": "also delete the pack folder (default false)"}}),
            &["pack"],
        ),
        tool("pack_show", "Pack summary: settings, profiles, resolved content (with dependency flags), lock state, running sessions.", json!({"pack": p_pack()}), &["pack"]),
        tool("pack_get", "Read a pack.toml value by path (e.g. jvm.memory, profiles.test, mods[0]).", json!({"pack": p_pack(), "path": {"type": "string"}}), &["pack"]),
        tool(
            "pack_set",
            "Set a pack.toml value by path, preserving comments. Paths: pack.minecraft, jvm.memory, jvm.args, options.renderDistance, config.\"config/x.json\".key, profiles.<p>.jvm.memory, profiles.<p>.disable, dev.config_reload (restart|resources|none), dev.restart (ask|auto), dev.hotswap (bool) ... Invalid results are rejected.",
            json!({"pack": p_pack(), "path": {"type": "string"}, "value": {"description": "any JSON value"}}),
            &["pack", "path", "value"],
        ),
        tool("pack_unset", "Remove a pack.toml value by path.", json!({"pack": p_pack(), "path": {"type": "string"}}), &["pack", "path"]),
        tool("profile_show", "Effective (merged) settings and mod list of a profile.", json!({"pack": p_pack(), "profile": p_profile()}), &["pack"]),
        tool(
            "pack_add",
            "Add a mod / resource pack / shader pack / datapack. Required dependencies are resolved automatically (transitively); jar-declared dependencies missing from the source are added too.",
            spec_props,
            &["pack"],
        ),
        tool(
            "pack_remove",
            "Remove content by id/slug/filename. Dependencies no longer needed are dropped from the lock.",
            json!({"pack": p_pack(), "id": {"type": "string"}, "profile": p_profile(), "resolve": {"type": "boolean"}}),
            &["pack", "id"],
        ),
        tool(
            "resolve",
            "Resolve pack.toml into pack.lock (pins versions, walks dependencies, reports incompatibilities).",
            json!({"pack": p_pack(), "update": {"description": "true = update everything to latest compatible; or a list of ids to update"}}),
            &["pack"],
        ),
        tool("update", "Update content to the newest compatible versions (unpins explicit versions of the named entries).", json!({"pack": p_pack(), "ids": {"type": "array", "items": {"type": "string"}, "description": "omit for all"}}), &["pack"]),
        tool("check_updates", "Show which entries have newer compatible versions without changing anything.", json!({"pack": p_pack()}), &["pack"]),
        tool(
            "sync",
            "Make the instance (<pack>/run/<profile>) match the pack. dry_run reports what would change and whether a restart is needed.",
            json!({"pack": p_pack(), "profile": p_profile(), "dry_run": {"type": "boolean"}, "agent": {"type": "boolean"}}),
            &["pack"],
        ),
        tool("check", "Pre-launch validation: syncs, then checks every jar's dependencies, Minecraft/Java versions, breaks/duplicates, manual downloads.", json!({"pack": p_pack(), "profile": p_profile()}), &["pack"]),
        tool(
            "doctor",
            "Detect and automatically fix pack problems (missing deps, wrong-version mods, out of memory...), iterating until clean. smoke=true also launches the game to catch runtime crashes.",
            json!({"pack": p_pack(), "profile": p_profile(), "smoke": {"type": "boolean"}, "apply": {"type": "boolean"}, "max_rounds": {"type": "integer"}, "timeout_secs": {"type": "integer"}}),
            &["pack"],
        ),
        tool("install", "Download Minecraft, loader, libraries, assets and Java for a pack (launch does this too).", json!({"pack": p_pack(), "profile": p_profile()}), &["pack"]),
        tool(
            "launch",
            "Launch the client (offline account). Syncs and installs first. wait=true waits for the title screen (or `world`).",
            json!({
                "pack": p_pack(), "profile": p_profile(), "world": {"type": "string", "description": "join this world folder directly"},
                "agent": {"type": "boolean"}, "username": {"type": "string"},
                "extra_jvm_args": {"type": "array", "items": {"type": "string"}}, "extra_game_args": {"type": "array", "items": {"type": "string"}},
                "wait": {"type": "boolean"}, "timeout_secs": {"type": "integer"},
            }),
            &["pack"],
        ),
        tool("stop", "Stop a running game (graceful, then kill).", sess(), &[]),
        tool("restart", "Restart a game with re-synced files, rejoining the current world.", sess(), &[]),
        tool("sessions", "All game sessions (including ones started by other mcpack processes).", json!({}), &[]),
        tool("session_status", "Status of a session, including the live in-game agent status.", sess(), &[]),
        tool(
            "logs",
            "Game log lines. Use `since` (= previous next_seq) to stream, `grep` (regex) to filter.",
            with_sess(json!({"since": {"type": "integer"}, "tail": {"type": "integer", "description": "default 100"}, "grep": {"type": "string"}})),
            &[],
        ),
        tool("wait_ready", "Wait until the title screen (or in-world with world=true).", with_sess(json!({"world": {"type": "boolean"}, "timeout_secs": {"type": "integer"}})), &[]),
        tool(
            "smoke_test",
            "Launch → wait for title screen → optionally create/join a world and run commands → screenshot → stop. Returns pass/fail with a structured diagnosis on failure.",
            json!({
                "pack": p_pack(), "profile": p_profile(), "timeout_secs": {"type": "integer"}, "keep_running": {"type": "boolean"},
                "world": {"type": "object", "properties": {"name": {"type": "string"}, "create": {"type": "boolean"}, "flat": {"type": "boolean"}, "seed": {"type": "integer"}, "game_mode": {"type": "string"}}},
                "commands": {"type": "array", "items": {"type": "string"}}, "screenshot": {"type": "boolean"},
            }),
            &["pack"],
        ),
        tool("diagnose", "Analyze the latest run's log + crash report into structured problems with suggested fixes.", json!({"pack": p_pack(), "profile": p_profile(), "session": p_session()}), &["pack"]),
        tool(
            "search_mods",
            "Search Modrinth (default) or CurseForge. With `pack`, filters by its Minecraft version and loader.",
            json!({
                "query": {"type": "string"}, "pack": p_pack(), "source": {"type": "string", "enum": ["modrinth", "curseforge"]},
                "kind": {"type": "string", "enum": ["mod", "resourcepack", "shaderpack", "datapack"]},
                "minecraft": {"type": "string"}, "loader": {"type": "string"}, "limit": {"type": "integer"},
            }),
            &["query"],
        ),
        tool(
            "mod_versions",
            "List versions of a project (filtered by the pack's Minecraft/loader when `pack` is given).",
            json!({"id": {"type": "string"}, "pack": p_pack(), "source": {"type": "string"}, "minecraft": {"type": "string"}, "loader": {"type": "string"}}),
            &["id"],
        ),
        tool("game_command", "Run a command in the current singleplayer world; returns its output.", with_sess(json!({"command": {"type": "string"}})), &["command"]),
        tool("screenshot", "Capture the game window to PNG.", sess(), &[]),
        tool("reload", "Live-reload resources or datapacks in the running game.", with_sess(json!({"what": {"type": "string", "enum": ["resources", "datapacks"]}})), &[]),
        tool(
            "world_enter",
            "Create (or join if it exists) a singleplayer world and wait until in-game.",
            with_sess(json!({"name": {"type": "string"}, "create": {"type": "boolean"}, "flat": {"type": "boolean"}, "seed": {"type": "integer"}, "game_mode": {"type": "string"}, "timeout_secs": {"type": "integer"}})),
            &[],
        ),
        tool("world_leave", "Save and leave the current world.", sess(), &[]),
        tool(
            "agent_call",
            "Call any in-game agent method: ping, status, command, client_command, chat, reload_resources, reload_datapacks, screenshot, create_world, join_world, leave_world, exit.",
            with_sess(json!({"method": {"type": "string"}, "params": {"type": "object"}})),
            &["method"],
        ),
        tool(
            "watch_start",
            "Hot reload: watch the pack and apply each change to the running game: resource packs/datapacks (and configs with dev.config_reload=\"resources\") reload live; changed mod code is hot-swapped when dev.hotswap = true (method-body changes; takes effect from the next launch); anything else needs a restart, which happens automatically with dev.restart = \"auto\" or is reported as action `restart_needed` with `restart_reasons` (default \"ask\": call `restart` to apply). See watch_events.",
            json!({"pack": p_pack(), "profile": p_profile(), "session": p_session()}),
            &["pack"],
        ),
        tool("watch_stop", "Stop a watch (by id, or by pack/profile).", json!({"watch": {"type": "string"}, "pack": p_pack(), "profile": p_profile()}), &[]),
        tool("watches", "Active watches.", json!({}), &[]),
        tool("watch_events", "Hot-reload events of a watch (since = last seq seen).", json!({"watch": {"type": "string"}, "since": {"type": "integer"}}), &["watch"]),
        tool("hot_apply", "Apply the current pack state now (what the watcher does on change).", json!({"pack": p_pack(), "profile": p_profile(), "session": p_session()}), &["pack"]),
        tool(
            "export_pack",
            "Export a profile as a modpack archive: Modrinth .mrpack (default) or CurseForge modpack .zip (manifest.json).",
            json!({"pack": p_pack(), "profile": p_profile(), "format": {"type": "string", "enum": ["mrpack", "curseforge"]}, "out": {"type": "string"}}),
            &["pack"],
        ),
        tool(
            "import_pack",
            "Import a modpack archive (.mrpack or CurseForge .zip, auto-detected) into a new pack directory (default <home>/packs/<name>).",
            json!({"file": {"type": "string"}, "dir": {"type": "string"}, "resolve": {"type": "boolean"}}),
            &["file"],
        ),
        tool("export_mrpack", "Export a profile as a Modrinth .mrpack.", json!({"pack": p_pack(), "profile": p_profile(), "out": {"type": "string"}}), &["pack"]),
        tool("import_mrpack", "Import a .mrpack into a new pack directory.", json!({"file": {"type": "string"}, "dir": {"type": "string"}, "resolve": {"type": "boolean"}}), &["file", "dir"]),
        tool("activity", "Activity journal of tool calls (newest first) from all front ends (app, cli, mcp).", json!({"limit": {"type": "integer"}, "since": {"type": "integer"}, "tool": {"type": "string"}}), &[]),
    ]
}

// ---------------------------------------------------------------- arg helpers

fn s(a: &Value, k: &str) -> Option<String> {
    match a.get(k) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}
fn req(a: &Value, k: &str) -> Result<String> {
    s(a, k).ok_or_else(|| Error::Invalid(format!("missing required argument `{k}`")))
}
fn b(a: &Value, k: &str) -> Option<bool> {
    a.get(k).and_then(|v| v.as_bool())
}
fn u(a: &Value, k: &str) -> Option<u64> {
    a.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}
fn pack_arg(a: &Value) -> Result<PathBuf> {
    s(a, "pack").or_else(|| s(a, "packId")).or_else(|| s(a, "dir")).map(PathBuf::from).ok_or_else(|| Error::Invalid("missing required argument `pack`".into()))
}
fn profile_arg(a: &Value) -> String {
    s(a, "profile").unwrap_or_else(|| DEFAULT_PROFILE.to_string())
}
fn strings(a: &Value, k: &str) -> Vec<String> {
    a.get(k)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}
fn from<T: serde::de::DeserializeOwned>(a: &Value) -> Result<T> {
    serde_json::from_value(a.clone()).map_err(|e| Error::Invalid(format!("bad arguments: {e}")))
}
fn to<T: serde::Serialize>(v: T) -> Result<Value> {
    Ok(serde_json::to_value(v)?)
}

fn session_arg(l: &Launcher, a: &Value) -> Result<String> {
    let pack = s(a, "pack").or_else(|| s(a, "packId")).map(PathBuf::from);
    l.pick_session(s(a, "session").as_deref(), pack.as_deref(), s(a, "profile").as_deref())
}

fn spec_from_args(a: &Value) -> Result<ModSpec> {
    let source = match s(a, "source") {
        Some(x) if x == "local" => {
            if s(a, "url").is_some() {
                Source::Url
            } else {
                Source::File
            }
        }
        Some(x) => Source::parse(&x).ok_or_else(|| Error::Invalid(format!("unknown source '{x}'")))?,
        None if s(a, "url").is_some() => Source::Url,
        None if s(a, "path").is_some() && s(a, "id").is_none() => Source::File,
        None => Source::Modrinth,
    };
    let kind = match s(a, "kind") {
        Some(k) => Some(ModKind::parse(&k).ok_or_else(|| Error::Invalid(format!("unknown kind '{k}'")))?),
        None => None,
    };
    let channel = match s(a, "channel").as_deref() {
        Some("release") => Some(Channel::Release),
        Some("beta") => Some(Channel::Beta),
        Some("alpha") => Some(Channel::Alpha),
        Some(other) => return Err(Error::Invalid(format!("unknown channel '{other}'"))),
        None => None,
    };
    let version = s(a, "version").filter(|v| v != "latest");
    let spec = ModSpec {
        source,
        id: s(a, "id").or_else(|| s(a, "slug")),
        version,
        kind,
        channel,
        url: s(a, "url"),
        path: s(a, "path"),
        sha1: s(a, "sha1"),
        filename: s(a, "filename"),
        optional_deps: b(a, "optional_deps"),
    };
    Ok(spec)
}

// ---------------------------------------------------------------- dispatcher

/// Call a tool by name. Records the call in the activity journal and emits activity events.
pub async fn call(l: &Launcher, tool: &str, args: Value, cx: &CallContext) -> Result<Value> {
    let args = if args.is_null() { json!({}) } else { args };
    let pack = s(&args, "pack").or_else(|| s(&args, "packId")).map(|p| {
        let d = crate::pack::pack_dir(Path::new(&p));
        std::path::absolute(&d).unwrap_or(d).display().to_string()
    });
    let journal = !(cx.origin == "app" && QUIET.contains(&tool));
    let started = Instant::now();
    let mut entry = ActivityEntry {
        id: format!("a-{}", &util::sha1_bytes(format!("{tool}{}{}", util::now_millis(), rand_tag()).as_bytes())[..10]),
        ts: util::now_millis(),
        origin: cx.origin.clone(),
        client: cx.client.clone(),
        tool: tool.to_string(),
        pack: pack.clone(),
        args: redact(&args),
        status: "running".into(),
        error_code: None,
        error: None,
        duration_ms: None,
        result: None,
    };
    if journal {
        let _ = registry::append_activity(&l.ctx.paths, &entry);
        l.ctx.events.emit(Event::Activity { entry: entry.clone() });
    }
    let res = dispatch(l, tool, &args).await;
    // Any successful call on a pack registers it — except the calls whose point is to unregister it.
    if let (Ok(_), Some(p), false) = (&res, &pack, matches!(tool, "pack_forget" | "pack_delete")) {
        if Path::new(p).join(crate::pack::PACK_FILE).exists() {
            registry::remember_pack(&l.ctx.paths, Path::new(p));
        }
    }
    if journal {
        entry.duration_ms = Some(started.elapsed().as_millis() as u64);
        match &res {
            Ok(v) => {
                entry.status = "ok".into();
                entry.result = Some(truncate_result(v));
            }
            Err(e) => {
                entry.status = "error".into();
                entry.error_code = Some(e.code().into());
                entry.error = Some(e.to_string());
            }
        }
        let _ = registry::append_activity(&l.ctx.paths, &entry);
        l.ctx.events.emit(Event::Activity { entry });
    }
    // Agents get compact results unless they ask for everything (see `help topic=results`).
    if cx.origin == "mcp" && !b(&args, "detail").unwrap_or(false) {
        return res.map(guide::compact);
    }
    res
}

/// Hint for an error, for front ends that show one (the MCP server adds it to error results).
pub fn error_hint(tool: &str, e: &Error) -> Option<String> {
    guide::hint(tool, e)
}

fn rand_tag() -> u64 {
    use rand::Rng;
    rand::thread_rng().gen()
}

fn redact(args: &Value) -> Value {
    let mut a = args.clone();
    if let Some(o) = a.as_object_mut() {
        if o.get("key").and_then(|k| k.as_str()) == Some("curseforge_api_key") {
            o.insert("value".into(), json!("<redacted>"));
        }
        if let Some(c) = o.get_mut("config").and_then(|c| c.as_object_mut()) {
            if c.contains_key("curseforge_api_key") {
                c.insert("curseforge_api_key".into(), json!("<redacted>"));
            }
        }
    }
    a
}

fn truncate_result(v: &Value) -> Value {
    let text = v.to_string();
    if text.len() <= 16 * 1024 {
        v.clone()
    } else {
        json!({"truncated": true, "bytes": text.len(), "preview": text.chars().take(2000).collect::<String>()})
    }
}

async fn dispatch(l: &Launcher, tool: &str, a: &Value) -> Result<Value> {
    let secs = |k: &str, d: u64| Duration::from_secs(u(a, k).unwrap_or(d));
    match tool {
        "help" => guide::help(a, &all_definitions()),
        "overview" => overview(l).await,
        "app_info" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "home": l.home().display().to_string(),
            "mode": "native",
            "agent_minecraft_versions": l.agent_versions(),
        })),
        "config_get" => to(l.config()),
        "config_set" => {
            if let Some(cfg) = a.get("config").and_then(|c| c.as_object()) {
                for (k, v) in cfg {
                    let val = match v {
                        Value::Null => String::new(),
                        Value::String(s) if s == "<set>" => continue,
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    l.config_set(k, &val)?;
                }
            } else {
                l.config_set(&req(a, "key")?, &s(a, "value").unwrap_or_default())?;
            }
            to(l.config())
        }
        "minecraft_versions" => to(l.minecraft_versions(b(a, "snapshots").unwrap_or(false)).await?),
        "loader_versions" => to(l.loader_versions(LoaderKind::parse(&req(a, "loader")?)?, &req(a, "minecraft")?).await?),
        "pack_list" => pack_list(l),
        "pack_init" => {
            let name = s(a, "name").unwrap_or_else(|| "my-pack".into());
            let dir = match s(a, "dir").or_else(|| s(a, "pack")) {
                Some(d) => PathBuf::from(d),
                None => l.home().join("packs").join(util::safe_component(&name)),
            };
            let loader = LoaderKind::parse(&s(a, "loader").unwrap_or_else(|| "fabric".into()))?;
            let info = l.pack_init(&dir, &name, &req(a, "minecraft")?, loader, s(a, "loader_version").as_deref())?;
            registry::remember_pack(&l.ctx.paths, Path::new(&info.dir));
            to(info)
        }
        "pack_forget" => {
            let d = pack_arg(a)?;
            let d = std::path::absolute(&d).unwrap_or(d);
            Ok(json!({"removed": registry::forget_pack(&l.ctx.paths, &d.display().to_string())}))
        }
        "pack_delete" => {
            let d = pack_arg(a)?;
            let d = std::path::absolute(&d).unwrap_or(d);
            let key = d.display().to_string();
            if l.sessions().iter().any(|s| s.running && s.pack_dir == key) {
                return Err(Error::Invalid("the game is running for this pack; stop it first".into()));
            }
            let files = b(a, "files").unwrap_or(false);
            if files {
                // Only folders the launcher created itself are deleted; a pack that lives in
                // someone's project directory is just forgotten.
                let root = l.home().join("packs");
                let root = std::path::absolute(&root).unwrap_or(root);
                if !(d.starts_with(&root) && d != root) {
                    return Err(Error::Invalid(format!(
                        "{} is outside {}; it can be removed from the list (files=false) but its folder is not deleted",
                        d.display(),
                        root.display()
                    )));
                }
                if !d.join(crate::pack::PACK_FILE).exists() {
                    return Err(Error::NotFound(format!("no pack in {}", d.display())));
                }
            }
            for w in l.watches().into_iter().filter(|w| w.pack_dir == key) {
                let _ = l.watch_stop(&w.id);
            }
            if files {
                std::fs::remove_dir_all(&d).map_err(|e| Error::io(&d, e))?;
            }
            let removed = registry::forget_pack(&l.ctx.paths, &key);
            Ok(json!({"removed": removed, "files_deleted": files, "dir": key}))
        }
        "pack_show" => to(l.pack_show(&pack_arg(a)?)?),
        "pack_get" => l.pack_get(&pack_arg(a)?, &s(a, "path").unwrap_or_default()),
        "pack_set" => {
            let v = a.get("value").cloned().ok_or_else(|| Error::Invalid("missing required argument `value`".into()))?;
            to(l.pack_set(&pack_arg(a)?, &req(a, "path")?, &v).await?)
        }
        "pack_unset" => to(l.pack_unset(&pack_arg(a)?, &req(a, "path")?).await?),
        "profile_show" => to(l.profile_show(&pack_arg(a)?, &profile_arg(a))?),
        "pack_add" => {
            let dir = pack_arg(a)?;
            let spec = spec_from_args(a)?;
            let resolve = b(a, "resolve").unwrap_or(true);
            let profile = s(a, "profile");
            let mut out = to(l.pack_add(&dir, profile.as_deref(), spec, resolve).await?)?;
            if resolve && b(a, "check").unwrap_or(true) {
                // Catch dependencies declared in jars but missing from the source's metadata.
                let doc = l
                    .doctor(&dir, DoctorOptions { profile: profile.clone(), smoke: false, apply: Some(true), max_rounds: Some(3), timeout_secs: None })
                    .await?;
                if let Some(o) = out.as_object_mut() {
                    o.insert("doctor".into(), to(&doc)?);
                    o.insert("pack".into(), to(l.pack_show(&dir)?)?);
                }
            }
            Ok(out)
        }
        "pack_remove" => {
            let id = s(a, "id").or_else(|| s(a, "slug")).ok_or_else(|| Error::Invalid("missing required argument `id`".into()))?;
            to(l.pack_remove(&pack_arg(a)?, s(a, "profile").as_deref(), &id, b(a, "resolve").unwrap_or(true)).await?)
        }
        "resolve" => {
            let policy = match a.get("update") {
                Some(Value::Bool(true)) => UpdatePolicy { all: true, only: vec![] },
                Some(Value::Array(_)) => UpdatePolicy { all: false, only: strings(a, "update") },
                Some(Value::String(s)) => UpdatePolicy { all: false, only: vec![s.clone()] },
                _ => UpdatePolicy::default(),
            };
            to(l.resolve(&pack_arg(a)?, policy).await?)
        }
        "update" => {
            let dir = pack_arg(a)?;
            let mut ids = strings(a, "ids");
            if let Some(one) = s(a, "id").or_else(|| s(a, "slug")) {
                ids.push(one);
            }
            // Unpin explicit versions of the named entries.
            let pack = Pack::load(&dir)?;
            let mut ed = crate::pack::edit::PackEditor::open(&dir)?;
            let mut changed = false;
            for spec in &pack.file.mods {
                if spec.version.is_some() && (ids.is_empty() || ids.iter().any(|i| spec.matches(i))) && matches!(spec.source, Source::Modrinth | Source::Curseforge) {
                    let mut s2 = spec.clone();
                    s2.version = None;
                    ed.add_mod(None, &s2)?;
                    changed = true;
                }
            }
            if changed {
                ed.save()?;
            }
            let policy = if ids.is_empty() { UpdatePolicy { all: true, only: vec![] } } else { UpdatePolicy { all: false, only: ids } };
            to(l.resolve(&dir, policy).await?)
        }
        "check_updates" => to(l.check_updates(&pack_arg(a)?).await?),
        "sync" => to(l.sync(&pack_arg(a)?, &profile_arg(a), b(a, "agent"), b(a, "dry_run").unwrap_or(false)).await?),
        "check" => to(l.check(&pack_arg(a)?, &profile_arg(a)).await?),
        "doctor" => {
            let opts: DoctorOptions = from(a)?;
            to(l.doctor(&pack_arg(a)?, opts).await?)
        }
        "install" => to(l.install(&pack_arg(a)?, &profile_arg(a)).await?),
        "launch" => {
            let dir = pack_arg(a)?;
            let opts: LaunchOptions = from(a)?;
            let r = l.launch(&dir, opts).await?;
            let mut v = to(&r)?;
            if b(a, "wait").unwrap_or(false) {
                let ready = l.wait_ready(&r.session.id, secs("timeout_secs", 300), s(a, "world").is_some()).await?;
                if let Some(o) = v.as_object_mut() {
                    o.insert("ready".into(), to(ready)?);
                    o.insert("session".into(), to(l.session_status(&r.session.id)?)?);
                }
            }
            Ok(v)
        }
        "stop" => to(l.stop(&session_arg(l, a)?, secs("grace_secs", 20)).await?),
        "restart" => to(l.restart(&session_arg(l, a)?).await?),
        "sessions" => to(l.sessions()),
        "session_status" | "agent_status" => {
            let id = session_arg(l, a)?;
            let st = l.session_status(&id)?;
            let agent = if st.running && st.agent_port.is_some() {
                l.agent_call(&id, "status", json!({}), Duration::from_secs(2)).await.ok()
            } else {
                None
            };
            let mut v = to(st)?;
            if let Some(o) = v.as_object_mut() {
                o.insert("agent".into(), agent.unwrap_or(Value::Null));
            }
            Ok(v)
        }
        "logs" => {
            let id = session_arg(l, a)?;
            to(l.logs(&id, u(a, "since"), Some(u(a, "tail").unwrap_or(100) as usize), s(a, "grep").as_deref())?)
        }
        "wait_ready" => to(l.wait_ready(&session_arg(l, a)?, secs("timeout_secs", 300), b(a, "world").unwrap_or(false)).await?),
        "smoke_test" => {
            let opts: SmokeOptions = from(a)?;
            to(l.smoke_test(&pack_arg(a)?, opts).await?)
        }
        "diagnose" => to(l.diagnose(&pack_arg(a)?, &profile_arg(a), s(a, "session").as_deref())?),
        "search_mods" => {
            let mut f = SearchFilters { minecraft: s(a, "minecraft"), loader: None, kind: None, limit: u(a, "limit").unwrap_or(10) as u32 };
            if let Some(k) = s(a, "kind") {
                f.kind = ModKind::parse(&k);
            }
            if let Some(lo) = s(a, "loader") {
                f.loader = Some(LoaderKind::parse(&lo)?);
            }
            if let Some(p) = s(a, "pack").or_else(|| s(a, "packId")) {
                let pack = Pack::load(&p)?;
                f.minecraft.get_or_insert(pack.file.pack.minecraft.clone());
                if f.loader.is_none() && pack.file.pack.loader.kind != LoaderKind::Vanilla {
                    f.loader = Some(pack.file.pack.loader.kind);
                }
            }
            let source = Source::parse(&s(a, "source").unwrap_or_else(|| "modrinth".into())).unwrap_or(Source::Modrinth);
            let hits = l.search(source, &req(a, "query")?, f).await?;
            Ok(Value::Array(
                hits.into_iter()
                    .map(|h| {
                        json!({
                            "slug": h.slug.clone().unwrap_or_else(|| h.id.clone()),
                            "id": h.id, "name": h.title.clone(), "title": h.title,
                            "kind": h.kind.unwrap_or_default(), "source": h.source,
                            "description": h.description, "downloads": h.downloads,
                            "author": h.author, "url": h.url, "latest": "",
                        })
                    })
                    .collect(),
            ))
        }
        "mod_versions" => {
            let (mut mc, mut loader) = (s(a, "minecraft"), s(a, "loader").map(|x| LoaderKind::parse(&x)).transpose()?);
            if let Some(p) = s(a, "pack") {
                let pack = Pack::load(&p)?;
                mc.get_or_insert(pack.file.pack.minecraft.clone());
                if loader.is_none() && pack.file.pack.loader.kind != LoaderKind::Vanilla {
                    loader = Some(pack.file.pack.loader.kind);
                }
            }
            let source = Source::parse(&s(a, "source").unwrap_or_else(|| "modrinth".into())).unwrap_or(Source::Modrinth);
            to(l.mod_versions(source, &req(a, "id")?, mc.as_deref(), loader).await?)
        }
        "game_command" => l.game_command(&session_arg(l, a)?, &req(a, "command")?).await,
        "screenshot" => Ok(json!({"path": l.screenshot(&session_arg(l, a)?).await?})),
        "reload" => {
            let id = session_arg(l, a)?;
            let method = match s(a, "what").as_deref() {
                Some("datapacks") => "reload_datapacks",
                _ => "reload_resources",
            };
            l.agent_call(&id, method, json!({}), Duration::from_secs(180)).await
        }
        "world_enter" => {
            let id = session_arg(l, a)?;
            let opts: WorldOptions = from(a)?;
            l.world_enter(&id, opts, secs("timeout_secs", 180)).await
        }
        "world_leave" => l.world_leave(&session_arg(l, a)?, secs("timeout_secs", 60)).await,
        "agent_call" => {
            let id = session_arg(l, a)?;
            let params = a.get("params").cloned().unwrap_or_else(|| json!({}));
            l.agent_call(&id, &req(a, "method")?, params, secs("timeout_secs", 60)).await
        }
        "watch_start" => to(l.watch_start(&pack_arg(a)?, &profile_arg(a), s(a, "session")).await?),
        "watch_stop" => {
            let id = match s(a, "watch") {
                Some(w) => w,
                None => {
                    let dir = pack_arg(a)?;
                    let dir = std::path::absolute(&dir).unwrap_or(dir).display().to_string();
                    let prof = profile_arg(a);
                    l.watches()
                        .into_iter()
                        .find(|w| w.pack_dir == dir && w.profile == prof)
                        .map(|w| w.id)
                        .ok_or_else(|| Error::NotFound("no watch for this pack/profile".into()))?
                }
            };
            to(l.watch_stop(&id)?)
        }
        "watches" => to(l.watches()),
        "watch_events" => to(l.watch_events(&req(a, "watch")?, u(a, "since"))?),
        "hot_apply" => to(l.hot_apply(&pack_arg(a)?, &profile_arg(a), s(a, "session").as_deref()).await),
        "export_pack" => {
            let format = crate::cfpack::PackFormat::parse(&s(a, "format").unwrap_or_else(|| "mrpack".into()))
                .ok_or_else(|| Error::Invalid("format must be mrpack or curseforge".into()))?;
            to(l.export_pack(&pack_arg(a)?, &profile_arg(a), format, s(a, "out").as_deref().map(Path::new)).await?)
        }
        "import_pack" => {
            let file = PathBuf::from(req(a, "file")?);
            let dir = match s(a, "dir") {
                Some(d) => PathBuf::from(d),
                None => {
                    let stem = file.file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| "imported".into());
                    l.home().join("packs").join(util::safe_component(&stem))
                }
            };
            let r = l.import_pack(&file, &dir, b(a, "resolve").unwrap_or(true)).await?;
            registry::remember_pack(&l.ctx.paths, Path::new(&r.pack_dir));
            to(r)
        }
        "export_mrpack" => to(l.export_mrpack(&pack_arg(a)?, &profile_arg(a), s(a, "out").as_deref().map(Path::new)).await?),
        "import_mrpack" => {
            let r = l.import_mrpack(Path::new(&req(a, "file")?), Path::new(&req(a, "dir")?), b(a, "resolve").unwrap_or(true)).await?;
            registry::remember_pack(&l.ctx.paths, Path::new(&r.pack_dir));
            to(r)
        }
        "activity" => to(registry::read_activity(&l.ctx.paths, u(a, "limit").unwrap_or(200) as usize, u(a, "since"), s(a, "tool").as_deref())),
        other => Err(Error::NotFound(format!("unknown tool '{other}' (see tools/list)"))),
    }
}

/// Everything an agent needs to get its bearings, with suggested next calls.
async fn overview(l: &Launcher) -> Result<Value> {
    let sessions: Vec<_> = l.sessions().into_iter().filter(|s| s.running).collect();
    let watches = l.watches();
    let mut next: Vec<String> = Vec::new();

    let mut packs = Vec::new();
    for dir in registry::known_packs(&l.ctx.paths) {
        let Ok(info) = l.pack_show(Path::new(&dir)) else { continue };
        let running: Vec<&str> = sessions.iter().filter(|s| s.pack_dir == info.dir).map(|s| s.id.as_str()).collect();
        if info.lock_stale {
            next.push(format!("resolve pack={} (pack.toml changed since the last resolve)", info.dir));
        }
        packs.push(json!({
            "name": info.name,
            "dir": info.dir,
            "minecraft": info.minecraft,
            "loader": info.loader,
            "profiles": info.profiles,
            "content": info.content.iter().filter(|c| c.top_level).count(),
            "lock": if info.lock_stale { "stale" } else { "synced" },
            "running": running,
            "watch": watches.iter().any(|w| w.pack_dir == info.dir),
        }));
    }

    let mut games = Vec::new();
    for s in &sessions {
        let game = match s.agent_port {
            Some(_) => l.agent_call(&s.id, "status", json!({}), Duration::from_millis(1500)).await.ok().map(|st| {
                json!({
                    "title_screen": st["title_screen"], "in_world": st["in_world"], "world": st.get("level_id"),
                    "loading": st["loading"], "screen": st.get("screen_title"), "fps": st["fps"],
                })
            }),
            None => None,
        };
        games.push(json!({
            "session": s.id, "pack": s.pack_dir, "profile": s.profile, "minecraft": s.minecraft,
            "uptime_s": util::now_millis().saturating_sub(s.started_ms) / 1000,
            "agent": game.unwrap_or_else(|| json!(if s.agent_port.is_some() { "starting" } else { "off" })),
            "hotswap": s.hotswap_port.is_some(),
        }));
    }

    let mut watch_list = Vec::new();
    for w in &watches {
        let last = l.watch_events(&w.id, None).ok().and_then(|e| e.into_iter().last());
        if let Some(e) = last.as_ref().filter(|e| e.action == "restart_needed") {
            if let Some(s) = sessions.iter().find(|s| s.pack_dir == w.pack_dir && s.profile == w.profile && s.started_ms <= e.at_ms) {
                next.push(format!("restart session={} to apply: {}", s.id, e.restart_reasons.first().cloned().unwrap_or_default()));
            }
        }
        watch_list.push(json!({
            "watch": w.id, "pack": w.pack_dir, "profile": w.profile,
            "last": last.map(|e| json!({"seq": e.seq, "action": e.action, "paths": e.paths, "restart_reasons": e.restart_reasons, "error": e.error})),
        }));
    }

    let hour_ago = util::now_millis().saturating_sub(3_600_000);
    let errors: Vec<Value> = registry::read_activity(&l.ctx.paths, 200, Some(hour_ago), None)
        .into_iter()
        .filter(|e| e.status == "error")
        .take(5)
        .map(|e| json!({"tool": e.tool, "pack": e.pack, "code": e.error_code, "error": e.error, "at_ms": e.ts}))
        .collect();

    if packs.is_empty() {
        next.push("pack_init name=<name> minecraft=1.21.1 (or import_pack file=<.mrpack|.zip>) to create a pack".into());
    } else if sessions.is_empty() {
        next.push("launch pack=<dir> to play, or smoke_test pack=<dir> profile=test to check it starts".into());
    }
    if !errors.is_empty() {
        next.push("diagnose pack=<dir> explains a failed launch; help topic=errors for the others".into());
    }
    next.push("help for the tool map; help topic=workflows for common sequences".into());

    Ok(json!({"packs": packs, "games": games, "watches": watch_list, "recent_errors": errors, "next": next}))
}

/// Known packs in the shape the desktop UI uses.
fn pack_list(l: &Launcher) -> Result<Value> {
    let watches = l.watches();
    let sessions = l.sessions();
    let mut out = Vec::new();
    for dir in registry::known_packs(&l.ctx.paths) {
        let Ok(info) = l.pack_show(Path::new(&dir)) else { continue };
        let running = sessions.iter().filter(|s| s.running && s.pack_dir == info.dir).map(|s| s.id.clone()).collect::<Vec<_>>();
        out.push(json!({
            "id": info.dir,
            "path": info.dir,
            "name": info.name,
            "minecraft": info.minecraft,
            "loader": info.loader,
            "loaderVersion": info.loader_version.clone().unwrap_or(info.loader_version_spec.clone()),
            "profiles": info.profiles,
            "lock": if info.lock_stale { "stale" } else { "synced" },
            "pendingChanges": 0,
            "watch": watches.iter().any(|w| w.pack_dir == info.dir),
            "running": running,
            "hotswap": info.pack.pointer("/dev/hotswap").and_then(|v| v.as_bool()).unwrap_or(false),
            "restartPolicy": info.pack.pointer("/dev/restart").and_then(|v| v.as_str()).unwrap_or("ask"),
            "configReload": info.pack.pointer("/dev/config_reload").and_then(|v| v.as_str()).unwrap_or("restart"),
        }));
    }
    Ok(Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pack_delete_removes_managed_folders_and_only_forgets_others() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let l = Launcher::with_home(home.path()).unwrap();
        let cx = CallContext::new("test");

        let managed = call(&l, "pack_init", json!({"name": "doomed", "minecraft": "1.21.1"}), &cx).await.unwrap();
        let managed = PathBuf::from(managed["dir"].as_str().unwrap());
        let other = call(&l, "pack_init", json!({"dir": outside.path().display().to_string(), "name": "mine", "minecraft": "1.21.1"}), &cx).await.unwrap();
        let other = PathBuf::from(other["dir"].as_str().unwrap());
        assert_eq!(pack_list(&l).unwrap().as_array().unwrap().len(), 2);

        // A pack outside <home>/packs never has its folder deleted.
        let err = call(&l, "pack_delete", json!({"pack": other.display().to_string(), "files": true}), &cx).await.unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(other.join(crate::pack::PACK_FILE).exists());
        assert_eq!(pack_list(&l).unwrap().as_array().unwrap().len(), 2);

        let r = call(&l, "pack_delete", json!({"pack": other.display().to_string()}), &cx).await.unwrap();
        assert_eq!(r["removed"], true);
        assert!(other.join(crate::pack::PACK_FILE).exists(), "files=false keeps the folder");

        let r = call(&l, "pack_delete", json!({"pack": managed.display().to_string(), "files": true}), &cx).await.unwrap();
        assert_eq!(r["files_deleted"], true);
        assert!(!managed.exists());
        assert!(pack_list(&l).unwrap().as_array().unwrap().is_empty());
    }

    /// Every advertised tool is in exactly one help group, every group entry is a real tool, and
    /// the advertised list is the short form.
    #[test]
    fn help_groups_cover_every_tool() {
        let names: Vec<String> = definitions().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
        for n in &names {
            let n_groups = guide::GROUPS.iter().filter(|(_, _, tools)| tools.contains(&n.as_str())).count();
            assert_eq!(n_groups, 1, "tool {n} must be in exactly one help group");
        }
        for (_, _, tools) in guide::GROUPS {
            for t in *tools {
                assert!(names.iter().any(|n| n == t), "help group lists unknown tool {t}");
            }
        }
        for (alias, target) in guide::ALIASES {
            assert!(!names.iter().any(|n| n == alias), "alias {alias} must not be advertised");
            assert!(names.iter().any(|n| n == target));
        }
        let advertised: usize = definitions().iter().map(|d| d["description"].as_str().unwrap().len()).sum();
        let full: usize = all_definitions().iter().map(|d| d["description"].as_str().unwrap().len()).sum();
        assert!(advertised < full, "tools/list should carry the short descriptions");
    }

    #[tokio::test]
    async fn help_overview_and_compact_results_for_agents() {
        let home = tempfile::tempdir().unwrap();
        let l = Launcher::with_home(home.path()).unwrap();
        let mcp = CallContext::new("mcp");

        let map = call(&l, "help", json!({}), &mcp).await.unwrap();
        assert!(map["groups"].as_array().unwrap().len() >= 5);
        let one = call(&l, "help", json!({"tool": "pack_set"}), &mcp).await.unwrap();
        assert!(one["description"].as_str().unwrap().contains("dev.hotswap"), "help tool= gives the full text");
        assert!(one["arguments"]["path"].is_object());
        let alias = call(&l, "help", json!({"tool": "export_mrpack"}), &mcp).await.unwrap();
        assert_eq!(alias["tool"], "export_pack");
        let topic = call(&l, "help", json!({"topic": "hot-reload"}), &mcp).await.unwrap();
        assert!(topic["text"].as_str().unwrap().contains("restart_needed"));
        let err = call(&l, "help", json!({"topic": "nope"}), &mcp).await.unwrap_err();
        assert_eq!(err.code(), "not_found");

        let empty = call(&l, "overview", json!({}), &mcp).await.unwrap();
        assert!(empty["next"][0].as_str().unwrap().starts_with("pack_init"), "{empty}");
        call(&l, "pack_init", json!({"name": "o", "minecraft": "1.21.1", "loader": "vanilla"}), &mcp).await.unwrap();
        let one_pack = call(&l, "overview", json!({}), &mcp).await.unwrap();
        assert_eq!(one_pack["packs"][0]["name"], "o");
        assert!(one_pack["next"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().starts_with("launch")));

        // Hints for common mistakes.
        let e = call(&l, "stop", json!({"session": "s-nope"}), &mcp).await.unwrap_err();
        assert!(error_hint("stop", &e).unwrap().contains("overview"), "{e}");
        let e = Error::Invalid("x".into());
        assert_eq!(error_hint("pack_set", &e).unwrap(), "help tool=pack_set shows the arguments");
    }

    /// What the watcher relies on: why a restart is needed, holding mod jars back while a game
    /// runs, and local mods being copies (rebuilding the source must not touch the instance).
    #[tokio::test]
    async fn sync_reports_restart_reasons_and_holds_mods() {
        use std::io::Write;
        let home = tempfile::tempdir().unwrap();
        let l = Launcher::with_home(home.path()).unwrap();
        let cx = CallContext::new("test");
        let jar_at = |path: &Path, body: &[u8]| {
            let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
            z.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(br#"{"schemaVersion":1,"id":"devmod","version":"1.0.0"}"#).unwrap();
            z.start_file("dev/Mod.class", zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(body).unwrap();
            z.finish().unwrap();
        };
        let dir = PathBuf::from(
            call(&l, "pack_init", json!({"name": "dev", "minecraft": "1.21.1", "loader": "vanilla"}), &cx).await.unwrap()["dir"]
                .as_str()
                .unwrap(),
        );
        let src = home.path().join("devmod.jar");
        jar_at(&src, b"v1");
        call(&l, "pack_add", json!({"pack": dir.display().to_string(), "source": "file", "path": src.display().to_string(), "kind": "mod", "check": false}), &cx)
            .await
            .unwrap();

        let pack = Pack::load(&dir).unwrap();
        let (lock, _) = l.ensure_lock(&pack).await.unwrap();
        let first = crate::instance::sync(&l.ctx, &pack, &lock, "default", Some(false), false).await.unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        let inst_jar = crate::paths::instance_dir(&dir, "default").join("mods").join("devmod.jar");
        let original = std::fs::read(&inst_jar).unwrap();

        // Rebuild the mod in place: the instance keeps its own copy.
        jar_at(&src, b"v2");
        assert_eq!(std::fs::read(&inst_jar).unwrap(), original, "local mods must be copied, not hard-linked");

        let (lock, _) = l.ensure_lock(&pack).await.unwrap();
        let plan = crate::instance::sync(&l.ctx, &pack, &lock, "default", Some(false), true).await.unwrap();
        assert!(plan.restart_required);
        assert_eq!(plan.mod_changes.len(), 1);
        assert_eq!(plan.mod_changes[0].status, "updated");
        assert!(plan.restart_reasons.iter().any(|r| r.starts_with("mods changed")), "{:?}", plan.restart_reasons);

        // Held: nothing else needs a restart, the jar is untouched and still pending afterwards.
        let held = crate::instance::sync_opts(&l.ctx, &pack, &lock, "default", Some(false), false, true).await.unwrap();
        assert!(!held.restart_required, "{:?}", held.restart_reasons);
        assert_eq!(held.held.len(), 1);
        assert_eq!(std::fs::read(&inst_jar).unwrap(), original);
        let again = crate::instance::sync(&l.ctx, &pack, &lock, "default", Some(false), true).await.unwrap();
        assert_eq!(again.mod_changes.len(), 1, "a held change stays pending");

        // A normal sync (e.g. at launch) applies it.
        crate::instance::sync(&l.ctx, &pack, &lock, "default", Some(false), false).await.unwrap();
        assert_ne!(std::fs::read(&inst_jar).unwrap(), original);
    }
}
