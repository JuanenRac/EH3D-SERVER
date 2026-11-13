#!/usr/bin/env python3
# =============================================================================
# workflow_hygiene.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Static hygiene check of GitHub Actions workflows (read only, standard library only).

It walks <root>/<repo>/.github/workflows/*.yml|yaml (or <root>/.github/workflows when root is a repository) and reports, per repository:

  unpinned-action     a `uses:` of a third-party action or workflow that is not pinned to a 40-hex SHA (own actions, JuanenRac/..., and local ./ ones are allowed)
  no-timeout          a job (not a call to a reusable workflow) without timeout-minutes
  no-permissions      a workflow without a top-level `permissions:`
  no-concurrency      a release or publish workflow with no `concurrency` (neither at the top nor in a job)
  prt-checkout        pull_request_target that checks out the code of the pull request (head ref or repository)
  secret-in-run       ${{ secrets.X }} interpolated inside a `run:` (pass it through env: instead)

Usage:
  python workflow_hygiene.py --root DIR              # table per repository, then the count per ecosystem (default root: the current folder)
  python workflow_hygiene.py --root DIR --json       # JSON output
  python workflow_hygiene.py --check --threshold 50  # exit 1 if there are MORE than 50 NEW findings (default 0)
  python workflow_hygiene.py --root . --write-baseline   # saves today's findings in tools/workflow_hygiene_baseline.json; --check then fails only on NEW ones
  python workflow_hygiene.py --repo NAME --rules unpinned-action,no-timeout --min-severity medium

Optional per-repository settings, tools/hygiene_config.json:
  {"workflow_hygiene": {"enabled": true, "rules": [...], "min_severity": "low", "baseline": "tools/workflow_hygiene_baseline.json", "threshold": 0}}

Nothing is modified (only --write-baseline writes, and only the baseline file). The YAML is read line by line (no PyYAML); known false
positives: a job whose timeout comes from a YAML anchor or merge key is reported; a step-level timeout does not count; `permissions`
only on jobs is reported (it is asked at the top); a "release" workflow is guessed from its name, trigger or commands.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

DEFAULT_ROOT = Path(".")
SKIP_DIRS = {"third_party", "node_modules", ".git"}   # plus every folder whose name starts with "_" or "."; more with --skip
OWN_OWNERS = ("juanenrac",)

SEVERITY = {"prt-checkout": "high", "secret-in-run": "high", "unpinned-action": "medium", "no-permissions": "medium",
            "no-timeout": "low", "no-concurrency": "low"}
WEIGHT = {"high": 3, "medium": 2, "low": 1}
SEV_ORDER = {"low": 0, "medium": 1, "high": 2}
RULES = tuple(SEVERITY)

SHA40 = re.compile(r"^[0-9a-f]{40}$")
USES = re.compile(r"^(?P<indent>\s*)(?:-\s+)?uses:\s*(?P<val>[^\s#]+)")
KEY = re.compile(r"^(?P<indent>\s*)(?P<dash>-\s+)?(?P<key>[\w.-]+):(?:\s+(?P<rest>.*))?$")
BLOCK = re.compile(r"^[|>][+-]?\d*\s*(#.*)?$")
JOB_NAME = re.compile(r'^(?P<indent>\s+)(?P<name>[\w.-]+|"[^"]+"|\'[^\']+\'):\s*(#.*)?$')
SECRET = re.compile(r"\$\{\{\s*secrets\.", re.I)
PR_HEAD = re.compile(r"(pull_request\.head|head_ref|refs/pull/|github\.event\.pull_request\.merge_commit_sha)", re.I)
RELEASE_WORDS = re.compile(r"(release|publish|deploy)", re.I)
RELEASE_CMDS = re.compile(r"(gh\s+release\s+(create|upload)|action-gh-release|npm\s+publish|twine\s+upload|cargo\s+publish|pypa/gh-action-pypi-publish)")


def ecosystem(repo: str) -> str:
    u = repo.upper()
    if u.startswith("ARMOR"):
        return "ARMOR"
    if u.startswith("URTC"):
        return "URTC"
    if u.startswith("HYDRA"):
        return "HYDRA"
    if u.startswith("EH3D") or u.startswith("ELECTRO-HOBBY-3D"):
        return "EH3D"
    return "OTHER"


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def strip_comment(line: str) -> str:
    """Remove a ' #...' comment that sits outside quotes (enough for workflow YAML)."""
    out, quote = [], ""
    for i, ch in enumerate(line):
        if quote:
            if ch == quote:
                quote = ""
        elif ch in "\"'":
            quote = ch
        elif ch == "#" and (i == 0 or line[i - 1] in " \t"):
            break
        out.append(ch)
    return "".join(out).rstrip()


class Workflow:
    """Line based view of a workflow: lines, bodies of block scalars (run: |), jobs and steps."""

    def __init__(self, text: str, name: str = ""):
        self.name = name
        self.lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
        self.body_lines: set[int] = set()     # 0-based lines inside a block scalar
        self.run_bodies: list[tuple[int, str]] = []   # (1-based line, text) of every `run:`
        self._scan_blocks()

    def _scan_blocks(self) -> None:
        i, n = 0, len(self.lines)
        while i < n:
            raw = self.lines[i]
            code = strip_comment(raw) if raw.strip() and not raw.lstrip().startswith("#") else ""
            m = KEY.match(code) if code else None
            if m and m.group("rest") is not None:
                col = len(m.group("indent")) + (len(m.group("dash")) if m.group("dash") else 0)
                rest = m.group("rest").strip()
                if BLOCK.match(rest):
                    j, body = i + 1, []
                    while j < n and (not self.lines[j].strip() or indent_of(self.lines[j]) > col):
                        self.body_lines.add(j)
                        body.append(self.lines[j])
                        j += 1
                    if m.group("key") == "run":
                        self.run_bodies.append((i + 1, "\n".join(body)))
                    i = j
                    continue
                if m.group("key") == "run":
                    self.run_bodies.append((i + 1, rest))
            i += 1

    def code_lines(self):
        """(index, line without comment) of the lines that are not a block scalar body, blank or a comment."""
        for i, raw in enumerate(self.lines):
            if i in self.body_lines or not raw.strip() or raw.lstrip().startswith("#"):
                continue
            yield i, strip_comment(raw)

    def top_level_keys(self) -> set[str]:
        keys = set()
        for _, code in self.code_lines():
            if code and not code[0].isspace() and not code.startswith("-"):
                m = re.match(r'^("?[\w.-]+"?)\s*:', code)
                if m:
                    keys.add(m.group(1).strip('"'))
        return keys

    def triggers(self) -> set[str]:
        """Events of `on:` (key on / "on" / true) in their scalar, list and map forms."""
        found: set[str] = set()
        lines = list(self.code_lines())
        for pos, (_, code) in enumerate(lines):
            m = re.match(r'^("?on"?|true)\s*:\s*(.*)$', code)
            if not m or code[0].isspace():
                continue
            rest = m.group(2).strip()
            if rest:
                found.update(x.strip(" \"'") for x in rest.strip("[]").split(",") if x.strip())
                return found
            for _, sub in lines[pos + 1:]:
                if sub and not sub[0].isspace():
                    break
                mm = re.match(r"^\s{1,4}-?\s*([\w-]+)\s*:?\s*$", sub) or re.match(r"^\s{1,4}([\w-]+)\s*:", sub)
                if mm and indent_of(sub) <= 4:
                    found.add(mm.group(1))
            return found
        return found

    def jobs(self) -> list[dict]:
        """[{name, line, keys, uses, start, end}] of the jobs section."""
        lines = list(self.code_lines())
        start = next((p for p, (_, c) in enumerate(lines) if re.match(r"^jobs\s*:\s*$", c)), None)
        if start is None:
            return []
        sect = []
        for idx, code in lines[start + 1:]:
            if code and not code[0].isspace():
                break
            sect.append((idx, code))
        if not sect:
            return []
        job_indent = min(indent_of(c) for _, c in sect if c.strip())
        jobs: list[dict] = []
        for idx, code in sect:
            ind = indent_of(code)
            m = JOB_NAME.match(code)
            if m and ind == job_indent:
                jobs.append({"name": m.group("name").strip("\"'"), "line": idx + 1, "keys": {}, "start": idx, "end": idx})
                continue
            if jobs:
                job = jobs[-1]
                job["end"] = idx
                if "child" not in job:
                    job["child"] = ind
                km = KEY.match(code)
                if km and not km.group("dash") and ind == job["child"]:
                    job["keys"][km.group("key")] = (km.group("rest") or "").strip()
        for j in jobs:
            j["uses"] = j["keys"].get("uses")
        return jobs

    def is_release(self) -> bool:
        fname = Path(self.name).stem
        if RELEASE_WORDS.search(fname):
            return True
        trig = self.triggers()
        text = "\n".join(self.lines)
        if "release" in trig:
            return True
        if re.search(r"(?mi)^name\s*:.*(release|publish)", text):
            return True
        if re.search(r"tags\s*:", text) and "push" in trig:
            return True
        return bool(RELEASE_CMDS.search("\n".join(b for _, b in self.run_bodies)) or re.search(r"uses:\s*\S*action-gh-release", text))


def check_workflow(path: Path, text: str, allow_owners=OWN_OWNERS) -> list[dict]:
    wf = Workflow(text, path.name)
    out: list[dict] = []

    def add(rule, line, msg):
        out.append({"file": path.name, "rule": rule, "severity": SEVERITY[rule], "line": line, "message": msg})

    # unpinned-action
    for i, code in wf.code_lines():
        m = USES.match(code)
        if not m:
            continue
        val = m.group("val").strip("\"'")
        if val.startswith("./") or val.startswith("docker://") and "@sha256:" in val:
            continue
        if val.startswith("docker://"):
            add("unpinned-action", i + 1, f"{val} (image without a digest)")
            continue
        if "@" not in val:
            add("unpinned-action", i + 1, f"{val} (no version)")
            continue
        ident, ref = val.rsplit("@", 1)
        owner = ident.split("/", 1)[0].lower()
        if owner in allow_owners or SHA40.match(ref):
            continue
        add("unpinned-action", i + 1, f"{ident}@{ref}")

    # no-permissions
    if "permissions" not in wf.top_level_keys():
        add("no-permissions", 1, "no top-level 'permissions:'")

    # jobs
    jobs = wf.jobs()
    for j in jobs:
        if j["uses"] is None and "timeout-minutes" not in j["keys"]:
            add("no-timeout", j["line"], f"job '{j['name']}' without timeout-minutes")

    # no-concurrency (release workflows only)
    if wf.is_release() and "concurrency" not in wf.top_level_keys() and not any("concurrency" in j["keys"] for j in jobs):
        add("no-concurrency", 1, "release or publish workflow without 'concurrency'")

    # prt-checkout
    if "pull_request_target" in wf.triggers():
        for i, code in wf.code_lines():
            km = KEY.match(code)
            if km and km.group("key") in ("ref", "repository") and km.group("rest") and PR_HEAD.search(km.group("rest")):
                add("prt-checkout", i + 1, f"pull_request_target with '{km.group('key')}: {km.group('rest').strip()}' (code of the pull request with the permissions of the base repository)")

    # secret-in-run
    for line, body in wf.run_bodies:
        is_block = line in wf.body_lines   # the body starts on the next line (0-based = line)
        for k, bl in enumerate(body.split("\n")):
            if SECRET.search(bl) and not bl.lstrip().startswith("#"):
                add("secret-in-run", line + (k + 1 if is_block else 0), "secret interpolated in 'run:' (pass it through env:)")
                break
    return out


def find_repos(root: Path, skip: frozenset = frozenset()):
    if (root / ".github" / "workflows").is_dir():
        yield root.resolve().name, root
        return
    for child in sorted(root.iterdir()):
        if child.is_dir() and child.name not in SKIP_DIRS and child.name not in skip and not child.name.startswith(("_", ".")) and (child / ".github" / "workflows").is_dir():
            yield child.name, child


def scan(root: Path, only: set[str] | None = None, allow_owners=OWN_OWNERS, skip: frozenset = frozenset()) -> dict:
    repos = {}
    for name, path in find_repos(root, skip):
        if only and name not in only:
            continue
        findings, nwf = [], 0
        for wf in sorted(list((path / ".github" / "workflows").glob("*.yml")) + list((path / ".github" / "workflows").glob("*.yaml"))):
            nwf += 1
            try:
                text = wf.read_text(encoding="utf-8", errors="replace")
            except OSError as exc:
                findings.append({"file": wf.name, "rule": "no-permissions", "severity": "low", "line": 0, "message": f"could not be read: {exc}"})
                continue
            findings.extend(check_workflow(wf, text, allow_owners))
        repos[name] = {"ecosystem": ecosystem(name), "workflows": nwf, "findings": findings, "path": str(path)}
    return repos


# ---- per-repository settings and baseline (a versioned allowlist) ----
CONFIG_FILE = "tools/hygiene_config.json"           # section "workflow_hygiene": enabled, rules, min_severity, baseline, threshold
DEFAULT_BASELINE = "tools/workflow_hygiene_baseline.json"


def finding_key(f: dict) -> str:
    """Identity of a finding WITHOUT the line number (an edit that moves lines does not make it new)."""
    return f"{f['file']}|{f['rule']}|{f['message']}"


def _load_json(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, ValueError):
        return None


def repo_section(repo_path: Path, config: str | None = CONFIG_FILE) -> dict:
    data = _load_json(repo_path / config) if config else None
    sec = data.get("workflow_hygiene") if isinstance(data, dict) else None
    return sec if isinstance(sec, dict) else {}


def baseline_path(repo_path: Path, sec: dict, override: str | None = None) -> Path:
    return repo_path / (override or sec.get("baseline") or DEFAULT_BASELINE)


def load_baseline(path: Path) -> Counter:
    data = _load_json(path)
    items = data.get("findings") if isinstance(data, dict) else None
    return Counter({k: int(v) for k, v in items.items()}) if isinstance(items, dict) else Counter()


def apply_config_and_baseline(repos: dict, config: str | None = CONFIG_FILE, baseline: str | None = None) -> None:
    """Apply the settings of each repository (enabled/rules/min_severity) and mark every finding as new or as part of the baseline."""
    for r in repos.values():
        rp = Path(r["path"])
        sec = repo_section(rp, config)
        r["disabled"] = sec.get("enabled") is False
        if r["disabled"]:
            r["findings"] = []
        else:
            rules = set(sec["rules"]) if isinstance(sec.get("rules"), list) else None
            floor = SEV_ORDER.get(sec.get("min_severity", "low"), 0)
            r["findings"] = [f for f in r["findings"] if (not rules or f["rule"] in rules) and SEV_ORDER[f["severity"]] >= floor]
        left = load_baseline(baseline_path(rp, sec, baseline))
        for f in r["findings"]:
            k = finding_key(f)
            if left[k] > 0:
                left[k] -= 1
                f["new"] = False
            else:
                f["new"] = True
        r["resolved"] = sum(left.values())   # baseline entries that no longer exist (they can be pruned)
        r["threshold"] = sec.get("threshold")


def write_baselines(repos: dict, config: str | None = CONFIG_FILE, baseline: str | None = None) -> list[str]:
    written = []
    for r in repos.values():
        rp = Path(r["path"])
        path = baseline_path(rp, repo_section(rp, config), baseline)
        cnt = Counter(finding_key(f) for f in r["findings"])
        path.parent.mkdir(parents=True, exist_ok=True)
        doc = {"_comment": "Known workflow findings; only findings not listed here fail the check. Regenerate with workflow_hygiene.py --write-baseline.",
               "findings": dict(sorted(cnt.items()))}
        path.write_text(json.dumps(doc, ensure_ascii=False, indent=1) + chr(10), encoding="utf-8")
        written.append(str(path))
    return written


def filter_findings(repos: dict, rules=None, min_severity="low") -> None:
    for r in repos.values():
        r["findings"] = [f for f in r["findings"] if (not rules or f["rule"] in rules) and SEV_ORDER[f["severity"]] >= SEV_ORDER[min_severity]]


def score(findings) -> int:
    return sum(WEIGHT[f["severity"]] for f in findings)


def summarize(repos: dict) -> dict:
    eco: defaultdict[str, dict[str, Any]] = defaultdict(lambda: {"repos": 0, "workflows": 0, "findings": 0, "score": 0, "by_rule": Counter()})
    for r in repos.values():
        e = eco[r["ecosystem"]]
        e["repos"] += 1
        e["workflows"] += r["workflows"]
        e["findings"] += len(r["findings"])
        e["score"] += score(r["findings"])
        e["by_rule"].update(f["rule"] for f in r["findings"])
    worst = sorted(((score(r["findings"]), n) for n, r in repos.items()), reverse=True)
    return {
        "ecosystems": {k: {**v, "by_rule": dict(v["by_rule"])} for k, v in sorted(eco.items())},
        "total_findings": sum(len(r["findings"]) for r in repos.values()),
        "total_new": sum(1 for r in repos.values() for f in r["findings"] if f.get("new", True)),
        "total_repos": len(repos),
        "total_workflows": sum(r["workflows"] for r in repos.values()),
        "by_rule": dict(Counter(f["rule"] for r in repos.values() for f in r["findings"])),
        "worst": [{"repo": n, "score": s, "findings": len(repos[n]["findings"])} for s, n in worst[:10] if s],
    }


def render_table(repos: dict, summary: dict, detail: bool = False) -> str:
    short = {"unpinned-action": "unpin", "no-timeout": "notmo", "no-permissions": "noperm", "no-concurrency": "nocon", "prt-checkout": "prt", "secret-in-run": "secret"}
    head = ["repo", "eco", "wf"] + [short[r] for r in RULES] + ["score"]
    rows = []
    for name, r in sorted(repos.items(), key=lambda kv: (-score(kv[1]["findings"]), kv[0])):
        cnt = Counter(f["rule"] for f in r["findings"])
        rows.append([name, r["ecosystem"], str(r["workflows"])] + [str(cnt.get(x, 0)) for x in RULES] + [str(score(r["findings"]))])
    widths = [max(len(head[i]), *(len(row[i]) for row in rows)) if rows else len(head[i]) for i in range(len(head))]
    lines = ["  ".join(h.ljust(widths[i]) for i, h in enumerate(head))]
    lines.append("  ".join("-" * w for w in widths))
    for row in rows:
        lines.append("  ".join(c.ljust(widths[i]) if i < 2 else c.rjust(widths[i]) for i, c in enumerate(row)))
    lines += ["", "BY ECOSYSTEM"]
    for eco, e in summary["ecosystems"].items():
        parts = ", ".join(f"{k}={v}" for k, v in sorted(e["by_rule"].items()))
        lines.append(f"  {eco:<6} repos={e['repos']:<3} workflows={e['workflows']:<4} findings={e['findings']:<5} points={e['score']:<5} {parts}")
    lines.append(f"NEW (not in the baseline): {summary['total_new']}")
    lines.append(f"TOTAL: {summary['total_findings']} findings in {summary['total_repos']} repositories / {summary['total_workflows']} workflows")
    if detail:
        lines.append("")
        for name, r in sorted(repos.items()):
            for f in r["findings"]:
                lines.append(f"{name}/{f['file']}:{f['line']} [{f['severity']}] {'NEW ' if f.get('new', True) else ''}{f['rule']}: {f['message']}")
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Static hygiene check of .github/workflows (read only).")
    ap.add_argument("--root", default=str(DEFAULT_ROOT), help="folder with the clones, or a repository (default: the current folder)")
    ap.add_argument("--repo", action="append", help="limit to this repository (repeatable)")
    ap.add_argument("--rules", help="rules to show, comma separated: " + ",".join(RULES))
    ap.add_argument("--min-severity", choices=list(SEV_ORDER), default="low")
    ap.add_argument("--skip", action="append", default=[], help="folder name to skip when root holds several repositories (repeatable)")
    ap.add_argument("--allow-owner", action="append", default=[], help="extra owner whose actions need not be pinned")
    ap.add_argument("--json", action="store_true", help="JSON output")
    ap.add_argument("--detail", action="store_true", help="list every finding with file and line")
    ap.add_argument("--check", action="store_true", help="exit 1 if the NEW findings (not in the baseline) exceed the threshold")
    ap.add_argument("--threshold", type=int, default=None, help="new findings tolerated with --check (default: 'threshold' of the repository settings, or 0)")
    ap.add_argument("--config", default=CONFIG_FILE, help="settings file inside each repository (default " + CONFIG_FILE + "); 'none' ignores it")
    ap.add_argument("--baseline", help="baseline inside each repository (default: the one of the settings or " + DEFAULT_BASELINE + ")")
    ap.add_argument("--write-baseline", action="store_true", help="WRITE the baseline of each analysed repository with today's findings (the only option that writes)")
    args = ap.parse_args(argv)
    root = Path(args.root)
    if not root.is_dir():
        print(f"root does not exist: {root}", file=sys.stderr)
        return 2
    rules = set(args.rules.split(",")) if args.rules else None
    if rules and not rules <= set(RULES):
        print("unknown rule: " + ", ".join(sorted(rules - set(RULES))), file=sys.stderr)
        return 2
    owners = OWN_OWNERS + tuple(o.lower() for o in args.allow_owner)
    repos = scan(root, set(args.repo) if args.repo else None, owners, frozenset(args.skip))
    cfg = None if args.config == "none" else args.config
    apply_config_and_baseline(repos, cfg, args.baseline)
    filter_findings(repos, rules, args.min_severity)
    if args.write_baseline:
        for path in write_baselines(repos, cfg, args.baseline):
            print("baseline written: " + path, file=sys.stderr)
        apply_config_and_baseline(repos, cfg, args.baseline)
    summary = summarize(repos)
    if args.json:
        print(json.dumps({"root": str(root), "summary": summary, "repos": repos}, ensure_ascii=False, indent=1))
    else:
        print(render_table(repos, summary, args.detail))
    if args.check:
        threshold = args.threshold
        if threshold is None:
            threshold = sum(int(r.get("threshold") or 0) for r in repos.values())
        over = summary["total_new"] > threshold
        print(f"WORKFLOW_HYGIENE={'FAIL' if over else 'PASS'} new={summary['total_new']} findings={summary['total_findings']} threshold={threshold}", file=sys.stderr if args.json else sys.stdout)
        return 1 if over else 0
    return 0


def run_in_ci(root: Path) -> list[str]:
    """The step of ci_validate: problems (one line each) only when the repository opted in with tools/hygiene_config.json or a baseline.
    Without either the step does not block."""
    root = Path(root)
    if not (root / ".github" / "workflows").is_dir():
        return []
    sec = repo_section(root)
    if not (root / CONFIG_FILE).is_file() and not baseline_path(root, sec).is_file():
        return []
    if sec.get("enabled") is False:
        return []
    repos = scan(root)
    apply_config_and_baseline(repos)
    problems = []
    threshold = sum(int(r.get("threshold") or 0) for r in repos.values())
    new = [f for r in repos.values() for f in r["findings"] if f.get("new", True)]
    if len(new) > threshold:
        for f in new[:20]:
            problems.append(f".github/workflows/{f['file']}:{f['line']} {f['rule']}: {f['message']}")
        problems.append(f"workflow hygiene: {len(new)} new finding(s), threshold {threshold} (python tools/workflow_hygiene.py --root . --detail)")
    return problems


if __name__ == "__main__":
    sys.exit(main())
