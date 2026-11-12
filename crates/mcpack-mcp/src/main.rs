//! `mcpack` — CLI and (optional) MCP server for the mcpack launcher.
//!
//!   mcpack call <tool> [json | key=value ...]   run any tool, print JSON
//!   mcpack tools                                list tools
//!   mcpack dev <pack> [profile]                 launch + hot reload + stream logs (Ctrl+C to stop)
//!   mcpack serve                                MCP server over stdio (for AI agents)

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use mcpack_core::tools::{self, CallContext};
use mcpack_core::Launcher;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, Mutex};

const HELP: &str = "mcpack — automation-first Minecraft launcher

USAGE:
  mcpack call <tool> [json | key=value ...]   Run a tool and print its JSON result
  mcpack tools [name]                         List tools (or show one tool's arguments)
  mcpack dev <pack> [profile]                 Launch, hot-reload on file changes, stream logs
  mcpack serve                                Run as an MCP server on stdio (AI agents)
  mcpack help

EXAMPLES:
  mcpack call pack_init dir=./mypack minecraft=1.21.1 loader=fabric
  mcpack call pack_add pack=./mypack id=sodium
  mcpack call smoke_test pack=./mypack profile=test
  mcpack call pack_set '{\"pack\":\"./mypack\",\"path\":\"jvm.memory\",\"value\":\"6G\"}'
  mcpack dev ./mypack test

ENV: MCPACK_HOME (data dir), CURSEFORGE_API_KEY, MCPACK_USERNAME, MCPACK_JAVA, RUST_LOG";

fn parse_args(rest: &[String]) -> Value {
    if rest.is_empty() {
        return json!({});
    }
    if rest.len() == 1 && rest[0].trim_start().starts_with('{') {
        return serde_json::from_str(&rest[0]).unwrap_or_else(|e| {
            eprintln!("invalid JSON arguments: {e}");
            std::process::exit(2);
        });
    }
    if rest.len() == 1 && rest[0] == "-" {
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
        return serde_json::from_str(&s).unwrap_or(json!({}));
    }
    let mut map = serde_json::Map::new();
    for kv in rest {
        let Some((k, v)) = kv.split_once('=') else {
            eprintln!("expected key=value, got '{kv}'");
            std::process::exit(2);
        };
        // Values that parse as JSON (numbers, bools, arrays, objects) are used as such.
        let val = match serde_json::from_str::<Value>(v) {
            Ok(j) if !j.is_string() => j,
            _ => Value::String(v.to_string()),
        };
        map.insert(k.to_string(), val);
    }
    Value::Object(map)
}

fn init_logging(default: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_target(false).try_init();
}

fn error_json(e: &mcpack_core::Error) -> Value {
    json!({"error": {"code": e.code(), "message": e.to_string()}})
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "serve" => {
            init_logging("warn");
            if let Err(e) = serve().await {
                eprintln!("mcpack serve: {e}");
                std::process::exit(1);
            }
        }
        "call" => {
            init_logging("info");
            let Some(tool) = args.get(1) else {
                eprintln!("usage: mcpack call <tool> [json | key=value ...]");
                std::process::exit(2);
            };
            let launcher = launcher_or_exit();
            let a = parse_args(&args[2..]);
            match tools::call(&launcher, tool, a, &CallContext::new("cli")).await {
                Ok(v) => {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                    // Keep supervising a game this process launched, so its logs/state are captured.
                    if tool == "launch" || (tool == "smoke_test" && v["still_running"].as_bool() == Some(true)) {
                        wait_for_games(&launcher).await;
                    }
                }
                Err(e) => {
                    println!("{}", serde_json::to_string_pretty(&error_json(&e)).unwrap_or_default());
                    std::process::exit(1);
                }
            }
        }
        "tools" => {
            let defs = tools::definitions();
            match args.get(1) {
                Some(name) => match defs.iter().find(|d| d["name"] == name.as_str()) {
                    Some(d) => println!("{}", serde_json::to_string_pretty(d).unwrap_or_default()),
                    None => {
                        eprintln!("unknown tool '{name}'");
                        std::process::exit(2);
                    }
                },
                None => {
                    for d in defs {
                        println!("{:<20} {}", d["name"].as_str().unwrap_or(""), d["description"].as_str().unwrap_or(""));
                    }
                }
            }
        }
        "dev" => {
            init_logging("info");
            let Some(pack) = args.get(1) else {
                eprintln!("usage: mcpack dev <pack> [profile]");
                std::process::exit(2);
            };
            let profile = args.get(2).cloned().unwrap_or_else(|| "default".into());
            dev(pack, &profile).await;
        }
        "version" | "--version" | "-V" => println!("mcpack {}", env!("CARGO_PKG_VERSION")),
        _ => println!("{HELP}"),
    }
}

fn launcher_or_exit() -> Launcher {
    match Launcher::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot initialize launcher: {e}");
            std::process::exit(1);
        }
    }
}

async fn wait_for_games(l: &Launcher) {
    eprintln!("[mcpack] game running; press Ctrl+C to detach (the game keeps running)");
    loop {
        if !l.sessions().into_iter().any(|s| s.running) {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = tokio::signal::ctrl_c() => break,
        }
    }
}

