#!/usr/bin/env python3
# =============================================================================
# universal_readme_check.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D)
# GPL-3.0-or-later - see LICENSE
# =============================================================================
# CANONICAL in ARMOR-COMMON/tools. Vendored byte-for-byte into the repositories of the four families (A.R.M.O.R., URTC, HYDRA-UMC, EH3D) by the sync script of each
# family, next to its ci_validate. Edit the rule here and run the sync again.
"""Checks that the README of a repository follows the universal layout of the four families.

The layout is 22 sections with a fixed name, a fixed icon and a fixed order, in the eight languages (en, es, fr, it, de, zh, ja, pt). Fourteen of them are required in
every repository; the others appear when the repository has the content. What is checked, for each of the eight README files:

* the eight files exist and have the same sequence of sections (and the same number of sub-sections in each);
* every section is one of the 22, is not repeated, appears in the fixed order and carries its fixed name for that language;
* the 14 required sections are there, At a glance first, Project status second, and AUTHOR and LICENSE last;
* the header: banner, title, language bar with the eight files, one-line description, badges and the honesty paragraph; the "Jump to" line under At a glance;
* the traffic lights of Project status; an Architecture section with a diagram;
* no dates, no mention of the version rule, no reference to private documents, tools or people;
* the number of tests in the badge and in At a glance agree, and with --run they are the number that the tests really report.

    python tools/universal_readme_check.py                # the repository this file lives in (as a warning list)
    python tools/universal_readme_check.py --repo PATH    # another repository
    python tools/universal_readme_check.py --strict       # exit 1 when something is wrong (a family switches to this when it has finished migrating)
    python tools/universal_readme_check.py --run          # also run the tests and compare their number with the README

From a CI validator: `check_repository(root)` returns the list of problems and `report(problems, strict)` prints the verdict line (`UNIVERSAL_README=PASS`,
`UNIVERSAL_README=WARN <n>` or `UNIVERSAL_README=FAIL <n>`) and returns the exit code to use.
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

LANGUAGE_FILES = [("en", "README.md"), ("es", "README_spa.md"), ("fr", "README_fra.md"), ("it", "README_ita.md"),
                  ("de", "README_deu.md"), ("zh", "README_zho.md"), ("ja", "README_jpn.md"), ("pt", "README_por.md")]

# the 22 sections in their fixed order, with the icon that identifies each one in every language
SECTIONS = [
    ("glance", "📋"), ("status", "🚦"), ("overview", "🎯"), ("role", "🧩"), ("architecture", "🏛"), ("screenshots", "🖼"), ("features", "✨"),
    ("security", "🔒"), ("interfaces", "🔌"), ("configuration", "⚙"), ("quickstart", "🚀"), ("testing", "🧪"), ("build", "🏗"),
    ("troubleshooting", "🩺"), ("structure", "📂"), ("roadmap", "🗺"), ("languages", "🌍"), ("related", "🔗"), ("documentation", "📚"),
    ("contributing", "🤝"), ("author", "👤"), ("license", "📜"),
]
ORDER = [section for section, _ in SECTIONS]
ID_OF_ICON = {icon: section for section, icon in SECTIONS}
# required in every repository: the rest depend on what the repository is (a server has interfaces, a library has no screenshots...)
REQUIRED = ["glance", "status", "overview", "role", "architecture", "quickstart", "structure", "roadmap", "languages", "related", "documentation", "contributing", "author", "license"]

# The name of every section in each language (the same ones that the README generators write; the role section also carries the name of the ecosystem).
NAMES = {
    "en": {
        "glance": "At a glance",
        "status": "Project status",
        "overview": "Overview",
        "role": "Role in A.R.M.O.R.",
        "architecture": "Architecture",
        "screenshots": "Screenshots",
        "features": "Features",
        "security": "Security & Safety",
        "interfaces": "Interfaces",
        "configuration": "Configuration",
        "quickstart": "Quick start",
        "testing": "Testing",
        "build": "Build & Release",
        "troubleshooting": "Troubleshooting",
        "structure": "Repository Structure",
        "roadmap": "Roadmap & Known limits",
        "languages": "Languages",
        "related": "Related Projects",
        "documentation": "Documentation",
        "contributing": "Contributing & Community",
        "author": "AUTHOR",
        "license": "LICENSE",
    },
    "es": {
        "glance": "De un vistazo",
        "status": "Estado del proyecto",
        "overview": "Resumen",
        "role": "Papel en A.R.M.O.R.",
        "architecture": "Arquitectura",
        "screenshots": "Capturas de pantalla",
        "features": "Funciones",
        "security": "Seguridad y protección",
        "interfaces": "Interfaces",
        "configuration": "Configuración",
        "quickstart": "Inicio rápido",
        "testing": "Pruebas",
        "build": "Compilación y publicación",
        "troubleshooting": "Resolución de problemas",
        "structure": "Estructura del repositorio",
        "roadmap": "Hoja de ruta y límites conocidos",
        "languages": "Idiomas",
        "related": "Proyectos relacionados",
        "documentation": "Documentación",
        "contributing": "Contribuir y comunidad",
        "author": "AUTOR",
        "license": "LICENCIA",
    },
    "fr": {
        "glance": "En un coup d'œil",
        "status": "État du projet",
        "overview": "Présentation",
        "role": "Rôle dans A.R.M.O.R.",
        "architecture": "Architecture",
        "screenshots": "Captures d'écran",
        "features": "Fonctionnalités",
        "security": "Sécurité et protection",
        "interfaces": "Interfaces",
        "configuration": "Configuration",
        "quickstart": "Démarrage rapide",
        "testing": "Tests",
        "build": "Compilation et publication",
        "troubleshooting": "Dépannage",
        "structure": "Structure du dépôt",
        "roadmap": "Feuille de route et limites connues",
        "languages": "Langues",
        "related": "Projets liés",
        "documentation": "Documentation",
        "contributing": "Contribuer et communauté",
        "author": "AUTEUR",
        "license": "LICENCE",
    },
    "it": {
        "glance": "In breve",
        "status": "Stato del progetto",
        "overview": "Panoramica",
        "role": "Ruolo in A.R.M.O.R.",
        "architecture": "Architettura",
        "screenshots": "Schermate",
        "features": "Funzionalità",
        "security": "Sicurezza e protezione",
        "interfaces": "Interfacce",
        "configuration": "Configurazione",
        "quickstart": "Avvio rapido",
        "testing": "Test",
        "build": "Compilazione e rilascio",
        "troubleshooting": "Risoluzione dei problemi",
        "structure": "Struttura del repository",
        "roadmap": "Roadmap e limiti noti",
        "languages": "Lingue",
        "related": "Progetti correlati",
        "documentation": "Documentazione",
        "contributing": "Contribuire e comunità",
        "author": "AUTORE",
        "license": "LICENZA",
    },
    "de": {
        "glance": "Auf einen Blick",
        "status": "Projektstatus",
        "overview": "Überblick",
        "role": "Rolle in A.R.M.O.R.",
        "architecture": "Architektur",
        "screenshots": "Screenshots",
        "features": "Funktionen",
        "security": "Sicherheit und Schutz",
        "interfaces": "Schnittstellen",
        "configuration": "Konfiguration",
        "quickstart": "Schnellstart",
        "testing": "Tests",
        "build": "Build und Veröffentlichung",
        "troubleshooting": "Fehlerbehebung",
        "structure": "Struktur des Repositorys",
        "roadmap": "Roadmap und bekannte Grenzen",
        "languages": "Sprachen",
        "related": "Verwandte Projekte",
        "documentation": "Dokumentation",
        "contributing": "Mitwirken und Community",
        "author": "AUTOR",
        "license": "LIZENZ",
    },
    "zh": {
        "glance": "速览",
        "status": "项目状态",
        "overview": "概述",
        "role": "在 A.R.M.O.R. 中的角色",
        "architecture": "架构",
        "screenshots": "屏幕截图",
        "features": "功能",
        "security": "安全与防护",
        "interfaces": "接口",
        "configuration": "配置",
        "quickstart": "快速开始",
        "testing": "测试",
        "build": "构建与发布",
        "troubleshooting": "故障排除",
        "structure": "仓库结构",
        "roadmap": "路线图与已知限制",
        "languages": "语言",
        "related": "相关项目",
        "documentation": "文档",
        "contributing": "参与贡献与社区",
        "author": "作者",
        "license": "许可证",
    },
    "ja": {
        "glance": "ひと目で",
        "status": "プロジェクトの状況",
        "overview": "概要",
        "role": "A.R.M.O.R. での役割",
        "architecture": "アーキテクチャ",
        "screenshots": "スクリーンショット",
        "features": "機能",
        "security": "セキュリティと安全",
        "interfaces": "インターフェース",
        "configuration": "設定",
        "quickstart": "クイックスタート",
        "testing": "テスト",
        "build": "ビルドとリリース",
        "troubleshooting": "トラブルシューティング",
        "structure": "リポジトリの構成",
        "roadmap": "ロードマップと既知の制限",
        "languages": "言語",
        "related": "関連プロジェクト",
        "documentation": "ドキュメント",
        "contributing": "コントリビュートとコミュニティ",
        "author": "作者",
        "license": "ライセンス",
    },
    "pt": {
        "glance": "Num relance",
        "status": "Estado do projeto",
        "overview": "Visão geral",
        "role": "Papel no A.R.M.O.R.",
        "architecture": "Arquitetura",
        "screenshots": "Capturas de ecrã",
        "features": "Funcionalidades",
        "security": "Segurança e proteção",
        "interfaces": "Interfaces",
        "configuration": "Configuração",
        "quickstart": "Início rápido",
        "testing": "Testes",
        "build": "Compilação e publicação",
        "troubleshooting": "Resolução de problemas",
        "structure": "Estrutura do repositório",
        "roadmap": "Roteiro e limites conhecidos",
        "languages": "Idiomas",
        "related": "Projetos relacionados",
        "documentation": "Documentação",
        "contributing": "Contribuir e comunidade",
        "author": "AUTOR",
        "license": "LICENÇA",
    },
}

LIGHTS = ("🟢", "🟡", "🟠", "🔴")
# things that must not be in a public README
# (written in pieces so that this file does not itself contain the words it looks for)
_PRIVATE_WORDS = ["SON" + "NET", "AUDI" + "TORIA", "HAND" + "OFF", "PENDIEN" + "TES", "RECUEN" + "TO", "CIERRE" + "_", "_pape" + "lera", "Co-Auth" + "ored",
                  "Generated" + " with", r"\bCla" + r"ude\b", "Anthro" + "pic"]
PRIVATE = re.compile("|".join(_PRIVATE_WORDS) + r"|\b[A-Z]{3,6}-(?!SHA|AES|RSA|GCM|ECC|EC\b|CBC|CTR|MD5)[A-Z]{2,6}-\d{3}\b")
VERSION_RULE = re.compile("odo" + "meter|od[oó]" + "metro|od[oó]m" + "[eè]tre|Kilometer" + "z[aä]hler|オドメー" + "ター|里程" + "表", re.IGNORECASE)
MONTHS = r"(?:Jan(?:uary)?|Feb(?:ruary)?|Mar(?:ch)?|Apr(?:il)?|May|Jun(?:e)?|Jul(?:y)?|Aug(?:ust)?|Sep(?:t(?:ember)?)?|Oct(?:ober)?|Nov(?:ember)?|Dec(?:ember)?)"
DATE = re.compile(rf"\b20[2-3]\d-[01]\d-[0-3]\d\b|\b[0-3]?\d[./][01]?\d[./]20[2-3]\d\b|\b{MONTHS}\.? [0-3]?\d,? 20[2-3]\d\b|\b[0-3]?\d {MONTHS}\.? 20[2-3]\d\b")
NUMBERED = re.compile(r"^\d+\.\s")
FENCE = re.compile(r"^\s*```")


def clean_icon(text: str) -> str:
    return text.replace("️", "")


class Readme:
    """A README split into its header and its level-two sections (code blocks are never read as headings)."""

    def __init__(self, text: str):
        self.lines = text.replace("\r\n", "\n").split("\n")
        self.prose: list[tuple[int, str]] = []   # (line number, line) outside code blocks
        self.sections: list[dict[str, Any]] = []
        fence = False
        current: dict[str, Any] | None = None
        self.header: list[str] = []
        for number, line in enumerate(self.lines, 1):
            if FENCE.match(line):
                fence = not fence
                if current is not None:
                    current["lines"].append(line)
                else:
                    self.header.append(line)
                continue
            if not fence:
                self.prose.append((number, line))
                if line.startswith("## "):
                    heading = line[3:].strip()
                    parts = heading.split(" ", 1)
                    icon = clean_icon(parts[0])
                    current = {"line": number, "heading": heading, "icon": icon, "title": parts[1].strip() if len(parts) > 1 else "",
                               "id": ID_OF_ICON.get(icon), "lines": [], "h3": 0}
                    self.sections.append(current)
                    continue
                if current is not None and line.startswith("### "):
                    current["h3"] += 1
            if current is not None:
                current["lines"].append(line)
            else:
                self.header.append(line)

    def section(self, section_id: str):
        return next((s for s in self.sections if s["id"] == section_id), None)


def tests_in_readme(readme: Readme) -> set[int]:
    """The test count that the badge and the At a glance table state."""
    found: set[int] = set()
    for line in readme.header:
        match = re.search(r"badge/Tests-(\d+)", line)
        if match:
            found.add(int(match.group(1)))
    glance = readme.section("glance")
    if glance:
        for line in glance["lines"]:
            if line.startswith("|") and re.match(r"\|\s*(Tests|Pruebas|Test|Testes|Tests|测试|テスト)\s*\|", line):
                match = re.search(r"(\d[\d,.]*\d|\d)", line.split("|", 2)[2])
                if match:
                    found.add(int(re.sub(r"[,.]", "", match.group(1))))
    return found


def check_file(name: str, language: str, readme: Readme, english: Readme | None) -> list[str]:
    problems: list[str] = []

    def add(message: str) -> None:
        problems.append(f"{name}: {message}")

    top = "\n".join(readme.lines[:12])
    if not re.search(r"(?is)<img[^>]*banner", top):
        add("no banner image at the top")
    if not any(line.startswith("# ") for line in readme.header):
        add("no title (a single '# ' line)")
    bar = "\n".join(readme.lines[:45])
    missing_files = [file for _, file in LANGUAGE_FILES if file != name and file not in bar]
    if missing_files:
        add("the language bar does not link " + ", ".join(missing_files))
    header_text = "\n".join(readme.header)
    if not re.search(r"(?m)^### \S", header_text):
        add("no one-line description (### line) under the language bar")
    if "img.shields.io" not in header_text:
        add("no badges under the description")
    if len(re.findall(r"(?m)^---\s*$", header_text)) < 2:
        add("no honesty paragraph between two '---' lines before the first section")

    ids = [s["id"] for s in readme.sections]
    for s in readme.sections:
        if s["id"] is None:
            add(f"line {s['line']}: '{s['heading']}' is not one of the 22 sections (unknown icon)")
        if NUMBERED.match(s["title"]):
            add(f"line {s['line']}: the section '{s['heading']}' is numbered")
    known = [i for i in ids if i is not None]
    for section in set(known):
        if known.count(section) > 1:
            add(f"the section '{NAMES['en'][section]}' appears more than once")
    positions = [ORDER.index(i) for i in known]
    if positions != sorted(positions):
        add("the sections are out of order; the order is: " + ", ".join(NAMES["en"][i] for i in ORDER))
    for section in REQUIRED:
        if section not in ids:
            add(f"required section missing: {NAMES['en'][section]}")
    if ids[:1] != ["glance"]:
        add("At a glance must be the first section")
    if ids[1:2] != ["status"]:
        add("Project status must be the second section")
    if ids[-2:] != ["author", "license"]:
        add("AUTHOR and LICENSE must be the last two sections")
    for s in readme.sections:
        if s["id"] is None:
            continue
        expected = NAMES[language][s["id"]]
        if s["id"] == "role":
            if language == "en" and not s["title"].startswith("Role in "):
                add(f"line {s['line']}: the name of the role section must be 'Role in <ecosystem>'")
        elif s["title"] != expected:
            add(f"line {s['line']}: the section is called '{s['title']}', it must be '{expected}'")

    glance = readme.section("glance")
    if glance and not any("](#" in line and "·" in line for line in glance["lines"]):
        add("At a glance has no 'Jump to' line with links to the sections")
    status = readme.section("status")
    if status and not any(line.startswith("|") and any(light in line for light in LIGHTS) for line in status["lines"]):
        add("Project status has no table with traffic lights")
    architecture = readme.section("architecture")
    if architecture:
        body = "\n".join(architecture["lines"])
        if not re.search(r"<img[^>]*architecture|!\[[^\]]*\]\([^)]*architecture", body, re.I):
            add("Architecture has no diagram image (images/architecture.svg)")

    for number, line in readme.prose:
        if VERSION_RULE.search(line):
            add(f"line {number}: it mentions the version rule")
        private = PRIVATE.search(line)
        if private:
            add(f"line {number}: it refers to a private document, tool or person: {private.group(0)}")
        dated = DATE.search(line)
        if dated:
            add(f"line {number}: it has a date: {dated.group(0)}")

    counts = tests_in_readme(readme)
    if len(counts) > 1:
        add(f"the number of tests differs between the badge and At a glance: {sorted(counts)}")

    if english is not None and english is not readme:
        mine = [s["icon"] for s in readme.sections]
        theirs = [s["icon"] for s in english.sections]
        if mine != theirs:
            add("the sequence of sections differs from README.md")
        else:
            for a, b in zip(readme.sections, english.sections):
                if a["h3"] != b["h3"]:
                    add(f"the section '{a['heading']}' has {a['h3']} sub-sections, README.md has {b['h3']}")
    return problems


def run_tests(root: Path) -> int | None:
    """The number of tests that the repository's own suite reports, or None when it cannot be found out here."""
    environment = dict(os.environ, PYTHONPATH=os.pathsep.join(str(p) for p in (root / "src", root) if p.exists()))
    candidates = []
    if (root / "tests").is_dir():
        candidates.append(([sys.executable, "-m", "unittest", "discover", "-s", "tests"], r"Ran (\d+) tests?"))
        candidates.append(([sys.executable, "-m", "pytest", "-q", "tests"], r"(\d+) passed"))
    for command, pattern in candidates:
        try:
            result = subprocess.run(command, cwd=root, env=environment, capture_output=True, text=True, timeout=1800)
        except (OSError, subprocess.TimeoutExpired):
            continue
        match = re.search(pattern, result.stderr + result.stdout)
        if match and int(match.group(1)) > 0:
            return int(match.group(1))
    return None


