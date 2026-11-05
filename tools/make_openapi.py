#!/usr/bin/env python3
# =============================================================================
# EH3D-SERVER - tools/make_openapi.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Writes the contract of the REST interface from the table below (no third-party package needed).

    python tools/make_openapi.py            # writes docs/openapi.json, docs/openapi.yaml and docs/ROUTES.md
    python tools/make_openapi.py --check    # exit 1 when one of the three files is not what the table gives

The table (OPERATIONS) is the one place where a route is described: its tag, who may call it (role and scope), what it takes and gives, and the
error codes it can answer with. The codes come from docs/error-codes.json (their status too), so a code that is not in the catalog stops the
build. `docs/openapi.json` is the document the tests read; `docs/openapi.yaml` is the same document in YAML for people and for the tools that
want YAML; `docs/ROUTES.md` is the table route / method / auth / description. `tools/check_openapi.py` and `tests/openapi.rs` compare the
document with the routes the router registers in `src/`.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PREFIX = "/api/v1"

# ---------------------------------------------------------------------------------------------------------------------------------------
# Reusable pieces

def ref(name: str) -> dict:
    return {"$ref": f"#/components/schemas/{name}"}


def obj(props: dict, required: list[str] | None = None, extra: bool = False, description: str = "") -> dict:
    out: dict = {"type": "object", "properties": props}
    if required:
        out["required"] = required
    if not extra:
        out["additionalProperties"] = False
    if description:
        out["description"] = description
    return out


S = {"type": "string"}
I = {"type": "integer"}
N = {"type": "number"}
B = {"type": "boolean"}


def s(description: str = "", **more) -> dict:
    return {"type": "string", **({"description": description} if description else {}), **more}


def arr(item: dict, **more) -> dict:
    return {"type": "array", "items": item, **more}


def nullable(schema: dict) -> dict:
    return {"oneOf": [schema, {"type": "null"}]}


NUMBER_PAIR = arr(N, minItems=2, maxItems=2)

