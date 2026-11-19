# Machine plugins, the queue of jobs and the stream of events

The third of the four common services (`docs/SERVICES.md`). The server first knew one kind of machine, the Bungard drilling and routing machine, and its routes (`/api/v1/machines`, `/api/v1/jobs`) were written for it. An application that is not for that machine (a laser, a pick-and-place, the stencil printer) needs the same things: a machine to choose, a job to prepare, start, hold and stop, how far it is, and to be told when it ends. This service gives them without the server knowing which machine is behind.

The contract is the `work` tag of `docs/openapi.yaml`. The code is `src/plugins.rs` (the plugins), `src/work.rs` (the machines of the plugins, the queue, the history and the notifications), `src/feed.rs` (the events) and `src/api_work.rs` (the routes).

**Nothing of the first version changes.** `/api/v1/machines`, `/api/v1/jobs` and the websocket `/api/v1/events` answer as they did, with the same tests. The generic queue is a new family beside them.

## The plugin

A machine plugin is a value of a type that implements `MachinePlugin` (`src/plugins.rs`):

| Operation | What it does |
|---|---|
| `kind`, `title` | the short name (`bungard`, `demo`, `laser`, `pnp`, `pcb`) that `plugin` says in machines and jobs, and a line of text |
| `capabilities` | what the machines can do: the kinds of request they take, whether they can be held, how progress is counted |
| `machines` | the machines of this kind, with their state (`idle` or `busy`) |
| `prepare` | checks a request and makes a job ready on a machine; nothing moves. Gives the id the plugin knows the job by and what a client draws of it |
| `start`, `pause`, `resume`, `stop` | the operations on a job; a plugin that cannot do one answers `not_supported` |
| `status` | the state of a job (`queued`, `running`, `paused`, `done`, `failed`, `cancelled`) and how far it is (`done` of `total`) |
| `discard` | a job that was prepared and will not start: the plugin lets go of what it kept |
| `manages_machines`, `check_settings` | whether the machines of the plugin are made with `POST /work/machines`, and what their settings may be |
| `detail` | what a client sees of a machine besides its state (the latch of a laser, the feeders of a pick-and-place machine...); optional |

To add a plugin: write the type, add it to `Registry::standard()` and describe it in `docs/openapi.yaml` through `tools/make_openapi.py`. The generic layer, the history, the events, the notifications, the roles and the audit come with it.

### `bungard`

The machines of DRILLPRO and ROUTEPRO: the ones of `/api/v1/machines`, whatever their connection (a node, a GRBL controller, the simulator). `prepare` takes the same requests as `POST /machines/{id}/jobs` (`drill`, `route`, `gcode`, with the `rack` of the second version), plans them with the same libraries and puts the job in **the same book** as the old route does, so `GET /jobs/{native}` shows it and `POST /jobs/{native}/confirm` answers its dialogs. `start` and `stop` are the old start and cancel; a free G-code job can be held and resumed, a drilling or routing job cannot (`501 not_supported`). The dialogs of the operator (change the tool, switch the spindle on) show as `running` with the message `waiting for the operator (...)` and make a notification; they are confirmed with the old `/jobs/{native}/confirm`. Progress is counted in items.

The generic queue lists the jobs made through it; a job made with the old routes stays in the old book and is not in the queue.

### `demo`

A machine with no hardware, for the tests of the applications and of the server. Its machines are made with `POST /work/machines` (`"plugin": "demo"`, settings `{"step_ms": 20}`, 0 to 3600000). A job is `{"kind": "demo", "steps": 10}` (1 to 100000); it takes `step_ms` milliseconds per step, so with `step_ms = 0` it ends as soon as it runs and with a long step it stays where it is put. It can be held and resumed (the clock stops while it is held). It lives in memory: a demo job that was open when the server stopped is marked as failed.

### `laser`

Laser machines for the laser programs. It is a **simulator**: the jobs are checked and run against a model of the machine, and nothing has been tried on a real laser. Its machines are made with `POST /work/machines` (`"plugin": "laser"`). The settings are checked, and a key that is not one of these is refused:

| Setting | Meaning |
|---|---|
| `bed_x_mm`, `bed_y_mm` | the bed, 10 to 5000 (400 by default) |
| `max_s` | the value of `S` that is full power, 1 to 65535 (1000) |
| `power_limit_pct` | the most power a job may ask, in percent of `max_s`, 1 to 100 (**40**) |
| `over_limit` | `reject` (default) refuses a job that asks for more; `clamp` cuts every `S` to the limit |
| `max_feed_mm_min`, `rapid_mm_min` | the fastest cutting feed (6000) and the speed of a rapid move (12000) |
| `latch_on_stop` | an emergency stop latches the machine (true) |
| `test_fire_max_s` | the longest test fire, 0.01 to 10 seconds (1) |
| `sim_rate` | how many times faster than real time the simulation works, 0 to 100000 (1; 0 is no waiting) |

Raising `power_limit_pct`, switching `over_limit` to `clamp` or `latch_on_stop` off loosens a protection; they are the settings of the machine and only an administrator changes them.

A job is one of three kinds:

* `{"kind": "gcode", "text": "...", "power_scale_pct": 50}` is a G-code program. It is read word by word and written again from the words that were understood (comments and anything else are dropped; `$` lines, real-time characters, `G28`, `G92`, tool changes and expressions are refused with `422 bad_gcode`). Every `S` is held to the power limit (`422 laser_power_limit`, or cut when the machine clamps), every feed to `max_feed_mm_min` (`422 laser_feed_limit`), every move and every arc to the bed (`422 outside_course`), and a rapid move with the beam on in constant mode (`M3`) is refused. A program that ends with the beam on gets an `M5`. `power_scale_pct` (1 to 100) lowers every `S` before the limit is looked at. The `view` has the number of lines, the length of the cut and of all the moves, the estimate in seconds, the highest `S`, how many were cut, the box of the cut and a drawing of at most 2000 points. The head starts where the machine left it.
* `{"kind": "test_fire", "power_pct": 10, "seconds": 0.5}` is a short pulse to focus, with the same limits as a program.
* `{"kind": "rearm"}` says that a person looked at the machine after an emergency stop.

**The emergency stop** is `POST /work/jobs/{id}/cancel` on a job that runs or is held: the job ends at once, the beam goes off and, unless `latch_on_stop` is off, the machine is **latched**. A latched machine refuses to prepare or start any job but `rearm` (`409 laser_estop_latched`) until a `rearm` job has run. A job can be held and resumed (the beam is off while it is held). Progress is counted in lines. `GET /work/machines/{id}` has `detail`: `estop_latched`, `position_mm`, `beam_on`, the power in force and the bed.

### `pnp`

Pick-and-place machines for the pick-and-place program. A **simulator**; nothing has been tried on a real machine. The machines are made with `POST /work/machines` (`"plugin": "pnp"`). Settings: `feeders` (slots, 1 to 200, 40 by default), `bed_x_mm`, `bed_y_mm`, `feeder_x0_mm`, `feeder_pitch_mm`, `feeder_y_mm` (where the slots are picked from), `travel_mm_s`, `pick_ms`, `place_ms`, `max_placements`, `require_stock`, `sim_fail_pick_every` and `sim_rate`. A machine has feeders in numbered slots, each with a part (its name) and how many are left.

* `{"kind": "load_feeders", "feeders": [{"slot": 1, "part": "R_0603_10k", "count": 150}], "replace": false}` says what is in the slots, as a person put it there; `replace: true` empties the slots that are not in the list (an empty list with `replace: true` empties them all).
* `{"kind": "place", "placements": [{"ref": "R1", "part": "R_0603_10k", "x": 20, "y": 30, "rot": 90, "side": "top"}], "offset": {"x": 0, "y": 0}}` places parts. It is checked when it is prepared: the references are different, every place (moved by the offset) is on the bed (`422 outside_course`), and every part is in a feeder with enough left (`409 pnp_feeder_short`; a machine with `require_stock` off lets a job start whose feeders hold too few, and the job fails when one runs dry). The parts are taken from the first slot that has some left. The `view` has the placements with their slot, the count of each part, the travel and the estimate.

