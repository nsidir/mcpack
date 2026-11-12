//! Build the java command line from a version JSON.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::install::GameInstall;
use crate::meta::mojang::{rules_allow, Arg, Features};
use crate::meta::Platform;

#[derive(Debug, Clone)]
pub struct LaunchParams {
    pub username: String,
    pub uuid: String,
    pub game_dir: PathBuf,
    pub max_memory: Option<String>,
    pub min_memory: Option<String>,
    pub extra_jvm: Vec<String>,
    pub extra_game: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub quick_play_world: Option<String>,
}

pub struct Command {
    pub java: PathBuf,
    pub args: Vec<String>,
}

pub fn build(install: &GameInstall, params: &LaunchParams, p: &Platform) -> Command {
    let v = &install.version;
    let features = Features::default()
        .with("has_custom_resolution", params.width.is_some() && params.height.is_some())
        .with("is_quick_play_singleplayer", params.quick_play_world.is_some());

    let classpath = install
        .classpath
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(p.classpath_separator());

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("auth_player_name", params.username.clone());
    vars.insert("version_name", v.id.clone());
    vars.insert("game_directory", params.game_dir.display().to_string());
    vars.insert("assets_root", install.assets_root.display().to_string());
    vars.insert(
        "game_assets",
        install.assets_root.join("virtual").join(&install.assets_index).display().to_string(),
    );
    vars.insert("assets_index_name", install.assets_index.clone());
    vars.insert("auth_uuid", params.uuid.clone());
    vars.insert("auth_access_token", "0".into());
    vars.insert("auth_session", "0".into());
    vars.insert("clientid", "0".into());
    vars.insert("auth_xuid", "0".into());
    vars.insert("user_type", "legacy".into());
    vars.insert("user_properties", "{}".into());
    vars.insert("version_type", v.kind.clone().unwrap_or_else(|| "release".into()));
    vars.insert("natives_directory", install.natives_dir.display().to_string());
    vars.insert("launcher_name", "mcpack".into());
    vars.insert("launcher_version", env!("CARGO_PKG_VERSION").into());
    vars.insert("classpath", classpath.clone());
    vars.insert("classpath_separator", p.classpath_separator().into());
    vars.insert("library_directory", install.libraries_dir.display().to_string());
    if let (Some(w), Some(h)) = (params.width, params.height) {
        vars.insert("resolution_width", w.to_string());
        vars.insert("resolution_height", h.to_string());
    }
    if let Some(world) = &params.quick_play_world {
        vars.insert("quickPlaySingleplayer", world.clone());
    }

    let subst = |s: &str| -> String {
        let mut out = s.to_string();
        let mut from = 0;
        while let Some(rel) = out[from..].find("${") {
            let start = from + rel;
            let Some(end_rel) = out[start..].find('}') else { break };
            let end = start + end_rel;
            let key = out[start + 2..end].to_string();
            let val = vars.get(key.as_str()).cloned().unwrap_or_default();
            out.replace_range(start..=end, &val);
            from = start + val.len();
        }
        out
    };

    let mut jvm: Vec<String> = Vec::new();
    if let Some(m) = &params.max_memory {
        jvm.push(format!("-Xmx{}", normalize_mem(m)));
    }
    if let Some(m) = &params.min_memory {
        jvm.push(format!("-Xms{}", normalize_mem(m)));
    }
    let mut game: Vec<String> = Vec::new();

    match &v.arguments {
        Some(args) => {
            for a in &args.jvm {
                push_arg(&mut jvm, a, p, &features, &subst);
            }
            for a in &args.game {
                push_arg(&mut game, a, p, &features, &subst);
            }
        }
        None => {
            jvm.push(subst("-Djava.library.path=${natives_directory}"));
            jvm.push("-cp".into());
            jvm.push(classpath.clone());
            if let Some(legacy) = &v.minecraft_arguments {
                for part in legacy.split_whitespace() {
                    game.push(subst(part));
                }
            }
        }
    }
    // Drop flags whose placeholder values we left empty (quick play paths etc.).
    game = strip_unvalued(game);

    jvm.extend(params.extra_jvm.iter().cloned());
    let mut args = jvm;
    args.push(v.main_class.clone().unwrap_or_else(|| "net.minecraft.client.main.Main".into()));
    args.extend(game);
    args.extend(params.extra_game.iter().cloned());
    Command { java: install.java.clone(), args }
}

fn push_arg(out: &mut Vec<String>, a: &Arg, p: &Platform, features: &Features, subst: &dyn Fn(&str) -> String) {
    match a {
        Arg::Plain(s) => out.push(subst(s)),
        Arg::Ruled { rules, value } => {
            if rules_allow(Some(rules), p, features) {
                for s in value.values() {
                    out.push(subst(&s));
                }
            }
        }
    }
}

/// Remove `--flag` entries followed by an empty value (placeholders we did not fill).
fn strip_unvalued(game: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(game.len());
    let mut i = 0;
    while i < game.len() {
        let cur = &game[i];
        if cur.starts_with("--") {
            if let Some(next) = game.get(i + 1) {
                if next.is_empty() {
                    i += 2;
                    continue;
                }
            }
        }
        out.push(cur.clone());
        i += 1;
    }
    out
}

/// Accept `4G`, `4g`, `4096M`, `4096` (MB).
pub fn normalize_mem(m: &str) -> String {
    let t = m.trim();
    if t.chars().all(|c| c.is_ascii_digit()) {
        format!("{t}M")
    } else {
        t.to_ascii_uppercase().trim_end_matches('B').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem() {
        assert_eq!(normalize_mem("4g"), "4G");
        assert_eq!(normalize_mem("2048"), "2048M");
        assert_eq!(normalize_mem("512MB"), "512M");
    }

    #[test]
    fn strips_empty_flags() {
        let v = vec!["--a".into(), "1".into(), "--quickPlayPath".into(), "".into(), "--b".into(), "x".into()];
        assert_eq!(strip_unvalued(v), vec!["--a", "1", "--b", "x"]);
    }
}
