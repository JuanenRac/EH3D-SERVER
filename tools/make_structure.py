#!/usr/bin/env python3
# =============================================================================
# A.R.M.O.R. - tools/make_structure.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D)
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Draws the Repository Structure of a repository from `docs/structure.json`.

    python tools/make_structure.py                  # writes images/repository-structure.svg (and the other languages when "svg": "all")
    python tools/make_structure.py --tree es        # prints the text tree of a language (what goes inside the README)
    python tools/make_structure.py --check          # exit 1 when the tree, the disk and the pictures disagree
    python tools/make_structure.py --check --readmes   # ... and when a README does not carry the tree of its language
    python tools/make_structure.py --png out.png    # also screenshots the English picture with Edge (to look at it)

One description per entry and per language (en, es, de, fr, it, ja, zh, pt) lives in
docs/structure.json; the same file gives the text tree (the names are identical in
every language) and the animated picture (no icons: type, lines, curves, a flow of
dashes). Every text of the picture is measured: one that would leave its card or
lie on another text stops the build.

docs/structure.json:

    {
      "repo": "ARMOR-VOICE-AI",
      "ecosystem": "armor",                       # armor | hydra | urtc | eh3d
      "svg": "en",                                # "en" (one picture) or "all" (one per language)
      "exempt": ["package-lock.json"],            # tracked first-level names the tree need not list
      "tagline": {"en": "...", ...},              # optional, a default exists in the eight languages
      "cards": [
        {"name": "src/armor_voice_ai/", "sub": L, "span": 3, "accent": "a",
         "rows": [{"name": "phrases.py", "desc": L}, ...]},          # a folder card: tree shows its rows one level down
        {"name": "tests/", "sub": L, "span": 1, "tree": L,
         "rows": [{"desc": L}, ...]},                                 # "tree": the folder is one line in the tree
        {"name": "docs/", "sub": L, "span": 1, "tree": L, "rows": [{"name": "SAFETY.md", "desc": L}]},
        {"name": L, "kind": "group", "sub": L, "span": 1,
         "rows": [{"name": "tools/", "desc": L}, ...]},               # a group: every row is a first-level entry
        {"name": L, "kind": "group", "span": 3, "columns": 2,
         "rows": [{"name": "run.sh  run.bat", "tree_name": "run.sh run.bat", "desc": L, "tree_desc": L}]}
      ]
    }