SCHEMAS: dict[str, dict] = {
    "ErrorBody": obj({"error": s("A code of the catalog (docs/error-codes.json)."), "detail": s("Words for a person; may be empty.")}, ["error", "detail"],
                     description="Every refusal has this shape, with the status that fits the code."),
    "Ok": obj({"ok": {"type": "boolean", "const": True}}, ["ok"]),
    "Role": {"type": "string", "enum": ["viewer", "operator", "admin"]},
    "Health": obj({
        "name": s(), "version": s("The version of the server."), "api_revision": I, "status": {"type": "string", "enum": ["ok", "degraded"]},
        "uptime_s": I, "storage": obj({"writable": B}), "recovered_locks": I,
        "features": obj({k: B for k in ["serial", "studio", "updates", "url_token", "local_until_setup", "trust_forwarded_for", "exit_on_panic", "persist_sessions"]}),
    }, ["name", "version", "api_revision", "status"], description="`uptime_s`, `storage`, `recovered_locks` and `features` come only with a valid token."),
    "SetupState": obj({"needed": B}, ["needed"]),
    "SetupRequest": obj({"code": s("The set-up code the server prints on its console."), "name": s(), "password": s(minLength=8, maxLength=128)}, ["code", "name", "password"]),
    "LoginRequest": obj({"name": s(), "password": s()}, ["name", "password"]),
    "LoginResponse": obj({"token": s(), "name": s(), "role": ref("Role"), "expires_s": I}, ["token", "name", "role", "expires_s"]),
    "Session": obj({"name": s(), "role": ref("Role"), "is_token": B}, ["name", "role", "is_token"]),
    "PasswordChange": obj({"old": s(), "new": s(minLength=8, maxLength=128)}, ["old", "new"]),
    "User": obj({"name": s(), "role": ref("Role"), "disabled": B}, ["name", "role", "disabled"]),
    "NewUser": obj({"name": s("Letters, digits, `.`, `_` and `-`, up to 32.", maxLength=32), "role": ref("Role"), "password": s(minLength=8, maxLength=128)},
                   ["name", "role", "password"]),
    "UserChange": obj({"role": ref("Role"), "password": s(minLength=8, maxLength=128), "disabled": B}),
    "Token": obj({
        "id": s(), "name": s(), "role": ref("Role"), "created": I, "last_used": nullable(I), "expires": nullable(I),
        "template": nullable(s()), "scopes": nullable(arr(S)),
    }, ["id", "name", "role", "created"], extra=True, description="Seconds since 1970 for `created`, `last_used` and `expires`; the secret is never listed."),
    "NewToken": obj({
        "name": s(), "role": ref("Role"), "days": {"type": "integer", "minimum": 1, "maximum": 3650},
        "template": {"type": "string", "enum": ["phoenix", "cam", "client", "custom"]},
        "scopes": arr(S, description="Scopes that narrow the role, see the scopes table of docs/API.md."),
    }, ["name", "role"]),
    "TokenSecret": obj({"id": s(), "token": s("Shown this once.")}, ["id", "token"]),
    "Connection": obj({
        "kind": {"type": "string", "enum": ["node", "demo", "grbl"]}, "address": s("host:port of the node."),
        "token": s("The node's access token; never shown by the API (`token_set` instead), and empty on an update keeps the stored one."),
        "token_set": B, "port": s("Serial port of a GRBL controller."), "baud": {"type": "integer", "minimum": 1200, "maximum": 2000000},
    }, ["kind"]),
    "Machine": obj({
        "id": s("Lowercase letters, digits and hyphens, up to 32."), "name": s(), "model": s(), "station": s("The bench, room or workshop; empty when none."),
        "connection": ref("Connection"), "dcf": s("The text of DRILLPRO.DCF."), "rcf": s("The text of ROUTEPRO.RCF."),
        "cfg": s("The tuning text of the machine; keys that hold secrets are shown empty."),
        "spindle": s(), "spindle_guard": s(), "spindle_original": {"type": "object"}, "spindle_vfd": {"type": "object"}, "allow_dollar": B,
    }, ["id", "name", "connection"], extra=True),
    "FleetJob": obj({"id": s(), "percent": N, "items_done": I, "items_total": I, "pause": nullable({"type": "object"}), "tool": nullable(I)}, extra=True),
    "Fleet": obj({
        "stations": arr(obj({"id": s(), "machines": arr(S)}, ["id", "machines"])),
        "machines": arr(obj({
            "id": s(), "name": s(), "model": s(), "station": s(), "connection": ref("Connection"),
            "state": {"type": "string", "enum": ["idle", "busy", "waiting"]}, "job": nullable(ref("FleetJob")), "ready": I,
        }, ["id", "state"], extra=True)),
        "summary": {"type": "object", "additionalProperties": True},
    }, ["stations", "machines", "summary"]),
    "DrillTool": obj({"number": I, "extra_stroke_mm": N, "feed_mm_min": N, "spindle_rpm": N}, ["number"], extra=True),
    "RouteTool": obj({"number": I, "depth_mm": N, "iterations": I, "feed_xy_mm_min": N, "feed_z_mm_min": N, "spindle_rpm": N}, ["number"], extra=True),
    "DrillRequest": obj({
        "kind": {"type": "string", "const": "drill"}, "name": s(), "board_mm": {"type": "number", "minimum": 0, "maximum": 50},
        "tools": arr(ref("DrillTool")), "holes": arr(obj({"tool": I, "x_mm": N, "y_mm": N}, ["tool", "x_mm", "y_mm"])),
        "files": obj({"ncd": s(), "dtd": s()}), "sorting": {"type": "string", "enum": ["none", "x", "y", "special"]},
        "skip": I, "skip_tools": arr(I),
    }, ["kind"], description="The holes or the files, not both (`both_sources`)."),
    "RouteRequest": obj({
        "kind": {"type": "string", "const": "route"}, "name": s(), "tools": arr(ref("RouteTool")),
        "paths": arr(obj({"pen": I, "points_mm": arr(NUMBER_PAIR)}, ["pen", "points_mm"])),
        "files": obj({"plt": s(), "rtd": s()}), "skip": I, "skip_tools": arr(I),
    }, ["kind"], description="The paths or the files, not both (`both_sources`)."),
    "GcodeRequest": obj({
        "kind": {"type": "string", "const": "gcode"}, "name": s(), "gcode": s("The text of the program; at most a million lines."),
        "skip": I, "restore_outputs": B,
    }, ["kind", "gcode"]),
    "JobRequest": {"oneOf": [ref("DrillRequest"), ref("RouteRequest"), ref("GcodeRequest")], "discriminator": {"propertyName": "kind"}},
    "Pause": obj({"kind": {"type": "string", "enum": ["tool_change", "spindle_start", "hold", "resume"]}, "tool": I}, ["kind"]),
    "JobView": obj({
        "kind": s(), "name": s(), "tools": arr({"type": "object"}), "holes": arr({"type": "object"}), "paths": arr({"type": "object"}),
        "bounds_mm": arr(N), "items": I, "hole_ends": arr(I), "estimate_s": N, "travel_mm": N, "skipped_tools": arr(I), "skipped": I,
        "dry_run": {"type": "object"}, "warnings": arr({"type": "object"}), "spindle_warnings": arr({"type": "object"}),
    }, extra=True, description="What a client draws and follows; see `The view` in docs/API.md."),
    "JobSummary": obj({
        "id": s(), "machine": s(), "name": s(), "kind": s(),
        "state": {"type": "string", "enum": ["ready", "connecting", "running", "waiting_operator", "done", "cancelled", "failed"]},
        "created": I, "started": nullable(I), "finished": nullable(I), "by": s(), "started_by": s(), "stopped_by": s(),
        "items_total": I, "items_done": I, "holes_total": I, "holes_done": I, "line": I, "resumed_from": s(), "message": s(),
        "pause": nullable(ref("Pause")), "estimate_s": N,
    }, ["id", "machine", "state"], extra=True),
    "Job": {"allOf": [ref("JobSummary"), obj({"view": ref("JobView")}, extra=True)]},
    "JobRequestRecord": obj({
        "id": s(), "machine": s(), "kind": s(), "name": s(), "state": s(), "resumed_from": s(), "next_skip": nullable(I), "request": ref("JobRequest"),
    }, ["id", "request"], extra=True),
    "ResumeFrom": obj({"skip": I, "skip_tools": arr(I), "restore_outputs": B, "name": s()}),
    "StopResult": obj({"ok": B, "stopped": B, "job": nullable(S)}, ["ok", "stopped"]),
    "Spindle": obj({
        "spindle": s(), "guard": {"type": "string", "enum": ["reject", "warn"]}, "limits": obj({"original": {"type": "object"}, "vfd": {"type": "object"}}),
        "active": nullable({"type": "object"}), "node_max_rpm": nullable(I),
    }, extra=True),
    "SpindleChange": obj({"spindle": s(), "guard": {"type": "string", "enum": ["reject", "warn"]}, "original": {"type": "object"}, "vfd": {"type": "object"}}),
    "Updates": obj({
        "enabled": B, "pending": {}, "state": {"type": "string", "enum": ["idle", "checking", "updating", "done", "error"]}, "message": s(), "log": {},
        "percent": N, "steps": arr(obj({"id": s(), "state": s(), "percent": N}, extra=True)),
        "history": arr({"type": "object"}), "components": arr(obj({"id": s(), "name": s(), "installed": s(), "can_undo": B, "newer": B}, extra=True)),
    }, ["enabled"], extra=True),
    "UpdateComponents": obj({"components": arr(S)}, ["components"]),
    "HeadStatus": {"type": "object", "additionalProperties": True, "description": "As the node answers: `node`, `camera`, `camera_model`, `laser`, `rpm`, `temp_c`, `heat`, `protect_open`, `uptime_s`."},
    "NodeAnswer": {"type": "object", "additionalProperties": True, "description": "The node's own answer, with its own status code."},
    "Ticket": obj({"ticket": s(), "expires_s": I}, ["ticket", "expires_s"]),
    "InspectRequest": obj({
        "holes": arr(obj({"x_mm": N, "y_mm": N, "dia_mm": N}, ["x_mm", "y_mm", "dia_mm"]), maxItems=500), "light": {"type": "integer", "minimum": 0, "maximum": 100},
        "auto_light": B, "cue": B, "restore_light": B, "profile": {"type": "object"},
    }),
    "InspectReport": obj({
        "verdict": {"type": "string", "enum": ["ok", "not_ok", "unknown"]}, "quality": N, "holes": arr({"type": "object"}), "milling": {"type": "object"},
        "findings": arr(obj({"code": s(), "hole": {}, "fails": B}, extra=True)),
    }, ["verdict"], extra=True),
    "InspectProfile": {"type": "object", "additionalProperties": True, "description": "`px_per_mm`, which checks are on and every tolerance; `preset` (`standard`, `fine_pitch` or `prototype`) starts from a preset."},
    "InspectCalibration": {"type": "object", "additionalProperties": True},
    "TaskKinds": obj({"kinds": arr(obj({"kind": s(), "available": B, "version": s()}, ["kind", "available"]))}, ["kinds"]),
    "NewTask": obj({"kind": s(), "name": s(minLength=1, maxLength=80), "params": {"type": "object"}, "project": s("The id of the artifact the task works on."),
                    "idempotency_key": s(minLength=1, maxLength=128)}, ["kind"]),
    "Task": obj({
        "id": s("t-<uuid v7>"), "kind": s(), "name": s(), "owner": s(), "state": {"type": "string", "enum": ["queued", "running", "done", "failed", "cancelled"]},
        "created": I, "started": nullable(I), "finished": nullable(I), "progress": obj({"done": I, "total": I, "unit": s()}, extra=True),
        "message": s(), "result": nullable({"type": "object"}), "engine": obj({"version": s()}, extra=True),
    }, ["id", "kind", "state"], extra=True),
    "TaskCancel": obj({"accepted": B, "task": ref("Task")}, ["accepted", "task"]),
    "NewArtifact": obj({
        "name": s("1 to 120 characters, no control characters or slashes."), "format": s("A media type in lower case."), "data_base64": s(),
        "schema_version": s(), "origin": {"type": "object"}, "producer": {"type": "object"}, "inputs": arr(obj({"artifact_id": s(), "blob_sha256": s()}, ["artifact_id", "blob_sha256"])),
        "params": {"type": "object"}, "result_type": {"type": "string", "pattern": "^T[1-7]$"},
    }, ["name", "format", "data_base64"]),
    "ArtifactCreated": obj({"artifact_id": s("a-<uuid v7>"), "blob_sha256": s(), "size": I}, ["artifact_id", "blob_sha256", "size"]),
    "ArtifactMeta": obj({
        "artifact_id": s(), "blob_sha256": s(), "size": I, "owner": s(), "name": s(), "format": s(), "schema_version": s(), "origin": {"type": "object"},
        "producer": {"type": "object"}, "inputs": arr({"type": "object"}), "params": {"type": "object"}, "result_type": nullable(S),
        "diagnostics": arr({"type": "object"}), "validation": {"type": "object"}, "audit": {"type": "object"}, "retention": s(), "keep_until": nullable(I),
    }, ["artifact_id", "blob_sha256"], extra=True),
    "RetentionChange": {
        "oneOf": [
            obj({"retention": {"const": "keep_while_referenced"}}, ["retention"]),
            obj({"retention": {"const": "pinned"}}, ["retention"]),
            obj({"retention": {"const": "keep_until"}, "until": {"type": "integer", "description": "Seconds since 1970."}}, ["retention", "until"]),
        ],
    },
    "VerifyResult": obj({"ok": B, "problems": arr(obj({"blob_sha256": s(), "problem": {"type": "string", "enum": ["missing", "damaged"]}}, ["blob_sha256", "problem"]))},
                        ["ok", "problems"]),
    "NewUpload": obj({"size": I, "sha256": s("64 lower-case hexadecimal digits.", pattern="^[0-9a-f]{64}$")}, ["size", "sha256"]),
    "UploadCreated": obj({"id": s("up-<uuid v7>")}, ["id"]),
    "UploadProgress": obj({"id": s(), "size": I, "received": I, "ranges": arr(arr(I))}, ["id", "size", "received"], extra=True),
    "UploadComplete": obj({
        "name": s(), "format": s(), "schema_version": s(), "origin": {"type": "object"}, "producer": {"type": "object"}, "inputs": arr({"type": "object"}),
        "params": {"type": "object"}, "result_type": s(),
    }, ["name", "format"], description="The description of an artifact, as in `POST /artifacts` without the bytes."),
    "Revision": obj({"number": I, "artifact_id": s(), "by": s(), "at": I}, ["number", "artifact_id"], extra=True),
    "Project": obj({
        "id": s(), "name": s(), "owner": s(), "created": I, "head": I, "revisions": arr(ref("Revision")), "deleted": B,
        "shares": arr(ref("Share")),
    }, ["id", "name", "owner"], extra=True, description="`shares` only for the owner and the administrators."),
    "NewProject": obj({"name": s(minLength=1, maxLength=80)}, ["name"]),
    "NewRevision": obj({"artifact_id": s(), "parent": {"type": "integer", "minimum": 0, "description": "The revision this one follows; 0 for the first."}}, ["artifact_id", "parent"]),
    "Share": obj({"who": s("A user name, or `token:<id>`."), "access": {"type": "string", "enum": ["read", "write"]}}, extra=True),
    "NewShare": obj({"user": s(), "token": s("The id of a token."), "access": {"type": "string", "enum": ["read", "write"]}}, description="A user or a token, not both."),
}

