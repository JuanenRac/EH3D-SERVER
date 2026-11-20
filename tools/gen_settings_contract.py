#!/usr/bin/env python3
# =============================================================================
# EH3D-SERVER - tools/gen_settings_contract.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Writes the contract of the settings of the server, `docs/settings-contract.json` (`eh3d.settings.contract/1`).

    python tools/gen_settings_contract.py            # writes docs/settings-contract.json
    python tools/gen_settings_contract.py --check    # exit 1 when the file is not what the code and the texts give
    python tools/gen_settings_contract.py --summary  # the number of settings by group

What a setting is (its type, its range, its default, whether it changes while the server runs, which route changes it) is READ FROM THE CODE: the
keys that `parse` of `src/config.rs` accepts with their ranges, the defaults of `ServerConfig::default()` and the lists of `src/policy.rs`. What a person
reads about it (group, risk, label, short help, in the eight languages) is in `tools/settings_contract_texts.py`. The check fails when
a key of the code has no texts, when the texts name a key the code does not have, when a language is missing, when `server.cfg.example` and the code
disagree on the keys or on the defaults, and when the file on disk is not the one these sources give. The server serves the same document at
`GET /api/v1/settings/contract` and the clients copy `docs/settings-contract.json` and run this check against it (see docs/CLIENT_SYNC.md).
Standard library only."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import settings_contract_texts as T  # noqa: E402

SCHEMA = "eh3d.settings.contract/1"
OUT = ROOT / "docs" / "settings-contract.json"
POLICY_PATH = "/api/v1/auth/policy"
UNITS = {"_s": "seconds", "_mb": "megabytes", "_kb": "kilobytes", "_gb": "gigabytes", "_hours": "hours", "_days": "days", "_lines": "lines", "_slots": "places"}
PATH_KEYS_FORMAT = {"listen": "address", "mdns_name": "name", "auth_extra_apps": "app-names"}


def read(name: str) -> str:
    return (ROOT / name).read_text(encoding="utf-8").replace("\r\n", "\n")


def parse_arms(config: str) -> dict[str, dict]:
    """The keys that `parse` accepts, with the type and the range each arm checks."""
    start = config.index("match key.as_str() {")
    end = config.index("other => return Err(at(", start)
    pieces = re.split(r'(?m)^\s{12}"([a-z0-9_]+)"\s*=>', config[start:end])
    found: dict[str, dict] = {}
    for key, body in zip(pieces[1::2], pieces[2::2]):
        number = re.search(r"number\(&key,\s*value,\s*([\d_]+),\s*([\d_]+)\)", body)
        if number:
            found[key] = {"type": "integer", "min": int(number.group(1).replace("_", "")), "max": int(number.group(2).replace("_", ""))}
        elif "flag(&key, value)" in body or '"true" | "yes"' in body:
            found[key] = {"type": "boolean"}
        elif "split(',')" in body:
            found[key] = {"type": "list", "format": PATH_KEYS_FORMAT.get(key, "text")}
        elif "PathBuf::from" in body:
            found[key] = {"type": "string", "format": "path"}
        else:
            entry = {"type": "string", "format": PATH_KEYS_FORMAT.get(key, "text")}
            length = re.search(r"count\(\) > (\d+)", body)
            if length:
                entry["max_length"] = int(length.group(1))
            found[key] = entry
    return found


def parse_defaults(config: str) -> dict[str, object]:
    block = config[config.index("impl Default for ServerConfig"):]
    block = block[:block.index("\n}\n")]
    out: dict[str, object] = {}
    for name, expr in re.findall(r"(?m)^\s+([a-z0-9_]+): (.+),$", block):
        if expr in ("true", "false"):
            out[name] = expr == "true"
        elif re.fullmatch(r"[\d_]+", expr):
            out[name] = int(expr.replace("_", ""))
        elif expr in ("None", "String::new()"):
            out[name] = ""
        elif expr == "Vec::new()":
            out[name] = []
        elif m := re.fullmatch(r'PathBuf::from\("([^"]*)"\)', expr):
            out[name] = m.group(1)
        elif m := re.fullmatch(r'"([^"]*)"\.parse\(\)\.expect\(.*\)', expr):
            out[name] = m.group(1)
        else:
            raise SystemExit(f"gen_settings_contract: cannot read the default of {name}: {expr}")
    return out


