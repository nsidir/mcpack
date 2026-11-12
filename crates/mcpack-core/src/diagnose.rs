//! Turn logs and crash reports into structured, actionable problems.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Problem {
    /// Stable kind: missing_dependency, wrong_dependency_version, incompatible_mods, duplicate_mod,
    /// mixin_failure, out_of_memory, wrong_java, missing_class, graphics_driver, crash, ...
    pub kind: String,
    /// `error` | `warning`
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// Jar files involved (mods/ filenames), when known.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

impl Problem {
    pub fn error(kind: &str, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), severity: "error".into(), message: message.into(), mods: vec![], suggestion: None, evidence: vec![], files: vec![] }
    }
    pub fn warning(kind: &str, message: impl Into<String>) -> Self {
        Self { severity: "warning".into(), ..Self::error(kind, message) }
    }
    pub fn mods(mut self, m: Vec<String>) -> Self {
        self.mods = m;
        self
    }
    pub fn suggest(mut self, s: impl Into<String>) -> Self {
        self.suggestion = Some(s.into());
        self
    }
    pub fn files(mut self, f: Vec<String>) -> Self {
        self.files = f;
        self
    }
    pub fn evidence(mut self, e: impl Into<String>) -> Self {
        self.evidence.push(e.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Diagnosis {
    pub summary: String,
    pub problems: Vec<Problem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash_report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash_description: Option<String>,
    /// Fabric loader's own suggested fixes ("Install X", "Replace Y").
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub loader_suggestions: Vec<String>,
    /// Most relevant log/stack lines.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excerpt: Vec<String>,
}

struct Patterns {
    requires: Regex,
    incompatible: Regex,
    duplicate: Regex,
    mixin_mod: Regex,
    mixin_cfg: Regex,
    class_version: Regex,
    missing_class: Regex,
    crash_desc: Regex,
    suspected: Regex,
    solution: Regex,
    exception: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        requires: Regex::new(
            r"Mod '(?P<name>[^']+)' \((?P<id>[^)]+)\) (?P<ver>\S+) requires (?P<req>.+?) of (?:mod )?'(?P<dep>[^']+)'(?: \((?P<depid>[^)]+)\))?,? (?P<rest>.*)$",
        )
        .unwrap(),
        incompatible: Regex::new(
            r"Mod '(?P<name>[^']+)' \((?P<id>[^)]+)\) \S+ is incompatible with (?P<req>.+?) of (?:mod )?'(?P<dep>[^']+)'(?: \((?P<depid>[^)]+)\))?",
        )
        .unwrap(),
        duplicate: Regex::new(r"(?i)(?:mod id '?([a-z0-9_\-]+)'? is (?:duplicated|provided by more than one)|duplicate mods? (?:found|for)[^']*'([a-z0-9_\-]+)')").unwrap(),
        mixin_mod: Regex::new(r"(?i)mixin apply for mod ([a-z0-9_\-]+) failed|from mod ([a-z0-9_\-]+)").unwrap(),
        mixin_cfg: Regex::new(r"([a-z0-9_\-]+)\.mixins?\.json").unwrap(),
        class_version: Regex::new(r"class file version (\d+)\.\d+\), this version of the Java Runtime only recognizes class file versions up to (\d+)").unwrap(),
        missing_class: Regex::new(r"(?:NoClassDefFoundError|ClassNotFoundException):? ([A-Za-z0-9_$./]+)").unwrap(),
        crash_desc: Regex::new(r"^Description: (.+)$").unwrap(),
        suspected: Regex::new(r"Suspected Mods?: (.+)$").unwrap(),
        solution: Regex::new(r"^\s*-\s*(Install|Replace|Remove|Upgrade|Downgrade) .+").unwrap(),
        exception: Regex::new(r"^(?:Caused by: |Exception in thread .*? )?([a-zA-Z_$][\w$.]*(?:Exception|Error))(?::|$)").unwrap(),
    })
}