L is a string (the same in every language) or {"en": ..., "es": ..., ...}. span is 1 to 3
(of a grid of three columns), accent a to d (the colours of the family), style list, stack or plain.
"""
from __future__ import annotations

import argparse
import fnmatch
import json
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _readme_svg as K  # noqa: E402

W = 1200
GRID_X, GRID_W, GRID_GAP, ROW_GAP = 360, 780, 25, 30
GRID_TOP = 200
EYEBROW = {"en": "REPOSITORY STRUCTURE", "es": "ESTRUCTURA DEL REPOSITORIO", "de": "REPOSITORY-STRUKTUR", "fr": "STRUCTURE DU DÉPÔT",
           "it": "STRUTTURA DEL REPOSITORY", "ja": "リポジトリ構造", "zh": "仓库结构", "pt": "ESTRUTURA DO REPOSITÓRIO"}
ROOT_LABEL = {"en": "repository root", "es": "raíz del repositorio", "de": "Wurzel des Repositorys", "fr": "racine du dépôt",
              "it": "radice del repository", "ja": "リポジトリのルート", "zh": "仓库根目录", "pt": "raiz do repositório"}
TAGLINE = {"en": "Two levels: the folders, and the files that carry the logic.",
           "es": "Dos niveles: las carpetas y los ficheros que llevan la lógica.",
           "de": "Zwei Ebenen: die Ordner und die Dateien, die die Logik tragen.",
           "fr": "Deux niveaux : les dossiers et les fichiers qui portent la logique.",
           "it": "Due livelli: le cartelle e i file che portano la logica.",
           "ja": "2階層：フォルダと、ロジックを担うファイル。",
           "zh": "两个层级：文件夹，以及承载逻辑的文件。",
           "pt": "Dois níveis: as pastas e os ficheiros que levam a lógica."}
NEVER_LISTED = re.compile(r"^(README(_[a-z]{3})?\.md|\.(?!github$).*)$")   # READMEs and dotfiles other than .github


class SpecError(ValueError):
    pass


# --------------------------------------------------------------------------- the specification
def L(value, lang: str) -> str:
    """A text in a language: a string is the same everywhere, a mapping must have the language."""
    if isinstance(value, dict):
        if lang not in value:
            raise SpecError(f"a text is missing the language '{lang}': {value}")
        return value[lang]
    return "" if value is None else str(value)


def load_spec(path: Path) -> dict:
    try:
        spec = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SpecError(f"{path} does not exist") from None
    except json.JSONDecodeError as error:
        raise SpecError(f"{path} is not valid JSON: {error}") from None
    validate_spec(spec)
    return spec


def validate_spec(spec: dict) -> None:
    for key in ("repo", "ecosystem", "cards"):
        if not spec.get(key):
            raise SpecError(f"structure.json needs '{key}'")
    K.palette(spec["ecosystem"])
    if spec.get("svg", "en") not in ("en", "all"):
        raise SpecError("'svg' is 'en' or 'all'")
    for card in spec["cards"]:
        if "name" not in card or not card.get("rows"):
            raise SpecError(f"every card needs a name and rows: {card.get('name')}")
        if card.get("span", 1) not in (1, 2, 3):
            raise SpecError(f"card '{card['name']}': span is 1, 2 or 3")
        if card.get("accent", "a") not in ("a", "b", "c", "d"):
            raise SpecError(f"card '{card['name']}': accent is a, b, c or d")
        kind = card_kind(card)
        if kind == "group":
            for row in card["rows"]:
                if not row.get("name"):
                    raise SpecError(f"group '{card['name']}': every row needs a name")
        elif not card.get("tree"):
            for row in card["rows"]:
                if not row.get("name"):
                    raise SpecError(f"folder '{card['name']}' has unnamed rows, so it needs a 'tree' description")


def card_kind(card: dict) -> str:
    return card.get("kind") or ("folder" if isinstance(card["name"], str) and card["name"].endswith("/") else "group")


def card_style(card: dict) -> str:
    if card.get("style"):
        return card["style"]
    if not any(r.get("name") for r in card["rows"]):
        return "plain"
    if card.get("span", 1) == 1 or card.get("columns", 1) > 1:
        return "stack"
    return "list"


# --------------------------------------------------------------------------- the text tree
def tree_items(spec: dict, lang: str) -> list[tuple[int, str, str]]:
    """(depth, name, description) in tree order."""
    items: list[tuple[int, str, str]] = []
    for card in spec["cards"]:
        if card_kind(card) == "group":
            for row in card["rows"]:
                desc = L(row["tree_desc"], lang) if "tree_desc" in row else L(row.get("desc"), lang)
                items.append((1, row.get("tree_name", row["name"]), desc))
        elif card.get("tree"):
            items.append((1, card["name"], L(card["tree"], lang)))
        else:
            items.append((1, card["name"], L(card.get("tree_head", ""), lang)))
            for row in card["rows"]:
                items.append((2, row.get("tree_name", row["name"]), L(row.get("tree_desc", row.get("desc")), lang)))
    return items


def render_tree(spec: dict, lang: str) -> str:
    items = tree_items(spec, lang)
    top_last = max(i for i, it in enumerate(items) if it[0] == 1)
    lines: list[tuple[str, str]] = [(f"{spec['repo']}/", "")]
    parent_last = False
    for i, (depth, name, desc) in enumerate(items):
        if depth == 1:
            last = i == top_last
            parent_last = last
            lines.append((("└── " if last else "├── ") + name, desc))
        else:
            nxt = items[i + 1][0] if i + 1 < len(items) else 1
            last_child = nxt == 1
            lines.append((("    " if parent_last else "│   ") + ("└── " if last_child else "├── ") + name, desc))
    column = max(len(left) for left, desc in lines if desc) + 3 if any(d for _, d in lines) else 0
    return "\n".join((left.ljust(column) + desc).rstrip() for left, desc in lines)


def tree_names(spec: dict) -> tuple[set[str], list[str], list[str]]:
    """The first-level names the tree lists, the second-level paths (folder/child) and the paths of the first level as written."""
    first: set[str] = set()
    second: list[str] = []
    full: list[str] = []
    current = ""
    for depth, name, _ in tree_items(spec, "en"):
        parts = [p for p in re.split(r"\s+", name.strip()) if p]
        if depth == 1:
            for part in parts:
                first.add(part.rstrip("/").split("/")[0])
                full.append(part.rstrip("/"))
            current = parts[0].rstrip("/") if parts else ""
        else:
            for part in parts:
                second.append(f"{current}/{part.rstrip('/')}")
    return first, second, full


# --------------------------------------------------------------------------- the disk
def tracked_files(root: Path) -> list[str] | None:
    try:
        result = subprocess.run(["git", "ls-files", "-z"], cwd=root, capture_output=True, check=True)
    except (OSError, subprocess.CalledProcessError):
        return None
    files = [x for x in result.stdout.decode("utf-8", "replace").split("\0") if x]
    return files or None


def check_disk(spec: dict, root: Path) -> list[str]:
    problems: list[str] = []
    first, second, full = tree_names(spec)
    files = tracked_files(root)
    if files is None:
        first_level = {p.name for p in root.iterdir()}
        present = lambda rel: (root / rel).exists()          # noqa: E731
    else:
        first_level = {f.split("/")[0] for f in files}
        tracked = set(files)
        folders = {"/".join(f.split("/")[:k]) for f in files for k in range(1, len(f.split("/")))}
        present = lambda rel: rel in tracked or rel in folders  # noqa: E731
    exempt = list(spec.get("exempt", []))
    missing = sorted(n for n in first_level
                     if not NEVER_LISTED.match(n) and not any(fnmatch.fnmatchcase(n, pat) for pat in first | set(exempt)))
    if missing:
        problems.append("versioned entries the tree does not list: " + ", ".join(missing))
    phantom = sorted(n for n in full if "*" not in n and not (root / n).exists() and n not in first_level)
    if phantom:
        problems.append("the tree lists entries that are not in the repository: " + ", ".join(phantom))
    gone = sorted(p for p in second if not present(p) and not (root / p).exists() and "*" not in p)
    if gone:
        problems.append("the tree lists files that are not in the repository: " + ", ".join(gone))
    for card in spec["cards"]:
        if card.get("complete") and card_kind(card) == "folder" and files is not None:
            folder = card["name"].rstrip("/") + "/"
            listed = {r["name"] for r in card["rows"] if r.get("name")}
            real = {f[len(folder):].split("/")[0] for f in files if f.startswith(folder)}
            lacking = sorted(n for n in real if n not in listed and n != "__init__.py")
            if lacking:
                problems.append(f"{card['name']} is marked complete but the tree leaves out: " + ", ".join(lacking))
    return problems


# --------------------------------------------------------------------------- the picture
def card_height(card: dict) -> float:
    n = len(card["rows"])
    style = card_style(card)
    cols = card.get("columns", 1)
    if style == "list":
        return 80 + (n - 1) * 27 + 31
    if style == "plain":
        return 80 + (n - 1) * 24 + 38
    per = -(-n // cols)
    return 76 + (per - 1) * 36 + 42


def build_svg(spec: dict, lang: str) -> str:
    pal = K.palette(spec["ecosystem"])
    colors = pal["colors"]
    mono, sans = K.MONO, K.SANS
    cards = spec["cards"]

    # grid placement: cards fill rows of three columns in order
    cw = (GRID_W - 2 * GRID_GAP) / 3
    rows: list[list[dict]] = []
    used = 0
    for card in cards:
        span = card.get("span", 1)
        if not rows or used + span > 3:
            rows.append([])
            used = 0
        rows[-1].append(card)
        used += span
    placed = []
    y = GRID_TOP
    for ri, row in enumerate(rows):
        h = max(card_height(c) for c in row)
        x = GRID_X
        for c in row:
            span = c.get("span", 1)
            w = span * cw + (span - 1) * GRID_GAP
            placed.append({"card": c, "x": x, "y": y, "w": w, "h": h, "row": ri})
            x += w + GRID_GAP
        y += h + ROW_GAP
    grid_bottom = y - ROW_GAP
    H = int(grid_bottom + 44)
    cv = K.Canvas(W, H)
    add = cv.add

    header = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="100%" role="img" aria-labelledby="t d">\n'
              f'<title id="t">Repository structure of {K.escape(spec["repo"])}</title>\n'
              f'<desc id="d">The root of {K.escape(spec["repo"])} and its main folders and files: '
              + K.escape(", ".join(L(c["name"], lang) for c in cards)) + '.</desc>')
    add(f'<defs>{K.common_defs(pal)}</defs>')
    add(f'<rect width="{W}" height="{H}" rx="14" fill="url(#bg)"/><rect width="{W}" height="{H}" rx="14" fill="url(#grid)"/>')
    add(f'<rect x="0.5" y="0.5" width="{W - 1}" height="{H - 1}" rx="14" fill="none" stroke="#1B2530"/>')

    # title block
    title_bound = (0, 0, W, 170)
    cv.text(60, 62, EYEBROW[lang], 12.5, colors[1], sans, "600", spacing=4.5 if lang not in ("ja", "zh") else 3, bound=title_bound)
    name_size = min(36.0, 1000 / (len(spec["repo"]) * 0.66))
    cv.text(60, 106, spec["repo"], round(name_size, 1), "#FFFFFF", sans, "700", spacing=0.5, bound=title_bound)
    add('<rect x="60" y="122" width="150" height="2.5" rx="1.25" fill="url(#line)"><animate attributeName="width" values="40;150;150" keyTimes="0;0.4;1" dur="3s" repeatCount="indefinite"/></rect>')
    tagline = L(spec.get("tagline"), lang) if spec.get("tagline") else TAGLINE[lang]
    cv.text(60, 152, tagline, 13.5, "#8A97A6", bound=title_bound)

    # connectors from the root (drawn under the cards)
    root_y = (GRID_TOP + grid_bottom) / 2
    paths = []
    row_tops: dict[int, tuple[float, float, list[dict]]] = {}
    for p in placed:
        r = p["row"]
        if r not in row_tops:
            row_tops[r] = (p["y"], p["h"], [])
        row_tops[r][2].append(p)
    for idx, r in enumerate(sorted(row_tops)):
        top, h, members = row_tops[r]
        if len(members) == 1:
            ty = top + h / 2
            paths.append((f"c{idx}", f"M300,{root_y:g} C335,{root_y:g} 330,{ty:g} {GRID_X},{ty:g}", [("ring", GRID_X, ty)]))
        else:
            bus = top - 15
            centres = [m["x"] + m["w"] / 2 for m in members]
            paths.append((f"c{idx}", f"M300,{root_y:g} C340,{root_y:g} 335,{bus:g} {GRID_X + 12},{bus:g} H{centres[-1]:g}",
                          [("drop", cx, bus, top) for cx in centres]))
    for pid, d, marks in paths:
        add(f'<path id="{pid}" d="{d}" fill="none" stroke="#243140" stroke-width="2"/>')
        add(f'<path d="{d}" fill="none" stroke="url(#line)" stroke-width="2" stroke-linecap="round" stroke-dasharray="6 9" opacity="0.9"><animate attributeName="stroke-dashoffset" from="0" to="-30" dur="1.6s" repeatCount="indefinite"/></path>')
        for m in marks:
            if m[0] == "drop":
                add(f'<path d="M{m[1]:g},{m[2]:g} V{m[3]:g}" stroke="url(#line)" stroke-width="2" fill="none"/>')
                add(f'<circle cx="{m[1]:g}" cy="{m[2]:g}" r="4" fill="{colors[1]}" filter="url(#glow)"/>')
            else:
                add(f'<circle cx="{m[1]:g}" cy="{m[2]:g}" r="4.5" fill="#0D1117" stroke="{colors[0]}" stroke-width="1.5"/>')
    for i, (pid, _, _) in enumerate(paths):
        color = colors[i % 3]
        add(f'<circle r="3.5" fill="{color}" filter="url(#glow)"><animateMotion dur="{3.4 + 0.8 * i:.1f}s" repeatCount="indefinite"><mpath href="#{pid}"/></animateMotion></circle>')

    # the root
    root_text = f"{spec['repo']}/"
    root_size = min(18.5, 196 / (len(root_text) * 0.62))
    if root_size < 10.5:
        raise SpecError(f"the name {spec['repo']} is too long for the root box")
    ry = root_y - 35
    rb = (60, ry, 240, 70)
    add(f'<rect x="60" y="{ry:g}" width="240" height="70" rx="12" fill="#0F1620" stroke="#2A3A4C" stroke-width="1.2"/>')
    add(f'<rect x="60" y="{ry:g}" width="240" height="70" rx="12" fill="none" stroke="{colors[0]}" stroke-opacity="0.5" stroke-width="1.2" filter="url(#glow)"><animate attributeName="stroke-opacity" values="0.15;0.6;0.15" dur="3.6s" repeatCount="indefinite"/></rect>')
    cv.text(80, ry + 31, root_text, round(root_size, 1), "#FFFFFF", mono, "700", bound=rb)
    cv.text(80, ry + 53, ROOT_LABEL[lang], 12, "#7D8A99", sans, spacing=1 if lang not in ("ja", "zh") else 0, bound=rb)
    add(f'<circle cx="300" cy="{root_y:g}" r="5" fill="{colors[0]}" filter="url(#glow)"/>')

    # the cards
    for n, p in enumerate(placed):
        c, x, y, w, h = p["card"], p["x"], p["y"], p["w"], p["h"]
        accent = colors["abcd".index(c.get("accent", "abcd"[n % 3]))]
        bound = (x, y, w, h)
        add(f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="10" fill="url(#card)" stroke="#1E2A36" stroke-width="1"/>')
        add(f'<rect x="{x:g}" y="{y + 14:g}" width="3" height="{h - 28:g}" rx="1.5" fill="{accent}"><animate attributeName="opacity" values="0.55;1;0.55" dur="4s" begin="{0.6 * n:g}s" repeatCount="indefinite"/></rect>')
        head = L(c["name"], lang)
        head_size = 19 if card_kind(c) == "folder" else 17
        fam = mono if card_kind(c) == "folder" else sans
        cv.text(x + 22, y + 34, head, head_size, "#F2F6FA", fam, "700", bound=bound)
        if c.get("sub"):
            cv.text(x + w - 18, y + 34, L(c["sub"], lang), 12, "#7D8A99", sans, anchor="end", spacing=0.3 if lang not in ("ja", "zh") else 0, bound=bound)
        add(f'<line x1="{x + 22:g}" y1="{y + 48:g}" x2="{x + w - 18:g}" y2="{y + 48:g}" stroke="{accent}" stroke-opacity="0.35" stroke-width="1"/>')
        style = card_style(c)
        crow = c["rows"]
        if style == "list":
            name_w = max(K.text_width(r["name"], 14.5, mono, "600") for r in crow)
            dx = x + 32 + name_w + 24
            for i, r in enumerate(crow):
                ty = y + 80 + i * 27
                cv.text(x + 32, ty, r["name"], 14.5, "#E6EDF3", mono, "600", bound=bound)
                cv.text(dx, ty, L(r.get("desc"), lang), 12.8, "#8A97A6", sans, bound=bound)
                add(f'<line x1="{x + 32:g}" y1="{ty + 8:g}" x2="{x + w - 22:g}" y2="{ty + 8:g}" stroke="#1A2430" stroke-width="1"/>')
        elif style == "plain":
            for i, r in enumerate(crow):
                cv.text(x + 22, y + 80 + i * 24, L(r.get("desc"), lang), 13.5, "#C9D4DF", mono if all(ord(ch) < 0x2E80 for ch in L(r.get("desc"), lang)) else sans, bound=bound)
        else:
            cols = c.get("columns", 1)
            per = -(-len(crow) // cols)
            col_w = (w - 54) / cols
            for i, r in enumerate(crow):
                col, line = divmod(i, per)
                tx = x + 32 + col * col_w if cols > 1 else x + 22
                ty = y + 76 + line * 36
                cv.text(tx, ty, r["name"], 13.5 if K.text_width(r["name"], 13.5, mono, "600") < col_w - 12 else 11.6, "#E6EDF3", mono, "600", bound=bound)
                cv.text(tx, ty + 16, L(r.get("desc"), lang), 11.8, "#7D8A99", sans, bound=bound)
    return cv.render(header)


def svg_path(root: Path, lang: str) -> Path:
    suffix = f"_{K.FILE_CODE[lang]}" if K.FILE_CODE[lang] else ""
    return root / "images" / f"repository-structure{suffix}.svg"


def picture_languages(spec: dict, langs: list[str] | None) -> list[str]:
    if langs:
        return langs
    return list(K.LANGUAGES) if spec.get("svg", "en") == "all" else ["en"]


# --------------------------------------------------------------------------- run
def check_readmes(spec: dict, root: Path, langs: list[str]) -> list[str]:
    problems = []
    for lang in langs:
        path = root / K.README_NAME[lang]
        if not path.is_file():
            problems.append(f"{K.README_NAME[lang]} does not exist")
            continue
        if render_tree(spec, lang) not in K.normalise(path.read_text(encoding="utf-8")):
            problems.append(f"{K.README_NAME[lang]} does not carry the tree of docs/structure.json (python tools/make_structure.py --tree {lang})")
    return problems


def run(root: Path, check: bool, readmes: bool, tree: str | None, langs: list[str] | None, png: Path | None) -> int:
    try:
        spec = load_spec(root / "docs" / "structure.json")
        if tree:
            print(render_tree(spec, tree))
            return 0
        pictures = picture_languages(spec, langs)
        svgs = {lang: build_svg(spec, lang) for lang in pictures}
        for svg in svgs.values():
            K.validate_xml(svg)
        trees = {lang: render_tree(spec, lang) for lang in K.LANGUAGES}   # raises when a language lacks a description
    except (SpecError, ValueError) as error:
        print(f"STRUCTURE=FAIL {error}")
        return 1
    if check:
        problems = check_disk(spec, root)
        for lang, svg in svgs.items():
            target = svg_path(root, lang)
            if not target.is_file():
                problems.append(f"{target.relative_to(root)} does not exist (run tools/make_structure.py)")
            elif K.normalise(target.read_text(encoding="utf-8")) != svg:
                problems.append(f"{target.relative_to(root)} does not match docs/structure.json (run tools/make_structure.py)")
        if readmes:
            problems += check_readmes(spec, root, list(K.LANGUAGES))
        if problems:
            print("STRUCTURE=FAIL " + "; ".join(problems))
            return 1
        print(f"STRUCTURE=PASS {spec['repo']} ({len(trees['en'].splitlines())} lines, {len(svgs)} picture(s))")
    else:
        for lang, svg in svgs.items():
            target = svg_path(root, lang)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(svg, encoding="utf-8", newline="\n")
            print(f"STRUCTURE=WRITTEN {target} ({len(svg)} bytes)")
    if png:
        first = next(iter(svgs))
        target = svg_path(root, first)
        if not target.is_file():
            import tempfile
            target = Path(tempfile.mkdtemp(prefix="structure_")) / "repository-structure.svg"
            target.write_text(svgs[first], encoding="utf-8")
        height = int(svgs[first].split('viewBox="0 0 1200 ')[1].split('"')[0])
        print(f"PNG={'written ' + str(png) if K.render_png(target, png, W, height) else 'skipped (Edge not available)'}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Draws images/repository-structure.svg and the text tree from docs/structure.json")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1], help="the repository (default: the one this file is in)")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--readmes", action="store_true", help="with --check: also require the tree in every README")
    parser.add_argument("--tree", choices=K.LANGUAGES, help="print the text tree of a language")
    parser.add_argument("--langs", help="comma separated languages of the pictures (default: the 'svg' setting)")
    parser.add_argument("--png", type=Path, help="also screenshot the first picture with Edge")
    args = parser.parse_args(argv)
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    langs = [x.strip() for x in args.langs.split(",")] if args.langs else None
    if langs and any(x not in K.LANGUAGES for x in langs):
        parser.error(f"languages are {', '.join(K.LANGUAGES)}")
    return run(args.root.resolve(), args.check, args.readmes, args.tree, langs, args.png)


if __name__ == "__main__":
    raise SystemExit(main())
