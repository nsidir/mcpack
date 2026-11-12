//! Embed prebuilt agent mod jars (agent-mod/dist/mcpack-agent-<mc>.jar) into the library.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist = manifest.join("../../agent-mod/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut entries = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dist) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if let Some(ver) = name.strip_prefix("mcpack-agent-").and_then(|s| s.strip_suffix(".jar")) {
                println!("cargo:rerun-if-changed={}", p.display());
                let abs = p.canonicalize().unwrap_or(p.clone());
                entries.push(format!("    ({:?}, include_bytes!({:?})),", ver, abs.display().to_string()));
            }
        }
    }
    entries.sort();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("agent_jars.rs");
    let code = format!(
        "/// (minecraft version, jar bytes) of the bundled automation agent.\npub static AGENT_JARS: &[(&str, &[u8])] = &[\n{}\n];\n",
        entries.join("\n")
    );
    std::fs::write(out, code).unwrap();
}
