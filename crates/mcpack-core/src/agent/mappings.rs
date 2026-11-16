//! Name mappings for the agent: Mojang (named) names → the names the running game uses.
//!
//! | runtime                          | names                         | source                          |
//! |----------------------------------|-------------------------------|---------------------------------|
//! | 26.1+ (any loader)               | Mojang (not obfuscated)       | none needed                     |
//! | vanilla 1.14.4 – 1.21.11         | obfuscated                    | Mojang client mappings          |
//! | Fabric / Quilt                   | intermediary                  | + Fabric intermediary (tiny v2) |
//! | Forge ≤ 1.20.4, NeoForge ≤ 1.20.4| SRG members (MCP/Mojang classes) | + installer `MAPPINGS` (tsrg) |
//! | Forge 1.20.6+, NeoForge 1.20.5+  | Mojang                        | none needed                     |
//! | Forge 1.7.10 – 1.12.2            | SRG, agent code uses MCP names | MCP srg + names (see [`legacy`]) |
//!
//! Output format (read by `agent-mod/src/main/java/dev/mcpack/agent/Mappings.java`):
//! `C <named> <runtime>` then `M <named> <runtime> <named param types>` / `F <named> <runtime>`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{Error, IoContext, Result};
use crate::http::DownloadItem;
use crate::install::GameInstall;
use crate::meta::loader::LoaderKind;
use crate::util;
use crate::Ctx;

pub mod legacy;

/// How the agent can reach a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSupport {
    /// The runtime uses the names the agent is written against.
    Named,
    /// A mappings file translates them.
    Mapped(PathBuf),
    /// No agent for this version/loader, with the reason.
    Unsupported(String),
}

/// `1.20.4` → `[1, 20, 4]`; `26.1` → `[26, 1]`; snapshots and odd ids → `None`.
pub fn mc_version_parts(mc: &str) -> Option<Vec<u32>> {
    mc.split('.').map(|p| p.parse().ok()).collect()
}

fn mc_cmp(mc: &str, other: &str) -> Option<std::cmp::Ordering> {
    Some(mc_version_parts(mc)?.cmp(&mc_version_parts(other)?))
}

/// Unobfuscated releases (year-based versions, 26.1+).
fn unobfuscated(mc: &str) -> bool {
    mc_version_parts(mc).is_some_and(|p| p.first().is_some_and(|m| *m >= 25))
}

/// Does this Forge-like runtime use SRG member names (versus Mojang names)?
fn forge_srg_runtime(kind: LoaderKind, mc: &str) -> bool {
    match kind {
        LoaderKind::Forge => mc_cmp(mc, "1.20.6").is_some_and(|o| o.is_lt()),
        LoaderKind::NeoForge => mc_cmp(mc, "1.20.5").is_some_and(|o| o.is_lt()),
        _ => false,
    }
}

