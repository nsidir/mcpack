//! Hot-swap: push changed mod code into a running game without restarting it.
//!
//! With `dev.hotswap = true` the game is launched with the JVM's debug agent listening on a
//! local port (`-agentlib:jdwp=...,address=127.0.0.1:<port>`). When a mod jar changes, the
//! watcher compares the jar the game is running with the new one ([`diff_jars`]) and redefines
//! the changed classes over JDWP ([`redefine`]) — the same thing an IDE does in debug mode.
//!
//! What falls back to a restart:
//! - on a standard JVM, anything beyond changes inside method bodies (adding or removing
//!   methods or fields, changing signatures, modifiers or the class hierarchy). The JetBrains
//!   Runtime accepts most of these; `-XX:+AllowEnhancedClassRedefinition` is added
//!   automatically when the game runs on it;
//! - new classes, mixin classes, and what Fabric reads only at startup (fabric.mod.json, mixin
//!   configs and refmaps, access wideners, nested jars);
//! - mods added or removed.
//!
//! What waits for the next launch (reported, not an error): changed classes the game has not
//! loaded yet and changed non-class files (assets, lang, data) inside the jar — the game keeps
//! the original jar open, so it reads those from the old file.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Debug-agent port of a session launched with hot-swap.
#[derive(Debug, Clone, Serialize)]
pub struct HotswapEndpoint {
    pub port: u16,
    /// Running on the JetBrains Runtime with enhanced class redefinition.
    pub enhanced: bool,
}

impl HotswapEndpoint {
    pub fn jvm_args(&self) -> Vec<String> {
        let mut a = vec![format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=127.0.0.1:{}", self.port)];
        if self.enhanced {
            a.push("-XX:+AllowEnhancedClassRedefinition".into());
        }
        a
    }
}

/// Prepare hot-swap for a launch with the `java` executable: pick a free local port, provided
/// the runtime ships the debug agent. `Err` explains why hot-swap is unavailable.
pub fn endpoint_for(java: &Path) -> Result<HotswapEndpoint, String> {
    let home = java_home(java).ok_or_else(|| format!("cannot find the Java home of {}", java.display()))?;
    let (dir, prefix, ext) = if cfg!(windows) {
        ("bin", "", "dll")
    } else if cfg!(target_os = "macos") {
        ("lib", "lib", "dylib")
    } else {
        ("lib", "lib", "so")
    };
    for lib in ["jdwp", "dt_socket"] {
        let p = home.join(dir).join(format!("{prefix}{lib}.{ext}"));
        if !p.exists() {
            return Err(format!("the Java runtime at {} has no debug agent ({} is missing)", home.display(), p.display()));
        }
    }
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("no free local port: {e}"))?;
    Ok(HotswapEndpoint { port, enhanced: is_jetbrains_runtime(&home) })
}

fn java_home(java: &Path) -> Option<PathBuf> {
    // <home>/bin/java(.exe)
    java.parent().filter(|b| b.file_name().is_some_and(|n| n == "bin")).and_then(Path::parent).map(Path::to_path_buf)
}

fn is_jetbrains_runtime(home: &Path) -> bool {
    std::fs::read_to_string(home.join("release")).is_ok_and(|r| r.contains("JetBrains"))
}

// ------------------------------------------------------------------ jar diff

/// What changed between the jar the game is running and the new one.
#[derive(Debug, Clone, Default, Serialize)]
pub struct JarDiff {
    /// Changed classes (binary names, `a.b.C`) with their new class files.
    #[serde(skip)]
    pub changed: Vec<(String, Vec<u8>)>,
    pub changed_classes: Vec<String>,
    pub added_classes: Vec<String>,
    pub removed_classes: Vec<String>,
    /// Changes Fabric only reads at startup (metadata, mixins, access wideners, nested jars).
    pub startup_changes: Vec<String>,
    /// Non-class files that changed: they keep their old version until the next launch.
    pub resources_changed: Vec<String>,
}

impl JarDiff {
    /// Why this diff cannot be applied to a running game, if it cannot.
    pub fn blocker(&self) -> Option<String> {
        if !self.startup_changes.is_empty() {
            return Some(format!("{} changed, which Fabric only reads at startup", short_list(&self.startup_changes)));
        }
        if !self.added_classes.is_empty() {
            return Some(format!("new classes ({}) cannot be loaded into the running game", short_list(&self.added_classes)));
        }
        None
    }
}