def rust_list(policy: str, name: str) -> list[str]:
    match = re.search(r"pub const " + name + r": &\[&str\] = &\[(.*?)\];", policy, re.S)
    if not match:
        raise SystemExit(f"gen_settings_contract: src/policy.rs has no {name}")
    return re.findall(r'"([a-z0-9_]+)"', match.group(1))


def example_values(text: str) -> tuple[set[str], dict[str, str]]:
    """The keys that server.cfg.example names (also in a commented line) and the values of the lines that are not commented."""
    keys = set(re.findall(r"(?m)^#?\s*([a-z0-9_]+) =", text))
    values = {k: v.strip() for k, v in re.findall(r"(?m)^([a-z0-9_]+) =(.*)$", text)}
    return keys, values


def build() -> tuple[dict, list[str]]:
    problems: list[str] = []
    config, policy = read("src/config.rs"), read("src/policy.rs")
    arms, defaults = parse_arms(config), parse_defaults(config)
    hot, restart = rust_list(policy, "KEYS"), rust_list(policy, "RESTART_KEYS")
    texts = T.SETTINGS
    for key in sorted(set(arms) - set(texts)):
        problems.append(f"{key}: a key of src/config.rs with no entry in tools/settings_contract_texts.py")
    for key in sorted(set(texts) - set(arms)):
        problems.append(f"{key}: an entry of tools/settings_contract_texts.py that src/config.rs does not accept")
    for key in sorted(set(arms) - set(defaults)):
        problems.append(f"{key}: no default in ServerConfig::default()")
    for key in hot:
        if key not in arms:
            problems.append(f"{key}: in the KEYS of src/policy.rs but not a key of server.cfg")
    for key in restart:
        if key not in hot:
            problems.append(f"{key}: in RESTART_KEYS of src/policy.rs but not in KEYS")
    example_keys, example_set = example_values(read("server.cfg.example"))
    for key in sorted(set(arms) - example_keys):
        problems.append(f"{key}: not in server.cfg.example")
    for key in sorted(example_keys - set(arms)):
        problems.append(f"{key}: in server.cfg.example but not a key of src/config.rs")
    for key, shown in example_set.items():
        if key in defaults and arms.get(key, {}).get("type") in ("boolean", "integer", "string"):
            want = defaults[key]
            have: object = shown
            if isinstance(want, bool):
                have = shown.lower() == "true" if shown.lower() in ("true", "false") else shown
            elif isinstance(want, int):
                have = int(shown) if shown.isdigit() else shown
            if have != want:
                problems.append(f"{key}: server.cfg.example says {shown!r}, the default of the code is {want!r}")
    auth_doc = read("docs/AUTH.md")
    for key in hot:
        if f"`{key}`" not in auth_doc:
            problems.append(f"{key}: a switch that changes while the server runs but docs/AUTH.md does not name it")
    for group, labels in T.GROUPS.items():
        if len(labels) != 8 or not all(labels):
            problems.append(f"group {group}: needs eight texts")
    for name, table in (("APPLY_NOTES", T.APPLY_NOTES), ("RISK_NOTES", T.RISK_NOTES)):
        for kind, notes in table.items():
            if notes is not None and (len(notes) != 8 or not all(notes)):
                problems.append(f"{name}.{kind}: needs eight texts")
    langs = T.LANGUAGES
    settings = []
    for key, (group, risk, label, help_) in texts.items():
        if key not in arms:
            continue
        if group not in T.GROUPS:
            problems.append(f"{key}: unknown group {group}")
        if risk not in T.RISK_NOTES:
            problems.append(f"{key}: unknown risk {risk}")
        for what, value in (("label", label), ("help", help_)):
            if len(value) != 8 or not all(isinstance(v, str) and v.strip() for v in value):
                problems.append(f"{key}: {what} needs eight texts ({', '.join(langs)})")
        in_policy = key in hot
        apply = "restart" if (not in_policy or key in restart) else "now"
        note_kind = "hot" if apply == "now" else ("restart_policy" if in_policy else "restart_file")
        risk_note = T.RISK_NOTES.get(risk)
        warning = {
            lang: T.APPLY_NOTES[note_kind][i] + (("" if lang in ("ja", "zh") else " ") + risk_note[i] if risk_note else "")
            for i, lang in enumerate(langs)
        }
        entry = {"key": key, "group": group, **arms[key], "default": defaults.get(key)}
        for suffix, unit in UNITS.items():
            if key.endswith(suffix) and arms[key]["type"] == "integer":
                entry["unit"] = unit
        entry.update({
            "apply": apply,
            "write": ({"via": "policy", "method": "PUT", "path": POLICY_PATH, "body": {"key": key, "value": "<value>", "confirm_weak": "<true when the new value is looser than recommended>"}}
                      if in_policy else {"via": "file", "method": None, "path": None, "file": "server.cfg"}),
            "role": "admin" if in_policy else None,
            "scope": "admin:policy" if in_policy else None,
            "confirm_when_looser": in_policy,
            "secret": key in T.SECRET_KEYS,
            "risk": risk,
            "label": dict(zip(langs, label)),
            "help": dict(zip(langs, help_)),
            "warning": warning,
        })
        settings.append(entry)
    doc = {
        "schema": SCHEMA,
        "languages": langs,
        "read": {"method": "GET", "path": "/api/v1/settings/contract", "role": "admin", "scope": "admin:policy",
                 "values": "The answer adds `values` (the value in force of each key; a secret key has no value) to this document."},
        "write": {
            "policy": {"method": "PUT", "path": POLICY_PATH, "role": "admin", "scope": "admin:policy",
                       "body": {"key": "string", "value": "boolean | integer", "confirm_weak": "boolean, default false"},
                       "applies": "At once, except the keys with apply = restart, which are kept at once and read when the server starts; the answer lists them in `restart_required`.",
                       "errors": {"bad_policy": "422: the key is not one the policy changes, or the value does not fit its type or range; the detail names the key.",
                                  "weak_confirm_required": "409: the new value is looser than recommended; send confirm_weak: true.",
                                  "forbidden": "403: the caller is not an administrator.", "forbidden_scope": "403: the token does not carry admin:policy."},
                       "audit": "policy_change: who, the key, the old and the new value (a secret key is written as ***)."},
            "file": {"method": None, "file": "server.cfg",
                     "applies": "After the server restarts. These keys cannot be changed through the API; the contract lists them so that a client shows them and says where they are set."},
        },
        "groups": [{"id": group, "label": dict(zip(langs, labels))} for group, labels in T.GROUPS.items() if any(v[0] == group for v in texts.values())],
        "settings": settings,
    }
    return doc, problems


