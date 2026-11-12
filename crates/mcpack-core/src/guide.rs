//! Agent ergonomics: a cheap map of the tool surface (`help`), on-demand docs, hints on errors,
//! and compact results — so an agent can find what it needs without loading every detail.
//!
//! `tools/list` carries one-sentence descriptions; the full text, arguments and examples come
//! from `help tool=<name>`, and workflows/settings/error advice from `help topic=<name>`.

use serde_json::{json, Value};

use crate::error::Error;

/// Tool groups, in the order an agent typically needs them. Every advertised tool is in exactly
/// one group (enforced by a test).
pub const GROUPS: &[(&str, &str, &[&str])] = &[
    ("start", "Orient yourself", &["overview", "help", "app_info", "activity"]),
    ("packs", "Packs", &["pack_list", "pack_init", "pack_show", "pack_delete", "pack_forget", "import_pack", "export_pack"]),
    ("settings", "Pack settings", &["pack_get", "pack_set", "pack_unset", "profile_show", "config_get", "config_set"]),
    ("content", "Mods and content", &["search_mods", "mod_versions", "pack_add", "pack_remove", "update", "check_updates", "resolve"]),
    ("check", "Check and repair", &["check", "doctor", "diagnose", "sync", "install"]),
    ("run", "Run the game", &["launch", "stop", "restart", "sessions", "session_status", "wait_ready", "logs", "smoke_test"]),
    ("game", "In-game (needs the agent)", &["world_enter", "world_leave", "game_command", "screenshot", "reload", "agent_call"]),
    ("hot", "Hot reload", &["watch_start", "watch_stop", "watches", "watch_events", "hot_apply"]),
    ("versions", "Version catalogs", &["minecraft_versions", "loader_versions"]),
];

/// Old names that still work but are not advertised.
pub const ALIASES: &[(&str, &str)] =
    &[("agent_status", "session_status"), ("export_mrpack", "export_pack"), ("import_mrpack", "import_pack")];

/// First sentence of a description (what `tools/list` shows).
pub fn summary(desc: &str) -> String {
    // A sentence ends at ". " / ".\n" that is not inside parentheses (e.g. "(e.g. jvm.memory)").
    let mut depth = 0i32;
    let b = desc.as_bytes();
    for (i, c) in desc.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '.' if depth <= 0 && b.get(i + 1).is_none_or(|n| *n == b' ' || *n == b'\n') => {
                let s = &desc[..=i];
                // Skip abbreviations like "e.g." / "i.e.".
                if !(s.ends_with("e.g.") || s.ends_with("i.e.") || s.ends_with("etc.")) {
                    return s.to_string();
                }
            }
            _ => {}
        }
    }
    desc.to_string()
}