/// Compare two mod jars.
pub fn diff_jars(old: &Path, new: &Path) -> Result<JarDiff, String> {
    let old_entries = read_jar(old)?;
    let new_entries = read_jar(new)?;
    let mut mixin_pkgs = mixin_packages(&old_entries);
    mixin_pkgs.extend(mixin_packages(&new_entries));
    let startup_files: BTreeSet<String> = startup_files(&old_entries).into_iter().chain(startup_files(&new_entries)).collect();
    let is_mixin = |class: &str| mixin_pkgs.iter().any(|p| class.starts_with(&format!("{p}.")));

    let mut d = JarDiff::default();
    let names: BTreeSet<&String> = old_entries.keys().chain(new_entries.keys()).collect();
    for name in names {
        if ignored_entry(name) {
            continue;
        }
        let (o, n) = (old_entries.get(name), new_entries.get(name));
        if o == n {
            continue;
        }
        let class = class_name(name);
        let startup = startup_files.contains(name) || name.starts_with("META-INF/jars/");
        match (class, o, n) {
            (Some(c), _, _) if is_mixin(&c) => d.startup_changes.push(format!("mixin class {c}")),
            (Some(c), Some(_), Some(bytes)) => {
                d.changed_classes.push(c.clone());
                d.changed.push((c, bytes.clone()));
            }
            (Some(c), None, Some(_)) => d.added_classes.push(c),
            (Some(c), Some(_), None) => d.removed_classes.push(c),
            (None, _, _) if startup => d.startup_changes.push(name.clone()),
            (None, _, _) => d.resources_changed.push(name.clone()),
            (Some(_), None, None) => {}
        }
    }
    Ok(d)
}

fn read_jar(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| format!("{} is not a valid jar: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let mut e = zip.by_index(i).map_err(|e| format!("{}: {e}", path.display()))?;
        if e.is_dir() {
            continue;
        }
        let mut buf = Vec::with_capacity(e.size() as usize);
        e.read_to_end(&mut buf).map_err(|err| format!("{}: {}: {err}", path.display(), e.name()))?;
        out.insert(e.name().to_string(), buf);
    }
    Ok(out)
}

fn ignored_entry(name: &str) -> bool {
    // Build metadata that changes on every build without changing behaviour.
    name == "META-INF/MANIFEST.MF" || (name.starts_with("META-INF/") && [".SF", ".RSA", ".DSA", ".EC"].iter().any(|s| name.ends_with(s)))
}

/// `a/b/C.class` → `a.b.C` (not for module-info or multi-release variants).
fn class_name(entry: &str) -> Option<String> {
    let stem = entry.strip_suffix(".class")?;
    if stem.ends_with("module-info") || stem.ends_with("package-info") || entry.starts_with("META-INF/") {
        return None;
    }
    Some(stem.replace('/', "."))
}

fn json_of(entries: &BTreeMap<String, Vec<u8>>, name: &str) -> Option<serde_json::Value> {
    entries.get(name).and_then(|b| serde_json::from_slice(b).ok())
}

/// Mixin config file names declared in fabric.mod.json.
fn mixin_configs(entries: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let Some(meta) = json_of(entries, "fabric.mod.json") else { return vec![] };
    meta.get("mixins")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(str::to_string).or_else(|| m.get("config").and_then(|c| c.as_str()).map(str::to_string)))
                .collect()
        })
        .unwrap_or_default()
}

fn mixin_packages(entries: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    mixin_configs(entries)
        .iter()
        .filter_map(|c| json_of(entries, c))
        .filter_map(|cfg| cfg.get("package").and_then(|p| p.as_str()).map(str::to_string))
        .collect()
}

/// Files Fabric (or Quilt) reads only while starting the game.
fn startup_files(entries: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut out = vec!["fabric.mod.json".to_string(), "quilt.mod.json".to_string()];
    let configs = mixin_configs(entries);
    for c in &configs {
        if let Some(r) = json_of(entries, c).and_then(|cfg| cfg.get("refmap").and_then(|r| r.as_str()).map(str::to_string)) {
            out.push(r);
        }
    }
    out.extend(configs);
    if let Some(aw) = json_of(entries, "fabric.mod.json").and_then(|m| m.get("accessWidener").and_then(|a| a.as_str()).map(str::to_string)) {
        out.push(aw);
    }
    out
}

fn short_list(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1..=3 => items.join(", "),
        n => format!("{} and {} more", items[..3].join(", "), n - 3),
    }
}

// ---------------------------------------------------------------------- JDWP