The simulated head takes the part off its feeder when it picks, so a feeder that is empty stops the job as `failed` (`feeder 3 is empty at R12`), and a machine with `sim_fail_pick_every` set to N loses the vacuum at the Nth placement of a job (the part is gone and the job fails there) to try how a program deals with it. The feeders are looked at again when the job starts, so a table changed in between is not trusted. `detail` has the `feeders` (with where each is picked from), the `head`, `placed_total` and `failed_picks`. Progress is counted in placements.

### `pcb`

Board stations that make reports and checks and move nothing. A **simulator** of the time a station takes; the reports themselves are made from the data the job brings. The machines are made with `POST /work/machines` (`"plugin": "pcb"`). Settings: the rules of the maker `min_track_mm`, `min_gap_mm`, `min_drill_mm`, `max_drill_mm` and `min_edge_mm`, `drill_hit_s`, `max_items`, `fail_on_violation`, `item_ms` and `sim_rate`.

* `rules_check`: `{"board": {"outline": {"width_mm", "height_mm"}, "tracks": [{"width_mm"}], "clearances": [{"gap_mm"}], "drills": [{"dia_mm", "x", "y"}]}, "rules": {...}}` against the rules (the request may put its own over the ones of the station). It lists every `track_width`, `clearance`, `drill_small`, `drill_outside` and `drill_edge` error and every `drill_large`, `drill_duplicate` and `no_outline` warning with the item, the value and the limit, and gives the verdict `pass` or `fail`.
* `drill_report`: the tools (holes of the same size), how many hits and how many are plated, the travel and an estimate of the time.
* `bom_report`: the parts grouped by value and footprint, the references that repeat (error) and the parts with no value or footprint (warning).
* `inspect`: `{"measurements": [{"name", "nominal", "tolerance", "value"}]}` each judged against its tolerance.

The report is in the `view` of the job as soon as it is prepared; the job then runs for the time the station takes to go over its items. A job whose report has errors ends `done` with the verdict `fail`, unless the station has `fail_on_violation`, and then it ends `failed`. `detail` has how many jobs were done, the kind and the verdict of the last one and the rules in force. Progress is counted in items.

## The machines

`GET /plugins` lists the plugins. `GET /work/machines` lists the machines of every plugin, each with its `plugin`, its state, the job of the generic queue that has it, its capabilities and, for the plugins that have something to say (`laser`, `pnp`, `pcb`), its `detail`. `POST /work/machines` (administrator, scope `machines:manage`) makes a machine for a plugin that keeps its machines here; its id may not be the id of any other machine, of any plugin (`409 machine_exists`). `PUT` and `DELETE` change and remove it (not while a job is open on it, `409 machine_busy`). The machines of `bungard` are made, changed and removed with the old routes.

## The queue and the history

| Request | Role | Scope |
|---|---|---|
| `POST /work/jobs` prepare a job: `{"machine", "request", "name", "app"}` | operator | `jobs:prepare` |
| `GET /work/jobs` list (filters `machine`, `state`, `app`, `mine`; `limit`, `offset`); `GET /work/jobs/{id}` read with its history | viewer | `machines:view` |
| `POST /work/jobs/{id}/start`, `/pause`, `/resume` | operator | `jobs:start` |
| `POST /work/jobs/{id}/cancel`, `DELETE /work/jobs/{id}` (a finished one) | operator | `jobs:cancel` |

A job is **queued** after `POST /work/jobs` (its `position` says its place among the queued jobs of the machine; at most `work_queue_max` wait) and starts only when a person starts it, one job at a time on a machine (`409 machine_busy`). It goes to `running`, may be `paused`, and ends `done`, `failed` or `cancelled`. The `history` of the job keeps every change with the time, the state, who made it (a person, or `machine` when the plugin said it) and a note. The state is read from the plugin every half second by a thread of the server and again whenever a job is asked for, so a client that only polls `GET /work/jobs/{id}` sees the real state. The jobs are kept in the data folder (the last `work_keep` finished ones); `view`, the drawing the plugin made when it prepared the job, stays in memory until the server restarts (for a `bungard` job `GET /jobs/{native}` has it).

