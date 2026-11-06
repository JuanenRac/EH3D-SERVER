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
    "RackSlot": obj({"tool": {"type": "integer", "minimum": 0, "maximum": 255}, "place": {"type": "integer", "minimum": 1}}, ["tool", "place"],
                    description="The place of the rack one tool of the job is in."),
    "JobRackV2": obj({
        "first": {"type": "integer", "minimum": 0, "default": 0, "description": "How many places of the rack the tools of the jobs before this one take (0 for the first job of the load)."},
        "carry_in": {"type": "boolean", "default": False, "description": "The chuck comes holding the last tool of the job before: the first change takes it to its place and picks the first tool of this job."},
        "carry_out": {"type": "boolean", "default": False, "description": "This job leaves its last tool in the chuck for the next one."},
        "slots": arr(ref("RackSlot"), description="The place of each tool of this job; a tool that is not listed is taken from the place the order of the job gives it."),
        "carry_slot": {"type": "integer", "minimum": 1, "description": "With `carry_in`, the place of the tool the chuck comes holding."},
    }, extra=True, description="The chaining of the tool changer of a request of the second version. Every field is optional; a field this server does not know is left out and named in a warning. See docs/ATC_CHAIN.md."),
    "DrillRequest": obj({
        "kind": {"type": "string", "const": "drill"}, "name": s(), "board_mm": {"type": "number", "minimum": 0, "maximum": 50},
        "tools": arr(ref("DrillTool")), "holes": arr(obj({"tool": I, "x_mm": N, "y_mm": N}, ["tool", "x_mm", "y_mm"])),
        "files": obj({"ncd": s(), "dtd": s()}), "sorting": {"type": "string", "enum": ["none", "x", "y", "special"]},
        "skip": I, "skip_tools": arr(I),
        "request_version": {"type": "integer", "minimum": 1, "description": "`2` when `rack` is present; absent in the requests of the first version."},
        "rack": ref("JobRackV2"),
    }, ["kind"], description="The holes or the files, not both (`both_sources`)."),
    "RouteRequest": obj({
        "kind": {"type": "string", "const": "route"}, "name": s(), "tools": arr(ref("RouteTool")),
        "paths": arr(obj({"pen": I, "points_mm": arr(NUMBER_PAIR)}, ["pen", "points_mm"])),
        "files": obj({"plt": s(), "rtd": s()}), "skip": I, "skip_tools": arr(I),
        "request_version": {"type": "integer", "minimum": 1, "description": "`2` when `rack` is present; absent in the requests of the first version."},
        "rack": ref("JobRackV2"),
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
        "rack_applied": {"type": "boolean", "description": "Only when the request had a `rack`: `true` when the plan used it, `false` when it was left out (a `rack_*` warning says why)."},
    }, extra=True, description="What a client draws and follows; see `The view` in docs/API.md."),
    "JobSummary": obj({
        "id": s(), "machine": s(), "name": s(), "kind": s(),
        "state": {"type": "string", "enum": ["ready", "connecting", "running", "waiting_operator", "done", "cancelled", "failed"]},
        "created": I, "started": nullable(I), "finished": nullable(I), "by": s(), "started_by": s(), "stopped_by": s(),
        "items_total": I, "items_done": I, "holes_total": I, "holes_done": I, "line": I, "resumed_from": s(), "message": s(),
        "pause": nullable(ref("Pause")), "estimate_s": N,
    }, ["id", "machine", "state"], extra=True),
    "Job": {"allOf": [ref("JobSummary"), obj({"view": ref("JobView"), "rack_applied": {"type": "boolean", "description": "As in the view: present only when the request had a `rack`."}}, extra=True)]},
    "JobResponse": {"allOf": [ref("Job")], "description": "The answer to a request for a job. With a `rack` in the request, `rack_applied` is `true` when the server planned with it, and `view.warnings` lists what was left out (`rack_needs_v2`, `rack_no_changer`, `rack_unknown_field`, `request_version_newer`)."},
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
    "catalog": "The name of a catalog (`materials`, `tools`, `machine-profiles`, `footprints`, `symbols` or `components`).",
    "item": "The name of an item of the catalog (the value of its `id`, or of its `name` for footprints and symbols).",
    "app": "The application that owns the projects (`studio`, `android`, `pcb`...; see docs/AUTH.md).",
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
    ("store", "The project store of every application: projects, files with versions, editing locks and the .eh3dproj container."),
    ("catalogs", "Shared catalogs the applications read: materials, tools, machine profiles, footprints, symbols and components."),    ("work", "Plugins and their machines, the generic queue of jobs with its history, the stream of events and the notifications."),
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
            "bad_gcode", "bad_hole", "bad_path", "bad_rack", "bad_sorting", "too_big", "wrong_machine"]