def check_repository(root: Path, run: bool = False) -> list[str]:
    """Every problem of the eight READMEs of the repository at `root` (an empty list when the layout is followed)."""
    problems: list[str] = []
    parsed: dict[str, Readme] = {}
    for language, file in LANGUAGE_FILES:
        path = root / file
        if not path.is_file():
            problems.append(f"{file}: missing")
            continue
        parsed[language] = Readme(path.read_text(encoding="utf-8", errors="replace"))
    english = parsed.get("en")
    for language, file in LANGUAGE_FILES:
        if language in parsed:
            problems += check_file(file, language, parsed[language], english)
    if run and english is not None:
        ran = run_tests(root)
        stated = tests_in_readme(english)
        if ran is None:
            problems.append("--run: the tests could not be run here, so the README count was not compared")
        elif stated and stated != {ran}:
            problems.append(f"README.md states {sorted(stated)} tests and the suite ran {ran}")
    return problems


def report(problems: list[str], strict: bool = False, limit: int = 12, stream=None) -> int:
    """Prints the verdict line and the first problems; returns the exit code (1 only when `strict` and something is wrong)."""
    stream = stream or sys.stdout

    def emit(text: str) -> None:
        try:
            print(text, file=stream)
        except UnicodeEncodeError:   # a console that cannot show the icons of the headings
            print(text.encode("ascii", "replace").decode("ascii"), file=stream)

    if not problems:
        emit("UNIVERSAL_README=PASS")
        return 0
    emit(f"UNIVERSAL_README={'FAIL' if strict else 'WARN'} {len(problems)}")
    for problem in problems[:limit]:
        emit(f"  universal_readme: {problem}")
    if len(problems) > limit:
        emit(f"  universal_readme: ... and {len(problems) - limit} more (run tools/universal_readme_check.py for all)")
    return 1 if strict else 0