/// Map common API packages to the mod that provides them.
fn mod_for_class(class: &str) -> Option<&'static str> {
    let c = class.replace('/', ".");
    const MAP: &[(&str, &str)] = &[
        ("net.fabricmc.fabric.", "fabric-api"),
        ("org.quiltmc.qsl.", "qsl"),
        ("me.shedaniel.clothconfig", "cloth-config"),
        ("me.shedaniel.autoconfig", "cloth-config"),
        ("dev.architectury.", "architectury-api"),
        ("com.terraformersmc.modmenu.", "modmenu"),
        ("software.bernie.geckolib", "geckolib"),
        ("dev.isxander.yacl", "yacl"),
        ("net.caffeinemc.mods.sodium", "sodium"),
        ("me.jellysquid.mods.sodium", "sodium"),
        ("net.irisshaders.", "iris"),
        ("org.ladysnake.cca", "cardinal-components-api"),
        ("dev.onyxstudios.cca", "cardinal-components-api"),
        ("fuzs.puzzleslib", "puzzles-lib"),
        ("com.teamresourceful.resourcefullib", "resourceful-lib"),
        ("snownee.kiwi", "kiwi"),
        ("vazkii.patchouli", "patchouli"),
        ("mod.azure.azurelib", "azurelib"),
        ("eu.midnightdust.lib", "midnightlib"),
    ];
    MAP.iter().find(|(p, _)| c.starts_with(p)).map(|(_, m)| *m)
}

