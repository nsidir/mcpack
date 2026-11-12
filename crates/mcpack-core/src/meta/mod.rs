//! Mojang / loader metadata models and rule evaluation.

pub mod java;
pub mod loader;
pub mod mojang;

/// Platform facts used for library/argument rule evaluation.
#[derive(Debug, Clone)]
pub struct Platform {
    /// Mojang OS name: `windows`, `osx`, `linux`.
    pub os: &'static str,
    /// `x86_64`, `x86`, `aarch64`...
    pub arch: &'static str,
}

impl Platform {
    pub fn current() -> Self {
        let os = match std::env::consts::OS {
            "windows" => "windows",
            "macos" => "osx",
            _ => "linux",
        };
        Self { os, arch: std::env::consts::ARCH }
    }

    pub fn is_64bit(&self) -> bool {
        matches!(self.arch, "x86_64" | "aarch64")
    }

    /// Key used by Mojang's java-runtime manifest.
    pub fn java_runtime_key(&self) -> &'static str {
        match (self.os, self.arch) {
            ("windows", "x86") => "windows-x86",
            ("windows", "aarch64") => "windows-arm64",
            ("windows", _) => "windows-x64",
            ("osx", "aarch64") => "mac-os-arm64",
            ("osx", _) => "mac-os",
            ("linux", "x86") => "linux-i386",
            _ => "linux",
        }
    }

    pub fn classpath_separator(&self) -> &'static str {
        if self.os == "windows" {
            ";"
        } else {
            ":"
        }
    }
}