def strict_requested(default: bool = False) -> bool:
    """A family turns the check into a failure by setting its own constant to True; the variable UNIVERSAL_README_STRICT=1/0 overrides it for one run."""
    value = os.environ.get("UNIVERSAL_README_STRICT", "").strip()
    return default if value == "" else value not in ("0", "false", "no")


# Whether the check fails (True) or only warns (False) in the CI of each family. A family changes to True, in this one line, when all of its repositories follow the layout.
FAMILY_STRICT = {"ARMOR": False, "URTC": False, "HYDRA": False, "EH3D": False}


def family_of(root: Path) -> str:
    name = root.name.upper()
    for prefix, family in (("ARMOR", "ARMOR"), ("URTC", "URTC"), ("HYDRA", "HYDRA"), ("EH3D", "EH3D"), ("ELECTRO-HOBBY", "EH3D")):
        if name.startswith(prefix):
            return family
    return ""


def run_in_ci(root: Path) -> list[str]:
    """What a ci_validate calls: prints the verdict and returns the reasons to fail (none while the family only warns). A fault of this check never fails the CI."""
    try:
        problems = check_repository(root)
        if report(problems, strict=strict_requested(FAMILY_STRICT.get(family_of(root), False))):
            return ["the README does not follow the universal layout (tools/universal_readme_check.py lists what to change)"]
    except Exception as error:   # noqa: BLE001 - a bug in a warning must not turn the CI red
        print(f"UNIVERSAL_README=ERROR {type(error).__name__}: {error}")
    return []


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--all", action="store_true", help="print every problem, not only the first ones")
    args = parser.parse_args(argv)
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    problems = check_repository(args.repo.resolve(), run=args.run)
    return report(problems, strict=args.strict or strict_requested(), limit=10**6 if args.all else 12)


if __name__ == "__main__":
    raise SystemExit(main())
