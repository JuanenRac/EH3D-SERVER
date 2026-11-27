# The API of the server

Everything is under `/api/v1/` and answers JSON. A mistake is `{"error": "<code>", "detail": "..."}` with the status that fits. Except the health check, the first set-up and the login, every request carries `Authorization: Bearer <token>`: the token of a login or the token of a program. A body is a JSON object: a list in its place is refused (`422 bad_request`), and an optional body that does not read is refused (`400 bad_request`), never ignored. A **viewer** reads, an **operator** also prepares, starts, stops and confirms jobs, an **administrator** also manages users, tokens and machines. The default port is 18200.

**The contract of the routes.** The list of the routes, with the method, the role and the scope each one needs, the schemas of what it takes and gives, an example and the error codes it can answer with, is `docs/openapi.yaml` (OpenAPI 3.1; `docs/openapi.json` is the same document for the tests, and `docs/ROUTES.md` is its table of routes). They are written by `tools/make_openapi.py` and checked against the router by `tests/openapi.rs` and `tools/check_openapi.py`. This document explains how the server behaves; when the two seem to differ, the OpenAPI document is the contract of the routes and this text is what to fix.

## Who is who

| Request | Role | What it does |
|---|---|---|
| `GET /health` | none | `{"name":"eh3d-server","version":"...","api_revision":1,"status":"ok"}`; `status` is `degraded` when the data folder takes no file or a lock was taken over after a panic. With a valid token also `uptime_s`, `storage` (`writable`), `recovered_locks` and `features` (`serial`, `studio`, `updates`, `url_token`, `local_until_setup`, `trust_forwarded_for`, `exit_on_panic`, `persist_sessions`) |
| `GET /setup` | none | `{"needed": true}` while there is no user |
| `POST /setup` | none | `{"code","name","password"}`: makes the first administrator with the set-up code the server prints on its console |
| `POST /login` | none | `{"name","password"}` gives `{"token","name","role","expires_s"}`; five wrong passwords block the name for a while (`429 throttled`). The login survives a restart of the server (`persist_sessions` of `server.cfg`, off by default: only the SHA-256 of the token is kept on the disk) until it expires or is ended with `POST /logout` |
| `POST /logout` | any | ends the login |
| `GET /session` | any | `{"name","role","is_token"}` |
| `PUT /account` | a person | `{"old","new"}`: changes the own password; wrong old passwords count like wrong passwords of a login (`429 throttled`) |
| `GET /users`, `POST /users` | admin | `{"name","role","password"}` creates |
| `PUT /users/:name`, `DELETE /users/:name` | admin | `{"role"?,"password"?,"disabled"?}`; the last administrator cannot be deleted, disabled or demoted (`409 last_admin`) |
| `GET /tokens`, `POST /tokens`, `DELETE /tokens/:id` | admin | tokens for programs (`{"name","role","days"?}` gives `{"id","token"}`: **the token is shown this once**); a program uses it like a login token. `days` (1 to 3650, else `422 bad_days`) makes it stop working after them; without it the token lasts as many days as `token_default_days` of `server.cfg` says (0: it does not expire). The list has for each token `created`, `expires` (seconds since 1970, `null` when it does not expire) and `last_used` (`null` while it was never used; kept exact in memory and written to the disk at most once an hour) |

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

`connection.kind` is `node`, `demo` or `grbl`. `demo` is the simulator: it never raises the control unit's home line, so its searches are sent in full, and it takes free G-code jobs too. `grbl` is a GRBL controller on a serial port of the computer the server runs on (`{"kind":"grbl","port":"/dev/ttyUSB0","baud":115200}`, the baud rate defaults to 115200 and goes from 1200 to 2000000); it takes G-code jobs only, and a server built without the `serial` feature (`cargo build --release --features serial`) marks such a job as failed when it starts, saying so. A `node` machine takes drilling and routing jobs only; a job of the other kind is refused with `422 wrong_machine`. `cfg` is in the format of the programs' machine file (`docs/CONNECTION.md` of EH3D-DRILLPRO lists every key); a typo is refused (`422 bad_cfg`) and the connection keys in it are ignored. The node's token is never shown by the API (`token_set: true` instead) and an update that leaves it empty keeps the stored one; the same goes for the keys of `cfg` that end in `_token`, `_secret` or hold a password (`head_token`, `node_token`...): they are shown with an empty value and an update that sends them empty keeps the stored value.

| Request | Role | What it does |
|---|---|---|
| `GET /machines`, `GET /machines/:id` | viewer | the machines (each with its `station`, the short name of the bench, the room or the workshop it belongs to; empty when it has none) |
| `GET /fleet` | viewer | every machine at once, grouped by station: `stations` (`id` and the `machines` in it), `machines` (`id`, `name`, `model`, `station`, `connection`, `state`: `idle`, `busy` or `waiting` for the operator, the active `job` with `percent`, `items_done`, `items_total`, `pause`, `tool` (the drill or pen at work now, or the one the operator is asked to put in; `null` for a G-code job), and the number of jobs `ready`) and a `summary` with the counts; with `pnp_nodes_enabled` on also `pnp_nodes` (the nodes of the pick-and-place machines: `registered`, `ready`, `not_ready`, `by_health`) |
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

A drilling or routing job on a machine with the automatic tool changer may chain its changes with the jobs before and after it and say in which place of the rack each tool is: `"request_version": 2` and a `rack` (`first`, `carry_in`, `carry_out`, `slots`, `carry_slot`, all optional). A request without them, or of the first version, is planned exactly as before. The job and its view answer `rack_applied` (`true` when the plan used the rack, `false` when it was left out, absent when the request had none), the warnings `rack_needs_v2`, `rack_no_changer`, `rack_unknown_field` and `request_version_newer` go in `warnings`, and a rack that does not fit is refused with `422 bad_rack`. The whole contract is in `docs/ATC_CHAIN.md`; the most places a request may name is `rack_max_slots` of `server.cfg`.

A G-code program (`"kind": "gcode"`) is the text of the program. It runs on a machine whose connection is `demo` or `grbl`:

```json
{ "kind": "gcode", "name": "frame", "gcode": "G21 G90\nG0 X10 Y10\nG1 X40 F300\nM30\n", "skip": 0, "restore_outputs": false }
```

The lines that are sent are the code without its comments (in parentheses or after `;`); blank lines and `%` are not sent. A line that is not G-code is refused with `bad_gcode` (and the line it is on): the settings and real-time commands of the controller (`$`, `!`, `~`, `?`, wherever in the line they stand outside a comment), control characters and characters outside ASCII cannot be part of a job (a machine whose administrator switched `allow_dollar` on takes the `$` lines of GRBL such as `$H`, `$X`, `$G`, `$J=...` and the settings, see below; `$RST` and the real-time characters are refused always), and a line may have 200 characters at the most. A program with no line to send is refused with `no_work`, one with more than a million lines with `too_big`. The view has `kind: "gcode"`, no holes, paths or tools, the `bounds_mm` of the straight moves (an arc counts as the line to its end), the time and length of the moves, and `items`, the number of lines that will be sent. The spindle switch of the machine judges the program too: the largest `S` and the largest `F` (converted to mm/min when the program works in inches) are checked against the spindle the machine carries; with `reject` (the default) a program beyond them is refused with `spindle_limit`, with `warn` the `S` words are cut to the top of the spindle and the excesses are listed in `spindle_warnings` (tool `0`).

What the controller would read in another way than the server did is refused with `bad_gcode` too, and `detail` gives the line and the reason. Blanks count for nothing, as GRBL reads them (`S1 000` is `S1000`, `F1 000` is `F1000`), so the limits are judged on what the machine would do, and the unit a line sets (`G20`, `G21`) counts for all its words. Refused are: a line the reader of G-code of the suite cannot read whole (`word-number-invalid`, `character-ignored`, `number-too-large`...), a word written twice or two codes of one group (`word-repeated`), `G93` and `G95` (`feed-mode-unsupported`: the feed could not be judged), canned cycles, cutter compensation, an unknown `G` code with axis words, a fractional `M` code, a negative `F` or `S`, an `E` word, a block-delete line (`/`), a comment in parentheses that is not closed, a carriage return inside a line, code after `M2`/`M30`, and a program that waits for the operator (`M0`, `M1`) with the spindle on. The names in parentheses are the codes of the reader and stand in `detail`; the error code stays `bad_gcode`. A comment is not sent, so what it says does not matter. A program that ends with the spindle or the coolant on, without `M5`, `M9`, `M2` or `M30`, is given `M5` and `M9` by the server after its last line.

`skip` is how many lines of the text (counting blank and comment lines) are already done. A program that goes on after a stop does not send them again: the codes they put in force (units, distance mode, plane, work coordinate system, feed mode and the last `F`) are given again in a line before the first one, and the job **waits for the operator before its first move** (state `waiting_operator` with `pause.kind` `resume`; `POST /jobs/:id/resume` or `confirm` lets it go). The spindle the program had on is started again after that confirmation, as part of the sequence under "Stopping and going on" below; the coolant only when `restore_outputs` is `true`. A failed or stopped job says in `line` where to start from again. A line after a `G92` offset is refused, and a controller that lost its position needs its work zero set again.

Every drilling or routing view carries `dry_run` (the exact steps of the plan followed on paper: `min_mm`, `max_mm` and `end_mm` as X, Y, Z in mm from the switches, `steps`, `seconds` and `within_course`) and `warnings`, what looks wrong in the work (`code`: `tiny`, `too_big`, `far`, `scale`, `out_of_course` or `units_supposed` (the drill file does not say its unit and its numbers were read as mils), with the numbers it is about in `values`, in mm, and `axis` for the last). A plan that takes an axis past the course of the machine's configuration file is refused with `outside_course` unless the tuning of the machine says `dry_run = warn`; `dry_run_margin_mm`, `sanity_tiny_mm` and `sanity_far_mm` are keys of the same tuning. A G-code job has neither.