const TOPICS: &[(&str, &str)] = &[
    (
        "workflows",
        "Common workflows (call the tools in this order):
- Orient: overview — packs, running games, watches, pending restarts, recent errors and suggested next calls.
- New pack: pack_init name=… minecraft=1.21.1 [loader=fabric] → search_mods query=… pack=… → pack_add pack=… id=<slug> (dependencies are added automatically) → check pack=… → smoke_test pack=… profile=test.
- Import/share: import_pack file=x.mrpack|x.zip → … ; export_pack pack=… format=mrpack|curseforge out=….
- Play/test: launch pack=… [profile] [world=<level id>] (wait=true waits for the title screen) → session_status / logs tail=50 → stop.
- Something broke: diagnose pack=… (log + crash report → problems with fixes) → doctor pack=… (applies fixes, smoke=true also launches) → check pack=….
- Change a setting: help topic=pack-settings, then pack_set pack=… path=jvm.memory value=\"6G\".
- Pick a version: mod_versions id=<slug> pack=… → pack_add pack=… id=<slug> version=<version_number> (re-adding pins it); update pack=… ids=[slug] goes back to latest.
- In-game automation: launch → wait_ready → world_enter name=test flat=true (creates or joins) → game_command command=\"time set day\" → screenshot.
- Dev loop (mods/resources you are editing): launch → watch_start pack=… → edit files → watch_events watch=<id> since=<seq>. See help topic=hot-reload.",
    ),
    (
        "pack-settings",
        "pack_set / pack_get / pack_unset paths into pack.toml (comments are kept; invalid results are rejected):
- pack.name, pack.minecraft, pack.loader.kind (fabric|quilt|vanilla), pack.loader.version (\"latest\" or exact)
- jvm.memory (\"4G\"), jvm.min_memory, jvm.args ([..]), jvm.game_args, jvm.java (path to a Java), jvm.width, jvm.height
- options.<key> — options.txt, e.g. options.renderDistance = 12, options.guiScale
- config.\"<file>\".<key> — patch a mod config (json/toml/properties/txt), e.g. config.\"config/sodium-options.json\".\"quality.weather_quality\" = \"FAST\"
- mods — prefer pack_add/pack_remove; mods[0].version pins an entry
- profiles.<p>.extends | agent (bool) | options.* | config.* | jvm.* | mods | disable ([ids]) | username — profiles overlay the base pack
- dev.restart (ask|auto), dev.config_reload (restart|resources|none), dev.hotswap (bool), dev.debounce_ms, dev.no_automation_defaults
Global launcher settings (not per pack): config_get / config_set key=curseforge_api_key|username|java_path|download_concurrency value=….",
    ),
    (
        "hot-reload",
        "watch_start pack=… [profile] watches the pack (and local `source = \"file\"` mods) and applies each change to the running game:
- resource packs / data packs: reloaded live (action live_reload);
- configs: restart, or live with dev.config_reload = \"resources\" (mods that reload configs with F3+T);
- a rebuilt mod with dev.hotswap = true (set before launching): changed method bodies are hot-swapped over the JVM debug agent (action hotswapped). New classes, mixins, fabric.mod.json, added/removed mods, or (on a standard JVM) new methods/fields need a restart; hotswap_failed says why;
- anything needing a restart: action restart_needed with restart_reasons (dev.restart = \"ask\", the default) — call restart session=… to apply; with dev.restart = \"auto\" it restarts and rejoins the world (action restarted).
Read results with watch_events watch=<id> since=<last seq>. hot_apply applies the current state once without watching. A game started by another process cannot be hot-swapped from here.",
    ),
    (
        "testing",
        "smoke_test pack=… [profile=test] [timeout_secs] [world={\"name\":\"test\",\"flat\":true}] [commands=[\"time set day\"]] [screenshot=true] launches, waits for the title screen (optionally enters a world, runs commands, takes a screenshot), stops, and returns pass/fail with a diagnosis on failure. keep_running=true leaves the game up for more calls. For a running game: wait_ready, session_status (agent: title_screen, in_world, fps…), logs grep=… tail=…, screenshot. Use a `test` profile (pack_init creates one) with low render distance for speed.",
    ),
    (
        "errors",
        "Errors are {code, message, hint?}. Codes and what to do:
- not_found: wrong id/slug/session/pack — search_mods, sessions, pack_list or overview to find the right one.
- invalid: bad arguments — help tool=<name>.
- resolve: no compatible version (Minecraft/loader mismatch) — mod_versions id=… pack=… to see what exists.
- launch: the game failed to start or crashed — diagnose pack=…, then doctor pack=….
- agent: the in-game agent is not reachable or the action needs a world — session_status; world_enter for world commands.
- config: missing configuration (e.g. CurseForge key) — config_set.
- http / io / hash_mismatch: network or disk problems — retry; check disk space.",
    ),
    (
        "results",
        "Results over MCP are compact: arrays longer than 25 items are cut (with a note of how many were left out) and null fields are dropped. Pass detail=true to any tool for the full result. Prefer narrow calls: logs tail=…/grep=…, pack_get path=…, profile_show for one profile, watch_events since=….",
    ),
];

/// `help`: the map (no arguments), a topic, or one tool's full description and arguments.
pub fn help(args: &Value, defs: &[Value]) -> Result<Value, Error> {
    let find = |name: &str| defs.iter().find(|d| d["name"] == name).cloned();
    if let Some(tool) = args.get("tool").and_then(|v| v.as_str()) {
        let name = ALIASES.iter().find(|(a, _)| *a == tool).map(|(_, t)| *t).unwrap_or(tool);
        let d = find(name).ok_or_else(|| Error::NotFound(format!("no tool '{tool}' (call help for the list)")))?;
        let group = GROUPS.iter().find(|(_, _, tools)| tools.contains(&name)).map(|(_, title, _)| *title);
        return Ok(json!({
            "tool": name,
            "group": group,
            "description": d["description"],
            "arguments": d["inputSchema"]["properties"],
            "required": d["inputSchema"]["required"],
        }));
    }
    if let Some(topic) = args.get("topic").and_then(|v| v.as_str()) {
        let (_, text) = TOPICS
            .iter()
            .find(|(t, _)| *t == topic)
            .ok_or_else(|| Error::NotFound(format!("no topic '{topic}'; topics: {}", topic_names().join(", "))))?;
        return Ok(json!({"topic": topic, "text": text}));
    }
    let groups: Vec<Value> = GROUPS
        .iter()
        .map(|(_, title, tools)| {
            let list: serde_json::Map<String, Value> = tools
                .iter()
                .map(|t| (t.to_string(), json!(find(t).and_then(|d| d["description"].as_str().map(summary)).unwrap_or_default())))
                .collect();
            json!({"group": title, "tools": list})
        })
        .collect();
    Ok(json!({
        "how": "help tool=<name> for a tool's full description and arguments; help topic=<name> for a guide. Start a task with overview.",
        "topics": topic_names(),
        "groups": groups,
    }))
}

fn topic_names() -> Vec<&'static str> {
    TOPICS.iter().map(|(t, _)| *t).collect()
}

/// A short suggestion for what to call after this error, if there is a useful one.
pub fn hint(tool: &str, e: &Error) -> Option<String> {
    let h = match (e.code(), tool) {
        ("not_found", "pack_add" | "mod_versions" | "pack_remove") => "search_mods query=<name> pack=<pack> finds the right slug; pack_show lists what the pack has",
        ("not_found", t) if t.contains("session") || matches!(t, "stop" | "restart" | "logs" | "screenshot" | "game_command" | "agent_call" | "wait_ready") => {
            "overview (or sessions) lists running games; launch pack=<dir> starts one"
        }
        ("not_found", "help") => "call help with no arguments for the list of tools and topics",
        ("not_found", _) => "overview lists packs, sessions and watches",
        ("invalid", t) => return Some(format!("help tool={t} shows the arguments")),
        ("resolve", _) => "mod_versions id=<slug> pack=<pack> shows the versions compatible with this Minecraft/loader",
        ("launch", _) => "diagnose pack=<pack> explains the failure; doctor pack=<pack> fixes common problems",
        ("agent", _) => "session_status shows whether the game is at the title screen or in a world; world commands need world_enter first",
        ("config", _) => "config_set key=<name> value=<value> (e.g. curseforge_api_key)",
        ("http", _) => "network error: retry; for CurseForge check the API key (config_get)",
        _ => return None,
    };
    Some(h.into())
}

/// Shrink a result for an agent: cut long arrays and drop nulls (recursively).
pub fn compact(v: Value) -> Value {
    const MAX_ITEMS: usize = 25;
    match v {
        Value::Array(items) => {
            let total = items.len();
            let mut out: Vec<Value> = items.into_iter().take(MAX_ITEMS).map(compact).collect();
            if total > MAX_ITEMS {
                out.push(json!(format!("… {} more (pass detail=true for all)", total - MAX_ITEMS)));
            }
            Value::Array(out)
        }
        Value::Object(map) => Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, compact(v))).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_stop_at_the_first_sentence() {
        assert_eq!(summary("Launch the client (offline account). Syncs and installs first."), "Launch the client (offline account).");
        assert_eq!(summary("Set a value by path (e.g. jvm.memory). Invalid results are rejected."), "Set a value by path (e.g. jvm.memory).");
        assert_eq!(summary("No period here"), "No period here");
        assert_eq!(summary("Uses e.g. x. Then more."), "Uses e.g. x.");
    }

    #[test]
    fn compact_cuts_long_arrays_and_nulls() {
        let v = compact(json!({"a": null, "list": (0..30).collect::<Vec<_>>(), "nested": {"b": null, "c": 1}}));
        assert!(v.get("a").is_none());
        assert_eq!(v["list"].as_array().unwrap().len(), 26);
        assert_eq!(v["list"][25], "… 5 more (pass detail=true for all)");
        assert_eq!(v["nested"], json!({"c": 1}));
    }
}