op("POST", "/machines/{id}/preview", "jobs", "operator", "Plan a job and show its view, creating nothing", scope="jobs:prepare", body="JobRequest",
   ok=(200, "JobView", "application/json"), errors=MACHINE_ERR + PLAN_ERR,
   example={"kind": "drill", "name": "board 1", "board_mm": 1.6, "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}],
            "holes": [{"tool": 1, "x_mm": 10.0, "y_mm": 10.0}]})
op("POST", "/machines/{id}/jobs", "jobs", "operator", "Plan a job and keep it ready", scope="jobs:prepare", body="JobRequest", ok=(200, "JobResponse", "application/json"),
   errors=MACHINE_ERR + PLAN_ERR, desc="A job never starts by itself: a person starts it with `POST /jobs/{id}/start`. A request of the second version (`request_version: 2`) may carry a `rack` that chains the changes of the tools (docs/ATC_CHAIN.md).")
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

# -- the project store of every application (docs/PROJECT_STORE.md)
SCHEMAS.update({
    "StoreFile": obj({"name": s(), "version": I, "sha256": s(), "size": I, "at": I, "by": s()}, ["name", "version", "sha256", "size"],
                     description="The current version of a file of a project."),
    "StoreLock": obj({"holder": s(), "since": I, "expires": I}, ["holder", "expires"], description="Who is editing the project and until when (seconds since 1970)."),
    "StoreProject": obj({
        "id": s(), "app": s(), "name": s(), "owner": s(), "created": I, "updated": I, "etag": s("Also the `ETag` header."), "deleted": B, "bytes": I,
        "lock": nullable(ref("StoreLock")), "files": arr(ref("StoreFile")),
    }, ["id", "app", "name", "owner", "etag", "files"]),
    "StoreProjectSummary": obj({
        "id": s(), "app": s(), "name": s(), "owner": s(), "created": I, "updated": I, "etag": s(), "deleted": B, "files": I, "locked": B,
    }, ["id", "app", "name", "owner", "etag"]),
    "StoreProjectPage": obj({"total": I, "offset": I, "limit": I, "items": arr(ref("StoreProjectSummary"))}, ["total", "offset", "limit", "items"]),
    "StoreFiles": obj({"files": arr(ref("StoreFile"))}, ["files"]),
    "StoreVersion": obj({"version": I, "sha256": s(), "size": I, "at": I, "by": s(), "comment": s()}, ["version", "sha256", "size"],
                        description="One version of a file: its number, the SHA-256 of its bytes (also its tag) and who wrote it."),
    "StoreVersionList": obj({"name": s(), "versions": arr(ref("StoreVersion"))}, ["name", "versions"], description="The newest version first."),
    "StoreWriteResult": {"allOf": [ref("StoreVersion"), obj({"name": s(), "unchanged": B}, ["name", "unchanged"], extra=True)],
                         "description": "The version written; `unchanged` is true when the bytes were those of the current version and no version was made."},
    "StoreLockState": obj({"lock": nullable(ref("StoreLock"))}, ["lock"]),
    "StoreImportResult": obj({
        "imported": I, "unchanged": I,
        "files": arr(obj({"name": s(), "version": I, "unchanged": B}, ["name", "version", "unchanged"])),
    }, ["imported", "unchanged", "files"]),
})

BINARY = {"type": "string", "format": "binary"}
STORE_ERR = ["bad_app", "unknown_project"]
FILE_Q = q("name", S, "The name of the file in the project, with its folders (`layers/top.gbr`).", True)
STORE = "/apps/{app}/projects"
op("GET", STORE, "store", "viewer", "List the projects of an application", scope="projects:read", ok=(200, "StoreProjectPage", "application/json"), errors=["bad_app"],
   query=[q("q", S, "Keep the projects whose name has this text, in any letter case."), q("limit", {"type": "integer", "minimum": 1, "maximum": 500}, "Projects per page (50)."),
          q("offset", {"type": "integer", "minimum": 0}, "Projects to skip.")],
   desc="The projects the caller sees (its own and, for an administrator, all; with `project_shared` every project of the application), the last changed first.")
op("POST", STORE, "store", "operator", "Make a project of an application", scope="projects:write", body="NewProject", ok=(201, "StoreProject", "application/json"),
   errors=["bad_app", "bad_name", "project_quota"], desc="The caller is the owner. The answer carries the `ETag` of the project.", example={"name": "front panel"})
op("GET", STORE + "/{id}", "store", "viewer", "Read a project with its files", scope="projects:read", ok=(200, "StoreProject", "application/json"), errors=STORE_ERR,
   desc="With the `ETag` of the project in the header.")
op("PUT", STORE + "/{id}", "store", "operator", "Rename a project", scope="projects:write", body="NewProject", ok=(200, "StoreProject", "application/json"),
   errors=STORE_ERR + ["bad_name", "locked", "precondition_required", "precondition_failed"],
   desc="Needs `If-Match` with the `ETag` of the project (unless `project_require_if_match` is off). Refused while somebody else holds the lock.", example={"name": "front panel v2"})
