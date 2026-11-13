#!/usr/bin/env python3
# =============================================================================
# EH3D-SERVER - tools/changelog_pr_check.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
# canonical: EH3D-DOCS tools 2
"""Checks, for a pull request, that the CHANGELOG gets a line under "## [Unreleased]" when the code changes.

    python tools/changelog_pr_check.py --base origin/main      # compares HEAD with the merge base of origin/main
    python tools/changelog_pr_check.py --base origin/main --warn-only

The change counts as code when it touches a file under src/, crates/, main/, app/ or tools/ (the folders can be listed in tools/ci_config.json under "changelog_paths").
Then "## [Unreleased]" must hold an entry that the base does not have (a new one or an edited one). A release that moves the entries to a version heading is not a missing line:
a change that adds a version heading to CHANGELOG.md passes (an empty "[Unreleased]" heading does not count).

Exit codes: 0 pass, 1 a line is missing, 64 wrong use. Standard library only; needs the history of the base (checkout with fetch-depth 0)."""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_PATHS = ("src/", "crates/", "main/", "app/", "tools/")
HEADING = re.compile(r"(?m)^## \[(?P<version>[^\]\n]+)\]")
ENTRY = re.compile(r"(?m)^[-*] ")


def git(*args: str, cwd: Path = ROOT) -> str:
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, encoding="utf-8", check=True).stdout


def unreleased_entries(text: str) -> int:
    """The number of bullets under the [Unreleased] heading (0 when the heading does not exist)."""
    text = text.replace("\r\n", "\n")
    heads = list(HEADING.finditer(text))
    for i, head in enumerate(heads):
        if head.group("version").lower() == "unreleased":
            end = heads[i + 1].start() if i + 1 < len(heads) else len(text)
            return len(ENTRY.findall(text[head.end():end]))
    return 0


def unreleased_lines(text: str) -> set[str]:
    """The text of the bullets under the [Unreleased] heading: a line that is new or edited is not in the set of the base."""
    text = text.replace("\r\n", "\n")
    heads = list(HEADING.finditer(text))
    for i, head in enumerate(heads):
        if head.group("version").lower() == "unreleased":
            end = heads[i + 1].start() if i + 1 < len(heads) else len(text)
            return {line.strip() for line in text[head.end():end].splitlines() if ENTRY.match(line)}
    return set()


def versions(text: str) -> set[str]:
    return {m.group("version") for m in HEADING.finditer(text.replace("\r\n", "\n"))}


def watched_paths(root: Path) -> tuple[str, ...]:
    config = root / "tools" / "ci_config.json"
    if config.is_file():
        try:
            listed = json.loads(config.read_text(encoding="utf-8")).get("changelog_paths")
        except ValueError:
            listed = None
        if isinstance(listed, list) and all(isinstance(item, str) for item in listed):
            return tuple(listed)
    return DEFAULT_PATHS


def problems(root: Path, base: str) -> list[str]:
    """The reasons the pull request misses its CHANGELOG line (empty when it is fine or needs none)."""
    changed = [name for name in git("diff", "--name-only", f"{base}...HEAD", cwd=root).splitlines() if name]
    watched = watched_paths(root)
    code = [name for name in changed if name.startswith(watched)]
    if not code:
        return []
    try:
        before = git("show", f"{base}:CHANGELOG.md", cwd=root)
    except subprocess.CalledProcessError:
        before = ""
    path = root / "CHANGELOG.md"
    after = path.read_text(encoding="utf-8") if path.is_file() else ""
    if {name for name in versions(after) - versions(before) if name.lower() != "unreleased"}:
        return []  # a release heading (an empty [Unreleased] added, or a heading renamed to it, is not one)
    if unreleased_lines(after) - unreleased_lines(before):
        return []
    return [f"{len(code)} file(s) of {', '.join(watched)} changed (for example {code[0]}) but CHANGELOG.md has no new line under \"## [Unreleased]\""]


def main(argv: list[str] | None = None) -> int:
    class Parser(argparse.ArgumentParser):
        def error(self, message: str):
            self.print_usage(sys.stderr)
            self.exit(64, f"{self.prog}: error: {message}\n")

    p = Parser(description="Checks that a pull request adds a line to the unreleased part of the CHANGELOG.")
    p.add_argument("--base", required=True, help="the branch the pull request goes to, for example origin/main")
    p.add_argument("--root", type=Path, default=ROOT, help="the repository (default: the one that holds this tool)")
    p.add_argument("--warn-only", action="store_true", help="print the problem but exit 0")
    a = p.parse_args(argv)
    try:
        found = problems(a.root, a.base)
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"CHANGELOG_PR=ERROR cannot compare with {a.base}: {exc}", file=sys.stderr)
        return 64
    for line in found:
        print(f"CHANGELOG_PR: {line}")
    if found and not a.warn_only:
        print("CHANGELOG_PR=FAIL")
        return 1
    print("CHANGELOG_PR=PASS" if not found else "CHANGELOG_PR=WARN")
    return 0


if __name__ == "__main__":
    sys.exit(main())
