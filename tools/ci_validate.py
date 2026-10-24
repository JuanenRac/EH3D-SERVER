#!/usr/bin/env python3
# =============================================================================
# EH3D-SERVER - tools/ci_validate.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""The repository checks that CI runs besides cargo: manifest, versions, licence, changelog, the eight README files
and the words that must never appear in public files. Prints CI_VALIDATION=PASS or FAIL with the reasons."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REQUIRED = ["schema_version", "ecosystem", "name", "version", "role", "stack", "technologies", "deployment_target",
            "maturity", "family", "parent", "build", "notes", "native_version"]
LANGUAGES = {"README.md": "en", "README_spa.md": "es", "README_deu.md": "de", "README_fra.md": "fr",
             "README_ita.md": "it", "README_jpn.md": "ja", "README_zho.md": "zh", "README_por.md": "pt"}
FORBIDDEN = re.compile(r"\b(claude|anthropic|sonnet)\b", re.IGNORECASE)
TEXT_SUFFIXES = {".md", ".rs", ".toml", ".json", ".yml", ".yaml", ".py", ".txt", ".svg"}
SKIPPED = {"target", ".git", "local-data", "re"}


def headings(text: str) -> list[int]:
    """The level of every heading outside code blocks, in order: the structure all eight READMEs share."""
    levels, fenced = [], False
    for line in text.splitlines():
        if line.lstrip().startswith("```"):
            fenced = not fenced
        elif not fenced and re.match(r"#{1,6} ", line):
            levels.append(len(line) - len(line.lstrip("#")))
    return levels


def main() -> int:
    problems: list[str] = []
    manifest = json.loads((ROOT / "eh3d.project.json").read_text(encoding="utf-8"))
    for key in REQUIRED:
        if key not in manifest:
            problems.append(f"eh3d.project.json lacks '{key}'")
    version = manifest.get("version", "?")
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', cargo)
    if not match or match.group(1) != version:
        problems.append(f"Cargo.toml version {match.group(1) if match else '?'} differs from the manifest {version}")
    if manifest.get("name") != ROOT.name and ROOT.name.startswith("EH3D"):
        problems.append(f"the manifest name {manifest.get('name')} differs from the folder {ROOT.name}")
    if "GNU GENERAL PUBLIC LICENSE" not in (ROOT / "LICENSE").read_text(encoding="utf-8", errors="replace")[:200]:
        problems.append("LICENSE is not the GPL text")
    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    top = re.search(r"(?m)^## \[(\d[^\]]*)\]", changelog)
    if not top or top.group(1) != version:
        problems.append(f"the newest CHANGELOG entry ({top.group(1) if top else 'none'}) is not the version {version}")
    english = headings((ROOT / "README.md").read_text(encoding="utf-8"))
    for name in LANGUAGES:
        path = ROOT / name
        if not path.exists():
            problems.append(f"{name} is missing")
            continue
        text = path.read_text(encoding="utf-8")
        if headings(text) != english:
            problems.append(f"{name} does not have the heading structure of README.md")
        if f"`{version}`" not in text:
            problems.append(f"{name} does not show the version {version}")
        for other in LANGUAGES:
            if other != name and other not in text:
                problems.append(f"{name} does not link to {other} in its language bar")
    for path in ROOT.rglob("*"):
        if not path.is_file() or SKIPPED & set(path.relative_to(ROOT).parts) or path.suffix not in TEXT_SUFFIXES:
            continue
        if path.name == "ci_validate.py":
            continue
        if FORBIDDEN.search(path.read_text(encoding="utf-8", errors="replace")):
            problems.append(f"{path.relative_to(ROOT)} mentions a word that must not appear in public files")
    if problems:
        for problem in problems:
            print("  FAIL", problem)
        print(f"CI_VALIDATION=FAIL project={manifest.get('name')} version={version}")
        return 1
    print(f"CI_VALIDATION=PASS project={manifest.get('name')} version={version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
