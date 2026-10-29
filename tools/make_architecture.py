#!/usr/bin/env python3
# =============================================================================
# A.R.M.O.R. - tools/make_architecture.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D)
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Draws the Architecture picture of a repository from `docs/architecture.json`.

    python tools/make_architecture.py             # writes images/architecture.svg
    python tools/make_architecture.py --check     # exit 1 when the picture, the JSON or the code it points to disagree
    python tools/make_architecture.py --png out.png   # also screenshots the picture with Edge (to look at it)
    python tools/make_architecture.py --root DIR  # another repository than the one this file lives in

The picture is 1200x640 with at most twelve boxes in three columns: who calls the
repository -> the repository -> what it depends on. It has no icons and no
drawings: type, lines, curves and a discreet SMIL motion (dashes flowing along
the arrows, a pulse that visits the boxes one after another). Every text is
measured; a text that would leave its box or lie on another one stops the build.

docs/architecture.json:

    {
      "repo": "ARMOR-VOICE-AI",                  # name of the repository (title of the picture)
      "ecosystem": "armor",                      # armor | hydra | urtc | eh3d (the palette)
      "tagline": "One line under the title",
      "description": "Plain sentence for screen readers (the <desc> of the SVG, the alt text)",
      "columns": ["CALLERS", "THE VOICE SERVICE", "CHOSEN BY THE INSTALLATION"],   # optional
      "nodes": [
        {"id": "srv", "column": 0, "level": "contract", "title": "ARMOR-SERVER",
         "lines": ["asks the service", ["holds the PIN", "ok"]],
         "code": ["src/service.py", "src/turn.py::9"]}        # what the box stands for (checked by --check)
      ],
      "edges": [{"from": "srv", "to": "turn", "label": "/v1/turn", "level": "tested"}],
      "legend": {"tested": "decides, tested (114 tests)"},    # optional text per level
      "footer": "A last honest sentence"                      # optional
    }

column: 0 callers, 1 the repository, 2 what it depends on. level: tested, contract,
config (amber, dashed: chosen by the installation or not tried for real), untested
(red: has not run on real hardware) or external (grey: not part of this repository).
A line is a string or [string, tone] with tone primary, muted, ok, warn or bad.
`code` entries are paths relative to the repository, optionally `path::text` when
the text has to be in the file: the check fails when the file or the text is gone.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _readme_svg as K  # noqa: E402

W, H = 1200, 640
MARGIN = 30
SIDE_W = 225
GAP = 90
TOP, BOTTOM = 116, 530
MAX_NODES = 12
LEVELS = ("tested", "contract", "config", "untested", "external")
TONES = ("primary", "muted", "ok", "warn", "bad")
DEFAULT_COLUMNS = ("CALLERS", "THIS REPOSITORY", "DEPENDS ON")
DEFAULT_LEGEND = {
    "tested": "tested part",
    "contract": "contract, signed or server part",
    "config": "chosen by the installation, never tried for real",
    "untested": "has not run on real hardware",
    "external": "outside this repository",
}


class SpecError(ValueError):
    pass