/// Prepare whatever the agent needs for this installed game.
pub async fn prepare(ctx: &Ctx, inst: &GameInstall) -> Result<AgentSupport> {
    let mc = inst.minecraft.as_str();
    if unobfuscated(mc) {
        return Ok(AgentSupport::Named);
    }
    let client_mappings = inst.version.downloads.as_ref().and_then(|d| d.client_mappings.clone());
    let Some(mojmap) = client_mappings else {
        // Before 1.14.4 Mojang published no mappings.
        if matches!(inst.loader, LoaderKind::Forge | LoaderKind::Vanilla) {
            return legacy::prepare(ctx, inst).await;
        }
        return Ok(AgentSupport::Unsupported(format!(
            "the in-game agent needs Minecraft 1.14.4+ on {} (vanilla and Forge also work on 1.7.10 – 1.12.2)",
            inst.loader.display()
        )));
    };
    if inst.loader.is_forge_like() && !forge_srg_runtime(inst.loader, mc) {
        return Ok(AgentSupport::Named);
    }
    let (namespace, target): (&str, Option<PathBuf>) = match inst.loader {
        LoaderKind::Vanilla => ("official", None),
        LoaderKind::Fabric | LoaderKind::Quilt => ("intermediary", Some(intermediary_file(ctx, mc).await?)),
        LoaderKind::Forge | LoaderKind::NeoForge => {
            let info = crate::install::forge::installer_info(ctx, inst.loader, mc, &inst.loader_version)
                .ok_or_else(|| Error::Agent("Forge install info missing; reinstall the pack".into()))?;
            let file = info.runtime_mappings.ok_or_else(|| Error::Agent("this Forge build ships no SRG mappings".into()))?;
            ("srg", Some(file))
        }
    };
    let mojmap_path = ctx.paths.versions().join(mc).join(format!("{mc}-client-mappings.txt"));
    ctx.http
        .download(&DownloadItem { url: mojmap.url.clone(), dest: mojmap_path.clone(), sha1: mojmap.sha1.clone(), size: mojmap.size })
        .await?;
    let key = util::sha1_bytes(
        format!("{}|{}|{}", util::sha1_file(&mojmap_path)?, namespace, target.as_ref().map(|p| p.display().to_string()).unwrap_or_default())
            .as_bytes(),
    );
    let out = ctx.paths.meta().join("agent-mappings").join(format!("{mc}-{namespace}-{}.txt", &key[..10]));
    if out.exists() {
        return Ok(AgentSupport::Mapped(out));
    }
    let mojmap_text = std::fs::read_to_string(&mojmap_path).ctx(&mojmap_path)?;
    let target_map = match &target {
        None => None,
        Some(p) if namespace == "intermediary" => Some(parse_tiny_jar(p)?),
        Some(p) => Some(parse_tsrg(&std::fs::read_to_string(p).ctx(p)?)),
    };
    let text = tokio::task::spawn_blocking(move || compose(&parse_mojmap(&mojmap_text), target_map.as_ref(), namespace))
        .await
        .map_err(|e| Error::Other(e.to_string()))?;
    util::write_atomic(&out, text.as_bytes())?;
    Ok(AgentSupport::Mapped(out))
}

async fn intermediary_file(ctx: &Ctx, mc: &str) -> Result<PathBuf> {
    let rel = format!("net/fabricmc/intermediary/{mc}/intermediary-{mc}-v2.jar");
    let dest = ctx.paths.libraries().join(&rel);
    ctx.http
        .download(&DownloadItem { url: format!("https://maven.fabricmc.net/{rel}"), dest: dest.clone(), sha1: None, size: None })
        .await?;
    Ok(dest)
}

// ---------------------------------------------------------------- parsing

#[derive(Debug, Default)]
pub struct MojClass {
    pub named: String,
    pub obf: String,
    pub fields: Vec<(String, String)>,
    /// (named, obf, return type, param types) in Java source notation.
    pub methods: Vec<(String, String, String, Vec<String>)>,
}

/// Mojang ProGuard mappings.
pub fn parse_mojmap(text: &str) -> Vec<MojClass> {
    let mut out: Vec<MojClass> = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            if let Some((named, rest)) = line.split_once(" -> ") {
                out.push(MojClass { named: named.to_string(), obf: rest.trim_end_matches(':').to_string(), ..Default::default() });
            }
            continue;
        }
        let Some(c) = out.last_mut() else { continue };
        let l = line.trim();
        if l.starts_with('#') {
            continue;
        }
        let Some((decl, obf)) = l.split_once(" -> ") else { continue };
        // Strip "12:34:" line numbers.
        let decl = decl.trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == ':');
        let Some((ty, rest)) = decl.split_once(' ') else { continue };
        if let Some((name, params)) = rest.split_once('(') {
            let params = params.trim_end_matches(')');
            let ps = if params.is_empty() { vec![] } else { params.split(',').map(str::to_string).collect() };
            c.methods.push((name.to_string(), obf.to_string(), ty.to_string(), ps));
        } else {
            c.fields.push((rest.to_string(), obf.to_string()));
        }
    }
    out
}