op("DELETE", STORE + "/{id}", "store", "operator", "Hide a project", scope="projects:write", errors=STORE_ERR + ["forbidden", "locked"],
   desc="Allowed to the owner and the administrators; the files stay until an administrator purges it.")
op("POST", STORE + "/{id}/purge", "store", "admin", "Remove a hidden project for good", scope="projects:write", errors=STORE_ERR,
   desc="Only a project that was hidden; its files go with it. Audited.")
op("GET", STORE + "/{id}/files", "store", "viewer", "List the files of a project", scope="projects:read", ok=(200, "StoreFiles", "application/json"), errors=STORE_ERR,
   desc="The current version of each file, by name. With the `ETag` of the project in the header.")
op("GET", STORE + "/{id}/files/content", "store", "viewer", "Read a file", scope="projects:read", ok=(200, BINARY, "application/octet-stream"),
   errors=STORE_ERR + ["unknown_file", "unknown_version", "blob_damaged"],
   query=[FILE_Q, q("version", {"type": "integer", "minimum": 1}, "An older version (the current one without it).")],
   desc="The bytes are read and checked against their SHA-256 every time (`500 blob_damaged` if they changed on the disk). The header `ETag` is the SHA-256 of the version and `X-Eh3d-Version` its number.")
op("PUT", STORE + "/{id}/files/content", "store", "operator", "Write a file as a new version", scope="projects:write", body=BINARY, body_media="application/octet-stream",
   ok=(200, "StoreWriteResult", "application/json"), ok_desc="The bytes were those of the current version: nothing was written. A new version answers 201.",
   errors=STORE_ERR + ["bad_filename", "file_too_big", "project_quota", "locked", "precondition_required", "precondition_failed", "too_big", "storage"],
   query=[FILE_Q, q("comment", S, "A note kept with the version (up to 200 characters).")],
   desc="Never overwrites: every change of the bytes is a new version. A file that exists needs `If-Match` with the SHA-256 of the version the new bytes were made from "
        "(`428 precondition_required` without it, `412 precondition_failed` if it is not the current one); a new file takes none. Answers `201` for a new version and `200` when nothing changed.")
op("DELETE", STORE + "/{id}/files/content", "store", "operator", "Remove a file from the list", scope="projects:write", errors=STORE_ERR + ["unknown_file", "locked", "precondition_required", "precondition_failed"],
   query=[FILE_Q], desc="Needs `If-Match` like a write. The versions stay and a later write brings the file back.")
op("GET", STORE + "/{id}/files/versions", "store", "viewer", "List the versions of a file", scope="projects:read", ok=(200, "StoreVersionList", "application/json"),
   errors=STORE_ERR + ["unknown_file"], query=[FILE_Q])
op("GET", STORE + "/{id}/lock", "store", "viewer", "See who is editing a project", scope="projects:read", ok=(200, "StoreLockState", "application/json"), errors=STORE_ERR)
op("PUT", STORE + "/{id}/lock", "store", "operator", "Take or renew the editing lock of a project", scope="projects:write", ok=(200, "StoreLockState", "application/json"),
   errors=STORE_ERR + ["locked"], query=[q("ttl_s", {"type": "integer", "minimum": 1}, "How long, in seconds; at most `project_lock_ttl_s`.")],
   desc="Refused with `423 locked` while somebody else holds a lock that has not expired. The holder renews it by calling again. While it lasts the others cannot write.")
op("DELETE", STORE + "/{id}/lock", "store", "operator", "Let the editing lock go", scope="projects:write", errors=STORE_ERR + ["locked", "not_locked"],
   query=[q("force", B, "An administrator takes the lock away from somebody else.")])
op("POST", STORE + "/{id}/import", "store", "operator", "Import an .eh3dproj container into a project", scope="projects:write", body=BINARY, body_media="application/octet-stream",
   ok=(200, "StoreImportResult", "application/json"),
   errors=STORE_ERR + ["bad_container", "bad_filename", "file_too_big", "project_quota", "locked", "too_big"],
   desc="The container is opened with the bounds of the container library of EH3D-COMMON (names, sizes, integrity record) as 1.0 or 1.1, and every entry becomes a new version of the file of that name; "
        "the entries that did not change make none. Everything or nothing.")
op("GET", STORE + "/{id}/export", "store", "viewer", "Export a project as an .eh3dproj container", scope="projects:read", ok=(200, BINARY, "application/zip"),
   errors=STORE_ERR + ["bad_container", "blob_damaged"], query=[q("spec", {"type": "string", "enum": ["1.0", "1.1"]}, "The version of the container (1.0 first, then 1.1, without it).")],
   desc="The current files, each checked against its hash, in a container with its integrity record. The project has to hold the `project.json` of the format.")