## The stream of events

`GET /api/v1/events/stream` is a stream of server-sent events (`text/event-stream`), which every browser reads with `EventSource` and every platform with a plain HTTP client. Each event is

```
id: 17
event: job.state
data: {"id":17,"kind":"job.state","machine":"demo-1","project":null,"job":"w-...","app":"studio","data":{...}}
```

The first one is `hello` (no `id`): `{"last_id", "resumed", "resync", "clients"}`.

| Kind | When |
|---|---|
| `project.created`, `project.changed`, `project.removed`, `project.file`, `project.file_removed`, `project.locked`, `project.unlocked` | the project store (`docs/PROJECT_STORE.md`); heard by the owner and the administrators, by everybody with `project_shared` |
| `job.created`, `job.state`, `job.progress` | the generic queue; `job.progress` at every tenth of the work |
| `machine.created`, `machine.changed`, `machine.removed` | the machines of the plugins |
| `catalog.changed` | an administrator replaced a catalog (`docs/CATALOGS.md`) |
| `notification` | something for the owner of a job (`id`, `kind`, `title`, `data`); heard by the owner and the administrators |

**Filters** (query): `machine`, `project`, `job`, `app` (the event has to be about it) and `kinds` (separated by commas; `job` also matches `job.state`). A token with scopes hears the families its scopes allow (`projects:read`, `machines:view`, `catalog:read`).

**Reconnecting.** Every event has a number that goes up by one. The server keeps the last `events_queue`; a client that reconnects with `Last-Event-ID: <last number it got>` (a browser sends it by itself; `?last_event_id=` does the same where a header cannot be set) gets the events it missed first, in order, and the live ones after, without repeats. If more were missed than the server keeps, the `hello` says `"resync": true` and the client has to ask for the state again (`GET /work/jobs`, `GET /work/machines`...); a client that is listening but falls behind the live flow by more than `events_queue` gets the event `resync` with the number it missed.

**Limits.** At most `events_clients_max` clients listen at once (`429 too_many_clients`); a comment line `: ping` goes out every `events_ping_s` seconds to keep proxies from closing the connection; when the login of a client ends (logout, expiry, the user taken off) its stream ends within half a minute.

**Authentication.** `Authorization: Bearer <token>`; an `EventSource` cannot set a header, so it can send a one-use ticket from `POST /tickets` in `?ticket=` (the token in the address only with `url_token = true`).

## The notifications

A job that ends, fails, is held by the machine or waits at a dialog of the operator makes a notification for the person who asked for it: `GET /notifications` (`?unread=true`; the administrators see all), `POST /notifications/read` with `{"ids": [...]}` or `{"all": true}`. The `title` is in English; a client shows its own text for the `kind`. They are kept in memory, the last `notifications_keep`. The stream carries a `notification` event at the same moment, so an application can show it without polling.

## Roles, scopes and audit

Operations that move a machine are the operator's and need `jobs:prepare`, `jobs:start` or `jobs:cancel`; the machines of the plugins are the administrator's (`machines:manage`, a scope that a token of the design programs can never hold). The audit has a line for every machine made, changed or removed and for every job prepared, started, held, resumed, cancelled or removed, with who and the ids.

## Planned plugins

These plugins do not exist yet. Each sheet says what the plugin will be, so that it can be built in one round without inventing the contract; nothing below is served by the server today. The order of work is `cnc`, `pnp`, `laser`, `cam`, `gerber`, `pcb`, `printer3d`. Every plugin follows the same rules: it is a value of a type that implements `MachinePlugin` and is added to `Registry::standard()`; it has a simulator that needs no hardware (`connection.kind = "demo"` or its own `sim`) so that every route is tested; its routes are described through `tools/make_openapi.py` (so `docs/openapi.*`, `docs/ROUTES.md` and `docs/API.md` carry them); its refusals are codes of `docs/error-codes.json` with their texts in the eight languages of the suite (en, es, it, de, fr, pt of Portugal, zh, ja); its settings have a range and a conservative default and are checked by `check_settings`; whatever is a safety limit is a setting, never a constant; and its job requests that move a machine are the operator's (`jobs:prepare`, `jobs:start`, `jobs:cancel`). A machine that can move has a stop that does not wait for the queue (`POST /machines/{id}/stop` of the machine family, as `bungard` has).

