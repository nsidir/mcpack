//! Minecraft 1.7.10 – 1.12.2: the agent's legacy code is written with MCP names
//! (`getMinecraft`). Forge runs the game with SRG member names (`func_71410_x`) and MCP class
//! names; vanilla runs it obfuscated (`bib.z`). The mappings come from MCP itself: `joined.srg`
//! (obfuscated ↔ SRG, with descriptors) plus a stable `methods.csv` / `fields.csv` (SRG → MCP).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::AgentSupport;
use crate::error::{Error, IoContext, Result};
use crate::http::DownloadItem;
use crate::install::GameInstall;
use crate::meta::loader::FORGE_MAVEN;
use crate::util;
use crate::Ctx;

/// MCP stable mappings release for a Minecraft version (SRG names are stable, so neighbouring
/// versions share one).
fn mcp_stable(mc: &str) -> Option<&'static str> {
    Some(match mc {
        "1.7.2" | "1.7.10" => "12-1.7.10",
        "1.8" => "18-1.8",
        "1.8.8" => "20-1.8.8",
        "1.8.9" => "22-1.8.9",
        "1.9" => "24-1.9",
        "1.9.4" => "26-1.9.4",
        "1.10" | "1.10.2" => "29-1.10.2",
        "1.11" | "1.11.2" => "32-1.11",
        "1.12" | "1.12.1" | "1.12.2" => "39-1.12",
        _ => return None,
    })
}

async fn fetch(ctx: &Ctx, coord: &str) -> Result<PathBuf> {
    let rel = util::maven_path(coord)?;
    let dest = ctx.paths.libraries().join(&rel);
    ctx.http.download(&DownloadItem { url: format!("{FORGE_MAVEN}{rel}"), dest: dest.clone(), sha1: None, size: None }).await?;
    Ok(dest)
}

fn zip_text(path: &Path, name: &str) -> Result<String> {
    let f = std::fs::File::open(path).ctx(path)?;
    let mut zip = zip::ZipArchive::new(f)?;
    let mut s = String::new();
    zip.by_name(name)
        .map_err(|_| Error::Agent(format!("{} has no {name}", path.display())))?
        .read_to_string(&mut s)
        .ctx(path)?;
    Ok(s)
}

pub async fn prepare(ctx: &Ctx, inst: &GameInstall) -> Result<AgentSupport> {
    let mc = inst.minecraft.as_str();
    let Some(stable) = mcp_stable(mc) else {
        return Ok(AgentSupport::Unsupported(format!(
            "the in-game agent supports Minecraft 1.7.10 – 1.12.2 and 1.14.4+, not {mc}"
        )));
    };
    // Forge deobfuscates to SRG names at runtime; vanilla stays obfuscated.
    let obf = inst.loader != crate::meta::loader::LoaderKind::Forge;
    let out = ctx.paths.meta().join("agent-mappings").join(format!("{mc}-mcp-{stable}{}.txt", if obf { "-obf" } else { "" }));
    if out.exists() {
        return Ok(AgentSupport::Mapped(out));
    }
    let srg_zip = fetch(ctx, &format!("de.oceanlabs.mcp:mcp:{mc}:srg@zip")).await?;
    let csv_zip = fetch(ctx, &format!("de.oceanlabs.mcp:mcp_stable:{stable}@zip")).await?;
    let joined = zip_text(&srg_zip, "joined.srg")?;
    let names = parse_csv(&zip_text(&csv_zip, "methods.csv")?)
        .into_iter()
        .chain(parse_csv(&zip_text(&csv_zip, "fields.csv")?))
        .collect::<HashMap<_, _>>();
    util::write_atomic(&out, compose(&joined, &names, obf).as_bytes())?;
    Ok(AgentSupport::Mapped(out))
}

/// `searge,name,...` rows → searge → name.
fn parse_csv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let mut it = l.splitn(3, ',');
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect()
}

/// JVM descriptor parameter list → Java type names (`(ILjava/lang/String;[B)V` → `int,java.lang.String,byte[]`).
fn params_of(desc: &str) -> Option<Vec<String>> {
    let inner = desc.strip_prefix('(')?.split(')').next()?;
    let mut out = Vec::new();
    let mut chars = inner.chars().peekable();
    while chars.peek().is_some() {
        let mut dims = 0;
        while chars.peek() == Some(&'[') {
            chars.next();
            dims += 1;
        }
        let base = match chars.next()? {
            'Z' => "boolean".to_string(),
            'B' => "byte".to_string(),
            'C' => "char".to_string(),
            'S' => "short".to_string(),
            'I' => "int".to_string(),
            'J' => "long".to_string(),
            'F' => "float".to_string(),
            'D' => "double".to_string(),
            'L' => {
                let mut name = String::new();
                for c in chars.by_ref() {
                    if c == ';' {
                        break;
                    }
                    name.push(c);
                }
                name.replace('/', ".")
            }
            _ => return None,
        };
        out.push(format!("{base}{}", "[]".repeat(dims)));
    }
    Some(out)
}