/// Result of redefining classes in the running game.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Redefined {
    /// Classes replaced in the running game.
    pub classes: Vec<String>,
    /// Changed classes the game has not loaded yet: they load from the original jar (old
    /// version) if they are needed before the next launch.
    pub not_loaded: Vec<String>,
}

/// Redefine `classes` (binary name → new class file) in the JVM whose debug agent listens on
/// `port`. All-or-nothing: on error nothing was changed. The error explains why.
pub async fn redefine(port: u16, classes: &[(String, Vec<u8>)]) -> Result<Redefined, String> {
    let mut out = Redefined::default();
    if classes.is_empty() {
        return Ok(out);
    }
    let mut j = Jdwp::connect(port).await.map_err(|e| e.explain())?;
    let r = async {
        let caps = j.command(1, 17, &[]).await?; // VirtualMachine.CapabilitiesNew
        if caps.get(7) == Some(&0) {
            return Err(JdwpError::Other("this JVM cannot redefine classes".into()));
        }
        let mut targets: Vec<(Vec<u8>, &[u8])> = Vec::new();
        for (name, bytes) in classes {
            let ids = j.classes_by_signature(&format!("L{};", name.replace('.', "/"))).await?;
            if ids.is_empty() {
                out.not_loaded.push(name.clone());
            } else {
                out.classes.push(name.clone());
                targets.extend(ids.into_iter().map(|id| (id, bytes.as_slice())));
            }
        }
        if !targets.is_empty() {
            j.redefine(&targets).await?;
        }
        Ok(())
    }
    .await;
    j.dispose().await;
    r.map_err(|e| e.explain())?;
    Ok(out)
}

#[derive(Debug)]
enum JdwpError {
    Io(String),
    Code(u16),
    Other(String),
}

impl JdwpError {
    fn explain(&self) -> String {
        const BODY_ONLY: &str =
            "a standard JVM can only hot-swap changes inside method bodies (running the game on the JetBrains Runtime allows more)";
        match self {
            JdwpError::Io(e) => format!("cannot reach the game's debug agent: {e}"),
            JdwpError::Other(e) => e.clone(),
            JdwpError::Code(c) => match c {
                60 => "a changed class file is invalid".into(),
                61 => "the change creates a circular class hierarchy".into(),
                62 => "the new code fails bytecode verification".into(),
                63 => format!("the change adds a method; {BODY_ONLY}"),
                64 => format!("the change adds, removes or retypes fields; {BODY_ONLY}"),
                66 => format!("the change alters a superclass or interfaces; {BODY_ONLY}"),
                67 => format!("the change removes a method; {BODY_ONLY}"),
                68 => "the classes were compiled for a newer Java than the game runs on".into(),
                69 => "a class name in the new jar does not match".into(),
                70 => format!("the change alters class modifiers; {BODY_ONLY}"),
                71 => format!("the change alters method modifiers (such as static or private); {BODY_ONLY}"),
                72 => format!("the change alters class attributes (such as nested classes or record components); {BODY_ONLY}"),
                99 => "this JVM does not support class redefinition".into(),
                other => format!("the JVM refused the change (JDWP error {other})"),
            },
        }
    }
}

impl From<std::io::Error> for JdwpError {
    fn from(e: std::io::Error) -> Self {
        JdwpError::Io(e.to_string())
    }
}

const HANDSHAKE: &[u8] = b"JDWP-Handshake";
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);

struct Jdwp {
    s: TcpStream,
    next_id: u32,
    ref_size: usize,
}

