# The API of the server

Everything is under `/api/v1/` and answers JSON. A mistake is `{"error": "<code>", "detail": "..."}` with the status that fits. Except the health check, the first set-up and the login, every request carries `Authorization: Bearer <token>`: the token of a login or the token of a program. A **viewer** reads, an **operator** also prepares, starts, stops and confirms jobs, an **administrator** also manages users, tokens and machines. The default port is 18200.

## Who is who

| Request | Role | What it does |
|---|---|---|
| `GET /health` | none | `{"name":"eh3d-server","version":"...","api_revision":1,"status":"ok"}`; `status` is `degraded` when the data folder takes no file or a lock was taken over after a panic. With a valid token also `uptime_s`, `storage` (`writable`), `recovered_locks` and `features` (`serial`, `studio`, `updates`, `url_token`, `local_until_setup`, `trust_forwarded_for`, `exit_on_panic`, `persist_sessions`) |
| `GET /setup` | none | `{"needed": true}` while there is no user |
| `POST /setup` | none | `{"code","name","password"}`: makes the first administrator with the set-up code the server prints on its console |
| `POST /login` | none | `{"name","password"}` gives `{"token","name","role","expires_s"}`; five wrong passwords block the name for a while (`429 throttled`). The login survives a restart of the server (`persist_sessions` of `server.cfg`, off by default: only the SHA-256 of the token is kept on the disk) until it expires or is ended with `POST /logout` |
| `POST /logout` | any | ends the login |
| `GET /session` | any | `{"name","role","is_token"}` |
| `PUT /account` | a person | `{"old","new"}`: changes the own password |
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

The lines that are sent are the code without its comments (in parentheses or after `;`); blank lines and `%` are not sent. A line that is not G-code is refused with `bad_gcode` (and the line it is on): the settings and real-time commands of the controller (`$`, `!`, `~`, `?`) and control characters cannot be part of a job (a machine whose administrator switched `allow_dollar` on takes the `$` lines of GRBL such as `$H`, `$X`, `$G`, `$J=...` and the settings, see below; `$RST` and the real-time characters are refused always), and a line may have 200 characters at the most. A program with no line to send is refused with `no_work`, one with more than a million lines with `too_big`. The view has `kind: "gcode"`, no holes, paths or tools, the `bounds_mm` of the straight moves (an arc counts as the line to its end), the time and length of the moves, and `items`, the number of lines that will be sent. The spindle switch of the machine judges the program too: the largest `S` and the largest `F` (converted to mm/min when the program works in inches) are checked against the spindle the machine carries; with `reject` (the default) a program beyond them is refused with `spindle_limit`, with `warn` the `S` words are cut to the top of the spindle and the excesses are listed in `spindle_warnings` (tool `0`).

`skip` is how many lines of the text (counting blank and comment lines) are already done. A program that goes on after a stop does not send them again: the codes they put in force (units, distance mode, plane, work coordinate system, feed mode and the last `F`) are given again in a line before the first one, and the job **waits for the operator before its first move** (state `waiting_operator` with `pause.kind` `resume`; `POST /jobs/:id/resume` or `confirm` lets it go). The spindle and the coolant are started again only when `restore_outputs` is `true`. A failed or stopped job says in `line` where to start from again. The offset of `G92` is not given back, and a controller that was reset needs its work zero set again.

Every drilling or routing view carries `dry_run` (the exact steps of the plan followed on paper: `min_mm`, `max_mm` and `end_mm` as X, Y, Z in mm from the switches, `steps`, `seconds` and `within_course`) and `warnings`, what looks wrong in the work (`code`: `tiny`, `too_big`, `far`, `scale` or `out_of_course`, with the numbers it is about in `values`, in mm, and `axis` for the last). A plan that takes an axis past the course of the machine's configuration file is refused with `outside_course` unless the tuning of the machine says `dry_run = warn`; `dry_run_margin_mm`, `sanity_tiny_mm` and `sanity_far_mm` are keys of the same tuning. A G-code job has neither.

