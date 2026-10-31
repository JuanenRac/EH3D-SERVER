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

`connection.kind` is `node`, `demo` or `grbl`. `demo` is the simulator: it never raises the control unit's home line, so its searches are sent in full, and it takes free G-code jobs too. `grbl` is a GRBL controller on a serial port of the computer the server runs on (`{"kind":"grbl","port":"/dev/ttyUSB0","baud":115200}`, the baud rate defaults to 115200 and goes from 1200 to 2000000); it takes G-code jobs only, and a server built without the `serial` feature (`cargo build --release --features serial`) marks such a job as failed when it starts, saying so. A `node` machine takes drilling and routing jobs only; a job of the other kind is refused with `422 wrong_machine`. `cfg` is in the format of the programs' machine file (`docs/CONNECTION.md` of EH3D-DRILLPRO lists every key); a typo is refused (`422 bad_cfg`) and the connection keys in it are ignored. The node's token is never shown by the API (`token_set: true` instead) and an update that leaves it empty keeps the stored one.

| Request | Role | What it does |
|---|---|---|
| `GET /machines`, `GET /machines/:id` | viewer | the machines (each with its `station`, the short name of the bench, the room or the workshop it belongs to; empty when it has none) |
| `GET /fleet` | viewer | every machine at once, grouped by station: `stations` (`id` and the `machines` in it), `machines` (`id`, `name`, `model`, `station`, `connection`, `state`: `idle`, `busy` or `waiting` for the operator, the active `job` with `percent`, `items_done`, `items_total`, `pause`, and the number of jobs `ready`) and a `summary` with the counts |
| `POST /machines` | admin | creates one |
| `PUT /machines/:id`, `DELETE /machines/:id` | admin | changes or removes one (not while a job runs on it: `409 machine_busy`) |
| `POST /machines/:id/preview` | operator | plans a job and answers its view, creating nothing |
| `POST /machines/:id/jobs` | operator | plans a job and keeps it **ready** |

## Jobs

A job request is a drilling job, a routing job or a free G-code program. A drilling or routing job carries the work as data or as the file the programs read.

```json
{ "kind": "drill", "name": "board 1", "board_mm": 1.6,
  "tools": [ { "number": 1, "extra_stroke_mm": 0.0, "feed_mm_min": 1000, "spindle_rpm": 24000 } ],
  "holes": [ { "tool": 1, "x_mm": 10.0, "y_mm": 10.0 } ] }

{ "kind": "drill", "sorting": "special", "files": { "ncd": "<the text of the .NCD>", "dtd": "<the text of the tool table>" } }

{ "kind": "route", "tools": [ { "number": 1, "depth_mm": 1.7, "iterations": 2, "feed_xy_mm_min": 800, "feed_z_mm_min": 300, "spindle_rpm": 24000 } ],
  "paths": [ { "pen": 1, "points_mm": [[10,10],[40,10],[40,30]] } ] }

{ "kind": "route", "files": { "plt": "<the text of the HP-GL file>", "rtd": "<the text of the tool table>" } }
```

Both kinds of drilling and routing job take two more fields. `skip_tools` is a list of tool numbers (pens for a routing job) that are switched off: their holes or paths are left out of the job, and a tool that is off does not have to be in the tool table. `skip` is how many holes (counted in the order the machine drills them, tool by tool) or paths (in the order sent) are already done, so a job that goes on after a stop does only the rest. A job whose work is all left out by them is refused with `no_work`, and the view says what was left out in `skipped_tools` and `skipped`. A program that sends the job of a window (DRILLPRO, ROUTEPRO, BUNGARD-GUI) sends the holes it chose together with `skip_tools`, so the server and the window agree; a drilling job and a routing job in a row are two jobs, started one after the other on the machine.

A G-code program (`"kind": "gcode"`) is the text of the program. It runs on a machine whose connection is `demo` or `grbl`:

```json
{ "kind": "gcode", "name": "frame", "gcode": "G21 G90\nG0 X10 Y10\nG1 X40 F300\nM30\n", "skip": 0, "restore_outputs": false }
```

