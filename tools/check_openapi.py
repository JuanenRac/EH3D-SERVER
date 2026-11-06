#!/usr/bin/env python3
# =============================================================================
# EH3D-SERVER - tools/check_openapi.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Checks that the contract of the interface (docs/openapi.json, docs/openapi.yaml) and the table of docs/ROUTES.md say what the router does.

    python -I tools/check_openapi.py        # prints OPENAPI_CHECK=PASS or FAIL with the reasons

It checks: the document reads and is OpenAPI 3.1 with the title and the version of the server; every route (method and path) that the router registers in
`src/` is in the document and the document has no other; every operation has an id, a summary, a tag, a role and responses; every `$ref` resolves; the
error codes an operation names are in `docs/error-codes.json` with a status the operation answers; the YAML and ROUTES.md carry the same routes as the
JSON and are what `tools/make_openapi.py` writes; and `docs/API.md` names the contract. It needs no package outside the standard library."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
METHODS = ("get", "post", "put", "delete", "head")
ROUTER_FILES = ("api.rs", "api_tasks.rs", "api_artifacts.rs", "api_projects.rs", "api_pstore.rs", "api_catalogs.rs", "api_work.rs", "api_auth.rs")
PREFIX = "/api/v1"


def router_routes(root: Path) -> set[tuple[str, str]]:
    """(METHOD, path) of every `.route("path", get(..).post(..))` the files of the router register, with `:name` parameters."""
    found: set[tuple[str, str]] = set()
    for name in ROUTER_FILES:
        text = (root / "src" / name).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
        for match in re.finditer(r"\.route\(\s*\"([^\"]+)\"", text):
            depth, index = 1, text.index("(", match.start()) + 1
            while depth and index < len(text):
                depth += {"(": 1, ")": -1}.get(text[index], 0)
                index += 1
            arguments = text[match.end():index - 1]
            for method in METHODS:
                if re.search(rf"(?<![A-Za-z0-9_]){method}\(", arguments):
                    found.add((method.upper(), match.group(1)))
    return found


def document_routes(doc: dict) -> set[tuple[str, str]]:
    out = set()
    for path, item in doc["paths"].items():
        for method in item:
            out.add((method.upper(), PREFIX + re.sub(r"\{(\w+)\}", r":\1", path)))
    return out


def refs_of(value, found: list[str]) -> None:
    if isinstance(value, dict):
        for key, item in value.items():
            if key == "$ref" and isinstance(item, str):
                found.append(item)
            else:
                refs_of(item, found)
    elif isinstance(value, list):
        for item in value:
            refs_of(item, found)