impl Jdwp {
    async fn connect(port: u16) -> Result<Self, JdwpError> {
        let mut s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(("127.0.0.1", port)))
            .await
            .map_err(|_| JdwpError::Io("connection timed out".into()))??;
        s.write_all(HANDSHAKE).await?;
        let mut hs = [0u8; 14];
        tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut hs))
            .await
            .map_err(|_| JdwpError::Io("no handshake (another debugger may be attached)".into()))??;
        if hs != HANDSHAKE {
            return Err(JdwpError::Io("unexpected handshake".into()));
        }
        let mut j = Self { s, next_id: 1, ref_size: 8 };
        let sizes = j.command(1, 7, &[]).await?; // VirtualMachine.IDSizes
        if sizes.len() >= 16 {
            j.ref_size = u32::from_be_bytes([sizes[12], sizes[13], sizes[14], sizes[15]]) as usize;
        }
        Ok(j)
    }

    async fn command(&mut self, set: u8, cmd: u8, data: &[u8]) -> Result<Vec<u8>, JdwpError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut p = Vec::with_capacity(11 + data.len());
        p.extend_from_slice(&((11 + data.len()) as u32).to_be_bytes());
        p.extend_from_slice(&id.to_be_bytes());
        p.push(0);
        p.push(set);
        p.push(cmd);
        p.extend_from_slice(data);
        self.s.write_all(&p).await?;
        loop {
            let mut h = [0u8; 11];
            tokio::time::timeout(REPLY_TIMEOUT, self.s.read_exact(&mut h))
                .await
                .map_err(|_| JdwpError::Io("no reply from the debug agent".into()))??;
            let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
            let mut body = vec![0u8; len.saturating_sub(11)];
            self.s.read_exact(&mut body).await?;
            let rid = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
            // Skip event packets (e.g. VM start) and anything that is not our reply.
            if h[8] & 0x80 != 0 && rid == id {
                let err = u16::from_be_bytes([h[9], h[10]]);
                return if err == 0 { Ok(body) } else { Err(JdwpError::Code(err)) };
            }
        }
    }

    /// Reference type ids of loaded classes with this JNI signature (one per class loader).
    async fn classes_by_signature(&mut self, sig: &str) -> Result<Vec<Vec<u8>>, JdwpError> {
        let mut data = (sig.len() as u32).to_be_bytes().to_vec();
        data.extend_from_slice(sig.as_bytes());
        let r = self.command(1, 2, &data).await?; // VirtualMachine.ClassesBySignature
        let n = u32::from_be_bytes(r.get(0..4).and_then(|b| b.try_into().ok()).ok_or_else(short)?) as usize;
        let step = 1 + self.ref_size + 4;
        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            let off = 4 + i * step;
            let rec = r.get(off..off + step).ok_or_else(short)?;
            let status = u32::from_be_bytes(rec[1 + self.ref_size..].try_into().map_err(|_| short())?);
            if status & 8 == 0 {
                // not in error state
                ids.push(rec[1..1 + self.ref_size].to_vec());
            }
        }
        Ok(ids)
    }

    async fn redefine(&mut self, classes: &[(Vec<u8>, &[u8])]) -> Result<(), JdwpError> {
        let mut data = (classes.len() as u32).to_be_bytes().to_vec();
        for (id, bytes) in classes {
            data.extend_from_slice(id);
            data.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            data.extend_from_slice(bytes);
        }
        self.command(1, 18, &data).await.map(|_| ()) // VirtualMachine.RedefineClasses
    }

    /// Detach cleanly so the agent accepts the next connection.
    async fn dispose(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.command(1, 6, &[])).await; // VirtualMachine.Dispose
        let _ = self.s.shutdown().await;
    }
}