/// obf → runtime names (keys use obf names, `/`-separated classes; method keys include the obf descriptor).
#[derive(Debug, Default)]
pub struct TargetMap {
    pub classes: HashMap<String, String>,
    pub methods: HashMap<(String, String, String), String>,
    pub fields: HashMap<(String, String), String>,
}

fn parse_tiny_jar(path: &Path) -> Result<TargetMap> {
    let f = std::fs::File::open(path).ctx(path)?;
    let mut zip = zip::ZipArchive::new(f)?;
    let mut text = String::new();
    zip.by_name("mappings/mappings.tiny")
        .map_err(|_| Error::Agent(format!("{} has no mappings.tiny", path.display())))?
        .read_to_string(&mut text)
        .ctx(path)?;
    Ok(parse_tiny(&text))
}

/// Tiny v2 with columns `official intermediary`.
pub fn parse_tiny(text: &str) -> TargetMap {
    let mut m = TargetMap::default();
    let mut owner = String::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        match cols.as_slice() {
            ["c", obf, int, ..] => {
                owner = obf.to_string();
                m.classes.insert(obf.to_string(), int.to_string());
            }
            ["", "m", desc, obf, int, ..] => {
                m.methods.insert((owner.clone(), obf.to_string(), desc.to_string()), int.to_string());
            }
            ["", "f", _desc, obf, int, ..] => {
                m.fields.insert((owner.clone(), obf.to_string()), int.to_string());
            }
            _ => {}
        }
    }
    m
}

/// TSRG v1 / v2 (first two name columns: obf → target).
pub fn parse_tsrg(text: &str) -> TargetMap {
    let mut m = TargetMap::default();
    let mut owner = String::new();
    for line in text.lines() {
        if line.starts_with("tsrg2 ") || line.trim().is_empty() || line.starts_with("\t\t") {
            continue;
        }
        let cols: Vec<&str> = line.trim().split(' ').collect();
        if !line.starts_with('\t') {
            if cols.len() >= 2 {
                owner = cols[0].to_string();
                m.classes.insert(cols[0].to_string(), cols[1].to_string());
            }
            continue;
        }
        if cols.len() >= 3 && cols[1].starts_with('(') {
            m.methods.insert((owner.clone(), cols[0].to_string(), cols[1].to_string()), cols[2].to_string());
        } else if cols.len() >= 2 {
            m.fields.insert((owner.clone(), cols[0].to_string()), cols[1].to_string());
        }
    }
    m
}

fn descriptor(ty: &str, class_obf: &HashMap<&str, &str>) -> String {
    let mut t = ty;
    let mut out = String::new();
    while let Some(inner) = t.strip_suffix("[]") {
        out.push('[');
        t = inner;
    }
    out.push_str(match t {
        "void" => "V",
        "boolean" => "Z",
        "byte" => "B",
        "char" => "C",
        "short" => "S",
        "int" => "I",
        "long" => "J",
        "float" => "F",
        "double" => "D",
        _ => {
            let name = class_obf.get(t).copied().unwrap_or(t);
            out.push('L');
            out.push_str(&name.replace('.', "/"));
            out.push(';');
            return out;
        }
    });
    out
}