# ---------------------------------------------------------------------------------------------------------------------------------------
# The table of operations

PATH_PARAMS = {
    "id": "The id of the item in the path (a machine, job, token, task, artifact, upload or project, as the route says).",
    "name": "The name of the user.",
    "sha": "The SHA-256 of the bytes, 64 lower-case hexadecimal digits.",
    "who": "The name of a user, or `token:<id>`.",
}
TAGS = [
    ("system", "Health, first set-up and the live session."),
    ("auth", "Login, the own account and tickets."),
    ("admin", "Users, tokens of programs and updates of the services."),
    ("machines", "The machines, their spindle and the fleet."),
    ("jobs", "Jobs that move a machine: planned, started and confirmed by a person."),
    ("head", "The node of the drill head: status, camera, light ring and quality check."),
    ("tasks", "Work for the computer that never moves a machine."),
    ("artifacts", "Files kept by identity: artifacts, blobs and uploads."),
    ("projects", "Projects with revisions and explicit sharing."),
]
SECURITY_BEARER = [{"bearerAuth": []}]
SECURITY_OPEN = [{"bearerAuth": []}, {"ticketAuth": []}]

OPERATIONS: list[dict] = []


def op(method: str, path: str, tag: str, role: str, summary: str, *, scope: str | None = None, desc: str = "", body=None, body_media: str = "application/json",
       body_required: bool = True, query=(), ok: tuple = (200, "Ok", "application/json"), ok_desc: str = "Done.", errors=(), example=None,
       responses: dict | None = None, security=None) -> None:
    """One operation. `role` is none (open), any (any login or token), person (a logged-in person), viewer, operator or admin."""
    OPERATIONS.append(dict(method=method, path=path, tag=tag, role=role, summary=summary, scope=scope, desc=desc, body=body, body_media=body_media,
                           body_required=body_required, query=list(query), ok=ok, ok_desc=ok_desc, errors=list(errors), example=example,
                           responses=responses, security=security))


def q(name: str, schema: dict, description: str, required: bool = False) -> dict:
    return {"name": name, "in": "query", "required": required, "description": description, "schema": schema}


MACHINE_ERR = ["unknown_machine"]
JOB_ERR = ["unknown_job"]