/// Analyze log lines plus an optional crash report.
pub fn analyze(log: &[String], crash: Option<(&Path, &str)>, exit_code: Option<i32>) -> Diagnosis {
    let p = patterns();
    let mut d = Diagnosis::default();
    let push = |d: &mut Diagnosis, pr: Problem| {
        if !d.problems.iter().any(|x| x.kind == pr.kind && x.message == pr.message) {
            d.problems.push(pr);
        }
    };

    let crash_lines: Vec<String> = crash.map(|(_, t)| t.lines().map(str::to_string).collect()).unwrap_or_default();
    if let Some((path, _)) = crash {
        d.crash_report = Some(path.display().to_string());
    }
    let all: Vec<&String> = log.iter().chain(crash_lines.iter()).collect();

    for line in &all {
        let l = line.as_str();
        if let Some(c) = p.requires.captures(l) {
            let rest = c.name("rest").map(|m| m.as_str()).unwrap_or("");
            let dep = c.name("depid").or(c.name("dep")).map(|m| m.as_str().to_string()).unwrap_or_default();
            let modid = c["id"].to_string();
            let (kind, msg, sugg) = if rest.contains("missing") {
                (
                    "missing_dependency",
                    format!("{} ({modid}) requires {} of '{}', which is missing", &c["name"], &c["req"], &c["dep"]),
                    format!("add '{dep}' to the pack (pack_add id=\"{dep}\"; use search_mods if the id differs on Modrinth)"),
                )
            } else {
                (
                    "wrong_dependency_version",
                    format!("{} ({modid}) requires {} of '{}' — {}", &c["name"], &c["req"], &c["dep"], rest.trim_end_matches('!')),
                    if dep == "minecraft" {
                        format!("'{modid}' targets a different Minecraft version; pin a compatible version or remove it")
                    } else {
                        format!("change the version of '{dep}' or '{modid}' (mod_versions lists candidates)")
                    },
                )
            };
            push(&mut d, Problem::error(kind, msg).mods(vec![modid, dep]).suggest(sugg).evidence(l.trim()));
        } else if let Some(c) = p.incompatible.captures(l) {
            let modid = c["id"].to_string();
            let dep = c.name("depid").or(c.name("dep")).map(|m| m.as_str().to_string()).unwrap_or_default();
            push(
                &mut d,
                Problem::error("incompatible_mods", format!("{} ({modid}) is incompatible with '{}'", &c["name"], &c["dep"]))
                    .mods(vec![modid.clone(), dep.clone()])
                    .suggest(format!("remove '{modid}' or '{dep}' (or change versions)"))
                    .evidence(l.trim()),
            );
        }
        if let Some(c) = p.duplicate.captures(l) {
            let id = c.get(1).or(c.get(2)).map(|m| m.as_str().to_string()).unwrap_or_default();
            push(
                &mut d,
                Problem::error("duplicate_mod", format!("mod '{id}' is present more than once"))
                    .mods(vec![id.clone()])
                    .suggest("remove the duplicate jar (check overrides/mods and manual files)")
                    .evidence(l.trim()),
            );
        }
        if l.contains("Mixin apply") && l.contains("failed")
            || l.contains("MixinApplyError")
            || l.contains("InvalidMixinException")
            || l.contains("MixinTransformerError")
        {
            let m = p
                .mixin_mod
                .captures(l)
                .and_then(|c| c.get(1).or(c.get(2)).map(|m| m.as_str().to_string()))
                .or_else(|| p.mixin_cfg.captures(l).map(|c| c[1].to_string()));
            let mods = m.clone().into_iter().collect();
            push(
                &mut d,
                Problem::error(
                    "mixin_failure",
                    format!("mixin failed to apply{}", m.as_ref().map(|m| format!(" (mod '{m}')")).unwrap_or_default()),
                )
                .mods(mods)
                .suggest("usually a mod conflict or wrong mod version for this Minecraft version; try removing/updating the named mod")
                .evidence(l.trim()),
            );
        }
        if l.contains("java.lang.OutOfMemoryError") {
            push(
                &mut d,
                Problem::error("out_of_memory", "the JVM ran out of memory")
                    .suggest("raise jvm.memory (pack_set path=\"jvm.memory\" value=\"6G\")")
                    .evidence(l.trim()),
            );
        }
        if l.contains("UnsupportedClassVersionError") || l.contains("compiled by a more recent version of the Java Runtime") {
            let need = p
                .class_version
                .captures(l)
                .and_then(|c| c[1].parse::<u32>().ok())
                .map(|v| v.saturating_sub(44));
            push(
                &mut d,
                Problem::error(
                    "wrong_java",
                    format!("a class needs a newer Java{}", need.map(|n| format!(" (Java {n})")).unwrap_or_default()),
                )
                .suggest("a mod targets a newer Minecraft/Java; pin an older version of it, or set jvm.java to a newer Java")
                .evidence(l.trim()),
            );
        }
        if let Some(c) = p.missing_class.captures(l) {
            let class = c[1].to_string();
            if !class.starts_with("net.minecraft") || class.contains("$") {
                let owner = mod_for_class(&class);
                push(
                    &mut d,
                    Problem::error("missing_class", format!("class {class} not found"))
                        .mods(owner.map(|o| vec![o.to_string()]).unwrap_or_default())
                        .suggest(match owner {
                            Some(o) => format!("probably missing dependency '{o}' — pack_add id=\"{o}\""),
                            None => "a dependency is missing or a mod is built for another Minecraft version".to_string(),
                        })
                        .evidence(l.trim()),
                );
            }
        }
        if l.contains("WGL: The driver does not appear to support OpenGL")
            || l.contains("GLFW error 65542")
            || l.contains("GLFW error 65543")
            || l.contains("Pixel format not accelerated")
        {
            push(
                &mut d,
                Problem::error("graphics_driver", "OpenGL is not available (graphics driver issue)")
                    .suggest("update GPU drivers; on laptops make sure java uses the discrete GPU")
                    .evidence(l.trim()),
            );
        }
        if p.solution.is_match(l) {
            let s = l.trim().trim_start_matches('-').trim().to_string();
            if !d.loader_suggestions.contains(&s) {
                d.loader_suggestions.push(s);
            }
        }
    }

    // Crash report specifics.
    for l in &crash_lines {
        if d.crash_description.is_none() {
            if let Some(c) = p.crash_desc.captures(l) {
                d.crash_description = Some(c[1].to_string());
            }
        }
        if let Some(c) = p.suspected.captures(l) {
            // Format: "Suspected Mod(s): Name (modid), Version: x, Name2 (id2), Version: y"
            let id_re = Regex::new(r"\(([A-Za-z0-9_\-.]+)\)").expect("regex");
            let mut mods: Vec<String> = id_re.captures_iter(&c[1]).map(|m| m[1].to_string()).collect();
            mods.dedup();
            if !mods.is_empty() {
                push(
                    &mut d,
                    Problem::error("suspected_mods", format!("crash report suspects: {}", mods.join(", ")))
                        .mods(mods)
                        .suggest("try removing or updating the suspected mod(s), then smoke_test again")
                        .evidence(l.trim()),
                );
            }
        }
    }

    // Excerpt: exception lines.
    let mut excerpt = Vec::new();
    for l in all.iter().rev() {
        if p.exception.is_match(l.trim()) || l.contains("[main/ERROR]") || l.contains("/FATAL]") {
            excerpt.push(l.trim().to_string());
            if excerpt.len() >= 15 {
                break;
            }
        }
    }
    excerpt.reverse();
    d.excerpt = excerpt;

    if d.problems.is_empty() {
        let crashed = crash.is_some() || exit_code.is_some_and(|c| c != 0);
        if crashed {
            let msg = d
                .crash_description
                .clone()
                .or_else(|| d.excerpt.first().cloned())
                .unwrap_or_else(|| format!("game exited with code {exit_code:?}"));
            d.problems.push(Problem::error("crash", msg).suggest("inspect `excerpt` and the crash report; bisect recently added mods"));
        }
    }
    d.summary = if d.problems.is_empty() {
        "no problems detected".to_string()
    } else {
        let errors = d.problems.iter().filter(|p| p.severity == "error").count();
        format!("{errors} error(s): {}", d.problems.iter().map(|p| p.kind.as_str()).collect::<Vec<_>>().join(", "))
    };
    d
}

