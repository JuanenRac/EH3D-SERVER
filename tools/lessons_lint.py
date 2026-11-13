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
    return f"{f['rule']}|{f['file']}|" + ("" if f["rule"] in COUNT_ONLY else f["message"])


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


# ---------------------------------------------------------------- orchestration
def lint_repo(repo: Path, rules=RULES, measured=None, config: str | None = None, baseline: str | None = None) -> dict:
    """config: relative path of the settings file (None = ignore it). baseline: relative path of the baseline (None = the one of the settings or the default)."""
    sec = load_section(repo, config)
    if sec.get("enabled") is False:
        return {"repo": repo.name, "catalogs": [], "findings": [], "notes": ["disabled in the settings"], "disabled": True, "resolved": 0}
    if measured is None and isinstance(sec.get("measured"), (int, float)):
        measured = float(sec["measured"])
    rules = set(rules)
    catalogs = find_catalogs(repo) if rules & {"visible-text", "lang-keys"} else []
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
    if not set(rules) <= set(RULES):
        print("unknown rule: " + ", ".join(sorted(set(rules) - set(RULES))), file=sys.stderr)
        return 2
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