def problems_of(root: Path) -> list[str]:
    problems: list[str] = []
    docs = root / "docs"
    try:
        doc = json.loads((docs / "openapi.json").read_text(encoding="utf-8"))
        catalog = {item["code"]: item["status"] for item in json.loads((docs / "error-codes.json").read_text(encoding="utf-8"))}
    except (OSError, ValueError, KeyError, TypeError) as error:
        return [f"docs/openapi.json or docs/error-codes.json cannot be read ({error.__class__.__name__}: {error})"]
    cargo = re.search(r'(?m)^version\s*=\s*"([^"]+)"', (root / "Cargo.toml").read_text(encoding="utf-8"))
    if not str(doc.get("openapi", "")).startswith("3.1"):
        problems.append("docs/openapi.json is not OpenAPI 3.1")
    info = doc.get("info", {})
    if info.get("title") != "EH3D Server API":
        problems.append("the title of the document is not 'EH3D Server API'")
    if not cargo or info.get("version") != cargo.group(1):
        problems.append(f"the version of the document ({info.get('version')}) is not the version of Cargo.toml")
    paths = doc.get("paths")
    if not isinstance(paths, dict) or not paths:
        return problems + ["docs/openapi.json has no paths"]

    code_routes, doc_routes = router_routes(root), document_routes(doc)
    if not code_routes:
        problems.append("the scan of the router found no route: the check is broken")
    for method, path in sorted(code_routes - doc_routes):
        problems.append(f"{method} {path} is in the router but not in docs/openapi.json")
    for method, path in sorted(doc_routes - code_routes):
        problems.append(f"{method} {path} is in docs/openapi.json but no route registers it")

    tags = {tag.get("name") for tag in doc.get("tags", [])}
    ids: set[str] = set()
    for path, item in paths.items():
        for method, operation in item.items():
            label = f"{method.upper()} {path}"
            if method not in METHODS:
                problems.append(f"{label}: '{method}' is not a method the router uses")
                continue
            if not str(operation.get("summary", "")).strip():
                problems.append(f"{label} has no summary")
            if not operation.get("responses"):
                problems.append(f"{label} has no responses")
            if not operation.get("tags") or not set(operation["tags"]) <= tags:
                problems.append(f"{label} has no tag, or one that the document does not declare")
            if operation.get("x-role") not in ("none", "any", "person", "viewer", "operator", "admin"):
                problems.append(f"{label} does not say who may call it (x-role)")
            operation_id = operation.get("operationId")
            if not operation_id or operation_id in ids:
                problems.append(f"{label} has no operationId or one that another operation has")
            ids.add(str(operation_id))
            answered = set(operation.get("responses", {}))
            for code in operation.get("x-error-codes", []):
                if code not in catalog:
                    problems.append(f"{label} names the error code {code}, which docs/error-codes.json does not have")
                elif str(catalog[code]) not in answered:
                    problems.append(f"{label} names {code} but has no response for its status {catalog[code]}")
            names = set(re.findall(r"\{(\w+)\}", path))
            declared = {p["name"] for p in operation.get("parameters", []) if p.get("in") == "path"}
            if names != declared:
                problems.append(f"{label}: the parameters of the path {sorted(names)} are not the ones declared {sorted(declared)}")

    refs: list[str] = []
    refs_of(doc, refs)
    for reference in sorted(set(refs)):
        target: object = doc
        for part in reference.removeprefix("#/").split("/"):
            target = target.get(part) if isinstance(target, dict) else None
        if target is None:
            problems.append(f"{reference} does not resolve")

    yaml_path = docs / "openapi.yaml"
    yaml_text = yaml_path.read_text(encoding="utf-8") if yaml_path.is_file() else ""
    for path in paths:
        if f'\n  "{path}":' not in yaml_text:
            problems.append(f"docs/openapi.yaml lacks the path {path}")
    yaml_paths = set(re.findall(r'(?m)^  "(/[^"]*)":', yaml_text))
    for path in sorted(yaml_paths - set(paths)):
        problems.append(f"docs/openapi.yaml has the path {path}, which docs/openapi.json does not")
    if not yaml_text.startswith("#") or "openapi: " not in yaml_text:
        problems.append("docs/openapi.yaml does not look like the OpenAPI document")

    table = (docs / "ROUTES.md").read_text(encoding="utf-8") if (docs / "ROUTES.md").is_file() else ""
    rows = set(re.findall(r"(?m)^\| `([^`]+)` \| ([A-Z]+) \|", table))
    listed = {(method, path) for path, method in rows}
    for method, path in sorted(code_routes - listed):
        problems.append(f"{method} {path} is not in docs/ROUTES.md")
    for method, path in sorted(listed - code_routes):
        problems.append(f"docs/ROUTES.md lists {method} {path}, which no route registers")

    api = (docs / "API.md").read_text(encoding="utf-8")
    if "openapi.yaml" not in api or "ROUTES.md" not in api:
        problems.append("docs/API.md does not name docs/openapi.yaml and docs/ROUTES.md as the contract of the routes")

    folder = str(Path(__file__).resolve().parent)
    if folder not in sys.path:
        sys.path.insert(0, folder)
    try:
        import make_openapi
    except ImportError as error:
        return problems + [f"tools/make_openapi.py does not import ({error})"]
    if Path(make_openapi.ROOT).resolve() == root.resolve():
        for name, text in make_openapi.outputs().items():
            path = root / name
            current = path.read_bytes().decode("utf-8").replace("\r\n", "\n") if path.is_file() else None
            if current != text:
                problems.append(f"{name} is not what tools/make_openapi.py writes: run it")
    return problems


def run_in_ci(root: Path) -> list[str]:
    return problems_of(root)


def main() -> int:
    problems = problems_of(ROOT)
    for problem in problems:
        print("  FAIL", problem)
    print("OPENAPI_CHECK=" + ("FAIL" if problems else "PASS"))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