The generic routes (`/work/machines`, `/work/jobs`, the events and the notifications) serve every plugin. A plugin adds only what they cannot say: its `request` kinds, its `detail` and, where the plugin needs a read that is not a job, a route under `/api/v1/work/machines/{id}/...` with the role and scope written in its sheet.

### `cnc`

For PHOENIX-CNC and any router, mill or engraver driven by G-code. Request kinds: `gcode` (`{"kind": "gcode", "text": "...", "skip": 0, "restore_outputs": false}`, the request PHOENIX-CNC already sends to `/machines/{id}/jobs`) and `file` (`{"kind": "file", "artifact": "<id>"}`, a G-code artifact of the store). Machines (`POST /work/machines`, `"plugin": "cnc"`) have a connection `grbl` (serial), `node` (a node of the suite) or `demo` (the simulator). The plugin plans nothing: it checks the program against the travel of the machine (`reject_out_of_travel`), counts lines for the progress and sends it through the same G-code runner `bungard` uses, so a held job resumes from its line. The job can be held and resumed, and a stop sends the controller's feed hold and reset.
* Routes: the generic ones, plus `GET /work/machines/{id}/position` (viewer, `machines:view`: the machine and work position, the state word of the controller, the override values) and `POST /work/machines/{id}/jog` (operator, `jobs:start`: `{"axis", "distance_mm", "feed"}`, refused while a job runs).
* States: `idle`, `busy`, `alarm` (shown as `busy` with `detail.alarm` until cleared by `POST /work/machines/{id}/unlock`, operator).
* Settings (defaults in brackets): `travel_x_mm`, `travel_y_mm`, `travel_z_mm` (the machine's own, 0 to 5000; unset means the check is off and the response says so), `jog_max_mm` (50, 0.01 to 500), `jog_max_feed` (1000 mm/min, 1 to 30000), `spindle_max_rpm` (12000, 0 to 65535), `reject_out_of_travel` (on), `resume_safe_z_mm` (5), `step_ms` of the simulator (20).
* Codes to add: `bad_gcode`, `out_of_travel`, `machine_alarm`, `jog_while_running`, `bad_axis`, `bad_feed`.

### `pnp`

For PHOENIX-PNP, a pick-and-place machine. Request kinds: `place` (`{"kind": "place", "job": "<artifact id of the placement list>", "board": {...}, "options": {"dry_run": true}}`) and `calibrate` (`{"kind": "calibrate", "what": "nozzle|camera|feeder", "target": "..."}`). A placement list is an artifact of the store (`csv` or `json`, the positions, rotations, packages and feeders). The plugin checks the list against the feeders and nozzles the machine has declared, makes the job ready with the count of components (`total`) and runs it step by step: pick, check, place; progress counts components. The job can be held and resumed between two components.
* Routes: the generic ones, plus `GET /work/machines/{id}/feeders` (viewer: each feeder with its component, remaining count and state), `PUT /work/machines/{id}/feeders/{feeder}` (operator, `jobs:prepare`: declare or refill) and `GET /work/machines/{id}/vision/last` (viewer: the last picture check, as the artifact id of the picture and the verdict).
* States: `idle`, `busy`, `waiting` (a dialog of the operator: a feeder is empty, a component failed its check).
* Settings: `feeders_max` (60, 1 to 200), `nozzles` (list, empty), `place_height_mm` (0.2, 0 to 5), `pick_retries` (2, 0 to 5), `vision_check` (on), `dry_run_default` (on: a new job moves nothing until the person turns it off), `step_ms` of the simulator.
* Codes to add: `bad_placement_list`, `unknown_feeder`, `feeder_empty`, `no_nozzle`, `vision_failed`, `component_missing`.

### `laser`

For PHOENIX-LASER, a laser cutter or engraver. Request kinds: `gcode` and `vector` (`{"kind": "vector", "artifact": "<id>", "operations": [{"layer", "mode": "cut|engrave", "power_pct", "speed", "passes"}]}`). The plugin enforces power limits in the server (a request above `power_max_pct` is refused, never clamped silently), keeps an emergency stop latched until an administrator releases it, and reports the limits in force in `detail`.
* Routes: the generic ones, plus `POST /work/machines/{id}/laser/test-fire` (operator, `jobs:start`: a short pulse at `power_pct` no higher than `test_fire_max_pct`, refused while a job runs, never when the enclosure input says open) and `POST /work/machines/{id}/laser/release` (administrator, `machines:manage`: releases a latched stop).
* States: `idle`, `busy`, `latched` (shown as `busy` with `detail.latched`).
* Settings: `power_max_pct` (60, 1 to 100), `test_fire_max_pct` (1, 0 to 10), `test_fire_ms` (50, 1 to 500), `require_enclosure_closed` (on), `require_air_assist` (on), `focus_check` (on), `latch_on_door_open` (on), `step_ms` of the simulator.
* Codes to add: `power_over_limit`, `laser_latched`, `enclosure_open`, `air_assist_off`, `bad_operation`, `test_fire_refused`.

### `cam`

For ISOCAM, STENCILCAM, AUTOLEVEL, CAM-DEBUGGER and VISION: the programs that turn a design into toolpaths or check a result. It does not move a machine: it is a plugin of computed jobs, with machines of the kind `worker` (the CM5 itself, or a node that runs a tool). Request kinds: `isolate` (`{"kind": "isolate", "artifact": "<gerber>", "params": {...}}`), `stencil`, `autolevel` (a probe map from a run) and `inspect` (`{"kind": "inspect", "artifact": "<picture or toolpath>"}`); each makes an artifact of the store as its result (`result_type`), reusing the tasks and artifacts of `docs/PROJECT_STORE.md`. Progress is the share of the algorithm done. A job can be cancelled, not held.
* Routes: the generic ones, plus `GET /work/machines/{id}/tools` (viewer: which of the kinds the worker has installed and their versions).
* Settings: `max_concurrent` (1, 1 to 4), `timeout_s` (600, 10 to 86400), `max_input_mib` (64, 1 to 512), `keep_results` (50, 1 to 1000), `sandbox_dir` (the folder the tool may write in).
* Codes to add: `tool_not_installed`, `bad_params`, `input_too_large`, `cam_failed`, `cam_timeout`.

### `gerber`

A viewer and checker of Gerber and drill files, with no movement at all (no machine, no stop): the machines are `worker`s as in `cam`. Request kinds: `load` (`{"kind": "load", "artifact": "<id>"}`: reads the files, lists the layers with their function, the units, the bounding box and the apertures), `drc` (a basic design rule check against the rules of the job: minimum track, clearance, hole size, annular ring, board edge distance) and `report` (a text or JSON report as an artifact). Progress counts the checks done.
* Routes: the generic ones, plus `GET /work/jobs/{id}/layers` (viewer: the layers of a finished `load`) and `GET /work/jobs/{id}/findings` (viewer: the findings of a finished `drc`, each with its layer, position and rule).
* Settings: `min_track_mm` (0.15, 0.01 to 5), `min_clearance_mm` (0.15, 0.01 to 5), `min_hole_mm` (0.3, 0.1 to 10), `min_annular_ring_mm` (0.15, 0.01 to 2), `edge_distance_mm` (0.3, 0 to 10), `max_findings` (500, 1 to 10000), `max_input_mib` (64).
* Codes to add: `bad_gerber`, `missing_layer`, `unit_unknown`, `too_many_findings`, `bad_rules`.

### `pcb`

For PHOENIX-PCB: the design-to-board jobs that the server runs on a worker, as `cam` does, with the board as the project in the store. Request kinds: `drc` (the whole design, not only the Gerber), `export` (`{"kind": "export", "formats": ["gerber", "drill", "bom", "pnp"]}`, each an artifact of the task) and `panelize` (`{"kind": "panelize", "rows", "columns", "spacing_mm"}`). The plugin reads the project by `{app}/{project}` of `docs/PROJECT_STORE.md` and takes its lock for the time of an export, so two exports of one project cannot overlap (`409 project_locked`).
* Routes: the generic ones; the results are artifacts.
* Settings: as `gerber` for the checks, plus `panel_max_mm` (300, 10 to 1000), `export_dir_keep` (10, 1 to 100), `timeout_s` (600).
* Codes to add: `bad_project`, `export_failed`, `bad_panel`, plus those of `gerber`.

### `printer3d`

For PHOENIX-3DP (a new application, `3dp` among the names of `auth_known_apps`), a 3D printer driven by G-code with Klipper (Moonraker), Marlin (serial) or the simulator. Machines have a connection `moonraker` (`{"url": "http://host:7125", "api_key": "..."}`, the key kept like the token of a node: never shown), `marlin` (serial port and baud rate) or `demo`. Request kinds: `print` (`{"kind": "print", "artifact": "<gcode>", "preheat": true}`), `preheat` (`{"kind": "preheat", "bed_c", "hotend_c"}`) and `gcode` (a short manual command list, operator). A print can be held and resumed (pause at a layer change), cancelled, and the progress is the share of the file sent; the layer is read from the `;LAYER:` comments or from the controller's report when it gives it.
* Routes: the generic ones, plus `GET /work/machines/{id}/temperatures` (viewer: for each heater `current_c`, `target_c`, `power_pct` and the `limit_c` in force; a short history), `PUT /work/machines/{id}/temperatures` (operator, `jobs:start`: `{"heater", "target_c"}`, refused above the limit), `GET /work/machines/{id}/layer` (viewer: `layer`, `layers`, `z_mm`, `eta_s` when known) and `POST /work/machines/{id}/emergency-stop` (operator, `jobs:cancel`: heaters off and motors released at once, the machine latched until `POST /work/machines/{id}/release` by an administrator).
* States: `idle`, `busy`, `waiting` (a held print or a filament change), `latched` (after an emergency stop or a thermal fault, shown as `busy` with `detail.latched`).
* Settings, all of them limits an administrator can change and none a constant: `hotend_max_c` (260, 0 to 450), `bed_max_c` (100, 0 to 150), `chamber_max_c` (60, 0 to 100), `heat_timeout_s` (900, 30 to 3600: a heater that does not reach its target in this time stops the job), `temp_deviation_c` (15, 1 to 50: a reading this far from the target for `deviation_s` (10, 1 to 120) latches the machine), `min_temp_c` (5: a reading below it is a broken sensor), `max_feed_mm_s` (200, 1 to 500), `max_accel_mm_s2` (3000, 100 to 20000), `bed_x_mm`, `bed_y_mm`, `bed_z_mm` (the machine's own; a program that goes past them is refused), `preheat_default_hotend_c` (200) and `preheat_default_bed_c` (60), `poll_ms` (1000, 200 to 10000), `step_ms` of the simulator. A target above a limit is refused with `422 temperature_over_limit` whoever asks; a limit changed above the hard maximum written here is refused at `check_settings`.
* The G-code is read before it is accepted: a temperature command (`M104`, `M109`, `M140`, `M190`, `M141`) above a limit refuses the whole program (`422 program_over_limit`), and `M112` in a file is refused (the emergency stop is the route's).
* The simulator heats and cools with a first-order model, can inject a thermal runaway, a lost sensor and a disconnection, and counts layers from the file, so every route and every code is tested without a printer.
* Codes to add: `temperature_over_limit`, `program_over_limit`, `heater_timeout`, `thermal_runaway`, `sensor_fault`, `printer_latched`, `printer_disconnected`, `bad_heater`.

## What has been tried

Everything above runs in the tests against the `demo` plugin, the simulator of the `bungard` plugin and the simulators `laser`, `pnp` and `pcb`. Nothing has been tried with a second kind of real machine: the evidence of the three new plugins is `simulator`, and the `bungard` plugin has run on a real machine only as far as the old routes have. The jobs of `demo`, `laser`, `pnp` and `pcb` live in memory: one that was open when the server stopped is marked as failed.