# --------------------------------------------------------------------------- the specification
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
    for key in ("repo", "ecosystem", "description", "nodes"):
        if not spec.get(key):
            raise SpecError(f"architecture.json needs '{key}'")
    K.palette(spec["ecosystem"])
    nodes = spec["nodes"]
    if not isinstance(nodes, list) or not 1 <= len(nodes) <= MAX_NODES:
        raise SpecError(f"between 1 and {MAX_NODES} nodes are allowed, got {len(nodes) if isinstance(nodes, list) else 'none'}")
    ids: dict[str, dict] = {}
    for node in nodes:
        for key in ("id", "column", "title"):
            if key not in node:
                raise SpecError(f"a node is missing '{key}': {node}")
        if node["id"] in ids:
            raise SpecError(f"duplicate node id '{node['id']}'")
        if node["column"] not in (0, 1, 2):
            raise SpecError(f"node '{node['id']}': column must be 0, 1 or 2")
        if node.get("level", "tested") not in LEVELS:
            raise SpecError(f"node '{node['id']}': level must be one of {', '.join(LEVELS)}")
        for line in node.get("lines", []):
            tone = line[1] if isinstance(line, list) else "primary"
            if tone not in TONES:
                raise SpecError(f"node '{node['id']}': tone '{tone}' is not one of {', '.join(TONES)}")
        ids[node["id"]] = node
    if not any(n["column"] == 1 for n in nodes):
        raise SpecError("at least one node has to be in column 1 (the repository)")
    for edge in spec.get("edges", []):
        for end in ("from", "to"):
            if edge.get(end) not in ids:
                raise SpecError(f"edge {edge}: '{end}' is not a node id")
        a, b = ids[edge["from"]]["column"], ids[edge["to"]]["column"]
        if {a, b} == {0, 2}:
            raise SpecError(f"edge {edge['from']} -> {edge['to']} joins the two outer columns: go through the repository")
        if edge.get("level", "tested") not in LEVELS:
            raise SpecError(f"edge {edge}: unknown level")


def spec_digest(spec: dict) -> str:
    return hashlib.sha256(json.dumps(spec, sort_keys=True, ensure_ascii=False).encode("utf-8")).hexdigest()[:16]


# --------------------------------------------------------------------------- the style of a level
def level_style(level: str, pal: dict) -> dict:
    c0, c1 = pal["colors"][0], pal["colors"][1]
    return {
        "tested": {"stroke": c0, "title": c0, "sw": 1.2, "dash": None},
        "contract": {"stroke": c1, "title": c1, "sw": 2, "dash": None},
        "config": {"stroke": K.AMBER, "title": K.AMBER, "sw": 2, "dash": "7 5"},
        "untested": {"stroke": K.BORDER, "title": K.INK, "sw": 1, "dash": None},
        "external": {"stroke": K.BORDER, "title": K.INK, "sw": 1, "dash": None},
    }[level]


def tone_color(tone: str, pal: dict) -> str:
    return {"primary": K.INK, "muted": K.MUTED, "ok": pal["colors"][0], "warn": K.AMBER, "bad": K.RED}[tone]


def node_lines(node: dict) -> list[tuple[str, str]]:
    out = []
    for i, line in enumerate(node.get("lines", [])):
        text, tone = (line[0], line[1]) if isinstance(line, list) else (line, "primary" if i == 0 else "muted")
        out.append((text, tone))
    return out


def node_height(node: dict) -> float:
    n = max(1, len(node.get("lines", [])))
    return 48 + 18 * (n - 1) + 24 + (12 if node.get("level") == "untested" else 0)


# --------------------------------------------------------------------------- layout
def layout(spec: dict) -> dict:
    nodes = spec["nodes"]
    present = {c for c in (0, 2) if any(n["column"] == c for n in nodes)}
    x_left = MARGIN + (SIDE_W + GAP if 0 in present else 0)
    x_right = W - MARGIN - (SIDE_W + GAP if 2 in present else 0)
    container = {"x": x_left, "w": x_right - x_left, "y": TOP}
    boxes: dict[str, dict] = {}

    for column, x in ((0, MARGIN), (2, W - MARGIN - SIDE_W)):
        y = TOP
        for node in (n for n in nodes if n["column"] == column):
            h = node_height(node)
            boxes[node["id"]] = {"x": x, "y": y, "w": SIDE_W, "h": h, "node": node}
            y += h + 16
        if y - 16 > BOTTOM:
            raise SpecError(f"column {column} is too tall ({y - 16:.0f}px, at most {BOTTOM}); use fewer or shorter boxes")

    inner = [n for n in nodes if n["column"] == 1]
    sub = 1 if len(inner) <= 3 else 2 if len(inner) <= 8 else 3
    pad, gap = 20, 16
    card_w = (container["w"] - 2 * pad - (sub - 1) * gap) / sub
    rows = [inner[i:i + sub] for i in range(0, len(inner), sub)]
    y = TOP + pad
    for row in rows:
        h = max(node_height(n) for n in row)
        for j, node in enumerate(row):
            boxes[node["id"]] = {"x": container["x"] + pad + j * (card_w + gap), "y": y, "w": card_w, "h": h, "node": node, "sub": j, "row": rows.index(row)}
        y += h + 20
    container["h"] = y - 20 + pad - TOP
    if container["y"] + container["h"] > BOTTOM + 8:
        raise SpecError(f"the repository column is too tall ({container['h']:.0f}px); use fewer or shorter boxes")
    return {"container": container, "boxes": boxes, "present": present}