/// Build the agent's mappings file text.
pub fn compose(classes: &[MojClass], target: Option<&TargetMap>, namespace: &str) -> String {
    let class_obf: HashMap<&str, &str> = classes.iter().map(|c| (c.named.as_str(), c.obf.as_str())).collect();
    let mut out = String::with_capacity(4 << 20);
    let _ = writeln!(out, "mcpack-mappings 1 {namespace}");
    for c in classes {
        let obf_slash = c.obf.replace('.', "/");
        let runtime = match target {
            None => c.obf.clone(),
            Some(t) => t.classes.get(&obf_slash).map(|r| r.replace('/', ".")).unwrap_or_else(|| c.obf.clone()),
        };
        let mut body = String::new();
        for (named, obf) in &c.fields {
            let rt = match target {
                None => obf.clone(),
                Some(t) => t.fields.get(&(obf_slash.clone(), obf.clone())).cloned().unwrap_or_else(|| obf.clone()),
            };
            if &rt != named {
                let _ = writeln!(body, "F {named} {rt}");
            }
        }
        for (named, obf, ret, params) in &c.methods {
            if named == "<init>" || named == "<clinit>" || named.starts_with("lambda$") {
                continue;
            }
            let rt = match target {
                None => obf.clone(),
                Some(t) => {
                    let desc = format!(
                        "({}){}",
                        params.iter().map(|p| descriptor(p, &class_obf)).collect::<String>(),
                        descriptor(ret, &class_obf)
                    );
                    t.methods.get(&(obf_slash.clone(), obf.clone(), desc)).cloned().unwrap_or_else(|| obf.clone())
                }
            };
            if &rt != named {
                let ps = if params.is_empty() { "-".to_string() } else { params.join(",") };
                let _ = writeln!(body, "M {named} {rt} {ps}");
            }
        }
        if runtime != c.named || !body.is_empty() {
            let _ = writeln!(out, "C {} {runtime}", c.named);
            out.push_str(&body);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOJ: &str = r#"# comment
net.minecraft.client.Minecraft -> evi:
# {"fileName":"Minecraft.java","id":"sourceFile"}
    net.minecraft.client.player.LocalPlayer player -> t
    1:2:net.minecraft.client.Minecraft getInstance() -> N
    3:4:void setScreen(net.minecraft.client.gui.screens.Screen) -> a
    void <init>(int) -> <init>
net.minecraft.client.player.LocalPlayer -> fiy:
net.minecraft.client.gui.screens.Screen -> exd:
"#;

    #[test]
    fn vanilla_names() {
        let text = compose(&parse_mojmap(MOJ), None, "official");
        assert!(text.contains("C net.minecraft.client.Minecraft evi\n"));
        assert!(text.contains("F player t\n"));
        assert!(text.contains("M getInstance N -\n"));
        assert!(text.contains("M setScreen a net.minecraft.client.gui.screens.Screen\n"));
        assert!(!text.contains("<init>"));
    }

    #[test]
    fn intermediary_names() {
        let tiny = "tiny\t2\t0\tofficial\tintermediary\n\
c\tevi\tnet/minecraft/class_310\n\
\tf\tLfiy;\tt\tfield_1724\n\
\tm\t()Levi;\tN\tmethod_1551\n\
\tm\t(Lexd;)V\ta\tmethod_1507\n";
        let text = compose(&parse_mojmap(MOJ), Some(&parse_tiny(tiny)), "intermediary");
        assert!(text.contains("C net.minecraft.client.Minecraft net.minecraft.class_310\n"));
        assert!(text.contains("F player field_1724\n"));
        assert!(text.contains("M getInstance method_1551 -\n"));
        assert!(text.contains("M setScreen method_1507 net.minecraft.client.gui.screens.Screen\n"));
    }

    #[test]
    fn srg_names() {
        let tsrg = "tsrg2 left right\nevi net/minecraft/client/Minecraft\n\tt f_91074_\n\tN ()Levi; m_91087_\n\t\tstatic\n\ta (Lexd;)V m_91152_\n";
        let text = compose(&parse_mojmap(MOJ), Some(&parse_tsrg(tsrg)), "srg");
        assert!(text.contains("C net.minecraft.client.Minecraft net.minecraft.client.Minecraft\n"));
        assert!(text.contains("M getInstance m_91087_ -\n"));
        assert!(text.contains("M setScreen m_91152_ net.minecraft.client.gui.screens.Screen\n"));
    }

    #[test]
    fn forge_runtime_names() {
        assert!(forge_srg_runtime(LoaderKind::Forge, "1.20.1"));
        assert!(forge_srg_runtime(LoaderKind::Forge, "1.16.5"));
        assert!(!forge_srg_runtime(LoaderKind::Forge, "1.20.6"));
        assert!(forge_srg_runtime(LoaderKind::NeoForge, "1.20.4"));
        assert!(!forge_srg_runtime(LoaderKind::NeoForge, "1.21.1"));
        assert!(unobfuscated("26.1"));
        assert!(!unobfuscated("1.21.11"));
    }
}
