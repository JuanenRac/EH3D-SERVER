#!/usr/bin/env python3
# =============================================================================
# A.R.M.O.R. - tools/_readme_svg.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D)
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""The drawing kit shared by make_architecture.py and make_structure.py.

It holds what both generators need and nothing else: the palettes of the four
families, an estimate of how wide a line of text is, a canvas that remembers
where every text was put and refuses a picture in which a text leaves its card
or lies on top of another text, an XML check, and a screenshot of the picture
made with Edge (a private profile, so a window of the person is never touched).

This file is the canonical copy in ARMOR-COMMON; the other repositories carry a
byte-for-byte copy that `sync_ci_tools.py --check` keeps from drifting.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from xml.sax.saxutils import escape

SANS = "'Segoe UI','Noto Sans','Noto Sans CJK SC','Microsoft YaHei','Yu Gothic','Helvetica Neue',Helvetica,Arial,sans-serif"
MONO = "'JetBrains Mono','Cascadia Code','SFMono-Regular',Consolas,'Liberation Mono',monospace"

# Semantic colours, the same in every family.
AMBER = "#FFB020"      # configured by the installation, or not tried for real
RED = "#FF6B6B"        # has not run on real hardware
INK = "#C9D1D9"
MUTED = "#8B949E"
FAINT = "#6E7681"
BG_CARD = "#161B22"
BORDER = "#30363D"

# (first, second, third, fourth) colours of each family. `title` is the gradient of the title rule.
PALETTES: dict[str, dict] = {
    "armor": {"colors": ("#00FF88", "#00E5FF", "#3D9BFF", "#9AA7FF"), "title": ("#00FF88", "#00E5FF")},
    "hydra": {"colors": ("#00E5FF", "#0088FF", "#FF9900", "#9AA7FF"), "title": ("#00E5FF", "#0088FF")},
    "urtc": {"colors": ("#00E5FF", "#0088FF", "#FF9900", "#9AA7FF"), "title": ("#00E5FF", "#FF9900")},
    "eh3d": {"colors": ("#00E5FF", "#4DA3FF", "#FFB300", "#FF5C8A"), "title": ("#00E5FF", "#FF0055")},
}

LANGUAGES = ("en", "es", "de", "fr", "it", "ja", "zh", "pt")
README_NAME = {"en": "README.md", "es": "README_spa.md", "de": "README_deu.md", "fr": "README_fra.md",
               "it": "README_ita.md", "ja": "README_jpn.md", "zh": "README_zho.md", "pt": "README_por.md"}
FILE_CODE = {"en": "", "es": "spa", "de": "deu", "fr": "fra", "it": "ita", "ja": "jpn", "zh": "zho", "pt": "por"}

EDGE_PATHS = (
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
)


def palette(name: str) -> dict:
    if name not in PALETTES:
        raise ValueError(f"unknown ecosystem '{name}' (expected one of {', '.join(PALETTES)})")
    return PALETTES[name]


def _char_em(ch: str, mono: bool, bold: bool) -> float:
    code = ord(ch)
    if code >= 0x2E80:                    # CJK, kana, full-width forms
        return 1.0
    if mono:
        return 0.62
    if ch in "il.,:;'|!` ":
        width = 0.30
    elif ch in "mwMW@":
        width = 0.86
    elif ch.isupper():
        width = 0.68
    elif ch.isdigit():
        width = 0.58
    else:
        width = 0.54
    return width + (0.05 if bold else 0.0)


def text_width(s: str, size: float, family: str = SANS, weight: str = "400", spacing: float = 0.0) -> float:
    """A cautious estimate (slightly wide on purpose) of the width in pixels of `s`."""
    mono = family.startswith("'JetBrains") or "Consolas" in family[:60]
    bold = str(weight) in ("600", "700", "800", "bold")
    return sum(_char_em(c, mono, bold) for c in s) * size + spacing * len(s)


def wrap(s: str, size: float, max_width: float, family: str = SANS, weight: str = "400", lines: int = 2) -> list[str]:
    """Breaks `s` at spaces into at most `lines` lines no wider than `max_width`; raises when it cannot."""
    words, out, current = s.split(), [], ""
    for word in words:
        trial = (current + " " + word).strip()
        if text_width(trial, size, family, weight) <= max_width or not current:
            current = trial
        else:
            out.append(current)
            current = word
    if current:
        out.append(current)
    if len(out) > lines or any(text_width(x, size, family, weight) > max_width for x in out):
        raise ValueError(f"'{s}' does not fit in {max_width:.0f}px on {lines} line(s)")
    return out