/// Launch + watch + stream logs: the no-AI hot reload workflow.
async fn dev(pack: &str, profile: &str) {
    let l = launcher_or_exit();
    let cx = CallContext::new("cli");
    let session = match tools::call(&l, "launch", json!({"pack": pack, "profile": profile}), &cx).await {
        Ok(v) => v["session"]["id"].as_str().unwrap_or_default().to_string(),
        Err(e) => {
            eprintln!("launch failed: {e}");
            if let Ok(d) = tools::call(&l, "diagnose", json!({"pack": pack, "profile": profile}), &cx).await {
                eprintln!("{}", serde_json::to_string_pretty(&d).unwrap_or_default());
            }
            std::process::exit(1);
        }
    };
    let watch = match tools::call(&l, "watch_start", json!({"pack": pack, "profile": profile, "session": session}), &cx).await {
        Ok(v) => v["id"].as_str().unwrap_or_default().to_string(),
        Err(e) => {
            eprintln!("watch failed: {e}");
            String::new()
        }
    };
    eprintln!("[mcpack] session {session} running; edit the pack to hot-reload (Ctrl+C stops the game)");
    let mut events = l.subscribe();
    let mut seen_watch = 0u64;
    let mut current = session;
    loop {
        tokio::select! {
            ev = events.recv() => {
                if let Ok(mcpack_core::events::Event::Log { text, .. }) = ev {
                    println!("{text}");
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if !watch.is_empty() {
                    if let Ok(evs) = l.watch_events(&watch, Some(seen_watch)) {
                        for e in evs {
                            seen_watch = e.seq;
                            if let Some(s) = &e.session {
                                current = s.clone();
                            }
                            eprintln!("[mcpack] hot reload: {} ({}){}", e.action, e.paths.join(", "),
                                e.error.as_ref().map(|x| format!(" — error: {x}")).unwrap_or_default());
                        }
                    }
                }
                let alive = l.session_status(&current).map(|s| s.running).unwrap_or(false);
                // A restart briefly has no running session; only give up when nothing runs.
                if !alive && !l.sessions().iter().any(|s| s.running) {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    if l.sessions().iter().any(|s| s.running) {
                        continue;
                    }
                    eprintln!("[mcpack] game exited");
                    if let Ok(d) = tools::call(&l, "diagnose", json!({"pack": pack, "profile": profile}), &cx).await {
                        if d["problems"].as_array().is_some_and(|p| !p.is_empty()) {
                            eprintln!("{}", serde_json::to_string_pretty(&d).unwrap_or_default());
                        }
                    }
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[mcpack] stopping…");
                let _ = tools::call(&l, "stop", json!({"session": current}), &cx).await;
                break;
            }
        }
    }
}

// ---------------------------------------------------------------- MCP server

const INSTRUCTIONS: &str = "mcpack is a Minecraft launcher where every action is a tool. Typical loop: \
pack_init → pack_add (dependencies are resolved automatically, including jar-declared ones) → check / doctor (auto-fixes problems) → \
smoke_test (launches the game, waits for the title screen, optional world + commands + screenshot, returns a diagnosis on failure). \
Tweak with pack_set (paths like jvm.memory, options.renderDistance, config.\"config/x.json\".key, profiles.<p>.*). \
For live iteration: launch, then watch_start (hot reload: resources/datapacks live, mods/JVM via auto-restart); \
inspect with logs, session_status, screenshot, game_command. Packs are directories containing pack.toml; prefer absolute paths.";

async fn serve() -> anyhow::Result<()> {
    let launcher = Launcher::new().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    // Writer: one JSON message per line on stdout (stdout carries protocol only).
    let writer = tokio::spawn(async move {
        let mut out = std::io::stdout();
        while let Some(msg) = rx.recv().await {
            let line = serde_json::to_string(&msg).unwrap_or_default();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    });
    let client: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}));
                continue;
            }
        };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let Some(id) = msg.get("id").cloned() else {
            continue; // notification (e.g. notifications/initialized)
        };
        match method.as_str() {
            "initialize" => {
                *client.lock().await = params.pointer("/clientInfo/name").and_then(|v| v.as_str()).map(str::to_string);
                let version = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2025-06-18").to_string();
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "mcpack", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }}));
            }
            "ping" => {
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": {}}));
            }
            "tools/list" => {
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools::definitions()}}));
            }
            "tools/call" => {
                let launcher = launcher.clone();
                let tx = tx.clone();
                let client = client.lock().await.clone();
                // Calls run concurrently: a long smoke test doesn't block logs/status calls.
                tokio::spawn(async move {
                    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    let cx = CallContext { origin: "mcp".into(), client };
                    let result = match tools::call(&launcher, &name, args, &cx).await {
                        Ok(v) => {
                            let mut content = vec![json!({"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()})];
                            let shot = if name == "screenshot" { v.get("path") } else { v.get("screenshot") };
                            if let Some(path) = shot.and_then(|p| p.as_str()) {
                                if let Ok(bytes) = std::fs::read(path) {
                                    content.push(json!({
                                        "type": "image",
                                        "mimeType": "image/png",
                                        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                                    }));
                                }
                            }
                            json!({"content": content, "isError": false})
                        }
                        Err(e) => json!({
                            "content": [{"type": "text", "text": serde_json::to_string_pretty(&error_json(&e)).unwrap_or_default()}],
                            "isError": true,
                        }),
                    };
                    let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
                });
            }
            "resources/list" => {
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": {"resources": []}}));
            }
            "prompts/list" => {
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": {"prompts": []}}));
            }
            other => {
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}}));
            }
        }
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}