# -- the shared catalogs (docs/CATALOGS.md)
SCHEMAS.update({
    "CatalogSummary": obj({
        "name": s(), "title": s(), "builtin": B, "items": I, "version": I, "sha256": s(), "updated": I, "by": s(), "source": s(), "etag": s(),
    }, ["name", "builtin", "items", "version", "etag"], description="`version` is 0 for a catalog nobody uploaded to; `source` is `embedded`, `json` or `csv`."),
    "CatalogList": obj({"catalogs": arr(ref("CatalogSummary"))}, ["catalogs"]),
    "CatalogPage": obj({
        "catalog": s(), "version": I, "etag": s("Also the `ETag` header."), "total": I, "offset": I, "limit": I,
        "items": arr({"type": "object", "additionalProperties": True}),
    }, ["catalog", "version", "etag", "total", "offset", "limit", "items"]),
    "CatalogItem": {"type": "object", "additionalProperties": True, "description": "One item, as the dataset has it; the field that names it is `id` (`name` for footprints and symbols)."},
    "CatalogDataset": {
        "oneOf": [arr({"type": "object", "additionalProperties": True}), {"type": "object", "additionalProperties": True}],
        "description": "A list of items, or an object with the list under `tools`, `profiles`, `footprints`, `symbols` or `parts` (`items` also works); the components base declares "
                       "`\"format\": \"phoenix-parts/1\"`. As CSV (`Content-Type: text/csv`): a header line and a row per item, the values as text.",
    },
})

CAT_ERR = ["unknown_catalog"]
op("GET", "/catalogs", "catalogs", "viewer", "List the catalogs", scope="catalog:read", ok=(200, "CatalogList", "application/json"),
   desc="Every catalog with its version, its number of items and its tag, also the ones nobody uploaded to.")
op("GET", "/catalogs/{catalog}", "catalogs", "viewer", "Search a catalog", scope="catalog:read", ok=(200, "CatalogPage", "application/json"),
   errors=CAT_ERR + ["bad_request"],
   query=[q("q", S, "Keep the items that have this text in any value, in any letter case."),
          q("filter", S, "Conditions `field:value` separated by commas: the field (`a.b` for a nested one) equals the value in any letter case; a value that starts with `~` only has to contain the rest; "
                         "on a list one of the values has to match."),
          q("sort", S, "The field to sort by (numbers as numbers, the rest as text)."), q("desc", B, "Descending."),
          q("limit", {"type": "integer", "minimum": 1}, "Items per page (50, at most `catalog_page_max`)."), q("offset", {"type": "integer", "minimum": 0}, "Items to skip.")],
   responses={"304": {"description": "The `If-None-Match` tag is the one of the catalog: nothing changed. No body."}},
   desc="With the `ETag` of the dataset in the header (its content, so it changes only when the dataset does). A client that keeps a copy sends it back in `If-None-Match`.")
op("GET", "/catalogs/{catalog}/items/{item}", "catalogs", "viewer", "Read one item of a catalog", scope="catalog:read", ok=(200, "CatalogItem", "application/json"),
   errors=CAT_ERR + ["unknown_item"])
op("PUT", "/catalogs/{catalog}", "catalogs", "admin", "Replace the dataset of a catalog", scope="catalog:write", body="CatalogDataset", ok=(200, "CatalogSummary", "application/json"),
   errors=CAT_ERR + ["bad_catalog", "catalog_readonly", "too_big"],
   desc="The whole dataset, as a new version, or nothing: one item that does not fit refuses the upload. The built-in `materials` is not written. Audited and told on the stream as `catalog.changed`.",
   example=[{"id": "d08", "kind": "drill", "dia_mm": 0.8}])
op("DELETE", "/catalogs/{catalog}", "catalogs", "admin", "Empty a catalog", scope="catalog:write", ok=(200, "CatalogSummary", "application/json"), errors=CAT_ERR + ["catalog_readonly"],
   desc="A new version with no items; the file of the previous one stays beside it.")