/// Newest crash report in the instance (optionally only those newer than `since_ms`).
pub fn newest_crash_report(instance: &Path, since_ms: Option<u64>) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let dirs = [instance.join("crash-reports"), instance.to_path_buf()];
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let is_report = (dir.ends_with("crash-reports") && name.ends_with(".txt")) || (name.starts_with("hs_err_pid") && name.ends_with(".log"));
            if !is_report {
                continue;
            }
            let Ok(modified) = e.metadata().and_then(|m| m.modified()) else { continue };
            if let Some(since) = since_ms {
                let ms = modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
                if ms + 2000 < since {
                    continue;
                }
            }
            if best.as_ref().is_none_or(|(t, _)| modified > *t) {
                best = Some((modified, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Read `logs/latest.log` lines from an instance.
pub fn latest_log(instance: &Path) -> Vec<String> {
    std::fs::read_to_string(instance.join("logs").join("latest.log"))
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fabric_missing_dep() {
        let log = vec![
            "net.fabricmc.loader.impl.FormattedException: Mod resolution encountered an incompatible mod set!".to_string(),
            "A potential solution has been determined:".to_string(),
            "\t - Install fabric-api, any version.".to_string(),
            "Unmet dependency listing:".to_string(),
            "\t - Mod 'Sodium Extra' (sodium-extra) 0.5.4 requires any version of mod 'fabric-api', which is missing!".to_string(),
            "\t - Mod 'Iris' (iris) 1.7.0 requires version 1.21.1 of 'Minecraft' (minecraft), but only the wrong version is present: 1.20.1!".to_string(),
        ];
        let d = analyze(&log, None, Some(1));
        assert!(d.problems.iter().any(|p| p.kind == "missing_dependency" && p.mods.contains(&"fabric-api".to_string())));
        assert!(d.problems.iter().any(|p| p.kind == "wrong_dependency_version" && p.mods.contains(&"iris".to_string())));
        assert_eq!(d.loader_suggestions, vec!["Install fabric-api, any version."]);
    }

    #[test]
    fn crash_report_and_classes() {
        let crash = "---- Minecraft Crash Report ----\nDescription: Initializing game\n\njava.lang.NoClassDefFoundError: me/shedaniel/clothconfig2/api/ConfigBuilder\nSuspected Mods: Cool Mod (coolmod), Version: 1.0\n";
        let d = analyze(&[], Some((Path::new("crash.txt"), crash)), Some(-1));
        assert_eq!(d.crash_description.as_deref(), Some("Initializing game"));
        assert!(d.problems.iter().any(|p| p.kind == "missing_class" && p.mods == vec!["cloth-config"]));
        assert!(d.problems.iter().any(|p| p.kind == "suspected_mods" && p.mods == vec!["coolmod"]));
    }

    #[test]
    fn clean_log_has_no_problems() {
        let d = analyze(&["[Render thread/INFO]: Sound engine started".to_string()], None, Some(0));
        assert!(d.problems.is_empty());
    }
}