The lines that are sent are the code without its comments (in parentheses or after `;`); blank lines and `%` are not sent. A line that is not G-code is refused with `bad_gcode` (and the line it is on): the settings and real-time commands of the controller (`$`, `!`, `~`, `?`) and control characters cannot be part of a job, and a line may have 200 characters at the most. A program with no line to send is refused with `no_work`, one with more than a million lines with `too_big`. The view has `kind: "gcode"`, no holes, paths or tools, the `bounds_mm` of the straight moves (an arc counts as the line to its end), the time and length of the moves, and `items`, the number of lines that will be sent. The spindle switch of the machine judges the program too: the largest `S` and the largest `F` (converted to mm/min when the program works in inches) are checked against the spindle the machine carries; with `reject` (the default) a program beyond them is refused with `spindle_limit`, with `warn` the `S` words are cut to the top of the spindle and the excesses are listed in `spindle_warnings` (tool `0`).

`skip` is how many lines of the text (counting blank and comment lines) are already done. A program that goes on after a stop does not send them again: the codes they put in force (units, distance mode, plane, work coordinate system, feed mode and the last `F`) are given again in a line before the first one, and the job **waits for the operator before its first move** (state `waiting_operator` with `pause.kind` `resume`; `POST /jobs/:id/resume` or `confirm` lets it go). The spindle and the coolant are started again only when `restore_outputs` is `true`. A failed or stopped job says in `line` where to start from again. The offset of `G92` is not given back, and a controller that was reset needs its work zero set again.