/// Build the agent mappings (MCP name → runtime name, grouped by class) from `joined.srg`. The
/// runtime names are SRG (Forge) or obfuscated (`obf`, vanilla).
fn compose(joined: &str, names: &HashMap<String, String>, obf: bool) -> String {
    // SRG class → obfuscated class.
    let mut class_obf: HashMap<String, String> = HashMap::new();
    // SRG owner → member lines.
    let mut by_owner: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    let tail = |s: &str| s.rsplit_once('/').map(|(_, n)| n.to_string()).unwrap_or_else(|| s.to_string());
    for line in joined.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        match cols.as_slice() {
            ["CL:", notch, srg] => {
                class_obf.insert(srg.replace('/', "."), notch.replace('/', "."));
            }
            ["FD:", notch, srg] => {
                let Some((owner, field)) = srg.rsplit_once('/') else { continue };
                let runtime = if obf { tail(notch) } else { field.to_string() };
                let named = names.get(field).map(String::as_str).unwrap_or(field);
                if named != runtime {
                    by_owner.entry(owner.replace('/', ".")).or_default().push(format!("F {named} {runtime}"));
                }
            }
            ["MD:", notch, _, srg, srg_desc] => {
                let Some((owner, method)) = srg.rsplit_once('/') else { continue };
                let runtime = if obf { tail(notch) } else { method.to_string() };
                let named = names.get(method).map(String::as_str).unwrap_or(method);
                if named == runtime {
                    continue;
                }
                let Some(params) = params_of(srg_desc) else { continue };
                let ps = if params.is_empty() { "-".to_string() } else { params.join(",") };
                by_owner.entry(owner.replace('/', ".")).or_default().push(format!("M {named} {runtime} {ps}"));
            }
            _ => {}
        }
    }
    let mut out = String::from("mcpack-mappings 1 mcp\n");
    let mut owners: std::collections::BTreeSet<String> = by_owner.keys().cloned().collect();
    if obf {
        owners.extend(class_obf.keys().cloned());
    }
    for owner in owners {
        let runtime = if obf { class_obf.get(&owner).cloned().unwrap_or_else(|| owner.clone()) } else { owner.clone() };
        let _ = writeln!(out, "C {owner} {runtime}");
        for l in by_owner.remove(&owner).unwrap_or_default() {
            out.push_str(&l);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_names() {
        let joined = "CL: bib net/minecraft/client/Minecraft\n\
FD: bib/R net/minecraft/client/Minecraft/field_71439_g\n\
MD: bib/z ()Lbib; net/minecraft/client/Minecraft/func_71410_x ()Lnet/minecraft/client/Minecraft;\n\
MD: bib/a (Ljava/lang/String;I[B)V net/minecraft/client/Minecraft/func_1_a (Ljava/lang/String;I[B)V\n";
        let names = HashMap::from([
            ("func_71410_x".to_string(), "getMinecraft".to_string()),
            ("field_71439_g".to_string(), "player".to_string()),
            ("func_1_a".to_string(), "doThing".to_string()),
        ]);
        let text = compose(joined, &names, false);
        assert!(text.starts_with("mcpack-mappings 1 mcp\n"));
        assert!(text.contains("C net.minecraft.client.Minecraft net.minecraft.client.Minecraft\n"));
        assert!(text.contains("F player field_71439_g\n"));
        assert!(text.contains("M getMinecraft func_71410_x -\n"));
        assert!(text.contains("M doThing func_1_a java.lang.String,int,byte[]\n"));
        assert_eq!(parse_csv("searge,name,side,desc\nfunc_1,a,0,\"x, y\"\n"), vec![("func_1".to_string(), "a".to_string())]);
        assert_eq!(mcp_stable("1.12.2"), Some("39-1.12"));
        assert_eq!(mcp_stable("1.13.2"), None);
        let vanilla = compose(joined, &names, true);
        assert!(vanilla.contains("C net.minecraft.client.Minecraft bib
"));
        assert!(vanilla.contains("F player R
"));
        assert!(vanilla.contains("M getMinecraft z -
"));
    }
}