# -- system and auth
op("GET", "/health", "system", "none", "Health check", ok=(200, "Health", "application/json"), ok_desc="The name, the version and whether the server is well.",
   desc="Open to anyone, so it says little. With a valid token it also gives the uptime, the state of the storage and the features of this build.",
   example={"name": "eh3d-server", "version": "0.1.1", "api_revision": 1, "status": "ok"}, security=[{}, {"bearerAuth": []}])
op("GET", "/setup", "system", "none", "Ask whether the first administrator is still needed", ok=(200, "SetupState", "application/json"), example={"needed": True})
op("POST", "/setup", "system", "none", "Make the first administrator", body="SetupRequest", errors=["bad_setup", "bad_name", "weak_password", "local_only"],
   desc="Needs the set-up code the server prints on its console. With `local_until_setup` on, only the computer the server runs on may call it while there is no user.",
   example={"code": "123456", "name": "juan", "password": "a long password"})
op("POST", "/login", "auth", "none", "Log in", body="LoginRequest", ok=(200, "LoginResponse", "application/json"), errors=["bad_credentials", "throttled"],
   desc="Five wrong passwords block the name for a while. The login survives a restart only with `persist_sessions`.",
   example={"name": "juan", "password": "a long password"})
op("POST", "/logout", "auth", "any", "End the login")
op("GET", "/session", "auth", "any", "Say who is logged in", ok=(200, "Session", "application/json"), example={"name": "juan", "role": "admin", "is_token": False})
op("PUT", "/account", "auth", "person", "Change the own password", body="PasswordChange", errors=["bad_credentials", "weak_password", "forbidden"],
   desc="A token of a program has no password and is refused with `403 forbidden`.")
op("POST", "/tickets", "auth", "any", "Make a ticket for the camera or the websocket", ok=(200, "Ticket", "application/json"), errors=["throttled"],
   desc="Good for one use within 30 seconds and made for the session that asked, so the address of the camera or of the websocket never carries the token.")
op("GET", "/events", "system", "viewer", "Open the websocket of the events", security=SECURITY_OPEN, ok=(101, None, None), ok_desc="Switching protocols: a websocket.",
   query=[q("ticket", S, "A ticket from `POST /tickets` (a browser cannot set a header on a websocket)."),
          q("token", S, "The token itself; accepted only when `url_token` is on in `server.cfg`.")],
   desc="The first text is `hello` with the summaries of the jobs; then `job`, `job_removed`, `machine`, `task`, `task_removed`, `project`, `revision` and `project_removed`, "
        "the last four only for who may see them; `resync` tells a client that fell behind to ask again. The server pings every `events_ping_s` and closes idle connections.")

# -- admin
op("GET", "/users", "admin", "admin", "List the users", ok=(200, arr(ref("User")), "application/json"))
op("POST", "/users", "admin", "admin", "Create a user", body="NewUser", errors=["bad_name", "weak_password", "user_exists"], example={"name": "ana", "role": "operator", "password": "a long password"})
op("PUT", "/users/{name}", "admin", "admin", "Change a user", body="UserChange", errors=["unknown_user", "weak_password", "last_admin"],
   desc="The last administrator cannot be disabled or demoted.")
op("DELETE", "/users/{name}", "admin", "admin", "Delete a user", errors=["unknown_user", "last_admin"])
op("GET", "/tokens", "admin", "admin", "List the tokens of programs", ok=(200, arr(ref("Token")), "application/json"))
op("POST", "/tokens", "admin", "admin", "Make a token for a program", body="NewToken", ok=(200, "TokenSecret", "application/json"),
   errors=["bad_name", "bad_days", "bad_template", "bad_scope", "forbidden_scope"],
   desc="The token is shown this once. Without `days` the token lasts `token_default_days` of `server.cfg` (0: it does not expire). With scopes its power is the intersection of the role and the scopes.",
   example={"name": "design-program", "role": "operator", "days": 90, "template": "phoenix"})
op("DELETE", "/tokens/{id}", "admin", "admin", "Revoke a token", errors=["unknown_token"])
op("POST", "/tokens/{id}/rotate", "admin", "admin", "Give a token a new secret", ok=(200, "TokenSecret", "application/json"), errors=["unknown_token"],
   desc="The new secret is shown this once and the old one stops working at once; the id, the scopes and the expiry stay.")
op("GET", "/updates", "admin", "admin", "See the updates of the services", ok=(200, "Updates", "application/json"),
   desc="Without `updates_dir` in `server.cfg` the answer is `{\"enabled\": false}`.")
op("POST", "/updates/check", "admin", "admin", "Ask the helper to look for the latest releases", errors=["updates_off", "update_refused"])
op("POST", "/updates/apply", "admin", "admin", "Ask the helper to update components", body="UpdateComponents", errors=["updates_off", "update_refused", "machines_busy"],
   example={"components": ["server", "studio"]})
op("POST", "/updates/undo", "admin", "admin", "Ask the helper to put back the previous version", body="UpdateComponents", errors=["updates_off", "update_refused", "machines_busy"],
   example={"components": ["studio"]})
op("POST", "/updates/clear-history", "admin", "admin", "Ask the helper to empty its history", errors=["updates_off", "update_refused"])

# -- machines
op("GET", "/machines", "machines", "viewer", "List the machines", scope="machines:view", ok=(200, arr(ref("Machine")), "application/json"))
op("POST", "/machines", "machines", "admin", "Create a machine", body="Machine", errors=["bad_id", "bad_station", "bad_address", "bad_name", "bad_dcf", "bad_cfg", "machine_exists"],
   desc="`connection.kind` is `node`, `demo` (the simulator) or `grbl` (a serial port of the computer the server runs on).",
   example={"id": "bungard", "name": "Bungard CCD", "model": "ccd-mtc", "connection": {"kind": "demo"}})
op("GET", "/machines/{id}", "machines", "viewer", "Read a machine", scope="machines:view", ok=(200, "Machine", "application/json"), errors=MACHINE_ERR)
op("PUT", "/machines/{id}", "machines", "admin", "Change a machine", body="Machine", ok=(200, "Machine", "application/json"),
   errors=MACHINE_ERR + ["bad_station", "bad_address", "bad_dcf", "bad_cfg", "bad_spindle", "machine_busy"],
   desc="Not while a job runs on it. Secrets sent empty keep the stored value.")
op("DELETE", "/machines/{id}", "machines", "admin", "Remove a machine", errors=MACHINE_ERR + ["machine_busy"])
op("GET", "/machines/{id}/spindle", "machines", "viewer", "Read the spindle and its limits", ok=(200, "Spindle", "application/json"), errors=MACHINE_ERR)
op("PUT", "/machines/{id}/spindle", "machines", "admin", "Set the spindle, its switch and its limits", body="SpindleChange", ok=(200, "Spindle", "application/json"),
   errors=MACHINE_ERR + ["bad_spindle"], desc="`guard` is `reject` (the default) or `warn`. Nothing changes when a value cannot be used.")