The holes or the files, not both. With files the holes are ordered as the program does (`sorting`: `none`, `x`, `y` or `special`; by default the machine's configuration says) and their units come from the machine's configuration file. A job is refused with a code before it exists: `no_work`, `both_sources`, `spindle_limit` (beyond what the spindle of the machine gives, see below), `unknown_tool` (a tool the work uses is not in the table), `bad_tool`, `bad_board`, `out_of_table` (a hole or a point outside the course of the machine), `outside_course` (the exact plan takes an axis past its course), `bad_ncd`, `too_big`, `bad_request`.

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
| `POST /jobs/:id/confirm` | operator | answers the dialog the job waits at |
| `POST /jobs/:id/cancel` | operator | stops a running job (the machine stops, the drivers off) or cancels a ready one |
| `POST /machines/:id/stop` | operator | **the stop button**: ends the job that is active on the machine without naming it, at once. On a GRBL machine the feed hold (`!`) and the soft reset (`0x18`) are written to the port from another thread, without waiting for the line that is being sent (a homing cycle or a long move included); on a node the drivers go off. `{"ok":true,"stopped":true,"job":"j..."}`, or `stopped:false` when this server has no job on the machine (it can only stop what it is running) |
| `POST /jobs/:id/pause` | operator | holds a running G-code job: the controller holds its movement and no line goes out until `resume`; `409 not_pausable` for a drilling or routing job, `409 not_in_state` when it is not running |
| `POST /jobs/:id/resume` | operator | lets a held G-code job go on (also the wait of a job that goes on after a stop); `409 not_in_state` when it is not held. `confirm` does the same for a G-code job |
| `GET /jobs/:id/request` | operator | `{"id","machine","kind","name","state","resumed_from","next_skip","request"}`: what was asked of the job exactly as it came (the file, the tool table, the options, the program text), kept with the job and removed with it (`DELETE /jobs/:id`, or when the list of finished jobs drops it, `keep_jobs`); it cannot be bigger than a request may be (`max_body_mb`). For a cancelled or failed job `next_skip` is where to go on from (otherwise `null`) |
| `POST /jobs/:id/resume-from` | operator | makes a **new job, ready and not started**, from the saved request of a cancelled or failed job, with its `skip` brought forward to what that job had done, so the client need not ask for the file again: a drilling job leaves out the holes done (`holes_done` added to the `skip` it had), a G-code job goes on from its `line`, a routing job leaves out the paths it finished (the one it cut short is cut again). The sums add up over several stops. The body is optional: `{"skip"?,"skip_tools"?,"restore_outputs"?,"name"?}` (`skip` replaces what was worked out, in holes, paths or lines; `skip_tools` replaces the tools that are off). The answer is the new job like `POST /machines/:id/jobs` (with `resumed_from`); `409 not_stopped` for a job that is ready, running or done, `422 no_work` when nothing is left, and the same refusals as a new job. It never moves the machine: it is started with `POST /jobs/:id/start` by a person, a drilling job asks for its tool and spindle, and a G-code job waits for the operator before its first move |
| `DELETE /jobs/:id` | operator | removes a job that is not running |

States: `ready` → `connecting` → `running` ⇄ `waiting_operator` → `done`, `cancelled` or `failed` (`message` says why). A job that was running when the server stopped is marked `failed`: nobody knows where the machine was. While it waits, `pause` is `{"kind":"tool_change","tool":2}` (put the tool in and confirm) or `{"kind":"spindle_start"}` (confirm the spindle start); for a G-code job it is `{"kind":"hold"}` (the operator paused it) or `{"kind":"resume"}` (it goes on after a stop and waits before its first move). A job also says who handled it: `by` asked for it, `started_by` started it and `stopped_by` stopped it (cancelled it or pressed the stop of its machine; `server` when the server itself ended while it ran), each empty while nobody did it; the same facts are in the audit and in `eh3d-jobs.log` (see `docs/DEPLOY.md`). A job is also given its `line`: for a G-code job the line of the program text it has got to (where a job that goes on starts from), 0 for the others. A job has `items_done` of `items_total` and `holes_done` of `holes_total` (a routing job counts paths; a G-code job has no holes, and its items are the lines sent). A G-code job that is refused by the controller (a line it does not take, an alarm, a link that fails) is marked `failed` with the line in `message`, and the server stops the controller as far as it can: nobody is at the screen to decide. A job is done when the machine is still after its last line, and fails if it is not within `gcode_finish_wait_s` seconds of `server.cfg`.

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

## The spindle of the machine

A machine carries one of two spindles, `original` (the one it leaves the factory with) or `vfd` (with a variable-frequency drive that the node of the machine drives), or none is said (empty: no job is judged against a spindle). The limits of each are the starting ones (`original`: 5000 to 63000 turns, 3000 mm/min along the cut, 4000 mm/min of plunge, bits up to 3.2 mm; `vfd`: 6000 to 24000 turns, 1500 and 1200 mm/min, bits up to 6 mm) with what the machine changed on top. They are also members of the machine (`spindle`, `spindle_guard`, `spindle_original`, `spindle_vfd`); a `PUT /machines/:id` that does not send them leaves them as they were.

**The `$` lines of GRBL.** The machine member `allow_dollar` (`false` by default; any role reads it, only an administrator changes it, with `PUT /machines/:id`, and an update that does not send it leaves it as it was) lets the free G-code jobs of that machine carry the `$` lines of GRBL: `$H` (homing), `$X` (unlock), `$G`, `$#`, `$$`, `$I`, `$C`, `$J=...` (jog) and the settings (`$100=250`). It is a safety switch: such a line can move the machine to its limit switches or change the settings of the controller, so it is turned on per machine and only where the operator is at the machine. Even then the server refuses the line `$RST` (it wipes the memory of the controller), the real-time characters (`!`, `~`, `?`), control characters and every `$` line with signs other than letters, digits, spaces and `=.-+#$`, with `422 bad_gcode`. A ready job is judged again when it starts, so a job made while the switch was on is refused (`bad_gcode`) if the switch has been turned off since. The simulator takes the safe ones (`$H` as a homing cycle that leaves the machine at its home, `$X`, `$G`, `$#`, `$$`, `$I`, `$C`, `$J=...` and the numbered settings) and refuses `$RST`, `$N0=...`, `$SLP` and the rest, failing the job at that line and saying which. On a GRBL machine the waits are set in `server.cfg`: an ordinary line `grbl_line_timeout_s` (5 s), the homing `grbl_homing_timeout_s` (120 s) and a jog `grbl_jog_timeout_s` (60 s); a line that does not end in time holds the machine (a homing cycle is reset, a jog is cancelled) and the job is `failed`, with the line in `message`.

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
| `POST /tasks` | operator | `{"kind","name"?,"params"?,"project"?,"idempotency_key"?}` gives `201` and the task. `name` is the kind when it is left out (1 to 80 characters, no control characters, else `422 bad_name`). `params` is an object (`422 bad_params` otherwise or when the engine does not accept it; `422 too_big` over `task_params_kb`). `project` is the id of the artifact the task works on; this build keeps no artifacts yet, so naming one answers `422 no_project` |
| `GET /tasks?state=&kind=` | viewer | the tasks the caller may see, newest first |
| `GET /tasks/:id` | viewer | one task, with its result |
| `POST /tasks/:id/cancel` | operator (owner or admin) | `{"accepted","task"}`. A queued task is cancelled at once; a running one is told to stop and ends `cancelled` with no result (what it was writing is discarded); a task that had already finished answers `accepted: false` and is not changed |
| `DELETE /tasks/:id` | operator (owner or admin) | removes a finished task; `409 task_active` while it waits or runs |

A task: `{"id":"t-<uuid v7>","kind","name","owner","state","created","started","finished","progress":{"done","total","unit"},"message","result","engine":{"version"}}`. The states are `queued`, `running`, `done`, `failed` and `cancelled`. The `result` of a finished task is `{"artifacts":[],"diagnostics":[...],"data":{}}`; a diagnostic is the common form of the suite (`code`, `severity`, `message`, and the optional `line`, `location`, `category`, `recoverability`, `args`). The codes of the task system itself are `EH3D-TASK-001` (a task asked to fail, the self-test), `EH3D-TASK-002` (longer than `task_timeout_s`), `EH3D-TASK-004` (the server restarted while it ran) and `EH3D-TASK-005` (the engine failed unexpectedly).

**Who sees what.** Every task has an owner: the user who made it, or the program token (`token:<id>`: two tokens with the same name are two owners). The owner and the administrators see it; for anybody else it does not exist: reading, cancelling or deleting it answers `404 unknown_task`, the same as for an id that was never made, and it is not in their list.

**The queue.** At most `max_concurrent_tasks` run at the same time, the rest wait in the order they came. The limits refuse before the task exists: `429 queue_full` (`max_tasks` waiting or running, all users), `429 quota` (`tasks_per_user` of one user), `422 too_big` (`task_params_kb`), `422 unknown_kind`, `422 engine_missing`. A task that runs longer than `task_timeout_s` is stopped and fails with `EH3D-TASK-002`. After a restart the queued tasks stay queued and go on, and the task that was running is `failed` with the message `interrupted by restart`; it may be sent again. The newest `keep_tasks` finished tasks are kept.

**Sending the same request twice.** With an `idempotency_key` (1 to 128 printable characters), a second request of the same caller with the same key and the same body (the parameters in any order) answers `201` with the first task, whether it is still there waiting or already done; the same key with another body answers `409 idempotency_conflict`. The key is remembered for `idempotency_ttl_s` (a day) and is the key of whoever sent it: another caller may use the same text for something else.

**Events.** The websocket sends `{"type":"task","task":{...}}` when a task is made, starts, moves on or ends, and `{"type":"task_removed","id":"..."}` when one is removed, **only to the clients that may see the task** (its owner and the administrators), the removal too.

## What happens, as it happens

`GET /events?ticket=<ticket>` upgrades to a websocket (a browser cannot set headers on one, so the address carries a ticket from `POST /tickets`; `Authorization: Bearer` works too, and `?token=` only with `url_token = true`). The server asks every 30 seconds whether the session is still good and closes the websocket when it is not. It also sends a ping every `events_ping_s` seconds (30) and closes a connection that has sent nothing at all, a pong included, for `events_idle_s` seconds (90), so a phone that left the Wi-Fi does not hold a place for ever. A client that read more slowly than the events came (the channel holds 256) is told `{"type":"resync","missed":N}`: it asks for the fleet and the jobs again, because it lost some. The first text is `{"type":"hello","jobs":[...]}` with the summaries of the jobs; after it every change arrives as `{"type":"job","job":{...}}`, `{"type":"job_removed","id":"..."}` or `{"type":"machine","id":"..."}`.

## Error codes

Every refusal carries one of these codes (the same list is in `error-codes.json`, which the clients read in their tests). The status is the usual one; a few codes come with another where the situation differs.

| Code | Status | Meaning |
|---|---|---|
| `bad_address` | 422 | The address of a machine or of its drill head node is missing or is not host:port. |
| `bad_board` | 422 | board_mm is not from 0 to 50. |
| `bad_cfg` | 422 | The tuning text of the machine does not read. |
| `bad_credentials` | 401 | The name or the password is wrong. |
| `bad_days` | 422 | The days a token lasts are not a number from 1 to 3650. |
| `bad_dcf` | 422 | The text of DRILLPRO.DCF or ROUTEPRO.RCF does not read. |
| `bad_gcode` | 422 | A line of the G-code program cannot be sent (control characters, a $ line that is not allowed, a line too long). |
| `bad_hole` | 422 | A hole has a coordinate that is not a number. |
| `bad_id` | 422 | The id of a machine is not made of lowercase letters, digits and hyphens (up to 32). |
| `bad_light` | 422 | The query of the ring of LEDs of the drill head is not valid. |
| `bad_name` | 422 | A name is empty, too long or has characters that are not allowed. |
| `bad_ncd` | 422 | The drilling file does not read. |
| `bad_params` | 422 | The parameters of a task do not fit its engine, are not an object, or the idempotency key is not valid. |
| `bad_path` | 422 | A point of a path has a coordinate that is not a number. |
| `bad_picture` | 502 | The picture the camera of the drill head sent cannot be read. |
| `bad_profile` | 422 | The inspection profile is not valid. |
| `bad_request` | 400 | The body of the request does not read as the request needs. |
| `bad_samples` | 422 | The number of samples of a measurement is out of range. |
| `bad_setup` | 403 | The set-up code is wrong, or the server already has a user. |
| `bad_sorting` | 422 | The sorting of the holes is not one the program knows. |
| `bad_spindle` | 422 | The spindle, the switch or the limits of a spindle are not valid. |
| `bad_station` | 422 | The station of a machine is not made of lowercase letters, digits and hyphens (up to 32). |
| `bad_tool` | 422 | A tool has values out of range. |
| `both_sources` | 422 | The job brings the work as data and as files; it must be one of them. |
| `engine_missing` | 422 | The kind of task is known but this build has no engine for it. |
| `forbidden` | 403 | The role of the caller is not enough for this request. |
| `head_refused` | 502 | The node of the drill head answered with a refusal. |
| `head_unreachable` | 502 | The node of the drill head does not answer. |
| `idempotency_conflict` | 409 | The idempotency key was already used by this caller for a request with another body. |
| `internal` | 500 | A task of the server failed; the server is still up. |
| `job_active` | 409 | The job is running: stop it first. |
| `last_admin` | 409 | The last administrator cannot be deleted, disabled or demoted. |
| `local_only` | 403 | While the server has no user, only the computer it runs on may make the first administrator. |
| `machine_busy` | 409 | A job is running on the machine. |
| `machine_exists` | 409 | A machine with this id already exists. |
| `machines_busy` | 409 | A job is running: the update would restart the server and stop it. |
| `no_head` | 404 | The machine has no node of the drill head. |
| `no_hole` | 409 | The picture of the drill head shows no hole to calibrate with. |
| `no_inspection` | 404 | No quality check has been made on this machine since the server started. |
| `no_project` | 422 | The artifact a task works on does not exist or cannot be seen. |
| `no_work` | 422 | The job has no holes, no paths and no lines to run. |
| `not_cancellable` | 409 | The job is neither ready nor running. |
| `not_in_state` | 409 | A job is held while it runs and resumed while it is held. |
| `not_pausable` | 409 | Only a G-code job can be held. |
| `not_ready` | 409 | The job already ran or is running. |
| `not_running` | 409 | The job is not running. |
| `not_stopped` | 409 | Only a cancelled or failed job can be gone on from. |
| `out_of_table` | 422 | A hole or a path is outside the table of the machine. |
| `outside_course` | 422 | The work goes past the course of an axis (the machine's switch is on reject). |
| `queue_full` | 429 | As many tasks are waiting or running as the server accepts (max_tasks). |
| `quota` | 429 | The caller already has as many tasks waiting or running as one user may (tasks_per_user). |
| `spindle_limit` | 422 | The job asks for more than the spindle of the machine gives (the machine's switch is on reject). |
| `storage` | 500 | The data folder could not be written. |
| `store` | 500 | The inspection profile could not be saved. |
| `task_active` | 409 | The task has not finished: cancel it first. |
| `tasks_off` | 501 | The computing tasks are switched off on this server (tasks_enabled). |
| `throttled` | 429 | Too many wrong passwords, or too many tickets waiting: try again later. |
| `too_big` | 422 | The request or the work is bigger than the server accepts (413 for a body over max_body_mb). |
| `too_many_holes` | 422 | More holes than one inspection takes. |
| `unauthorized` | 401 | The token is missing, not valid or has expired. |
| `unknown_job` | 404 | There is no job with this id. |
| `unknown_kind` | 422 | There is no such kind of task. |
| `unknown_machine` | 404 | There is no machine with this id. |
| `unknown_task` | 404 | There is no task with this id, or it belongs to somebody else. |
| `unknown_token` | 404 | There is no token with this id. |
| `unknown_tool` | 422 | The job uses a tool that it does not describe. |
| `unknown_user` | 404 | There is no user with this name. |
| `update_refused` | 409 | The update helper refused the request. |
| `updates_off` | 501 | The updates are not set up on this server (updates_dir). |
| `user_exists` | 409 | A user with this name already exists. |
| `weak_password` | 422 | The password is not from 8 to 128 characters. |
| `wrong_machine` | 422 | The job is of a kind the machine does not take. |