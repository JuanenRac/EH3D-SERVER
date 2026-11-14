#!/usr/bin/env python3
# =============================================================================
# lessons_lint.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Static rules that turn recurring CI failures into checks (read only, standard library only).

Usage (on ONE repository, under 15 seconds):
  python lessons_lint.py --repo DIR
  python lessons_lint.py --repo DIR --json | --check | --rules subprocess-timeout,lang-keys | --measured 91.5
  python lessons_lint.py --repo DIR --precheck [--base origin/main]   # before every push: the rules over the change, the rules of the change itself and the linters the workflows run
  python lessons_lint.py --repo DIR --write-baseline   # saves today's findings in tools/lessons_lint_baseline.json; --check then fails only on NEW ones

Optional per-repository settings, tools/hygiene_config.json:
  {"lessons_lint": {"enabled": true, "rules": [...], "ignore": {"subprocess-timeout": ["tools/x.py"]}, "baseline": "tools/lessons_lint_baseline.json", "measured": 91.5}}

Rules (each with its known false positive):
  subprocess-timeout  subprocess.run/check_output/check_call/call without timeout= (a hung process hangs the CI).
                      FP: calls with **kwargs are skipped (unknown); a process that MUST be long (an installer): mark the line with  # lessons: ok
  visible-text        print(...)/sys.exit(...)/SystemExit(...)/help=/description= with a literal sentence outside the i18n catalogue, only in repositories
                      with a catalogue (locales/en.json) and only in files that import i18n; tests are skipped.
                      FP: machine readable output (KEY=value) or proper names of several words; mark the line with  # lessons: ok
  coverage-floor      a coverage floor (fail_under / --fail-under) HIGHER than the last measured coverage (--measured, settings, .lessons_coverage.json,
                      coverage.json or coverage.xml). Without a saved measure the rule never blocks (it only informs).
                      FP: the measure may be old (another branch).
  lang-keys           keys of locales/en.json missing in one of the other 7 languages (en es de fr it ja zh pt), or a missing language file.
                      FP: keys that exist only in en.json on purpose (list them in "_optional" inside en.json); catalogues laid out in another way
                      (only <folder>/<lang>.json is looked at).
  version-mismatch    the project version written in several places (manifest *.project.json, pyproject, Cargo, package.json, __version__ / TOOL_VERSION,
                      the Version table and the badge of README*.md) does not agree; 0.8.3.0 equals 0.8.3.
                      FP: a README table that quotes another component on purpose; workspace crates with their own versions; an internal library
                      with its own version is ignored on its own (its package must carry the name of the repository; if not, use `ignore`).