op("POST", "/machines/{id}/stop", "machines", "operator", "Stop the machine now", scope="jobs:cancel", ok=(200, "StopResult", "application/json"), errors=MACHINE_ERR,
   desc="The stop button: ends the job that is active on the machine without naming it. `stopped` is false when this server has no job on the machine.")
op("GET", "/fleet", "machines", "viewer", "Every machine at once, grouped by station", scope="machines:view", ok=(200, "Fleet", "application/json"))

# -- jobs
PLAN_ERR = ["bad_request", "no_work", "both_sources", "spindle_limit", "unknown_tool", "bad_tool", "bad_board", "out_of_table", "outside_course", "bad_ncd", "bad_dcf",
            "bad_gcode", "bad_hole", "bad_path", "bad_sorting", "too_big", "wrong_machine"]
op("POST", "/machines/{id}/preview", "jobs", "operator", "Plan a job and show its view, creating nothing", scope="jobs:prepare", body="JobRequest",
   ok=(200, "JobView", "application/json"), errors=MACHINE_ERR + PLAN_ERR,
   example={"kind": "drill", "name": "board 1", "board_mm": 1.6, "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}],
            "holes": [{"tool": 1, "x_mm": 10.0, "y_mm": 10.0}]})
op("POST", "/machines/{id}/jobs", "jobs", "operator", "Plan a job and keep it ready", scope="jobs:prepare", body="JobRequest", ok=(200, "Job", "application/json"),
   errors=MACHINE_ERR + PLAN_ERR, desc="A job never starts by itself: a person starts it with `POST /jobs/{id}/start`.")
op("GET", "/jobs", "jobs", "viewer", "List the jobs, newest first", scope="machines:view", ok=(200, arr(ref("JobSummary")), "application/json"),
   query=[q("machine", S, "Only the jobs of this machine."), q("state", S, "Only the jobs in this state.")])
op("GET", "/jobs/{id}", "jobs", "viewer", "Read a job with its view", scope="machines:view", ok=(200, "Job", "application/json"), errors=JOB_ERR)
op("DELETE", "/jobs/{id}", "jobs", "operator", "Remove a job that is not running", errors=JOB_ERR + ["job_active"])
op("POST", "/jobs/{id}/start", "jobs", "operator", "Plan again and run the job", scope="jobs:start", ok=(200, "Job", "application/json"),
   errors=JOB_ERR + ["not_ready", "machine_busy"] + PLAN_ERR[1:],
   desc="One job at a time on a machine. A person starts it; the machine's constants of this moment are used.")
op("POST", "/jobs/{id}/confirm", "jobs", "operator", "Answer the dialog the job waits at", scope="jobs:confirm", errors=JOB_ERR + ["not_running"])
op("POST", "/jobs/{id}/cancel", "jobs", "operator", "Stop a running job or cancel a ready one", scope="jobs:cancel", errors=JOB_ERR + ["not_cancellable"])
op("POST", "/jobs/{id}/pause", "jobs", "operator", "Hold a running G-code job", scope="jobs:start", errors=JOB_ERR + ["not_pausable", "not_in_state"])
op("POST", "/jobs/{id}/resume", "jobs", "operator", "Let a held G-code job go on", scope="jobs:start", errors=JOB_ERR + ["not_in_state"])
op("GET", "/jobs/{id}/request", "jobs", "operator", "Read what was asked of the job, as it came", scope="machines:view", ok=(200, "JobRequestRecord", "application/json"), errors=JOB_ERR)
op("POST", "/jobs/{id}/resume-from", "jobs", "operator", "Make a new ready job from a cancelled or failed one", scope="jobs:prepare", body="ResumeFrom", body_required=False,
   ok=(200, "Job", "application/json"), errors=JOB_ERR + ["not_stopped", "no_work"],
   desc="The new job goes on from what the old one had done. It never moves the machine; a person starts it.")

# -- the node of the drill head
HEAD_ERR = MACHINE_ERR + ["no_head", "bad_address", "head_unreachable", "head_refused"]
op("GET", "/machines/{id}/head/status", "head", "viewer", "Status of the node of the drill head", ok=(200, "HeadStatus", "application/json"), errors=HEAD_ERR)
op("POST", "/machines/{id}/head/measure", "head", "operator", "One measure of the table under the head", ok=(200, "NodeAnswer", "application/json"),
   query=[q("samples", {"type": "integer", "minimum": 1, "maximum": 31}, "How many samples (1 to 31).")], errors=HEAD_ERR + ["bad_samples"])
op("GET", "/machines/{id}/head/stream", "head", "viewer", "The camera of the node as MJPEG", security=SECURITY_OPEN,
   ok=(200, {"type": "string", "format": "binary"}, "multipart/x-mixed-replace"), ok_desc="The pictures, handed on while they come.", errors=HEAD_ERR,
   query=[q("ticket", S, "A ticket from `POST /tickets`; a browser cannot set a header on a picture."), q("token", S, "Only with `url_token` on in `server.cfg`.")])
op("GET", "/machines/{id}/head/light", "head", "viewer", "The ring of LEDs as the node says it", ok=(200, "NodeAnswer", "application/json"), errors=HEAD_ERR)
op("POST", "/machines/{id}/head/light", "head", "operator", "Turn the ring on or off and change how it shines", ok=(200, "NodeAnswer", "application/json"),
   errors=HEAD_ERR + ["bad_light"],
   query=[q("on", {"type": "integer", "enum": [0, 1]}, "0 or 1."), q("level", {"type": "integer", "minimum": 0, "maximum": 100}, "Kept under the ceiling of the node."),
          q("tone", {"type": "string", "enum": ["white", "cool", "warm", "red", "green", "blue"]}, "The tone."), q("color", S, "Six hexadecimal digits."),
          q("sectors", S, "`all`, `front`, `right`, `back`, `left`, `front_back`, `left_right` or `0x` and two digits."),
          q("cue", {"type": "string", "enum": ["ok", "nok", "busy", "warn", "alarm", "none"]}, "A cue the ring shows."), q("cue_ms", I, "How long the cue lasts.")])
op("GET", "/machines/{id}/head/snapshot", "head", "viewer", "One picture of the camera", ok=(200, {"type": "string", "format": "binary"}, "image/jpeg"), errors=HEAD_ERR)
op("POST", "/machines/{id}/head/inspect", "head", "operator", "Check the quality of the work with the camera", body="InspectRequest", body_required=False,
   ok=(200, "InspectReport", "application/json"), errors=HEAD_ERR + ["too_many_holes", "bad_profile", "bad_picture"],
   desc="Lights the ring when `light` is given, takes a picture, judges it with the profile of the machine and makes the ring say how it went.")