# --------------------------------------------------------------------------- drawing
def edge_geometry(spec: dict, lay: dict) -> list[dict]:
    boxes, cont = lay["boxes"], lay["container"]
    edges = spec.get("edges", [])
    # edges that meet at the same side of a box (or at the same row of the frame of the repository) are spread along it
    groups: dict[tuple, list[int]] = {}
    for i, e in enumerate(edges):
        for end in ("from", "to"):
            box = boxes[e[end]]
            other = boxes[e["to" if end == "from" else "from"]]
            if box["node"]["column"] == 1 and other["node"]["column"] != 1:
                key = ("frame", other["node"]["column"], box["row"])
            else:
                key = ("box", e[end])
            groups.setdefault(key, []).append((i, end))

    def spread(edge_index: int, end: str) -> float:
        e = edges[edge_index]
        box = boxes[e[end]]
        other = boxes[e["to" if end == "from" else "from"]]
        if box["node"]["column"] == 1 and other["node"]["column"] != 1:
            key = ("frame", other["node"]["column"], box["row"])
        else:
            key = ("box", e[end])
        members = groups[key]
        n = len(members)
        offset = (members.index((edge_index, end)) - (n - 1) / 2) * 22
        centre = box["y"] + box["h"] / 2 + offset
        return min(max(centre, box["y"] + 18), box["y"] + box["h"] - 18)

    out = []
    for i, e in enumerate(edges):
        a, b = boxes[e["from"]], boxes[e["to"]]
        ca, cb = a["node"]["column"], b["node"]["column"]
        ya, yb = spread(i, "from"), spread(i, "to")
        if ca == 1 and cb == 1:
            if a["row"] == b["row"] and abs(a["sub"] - b["sub"]) == 1:
                left, right = (a, b) if a["sub"] < b["sub"] else (b, a)
                y = left["y"] + left["h"] / 2
                x1, x2 = (left["x"] + left["w"], right["x"]) if a is left else (right["x"], left["x"] + left["w"])
                d = f"M{x1:g} {y:g} H{x2:g}"
                mid = ((x1 + x2) / 2, y)
            elif a["sub"] == b["sub"] and abs(a["row"] - b["row"]) == 1:
                x = a["x"] + a["w"] / 2
                y1, y2 = (a["y"] + a["h"], b["y"]) if a["row"] < b["row"] else (a["y"], b["y"] + b["h"])
                d = f"M{x:g} {y1:g} V{y2:g}"
                mid = (x, (y1 + y2) / 2)
            else:
                raise SpecError(f"edge {e['from']} -> {e['to']}: boxes of the repository can only be joined when they are next to each other")
            out.append({"d": d, "mid": mid, "span": 16, "e": e, "level": e.get("level", a["node"].get("level", "tested"))})
            continue
        # an edge between a side box and the repository attaches at the frame of the repository
        side = a if ca != 1 else b
        side_is_source = ca != 1
        side_col = side["node"]["column"]
        if side_col == 0:
            xs, xr = side["x"] + side["w"], cont["x"]
        else:
            xs, xr = side["x"], cont["x"] + cont["w"]
        ys = ya if side_is_source else yb
        yr = yb if side_is_source else ya
        if side_is_source:
            x1, y1, x2, y2 = xs, ys, xr, yr
        else:
            x1, y1, x2, y2 = xr, yr, xs, ys
        xm = (x1 + x2) / 2
        d = f"M{x1:g} {y1:g} H{x2:g}" if abs(y1 - y2) < 0.5 else f"M{x1:g} {y1:g} C{xm:g} {y1:g} {xm:g} {y2:g} {x2:g} {y2:g}"
        out.append({"d": d, "mid": (xm, (y1 + y2) / 2), "span": GAP - 8, "e": e, "level": e.get("level", side["node"].get("level", "tested"))})
    return out