More rules, from the lessons that came back most often (not part of the default CI set: ask for them with "rules" in the settings, or run --precheck, which uses all of them
over the files and lines of the change only; mark a line with  # lessons: ok  to accept it):
  mosquitto-direct       a test or script that starts the MQTT broker from the system program (a security profile can stop it reading the configuration):
                         copy the program to the test folder first. FP: a copy made in another module.
  memcmp-string-literal  C/C++ memcmp against a string literal where tests read the magic bytes from the source as byte literals. FP: any other memcmp use.
  regex-control-char     a pattern written in a plain string where a backslash escape (b, 1, a) became a control character: use a raw string.
  catalog-register       texts of the fr/de/pt catalogue in the familiar form (tu/du/teu): they must be formal. FP: a quoted word of that spelling.
  ts-es2020-api          .at(), .replaceAll(), Object.hasOwn(), .findLast() in TypeScript whose tsconfig targets ES2020. FP: a polyfilled project.
  e2e-broad-locator      locator('header select') made only of tag names in an e2e test: use a test id. FP: a page with a single such element.
  forbidden-literal      text you list in the settings ("forbidden": [{"pattern": ..., "message": ..., "globs": [...]}]), e.g. an old port number.
  worktree-name          the folder is not named like the repository of its manifest (the validators want the same name).
  shell-not-executable   a committed shell script with a shebang and without the executable bit.
Rules of the change itself (--precheck, needs git):
  generated-hand-edit    a README block between generated-section markers, or data/glance.json, edited without its source in the change. FP: a prose edit in a block that is not generated.
  test-count-stale       tests added and no README, glance or manifest touched: the generator of the test counts is the last step.
  stacked-or-behind      the branch is behind the base, or carries the commits of another branch.
  private-reference      private words, internal identifiers or local paths in added lines. commit-trailer: the same, or an assistant trailer, in a commit message.
It also prints the commands the workflows of the repository run (ruff, mypy, cargo fmt, tsc...) so the same ones are run before pushing, and the new findings of
workflow_hygiene.py with its speed rules when that tool is beside this one.

Nothing is modified (only --write-baseline writes, and only the baseline file).
"""
from __future__ import annotations

import argparse
import ast
import fnmatch
import json
import re
import sys
import warnings
from collections import Counter
from pathlib import Path
from typing import Any

LANGS = ("en", "es", "de", "fr", "it", "ja", "zh", "pt")
RULES = ("subprocess-timeout", "visible-text", "coverage-floor", "lang-keys", "version-mismatch")
SKIP_DIRS = {".git", "node_modules", "venv", ".venv", "__pycache__", "build", "dist", "target", "third_party", ".mypy_cache", ".ruff_cache", "site-packages", ".compile"}
PRAGMA = re.compile(r"#\s*lessons:\s*ok")
SENTENCE = re.compile(r"[A-Za-zÀ-ÿ]{3,}[ ,:]+[A-Za-zÀ-ÿ]{2,}")
SUBPROCESS_FUNCS = {"run", "check_output", "check_call", "call"}


def walk(repo: Path, suffixes: tuple[str, ...]):
    stack = [repo]
    while stack:
        d = stack.pop()
        try:
            entries = sorted(d.iterdir())
        except OSError:
            continue
        for e in entries:
            if e.is_dir():
                if e.name not in SKIP_DIRS:
                    stack.append(e)
            elif e.suffix in suffixes:
                yield e


def finding(rule, path, line, message, repo):
    try:
        rel = path.relative_to(repo).as_posix()
    except ValueError:
        rel = str(path)
    return {"rule": rule, "file": rel, "line": line, "message": message}


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8-sig", errors="replace")


# ---------------------------------------------------------------- subprocess-timeout
def lint_subprocess(tree: ast.AST, lines: list[str]):
    mod_aliases, func_aliases = set(), set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for a in node.names:
                if a.name == "subprocess":
                    mod_aliases.add(a.asname or "subprocess")
        elif isinstance(node, ast.ImportFrom) and node.module == "subprocess":
            for a in node.names:
                if a.name in SUBPROCESS_FUNCS:
                    func_aliases.add(a.asname or a.name)
    out = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        fn = node.func
        name = None
        if isinstance(fn, ast.Attribute) and fn.attr in SUBPROCESS_FUNCS and isinstance(fn.value, ast.Name) and fn.value.id in mod_aliases:
            name = f"subprocess.{fn.attr}"
        elif isinstance(fn, ast.Name) and fn.id in func_aliases:
            name = fn.id
        if not name:
            continue
        if any(k.arg == "timeout" for k in node.keywords) or any(k.arg is None for k in node.keywords):
            continue
        if any(PRAGMA.search(lines[i - 1]) for i in range(node.lineno, min((node.end_lineno or node.lineno), len(lines)) + 1)):
            continue
        out.append((node.lineno, f"{name}(...) without timeout="))
    return out


# ---------------------------------------------------------------- visible-text
def imports_i18n(tree: ast.AST) -> bool:
    for node in ast.walk(tree):
        if isinstance(node, ast.Import) and any(a.name.split(".")[-1].startswith("i18n") for a in node.names):
            return True
        if isinstance(node, ast.ImportFrom) and ((node.module or "").split(".")[-1].startswith("i18n") or any(a.name.startswith("i18n") for a in node.names)):
            return True
    return False


def lint_visible(tree: ast.AST, lines: list[str]):
    out = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        fn = node.func
        fname = fn.attr if isinstance(fn, ast.Attribute) else getattr(fn, "id", "")
        targets = []
        if fname in ("print", "exit", "SystemExit"):
            targets = list(node.args)
        targets += [kw.value for kw in node.keywords if kw.arg in ("help", "description")]
        if PRAGMA.search(lines[node.lineno - 1]):
            continue
        for target in targets:
            if isinstance(target, ast.Call):   # t("key") or any call: it is checked in its own arguments
                continue
            for sub in ast.walk(target):
                if isinstance(sub, ast.Constant) and isinstance(sub.value, str) and SENTENCE.search(sub.value):
                    out.append((node.lineno, f"{fname}(): visible text outside the catalogue: {sub.value[:50]!r}"))
                    break
    return out


def find_catalogs(repo: Path):
    """Folders with an en.json that look like a language catalogue (named locales/i18n/lang(s)/translations, or with another <lang>.json beside it)."""
    cats = []
    for en in walk(repo, (".json",)):
        if en.name != "en.json":
            continue
        d = en.parent
        if d.name.lower() in ("locales", "locale", "i18n", "lang", "langs", "languages", "translations", "lng") or any((d / f"{l}.json").exists() for l in LANGS if l != "en"):
            cats.append(d)
    return cats


def lint_python(repo: Path, rules: set[str], has_catalog: bool):
    out = []
    for path in walk(repo, (".py",)):
        try:
            text = read(path)
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")   # SyntaxWarning about escapes in other people's code is not the business of this tool
                tree = ast.parse(text)
        except (SyntaxError, ValueError, OSError):
            continue
        lines = text.split("\n")
        if "subprocess-timeout" in rules:
            out += [finding("subprocess-timeout", path, ln, msg, repo) for ln, msg in lint_subprocess(tree, lines)]
        if "visible-text" in rules and has_catalog and not re.search(r"(^|/)(tests?|docs)(/|$)", path.relative_to(repo).as_posix()) \
                and not path.name.startswith(("test_", "i18n")) and imports_i18n(tree):
            out += [finding("visible-text", path, ln, msg, repo) for ln, msg in lint_visible(tree, lines)]
    return out


# ---------------------------------------------------------------- coverage-floor
FLOOR_PATTERNS = (
    re.compile(r"--fail[-_]under[= ]+(\d+(?:\.\d+)?)"),
    re.compile(r"^\s*fail[-_]under\s*=\s*(\d+(?:\.\d+)?)", re.M),
    re.compile(r"\"?fail[-_]under\"?\s*:\s*(\d+(?:\.\d+)?)"),
)


def coverage_floor(repo: Path):
    best, where = None, None
    candidates = [repo / "pyproject.toml", repo / ".coveragerc", repo / "setup.cfg", repo / "tox.ini", repo / "Makefile"]
    candidates += sorted((repo / ".github" / "workflows").glob("*.y*ml")) if (repo / ".github" / "workflows").is_dir() else []
    candidates += sorted((repo / "tools").glob("*ci*validate*")) if (repo / "tools").is_dir() else []
    for f in candidates:
        if not f.is_file():
            continue
        try:
            text = read(f)
        except OSError:
            continue
        for pat in FLOOR_PATTERNS:
            for m in pat.finditer(text):
                v = float(m.group(1))
                if best is None or v > best:
                    best, where = v, f
    return best, where


def coverage_measured(repo: Path, cli: float | None):
    if cli is not None:
        return cli, "--measured"
    f = repo / ".lessons_coverage.json"
    if f.is_file():
        try:
            return float(json.loads(read(f))["measured"]), f.name
        except (ValueError, KeyError, TypeError):
            pass
    f = repo / "coverage.json"
    if f.is_file():
        try:
            return float(json.loads(read(f))["totals"]["percent_covered"]), f.name
        except (ValueError, KeyError, TypeError):
            pass
    f = repo / "coverage.xml"
    if f.is_file():
        m = re.search(r'line-rate="([0-9.]+)"', read(f)[:2000])
        if m:
            return round(float(m.group(1)) * 100, 2), f.name
    return None, None


def lint_coverage(repo: Path, cli_measured):
    floor, where = coverage_floor(repo)
    if floor is None:
        return [], None
    measured, src = coverage_measured(repo, cli_measured)
    if measured is None:
        return [], f"floor {floor:g} in {where.name}; no measured coverage saved (pass --measured or create .lessons_coverage.json)"
    if floor > measured:
        return [finding("coverage-floor", where, 0, f"floor {floor:g} is higher than the last measured coverage {measured:g} ({src})", repo)], None
    return [], None


# ---------------------------------------------------------------- lang-keys
def flatten(obj, prefix=""):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if isinstance(k, str) and k.startswith("_") and not prefix:
                continue
            yield from flatten(v, f"{prefix}.{k}" if prefix else k)
    else:
        yield prefix


def lint_lang_keys(repo: Path, catalogs):
    out = []
    for d in catalogs:
        try:
            en_raw = json.loads(read(d / "en.json"))
        except ValueError:
            out.append(finding("lang-keys", d / "en.json", 0, "en.json is not valid JSON", repo))
            continue
        optional = set(en_raw.get("_optional", [])) if isinstance(en_raw, dict) and isinstance(en_raw.get("_optional"), list) else set()
        en_keys = set(flatten(en_raw)) - optional
        for lang in LANGS[1:]:
            f = d / f"{lang}.json"
            if not f.is_file():
                out.append(finding("lang-keys", f, 0, f"missing file for the language '{lang}'", repo))
                continue
            try:
                keys = set(flatten(json.loads(read(f))))
            except ValueError:
                out.append(finding("lang-keys", f, 0, "not valid JSON", repo))
                continue
            missing = sorted(en_keys - keys)
            if missing:
                more = f" (+{len(missing) - 5} more)" if len(missing) > 5 else ""
                out.append(finding("lang-keys", f, 0, f"{len(missing)} keys of en.json missing: {', '.join(missing[:5])}{more}", repo))
    return out


# ---------------------------------------------------------------- version-mismatch
VERSION_RE = r"v?(\d+\.\d+\.\d+(?:\.\d+)?)"
README_LABELS = ("version", "versión", "versao", "versão", "versione", "バージョン", "版本", "版")
PREFIX_TOKENS = {"urtc", "hydra", "umc", "armor", "eh3d", "electro", "hobby", "3d"}


def is_own_package(repo_name: str, pkg: str) -> bool:
    """True if the Python package carries the name of the repository itself (urtc_gateway in URTC-GATEWAY). An internal library with its own
    version (urtc_can in URTC-FLASHER, vision_companion in URTC-VISION-TOOL) is not the version of the product and is ignored."""
    repo_n, pkg_n = repo_name.lower().replace("-", "_"), pkg.lower().replace("-", "_")
    if pkg_n in repo_n or repo_n in pkg_n:
        return True
    own = {t for t in pkg_n.split("_") if t and t not in PREFIX_TOKENS}
    return own <= set(repo_n.split("_"))


def versions_found(repo: Path):
    found: list[tuple[str, str, int]] = []   # (source, version, line)

    def add(src, ver, line=0):
        ver = ver.lstrip("v")
        if re.fullmatch(r"\d+\.\d+\.\d+\.0", ver):   # 0.8.3.0 and 0.8.3 are the same version (npm and Cargo do not allow 4 components)
            ver = ver[:-2]
        found.append((src, ver, line))

    for f in sorted(repo.glob("*.project.json")):
        try:
            v = json.loads(read(f)).get("version")
        except ValueError:
            continue
        if isinstance(v, str):
            add(f.name, v)
    f = repo / "pyproject.toml"
    if f.is_file():
        sect = ""
        for i, ln in enumerate(read(f).split("\n"), 1):
            m = re.match(r"^\s*\[([^\]]+)\]", ln)
            if m:
                sect = m.group(1)
            elif sect in ("project", "tool.poetry") and (mv := re.match(r"^\s*version\s*=\s*[\"']([^\"']+)[\"']", ln)):
                add("pyproject.toml", mv.group(1), i)
    f = repo / "Cargo.toml"
    if f.is_file():
        sect = ""
        for i, ln in enumerate(read(f).split("\n"), 1):
            m = re.match(r"^\s*\[([^\]]+)\]", ln)
            if m:
                sect = m.group(1)
            elif sect == "package" and (mv := re.match(r"^\s*version\s*=\s*[\"']([^\"']+)[\"']", ln)):
                add("Cargo.toml", mv.group(1), i)
    f = repo / "package.json"
    if f.is_file():
        try:
            v = json.loads(read(f)).get("version")
        except ValueError:
            v = None
        if isinstance(v, str):
            add("package.json", v)
    for p in walk(repo, (".py",)):
        rel = p.relative_to(repo).as_posix()
        depth = rel.count("/")
        if p.name in ("__init__.py", "version.py", "_version.py", "tool_version.py") and depth <= 3 and not re.search(r"(^|/)(tests?|tools|docs)/", rel):
            if p.name in ("__init__.py", "_version.py", "version.py") and depth >= 1 and not is_own_package(repo.name, p.parent.name):
                continue   # internal library with its own version
            for i, ln in enumerate(read(p).split("\n"), 1):
                m = re.match(r"^(?:__version__|TOOL_VERSION|VERSION)\s*(?::\s*\w+\s*)?=\s*[\"']([^\"']+)[\"']", ln)
                if m and re.fullmatch(VERSION_RE, m.group(1)):
                    add(rel, m.group(1), i)
    for f in sorted(repo.glob("README*.md")):
        text = read(f)
        for i, ln in enumerate(text.split("\n")[:80], 1):
            m = re.match(r"^\|\s*([^|]+?)\s*\|\s*`?" + VERSION_RE + r"`?\s*\|", ln)
            if m and m.group(1).strip("* ").lower() in README_LABELS:
                add(f.name + " (table)", m.group(2), i)
            b = re.search(r"img\.shields\.io/badge/[Vv]ersion-" + VERSION_RE + r"-", ln)
            if b:
                add(f.name + " (badge)", b.group(1), i)
    return found


def lint_versions(repo: Path):
    found = versions_found(repo)
    distinct = {v for _, v, _ in found}
    if len(distinct) <= 1:
        return []
    groups: dict[str, list[str]] = {}
    for src, v, _ in found:
        groups.setdefault(v, []).append(src)
    detail = "; ".join(f"{v}: {', '.join(srcs[:4])}{'...' if len(srcs) > 4 else ''}" for v, srcs in sorted(groups.items()))
    first = min((ln for _, _, ln in found if ln), default=0)
    return [finding("version-mismatch", repo / "README.md", first, f"{len(distinct)} different versions: {detail}", repo)]


# ---------------------------------------------------------------- settings and baseline
CONFIG_FILE = "tools/hygiene_config.json"      # section "lessons_lint": enabled, rules, ignore {rule: [path globs]}, baseline, measured
DEFAULT_BASELINE = "tools/lessons_lint_baseline.json"
COUNT_ONLY = {"subprocess-timeout", "version-mismatch", "coverage-floor"}   # their identity is rule + file (counted per file); the text does not count


def load_section(repo: Path, config: str | None) -> dict:
    if not config:
        return {}
    try:
        data = json.loads(read(repo / config))
    except (OSError, ValueError):
        return {}
    sec = data.get("lessons_lint") if isinstance(data, dict) else None
    return sec if isinstance(sec, dict) else {}


def finding_key(f: dict) -> str:
    return f"{f['rule']}|{f['file']}|" + ("" if f["rule"] in COUNT_ONLY | COUNT_ONLY_MORE else f["message"])


def load_baseline(path: Path) -> Counter:
    try:
        items = json.loads(read(path)).get("findings")
    except (OSError, ValueError, AttributeError):
        return Counter()
    return Counter({k: int(v) for k, v in items.items()}) if isinstance(items, dict) else Counter()


def is_ignored(f: dict, ignore: dict) -> bool:
    return any(fnmatch.fnmatch(f["file"], g) for g in list(ignore.get(f["rule"], [])) + list(ignore.get("*", [])))


def write_baseline(result: dict, path: Path) -> None:
    cnt = Counter(finding_key(f) for f in result["findings"])
    doc = {"_comment": "Known findings of lessons_lint.py; only findings not listed here fail the check. Regenerate with lessons_lint.py --write-baseline.",
           "findings": dict(sorted(cnt.items()))}
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(doc, ensure_ascii=False, indent=1) + chr(10), encoding="utf-8")


# ---------------------------------------------------------------- more rules: the lessons that came back most often
# FILE_RULES look at the files (in a precheck only at the files and lines the change touches); PRECHECK_RULES need the change itself (git).
FILE_RULES = ("mosquitto-direct", "memcmp-string-literal", "regex-control-char", "catalog-register", "ts-es2020-api", "e2e-broad-locator",
              "forbidden-literal", "worktree-name", "shell-not-executable")
PRECHECK_RULES = ("generated-hand-edit", "test-count-stale", "stacked-or-behind", "private-reference", "commit-trailer")
ALL_RULES = RULES + FILE_RULES + PRECHECK_RULES
WHOLE_REPO_RULES = ("worktree-name", "coverage-floor", "lang-keys", "version-mismatch")   # a precheck keeps these whole, whatever the change
# rules whose finding is the same whatever the line: they are counted per file in a baseline
COUNT_ONLY_MORE = {"memcmp-string-literal", "e2e-broad-locator", "ts-es2020-api", "mosquitto-direct", "regex-control-char"}

# words that must never reach a public repository, written in pieces so that this very file does not contain them
_PRIVATE_PIECES = (("SON", "NET"), ("AUDIT", "ORIA"), ("Cla", "ude"), ("Co-Auth", "ored-By"), ("Generated", " with"), ("Users[\\\\/]ju", "ane"))
PRIVATE_RE = re.compile("|".join(a + b if "[" in a else re.escape(a + b) for a, b in _PRIVATE_PIECES) + r"|\b(?:AR|UR|HY|GR)\d{4}\b")
TRAILER_RE = re.compile("|".join(re.escape(a + b) for a, b in _PRIVATE_PIECES[2:5]))
TEXT_SUFFIXES = (".py", ".ts", ".tsx", ".js", ".mjs", ".md", ".json", ".yml", ".yaml", ".toml", ".txt", ".sh", ".rs", ".go", ".kt", ".swift", ".c", ".h", ".cpp", ".hpp", ".cc")
C_SUFFIXES = (".c", ".h", ".cpp", ".hpp", ".cc", ".cxx")
INFORMAL = {   # the register of the catalogue texts: fr and pt in the formal form, de with Sie (the pronouns of the familiar form are what is looked for)
    "fr": re.compile(r"\b(tu|toi|ton|ta|tes)\b", re.I),
    "de": re.compile(r"\b(du|dir|dich|dein|deine|deinen|deinem|deiner)\b", re.I),
    "pt": re.compile(r"\b(tu|teu|tua|teus|tuas)\b", re.I),
}
PLACEHOLDER = re.compile(r"\{\{[^}]*\}\}|\{[^}]*\}|%\w")
SPAWN_NAMES = {"Popen", "run", "call", "check_call", "check_output"}
RE_FUNCS = {"compile", "sub", "subn", "search", "match", "fullmatch", "findall", "finditer", "split"}
CONTROL = re.compile("[\x01-\x08\x0b\x0c\x0e-\x1f]")


def git_run(repo: Path, *args: str, timeout: int = 60) -> str | None:
    """Output of a read only git command, or None when git is missing, the folder is not a repository or the command fails."""
    import subprocess
    try:
        out = subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return out.stdout if out.returncode == 0 else None


def lint_mosquitto(tree: ast.AST, text: str):
    """A Python test that starts the broker program straight from the system: under AppArmor (the image of the runner moves) it cannot read the configuration
    of the test. A private copy of the program in the folder of the test runs without that profile."""
    low = text.lower()
    if "mosquitto" not in low:
        return []
    copies = any(isinstance(n, ast.Call) and (isinstance(n.func, ast.Attribute) and n.func.attr in ("copy", "copy2", "copyfile", "copyfileobj"))
                 for n in ast.walk(tree))
    if copies:
        return []
    out = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call) or not node.args or not isinstance(node.args[0], (ast.List, ast.Tuple)) or not node.args[0].elts:
            continue
        fn = node.func
        name = fn.attr if isinstance(fn, ast.Attribute) else getattr(fn, "id", "")
        if name not in SPAWN_NAMES:
            continue
        first, rest = node.args[0].elts[0], node.args[0].elts[1:]
        direct = isinstance(first, ast.Constant) and isinstance(first.value, str) and re.fullmatch(r"(.*[\\/])?mosquitto(\.exe)?", first.value) is not None
        with_conf = bool(rest) and isinstance(rest[0], ast.Constant) and rest[0].value == "-c"
        if direct or with_conf:
            out.append((node.lineno, "the MQTT broker is started from the system program: copy the program to the test folder first (a security profile can stop it reading the configuration)"))
    return out


def lint_python_more(repo: Path, rules: set[str]):
    out = []
    for path in walk(repo, (".py",)):
        try:
            text = read(path)
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                tree = ast.parse(text)
        except (SyntaxError, ValueError, OSError):
            continue
        lines = text.split("\n")
        if "mosquitto-direct" in rules:
            out += [finding("mosquitto-direct", path, ln, msg, repo) for ln, msg in lint_mosquitto(tree, text) if not PRAGMA.search(lines[ln - 1])]
        if "regex-control-char" in rules:
            for node in ast.walk(tree):
                if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr in RE_FUNCS and isinstance(node.func.value, ast.Name) and node.func.value.id == "re":
                    for arg in node.args[:2]:
                        if isinstance(arg, ast.Constant) and isinstance(arg.value, str) and CONTROL.search(arg.value) and not PRAGMA.search(lines[node.lineno - 1]):
                            out.append(finding("regex-control-char", path, node.lineno, "a backslash sequence of the pattern (\\b, \\1, \\a...) became a control character in a plain string: use a raw string", repo))
                            break
    return out


def lint_text_files(repo: Path, rules: set[str], forbidden: list):
    """The rules that read text: the broker in shell and workflows, memcmp with a literal, ES2020 only TypeScript, broad e2e locators and the forbidden literals."""
    out = []
    es2020 = False
    if "ts-es2020-api" in rules:
        for ts in walk(repo, (".json",)):
            if ts.name.startswith("tsconfig"):
                body = read(ts)
                if re.search(r"(?i)\"es2020\"", body) and not re.search(r"(?i)\"(es2021|es2022|es2023|es2024|esnext)\"", body):
                    es2020 = True
    suffixes = tuple(sorted(set(C_SUFFIXES + (".sh", ".yml", ".yaml", ".ts", ".tsx", ".js") + (TEXT_SUFFIXES if forbidden else ()))))
    for path in walk(repo, suffixes):
        try:
            text = read(path)
        except OSError:
            continue
        rel = path.relative_to(repo).as_posix()
        lines = text.split("\n")
        if "mosquitto-direct" in rules and path.suffix in (".sh", ".yml", ".yaml") and not re.search(r"\bcp\b[^\n]*mosquitto", text):
            for i, ln in enumerate(lines, 1):
                if re.match(r"^\s*(sudo\s+)?mosquitto\s+-c\s", ln) and not PRAGMA.search(ln):
                    out.append(finding("mosquitto-direct", path, i, "the MQTT broker is started from the system program: copy the program first (a security profile can stop it reading the configuration)", repo))
        if "memcmp-string-literal" in rules and path.suffix in C_SUFFIXES:
            for m in re.finditer(r"\bmemcmp\s*\([^;]*?\"[^\"\n]*\"", text):
                at = text.count("\n", 0, m.start()) + 1
                if not PRAGMA.search(lines[at - 1]):
                    out.append(finding("memcmp-string-literal", path, at, "memcmp against a string literal: the tests that read the magic bytes from the source expect byte literals", repo))
        if es2020 and path.suffix in (".ts", ".tsx") and not path.name.endswith(".d.ts"):
            for i, ln in enumerate(lines, 1):
                if re.search(r"\.at\(\s*-?\d|\.replaceAll\(|Object\.hasOwn\(|\.findLast(Index)?\(", ln) and not PRAGMA.search(ln):
                    out.append(finding("ts-es2020-api", path, i, "an API newer than ES2020 where the type check targets ES2020 (use items[items.length - 1], replace with a regex, hasOwnProperty)", repo))
        if "e2e-broad-locator" in rules and path.suffix in (".ts", ".js", ".tsx") and re.search(r"(\.spec\.|\.e2e\.|(^|/)e2e/)", rel):
            for i, ln in enumerate(lines, 1):
                if re.search(r"\.locator\(\s*(['\"`])[a-z][\w-]*(\s+[a-z][\w-]*)*\1\s*\)", ln) and not PRAGMA.search(ln):
                    out.append(finding("e2e-broad-locator", path, i, "a locator made only of tag names breaks (strict mode) when the page gets a second one: use a test id", repo))
        if "forbidden-literal" in rules and forbidden:
            for item in forbidden:
                if not isinstance(item, dict) or "pattern" not in item:
                    continue
                globs = item.get("globs") or ["*"]
                if not any(fnmatch.fnmatch(rel, g) for g in globs):
                    continue
                try:
                    rx = re.compile(item["pattern"])
                except re.error:
                    continue
                for i, ln in enumerate(lines, 1):
                    if rx.search(ln) and not PRAGMA.search(ln):
                        out.append(finding("forbidden-literal", path, i, str(item.get("message", "forbidden text: " + item["pattern"])), repo))
    return out


def catalog_strings(obj, prefix=""):
    """(key, text) of every text of a catalogue; the keys that start with an underscore at the top are metadata."""
    if isinstance(obj, dict):
        for k, v in obj.items():
            if not (isinstance(k, str) and k.startswith("_") and not prefix):
                yield from catalog_strings(v, f"{prefix}.{k}" if prefix else str(k))
    elif isinstance(obj, str):
        yield prefix, obj


def lint_catalog_register(repo: Path, catalogs):
    out = []
    for d in catalogs:
        for lang, rx in INFORMAL.items():
            f = d / f"{lang}.json"
            if not f.is_file():
                continue
            try:
                data = json.loads(read(f))
            except ValueError:
                continue
            bad = [key for key, text in catalog_strings(data) if rx.search(PLACEHOLDER.sub(" ", text))]
            if bad:
                out.append(finding("catalog-register", f, 0, f"{len(bad)} texts in the familiar form ({lang}): {', '.join(bad[:3])}{'...' if len(bad) > 3 else ''}", repo))
    return out


def lint_manifest_name(repo: Path):
    out = []
    for f in sorted(repo.glob("*.project.json")):
        try:
            name = json.loads(read(f)).get("name")
        except ValueError:
            continue
        if isinstance(name, str) and name != repo.name:
            out.append(finding("worktree-name", f, 0, f"the manifest says '{name}' but the folder is '{repo.name}': the validators want a worktree named like the repository", repo))
    return out


def lint_executable_scripts(repo: Path):
    listing = git_run(repo, "ls-files", "-s", "--", "*.sh")
    out = []
    for line in (listing or "").splitlines():
        meta, _, rel = line.partition("\t")
        if meta.startswith("100644") and rel:
            try:
                first = read(repo / rel).split("\n", 1)[0]
            except OSError:
                continue
            if first.startswith("#!"):
                out.append(finding("shell-not-executable", repo / rel, 0, "a shell script with a shebang is committed without the executable bit (git update-index --chmod=+x)", repo))
    return out


# ---------------------------------------------------------------- the change itself (precheck)
HUNK = chr(64) * 2   # the start of a hunk of a diff, written so that no template check finds a placeholder mark in this file


def parse_diff(diff: str):
    """{path: {line number: text}} of the added lines of a `git diff -U0`."""
    added: dict[str, dict[int, str]] = {}
    path, line_no = None, 0
    for raw in diff.split("\n"):
        if raw.startswith("+++ "):
            path = raw[4:].strip()
            path = path[2:] if path.startswith("b/") else (None if path == "/dev/null" else path)
        elif raw.startswith(HUNK):
            m = re.search(r"\+(\d+)", raw)
            line_no = int(m.group(1)) if m else 0
        elif raw.startswith("+") and not raw.startswith("+++") and path:
            added.setdefault(path, {})[line_no] = raw[1:]
            line_no += 1
    return added


def resolve_base(repo: Path, base: str | None) -> str | None:
    for cand in ([base] if base else []) + ["origin/main", "main"]:
        if cand and git_run(repo, "rev-parse", "--verify", "-q", cand + "^{commit}") is not None:
            return cand
    return None


def change_of(repo: Path, base: str | None):
    """(base, added lines by path, changed paths). Untracked files count as wholly added."""
    ref = resolve_base(repo, base)
    diff = git_run(repo, "diff", "-U0", "--no-color", ref) if ref else git_run(repo, "diff", "-U0", "--no-color", "HEAD")
    added = parse_diff(diff or "")
    changed = set(added)
    names = git_run(repo, "diff", "--name-only", ref or "HEAD") or ""
    changed |= {n for n in names.split("\n") if n}
    for rel in (git_run(repo, "ls-files", "--others", "--exclude-standard") or "").split("\n"):
        if rel and (repo / rel).is_file() and rel.endswith(TEXT_SUFFIXES):
            try:
                added[rel] = {i: t for i, t in enumerate(read(repo / rel).split("\n"), 1)}
            except OSError:
                continue
            changed.add(rel)
    return ref, added, changed


def lint_change(repo: Path, rules: set[str], ref: str | None, added: dict, changed: set):
    out = []
    if "private-reference" in rules:
        for rel, lines in sorted(added.items()):
            if not rel.endswith(TEXT_SUFFIXES):
                continue
            for ln, text in sorted(lines.items()):
                m = PRIVATE_RE.search(text)
                if m:
                    out.append(finding("private-reference", repo / rel, ln, f"'{m.group(0)}' must not reach a public repository", repo))
    if ref and "commit-trailer" in rules:
        log = git_run(repo, "log", f"{ref}..HEAD", "--format=%h%x1f%B%x1e") or ""
        for entry in log.split("\x1e"):
            sha, _, body = entry.strip().partition("\x1f")
            m = TRAILER_RE.search(body) or PRIVATE_RE.search(body)
            if sha and m:
                out.append(finding("commit-trailer", repo / ".git", 0, f"commit {sha} says '{m.group(0)}' in its message", repo))
    if ref and "stacked-or-behind" in rules:
        behind = (git_run(repo, "rev-list", "--count", f"HEAD..{ref}") or "0").strip()
        if behind.isdigit() and int(behind) > 0:
            out.append(finding("stacked-or-behind", repo / ".git", 0, f"the branch is {behind} commits behind {ref}: merge it before asking for a review", repo))
        current = (git_run(repo, "branch", "--show-current") or "").strip()
        commits = (git_run(repo, "rev-list", f"{ref}..HEAD") or "").split()
        for sha in commits[:20]:
            heads = [b.strip() for b in (git_run(repo, "branch", "-r", "--contains", sha) or "").split("\n") if b.strip() and "->" not in b]
            other = [b for b in heads if b not in (ref, f"origin/{current}")]
            if other:
                out.append(finding("stacked-or-behind", repo / ".git", 0, f"the branch carries commits of another branch ({other[0]}): every branch starts at {ref}", repo))
                break
    if "test-count-stale" in rules:
        tests = 0
        for rel, lines in added.items():
            if re.search(r"(^|/)(tests?|__tests__)/|(^|/)test_|\.test\.|\.spec\.", rel):
                tests += sum(1 for t in lines.values() if re.match(r"\s*(async def test_|def test_|it\(|test\(|#\[test\])", t))
        counted = any(re.match(r"README.*\.md$|data/glance\.json$|.*\.project\.json$", c) for c in changed)
        if tests and not counted:
            out.append(finding("test-count-stale", repo / "README.md", 0, f"{tests} tests added and no README, glance or manifest in the change: run the generator of the test counts last", repo))
    if "generated-hand-edit" in rules:
        sources = any(re.search(r"(sections\.json|readme_src|readme_sections|glance\.py|make_glance|\.project\.json)", c) for c in changed)
        touched: list[str] = []
        if "data/glance.json" in changed:
            touched.append("data/glance.json")
        for rel, lines in added.items():
            if re.match(r"README.*\.md$", rel) and (repo / rel).is_file():
                spans, start = [], None
                for i, ln in enumerate(read(repo / rel).split("\n"), 1):
                    if re.search(r"<!--\s*[\w.-]+-section:[^>]*-->", ln):
                        if start:
                            spans.append((start, i))
                        start = i
                    elif start and (ln.startswith("## ") or re.search(r"<!--\s*/[\w.-]+-section", ln)):
                        spans.append((start, i))
                        start = None
                if start:
                    spans.append((start, 10 ** 9))
                if any(a < n < b for n in lines for a, b in spans):
                    touched.append(rel)
        if touched and not sources:
            out.append(finding("generated-hand-edit", repo / touched[0], 0, f"generated text edited by hand ({', '.join(touched[:3])}): change the source and regenerate", repo))
    return out


def linters_to_run(repo: Path) -> list[str]:
    """The checks the workflows of the repository run, to be run before pushing (the exact ones, not similar ones)."""
    seen: list[str] = []
    wf = repo / ".github" / "workflows"
    if not wf.is_dir():
        return seen
    rx = re.compile(r"((?:python\d? -m )?(?:ruff check|mypy|cargo fmt|cargo clippy|gofmt|go vet|staticcheck|tsc|eslint)\b[^\n&|]*|npm run (?:typecheck|lint)\b[^\n&|]*|python3? -I? ?tools/[\w]*ci_validate\.py)")
    for f in sorted(wf.glob("*.y*ml")):
        for ln in read(f).split("\n"):
            m = rx.search(ln)
            if m and not ln.lstrip().startswith("#"):
                cmd = m.group(1).strip()
                if cmd not in seen:
                    seen.append(cmd)
    return seen[:12]


def precheck_repo(repo: Path, base: str | None = None, measured=None, config: str | None = CONFIG_FILE, baseline: str | None = None) -> dict:
    """Everything an agent runs before pushing: the rules of the files over the files and lines of the change, the rules of the change and the workflow rules."""
    sec = load_section(repo, config)
    files_rules = set(RULES) | set(FILE_RULES)
    result = lint_repo(repo, tuple(files_rules), measured, config, baseline)
    ref, added, changed = change_of(repo, base)
    scoped = []
    for f in result["findings"]:
        if f["rule"] not in WHOLE_REPO_RULES:   # the other rules only count in the files and the lines the change touches
            if f["file"] not in changed or (f["line"] and f["line"] not in added.get(f["file"], {})):
                continue
        scoped.append(f)
    ignore: dict[str, Any] = sec["ignore"] if isinstance(sec.get("ignore"), dict) else {}
    extra = [f for f in lint_change(repo, set(PRECHECK_RULES), ref, added, changed) if not is_ignored(f, ignore)]
    for f in extra:
        f["new"] = True
    scoped += extra
    scoped += [f for f in workflow_findings(repo) if f["file"] in changed or (f["rule"] == "changelog-union" and "CHANGELOG.md" in changed)]   # only the workflows the change touches
    scoped.sort(key=lambda x: (x["rule"], x["file"], x["line"]))
    result["findings"] = scoped
    result["base"] = ref
    result["run_before_push"] = linters_to_run(repo)
    return result


def workflow_findings(repo: Path):
    """The new findings of workflow_hygiene.py with its speed rules (cancel-in-progress on main, ubuntu-latest in infrastructure jobs...), when that tool is beside this one."""
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import workflow_hygiene as wh
    except ImportError:
        return []
    if not (repo / ".github" / "workflows").is_dir():
        return []
    try:
        repos = wh.scan(repo, speed=True)
    except TypeError:   # an older copy without the speed rules
        repos = wh.scan(repo)
    wh.apply_config_and_baseline(repos)
    out = []
    for r in repos.values():
        for f in r["findings"]:
            if f.get("new", True):
                out.append({"rule": f["rule"], "file": f".github/workflows/{f['file']}", "line": f["line"], "message": f["message"], "new": True})
    return out


# ---------------------------------------------------------------- orchestration
def lint_repo(repo: Path, rules=RULES, measured=None, config: str | None = None, baseline: str | None = None) -> dict:
    """config: relative path of the settings file (None = ignore it). baseline: relative path of the baseline (None = the one of the settings or the default)."""
    sec = load_section(repo, config)
    if sec.get("enabled") is False:
        return {"repo": repo.name, "catalogs": [], "findings": [], "notes": ["disabled in the settings"], "disabled": True, "resolved": 0}
    if measured is None and isinstance(sec.get("measured"), (int, float)):
        measured = float(sec["measured"])
    rules = set(rules)
    catalogs = find_catalogs(repo) if rules & {"visible-text", "lang-keys", "catalog-register"} else []
    findings: list[dict] = []
    notes: list[str] = []
    if rules & {"subprocess-timeout", "visible-text"}:
        findings += lint_python(repo, rules, bool(catalogs))
    if "coverage-floor" in rules:
        f, note = lint_coverage(repo, measured)
        findings += f
        if note:
            notes.append(note)
    if "lang-keys" in rules:
        findings += lint_lang_keys(repo, catalogs)
    if "version-mismatch" in rules:
        findings += lint_versions(repo)
    if rules & {"mosquitto-direct", "regex-control-char"}:
        findings += lint_python_more(repo, rules)
    if rules & {"mosquitto-direct", "memcmp-string-literal", "ts-es2020-api", "e2e-broad-locator", "forbidden-literal"}:
        forbidden: list[Any] = sec["forbidden"] if isinstance(sec.get("forbidden"), list) else []
        findings += lint_text_files(repo, rules, forbidden)
    if "catalog-register" in rules:
        findings += lint_catalog_register(repo, catalogs)
    if "worktree-name" in rules:
        findings += lint_manifest_name(repo)
    if "shell-not-executable" in rules:
        findings += lint_executable_scripts(repo)
    ignore: dict[str, Any] = sec["ignore"] if isinstance(sec.get("ignore"), dict) else {}
    findings = [f for f in findings if not is_ignored(f, ignore)]
    findings.sort(key=lambda x: (x["rule"], x["file"], x["line"]))
    result: dict[str, Any] = {"repo": repo.name, "catalogs": [c.relative_to(repo).as_posix() or "." for c in catalogs], "findings": findings, "notes": notes}
    left = load_baseline(repo / (baseline or sec.get("baseline") or DEFAULT_BASELINE))
    for f in findings:
        k = finding_key(f)
        f["new"] = left[k] <= 0
        if not f["new"]:
            left[k] -= 1
    result["resolved"] = sum(left.values())   # baseline entries that no longer exist (they can be pruned)
    return result


def render(result: dict) -> str:
    lines = [f"lessons_lint: {result['repo']}  (catalogues: {', '.join(result['catalogs']) or 'none'})"]
    counts: dict[str, int] = {}
    for f in result["findings"]:
        counts[f["rule"]] = counts.get(f["rule"], 0) + 1
        lines.append(f"  {f['file']}:{f['line']}  {'NEW ' if f.get('new', True) else ''}[{f['rule']}] {f['message']}")
    for n in result["notes"]:
        lines.append(f"  note: {n}")
    new = sum(1 for f in result["findings"] if f.get("new", True))
    lines.append("SUMMARY: " + (", ".join(f"{k}={v}" for k, v in sorted(counts.items())) or "no findings") + f"  total={len(result['findings'])} new={new} stale_baseline={result.get('resolved', 0)}")
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Static rules derived from recurring CI failures (read only).")
    ap.add_argument("--repo", default=".", help="repository folder")
    ap.add_argument("--rules", help="comma separated rules: " + ",".join(RULES))
    ap.add_argument("--measured", type=float, help="last measured coverage (percent) for the coverage-floor rule")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--check", action="store_true", help="exit 1 if there are NEW findings (not in the baseline)")
    ap.add_argument("--config", default=CONFIG_FILE, help="settings file inside the repository (default " + CONFIG_FILE + "); 'none' ignores it")
    ap.add_argument("--baseline", help="baseline inside the repository (default: the one of the settings or " + DEFAULT_BASELINE + ")")
    ap.add_argument("--precheck", action="store_true", help="what to run before pushing: the rules over the change (against --base), the rules of the change itself (git), the workflow rules and the list of linters the workflows run; exit 1 on a new finding")
    ap.add_argument("--base", help="reference the change is compared with in --precheck (default origin/main, then main)")
    ap.add_argument("--write-baseline", action="store_true", help="WRITE the baseline with today's findings (the only option that writes)")
    args = ap.parse_args(argv)
    repo = Path(args.repo).resolve()
    if not repo.is_dir():
        print(f"does not exist: {repo}", file=sys.stderr)
        return 2
    cfg = None if args.config == "none" else args.config
    sec = load_section(repo, cfg)
    cfg_rules = tuple(sec["rules"]) if isinstance(sec.get("rules"), list) else None
    rules = tuple(args.rules.split(",")) if args.rules else (cfg_rules or RULES)
    if not set(rules) <= set(ALL_RULES):
        print("unknown rule: " + ", ".join(sorted(set(rules) - set(RULES))), file=sys.stderr)
        return 2
    if args.precheck:
        result = precheck_repo(repo, args.base, args.measured, cfg, args.baseline)
        print(json.dumps(result, ensure_ascii=False, indent=1) if args.json else render(result))
        new = sum(1 for f in result["findings"] if f.get("new", True))
        for cmd in result.get("run_before_push", []):
            print(f"  run before pushing: {cmd}", file=sys.stderr if args.json else sys.stdout)
        print(f"PRECHECK={'FAIL' if new else 'PASS'} new={new} findings={len(result['findings'])} base={result.get('base')}", file=sys.stderr if args.json else sys.stdout)
        return 1 if new else 0
    result = lint_repo(repo, rules, args.measured, cfg or "", args.baseline)
    if args.write_baseline:
        target = repo / (args.baseline or sec.get("baseline") or DEFAULT_BASELINE)
        write_baseline(result, target)
        print("baseline written: " + str(target), file=sys.stderr)
        result = lint_repo(repo, rules, args.measured, cfg or "", args.baseline)
    print(json.dumps(result, ensure_ascii=False, indent=1) if args.json else render(result))
    if args.check:
        new = sum(1 for f in result["findings"] if f.get("new", True))
        bad = new > 0
        print(f"LESSONS_LINT={'FAIL' if bad else 'PASS'} new={new} findings={len(result['findings'])}", file=sys.stderr if args.json else sys.stdout)
        return 1 if bad else 0
    return 0


def run_in_ci(root: Path) -> list[str]:
    """The step of ci_validate: problems (one line each) only when the repository opted in with tools/hygiene_config.json or a baseline.
    Without either the step does not block."""
    root = Path(root)
    sec = load_section(root, CONFIG_FILE)
    if not (root / CONFIG_FILE).is_file() and not (root / (sec.get("baseline") or DEFAULT_BASELINE)).is_file():
        return []
    rules = tuple(sec["rules"]) if isinstance(sec.get("rules"), list) else RULES
    rules = tuple(r for r in rules if r in RULES)
    result = lint_repo(root, rules, None, CONFIG_FILE, None)
    new = [f for f in result["findings"] if f.get("new", True)]
    problems = [f"{f['file']}:{f['line']} {f['rule']}: {f['message']}" for f in new[:20]]
    if new:
        problems.append(f"lessons lint: {len(new)} new finding(s) (python tools/lessons_lint.py --repo . )")
    return problems


if __name__ == "__main__":
    sys.exit(main())