op("GET", "/machines/{id}/head/inspect", "head", "viewer", "The last check of the machine", ok=(200, "InspectReport", "application/json"), errors=MACHINE_ERR + ["no_inspection"])
op("GET", "/machines/{id}/head/inspect/profile", "head", "viewer", "The profile of the analyser", ok=(200, "InspectProfile", "application/json"), errors=MACHINE_ERR)
op("PUT", "/machines/{id}/head/inspect/profile", "head", "operator", "Change the profile of the analyser", body="InspectProfile", ok=(200, "InspectProfile", "application/json"),
   errors=MACHINE_ERR + ["bad_profile", "storage", "store"], desc="What cannot be used is listed in `rejected` and changes nothing.")
op("POST", "/machines/{id}/head/inspect/calibrate", "head", "operator", "Set the scale of the picture from the first hole found", ok=(200, "InspectCalibration", "application/json"),
   errors=MACHINE_ERR + ["no_inspection", "no_hole", "store"])

# -- tasks
TASK_OFF = ["tasks_off"]
op("GET", "/tasks/kinds", "tasks", "viewer", "The kinds of task and whether this build has an engine for each", scope="tasks:read", ok=(200, "TaskKinds", "application/json"), errors=TASK_OFF)
op("POST", "/tasks", "tasks", "operator", "Make a task", scope="tasks:run", body="NewTask", ok=(201, "Task", "application/json"), ok_desc="The task (also for a repeated idempotency key).",
   errors=TASK_OFF + ["bad_name", "bad_params", "unknown_kind", "engine_missing", "no_project", "too_big", "queue_full", "quota", "idempotency_conflict"],
   desc="A task has no effect on any machine, ever.", example={"kind": "server.selftest", "name": "check", "params": {"steps": 3}})
op("GET", "/tasks", "tasks", "viewer", "List the tasks the caller may see, newest first", scope="tasks:read", ok=(200, arr(ref("Task")), "application/json"), errors=TASK_OFF,
   query=[q("state", S, "Only the tasks in this state."), q("kind", S, "Only the tasks of this kind.")])
op("GET", "/tasks/{id}", "tasks", "viewer", "Read a task with its result", scope="tasks:read", ok=(200, "Task", "application/json"), errors=TASK_OFF + ["unknown_task"])
op("POST", "/tasks/{id}/cancel", "tasks", "operator", "Cancel a task", scope="tasks:run", ok=(200, "TaskCancel", "application/json"), errors=TASK_OFF + ["unknown_task"],
   desc="Allowed to the owner and the administrators. `accepted` is false for a task that had already finished.")
op("DELETE", "/tasks/{id}", "tasks", "operator", "Remove a finished task", scope="tasks:run", errors=TASK_OFF + ["unknown_task", "task_active"], desc="Allowed to the owner and the administrators.")

# -- artifacts, blobs, uploads
ART_ERR = ["unknown_artifact"]
op("GET", "/artifacts", "artifacts", "viewer", "List the artifacts the caller can see, newest first", scope="artifacts:read", ok=(200, arr(ref("ArtifactMeta")), "application/json"),
   query=[q("format", S, "Only this media type."), q("result_type", S, "Only this result type (T1 to T7).")])
op("POST", "/artifacts", "artifacts", "operator", "Keep a file as an artifact", scope="artifacts:write", body="NewArtifact", ok=(201, "ArtifactCreated", "application/json"),
   errors=["bad_artifact", "too_big", "store_full", "blob_damaged"], desc="The bytes are limited by `max_body_mb` (the request) and `max_upload_mb`; larger files go by an upload.",
   example={"name": "board.phoenixproj", "format": "application/x-phoenixproj", "data_base64": "aGVsbG8="})
op("POST", "/artifacts/verify", "artifacts", "admin", "Read every blob again against its hash", scope="artifacts:read", ok=(200, "VerifyResult", "application/json"))
op("GET", "/artifacts/{id}", "artifacts", "viewer", "The bytes of an artifact, streamed", scope="artifacts:read",
   ok=(200, {"type": "string", "format": "binary"}, "application/octet-stream"), ok_desc="The bytes; `x-eh3d-blob-sha256` and `x-eh3d-format` say the hash and the format.",
   errors=ART_ERR + ["blob_damaged"])
op("DELETE", "/artifacts/{id}", "artifacts", "admin", "Remove an artifact", scope="artifacts:write", errors=ART_ERR + ["artifact_in_use"],
   desc="The blob goes too when no record points to it any more.")
op("GET", "/artifacts/{id}/meta", "artifacts", "viewer", "The record of an artifact", scope="artifacts:read", ok=(200, "ArtifactMeta", "application/json"), errors=ART_ERR)
op("PUT", "/artifacts/{id}/retention", "artifacts", "admin", "Set how long an artifact is kept", scope="artifacts:write", body="RetentionChange", errors=ART_ERR + ["bad_request"],
   example={"retention": "pinned"})
op("HEAD", "/blobs/{sha}", "artifacts", "viewer", "Ask whether the caller can see bytes with this hash", scope="artifacts:read",
   ok=(200, None, None), ok_desc="An artifact the caller can see points to bytes with this hash.",
   responses={"404": {"description": "No such bytes for this caller (the same as for a hash nobody holds). No body."}})
op("POST", "/uploads", "artifacts", "operator", "Begin an upload in parts", scope="artifacts:write", body="NewUpload", ok=(201, "UploadCreated", "application/json"),
   errors=["too_big", "bad_upload", "quota", "store_full"])
op("PUT", "/uploads/{id}", "artifacts", "operator", "Send a part of an upload", scope="artifacts:write", body={"type": "string", "format": "binary"},
   body_media="application/octet-stream", ok=(200, "UploadProgress", "application/json"), errors=["unknown_upload", "upload_conflict", "bad_upload"],
   query=[q("offset", {"type": "integer", "minimum": 0}, "Where the part starts, in bytes.", True)],
   desc="Parts may come at any place and in any order. Allowed to the owner of the upload.")
op("DELETE", "/uploads/{id}", "artifacts", "operator", "Throw an upload away", scope="artifacts:write", errors=["unknown_upload"], desc="Allowed to the owner of the upload.")
op("POST", "/uploads/{id}/complete", "artifacts", "operator", "Finish an upload and make its artifact", scope="artifacts:write", body="UploadComplete",
   ok=(201, "ArtifactCreated", "application/json"), errors=["unknown_upload", "upload_incomplete", "hash_mismatch", "bad_artifact", "store_full"],
   desc="Allowed to the owner of the upload. A hash that is not the declared one discards the upload.")