# -- the plugins, the generic queue, the stream of events and the notifications (docs/MACHINE_PLUGINS.md)
WORK_STATE = {"type": "string", "enum": ["queued", "running", "paused", "done", "failed", "cancelled"]}
SCHEMAS.update({
    "PluginInfo": obj({"kind": s(), "title": s(), "capabilities": {"type": "object", "additionalProperties": True}, "manages_machines": B},
                      ["kind", "title", "capabilities", "manages_machines"], description="`manages_machines` is true when the machines of the plugin are made with `POST /work/machines`."),
    "PluginList": obj({"plugins": arr(ref("PluginInfo"))}, ["plugins"]),
    "WorkMachineInfo": obj({
        "id": s(), "name": s(), "plugin": s(), "state": {"type": "string", "enum": ["idle", "busy"]}, "job": nullable(S),
        "capabilities": {"type": "object", "additionalProperties": True},
    }, ["id", "name", "plugin", "state"], description="`job` is the job of the generic queue that has the machine, when there is one."),
    "WorkMachineList": obj({"machines": arr(ref("WorkMachineInfo"))}, ["machines"]),
    "NewWorkMachine": obj({"id": s("Lowercase letters, digits and hyphens, up to 32."), "name": s(minLength=1, maxLength=80), "plugin": s(),
                           "settings": {"type": "object", "additionalProperties": True}}, ["id", "name", "plugin"]),
    "WorkMachineRecord": obj({"id": s(), "name": s(), "plugin": s(), "settings": {"type": "object", "additionalProperties": True}, "created": I},
                             ["id", "name", "plugin", "settings"]),
    "WorkMachineChange": obj({"name": s(minLength=1, maxLength=80), "settings": {"type": "object", "additionalProperties": True}}),
    "NewWorkJob": obj({
        "machine": s("The id of a machine of any plugin."), "request": {"type": "object", "additionalProperties": True, "description": "What the plugin of the machine takes: for `bungard` a drill, route or gcode request (`JobRequest`), for `demo` `{\"kind\": \"demo\", \"steps\": 10}`."},
        "name": s(maxLength=80), "app": s("The application that asks; the default is the one of the login."),
    }, ["machine", "request"]),
    "WorkState": WORK_STATE,
    "WorkHistoryEntry": obj({"at": I, "state": ref("WorkState"), "by": s("A person, or `machine`/`server` when the change came from them."), "note": s()}, ["at", "state", "by"]),
    "WorkJobSummary": obj({
        "id": s("w-<uuid v7>"), "machine": s(), "plugin": s(), "app": s(), "name": s(), "owner": s(), "by": s(), "state": ref("WorkState"),
        "created": I, "started": nullable(I), "finished": nullable(I), "done": I, "total": I, "percent": {"type": "integer", "minimum": 0, "maximum": 100},
        "message": s(), "position": {"type": "integer", "minimum": 0, "description": "The place among the queued jobs of its machine (1 is the next); 0 when it is not queued."},
    }, ["id", "machine", "plugin", "state"], extra=True),
    "WorkJob": {"allOf": [ref("WorkJobSummary"), obj({"native": s("The id the plugin knows the job by."), "view": {}, "history": arr(ref("WorkHistoryEntry"))}, extra=True)],
                "description": "`view` is what the plugin drew when it prepared the job and is kept until the server restarts."},
    "WorkJobPage": obj({"total": I, "offset": I, "limit": I, "items": arr(ref("WorkJobSummary"))}, ["total", "offset", "limit", "items"]),
    "Notification": obj({"id": I, "at": I, "kind": s(), "title": s("In English; a client shows its own text for the `kind`."), "data": {"type": "object", "additionalProperties": True}, "read": B},
                        ["id", "kind", "title", "read"]),
    "NotificationList": obj({"notifications": arr(ref("Notification"))}, ["notifications"]),
    "ReadNotifications": obj({"ids": arr(I), "all": B}, description="The ones named, or all of those the caller sees."),
    "Marked": obj({"marked": I}, ["marked"]),
})

W_READ = dict(scope="machines:view")
op("GET", "/plugins", "work", "viewer", "List the machine plugins", ok=(200, "PluginList", "application/json"), **W_READ,
   desc="`bungard` (the machines of DRILLPRO and ROUTEPRO, the same as `/machines`) and `demo` (a machine with no hardware), with what each can do.")
op("GET", "/work/machines", "work", "viewer", "List the machines of every plugin", ok=(200, "WorkMachineList", "application/json"), **W_READ,
   desc="The machines of `/machines` appear here as machines of the `bungard` plugin, with their state; the ones of the other plugins are made with `POST /work/machines`.")
op("POST", "/work/machines", "work", "admin", "Make a machine of a plugin", scope="machines:manage", body="NewWorkMachine", ok=(201, "WorkMachineRecord", "application/json"),
   errors=["bad_id", "bad_name", "unknown_plugin", "not_supported", "bad_plugin_request", "machine_exists"],
   desc="Only for a plugin that keeps its machines here (`demo`); for `bungard` use `POST /machines`. The id cannot be one of any other machine.",
   example={"id": "demo-1", "name": "Demo machine", "plugin": "demo", "settings": {"step_ms": 20}})
op("GET", "/work/machines/{id}", "work", "viewer", "Read a machine of a plugin", ok=(200, "WorkMachineInfo", "application/json"), errors=MACHINE_ERR, **W_READ)
op("PUT", "/work/machines/{id}", "work", "admin", "Change a machine made through the plugins", scope="machines:manage", body="WorkMachineChange", ok=(200, "WorkMachineRecord", "application/json"),
   errors=MACHINE_ERR + ["bad_name", "bad_plugin_request", "unknown_plugin"], example={"name": "Demo machine 2"})
op("DELETE", "/work/machines/{id}", "work", "admin", "Remove a machine made through the plugins", scope="machines:manage", errors=MACHINE_ERR + ["machine_busy"],
   desc="Refused while a job of the queue is open on it.")