The holes or the files, not both. With files the holes are ordered as the program does (`sorting`: `none`, `x`, `y` or `special`; by default the machine's configuration says) and their units come from the machine's configuration file. A job is refused with a code before it exists: `no_work`, `both_sources`, `spindle_limit` (beyond what the spindle of the machine gives, see below), `unknown_tool` (a tool the work uses is not in the table), `bad_tool`, `bad_board`, `out_of_table` (a hole or a point outside the course of the machine), `bad_ncd`, `too_big`, `bad_request`.

The **view** is what a client draws and follows:

```json
{ "kind": "drill", "name": "board 1",
  "tools": [ { "number": 1, "spindle_rpm": 24000, "count": 2, "feed_mm_min": 1000, "depth_mm": null, "iterations": null } ],
  "holes": [ { "tool": 1, "x_mm": 10.0, "y_mm": 10.0 } ],
  "paths": [], "bounds_mm": [10.0, 10.0, 30.0, 20.0],
  "items": 34, "hole_ends": [14, 17, 24], "estimate_s": 28.8, "travel_mm": 10.0,
  "skipped_tools": [], "skipped": 0 }
```

`holes` are in the order the machine drills them (by tool in order of first use). `hole_ends[i]` is the item at which the hole `i` is finished, so a client that knows the item the job has reached knows how many holes are done. `estimate_s` is the time of the movements without the dialogs of the operator.

| Request | Role | What it does |
|---|---|---|
| `GET /jobs?machine=&state=` | viewer | the jobs, newest first, without their view |
| `GET /jobs/:id` | viewer | one job with its view |
| `POST /jobs/:id/start` | operator | plans it again with the machine's constants of this moment and runs it; **a job never starts by itself**; one job at a time on a machine (`409 machine_busy`) |
| `POST /jobs/:id/confirm` | operator | answers the dialog the job waits at |
| `POST /jobs/:id/cancel` | operator | stops a running job (the machine stops, the drivers off) or cancels a ready one |
| `POST /jobs/:id/pause` | operator | holds a running G-code job: the controller holds its movement and no line goes out until `resume`; `409 not_pausable` for a drilling or routing job, `409 not_in_state` when it is not running |
| `POST /jobs/:id/resume` | operator | lets a held G-code job go on (also the wait of a job that goes on after a stop); `409 not_in_state` when it is not held. `confirm` does the same for a G-code job |
| `DELETE /jobs/:id` | operator | removes a job that is not running |

States: `ready` → `connecting` → `running` ⇄ `waiting_operator` → `done`, `cancelled` or `failed` (`message` says why). A job that was running when the server stopped is marked `failed`: nobody knows where the machine was. While it waits, `pause` is `{"kind":"tool_change","tool":2}` (put the tool in and confirm) or `{"kind":"spindle_start"}` (confirm the spindle start); for a G-code job it is `{"kind":"hold"}` (the operator paused it) or `{"kind":"resume"}` (it goes on after a stop and waits before its first move). A job is also given its `line`: for a G-code job the line of the program text it has got to (where a job that goes on starts from), 0 for the others. A job has `items_done` of `items_total` and `holes_done` of `holes_total` (a routing job counts paths; a G-code job has no holes, and its items are the lines sent). A G-code job that is refused by the controller (a line it does not take, an alarm, a link that fails) is marked `failed` with the line in `message`, and the server stops the controller as far as it can: nobody is at the screen to decide. A job is done when the machine is still after its last line, and fails if it is not within `gcode_finish_wait_s` seconds of `server.cfg`.

## Updates

For an administrator, when `updates_dir` is set in `server.cfg` (see `docs/DEPLOY.md`). The server does not talk to GitHub: it leaves a request in the folder and the helper `deploy/eh3d-update.py` does the rest.

| Request | What it does |
|---|---|
| `GET /api/v1/updates` | `{ "enabled", "pending", "state", "message", "log", "components": [{ "id", "name", "installed", "latest", "newer", "url", "published" }] }`; `state` is `idle`, `checking`, `updating`, `done` or `error`. Without `updates_dir`: `{ "enabled": false, ... }`. |
| `POST /api/v1/updates/check` | Asks the helper to look at the latest releases on GitHub. 501 `updates_off` without `updates_dir`; 409 `update_refused` when a request waits or the helper is working. |
| `POST /api/v1/updates/apply` | Body `{ "components": ["server", "studio"] }`: asks the helper to update those components (the ones that are not newer are not touched by the Studio, which sends only the newer). 409 for a component that cannot be updated or for a request that is already waiting. |

The components are `server` (EH3D-SERVER, installed as `/opt/eh3d-server`) and `studio` (EH3D-STUDIO, the folder of its pages). The helper checks the SHA-256 of each file against the one the release publishes, and keeps the old folder as `.previous`.

## The node of the drill head

For a machine whose configuration names the node of its drill head (`head_node`, `head_address` and `head_token`, the keys of the programs' machine file). The server asks the node for the viewer or the operator, with the token of the machine, so the node's token never leaves the server.

| Request | Role | What it does |
|---|---|---|
| `GET /api/v1/machines/:id/head/status` | viewer | the status of the node as it answers it (`node`, `camera`, `camera_model`, `laser`, `rpm`, `temp_c`, `heat`, `protect_open`, `uptime_s`) |
| `POST /api/v1/machines/:id/head/measure[?samples=N]` | operator | one measure of the table under the head (`samples` from 1 to 31); the node's own answer and status code, so a refused measure says why |
| `GET /api/v1/machines/:id/head/stream[?token=T]` | viewer | the camera of the node as MJPEG, handed on while it comes; the token may be in the address because a browser cannot set a header on a picture |
| `GET /api/v1/machines/:id/head/light` | viewer | the ring of LEDs around the camera as the node says it (`on`, `level`, `max_level`, `tone`, `sectors`, `leds`, `auto_off_s`, `color`, `cue`) |
| `POST /api/v1/machines/:id/head/light?on=1&level=60&tone=white&sectors=all` | operator | turns the ring on or off and changes how it shines: `on` (`0` or `1`), `level` (0 to 100, kept under the ceiling of the node), `tone` (`white`, `cool`, `warm`, `red`, `green`, `blue`), `color` (six hexadecimal digits), `sectors` (`all`, `front`, `right`, `back`, `left`, `front_back`, `left_right` or `0x` and two digits) and `cue` (`ok`, `nok`, `busy`, `warn`, `alarm`, `none`) with `cue_ms`; the node's own answer and status code |
| `GET /api/v1/machines/:id/head/snapshot` | viewer | one picture of the camera as JPEG |
| `POST /api/v1/machines/:id/head/inspect` | operator | the quality check of the work: with `{"holes": [{"x_mm", "y_mm", "dia_mm"}], "light": 60, "auto_light": true, "cue": true, "restore_light": false, "profile": {...}}` the server lights the ring (when `light` is given), takes a picture, judges it with the profile of the machine (the holes of the job that the camera sees, from the centre of the picture, x to the right and y up, in mm; the milled channels; the picture itself) and makes the ring say how it went. The answer is the report: `verdict` (`ok`, `not_ok` or `unknown`), `quality`, `holes`, `milling` and `findings` (a `code`, the `hole` it is about and whether it `fails`) |
| `GET /api/v1/machines/:id/head/inspect` | viewer | the last check of the machine (`404 no_inspection` until one is made after the server starts) |
| `GET /api/v1/machines/:id/head/inspect/profile` | viewer | the profile of the analyser of the machine: the scale of the picture (`px_per_mm`), which checks are on and every tolerance |
| `PUT /api/v1/machines/:id/head/inspect/profile` | operator | changes the profile: `{"preset": "standard" \| "fine_pitch" \| "prototype"}` starts from a preset (the scale is kept) and any member of the profile changes what it names; what cannot be used is listed in `rejected` and changes nothing |
| `POST /api/v1/machines/:id/head/inspect/calibrate` | operator | sets `px_per_mm` from the first hole of the last check that was found (how many pixels it covered against its width in the job); `409 no_hole` when none was found |

Refusals: `422 bad_light` (the query for the ring has signs the node does not use), `422 too_many_holes` (more than 500), `422 bad_profile`, `502 bad_picture` (the picture of the node could not be read), `404 no_head` (the machine has no `head_address`), `422 bad_address`, `422 bad_samples`, `502 head_unreachable` (the node does not answer in four seconds) and `502 head_refused` (the node answered with a status other than 200).

## The spindle of the machine

A machine carries one of two spindles, `original` (the one it leaves the factory with) or `vfd` (with a variable-frequency drive that the node of the machine drives), or none is said (empty: no job is judged against a spindle). The limits of each are the starting ones (`original`: 10000 to 60000 turns, 3000 mm/min along the cut, 4000 mm/min of plunge, bits up to 3.2 mm; `vfd`: 6000 to 24000 turns, 1500 and 1200 mm/min, bits up to 6 mm) with what the machine changed on top. They are also members of the machine (`spindle`, `spindle_guard`, `spindle_original`, `spindle_vfd`); a `PUT /machines/:id` that does not send them leaves them as they were.

| Request | Role | What it does |
|---|---|---|
| `GET /api/v1/machines/:id/spindle` | viewer | `{"spindle","guard","limits":{"original":{...},"vfd":{...}},"active","node_max_rpm"}`: the spindle, the switch (`reject` or `warn`), the limits of both (`min_rpm`, `max_rpm`, `power_w`, `max_xy_mm_min`, `max_z_mm_min`, `max_bit_mm`), those of the spindle in use and its top speed, which is what `spindle.max_rpm` of the node of the machine should be (`null` with no spindle) |
| `PUT /api/v1/machines/:id/spindle` | admin | `{"spindle"?,"guard"?,"original"?,"vfd"?}`: sets what is sent; `original` and `vfd` take the limits that differ from the starting ones (`{}` goes back to them). The answer is the same as the `GET`. `422 bad_spindle` for a spindle that is not one of the two, a switch that is not `reject` or `warn`, or limits that cannot be used (the slowest above the fastest, a zero feed); nothing changes then |

With a spindle chosen, the planning (the preview, the creation of a job and its start) judges each tool the job uses: the turns above the top of the spindle (`rpm_above_max`) or, when not zero, below the slowest (`rpm_below_min`), the feed along the cut of a routing job (`feed_xy_above_max`) and the plunge of a routing job or the feed of a drilling job (`feed_z_above_max`). With the switch on `reject` (the default) a job with any excess is refused with `422 spindle_limit`, the detail saying which tool and by how much; with `warn` it goes through, the turns are cut to the top of the spindle (that is what the node is sent) and the view lists the excesses in `spindle_warnings` (`code`, `tool`, `value`, `limit`).

## What happens, as it happens

`GET /events?token=<token>` upgrades to a websocket (a browser cannot set headers on one, so the token goes in the address). The first text is `{"type":"hello","jobs":[...]}` with the summaries of the jobs; after it every change arrives as `{"type":"job","job":{...}}`, `{"type":"job_removed","id":"..."}` or `{"type":"machine","id":"..."}`.
