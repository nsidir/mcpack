//! Embed the prebuilt agent jar (agent-mod/dist/mcpack-agent.jar) into the library.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let jar = manifest.join("../../agent-mod/dist/mcpack-agent.jar");
    println!("cargo:rerun-if-changed={}", jar.display());
    let code = if jar.exists() {
        let abs = jar.canonicalize().unwrap_or(jar.clone());
        format!(
            "/// The bundled automation agent (a Java agent, loader- and version-independent).\npub static AGENT_JAR: Option<&[u8]> = Some(include_bytes!({:?}));\n",
            abs.display().to_string()
        )
    } else {
        "/// The bundled automation agent (missing from this build).\npub static AGENT_JAR: Option<&[u8]> = None;\n".to_string()
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("agent_jar.rs");
    std::fs::write(out, code).unwrap();
}