WJOB_ERR = ["unknown_job", "plugin_failed"]
op("GET", "/work/jobs", "work", "viewer", "List the jobs of the generic queue, newest first", ok=(200, "WorkJobPage", "application/json"), **W_READ,
   query=[q("machine", S, "Only this machine."), q("state", WORK_STATE, "Only this state."), q("app", S, "Only the jobs asked by this application."),
          q("mine", B, "Only the caller's."), q("limit", {"type": "integer", "minimum": 1, "maximum": 500}, "Jobs per page (50)."), q("offset", {"type": "integer", "minimum": 0}, "Jobs to skip.")],
   desc="The jobs that are open are read from their plugins first.")
op("POST", "/work/jobs", "work", "operator", "Prepare a job on a machine of any plugin", scope="jobs:prepare", body="NewWorkJob", ok=(201, "WorkJob", "application/json"),
   errors=["unknown_machine", "bad_plugin_request", "bad_app", "queue_full", "plugin_failed"],
   desc="The plugin of the machine checks the request and makes the job ready (`queued`); nothing moves until a person starts it. At most `work_queue_max` jobs wait.",
   example={"machine": "demo-1", "request": {"kind": "demo", "steps": 10}, "name": "test run", "app": "studio"})
op("GET", "/work/jobs/{id}", "work", "viewer", "Read a job with its history", ok=(200, "WorkJob", "application/json"), errors=["unknown_job"], **W_READ,
   desc="The state is read from the plugin first.")
op("DELETE", "/work/jobs/{id}", "work", "operator", "Remove a finished job", scope="jobs:cancel", errors=["unknown_job", "job_active"])
op("POST", "/work/jobs/{id}/start", "work", "operator", "Start a queued job", scope="jobs:start", ok=(200, "WorkJob", "application/json"),
   errors=WJOB_ERR + ["not_ready", "machine_busy", "unknown_machine", "bad_plugin_request"], desc="One job at a time on a machine.")
op("POST", "/work/jobs/{id}/pause", "work", "operator", "Hold a running job", scope="jobs:start", ok=(200, "WorkJob", "application/json"),
   errors=WJOB_ERR + ["not_in_state", "not_supported"], desc="A `bungard` machine holds only free G-code jobs (`not_supported` otherwise).")
op("POST", "/work/jobs/{id}/resume", "work", "operator", "Let a held job go on", scope="jobs:start", ok=(200, "WorkJob", "application/json"),
   errors=WJOB_ERR + ["not_in_state", "not_supported"])
op("POST", "/work/jobs/{id}/cancel", "work", "operator", "Cancel a queued job or stop a running one", scope="jobs:cancel", ok=(200, "WorkJob", "application/json"),
   errors=WJOB_ERR + ["not_cancellable", "not_in_state"])
op("GET", "/events/stream", "work", "viewer", "Open the stream of events (server-sent events)", security=SECURITY_OPEN, ok=(200, S, "text/event-stream"),
   errors=["too_many_clients"],
   query=[q("machine", S, "Only the events about this machine."), q("project", S, "Only the events about this project."), q("job", S, "Only the events about this job."),
          q("app", S, "Only the events of this application."), q("kinds", S, "Kinds separated by commas; `job` matches `job.state`, `job.created`..."),
          q("last_event_id", {"type": "integer", "minimum": 0}, "Where to go on from (the header `Last-Event-ID` does the same; a browser sends it by itself when it reconnects)."),
          q("ticket", S, "A ticket from `POST /tickets` (an EventSource cannot set a header)."), q("token", S, "The token itself; only when `url_token` is on.")],
   desc="Every event is `id: <number>`, `event: <kind>` and `data: <JSON>`. First `hello` (`last_id`, `resumed`, `resync`, `clients`), then the events of the kinds `project.*`, `job.created`, `job.state`, `job.progress` "
        "(every tenth of the work), `machine.*`, `catalog.changed` and `notification`, those the caller may see and its token's scopes allow. With `Last-Event-ID` the events kept after that number "
        "(the last `events_queue`) come first; `resync: true` in the `hello` says some were lost and the client has to ask for the state again, as does the event `resync` for a client that falls behind. "
        "A comment line (`: ping`) every `events_ping_s` seconds keeps the connection alive; the stream ends when the login ends. At most `events_clients_max` clients listen at once. "
        "The websocket `GET /events` is not changed.")
op("GET", "/notifications", "work", "viewer", "List the notifications of the caller", ok=(200, "NotificationList", "application/json"), **W_READ,
   query=[q("unread", B, "Only the ones not read."), q("limit", {"type": "integer", "minimum": 1, "maximum": 500}, "Up to this many (50).")],
   desc="A job that ended, failed, is held or waits for the operator makes a notification for the person who asked for it (the administrators see all). Kept in memory, the last `notifications_keep`.")
op("POST", "/notifications/read", "work", "viewer", "Mark notifications as read", body="ReadNotifications", ok=(200, "Marked", "application/json"), **W_READ,
   example={"all": True})

