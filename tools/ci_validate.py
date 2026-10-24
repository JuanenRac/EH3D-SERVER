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



BUILD_FOLDERS = {".compile", "build", "dist", "target", "node_modules", ".venv", "venv", "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache",
                 "managed_components", ".gradle", ".cxx", ".idea", ".vscode"}
PRIVATE_FOLDERS = {"local-data", "re", "secrets"}
PRIVATE_FILES = re.compile(r"(^|/)(\.env(\.(?!example$).*)?|id_[a-z0-9]+|\.coverage|sdkconfig(\.old)?|Thumbs\.db|\.DS_Store)$|\.(pem|key|p12|pfx|jks|keystore|log|tmp|pyc)$", re.IGNORECASE)
MAX_TRACKED_BYTES = 1_500_000


def hygiene_problems() -> list[str]:
    """Build folders, private data, keys, logs and big files must never be tracked by git."""
    import subprocess

    try:
        out = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return []
    found: list[str] = []
    folders: dict[str, int] = {}
    for raw in out.split(b"\0"):
        if not raw:
            continue
        name = raw.decode("utf-8", "replace")
        parts = name.split("/")
        bad = next((p for p in parts[:-1] if p in BUILD_FOLDERS), None) or (parts[0] if len(parts) > 1 and parts[0] in PRIVATE_FOLDERS else None)
        if bad:
            folders[bad] = folders.get(bad, 0) + 1
            continue
        if PRIVATE_FILES.search(name):
            found.append(f"{name} is a private or local file and must not be tracked")
        path = ROOT / name
        if path.is_file() and path.stat().st_size > MAX_TRACKED_BYTES and path.suffix.lower() not in {".png", ".jpg", ".svg", ".pdf", ".stl", ".step"}:
            found.append(f"{name} is bigger than {MAX_TRACKED_BYTES // 1000} KB: build output does not belong in the repository")
    for folder, count in sorted(folders.items()):
        found.append(f"{folder}/ is a build or private folder and {count} of its files are tracked")
    return found

STATUS_LIGHTS = ("🟢", "🟡", "🟠", "🔴")


def status_lights(text: str) -> list[str] | None:
    """The lights of the "Project status" section of a README, in order, or None when the section is missing or malformed.

    The section is the second `## ` heading (right after "At a glance"), has the legend with the four lights and a
    table whose State column holds only a light."""
    lines, fenced, heads = text.splitlines(), False, []
    for index, line in enumerate(lines):
        if line.lstrip().startswith("```"):
            fenced = not fenced
        elif not fenced and line.startswith("## "):
            heads.append(index)
    if len(heads) < 3 or not lines[heads[1]].startswith("## 🚦 "):
        return None
    block = lines[heads[1]:heads[2]]
    if not all(light in " ".join(block) for light in STATUS_LIGHTS):
        return None
    rows = [line for line in block if line.startswith("|")][2:]
    lights = []
    for row in rows:
        cells = [part.strip() for part in re.split(r"(?<!\\)\|", row.strip().strip("|"))]
        if len(cells) != 3 or cells[1] not in STATUS_LIGHTS or not cells[0] or not cells[2]:
            return None
        lights.append(cells[1])
    return lights if len(lights) >= 3 else None


TREE_EXEMPT = re.compile(
    r"^(README(_[a-z]{3})?\.md|LICENSE(\.md)?|CHANGELOG\.md|CODE_OF_CONDUCT\.md|CONTRIBUTING\.md|SECURITY\.md|SUPPORT\.md|VERSION"
    r"|eh3d\.project\.json|images|\..*|build.*|run.*|dev.*|bump.*|.*\.lock|Cargo\.toml|package(-lock)?\.json|pyproject\.toml|.*\.toml)$"
)
TREE_ROW = re.compile(r"^[├└]── ([^\s,]+(?:(?:, | / | \+ )[^\s,]+)*)")


def tree_names(text: str) -> set[str] | None:
    """The first-level names that the file tree of a README lists (the code block with the most tree rows), or None without a tree."""
    best, best_rows = "", 0
    for block in re.finditer(r"```[a-z]*\r?\n(.*?)```", text, re.S):
        rows = len(re.findall(r"^[├└]── ", block.group(1), re.M))
        if rows > best_rows:
            best, best_rows = block.group(1), rows
    if not best_rows:
        return None
    names: set[str] = set()
    for line in best.splitlines():
        found = TREE_ROW.match(line)
        if not found:
            continue
        previous = ""
        for token in re.split(r", | / | \+ ", found.group(1)):
            for part in re.split(r"/(?=\.)", token):
                part = part.rstrip("/")
                if part.startswith(".") and "." in previous.lstrip("."):
                    part = previous.rsplit(".", 1)[0] + part
                if part.split("/")[0]:
                    names.add(part.split("/")[0])
                previous = part
    return names


def tree_problems(tracked: list[str] | None = None) -> list[str]:
    """Every versioned first-level entry must appear in the file tree of each README (the structure that the READMEs describe must not go stale)."""
    import fnmatch
    import subprocess

    if tracked is None:
        try:
            out = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
        except (OSError, subprocess.CalledProcessError):
            return []
        tracked = sorted({item.split("/")[0] for item in out.decode("utf-8", "replace").split(chr(0)) if item})
    problems = []
    for name in LANGUAGES:
        path = ROOT / name
        if not path.exists():
            continue
        listed = tree_names(path.read_text(encoding="utf-8"))
        if listed is None:
            continue
        missing = [entry for entry in tracked if not TREE_EXEMPT.match(entry) and not any(fnmatch.fnmatchcase(entry, pattern) for pattern in listed)]
        if missing:
            problems.append(f"{name} file tree does not list: {', '.join(missing)}")
    return problems


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
    english_lights = status_lights((ROOT / "README.md").read_text(encoding="utf-8"))
    for name in LANGUAGES:
        path = ROOT / name
        if not path.exists():
            problems.append(f"{name} is missing")
            continue
        text = path.read_text(encoding="utf-8")
        if headings(text) != english:
            problems.append(f"{name} does not have the heading structure of README.md")
        lights = status_lights(text)
        if lights is None:
            problems.append(f"{name} has no valid 'Project status' section right after 'At a glance' (a table of lights, one row per part)")
        elif lights != english_lights:
            problems.append(f"{name} does not show the same lights as README.md in its 'Project status' table")
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
    problems.extend(hygiene_problems())
    problems.extend(tree_problems())
    if problems:
        for problem in problems:
            print("  FAIL", problem)
        print(f"CI_VALIDATION=FAIL project={manifest.get('name')} version={version}")
        return 1
    print(f"CI_VALIDATION=PASS project={manifest.get('name')} version={version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