def build_svg(spec: dict) -> str:
    pal_all = K.palette(spec["ecosystem"])
    colors = pal_all["colors"]
    lay = layout(spec)
    boxes, cont = lay["boxes"], lay["container"]
    cv = K.Canvas(W, H)
    add = cv.add
    digest = spec_digest(spec)

    header = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-labelledby="t d" '
              f'font-family="{K.SANS}">\n<!-- generated by tools/make_architecture.py from docs/architecture.json ({digest}); do not edit -->\n'
              f'<title id="t">{K.escape(spec["repo"])} architecture</title>\n<desc id="d">{K.escape(spec["description"])}</desc>')
    geo = edge_geometry(spec, lay)
    used_colors = sorted({level_style(g["level"], pal_all)["stroke"] for g in geo} | {level_style(g["level"], pal_all)["title"] for g in geo})
    markers = "".join(f'<marker id="m{c[1:]}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" fill="{c}"/></marker>'
                      for c in used_colors if c != K.BORDER)
    markers += f'<marker id="m{K.BORDER[1:]}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" fill="{K.MUTED}"/></marker>'
    add(f'<defs>{K.common_defs(pal_all, grid=False)}'
        f'<linearGradient id="svc" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{colors[0]}" stop-opacity="0.10"/><stop offset="1" stop-color="{colors[1]}" stop-opacity="0.06"/></linearGradient>'
        f'{markers}</defs>')
    add(f'<rect x="1" y="1" width="{W - 2}" height="{H - 2}" rx="18" fill="url(#bg)" stroke="{K.BORDER}" stroke-width="2"/>')

    # title block
    cv.text(36, 46, spec["repo"], 24, "#FFFFFF", K.SANS, "700", bound=(0, 0, W, 90))
    if spec.get("tagline"):
        cv.text(36, 70, spec["tagline"], 14, K.MUTED, bound=(0, 0, W, 90))

    # column titles
    titles = list(spec.get("columns") or DEFAULT_COLUMNS)
    titles += list(DEFAULT_COLUMNS[len(titles):])
    pos = {0: MARGIN + 6, 1: cont["x"] + 10, 2: W - MARGIN - SIDE_W + 6}
    widths = {0: SIDE_W, 1: cont["w"], 2: SIDE_W}
    for c in (0, 1, 2):
        if c == 1 or c in lay["present"]:
            cv.text(pos[c], 104, titles[c], 12, K.MUTED, K.SANS, "700", spacing=2,
                    bound=(pos[c] - 6 - (0 if c == 1 else 0), 90, widths[c] + 12, 20))

    # the frame of the repository, then every box
    n_inner = sum(1 for n in spec["nodes"] if n["column"] == 1)
    add(f'<rect x="{cont["x"]:g}" y="{cont["y"]:g}" width="{cont["w"]:g}" height="{cont["h"]:g}" rx="16" fill="url(#svc)" stroke="{colors[0]}" stroke-width="2">'
        f'<animate attributeName="stroke-opacity" values="0.55;1;0.55" dur="4s" repeatCount="indefinite"/></rect>')
    inner_index = 0
    for node in spec["nodes"]:
        b = boxes[node["id"]]
        st = level_style(node.get("level", "tested"), pal_all)
        level = node.get("level", "tested")
        x, y, w, h = b["x"], b["y"], b["w"], b["h"]
        bound = (x, y, w, h)
        dash = f' stroke-dasharray="{st["dash"]}"' if st["dash"] else ""
        sw = st["sw"] if node["column"] != 1 else 1
        stroke_op = ' stroke-opacity="0.7"' if node["column"] == 1 and level == "tested" else ""
        anim = ""
        if node["column"] == 1:
            dur = max(6, n_inner)
            anim = (f'<animate attributeName="stroke-opacity" values="0.45;1;0.45;0.45" keyTimes="0;0.08;0.2;1" dur="{dur}s" begin="{inner_index}s" repeatCount="indefinite"/>'
                    f'<animate attributeName="stroke-width" values="1;2.4;1;1" keyTimes="0;0.08;0.2;1" dur="{dur}s" begin="{inner_index}s" repeatCount="indefinite"/>')
            inner_index += 1
        elif level == "config":
            anim = '<animate attributeName="stroke-dashoffset" from="0" to="-24" dur="3s" repeatCount="indefinite"/>'
        stroke = st["stroke"]
        add(f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="{10 if node["column"] == 1 else 12}" fill="{K.BG_CARD}" stroke="{stroke}" stroke-width="{sw}"{dash}{stroke_op}>{anim}</rect>')
        size = node.get("title_size") or (14 if node["column"] == 1 else 15)
        cv.text(x + 16, y + 26, node["title"], size, st["title"], K.SANS, "700", bound=bound)
        lines = node_lines(node)
        for i, (line, tone) in enumerate(lines):
            cv.text(x + 16, y + 48 + 18 * i, line, 12, tone_color(tone, pal_all), bound=bound)
        if level == "untested":
            add(f'<rect x="{x + 16:g}" y="{y + h - 14:g}" width="{w - 32:g}" height="3" rx="1.5" fill="{K.RED}" opacity="0.6"><animate attributeName="opacity" values="0.25;0.85;0.25" dur="2.8s" repeatCount="indefinite"/></rect>')

    # arrows (static line + flowing dashes + one travelling dot), labels last so they stay readable
    for i, g in enumerate(geo):
        st = level_style(g["level"], pal_all)
        color = st["stroke"] if g["level"] not in ("external", "untested") else K.MUTED
        marker = f'm{(st["stroke"] if g["level"] not in ("external", "untested") else K.BORDER)[1:]}'
        dash = ' stroke-dasharray="6 4"' if g["level"] == "config" else ""
        add(f'<path id="p{i}" d="{g["d"]}" stroke="{color}" stroke-width="2" fill="none"{dash} marker-end="url(#{marker})"/>')
        add(f'<path d="{g["d"]}" stroke="{color}" stroke-width="2" fill="none" stroke-linecap="round" stroke-dasharray="4 8" opacity="0.95">'
            f'<animate attributeName="stroke-dashoffset" from="0" to="-24" dur="1.2s" repeatCount="indefinite"/></path>')
        begin = f"{0.35 * i:.2f}s"
        add(f'<circle r="3.2" fill="{color}"><animateMotion dur="1.8s" begin="{begin}" repeatCount="indefinite"><mpath href="#p{i}"/></animateMotion>'
            f'<animate attributeName="opacity" values="0;1;1;0" keyTimes="0;0.15;0.85;1" dur="1.8s" begin="{begin}" repeatCount="indefinite"/></circle>')
    for g in geo:
        label = g["e"].get("label")
        if not label:
            continue
        st = level_style(g["level"], pal_all)
        color = st["stroke"] if g["level"] not in ("external", "untested") else K.MUTED
        try:
            lines = K.wrap(label, 10.5, g["span"] - 8)
        except ValueError as error:
            raise SpecError(f"edge label {error}") from None
        mx, my = g["mid"]
        pill_w = max(K.text_width(x, 10.5) for x in lines) + 10
        pill_h = 6 + 12.5 * len(lines)
        add(f'<rect x="{mx - pill_w / 2:g}" y="{my - pill_h / 2:g}" width="{pill_w:g}" height="{pill_h:g}" rx="4" fill="#0D1117" opacity="0.92"/>')
        for j, ln in enumerate(lines):
            cv.text(mx, my - pill_h / 2 + 12 + 12.5 * j - 1, ln, 10.5, color, K.SANS, "400", anchor="middle")

    # legend and footer
    used = [lv for lv in LEVELS if any(n.get("level", "tested") == lv for n in spec["nodes"])]
    texts = {**DEFAULT_LEGEND, **spec.get("legend", {})}
    lx, ly = 36, 560
    for lv in used:
        st = level_style(lv, pal_all)
        label = texts[lv]
        need = 22 + K.text_width(label, 12) + 34
        if lx + need > W - 40:
            lx, ly = 36, ly + 20
        if lv == "untested":
            add(f'<rect x="{lx}" y="{ly - 12}" width="14" height="14" rx="3" fill="{K.RED}" opacity="0.7"/>')
        else:
            dash = ' stroke-dasharray="3 2"' if lv == "config" else ""
            stroke = st["stroke"]
            add(f'<rect x="{lx}" y="{ly - 12}" width="14" height="14" rx="3" fill="none" stroke="{stroke}" stroke-width="2"{dash}/>')
        cv.text(lx + 22, ly, label, 12, K.INK)
        lx += need
    if spec.get("footer"):
        cv.text(36, 612 if ly > 560 else 604, spec["footer"], 12, K.FAINT)
    return cv.render(header)


# --------------------------------------------------------------------------- the check
def check_code_refs(spec: dict, root: Path) -> list[str]:
    problems = []
    for node in spec["nodes"]:
        for ref in node.get("code", []):
            path_part, _, needle = ref.partition("::")
            target = root / path_part
            if not target.exists():
                problems.append(f"node '{node['id']}' points to {path_part}, which does not exist")
                continue
            if needle and target.is_dir():
                problems.append(f"node '{node['id']}': {path_part} is a folder, it cannot contain '{needle}'")
                continue
            if needle and needle not in target.read_text(encoding="utf-8", errors="replace"):
                problems.append(f"node '{node['id']}': {path_part} no longer contains '{needle}'")
    return problems


def run(root: Path, check: bool, png: Path | None) -> int:
    spec_file = root / "docs" / "architecture.json"
    out_file = root / "images" / "architecture.svg"
    try:
        spec = load_spec(spec_file)
        svg = build_svg(spec)
        K.validate_xml(svg)
    except (SpecError, ValueError) as error:
        print(f"ARCHITECTURE=FAIL {error}")
        return 1
    if check:
        problems = check_code_refs(spec, root)
        if not out_file.is_file():
            problems.append(f"{out_file.relative_to(root)} does not exist (run tools/make_architecture.py)")
        elif K.normalise(out_file.read_text(encoding="utf-8")) != svg:
            problems.append(f"{out_file.relative_to(root)} does not match docs/architecture.json (run tools/make_architecture.py)")
        if problems:
            print("ARCHITECTURE=FAIL " + "; ".join(problems))
            return 1
        print(f"ARCHITECTURE=PASS {spec['repo']} ({len(spec['nodes'])} boxes)")
    else:
        out_file.parent.mkdir(parents=True, exist_ok=True)
        out_file.write_text(svg, encoding="utf-8", newline="\n")
        print(f"ARCHITECTURE=WRITTEN {out_file} ({len(svg)} bytes)")
    if png:
        shot = K.render_png(out_file if out_file.is_file() else _tmp_svg(svg), png, W, H)
        print(f"PNG={'written ' + str(png) if shot else 'skipped (Edge not available)'}")
    return 0


def _tmp_svg(svg: str) -> Path:
    import tempfile
    handle = Path(tempfile.mkdtemp(prefix="arch_")) / "architecture.svg"
    handle.write_text(svg, encoding="utf-8")
    return handle


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Draws images/architecture.svg from docs/architecture.json")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1], help="the repository (default: the one this file is in)")
    parser.add_argument("--check", action="store_true", help="fail when the picture, the JSON or the code it points to disagree")
    parser.add_argument("--png", type=Path, help="also screenshot the picture with Edge")
    args = parser.parse_args(argv)
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    return run(args.root.resolve(), args.check, args.png)


if __name__ == "__main__":
    raise SystemExit(main())
