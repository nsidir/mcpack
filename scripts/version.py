#!/usr/bin/env python3
"""Keep mcpack's version numbers in sync.

    python scripts/version.py check            # all files agree and are not behind the latest v* tag
    python scripts/version.py show             # print the current version
    python scripts/version.py bump patch|minor|major|X.Y.Z

The version lives in six places: the workspace Cargo.toml, the app's Cargo.toml, package.json,
tauri.conf.json, and the mcpack entries of both Cargo.lock files. `bump` rewrites all of them.
The agent mod (agent-mod/gradle.properties) is versioned separately and is not touched.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SEMVER = re.compile(r"^\d+\.\d+\.\d+$")


# (file, pattern with the version in group 2, label)
LOCATIONS: list[tuple[str, re.Pattern[str], str]] = [
    ("Cargo.toml", re.compile(r'(\[workspace\.package\][^\[]*?\nversion = ")([^"]+)(")', re.S), "workspace"),
    ("app/src-tauri/Cargo.toml", re.compile(r'(\[package\][^\[]*?\nversion = ")([^"]+)(")', re.S), "app crate"),
    ("app/package.json", re.compile(r'("version"\s*:\s*")([^"]+)(")'), "package.json"),
    ("app/src-tauri/tauri.conf.json", re.compile(r'("version"\s*:\s*")([^"]+)(")'), "tauri.conf.json"),
    ("Cargo.lock", re.compile(r'(name = "mcpack-core"\r?\nversion = ")([^"]+)(")'), "Cargo.lock mcpack-core"),
    ("Cargo.lock", re.compile(r'(name = "mcpack-mcp"\r?\nversion = ")([^"]+)(")'), "Cargo.lock mcpack-mcp"),
    ("app/src-tauri/Cargo.lock", re.compile(r'(name = "mcpack-app"\r?\nversion = ")([^"]+)(")'), "app Cargo.lock mcpack-app"),
    ("app/src-tauri/Cargo.lock", re.compile(r'(name = "mcpack-core"\r?\nversion = ")([^"]+)(")'), "app Cargo.lock mcpack-core"),
]


def read(rel: str) -> str:
    with open(ROOT / rel, encoding="utf-8", newline="") as f:
        return f.read()


def write(rel: str, text: str) -> None:
    with open(ROOT / rel, "w", encoding="utf-8", newline="") as f:
        f.write(text)


def versions() -> list[tuple[str, str, str | None]]:
    out = []
    for rel, pat, label in LOCATIONS:
        m = pat.search(read(rel))
        out.append((rel, label, m.group(2) if m else None))
    return out


def key(v: str) -> tuple[int, ...]:
    return tuple(int(p) for p in v.split("."))


def latest_tag() -> str | None:
    try:
        tags = subprocess.run(["git", "tag", "--list", "v*"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.split()
    except (OSError, subprocess.CalledProcessError):
        return None
    vs = [t[1:] for t in tags if SEMVER.match(t[1:])]
    return max(vs, key=key) if vs else None


def check() -> int:
    found = versions()
    problems = [f"  {label} ({rel}): version not found" for rel, label, v in found if v is None]
    values = {v for _, _, v in found if v}
    if len(values) > 1:
        problems.append("  versions disagree:")
        problems += [f"    {v:<10} {label} ({rel})" for rel, label, v in found if v]
    for v in values:
        if not SEMVER.match(v):
            problems.append(f"  '{v}' is not X.Y.Z")
    if not problems and values:
        (v,) = values
        tag = latest_tag()
        if tag and key(v) < key(tag):
            problems.append(f"  version {v} is behind the latest release tag v{tag}")
    if problems:
        print("version check failed:\n" + "\n".join(problems))
        print("fix with: python scripts/version.py bump X.Y.Z")
        return 1
    return 0


def bump(arg: str) -> int:
    found = versions()
    current = next((v for _, _, v in found if v), None)
    if current is None or not SEMVER.match(current):
        print(f"cannot read the current version ({current!r})")
        return 1
    major, minor, patch = key(current)
    new = {
        "major": f"{major + 1}.0.0",
        "minor": f"{major}.{minor + 1}.0",
        "patch": f"{major}.{minor}.{patch + 1}",
    }.get(arg, arg)
    if not SEMVER.match(new):
        print(f"'{arg}' is not patch|minor|major|X.Y.Z")
        return 1
    texts: dict[str, str] = {}
    for rel, pat, label in LOCATIONS:
        text = texts.get(rel) or read(rel)
        text, n = pat.subn(lambda m: m.group(1) + new + m.group(3), text, count=1)
        if n != 1:
            print(f"could not update {label} ({rel})")
            return 1
        texts[rel] = text
    for rel, text in texts.items():
        write(rel, text)
    print(f"{current} -> {new}")
    return check()


def main(argv: list[str]) -> int:
    if len(argv) >= 1 and argv[0] == "check":
        return check()
    if len(argv) >= 1 and argv[0] == "show":
        print(next((v for _, _, v in versions() if v), "?"))
        return 0
    if len(argv) == 2 and argv[0] == "bump":
        return bump(argv[1])
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