fn short() -> JdwpError {
    JdwpError::Io("truncated reply".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn jar(path: &Path, files: &[(&str, &[u8])]) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    const META: &[u8] = br#"{"schemaVersion":1,"id":"m","version":"1","mixins":["m.mixins.json"],"accessWidener":"m.accesswidener"}"#;
    const MIXINS: &[u8] = br#"{"package":"com.example.mixin","refmap":"m-refmap.json","client":["TitleMixin"]}"#;

    #[test]
    fn classifies_jar_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("old.jar"), dir.path().join("new.jar"));
        jar(
            &old,
            &[
                ("fabric.mod.json", META),
                ("m.mixins.json", MIXINS),
                ("META-INF/MANIFEST.MF", b"Built: 1"),
                ("com/example/Mod.class", b"v1"),
                ("com/example/Same.class", b"same"),
                ("com/example/Gone.class", b"x"),
                ("assets/m/lang/en_us.json", b"{}"),
            ],
        );
        jar(
            &new,
            &[
                ("fabric.mod.json", META),
                ("m.mixins.json", MIXINS),
                ("META-INF/MANIFEST.MF", b"Built: 2"),
                ("com/example/Mod.class", b"v2"),
                ("com/example/Same.class", b"same"),
                ("assets/m/lang/en_us.json", b"{\"a\":\"b\"}"),
            ],
        );
        let d = diff_jars(&old, &new).unwrap();
        assert_eq!(d.changed_classes, ["com.example.Mod"]);
        assert_eq!(d.changed[0].1, b"v2");
        assert_eq!(d.removed_classes, ["com.example.Gone"]);
        assert_eq!(d.resources_changed, ["assets/m/lang/en_us.json"]);
        assert!(d.startup_changes.is_empty() && d.added_classes.is_empty(), "{d:?}");
        assert!(d.blocker().is_none());

        // Mixin classes, new classes and Fabric metadata block a hot-swap.
        jar(
            &new,
            &[
                ("fabric.mod.json", META),
                ("m.mixins.json", MIXINS),
                ("m.accesswidener", b"accessWidener v2 named"),
                ("com/example/Mod.class", b"v1"),
                ("com/example/Same.class", b"same"),
                ("com/example/Gone.class", b"x"),
                ("com/example/Extra.class", b"new"),
                ("com/example/mixin/TitleMixin.class", b"mixin"),
                ("assets/m/lang/en_us.json", b"{}"),
            ],
        );
        let d = diff_jars(&old, &new).unwrap();
        assert!(d.changed_classes.is_empty());
        assert_eq!(d.added_classes, ["com.example.Extra"]);
        assert_eq!(d.startup_changes, ["mixin class com.example.mixin.TitleMixin", "m.accesswidener"]);
        assert!(d.blocker().unwrap().contains("only reads at startup"));
    }

    #[test]
    fn jvm_args_bind_locally() {
        let e = HotswapEndpoint { port: 5005, enhanced: true };
        assert_eq!(
            e.jvm_args(),
            ["-agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=127.0.0.1:5005", "-XX:+AllowEnhancedClassRedefinition"]
        );
    }

    /// End-to-end against a real JVM. Needs a JDK: set MCPACK_TEST_JDK to its home, e.g.
    /// `MCPACK_TEST_JDK="C:/Program Files/Java/jdk-21" cargo test -p mcpack-core hotswap -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn redefines_classes_in_a_running_jvm() {
        let jdk = PathBuf::from(std::env::var("MCPACK_TEST_JDK").expect("set MCPACK_TEST_JDK"));
        let exe = |n: &str| jdk.join("bin").join(if cfg!(windows) { format!("{n}.exe") } else { n.to_string() });
        let dir = tempfile::tempdir().unwrap();
        let compile = |tag: &str, src_text: &str| {
            let src = dir.path().join(tag);
            std::fs::create_dir_all(src.join("demo")).unwrap();
            std::fs::write(src.join("demo/Main.java"), src_text).unwrap();
            let ok = std::process::Command::new(exe("javac")).arg("demo/Main.java").current_dir(&src).status().unwrap();
            assert!(ok.success());
            src
        };
        let main = |msg_body: &str, extra: &str| {
            format!(
                "package demo; public class Main {{ {extra} static String msg() {{ {msg_body} }} \
                 public static void main(String[] a) throws Exception {{ \
                 while (true) {{ System.out.println(msg()); System.out.flush(); Thread.sleep(100); }} }} }}"
            )
        };
        let v1 = compile("v1", &main("return \"one\";", ""));
        let v2 = compile("v2", &main("return \"two\";", ""));
        // Adds a method: refused by a standard JVM.
        let v3 = compile("v3", &main("return extra();", "static String extra() { return \"x\"; }"));

        let ep = endpoint_for(&exe("java")).expect("the JDK has a debug agent");
        let mut child = tokio::process::Command::new(exe("java"))
            .args(ep.jvm_args())
            .args(["-cp", &v1.display().to_string(), "demo.Main"])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut out = tokio::io::BufReader::new(child.stdout.take().unwrap());
        async fn wait_for<R: tokio::io::AsyncBufRead + Unpin>(out: &mut R, want: &str) {
            use tokio::io::AsyncBufReadExt;
            let mut line = String::new();
            for _ in 0..300 {
                line.clear();
                out.read_line(&mut line).await.unwrap();
                if line.trim() == want {
                    return;
                }
            }
            panic!("never printed {want}");
        }
        wait_for(&mut out, "one").await;

        let bytes = |src: &Path| std::fs::read(src.join("demo/Main.class")).unwrap();
        let r = redefine(ep.port, &[("demo.Main".into(), bytes(&v2)), ("demo.NotLoaded".into(), b"x".to_vec())]).await.unwrap();
        assert_eq!(r.classes, ["demo.Main"]);
        assert_eq!(r.not_loaded, ["demo.NotLoaded"]);
        wait_for(&mut out, "two").await;

        // A second connection works (the agent listens again after we detach), and a schema
        // change is refused with an explanation, leaving the running code alone.
        let err = redefine(ep.port, &[("demo.Main".into(), bytes(&v3))]).await.unwrap_err();
        assert!(err.contains("adds a method"), "{err}");
        wait_for(&mut out, "two").await;

        // And it still accepts good changes afterwards.
        redefine(ep.port, &[("demo.Main".into(), bytes(&v1))]).await.unwrap();
        wait_for(&mut out, "one").await;

        child.kill().await.unwrap();
    }
}
