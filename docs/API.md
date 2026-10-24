# The API of the server

Everything is under `/api/v1/` and answers JSON. A mistake is `{"error": "<code>", "detail": "..."}` with the status that fits. Except the health check, the first set-up and the login, every request carries `Authorization: Bearer <token>`: the token of a login or the token of a program. A **viewer** reads, an **operator** also prepares, starts, stops and confirms jobs, an **administrator** also manages users, tokens and machines. The default port is 18200.

## Who is who

| Request | Role | What it does |
|---|---|---|
| `GET /health` | none | `{"name":"eh3d-server","version":"..."}` |
| `GET /setup` | none | `{"needed": true}` while there is no user |
| `POST /setup` | none | `{"code","name","password"}`: makes the first administrator with the set-up code the server prints on its console |
| `POST /login` | none | `{"name","password"}` gives `{"token","name","role","expires_s"}`; five wrong passwords block the name for a while (`429 throttled`) |
| `POST /logout` | any | ends the login |
| `GET /session` | any | `{"name","role","is_token"}` |
| `PUT /account` | a person | `{"old","new"}`: changes the own password |
| `GET /users`, `POST /users` | admin | `{"name","role","password"}` creates |
| `PUT /users/:name`, `DELETE /users/:name` | admin | `{"role"?,"password"?,"disabled"?}`; the last administrator cannot be deleted, disabled or demoted (`409 last_admin`) |
| `GET /tokens`, `POST /tokens`, `DELETE /tokens/:id` | admin | tokens for programs (`{"name","role"}` gives `{"id","token"}`: **the token is shown this once**); a program uses it like a login token |

Passwords are 8 to 128 characters; names are letters, digits, `.`, `_` and `-` up to 32.

## Machines

A machine is how to reach it (a node over the network, or the simulator), the text of its configuration files and the values tuned on it.

```json
{
  "id": "bungard",
  "name": "Bungard CCD",
  "model": "ccd-mtc",
  "connection": { "kind": "node", "address": "192.168.0.50:5800", "token": "the node's access token" },
  "dcf": "<the text of DRILLPRO.DCF>",
  "rcf": "<the text of ROUTEPRO.RCF>",
  "cfg": "y_search_hz = 2000\nspindle = node\n"
}
```

`connection.kind` is `node` or `demo` (the simulator: it never raises the control unit's home line, so its searches are sent in full). `cfg` is in the format of the programs' machine file (`docs/CONNECTION.md` of EH3D-DRILLPRO lists every key); a typo is refused (`422 bad_cfg`) and the connection keys in it are ignored. The node's token is never shown by the API (`token_set: true` instead) and an update that leaves it empty keeps the stored one.

| Request | Role | What it does |
|---|---|---|
| `GET /machines`, `GET /machines/:id` | viewer | the machines |
| `POST /machines` | admin | creates one |
| `PUT /machines/:id`, `DELETE /machines/:id` | admin | changes or removes one (not while a job runs on it: `409 machine_busy`) |
| `POST /machines/:id/preview` | operator | plans a job and answers its view, creating nothing |
| `POST /machines/:id/jobs` | operator | plans a job and keeps it **ready** |

## Jobs

A job request is a drilling job or a routing job, with the work as data or as the file the programs read.

```json
{ "kind": "drill", "name": "board 1", "board_mm": 1.6,
  "tools": [ { "number": 1, "extra_stroke_mm": 0.0, "feed_mm_min": 1000, "spindle_rpm": 24000 } ],
  "holes": [ { "tool": 1, "x_mm": 10.0, "y_mm": 10.0 } ] }

{ "kind": "drill", "sorting": "special", "files": { "ncd": "<the text of the .NCD>", "dtd": "<the text of the tool table>" } }

{ "kind": "route", "tools": [ { "number": 1, "depth_mm": 1.7, "iterations": 2, "feed_xy_mm_min": 800, "feed_z_mm_min": 300, "spindle_rpm": 24000 } ],
  "paths": [ { "pen": 1, "points_mm": [[10,10],[40,10],[40,30]] } ] }

{ "kind": "route", "files": { "plt": "<the text of the HP-GL file>", "rtd": "<the text of the tool table>" } }
```

The holes or the files, not both. With files the holes are ordered as the program does (`sorting`: `none`, `x`, `y` or `special`; by default the machine's configuration says) and their units come from the machine's configuration file. A job is refused with a code before it exists: `no_work`, `both_sources`, `unknown_tool` (a tool the work uses is not in the table), `bad_tool`, `bad_board`, `out_of_table` (a hole or a point outside the course of the machine), `bad_ncd`, `too_big`, `bad_request`.

The **view** is what a client draws and follows:

```json
{ "kind": "drill", "name": "board 1",
  "tools": [ { "number": 1, "spindle_rpm": 24000, "count": 2, "feed_mm_min": 1000, "depth_mm": null, "iterations": null } ],
  "holes": [ { "tool": 1, "x_mm": 10.0, "y_mm": 10.0 } ],
  "paths": [], "bounds_mm": [10.0, 10.0, 30.0, 20.0],
  "items": 34, "hole_ends": [14, 17, 24], "estimate_s": 28.8, "travel_mm": 10.0 }
```

`holes` are in the order the machine drills them (by tool in order of first use). `hole_ends[i]` is the item at which the hole `i` is finished, so a client that knows the item the job has reached knows how many holes are done. `estimate_s` is the time of the movements without the dialogs of the operator.

| Request | Role | What it does |
|---|---|---|
| `GET /jobs?machine=&state=` | viewer | the jobs, newest first, without their view |
| `GET /jobs/:id` | viewer | one job with its view |
| `POST /jobs/:id/start` | operator | plans it again with the machine's constants of this moment and runs it; **a job never starts by itself**; one job at a time on a machine (`409 machine_busy`) |
| `POST /jobs/:id/confirm` | operator | answers the dialog the job waits at |
| `POST /jobs/:id/cancel` | operator | stops a running job (the machine stops, the drivers off) or cancels a ready one |
| `DELETE /jobs/:id` | operator | removes a job that is not running |

States: `ready` → `connecting` → `running` ⇄ `waiting_operator` → `done`, `cancelled` or `failed` (`message` says why). A job that was running when the server stopped is marked `failed`: nobody knows where the machine was. While it waits, `pause` is `{"kind":"tool_change","tool":2}` (put the tool in and confirm) or `{"kind":"spindle_start"}` (confirm the spindle start). A job has `items_done` of `items_total` and `holes_done` of `holes_total` (a routing job counts paths).

## What happens, as it happens

`GET /events?token=<token>` upgrades to a websocket (a browser cannot set headers on one, so the token goes in the address). The first text is `{"type":"hello","jobs":[...]}` with the summaries of the jobs; after it every change arrives as `{"type":"job","job":{...}}`, `{"type":"job_removed","id":"..."}` or `{"type":"machine","id":"..."}`.
