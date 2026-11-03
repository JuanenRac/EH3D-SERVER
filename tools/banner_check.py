#!/usr/bin/env python3
# =============================================================================
# EH3D-SERVER - tools/banner_check.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
# canonical: EH3D-DOCS tools 1
"""Checks that the banner of a repository (images/*BANNER*.svg) says what its manifest says.

A banner is copied from another repository and then edited by hand, and the copy keeps the name, the role or the technologies of the original. The check reads the text of
the banner and compares it with eh3d.project.json:

* the tag at the top (``EH3D · ROLE``) and the badge at the top right (``ROLE``) show the role of the manifest;
* the big title is the name of the repository;
* the grey line under the tagline lists the technologies of the manifest, in order, joined with ``·``;
* the pills show the first four technologies, in order;
* the tagline exists (its words are free: they are a slogan, not data).

    python tools/banner_check.py              # check the banner of this repository
    python tools/banner_check.py --repo PATH  # check another repository

``ci_validate.py`` calls ``run_in_ci``. A repository without a banner has nothing to check, and the plain banner of a new repository (only the name and a slogan) is checked for its name alone. Exit codes: 0 correct, 1 differences, 64 wrong use.
"""
from __future__ import annotations

import html
import json
import re
import sys
from pathlib import Path

TEXT = re.compile(r"<text\b([^>]*)>(.*?)</text>", re.S)
TAG = re.compile(r"<[^>]+>")  # a <tspan> inside a <text> is part of its content
ATTR = re.compile(r'(\w[\w-]*)="([^"]*)"')


def texts(svg: str) -> list[tuple[dict[str, str], str]]:
    """Every <text> of the picture as (attributes, content), in document order."""
    return [(dict(ATTR.findall(attrs)), html.unescape(TAG.sub("", body)).strip()) for attrs, body in TEXT.findall(svg)]


def banner_files(root: Path) -> list[Path]:
    folder = root / "images"
    return sorted(path for path in folder.iterdir() if path.suffix.lower() == ".svg" and "banner" in path.name.lower()) if folder.is_dir() else []


def problems_of(svg: str, manifest: dict, label: str) -> list[str]:
    """The differences between the text of one banner and the manifest."""
    found = texts(svg)
    role = str(manifest.get("role", "")).upper()
    techs = [str(t) for t in manifest.get("technologies", [])]
    name = str(manifest.get("name", ""))
    out: list[str] = []
    contents = [body for _, body in found]

    def has(kind: str, wanted: str) -> None:
        if wanted not in contents:
            out.append(f"{label}: the {kind} should say '{wanted}'")

    has("title", name)
    if not any(body.startswith("EH3D · ") for body in contents):
        return out  # the plain banner of a new repository (name and slogan only) has no role, line or pills to compare
    has("tag at the top", f"EH3D · {role}")
    has("badge", role)
    line = " · ".join(t.upper() for t in techs)
    has("line of technologies", line)
    pills = [body for attrs, body in found if attrs.get("font-size") == "10" and attrs.get("letter-spacing") == "0.5"]
    wanted = [t.upper() for t in techs[:4]]
    if pills != wanted:
        out.append(f"{label}: the pills show {pills} and the first technologies of the manifest are {wanted}")
    taglines = [body for attrs, body in found if attrs.get("font-size") == "22"]
    if not any(taglines):
        out.append(f"{label}: the banner has no tagline")
    return out


def problems(root: Path) -> list[str]:
    manifest_path = next((root / name for name in ("eh3d.project.json", "electro-hobby-3d.project.json") if (root / name).is_file()), root / "eh3d.project.json")
    banners = banner_files(root)
    if not banners or not manifest_path.is_file():
        return []
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except ValueError:
        return []  # a manifest that cannot be read is reported by the validator itself
    out: list[str] = []
    for banner in banners:
        out.extend(problems_of(banner.read_text(encoding="utf-8", errors="replace"), manifest, f"images/{banner.name}"))
    return out


def run_in_ci(root: Path) -> list[str]:
    """What a ci_validate calls: the reasons to fail."""
    return problems(root)


def main(argv: list[str]) -> int:
    root = Path(__file__).resolve().parents[1]
    args = list(argv)
    if args and args[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    if args[:1] == ["--repo"] and len(args) == 2:
        root = Path(args[1]).resolve()
    elif args:
        print("usage: banner_check.py [--repo PATH]", file=sys.stderr)
        return 64
    found = problems(root)
    for line in found:
        print("  FAIL", line)
    print(f"BANNER_CHECK={'FAIL' if found else 'PASS'} repo={root.name}")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