# -- the single login of every application (docs/AUTH.md)
SCHEMAS.update({
    "AppLoginRequest": obj({"name": s(), "password": s(), "app": s("The application the login is for: one of `GET /auth/apps` (`studio`, `android`, `ios`, `watch`, `hmi`, `pcb`, `cam`...).")},
                           ["name", "password", "app"]),
    "AppLoginResponse": obj({"token": s(), "name": s(), "role": ref("Role"), "app": s(), "session_id": s("Shown in the list of sessions; the token never is."), "expires_s": I},
                            ["token", "name", "role", "app", "session_id", "expires_s"]),
    "AuthClaims": obj({
        "name": s(), "role": ref("Role"), "is_token": B, "app": nullable(S), "session_id": nullable(S), "created": nullable(I), "last_seen": nullable(I),
        "expires": nullable(I), "token_id": nullable(S), "template": nullable(S), "scopes": nullable(arr(S)),
    }, ["name", "role", "is_token"], description="The claims of the login: who, with which role, for which application, limited to which scopes (a token of a program has no session, so `app` is null)."),
    "AuthSession": obj({
        "id": s(), "name": s(), "role": ref("Role"), "app": s("Empty for a login made with `POST /login`."), "created": I, "last_seen": I, "expires": I, "ip": s(), "current": B,
    }, ["id", "name", "role", "app", "created", "expires", "current"]),
    "AuthSessionList": obj({"sessions": arr(ref("AuthSession"))}, ["sessions"]),
    "Refreshed": obj({"expires_s": I}, ["expires_s"]),
    "LoggedOutAll": obj({"closed": I}, ["closed"]),
    "AuthPolicy": obj({
        "switches": arr(obj({"key": s(), "value": {}, "safe": B}, ["key", "value", "safe"])),
        "attention": arr(S), "extra_apps": arr(S),
        "editable": arr(S, description="The keys `PUT /auth/policy` can change while the server runs."),
    }, ["switches", "attention", "extra_apps"], description="The switches of security of `server.cfg` with their values (never a secret) and, in `attention`, those that are looser than the recommended value."),
    "AuthApps": obj({"apps": arr(S), "known_only": B}, ["apps", "known_only"]),
})

op("POST", "/auth/login", "auth", "none", "Log in for an application", body="AppLoginRequest", ok=(200, "AppLoginResponse", "application/json"),
   errors=["bad_credentials", "throttled", "bad_app"],
   desc="Like `POST /login`, and the login says which application it is for. Wrong passwords block the name (`auth_max_failures`, `auth_lockout_s`) and the address (`login_ip_limit`); "
        "a person has at most `auth_max_sessions` logins open (the oldest is closed). Every login, failure and refusal is in the audit, never the password or the token.",
   example={"name": "juan", "password": "a long password", "app": "studio"})
op("POST", "/auth/logout", "auth", "any", "End the login")
op("POST", "/auth/logout-all", "auth", "person", "End every login of the caller", ok=(200, "LoggedOutAll", "application/json"))
op("POST", "/auth/refresh", "auth", "person", "Renew the login", ok=(200, "Refreshed", "application/json"), errors=["unknown_session"],
   desc="The login lasts again as long as a login does, but never past `auth_refresh_max_hours` from the moment it was made. The token stays the same.")
op("GET", "/auth/session", "auth", "any", "The claims of the login", ok=(200, "AuthClaims", "application/json"),
   example={"name": "juan", "role": "admin", "is_token": False, "app": "studio"})
op("GET", "/auth/sessions", "auth", "any", "List the open logins of the caller", ok=(200, "AuthSessionList", "application/json"), errors=["forbidden"],
   query=[q("all", B, "Every login of every person; only for an administrator.")],
   desc="The newest first, with the application, the address and when each was last seen; `current` marks the one that asks. A token of a program has none.")
op("DELETE", "/auth/sessions/{id}", "auth", "person", "Close one login", errors=["unknown_session"],
   desc="A person closes their own logins; an administrator closes anybody's. The id is the one the list shows. The token of that login stops working at once.")
op("GET", "/auth/policy", "auth", "admin", "See the switches of security", ok=(200, "AuthPolicy", "application/json"),
   desc="What Studio shows in its security page: the values of the switches in `server.cfg` and which of them deserve attention. No secret is ever in it.")
op("GET", "/auth/apps", "auth", "any", "The applications the server accepts", ok=(200, "AuthApps", "application/json"))