The holes or the files, not both. A drill file that does not declare how its numbers are written (a machine file without a header, `INCH` alone, digits or zeros left to be supposed) is refused with `bad_ncd` unless the request names the format in `drill_format`: `mils`, `inch-2.4-tz`, `inch-2.4-lz`, `metric-3.3-tz` or `metric-3.3-lz`; what the file declares wins. The `drill` job of the CAM service takes the same name in `params.drill_format`. With files the holes are ordered as the program does (`sorting`: `none`, `x`, `y` or `special`; by default the machine's configuration says) and their units come from the machine's configuration file. A job is refused with a code before it exists: `no_work`, `both_sources`, `spindle_limit` (beyond what the spindle of the machine gives, see below), `unknown_tool` (a tool the work uses is not in the table), `bad_tool`, `bad_board`, `out_of_table` (a hole or a point outside the course of the machine), `outside_course` (the exact plan takes an axis past its course), `bad_ncd` (the drilling file does not read, or has lines that are not holes, such as a slot), `bad_path` (a point that is not a number, or a plot file with something that cannot be read or followed), `too_big`, `bad_rack` (the chaining of the tool changer does not fit, see below), `bad_request`.

The **view** is what a client draws and follows:

```json
{ "kind": "drill", "name": "board 1",
  "tools": [ { "number": 1, "spindle_rpm": 24000, "count": 2, "feed_mm_min": 1000, "depth_mm": null, "iterations": null } ],
  "holes": [ { "tool": 1, "x_mm": 10.0, "y_mm": 10.0 } ],
  "paths": [], "bounds_mm": [10.0, 10.0, 30.0, 20.0],
  "items": 34, "hole_ends": [14, 17, 24], "estimate_s": 28.8, "travel_mm": 10.0,
  "skipped_tools": [], "skipped": 0 }
```

`holes` are in the order the machine drills them (by tool in order of first use). `hole_ends[i]` is the item at which the hole `i` is finished (for a routing job, the path `i`), so a client that knows the item the job has reached knows how many holes or paths are done. `estimate_s` is the time of the movements without the dialogs of the operator.

| Request | Role | What it does |
|---|---|---|
| `GET /jobs?machine=&state=` | viewer | the jobs, newest first, without their view |
| `GET /jobs/:id` | viewer | one job with its view |
| `POST /jobs/:id/start` | operator | plans it again with the machine's constants of this moment and runs it; **a job never starts by itself**; one job at a time on a machine (`409 machine_busy`) |
| `POST /jobs/:id/confirm` | operator | answers the dialog the job waits at; the body is optional, `{"question"?}` (see "The question being answered" below) |
| `POST /jobs/:id/cancel` | operator | stops a running job (the machine stops, the drivers off) or cancels a ready one |
| `POST /machines/:id/stop` | operator | **the stop button**: ends the job that is active on the machine without naming it, at once. On a GRBL machine the feed hold (`!`) and the soft reset (`0x18`) are written to the port from another thread, without waiting for the line that is being sent (a homing cycle or a long move included); on a node the drivers go off. `{"ok":true,"stopped":true,"job":"j..."}`, or `stopped:false` when this server has no job on the machine (it can only stop what it is running) |
| `POST /jobs/:id/pause` | operator | holds a running G-code job: the controller holds its movement and no line goes out until `resume`; `409 not_pausable` for a drilling or routing job, `409 not_in_state` when it is not running |
| `POST /jobs/:id/resume` | operator | lets a held G-code job go on (also the wait of a job that goes on after a stop); `409 not_in_state` when it is not held; the same optional body as `confirm`. `confirm` does the same for a G-code job |
| `GET /jobs/:id/request` | operator | `{"id","machine","kind","name","state","resumed_from","next_skip","request"}`: what was asked of the job exactly as it came (the file, the tool table, the options, the program text), kept with the job and removed with it (`DELETE /jobs/:id`, or when the list of finished jobs drops it, `keep_jobs`); it cannot be bigger than a request may be (`max_body_mb`). For a cancelled or failed job `next_skip` is where to go on from (otherwise `null`) |
| `POST /jobs/:id/resume-from` | operator | makes a **new job, ready and not started**, from the saved request of a cancelled or failed job, with its `skip` brought forward to what that job had done, so the client need not ask for the file again: a drilling job leaves out the holes done (`holes_done` added to the `skip` it had), a G-code job goes on from `resume_margin_lines` lines that move before its `line` (the controller takes lines ahead of the movement), with the codes in force and the motion (`G0` to `G3`) given back (see "Stopping and going on" below), a routing job leaves out the paths it finished (the one it cut short is cut again). The sums add up over several stops. The body is optional: `{"skip"?,"skip_tools"?,"restore_outputs"?,"name"?}` (`skip` replaces what was worked out, in holes, paths or lines; `skip_tools` replaces the tools that are off). The answer is the new job like `POST /machines/:id/jobs` (with `resumed_from`); `409 not_stopped` for a job that is ready, running or done, `422 no_work` when nothing is left, `422 bad_request` (with the reason in `detail`) when the server cannot say by itself where to go on (a G-code job in relative distances `G91` at that point or among the lines sent again, with a `G92` or `G43.1` offset that the reset of the controller loses, or that would set `G92` or `G10` again from where the head stands, or when the stop of the machine was not confirmed; a drilling or routing job that ran on the tool changer, whose chuck may hold any tool) — then a person puts the machine right and names `skip` —, also `422 bad_request` for a named line of a G-code job after which the place of the tool cannot be read from the program, and the same refusals as a new job. It never moves the machine: it is started with `POST /jobs/:id/start` by a person, a drilling job asks for its tool and spindle, and a G-code job waits for the operator before its first move |
| `DELETE /jobs/:id` | operator | removes a job that is not running |

States: `ready` → `connecting` → `running` ⇄ `waiting_operator` → `done`, `cancelled` or `failed` (`message` says why). A job that was running when the server stopped is marked `failed`: nobody knows where the machine was. While it waits, `pause` is `{"kind":"tool_change","tool":2}` (put the tool in and confirm) or `{"kind":"spindle_start"}` (confirm the spindle start); for a G-code job it is `{"kind":"hold"}` (the operator paused it) or `{"kind":"resume"}` (it goes on after a stop and waits before its first move). A job also says who handled it: `by` asked for it, `started_by` started it and `stopped_by` stopped it (cancelled it or pressed the stop of its machine; `server` when the server itself ended while it ran), each empty while nobody did it; the same facts are in the audit and in `eh3d-jobs.log` (see `docs/DEPLOY.md`). A job is also given its `line`: for a G-code job the line of the program text it has got to (where a job that goes on starts from), 0 for the others. A job has `items_done` of `items_total` and `holes_done` of `holes_total` (a routing job counts paths; a G-code job has no holes, and its items are the lines sent). A G-code job that is refused by the controller (a line it does not take, an alarm, a link that fails) is marked `failed` with the line in `message`, and the server stops the controller as far as it can: nobody is at the screen to decide. A job is done when the machine is still after its last line, and fails if it is not within `gcode_finish_wait_s` seconds of `server.cfg`.

**The question being answered.** While a job waits, `pause` of the job (in the job, the lists, the fleet and the events) carries `question`, an identifier that is new every time the job stops to ask, also when it asks the same thing again. A screen sends back the one it is showing: `POST /jobs/:id/confirm` or `/resume` with `{"question": "q1a2b3c4"}`. When the job asks something else by then, or that question was already answered, nothing is answered and the reply is `409 question_changed`: read the job again, show what it asks and let the person answer that. A question is answered once. With the setting `answer_needs_question` off (the default) an answer without `question` is taken, as the clients published before send it; with it on, it is refused with `409 question_required`. The routes of the work queue (`/work/jobs/{id}/resume`) and of the compatibility layer carry no question: they only let go a job an operator held, never the wait before the first move of a job that goes on after a stop. One question needs its name whatever the setting says: the second wait of a job that goes on, before the spindle or the laser is switched on (`pause.power_on` is `true`, `kind` stays `resume`); an answer without `question` is refused there with `409 question_required`.

**Stopping and going on (G-code).** A stop of a GRBL machine writes the feed hold at once, asks the controller for its state until it says that the machine stands still (`Hold:0`, `Idle`, `Door:0` or `Door:1`) and only then writes the soft reset, which stops the spindle and the coolant. When the controller does not say so within `grbl_stop_wait_s` seconds, goes into alarm or cannot be read, the reset goes all the same and `message` of the job says that the position of the machine is not known; a homing cycle, which a feed hold does not stop, is reset at once.

A job that goes on (`skip` above 0) looks at the controller before it waits for the operator and again after the confirmation, and fails with `the job is not resumed: ...` in `message`, with nothing sent, when the controller cannot be read, is not idle, or, for a job made by `resume-from` without a `skip`, when the machine is farther than `resume_position_tolerance_um` from where the stopped job left it. After the confirmation (`POST /jobs/:id/resume` or `confirm`, once for each question) it sends, in this order: absolute millimetres and the plane and work coordinates of the program; up to `resume_safe_z_mm`, or to the height the program had when that is higher; the spindle the program had on, and `resume_spindle_wait_s` seconds; the coolant when `restore_outputs` is `true`; across to the X and Y the program had reached; down to its Z at no more than `resume_plunge_feed_mm_min`; the units, the feed and the motion of the program; and then the program from the line `skip`.

**Switching the spindle or the laser on again.** Unless the machine is said to carry a spindle (see `tool_head` below), the `M3` or `M4` of that sequence is not sent after the first confirmation: the server waits until the machine stands still, then the job waits again (`pause` with `power_on: true` and a new `question`) and the line goes out only after that question is answered by name. It looks at the controller once more after the answer. Cancelling there stops the machine with the spindle or the laser still off. On a machine with `tool_head: "laser"` the moves across and down come first, the `M3` or `M4` comes once the head is back over the point, and there is no wait for a speed; otherwise the order above is kept. A program with no spindle or laser on at that point asks only once, and the `M3` and `M4` of the program itself, from the line `skip` on, are sent as lines of the program.

The point is the one the lines before `skip` give: a line is refused (`422 bad_request`) when it cannot be read from them (`G91` in force there, `G92`, `G43.1`, a canned cycle, or an axis last moved by `G28`, `G30`, `G53`, a probe, a homing or jog line, or left behind by `G10` or another work coordinate system). The server cannot tell whether the work zero or the tool was changed: the person who confirms looks at them. A controller that restarts when its port is opened loses its position; the server then refuses to go on by itself and a person homes it, sets the zero and names the line.

## Updates

For an administrator, when `updates_dir` is set in `server.cfg` (see `docs/DEPLOY.md`). The server does not talk to GitHub: it leaves a request in the folder and the helper `deploy/eh3d-update.py` does the rest.

| Request | What it does |
|---|---|
| `GET /api/v1/updates` | `{ "enabled", "pending", "state", "message", "log", "percent", "steps": [{ "id", "state", "percent" }], "history": [{ "id", "started", "kind", "label", "state" }], "components": [{ "id", "name", "installed", "previous", "can_undo", "latest", "newer", "url", "published", "notes" }] }`; `previous` is the version that the last update of the component left behind when the helper knows it (otherwise `null`) and `can_undo` says whether that version is kept and can be put back; `state` is `idle`, `checking`, `updating`, `done` or `error`; `percent` is how much of the whole job is done, each step (`<component>.download`, `.verify`, `.install` and `.restart` for the one that restarts) is `pending`, `running`, `done` or `failed`, `history` holds the last fifteen jobs and `notes` the text of the latest release of the component. Without `updates_dir`: `{ "enabled": false, ... }`. |
| `POST /api/v1/updates/check` | Asks the helper to look at the latest releases on GitHub. 501 `updates_off` without `updates_dir`; 409 `update_refused` when a request waits or the helper is working. |
| `POST /api/v1/updates/apply` | Body `{ "components": ["server", "studio"] }`: asks the helper to update those components (the ones that are not newer are not touched by the Studio, which sends only the newer). 409 for a component that cannot be updated or for a request that is already waiting. |
| `POST /api/v1/updates/undo` | Body `{ "components": ["studio"] }`: asks the helper to put back the version that the last update of those components left behind (the one now installed becomes the one left behind, so undoing twice goes forward again). Only components with `can_undo` true; 409 `update_refused` otherwise or when a request waits, and 409 `machines_busy` while a job is active (the server restarts). |
| `POST /api/v1/updates/clear-history` | Asks the helper to empty the list of the jobs it did before (`history`). 409 `update_refused` when a request waits or the helper is working. |

The components are `server` (EH3D-SERVER, installed as `/opt/eh3d-server`) and `studio` (EH3D-STUDIO, the folder of its pages). The helper checks the SHA-256 of each file against the one the release publishes, and keeps the old folder as `.previous` (and says in `previous.json` whether it is there and which version it holds).

## The node of the drill head

For a machine whose configuration names the node of its drill head (`head_node`, `head_address` and `head_token`, the keys of the programs' machine file). The server asks the node for the viewer or the operator, with the token of the machine, so the node's token never leaves the server.

| Request | Role | What it does |
|---|---|---|
| `GET /api/v1/machines/:id/head/status` | viewer | the status of the node as it answers it (`node`, `camera`, `camera_model`, `laser`, `rpm`, `temp_c`, `heat`, `protect_open`, `uptime_s`) |
| `POST /api/v1/machines/:id/head/measure[?samples=N]` | operator | one measure of the table under the head (`samples` from 1 to 31); the node's own answer and status code, so a refused measure says why |
| `GET /api/v1/machines/:id/head/stream[?ticket=T]` | viewer | the camera of the node as MJPEG, handed on while it comes; a browser cannot set a header on a picture, so the address may carry a **ticket** (see `POST /tickets`), never the token of the session unless `url_token = true` is set in `server.cfg` |
| `POST /api/v1/tickets` | viewer | `{"ticket","expires_s"}`: a ticket for the camera or the websocket, good for **one use within 30 seconds** and made for the session that asked; the address carries the ticket, so the token of the session stays out of histories, proxy logs and `Referer` |
| `GET /api/v1/machines/:id/head/light` | viewer | the ring of LEDs around the camera as the node says it (`on`, `level`, `max_level`, `tone`, `sectors`, `leds`, `auto_off_s`, `color`, `cue`) |
| `POST /api/v1/machines/:id/head/light?on=1&level=60&tone=white&sectors=all` | operator | turns the ring on or off and changes how it shines: `on` (`0` or `1`), `level` (0 to 100, kept under the ceiling of the node), `tone` (`white`, `cool`, `warm`, `red`, `green`, `blue`), `color` (six hexadecimal digits), `sectors` (`all`, `front`, `right`, `back`, `left`, `front_back`, `left_right` or `0x` and two digits) and `cue` (`ok`, `nok`, `busy`, `warn`, `alarm`, `none`) with `cue_ms`; the node's own answer and status code |
| `GET /api/v1/machines/:id/head/snapshot` | viewer | one picture of the camera as JPEG |
| `POST /api/v1/machines/:id/head/inspect` | operator | the quality check of the work: with `{"holes": [{"x_mm", "y_mm", "dia_mm"}], "light": 60, "auto_light": true, "cue": true, "restore_light": false, "profile": {...}}` the server lights the ring (when `light` is given), takes a picture, judges it with the profile of the machine (the holes of the job that the camera sees, from the centre of the picture, x to the right and y up, in mm; the milled channels; the picture itself) and makes the ring say how it went. The answer is the report: `verdict` (`ok`, `not_ok` or `unknown`), `quality`, `holes`, `milling` and `findings` (a `code`, the `hole` it is about and whether it `fails`) |
| `GET /api/v1/machines/:id/head/inspect` | viewer | the last check of the machine (`404 no_inspection` until one is made after the server starts) |
| `GET /api/v1/machines/:id/head/inspect/profile` | viewer | the profile of the analyser of the machine: the scale of the picture (`px_per_mm`), which checks are on and every tolerance |
| `PUT /api/v1/machines/:id/head/inspect/profile` | operator | changes the profile: `{"preset": "standard" \| "fine_pitch" \| "prototype"}` starts from a preset (the scale is kept) and any member of the profile changes what it names; what cannot be used is listed in `rejected` and changes nothing |
| `POST /api/v1/machines/:id/head/inspect/calibrate` | operator | sets `px_per_mm` from the first hole of the last check that was found (how many pixels it covered against its width in the job); `409 no_hole` when none was found |

Refusals: `422 bad_light` (the query for the ring has signs the node does not use), `422 too_many_holes` (more than 500), `422 bad_profile`, `502 bad_picture` (the picture of the node could not be read), `404 no_head` (the machine has no `head_address`), `422 bad_address`, `422 bad_samples`, `502 head_unreachable` (the node does not answer in four seconds) and `502 head_refused` (the node answered with a status other than 200).

## The nodes of the pick-and-place machines

A pick-and-place machine can stand on two kinds of node: a **feeder** node (the chain of tape feeders) and a **head** node (the pump, the valves and the axes of the nozzles). Their words are in the `docs/CONTRACT.md` of EH3D-NODE-PNP-FEEDER and EH3D-NODE-PNP-HEAD: HTTP and JSON, a token as `Authorization: Bearer`, `GET /api/status` answered with or without it. The server keeps a list of them, asks each one on a timer and shows what it said, so a client never reaches a node by itself. It is off by default (`pnp_nodes_enabled`): with it off every request below answers `501 pnp_nodes_off`, no connection to a node is opened and `GET /fleet` is as it was. Nothing here has run on a real feeder or head: the tests ask nodes in memory, and every node says `evidence: simulator`.

| Request | Who | What it does |
|---|---|---|
| `GET /pnp/nodes` | viewer, `machines:view` | `nodes` (each as below), a `summary` (`registered`, `ready`, `not_ready`, `by_health`) and the `limits` in force; it opens no connection |
| `POST /pnp/nodes` | admin, `machines:manage` | registers `{"kind": "feeder" or "head", "name", "address": "host:port", "token", "machine", "id", "enabled"}`; the id is `<kind>-<n>` when it is not given; `422 bad_pnp_node`, `409 pnp_node_exists` (the same id or address), `409 pnp_nodes_full`; audited |
| `GET /pnp/nodes/:id` | viewer, `machines:view` | one node; `404 unknown_pnp_node` |
| `PUT /pnp/nodes/:id` | admin, `machines:manage` | changes `name`, `address`, `token` (`********` keeps it), `machine` or `enabled`; a node whose address, token or switch changes starts again from `unknown`; audited |
| `DELETE /pnp/nodes/:id` | admin, `machines:manage` | removes it; audited |
| `POST /pnp/nodes/:id/poll` | operator, `jobs:prepare` | asks the node now (the status, then a read that needs the token) and gives the node as the server sees it; `409 pnp_node_disabled`; audited |

A node is `id`, `kind`, `name`, `address`, `token` (`********` or empty; the token is never shown, logged or audited), `machine` (a machine of the plugin `pnp`, or empty), `enabled`, `health`, `ready`, `silent_for_s`, `polls` (`ok` and `failed`), `last_error`, `status` (what the node said last, only the keys of its contract with their sizes bound: for a feeder `safe`, `selfcheck`, `slots`, `online`, `bus`, the `feeders` and the counts `low`, `empty` and `faulted`; for a head `safe`, `selfcheck`, `nozzle_count`, `pump`, `xy_clear`, `xy_blocking`, `z_safe_mm`, the `nozzles` and `faulted`), `evidence` and `simulator_only`.

**Health, and what is safe.** The server degrades towards what is safer: only `ok` and `warn` (the node's own verdict) are *ready*. `unknown` (never asked), `disabled`, `unreachable` (the last poll failed: no answer, an HTTP status other than 200, an answer that is not the contract or too big), `refused` (the node answered but refuses the token, so it will go to its own safe state), `silent` (nothing good for `pnp_node_silence_s`, never less than two polls, whatever it said before), `safe` (the node says it is safe, or does not say), `fail` (its own check) are not ready. A machine of the plugin `pnp` that has a node that is switched on and not ready does not start a job: `409 pnp_node_not_ready` with the node and its health; jobs that were prepared wait. A person acknowledges the safe state on the node itself; the server only sees it come back. The detail of such a machine lists its `nodes`, and while the switch is on `GET /fleet` has `pnp_nodes` with the same counts. The websocket sends `{"type":"pnp_node","id":"...","health":"...","ready":true}` when a node changes its health (not at every poll).

**Staying heard.** A head node goes to its safe state when no request with its token reaches it for `link_lost_ms` (2000 by default), also while it stands still; a feeder node after `server_lost_ms` (15000 by default). So each node is asked every `pnp_node_poll_s` seconds (1 by default), each question on a connection of its own that does not wait for the answers of the others: a node in the middle of a long move or slow to answer, or another node that is gone, does not make the server quiet (up to three questions to a node wait at once; the answer to an earlier one that comes late is ignored). The server sends these nodes no order of movement. Each node says `link_lost_ms`, `link_lost_declared` and `poll_too_slow`: the time is the one the node gives in its status (`link_lost_ms` for a head, `server_lost_ms` for a feeder) when it gives one, and the default of its contract otherwise. When the interval is that time or more, the list has a `warnings` entry (`{"code": "poll_too_slow", "node", "poll_s", "link_lost_ms", "link_lost_declared"}`) and the console of the server says it, when it starts and when a node first says its time. A head set to less than 1000 ms cannot be kept heard by this setting.

**Settings** (`server.cfg`, read when the server starts): `pnp_nodes_enabled`, `pnp_nodes_max` (1 to 200), `pnp_node_poll_s` (1 to 3600, 1 by default; below the time after which a node goes safe for not hearing the server), `pnp_node_silence_s` (2 to 3600) and `pnp_node_timeout_s` (1 to 30). All five are in `docs/settings-contract.json` with their texts in the eight languages.

## The printers the server watches

**Read only.** The server reads the state of 3D printers and shows it; it never sends a printer an order, a temperature, G-code or a file, and there is no route that could. The list is off until `printers_enabled = true` (`501 printers_off` before), and holds up to `printers_max` printers of two families of protocol, each simulated (`link: "sim"`, in memory, no connection) or on the local network (`link: "lan"`):

| Family | How it is read |
|---|---|
| `bambu_lan` | MQTT over TLS to the printer (port `bambu_lan_port`), only while `bambu_lan_enabled` is on too. The certificate is pinned by its SHA-256 (`fingerprint`): with none, or another, nothing is sent and `certificate_seen` says what the printer showed, to compare it with the printer and pin it. The only message published is the request for the whole state, at most every five minutes while the printer talks. |
| `moonraker` | Klipper through Moonraker, plain HTTP (port 7125 unless the address names one): only `GET /printer/objects/query` and `GET /server/webcams/list`. |

| Method and path | Role | What |
|---|---|---|
| `GET /printers` | viewer | the contract `eh3d.printers.status/1`: the printers, how many are in each health and the limits |
| `GET /printers/:id` | viewer | one printer |
| `POST /printers` | admin | register a printer (`name`, `family`, `link`, `address`, `serial`, `fingerprint`, `secret`) |
| `PUT /printers/:id` | admin | change what the server keeps of it |
| `DELETE /printers/:id` | admin | remove it from the list |

The access code or key (`secret`) is kept apart from the list, in `printer-secrets.json` of the data folder, is shown as `***` and is never logged or audited; sent back as `***` it is kept. The serial is shown shortened. A body that does not read is refused without quoting it. A printer has a `health`: `online`, `stale` (what it said is older than `printer_stale_s`), `offline`, `refused` (the access code), `untrusted` (the certificate), `bad_report`, `off` (`bambu_lan_enabled`), `disabled` or `unknown`; only `online` and `stale` carry a `status`, and `current` is true only for `online`. In the `status`, what the printer did not say, or a number that cannot be right, is `null`, never zero: `state`, `temperatures` (`nozzle`, `bed`, `chamber`), `progress_pct`, `remaining_s` (`remaining_estimated` says the server worked it out, as for Klipper), `layer` and `layers`, `job_name`, `alerts` (the health codes of a Bambu printer, a failed print, a stopped firmware) and `camera.present` (never an address or a picture). A change of health, state or alerts is told on the websocket as `{"type":"printer","id","health","state","alerts"}`. Everything here has been tried against printers in memory only (`evidence` is `simulator` or `unvalidated`): nothing has run against a real printer.

## The spindle of the machine

A machine carries one of two spindles, `original` (the one it leaves the factory with) or `vfd` (with a variable-frequency drive that the node of the machine drives), or none is said (empty: no job is judged against a spindle). The limits of each are the starting ones (`original`: 5000 to 63000 turns, 3000 mm/min along the cut, 4000 mm/min of plunge, bits up to 3.2 mm; `vfd`: 6000 to 24000 turns, 1500 and 1200 mm/min, bits up to 6 mm) with what the machine changed on top. They are also members of the machine (`spindle`, `spindle_guard`, `spindle_original`, `spindle_vfd`); a `PUT /machines/:id` that does not send them leaves them as they were.

**What the machine carries for G-code jobs.** The machine member `tool_head` is `"laser"`, `"spindle"` or empty (the default: not said); an update that does not send it leaves it as it was, and any other word is refused with `bad_spindle`. It decides the second question of a job that goes on after a stop: with `laser` it is always asked; with `spindle` never; when it is empty it is not asked only if `spindle` (`original` or `vfd`) names the spindle the machine carries. The default asks, because a machine that is not described may carry a laser.

**The `$` lines of GRBL.** The machine member `allow_dollar` (`false` by default; any role reads it, only an administrator changes it, with `PUT /machines/:id`, and an update that does not send it leaves it as it was) lets the free G-code jobs of that machine carry the `$` lines of GRBL: `$H` (homing), `$X` (unlock), `$G`, `$#`, `$$`, `$I`, `$C`, `$J=...` (jog) and the settings (`$100=250`). It is a safety switch: such a line can move the machine to its limit switches or change the settings of the controller, so it is turned on per machine and only where the operator is at the machine. Even then the server refuses the line `$RST` (it wipes the memory of the controller), the real-time characters (`!`, `~`, `?`), control characters and every `$` line with signs other than letters, digits, spaces and `=.-+#` (a second `$` only in `$$`), with `422 bad_gcode`. Blanks do not hide a command (`$ RST=$` is `$RST=$`), and the words of a jog are read like those of any line, its feed judged against the spindle of the machine. A ready job is judged again when it starts, so a job made while the switch was on is refused (`bad_gcode`) if the switch has been turned off since. The simulator takes the safe ones (`$H` as a homing cycle that leaves the machine at its home, `$X`, `$G`, `$#`, `$$`, `$I`, `$C`, `$J=...` and the numbered settings) and refuses `$RST`, `$N0=...`, `$SLP` and the rest, failing the job at that line and saying which. On a GRBL machine the waits are set in `server.cfg`: an ordinary line `grbl_line_timeout_s` (5 s), the homing `grbl_homing_timeout_s` (120 s) and a jog `grbl_jog_timeout_s` (60 s); a line that does not end in time holds the machine (a homing cycle is reset, a jog is cancelled) and the job is `failed`, with the line in `message`.

| Request | Role | What it does |
|---|---|---|
| `GET /api/v1/machines/:id/spindle` | viewer | `{"spindle","guard","limits":{"original":{...},"vfd":{...}},"active","node_max_rpm"}`: the spindle, the switch (`reject` or `warn`), the limits of both (`min_rpm`, `max_rpm`, `power_w`, `max_xy_mm_min`, `max_z_mm_min`, `max_bit_mm`), those of the spindle in use and its top speed, which is what `spindle.max_rpm` of the node of the machine should be (`null` with no spindle) |
| `PUT /api/v1/machines/:id/spindle` | admin | `{"spindle"?,"guard"?,"original"?,"vfd"?}`: sets what is sent; `original` and `vfd` take the limits that differ from the starting ones (`{}` goes back to them). The answer is the same as the `GET`. `422 bad_spindle` for a spindle that is not one of the two, a switch that is not `reject` or `warn`, or limits that cannot be used (the slowest above the fastest, a zero feed); nothing changes then |

With a spindle chosen, the planning (the preview, the creation of a job and its start) judges each tool the job uses: the turns above the top of the spindle (`rpm_above_max`) or, when not zero, below the slowest (`rpm_below_min`), the feed along the cut of a routing job (`feed_xy_above_max`) and the plunge of a routing job or the feed of a drilling job (`feed_z_above_max`). With the switch on `reject` (the default) a job with any excess is refused with `422 spindle_limit`, the detail saying which tool and by how much; with `warn` it goes through, the turns are cut to the top of the spindle (that is what the node is sent) and the view lists the excesses in `spindle_warnings` (`code`, `tool`, `value`, `limit`).

## Tasks

A **task** is work for the computer: checking a design, making the files for a factory, running a simulation. It has **no effect on any machine**, ever, and it may start by itself according to the queue and the limits; a **job** (above) moves a machine and never starts by itself. The two families do not meet: a finished task changes no job, and the code of the tasks does not name machines, connections or jobs (a test reads the source and fails if it does). Running the engines in a process of their own, without even the memory of the connections, is the next layer and is not done yet. All of this is under `/api/v1/tasks` and can be switched off with `tasks_enabled = false` (every request answers `501 tasks_off`).

| Request | Role | What it does |
|---|---|---|
| `GET /tasks/kinds` | viewer | `{"kinds":[{"kind","available","version"}]}`: the kinds of the design suite (`phoenix.import`, `phoenix.check`, `phoenix.export.fab`, `phoenix.route.auto`, `phoenix.library.sanitize`, `phoenix.verify`, `phoenix.passport.build`, `phoenix.sim.spice`, `phoenix.sim.thermal`, `phoenix.sim.rf`, `phoenix.ai.assist`) with whether this build has an engine for each. No engine of the suite is in the server yet, so none is available; with `tasks_selftest = true` the engine `server.selftest` is (it counts steps, can be asked to fail and gives the SHA-256 of a text: it exists to exercise the task system) |
| `POST /tasks` | operator | `{"kind","name"?,"params"?,"project"?,"idempotency_key"?}` gives `201` and the task. `name` is the kind when it is left out (1 to 80 characters, no control characters, else `422 bad_name`). `params` is an object (`422 bad_params` otherwise or when the engine does not accept it; `422 too_big` over `task_params_kb`). `project` is the id of the artifact the task works on; it has to be an artifact the caller can see, else `422 no_project` |
| `GET /tasks?state=&kind=` | viewer | the tasks the caller may see, newest first |
| `GET /tasks/:id` | viewer | one task, with its result |
| `POST /tasks/:id/cancel` | operator (owner or admin) | `{"accepted","task"}`. A queued task is cancelled at once; a running one is told to stop and ends `cancelled` with no result (what it was writing is discarded); a task that had already finished answers `accepted: false` and is not changed |
| `DELETE /tasks/:id` | operator (owner or admin) | removes a finished task; `409 task_active` while it waits or runs |

A task: `{"id":"t-<uuid v7>","kind","name","owner","state","created","started","finished","progress":{"done","total","unit"},"message","result","engine":{"version"}}`. The states are `queued`, `running`, `done`, `failed` and `cancelled`. The `result` of a finished task is `{"artifacts":[{"artifact_id","name","kind","result_type"}],"diagnostics":[...],"data":{}}`; a diagnostic is the common form of the suite (`code`, `severity`, `message`, and the optional `line`, `location`, `category`, `recoverability`, `args`). The codes of the task system itself are `EH3D-TASK-001` (a task asked to fail, the self-test), `EH3D-TASK-002` (longer than `task_timeout_s`), `EH3D-TASK-004` (the server restarted while it ran) and `EH3D-TASK-005` (the engine failed unexpectedly), `EH3D-TASK-006` (the artifact the task works on could not be read) and `EH3D-TASK-007` (the files it made could not be stored).

**Who sees what.** Every task has an owner: the user who made it, or the program token (`token:<id>`: two tokens with the same name are two owners). The owner and the administrators see it; for anybody else it does not exist: reading, cancelling or deleting it answers `404 unknown_task`, the same as for an id that was never made, and it is not in their list.

**The queue.** At most `max_concurrent_tasks` run at the same time, the rest wait in the order they came. The limits refuse before the task exists: `429 queue_full` (`max_tasks` waiting or running, all users), `429 quota` (`tasks_per_user` of one user), `422 too_big` (`task_params_kb`), `422 unknown_kind`, `422 engine_missing`. A task that runs longer than `task_timeout_s` is stopped and fails with `EH3D-TASK-002`. After a restart the queued tasks stay queued and go on, and the task that was running is `failed` with the message `interrupted by restart`; it may be sent again. The newest `keep_tasks` finished tasks are kept.

**Sending the same request twice.** With an `idempotency_key` (1 to 128 printable characters), a second request of the same caller with the same key and the same body (the parameters in any order) answers `201` with the first task, whether it is still there waiting or already done; the same key with another body answers `409 idempotency_conflict`. The key is remembered for `idempotency_ttl_s` (a day) and is the key of whoever sent it: another caller may use the same text for something else.

**Events.** The websocket sends `{"type":"task","task":{...}}` when a task is made, starts, moves on or ends, and `{"type":"task_removed","id":"..."}` when one is removed, **only to the clients that may see the task** (its owner and the administrators), the removal too.

## Artifacts, blobs and uploads

The server keeps files for the design programs and for the tasks. There are two identities and they are never confused: the **`blob_sha256`** is the identity of the *bytes* (the key under which they are stored once, however many artifacts point to them) and the **`artifact_id`** (`a-<uuid v7>`) is the identity of the *record*: who owns it, what made it, from which inputs and with which parameters. Two artifacts may share a blob and differ in provenance. A task refers to its project by `artifact_id`, never by a hash.

| Request | Role | What it does |
|---|---|---|
| `POST /artifacts` | operator | `{"name","format","data_base64", "schema_version"?,"origin"?,"producer"?,"inputs"?,"params"?,"result_type"?}` gives `201 {"artifact_id","blob_sha256","size"}`. `name` is 1 to 120 characters without control characters or slashes; `format` is a media type in lower case (`application/x-phoenixproj`); `result_type` is `T1` to `T7`; `inputs` are `{"artifact_id","blob_sha256"}` of artifacts the caller can see, with the hash they carry; `params` is an object of at most 64 KB. The bytes are limited by `max_body_mb` (the request) and `max_upload_mb`; larger files go by an upload. `422 bad_artifact` for a description that does not fit, `422 too_big`, `507 store_full` |
| `GET /artifacts?format=&result_type=` | viewer | the records the caller can see, newest first |
| `GET /artifacts/:id/meta` | viewer | the record: `artifact_id`, `blob_sha256`, `size`, `owner`, `name`, `format`, `schema_version`, `origin` (`project`, `revision`), `producer` (`tool`, `version`, `config_sha256`), `inputs`, `params`, `result_type`, `diagnostics`, `validation`, `audit.created_at`, `retention`, `keep_until` |
| `GET /artifacts/:id` | viewer | the bytes, streamed (`x-eh3d-blob-sha256` and `x-eh3d-format` say the hash and the format) |
| `DELETE /artifacts/:id` | admin | removes the record, and the blob when no record points to it any more; `409 artifact_in_use` while a task works on it or made it |
| `PUT /artifacts/:id/retention` | admin | `{"retention":"keep_while_referenced"}`, `{"retention":"pinned"}` or `{"retention":"keep_until","until":<seconds since 1970>}` |
| `HEAD /blobs/:sha256` | viewer | `200` only when an artifact the caller can see points to bytes with this hash; otherwise `404`, the same as for a hash nobody holds |
| `POST /artifacts/verify` | admin | reads every blob again against its hash: `{"ok","problems":[{"blob_sha256","problem":"missing"\|"damaged"}]}` (after restoring a backup) |

`validation` keeps the states apart and is never one flag: `intact` (the bytes are the ones whose hash the record carries; true), `readable`, `coherent`, `erc_drc_run`, `oracle_checked` and `physically_validated`, each `null` until something has established it. `audit.created_at` is metadata and is never part of the content.

**Privacy.** An artifact is seen by its owner and the administrators; for anybody else it does not exist (`404 unknown_artifact`). Whether a blob exists is not a question anybody may ask about other people's files: `HEAD /blobs/:sha256` answers about the bytes the caller can see, and the deduplication is invisible from outside (a caller that cannot see the blob sends the bytes, gets an artifact of its own, and the server keeps one copy). The audit and the logs say ids and hashes, never names, parameters or contents.

**Uploads in parts.**

| Request | Role | What it does |
|---|---|---|
| `POST /uploads` | operator | `{"size","sha256"}` gives `201 {"id":"up-<uuid v7>"}`. `422 too_big` over `max_upload_mb`, `422 bad_upload` for a hash that is not 64 lower-case hexadecimal digits, `429 quota` over `uploads_per_user`, `507 store_full` |
| `PUT /uploads/:id?offset=N` | operator (the owner) | the raw bytes of a part, at any place and in any order: `{"id","size","received","ranges"}`. A stretch that was received already must carry the same bytes (it is accepted again); other bytes at the same place give `409 upload_conflict` and nothing is written; a part past the declared size or an empty one gives `422 bad_upload` |
| `POST /uploads/:id/complete` | operator (the owner) | the description of the artifact (as in `POST /artifacts`, without the bytes) gives `201` like `POST /artifacts`. `409 upload_incomplete` while a stretch is missing; a hash that is not the declared one gives `422 hash_mismatch` and the upload is discarded. A description that does not fit leaves the upload as it was |
| `DELETE /uploads/:id` | operator (the owner) | throws it away |

An upload nobody touched for `upload_ttl_s` is cleaned, and so are the files of an upload that is no longer known. An upload of somebody else does not exist for you (`404 unknown_upload`). A partial artifact is never visible as complete: the artifact exists only after `complete`.

**Retention.** Everything a task works on or made is kept while the task is there. An artifact nothing points to (no task, no other artifact that lists it as an input) is purged `artifact_unreferenced_days` days after it was made (0: never); `pinned` is never purged and `keep_until` goes when its date has passed and nothing points to it. The purge and the cleaning of old uploads run when the server starts and whenever an upload begins. The store counts each blob once and refuses what would pass `artifact_store_gb`.

**What a task makes.** The files an engine makes become artifacts of the owner of the task, with `producer` (the kind and the version of the engine and the SHA-256 of the parameters), `origin.project`, `inputs` (the project, by id and hash) and `params`; the `result.artifacts` of the task lists them as `{"artifact_id","name","kind","result_type"}`. A task cancelled while its files are stored leaves none of them. A task that names a `project` gets the bytes of that artifact, read and checked against their hash; `422 no_project` when it does not exist or cannot be seen by the caller.

## Projects, sharing and the scopes of tokens

**Projects.** A project is a name, an owner and a line of revisions; each revision points to an artifact (the file of the design at that moment, which the server keeps as it is and never needs to understand). A program never needs the server to open, edit and save a local project.

| Request | Role | What it does |
|---|---|---|
| `GET /projects` | viewer | the projects the caller sees (its own, those shared with it, and for an administrator all), newest first |
| `POST /projects` | operator | `{"name"}` gives `201` and the project; the caller is the owner. `422 bad_name` for a name that is empty, over 80 characters or has control characters |
| `GET /projects/:id` | viewer | `{"id","name","owner","created","head","revisions":[{"number","artifact_id","by","at"}],"deleted"}`; the list of `shares` only for the owner and the administrators |
| `POST /projects/:id/revisions` | operator (write access) | `{"artifact_id","parent"}` gives `201` and the revision. `parent` is the revision this one follows (0 for the first); it is accepted only when that is still the last one, else `409 stale_parent`: of two saves from the same parent one wins and the other is told. The artifact has to be one the caller sees (`404 unknown_artifact`) |
| `DELETE /projects/:id` | operator (owner or admin) | hides the project; its revisions stay until an administrator purges it |
| `POST /projects/:id/purge` | admin | removes a hidden project for good (audited); `404 unknown_project` for one that is not hidden |
| `POST /projects/:id/shares` | operator (owner or admin) | `{"user":"name"}` or `{"token":"id"}`, and `"access":"read"` or `"write"`: shares the project with that person only, or changes the access given. `404 unknown_user` or `unknown_token`, `422 bad_share` (neither or both given, the owner, or an access that does not exist), `501 shares_off` |
| `DELETE /projects/:id/shares/:who` | operator (owner or admin) | takes the sharing back (`:who` is the name of the user, or `token:<id>`) |
| `POST /artifacts/:id/shares`, `GET /artifacts/:id/shares`, `DELETE /artifacts/:id/shares/:who` | operator (owner or admin) | the same for one artifact, for reading only (`"access":"read"`) |

**Who sees what.** Every project, artifact, task and upload has an owner: the user who made it, or `token:<id>` for a program's token. The server files them under an identifier of the user that is never given again, and shows the name: a new user with the name of a deleted one is another person, who sees and has nothing of what the deleted one made or was shared; that stays for the administrators. The owner and the administrators see it. Anybody else sees it only through an explicit share, one person at a time: a project shared with `read` lets that person see the project and the artifacts of its revisions (and the artifacts made for it); with `write` it also lets them add revisions. Nothing is shared by default. A person it was shared with cannot share it on, look at who else has it, delete it or purge it (`403 forbidden`: they see it, so they are told the truth). What is not visible is as if it did not exist: `404 unknown_project`, `unknown_artifact`, `unknown_task` or `unknown_upload`, the same as for an id that was never made; and `HEAD /blobs/:sha256` answers yes only about bytes that an artifact the caller sees points to. With `shares_enabled = false` nothing can be shared and only the owner and the administrators see anything. The tasks stay with whoever made them even when the project is shared.

**Events.** The websocket sends `{"type":"project","project":{...}}` (when a project is made or its sharing changes), `{"type":"revision","project","revision"}` and `{"type":"project_removed","id"}` only to the owner, the people it is shared with and the administrators; the person whose share was taken back gets `project_removed`. Events of tasks go only to the owner of the task and the administrators.

**Tokens of programs: template, scopes, rotation.** `POST /tokens` also takes `"template"` (`phoenix`, `cam`, `client` or `custom`; `422 bad_template`) and `"scopes"` (a list; `422 bad_scope` for one the server does not know). A token made without scopes keeps exactly what its role allows today. With scopes, what it may do is the **intersection** of its role and its scopes, on the old requests too:

| Scope | Allows |
|---|---|
| `projects:read`, `projects:write` | read the projects; make, change, share and delete them |
| `artifacts:read`, `artifacts:write` | read artifacts and ask about blobs; make, upload, share and delete them |
| `catalog:read`, `catalog:write` | read the shared catalogs; replace or empty one of them (`docs/CATALOGS.md`) |
| `tasks:read`, `tasks:run` | list and read tasks; make, cancel and delete them |
| `machines:view` | read the machines, the fleet and the jobs, and what the head of a machine says (status, light, camera, last check, profile of the analyser) |
| `jobs:prepare` | the preview, the creation of a job and the resuming from a stopped one, and the actions on the head of a machine (measure, light, check, calibration, profile) |
| `jobs:authorize` | authorising the hash of a prepared job of the generic queue (`POST /work/jobs/:id/authorize`); never given to a token of the `phoenix` or `cam` template |
| `jobs:start` | start, hold and resume a job |
| `jobs:confirm` | answer the dialog a running job waits at |
| `jobs:cancel` | cancel a job, remove a finished one and stop a machine |
| `machines:manage` | make, change and remove the machines (and the spindle of one) and the machines of the plugins (`docs/MACHINE_PLUGINS.md`) |
| `admin:users` | list, make, change and delete the users, and list every open login |
| `admin:tokens` | list, make, rotate and revoke the tokens, and read the keys |
| `admin:policy` | read and change the switches of security, and read the audit |
| `admin:updates` | look at, check, apply and undo the updates of the services |

A scope the token lacks answers `403 forbidden_scope`; a role that is too low still answers `403 forbidden`, whatever the scopes. The `phoenix` and `cam` templates are for the design programs and **can never hold** `jobs:authorize`, `jobs:start`, `jobs:confirm`, `jobs:cancel`, `machines:manage`, `admin:users`, `admin:tokens`, `admin:policy` or `admin:updates`: asking for one is refused (`403 forbidden_scope`) and nothing is made, and a token of those templates made without scopes gets all the others. Even a file of tokens edited by hand cannot give them the scopes of the machines: they are taken away when the token is recognised. Only a person logged in (or a `client` or `custom` token) carries them. `GET /tokens` lists `template` and `scopes`. `POST /tokens/:id/rotate` (admin) gives the token a new secret, shown this once, and the old one stops working at once; the id, the scopes and the expiry stay. A token that is itself limited to some scopes may only rotate a token limited to some of the same ones (`403 forbidden_scope`). A revoked (`DELETE /tokens/:id`) or expired token is answered `401 unauthorized`.

**Administration needs its own scope.** The requests of administration (users, tokens, security, updates) answer `403 forbidden_scope` to a token that has scopes and lacks the one of the request, even if the token is an administrator's; a token without scopes still does whatever its role allows. To migrate a program that had an administrator token with scopes (for instance `catalog:write`) and really has to administer, list the tokens (`GET /tokens`), make a new one with the scope it needs (`POST /tokens` with `"scopes": ["admin:users"]`, for example) and revoke the old one; the design programs never need them and cannot hold them. A token that has scopes can make a token only with some of its own scopes (`POST /tokens` without `scopes`, or with one the caller lacks, answers `403 forbidden_scope`), so a limited token cannot make a looser one to get out of its limits.

**A token never gives more than it has.** All of this is `403 forbidden_scope`. A token that has scopes:

- never makes an administrator, and never changes (password, role, enabled or disabled) or deletes one;
- makes a user of another role only if it carries every scope that role can use. For a `viewer`: `projects:read`, `catalog:read`, `artifacts:read`, `tasks:read` and `machines:view`. For an `operator`: those and `projects:write`, `artifacts:write`, `tasks:run`, `jobs:prepare`, `jobs:authorize`, `jobs:start`, `jobs:confirm` and `jobs:cancel`. The same holds to set a password, to enable a user again or to raise a role. Lowering a role, disabling or deleting a user who is not an administrator only needs `admin:users`;
- makes the key of a 3D printer (`POST /work/machines/{id}/compat/key`) only with `machines:view`, and a key that writes only with `jobs:prepare`, `jobs:start` and `jobs:cancel` too.

**What a token that expires makes ends with it.** A token made by a token that has an expiry expires at the latest when its maker does, whatever `days` says (`GET /tokens` shows the date). Such a token rotates only tokens that expire no later than itself, and makes no key of a 3D printer (those keys do not expire). A person logged in, and a token without expiry, are not limited this way.

**A token that expires opens no account.** Users and passwords stay after the token has ended, so a token that has an expiry, whatever its role and scopes, answers `403 forbidden_scope` to `POST /users` and to a `PUT /users/{name}` that sets a password, enables a user (`"disabled": false`) or raises a role; nothing of that request is changed. It can still do what only takes away (lower a role, disable, delete), within its scopes. To provision users from a program, give it a token without expiry (`POST /tokens` without `days`, made by a login, while `token_default_days` is 0), or do it from a login of a person. With `token_default_days` above 0 every token made without `days` expires and none can be made without expiry, so users are made and passwords set from a login.

**The generic work of the machines** (plugins, a queue of jobs with history for any kind of machine, the stream of events and the notifications) is under `/api/v1/work`, `/api/v1/plugins`, `/api/v1/events/stream` and `/api/v1/notifications`; see `docs/MACHINE_PLUGINS.md`. The websocket below is not changed.

**The shared catalogs** (materials, tools, machine profiles, footprints, symbols, components) are read by every application under `/api/v1/catalogs` and written by an administrator; see `docs/CATALOGS.md`.

**The design run, the audit, the keys and the quota.** `POST /apps/pcb/jobs` runs the installed design program on a board of a project and keeps the report as a file of the project (`docs/DESIGN_RUN.md`); `GET /audit` reads the audit (administrator); `GET /auth/keys` lists the tokens by identifier and age and the fingerprint of the certificate of the proxy (administrator); `PUT /auth/policy` changes one switch of security while the server runs (administrator, `docs/AUTH.md`); `GET /settings/contract` gives the contract of every setting of `server.cfg` with its texts in eight languages and the values in force (administrator, `docs/CLIENT_SYNC.md`); `GET /settings/values` gives the values in force, their origin and whether the security page changes them, light (a few KB), read only and for any login, with an `ETag` and no secret, text or path (`docs/CLIENT_SYNC.md`); `GET /apps/{app}/quota` tells the limits of the store and what the application uses (`docs/PROJECT_STORE.md`). The exact shapes are in `docs/openapi.yaml`.

**The single sign-in of every application** (sessions that name the application they were opened for, the list of open sessions, the keys and the policy of the server) is described in `docs/AUTH.md`.

**The project store of every application** is a second way to keep a project: files with versions, `If-Match`, editing locks and the `.eh3dproj` container, under `/api/v1/apps/{app}/projects`. It does not change anything above; it is described in `docs/PROJECT_STORE.md` and in the `store` tag of the contract.

## What happens, as it happens

`GET /events?ticket=<ticket>` upgrades to a websocket (a browser cannot set headers on one, so the address carries a ticket from `POST /tickets`; `Authorization: Bearer` works too, and `?token=` only with `url_token = true`). The server asks every 30 seconds whether the session is still good and closes the websocket when it is not. It also sends a ping every `events_ping_s` seconds (30) and closes a connection that has sent nothing at all, a pong included, for `events_idle_s` seconds (90), so a phone that left the Wi-Fi does not hold a place for ever. A client that read more slowly than the events came (the channel holds 256) is told `{"type":"resync","missed":N}`: it asks for the fleet and the jobs again, because it lost some. The first text is `{"type":"hello","jobs":[...]}` with the summaries of the jobs; after it every change arrives as `{"type":"job","job":{...}}`, `{"type":"job_removed","id":"..."}` or `{"type":"machine","id":"..."}`.

## Error codes

Every refusal carries one of these codes (the same list is in `error-codes.json`, which the clients read in their tests). The status is the usual one; a few codes come with another where the situation differs.

| Code | Status | Meaning |
|---|---|---|
| `air_assist_off` | 409 | The air assist input of the laser machine says off and the machine requires it on. |
| `artifact_in_use` | 409 | A task still works on this artifact or made it: remove the task first. |
| `authorization_required` | 409 | The server asks for the authorization of a job of this plugin before it starts (work_require_authorization) and the job has none for its present hash. |
| `bad_address` | 422 | The address of a machine or of its drill head node is missing or is not host:port. |
| `bad_app` | 422 | The name of the application is not one this server accepts (lower case letters, digits and hyphens, one of the known applications or of auth_extra_apps). |
| `bad_artifact` | 422 | The description of an artifact does not fit (name, format, result_type, inputs or params). |
| `bad_axis` | 422 | The axis of a jog is not one of x, y or z. |
| `bad_bgcode` | 422 | A binary G-code file (.bgcode) cannot be read or written: it is cut, its checksum is wrong, a block is out of order, of a type or a compression the format does not have, or it is bigger than the limit. |
| `bad_board` | 422 | board_mm is not from 0 to 50. |
| `bad_catalog` | 422 | The dataset uploaded to a catalog does not fit: not JSON or CSV that reads, items that are not objects, an item without the field that names it (or repeated), a missing format, or more items than catalog_max_items. |
| `bad_cfg` | 422 | The tuning text of the machine does not read. |
| `bad_container` | 422 | The .eh3dproj container cannot be read or written: structure, names, sizes, integrity record, or the project has no project.json. |
| `bad_credentials` | 401 | The name or the password is wrong. |
| `bad_days` | 422 | The days a token lasts are not a number from 1 to 3650. |
| `bad_dcf` | 422 | The text of DRILLPRO.DCF or ROUTEPRO.RCF does not read. |
| `bad_design_request` | 422 | The request to run the design program does not fit: the kind is not one of check, bom, fabpack or panel, the board file is missing, or the options are not accepted for the kind. |
| `bad_feed` | 422 | The feed of a jog is not a number above 0 and up to jog_max_feed of the machine. |
| `bad_filename` | 422 | The name of a file is not safe: empty, absolute, with .. or a drive, with control characters, a reserved name of Windows, too long, or the same as another in other letter case. |
| `bad_gcode` | 422 | A line of the G-code program cannot be sent (control characters, a $ line that is not allowed, a line too long). |
| `bad_gerber` | 422 | A Gerber or Excellon file cannot be read: the format, a coordinate or a command is wrong, or the file holds no data. |
| `bad_heater` | 422 | The heater is not hotend, bed or chamber. |
| `bad_hole` | 422 | A hole has a coordinate that is not a number. |
| `bad_id` | 422 | The id of a machine is not made of lowercase letters, digits and hyphens (up to 32). |
| `bad_light` | 422 | The query of the ring of LEDs of the drill head is not valid. |
| `bad_mesh` | 422 | The mesh to slice is not a closed STL or 3MF the slicer can read, or has no closed outline. |
| `bad_name` | 422 | A name is empty, too long or has characters that are not allowed. |
| `bad_ncd` | 422 | The drilling file does not read. |
| `bad_operation` | 422 | An operation of a vector job for a laser is not valid: its layer, its mode (cut or engrave), its power, its speed or its passes. |
| `bad_params` | 422 | The parameters of a task do not fit its engine, are not an object, or the idempotency key is not valid. |
| `bad_path` | 422 | A point of a path has a coordinate that is not a number. |
| `bad_picture` | 502 | The picture the camera of the drill head sent cannot be read. |
| `bad_placement_list` | 422 | A list of placements that came as an artifact cannot be read as JSON or as CSV with the columns ref, part, x and y. |
| `bad_plugin_request` | 422 | The request does not fit what the plugin of the machine takes, or the settings of a machine of a plugin are not valid. |
| `bad_pnp_node` | 422 | A node of a pick-and-place machine does not fit: its kind (feeder or head), its id, its name, its address (host:port), its token (no spaces or control characters) or its machine (a machine of the plugin pnp). |
| `bad_policy` | 422 | The switch of security is not one that can change while the server runs, or its value is not of the right type or is out of range. |
| `bad_printer` | 422 | A printer of the list does not fit: its family (bambu_lan or moonraker), its link (sim or lan), its id, its name, its address (host or host:port), its serial, its fingerprint or its access code. |
| `bad_profile` | 422 | The inspection profile is not valid. |
| `bad_rack` | 422 | The rack of a job request of the second version does not fit: places that repeat, the test pin as a place, more places than rack_max_slots, a carry_slot without carry_in, or a place the machine does not define or reach. |
| `bad_request` | 400 | The body of the request does not read as the request needs. |
| `bad_rules` | 422 | The rules of a check are not an object of known rules with values inside their ranges. |
| `bad_samples` | 422 | The number of samples of a measurement is out of range. |
| `bad_scope` | 422 | A scope of a token is not one the server knows. |
| `bad_setup` | 403 | The set-up code is wrong, or the server already has a user. |
| `bad_share` | 422 | The sharing does not fit: give a user or a token, not the owner, with an access that exists for it. |
| `bad_sorting` | 422 | The sorting of the holes is not one the program knows. |
| `bad_spindle` | 422 | The spindle, the switch or the limits of a spindle are not valid. |
| `bad_station` | 422 | The station of a machine is not made of lowercase letters, digits and hyphens (up to 32). |
| `bad_template` | 422 | The template of a token is not phoenix, cam, client or custom. |
| `bad_tool` | 422 | A tool has values out of range. |
| `bad_upload` | 422 | The declaration of an upload or one of its parts does not fit (hash, empty part, a part past the declared size). |
| `bambu_lan_off` | 409 | The LAN link to a real Bambu Lab printer is off on this server (the switch bambu_lan_enabled of server.cfg), so only the simulated printer (host sim) is reached. |
| `bgcode_unsupported` | 422 | A binary G-code file uses something the server does not read: another version of the format, or G-code packed with MeatPack. |
| `blob_damaged` | 500 | The bytes of an artifact are missing or are not what their hash says. |
| `both_sources` | 422 | The job brings the work as data and as files; it must be one of them. |
| `catalog_readonly` | 409 | The catalog is built in and is not written, not even by an administrator. |
| `compat_off` | 404 | The routes of Moonraker and OctoPrint at the root of the address are off: the switch compat_api of the security page is not on. |
| `compat_write_off` | 404 | The write routes of Moonraker and OctoPrint are off: the switch compat_write of the security page is not on. |
| `design_run_unavailable` | 501 | The server is not set up to run the design program: design_run is off or design_program is not set. |
| `enclosure_open` | 409 | The enclosure input of the laser machine says open and the machine requires it closed. |
| `engine_missing` | 422 | The kind of task is known but this build has no engine for it. |
| `farm_no_free_printer` | 409 | No printer of the list is free, unlatched and able to take the job. |
| `file_too_big` | 413 | The file is bigger than project_max_file_mb. |
| `focus_not_checked` | 409 | The focus input of the laser machine says the head is not focused and the machine requires it to be. |
| `forbidden` | 403 | The role of the caller is not enough for this request. |
| `forbidden_scope` | 403 | The token is limited to scopes that do not include this one, the request would give more than the token has (or outlive it), or a token of a design program was asked for a scope of the machines. |
| `hash_mismatch` | 422 | The bytes of the upload do not have the SHA-256 that was declared; the upload is discarded. |
| `head_refused` | 502 | The node of the drill head answered with a refusal. |
| `head_unreachable` | 502 | The node of the drill head does not answer. |
| `heater_timeout` | 409 | A heater did not get to its target in heat_timeout_s: the job stops and the heaters go off. |
| `idempotency_conflict` | 409 | The idempotency key was already used by this caller for a request with another body. |
| `input_too_large` | 413 | The files of a job are bigger than max_input_mib of the worker. |
| `internal` | 500 | A task of the server failed; the server is still up. |
| `job_active` | 409 | The job is running: stop it first. |
| `jog_while_running` | 409 | A job is running or held on the machine: stop it before a jog. |
| `laser_estop_latched` | 409 | An emergency stop latched the laser machine: nothing is prepared or started on it until an administrator releases it. |
| `laser_feed_limit` | 422 | A line of the G-code program asks for a cutting feed over max_feed_mm_min of the laser machine. |
| `laser_power_limit` | 422 | The job asks for more power (S) than the limit of the laser machine (power_limit_pct of max_s) and the machine is set to reject it. |
| `last_admin` | 409 | The last administrator cannot be deleted, disabled or demoted. |
| `local_only` | 403 | While the server has no user, only the computer it runs on may make the first administrator. |
| `locked` | 423 | Somebody else is editing the project (its lock has not expired). |
| `machine_alarm` | 409 | The machine is latched by an emergency stop (or in alarm): nothing is prepared, started or jogged until an administrator releases it. |
| `machine_busy` | 409 | A job is running on the machine. |
| `machine_exists` | 409 | A machine with this id already exists. |
| `machines_busy` | 409 | A job is running: the update would restart the server and stop it. |
| `missing_layer` | 422 | The job brings no file, or the check needs a copper layer and none of the files is one. |
| `no_head` | 404 | The machine has no node of the drill head. |
| `no_hole` | 409 | The picture of the drill head shows no hole to calibrate with. |
| `no_inspection` | 404 | No quality check has been made on this machine since the server started. |
| `no_project` | 422 | The artifact a task works on does not exist or cannot be seen. |
| `no_work` | 422 | The job has no holes, no paths and no lines to run. |
| `not_cancellable` | 409 | The job is neither ready nor running. |
| `not_in_state` | 409 | A job is held while it runs and resumed while it is held. |
| `not_locked` | 409 | The project has no lock to release. |
| `not_pausable` | 409 | Only a G-code job can be held. |
| `not_ready` | 409 | The job already ran or is running. |
| `not_running` | 409 | The job is not running. |
| `not_stopped` | 409 | Only a cancelled or failed job can be gone on from. |
| `not_supported` | 501 | The plugin of the machine does not support this operation (for example, holding a drilling job), or its machines are made elsewhere. |
| `out_of_table` | 422 | A hole or a path is outside the table of the machine. |
| `out_of_travel` | 422 | A move of the program or of a jog goes outside the travel of the machine (the machine's switch is on reject). |
| `outside_course` | 422 | The work goes past the course of an axis (the machine's switch is on reject). |
| `plugin_failed` | 502 | The plugin or the machine behind it failed while doing this. |
| `pnp_feeder_short` | 409 | A part of the placement job is in no feeder of the pick-and-place machine, or the feeders hold fewer than the job needs while the machine requires the stock. |
| `pnp_node_disabled` | 409 | The node of the pick-and-place machine is switched off, so it is not asked. |
| `pnp_node_exists` | 409 | A node with this id or this address is already registered. |
| `pnp_node_not_ready` | 409 | A node of this pick-and-place machine is silent, refuses the token, says it is safe or fails its own check, so the job does not start. |
| `pnp_nodes_full` | 409 | The list of the nodes of the pick-and-place machines is full (pnp_nodes_max). |
| `pnp_nodes_off` | 501 | The nodes of the pick-and-place machines are switched off on this server (pnp_nodes_enabled). |
| `precondition_failed` | 412 | The If-Match tag is not the one of the current version: read the file again before writing. |
| `precondition_required` | 428 | A write over an existing file needs If-Match with the tag (SHA-256) of the version it was made from. |
| `printer_disconnected` | 502 | The printer does not answer. |
| `printer_exists` | 409 | A printer with this id, this address or this serial is already registered. |
| `printer_latched` | 409 | An emergency stop or a fault of a heater latched the printer: nothing is prepared, started or heated on it until an administrator releases it. |
| `printer_untrusted` | 502 | The certificate of the printer is not the pinned one, or none is pinned: the connection is refused. |
| `printers_full` | 409 | The list of the printers is full (printers_max). |
| `printers_off` | 501 | The list of the printers is switched off on this server (printers_enabled). |
| `program_over_limit` | 422 | A program for a 3D printer asks for a temperature or a feed over a limit of the machine, so the whole program is refused. |
| `project_quota` | 507 | The project, the application or the store would pass its limit (project_max_mb, project_max_files, project_max_projects). |
| `question_changed` | 409 | The question named in the answer is not the one the job asks now: read the job again. |
| `question_required` | 409 | The answer to a job names no question and the setting answer_needs_question asks for it. |
| `queue_full` | 429 | As many tasks are waiting or running as the server accepts (max_tasks). |
| `quota` | 429 | The caller already has as many tasks waiting or running as one user may (tasks_per_user). |
| `sensor_fault` | 409 | A heater reads below min_temp_c, which is a broken sensor: the printer was stopped and the machine is latched until an administrator releases it. |
| `shares_off` | 501 | Sharing is switched off on this server (shares_enabled). |
| `slice_failed` | 502 | The program that slices did not give a result. |
| `slice_limit` | 422 | The object or the profile goes past a limit of the printer or of the slicer; nothing is cut down to fit. |
| `slice_off` | 404 | Slicing on the server is off: the switch slice of the security page is not on. |
| `spindle_limit` | 422 | The job asks for more than the spindle of the machine gives (the machine's switch is on reject). |
| `stale_authorization` | 409 | The hash a person authorised, or the one a start names, is not the hash of the job as it is now, so nothing starts. |
| `stale_parent` | 409 | The revision does not follow the last one of the project: look at what changed first. |
| `storage` | 500 | The data folder could not be written. |
| `store` | 500 | The inspection profile could not be saved. |
| `store_full` | 507 | The artifact store would pass artifact_store_gb. |
| `task_active` | 409 | The task has not finished: cancel it first. |
| `tasks_off` | 501 | The computing tasks are switched off on this server (tasks_enabled). |
| `temperature_over_limit` | 422 | A target of temperature is over the limit of the heater (the profile of the model of the printer, or the limit set for the machine). |
| `test_fire_refused` | 422 | A test fire asks for more power than test_fire_max_pct of the laser machine. |
| `thermal_runaway` | 409 | A heater is far from its target for too long or is past its limit: the printer was stopped and the machine is latched until an administrator releases it. |
| `throttled` | 429 | Too many wrong passwords, or too many tickets waiting: try again later. |
| `too_big` | 422 | The request or the work is bigger than the server accepts (413 for a body over max_body_mb). |
| `too_many_clients` | 429 | As many clients listen to the stream of events as events_clients_max allows. |
| `too_many_holes` | 422 | More holes than one inspection takes. |
| `unauthorized` | 401 | The token is missing, not valid or has expired. |
| `unit_unknown` | 422 | A Gerber or Excellon file uses coordinates before it says its units. |
| `unknown_artifact` | 404 | There is no artifact with this id, or it belongs to somebody else. |
| `unknown_catalog` | 404 | There is no catalog with this name. |
| `unknown_file` | 404 | The project has no such file. |
| `unknown_item` | 404 | The catalog has no item with this name. |
| `unknown_job` | 404 | There is no job with this id. |
| `unknown_kind` | 422 | There is no such kind of task. |
| `unknown_machine` | 404 | There is no machine with this id. |
| `unknown_plugin` | 422 | There is no plugin of this kind. |
| `unknown_pnp_node` | 404 | There is no node of a pick-and-place machine with this id. |
| `unknown_printer` | 404 | There is no printer with this id in the list. |
| `unknown_project` | 404 | There is no project with this id, or it belongs to somebody else. |
| `unknown_session` | 404 | There is no open login with this id, or it is not the caller's, or the login cannot be renewed any more. |
| `unknown_task` | 404 | There is no task with this id, or it belongs to somebody else. |
| `unknown_token` | 404 | There is no token with this id. |
| `unknown_tool` | 422 | The job uses a tool that it does not describe. |
| `unknown_upload` | 404 | There is no upload with this id, or it belongs to somebody else. |
| `unknown_user` | 404 | There is no user with this name. |
| `unknown_version` | 404 | The file has no such version (or it was dropped past project_versions_kept). |
| `update_refused` | 409 | The update helper refused the request. |
| `updates_off` | 501 | The updates are not set up on this server (updates_dir). |
| `upload_conflict` | 409 | A part carries other bytes than the ones already received at the same place. |
| `upload_incomplete` | 409 | Parts are still missing: the upload cannot be completed. |
| `user_exists` | 409 | A user with this name already exists. |
| `weak_confirm_required` | 409 | The new value of the switch of security is looser than recommended: send confirm_weak: true to apply it. |
| `weak_password` | 422 | The password is not from 8 to 128 characters. |
| `wrong_machine` | 422 | The job is of a kind the machine does not take. |