def render(doc: dict) -> str:
    return json.dumps(doc, ensure_ascii=False, indent=2) + "\n"


def main(argv: list[str]) -> int:
    if "--help" in argv or "-h" in argv:
        print(__doc__)
        return 0
    doc, problems = build()
    if "--summary" in argv:
        counts: dict[str, int] = {}
        for item in doc["settings"]:
            counts[item["group"]] = counts.get(item["group"], 0) + 1
        for group, n in counts.items():
            print(f"{group}: {n}")
        policy = sum(1 for i in doc["settings"] if i["write"]["via"] == "policy")
        print(f"total: {len(doc['settings'])} (policy {policy}, file {len(doc['settings']) - policy})")
        return 0
    if problems:
        print("SETTINGS_CONTRACT=FAIL")
        for line in problems:
            print("  " + line)
        return 1
    text = render(doc)
    if "--check" in argv:
        have = OUT.read_text(encoding="utf-8").replace("\r\n", "\n") if OUT.exists() else ""
        if have != text:
            print("SETTINGS_CONTRACT=FAIL")
            print("  docs/settings-contract.json is not what the code and tools/settings_contract_texts.py give: run python tools/gen_settings_contract.py")
            return 1
        print(f"SETTINGS_CONTRACT=PASS ({len(doc['settings'])} settings, {len(doc['languages'])} languages)")
        return 0
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT)} ({len(doc['settings'])} settings)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