class Canvas:
    """Collects SVG pieces and the box of every text, and checks them before the picture is written."""

    def __init__(self, width: int, height: int):
        self.width, self.height = width, height
        self.parts: list[str] = []
        self.boxes: list[tuple[str, float, float, float, float, tuple[float, float, float, float] | None]] = []

    def add(self, piece: str) -> None:
        self.parts.append(piece)

    def text(self, x, y, s, size, fill, family=SANS, weight="400", anchor="start", spacing=0.0,
             opacity=None, bound=None, tag="") -> None:
        """Puts a text. `bound` is the (x, y, w, h) of the card the text has to stay inside."""
        if not s:
            return
        width = text_width(s, size, family, weight, spacing)
        if anchor == "end":
            left, right = x - width, x
        elif anchor == "middle":
            left, right = x - width / 2, x + width / 2
        else:
            left, right = x, x + width
        self.boxes.append((s, left, y - 0.78 * size, right, y + 0.22 * size, bound))
        extra = f' letter-spacing="{spacing}"' if spacing else ""
        extra += f' opacity="{opacity}"' if opacity else ""
        extra += f' data-tag="{tag}"' if tag else ""
        self.add(f'<text x="{x:g}" y="{y:g}" font-family="{family}" font-size="{size:g}" font-weight="{weight}" '
                 f'fill="{fill}" text-anchor="{anchor}"{extra}>{escape(s)}</text>')

    def problems(self, margin: float = 24.0) -> list[str]:
        found: list[str] = []
        for s, left, top, right, bottom, bound in self.boxes:
            if left < margin or right > self.width - margin or top < 0 or bottom > self.height:
                found.append(f"'{s}' leaves the picture ({left:.0f}..{right:.0f})")
            if bound is not None:
                bx, by, bw, bh = bound
                if left < bx + 6 or right > bx + bw - 6 or top < by or bottom > by + bh:
                    found.append(f"'{s}' leaves its card ({left:.0f}..{right:.0f} against {bx:.0f}..{bx + bw:.0f})")
        for i, (s1, l1, t1, r1, b1, _) in enumerate(self.boxes):
            for s2, l2, t2, r2, b2, _ in self.boxes[i + 1:]:
                if l1 < r2 - 0.5 and l2 < r1 - 0.5 and t1 < b2 - 0.5 and t2 < b1 - 0.5:
                    found.append(f"'{s1}' and '{s2}' overlap")
        return found

    def render(self, header: str, footer: str = "</svg>") -> str:
        bad = self.problems()
        if bad:
            raise ValueError("the picture does not fit:\n  " + "\n  ".join(bad[:12]) + (f"\n  ... {len(bad) - 12} more" if len(bad) > 12 else ""))
        return "\n".join([header, *self.parts, footer]) + "\n"


def common_defs(pal: dict, grid: bool = True) -> str:
    first, second = pal["title"]
    c0 = pal["colors"][0]
    out = (
        '<linearGradient id="bg" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#0A0C10"/>'
        '<stop offset="0.5" stop-color="#0D1117"/><stop offset="1" stop-color="#05070A"/></linearGradient>'
        '<linearGradient id="card" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#121821"/>'
        '<stop offset="1" stop-color="#0B0F15"/></linearGradient>'
        f'<linearGradient id="line" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="{first}"/><stop offset="1" stop-color="{second}"/></linearGradient>'
        '<filter id="glow" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="3" result="b"/>'
        '<feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>'
    )
    if grid:
        out += (f'<pattern id="grid" width="40" height="40" patternUnits="userSpaceOnUse"><path d="M40 0H0V40" fill="none" stroke="{c0}" stroke-opacity="0.035"/>'
                f'<circle cx="40" cy="40" r="1" fill="{c0}" fill-opacity="0.09"/></pattern>')
    return out


def validate_xml(svg: str) -> ET.Element:
    """Parses the picture; raises ValueError when it is not well-formed XML or lacks title and desc."""
    try:
        root = ET.fromstring(svg)
    except ET.ParseError as error:
        raise ValueError(f"the SVG is not valid XML: {error}") from None
    ns = "{http://www.w3.org/2000/svg}"
    if root.tag != ns + "svg":
        raise ValueError("the root element is not <svg>")
    if root.find(ns + "title") is None or root.find(ns + "desc") is None:
        raise ValueError("the SVG needs a <title> and a <desc>")
    if "viewBox" not in root.attrib:
        raise ValueError("the SVG needs a viewBox")
    return root


def find_edge() -> str | None:
    override = os.environ.get("EDGE_PATH")
    for candidate in ([override] if override else []) + list(EDGE_PATHS):
        if candidate and Path(candidate).is_file():
            return candidate
    return shutil.which("msedge")


def render_png(svg_path: Path, png_path: Path, width: int, height: int, budget_ms: int = 4000) -> bool:
    """Screenshots the SVG with headless Edge in a private profile. Returns False when Edge is not there.
    It never touches an Edge window that is already open (a throw-away user data directory is used)."""
    edge = find_edge()
    if not edge:
        return False
    png_path.parent.mkdir(parents=True, exist_ok=True)
    if png_path.exists():
        png_path.unlink()
    with tempfile.TemporaryDirectory(prefix="readme_svg_edge_", ignore_cleanup_errors=True) as profile:
        command = [edge, "--headless", "--disable-gpu", "--no-first-run", f"--user-data-dir={profile}",
                   f"--window-size={width},{height}", "--hide-scrollbars", f"--virtual-time-budget={budget_ms}",
                   f"--screenshot={png_path}", svg_path.resolve().as_uri()]
        try:
            subprocess.run(command, capture_output=True, timeout=90, check=False)
        except (OSError, subprocess.TimeoutExpired):
            return False
        # Edge may hand the work to a helper process and return first: wait for the file to appear and stop growing
        previous = -1
        for _ in range(60):
            if png_path.is_file() and png_path.stat().st_size == previous and previous > 0:
                break
            previous = png_path.stat().st_size if png_path.is_file() else -1
            time.sleep(0.5)
    return png_path.is_file() and png_path.stat().st_size > 0


def normalise(text: str) -> str:
    return text.replace("\r\n", "\n")