# -- policy changes, keys and audit (docs/AUTH.md), the quota of the store and the design run (docs/DESIGN_RUN.md)
SCHEMAS.update({
    "PolicyChange": obj({
        "key": s("One of `editable` in `GET /auth/policy`."), "value": {"description": "A boolean or a whole number, as the key takes."},
        "confirm_weak": {"type": "boolean", "default": False, "description": "Needed when the new value is looser than the recommended one."},
    }, ["key", "value"], description="Changes one switch of security while the server runs. The change is kept in `policy.json` of the data folder and wins over `server.cfg` for that key."),
    "AuthKeys": obj({
        "tokens": arr(obj({
            "id": s(), "name": s(), "role": ref("Role"), "created": I, "last_used": nullable(I), "expires": nullable(I), "age_s": I, "template": nullable(S),
        }, ["id", "name", "role", "created", "age_s"])),
        "tls": obj({"configured": B, "sha256": nullable(s("SHA-256 of the certificate, in hexadecimal."))}, ["configured", "sha256"],
                   description="The certificate that `tls_cert_file` names, the one the proxy in front of the server shows; the server does not terminate TLS itself."),
    }, ["tokens", "tls"], description="Identifiers, ages and the fingerprint of the certificate; never a secret."),
    "AuditLine": obj({"at": I, "event": s(), "who": s()}, ["at", "event"], extra=True, description="The fields the event was written with (`who`, `role`, `ip`, ...); never a password or a token."),
    "AuditLines": obj({"enabled": B, "lines": arr(ref("AuditLine"))}, ["enabled", "lines"], description="The newest first. `enabled` is false when `audit_file` is off and there is nothing to read."),
    "AppQuota": obj({
        "app": s(),
        "limits": obj({"project_max_mb": I, "project_max_file_mb": I, "project_max_files": I, "project_max_projects": I},
                      ["project_max_mb", "project_max_file_mb", "project_max_files", "project_max_projects"]),
        "usage": obj({"projects_in_app": I, "projects_left": I, "visible_projects": I, "bytes": I, "files": I},
                     ["projects_in_app", "projects_left", "visible_projects", "bytes", "files"]),
        "projects": arr(obj({"id": s(), "name": s(), "bytes": I, "files": I, "bytes_left": I, "files_left": I}, ["id", "name", "bytes", "files", "bytes_left", "files_left"])),
    }, ["app", "limits", "usage", "projects"], description="The limits of the store and what the application uses of them, for the projects the caller sees (at most 200 listed, the last changed first)."),
    "DesignJobRequest": obj({
        "kind": {"type": "string", "enum": ["check", "bom", "fabpack", "panel"]},
        "project": s("The id of a project of the `pcb` application."),
        "file": s("The board file of the project (`options.file` also works)."),
        "options": obj({
            "profile": s(), "lang": s(), "panel_boards": {"type": "integer", "minimum": 1, "maximum": 1000}, "dnp": arr(S),
        }, description="Only `fabpack` takes options (`profile`, `lang`, `panel_boards`, `dnp`); the other kinds take none."),
    }, ["kind", "project"], extra=True),
})
op("PUT", "/auth/policy", "auth", "admin", "Change a switch of security while the server runs", body="PolicyChange", ok=(200, "AuthPolicy", "application/json"),
   errors=["bad_policy", "weak_confirm_required", "storage"],
   desc="One switch at a time, answering the new policy. Only the keys in `editable` can change; the others need a restart. A value looser than recommended (`safe: false`) changes nothing "
        "unless the body says `confirm_weak: true`. Every change is in the audit with the old and the new value.",
   example={"key": "password_min_len", "value": 12})
op("GET", "/auth/keys", "auth", "admin", "The identifiers of the keys and the fingerprint of the certificate", ok=(200, "AuthKeys", "application/json"),
   desc="The tokens of programs by id, name, role and age, and the SHA-256 fingerprint of the certificate named by `tls_cert_file`. No secret is ever in it.")
op("GET", "/audit", "admin", "admin", "Read the audit", ok=(200, "AuditLines", "application/json"),
   query=[q("since", I, "Only the lines from this moment (seconds since 1970)."), q("until", I, "Only the lines up to this moment."),
          q("who", S, "Only the lines of this person or token."), q("event", S, "Only this event."),
          q("limit", {"type": "integer", "minimum": 1, "maximum": 1000}, "Up to this many lines (100).")],
   desc="Reads `audit.jsonl` and the older copy it was set aside as, the newest first.")
op("GET", "/apps/{app}/quota", "store", "viewer", "The limits of the store and what the application uses", scope="projects:read", ok=(200, "AppQuota", "application/json"),
   errors=["bad_app"], desc="`project_max_mb`, `project_max_file_mb`, `project_max_files` and `project_max_projects` with the use, so that a client shows the limit and not only the use.")
op("POST", "/apps/pcb/jobs", "work", "operator", "Run the design program on a board of a project", scope="jobs:prepare", body="DesignJobRequest", ok=(201, "WorkJob", "application/json"),
   errors=["design_run_unavailable", "bad_design_request", "unknown_project", "unknown_file", "bad_filename", "queue_full", "forbidden_scope"],
   desc="Runs the installed program (`design_program`) with `design_run` on, no shell and an empty environment, in a folder of its own, with a time and an output limit. The job is in the queue "
        "(plugin `phoenix`) and ends `done` or `failed`; the report is written to the project as `reports/<kind>.json`. The token also needs `projects:write`. Without the setting: `501 design_run_unavailable`.",
   example={"kind": "check", "project": "p-0190", "file": "board.json", "options": {}})

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
