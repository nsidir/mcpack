#!/usr/bin/env python3
"""Refuse to commit API keys (pre-commit hook; file paths are passed as arguments).

Catches CurseForge API keys (`$2a$10$` + 53 chars) and a filled-in `curseforge_api_key`
setting. A build-time key belongs in the MCPACK_CURSEFORGE_API_KEY environment variable
(or a CI secret), never in the repository.
"""

from __future__ import annotations

import re
import sys

PATTERNS = [
    (re.compile(rb"\$2[aby]\$\d{2}\$[A-Za-z0-9./]{53}"), "CurseForge API key"),
    (re.compile(rb"""curseforge_api_key\s*=\s*["'][^"'<\s]{8,}["']"""), "curseforge_api_key with a value"),
    (re.compile(rb"""MCPACK_CURSEFORGE_API_KEY\s*[=:]\s*["']?[^\s"'$<{]{8,}"""), "MCPACK_CURSEFORGE_API_KEY with a value"),
]


def main(paths: list[str]) -> int:
    bad = 0
    for path in paths:
        try:
            with open(path, "rb") as f:
                data = f.read()
        except OSError:
            continue
        for pat, what in PATTERNS:
            for m in pat.finditer(data):
                line = data.count(b"\n", 0, m.start()) + 1
                print(f"{path}:{line}: looks like a {what}; do not commit keys (use the environment or a CI secret)")
                bad += 1
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