# -- projects and sharing
PRJ_ERR = ["unknown_project"]
op("GET", "/projects", "projects", "viewer", "List the projects the caller sees", scope="projects:read", ok=(200, arr(ref("Project")), "application/json"))
op("POST", "/projects", "projects", "operator", "Make a project", scope="projects:write", body="NewProject", ok=(201, "Project", "application/json"), errors=["bad_name"],
   example={"name": "front panel"})
op("GET", "/projects/{id}", "projects", "viewer", "Read a project with its revisions", scope="projects:read", ok=(200, "Project", "application/json"), errors=PRJ_ERR)
op("DELETE", "/projects/{id}", "projects", "operator", "Hide a project", scope="projects:write", errors=PRJ_ERR, desc="Allowed to the owner and the administrators; the revisions stay until a purge.")
op("POST", "/projects/{id}/purge", "projects", "admin", "Remove a hidden project for good", scope="projects:write", errors=PRJ_ERR)
op("POST", "/projects/{id}/revisions", "projects", "operator", "Add a revision", scope="projects:write", body="NewRevision", ok=(201, "Revision", "application/json"),
   errors=PRJ_ERR + ["stale_parent", "unknown_artifact"], desc="Needs write access to the project. `parent` must still be the last revision.",
   example={"artifact_id": "a-0190", "parent": 0})
op("POST", "/projects/{id}/shares", "projects", "operator", "Share a project with one person or token", scope="projects:write", body="NewShare",
   errors=PRJ_ERR + ["unknown_user", "unknown_token", "bad_share", "shares_off"], desc="Allowed to the owner and the administrators.", example={"user": "ana", "access": "read"})
op("DELETE", "/projects/{id}/shares/{who}", "projects", "operator", "Take a sharing back", scope="projects:write", errors=PRJ_ERR + ["shares_off"],
   desc="Allowed to the owner and the administrators.")
op("GET", "/artifacts/{id}/shares", "projects", "viewer", "Who an artifact is shared with", scope="artifacts:read", ok=(200, arr(ref("Share")), "application/json"), errors=ART_ERR,
   desc="Only for the owner and the administrators.")
op("POST", "/artifacts/{id}/shares", "projects", "operator", "Share an artifact for reading", scope="artifacts:write", body="NewShare",
   errors=ART_ERR + ["unknown_user", "unknown_token", "bad_share", "shares_off"], desc="Allowed to the owner and the administrators; the access is `read`.")
op("DELETE", "/artifacts/{id}/shares/{who}", "projects", "operator", "Take the sharing of an artifact back", scope="artifacts:write", errors=ART_ERR + ["shares_off"],
   desc="Allowed to the owner and the administrators.")

# ---------------------------------------------------------------------------------------------------------------------------------------
# Building the document


def catalog() -> dict[str, dict]:
    items = json.loads((ROOT / "docs" / "error-codes.json").read_text(encoding="utf-8"))
    return {item["code"]: item for item in items}


def cargo_version() -> str:
    found = re.search(r'(?m)^version\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    if not found:
        raise SystemExit("Cargo.toml has no version")
    return found.group(1)


def operation_id(method: str, path: str) -> str:
    words = re.sub(r"[{}]", "", path).strip("/").replace("-", "_").split("/")
    return method.lower() + "_" + "_".join(words)


def schema_of(value) -> dict:
    return ref(value) if isinstance(value, str) else value


def build_operation(spec: dict, codes: dict[str, dict]) -> dict:
    role = spec["role"]
    errors = list(spec["errors"])
    if role != "none":
        errors.append("unauthorized")
    if role in ("person", "operator", "admin"):
        errors.append("forbidden")
    if spec["scope"]:
        errors.append("forbidden_scope")
    if spec["body"] is not None and spec["body_media"] == "application/json":
        errors.append("bad_request")
    for code in errors:
        if code not in codes:
            raise SystemExit(f"{spec['method']} {spec['path']}: the code {code} is not in docs/error-codes.json")
    responses: dict[str, dict] = {}
    status, schema, media = spec["ok"]
    ok_response: dict = {"description": spec["ok_desc"]}
    if schema is not None:
        content: dict = {"schema": schema_of(schema)}
        if spec["example"] is not None and spec["method"] != "POST":
            content["example"] = spec["example"]
        ok_response["content"] = {media: content}
    if schema == "Ok":
        ok_response["content"][media]["example"] = {"ok": True}
    responses[str(status)] = ok_response
    by_status: dict[str, list[str]] = {}
    for code in dict.fromkeys(errors):
        by_status.setdefault(str(codes[code]["status"]), []).append(code)
    for status_text, group in sorted(by_status.items()):
        responses[status_text] = {
            "description": "Refused with " + ", ".join(f"`{code}`" for code in sorted(group)) + ".",
            "content": {"application/json": {"schema": ref("ErrorBody"), "example": {"error": sorted(group)[0], "detail": ""}}},
        }
    for status_text, response in (spec["responses"] or {}).items():
        responses[status_text] = response
    description = spec["desc"]
    role_text = {"none": "No login.", "any": "Any login or token.", "person": "A person logged in (not a token).", "viewer": "Role viewer or higher.",
                 "operator": "Role operator or higher.", "admin": "Role admin."}[role]
    description = (description + " " if description else "") + role_text + (f" Scope `{spec['scope']}` if the token is limited by scopes." if spec["scope"] else "")
    operation: dict = {
        "operationId": operation_id(spec["method"], spec["path"]),
        "tags": [spec["tag"]],
        "summary": spec["summary"],
        "description": description,
        "x-role": role,
        "x-scope": spec["scope"],
        "x-error-codes": sorted(dict.fromkeys(errors)),
    }
    if spec["scope"] is None:
        del operation["x-scope"]
    names = re.findall(r"\{(\w+)\}", spec["path"])
    parameters = [{"name": n, "in": "path", "required": True, "description": PATH_PARAMS[n], "schema": S} for n in names] + spec["query"]
    if parameters:
        operation["parameters"] = parameters
    if spec["body"] is not None:
        content = {"schema": schema_of(spec["body"])}
        if spec["example"] is not None and spec["method"] in ("POST", "PUT"):
            content["example"] = spec["example"]
        operation["requestBody"] = {"required": spec["body_required"], "content": {spec["body_media"]: content}}
    if spec["security"] is not None:
        operation["security"] = spec["security"]
    elif role == "none":
        operation["security"] = []
    operation["responses"] = responses
    return operation


def build() -> dict:
    codes = catalog()
    paths: dict[str, dict] = {}
    seen: set[tuple[str, str]] = set()
    for spec in OPERATIONS:
        key = (spec["method"], spec["path"])
        if key in seen:
            raise SystemExit(f"{key} is described twice")
        seen.add(key)
        paths.setdefault(spec["path"], {})[spec["method"].lower()] = build_operation(spec, codes)
    ids = [operation["operationId"] for item in paths.values() for operation in item.values()]
    if len(ids) != len(set(ids)):
        raise SystemExit("two operations have the same operationId")
    return {
        "openapi": "3.1.0",
        "info": {
            "title": "EH3D Server API",
            "version": cargo_version(),
            "summary": "The REST and websocket interface of the EH3D server.",
            "description": (
                "Everything is under `/api/v1/` and answers JSON. A mistake is `{\"error\": \"<code>\", \"detail\": \"...\"}` with the status that fits; the codes are the "
                "catalog in `docs/error-codes.json`. Except the health check, the first set-up and the login, every request carries `Authorization: Bearer <token>`: the token "
                "of a login or the token of a program. A viewer reads, an operator also prepares, starts, stops and confirms jobs, an administrator also manages users, tokens "
                "and machines; a token may be limited further by scopes. Limits: passwords are 8 to 128 characters, a request body is at most `max_body_mb` of `server.cfg` "
                "(else `too_big`), a ticket is good for one use within 30 seconds, and five wrong passwords block a name for a while (`throttled`). "
                "`docs/API.md` explains the behaviour; this document is the contract of the routes."
            ),
            "license": {"name": "GPL-3.0-or-later", "identifier": "GPL-3.0-or-later"},
        },
        "servers": [{"url": PREFIX, "description": "The default port of the server is 18200."}],
        "tags": [{"name": name, "description": text} for name, text in TAGS],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearerAuth": {"type": "http", "scheme": "bearer", "description": "The token of a login (`POST /login`) or of a program (`POST /tokens`)."},
                "ticketAuth": {"type": "apiKey", "in": "query", "name": "ticket", "description": "A ticket from `POST /tickets`, for the camera and the websocket."},
            },
            "schemas": SCHEMAS,
        },
        "security": SECURITY_BEARER,
    }


# ---------------------------------------------------------------------------------------------------------------------------------------
# YAML (block style; strings that are not plain words are written as JSON strings, which YAML reads the same)

PLAIN = re.compile(r"^[A-Za-z_][A-Za-z0-9_./-]*( [A-Za-z0-9_./-]+)*$")
RESERVED = {"true", "false", "null", "yes", "no", "on", "off", "y", "n", "~"}


def scalar(value) -> str:
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return json.dumps(value)
    text = str(value)
    if PLAIN.match(text) and text.lower() not in RESERVED:
        return text
    return json.dumps(text, ensure_ascii=False)


def key_text(key: str) -> str:
    return scalar(key)


def to_yaml(value, indent: int = 0) -> list[str]:
    pad = "  " * indent
    lines: list[str] = []
    if isinstance(value, dict):
        if not value:
            return [pad + "{}"]
        for key, item in value.items():
            if isinstance(item, (dict, list)) and item:
                lines.append(f"{pad}{key_text(key)}:")
                lines.extend(to_yaml(item, indent + 1))
            else:
                text = "{}" if item == {} and isinstance(item, dict) else "[]" if item == [] and isinstance(item, list) else scalar(item)
                lines.append(f"{pad}{key_text(key)}: {text}")
    elif isinstance(value, list):
        for item in value:
            if isinstance(item, dict) and item:
                inner = to_yaml(item, indent + 1)
                lines.append(f"{pad}- {inner[0].lstrip()}")
                lines.extend(inner[1:])
            elif isinstance(item, list) and item:
                inner = to_yaml(item, indent + 1)
                lines.append(f"{pad}- {inner[0].lstrip()}")
                lines.extend(inner[1:])
            else:
                text = "{}" if item == {} and isinstance(item, dict) else "[]" if item == [] and isinstance(item, list) else scalar(item)
                lines.append(f"{pad}- {text}")
    else:
        lines.append(pad + scalar(value))
    return lines


def yaml_text(doc: dict) -> str:
    header = "# The contract of the REST interface of the EH3D server (OpenAPI 3.1).\n# Written by tools/make_openapi.py from the same table as openapi.json: do not edit by hand.\n"
    return header + "\n".join(to_yaml(doc)) + "\n"


def json_text(doc: dict) -> str:
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"


# ---------------------------------------------------------------------------------------------------------------------------------------
# ROUTES.md

AUTH_TEXT = {"none": "none", "any": "any login or token", "person": "a person", "viewer": "viewer", "operator": "operator", "admin": "admin"}


def router_path(path: str) -> str:
    return PREFIX + re.sub(r"\{(\w+)\}", r":\1", path)


def routes_rows(doc: dict) -> list[tuple[str, str, str, str]]:
    rows = []
    for path, item in doc["paths"].items():
        for method, operation in item.items():
            auth = AUTH_TEXT[operation["x-role"]]
            if operation.get("x-scope"):
                auth += f" + `{operation['x-scope']}`"
            rows.append((router_path(path), method.upper(), auth, operation["summary"]))
    order = {"GET": 0, "POST": 1, "PUT": 2, "DELETE": 3, "HEAD": 4}
    rows.sort(key=lambda r: (r[0], order[r[1]]))
    return rows


def routes_text(doc: dict) -> str:
    out = [
        "# Routes of the server",
        "",
        "Every route of the router, with the method, who may call it and what it does. This table is written by `tools/make_openapi.py` from the same table as "
        "`docs/openapi.yaml`, the contract of the interface (with the schemas, the examples and the error codes of each route); do not edit it by hand.",
        "",
        "| Route | Method | Auth | Description |",
        "|---|---|---|---|",
    ]
    for route, method, auth, summary in routes_rows(doc):
        out.append(f"| `{route}` | {method} | {auth} | {summary} |")
    out.append("")
    return "\n".join(out)


def outputs() -> dict[str, str]:
    doc = build()
    return {"docs/openapi.json": json_text(doc), "docs/openapi.yaml": yaml_text(doc), "docs/ROUTES.md": routes_text(doc)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--check", action="store_true", help="exit 1 when a written file is not what the table gives")
    args = parser.parse_args()
    files = outputs()
    problems = []
    for name, text in files.items():
        path = ROOT / name
        if args.check:
            current = path.read_bytes().decode("utf-8").replace("\r\n", "\n") if path.is_file() else None
            if current != text:
                problems.append(f"{name} is not what tools/make_openapi.py writes: run it")
        else:
            path.write_bytes(text.encode("utf-8"))
            print(f"wrote {name}")
    for problem in problems:
        print("  FAIL", problem)
    if args.check:
        print("MAKE_OPENAPI=" + ("FAIL" if problems else "PASS"))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
