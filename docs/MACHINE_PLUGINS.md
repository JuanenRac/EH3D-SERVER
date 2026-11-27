# Machine plugins, the queue of jobs and the stream of events

The third of the four common services (`docs/SERVICES.md`). The server first knew one kind of machine, the Bungard drilling and routing machine, and its routes (`/api/v1/machines`, `/api/v1/jobs`) were written for it. An application that is not for that machine (a laser, a pick-and-place, the stencil printer) needs the same things: a machine to choose, a job to prepare, start, hold and stop, how far it is, and to be told when it ends. This service gives them without the server knowing which machine is behind.

The contract is the `work` tag of `docs/openapi.yaml`. The code is `src/plugins.rs` (the plugins), `src/work.rs` (the machines of the plugins, the queue, the history and the notifications), `src/feed.rs` (the events) and `src/api_work.rs` (the routes).

**Nothing of the first version changes.** `/api/v1/machines`, `/api/v1/jobs` and the websocket `/api/v1/events` answer as they did, with the same tests. The generic queue is a new family beside them.

## The plugin

A machine plugin is a value of a type that implements `MachinePlugin` (`src/plugins.rs`):

| Operation | What it does |
|---|---|
| `kind`, `title` | the short name (`bungard`, `demo`, `laser`, `pnp`, `pcb`, `cnc`, `gerber`, `cam`, `printer3d`, `vision`, `cad`) that `plugin` says in machines and jobs, and a line of text |
| `capabilities` | what the machines can do: the kinds of request they take, whether they can be held, how progress is counted |
| `machines` | the machines of this kind, with their state (`idle` or `busy`) |
| `prepare` | checks a request and makes a job ready on a machine; nothing moves. Gives the id the plugin knows the job by and what a client draws of it |
| `start`, `pause`, `resume`, `stop` | the operations on a job; a plugin that cannot do one answers `not_supported` |
| `status` | the state of a job (`queued`, `running`, `paused`, `done`, `failed`, `cancelled`) and how far it is (`done` of `total`) |
| `discard` | a job that was prepared and will not start: the plugin lets go of what it kept |
| `manages_machines`, `check_settings` | whether the machines of the plugin are made with `POST /work/machines`, and what their settings may be |
| `detail` | what a client sees of a machine besides its state (the latch of a laser, the feeders of a pick-and-place machine...); optional |
| `routes`, `operation` | the routes a kind adds under `/work/machines/{id}/` (a jog, a test fire, the heaters of a printer), each with its verb, its role and its scope, and the work they do; optional |
| `auto_start` | whether the queue starts the next job of a free machine by itself; a switch of the machine, off unless the machine says so |

**Simulator only.** `laser`, `pnp`, `pcb`, `cnc`, `gerber`, `cam`, `printer3d`, `vision` and `cad` have only run against the models in this repository; nothing has run on a real laser, router, pick-and-place machine, board station or printer through the server. They say so in the data: `capabilities.evidence` is `simulator` and the `detail` of each of their machines (the plugin's own detail plus the two keys the server adds) has `evidence: "simulator"` and `simulator_only: true`. A client must not show such a machine as proven on hardware; the proof on a real machine is still to do.

To add a plugin: write the type, add it to `Registry::standard()` and describe it in `docs/openapi.yaml` through `tools/make_openapi.py`. The generic layer, the history, the events, the notifications, the roles and the audit come with it.

**A file named in a request.** A request that has `artifact` (or `job` in a `place` request) names an artifact of the store: the server gives the plugin its text as `artifact_text`, if the caller sees that artifact and it is text up to 64 MiB. A text the caller puts in `artifact_text` himself is thrown away. A plugin that reads a file (G-code, Gerber, Excellon, a list of placements) takes it from there, or from the data of the request.

**Keys in the settings.** The key of the API of a printer (`connection.api_key`) is kept in the settings of its machine, because the server needs it to reach the printer, and is shown as `********` wherever the settings are shown. A change of settings that sends the stars back keeps the key.

### `bungard`

The machines of DRILLPRO and ROUTEPRO: the ones of `/api/v1/machines`, whatever their connection (a node, a GRBL controller, the simulator). `prepare` takes the same requests as `POST /machines/{id}/jobs` (`drill`, `route`, `gcode`, with the `rack` of the second version), plans them with the same libraries and puts the job in **the same book** as the old route does, so `GET /jobs/{native}` shows it and `POST /jobs/{native}/confirm` answers its dialogs. `start` and `stop` are the old start and cancel; a free G-code job can be held and resumed, a drilling or routing job cannot (`501 not_supported`). The dialogs of the operator (change the tool, switch the spindle on) show as `running` with the message `waiting for the operator (...)` and make a notification; they are confirmed with the old `/jobs/{native}/confirm`. Progress is counted in items.

The generic queue lists the jobs made through it; a job made with the old routes stays in the old book and is not in the queue.

### `demo`

A machine with no hardware, for the tests of the applications and of the server. Its machines are made with `POST /work/machines` (`"plugin": "demo"`, settings `{"step_ms": 20}`, 0 to 3600000). A job is `{"kind": "demo", "steps": 10}` (1 to 100000); it takes `step_ms` milliseconds per step, so with `step_ms = 0` it ends as soon as it runs and with a long step it stays where it is put. It can be held and resumed (the clock stops while it is held). It lives in memory: a demo job that was open when the server stopped is marked as failed.

### `laser`

Laser machines for the laser programs. It is a **simulator**: the jobs are checked and run against a model of the machine, and nothing has been tried on a real laser. Its machines are made with `POST /work/machines` (`"plugin": "laser"`). The settings are checked, and a key that is not one of these is refused:

| Setting | Meaning |
|---|---|
| `bed_x_mm` | width of the bed, 10 to 5000 (400 by default) |
| `bed_y_mm` | depth of the bed, 10 to 5000 (400 by default) |
| `max_s` | the value of S that is full power, 1 to 65535 (1000 by default) |
| `power_limit_pct` | the most power a job may ask, in percent of max_s, 1 to 100 (40 by default) |
| `over_limit` | `reject` (default) refuses a job that asks for more power than the limit, `clamp` cuts it to the limit |
| `max_feed_mm_min` | the fastest cutting feed, 1 to 100000 (6000 by default) |
| `rapid_mm_min` | the speed of a rapid move, 1 to 100000 (12000 by default) |
| `latch_on_stop` | an emergency stop latches the machine until an administrator releases it (true by default) |
| `test_fire_max_s` | the longest test fire, 0.01 to 10 seconds (1 by default) |
| `test_fire_max_pct` | the most power of a test fire, in percent of max_s, 0 to 10 (1 by default) |
| `require_enclosure_closed` | no job and no test fire while the enclosure input says open (true by default) |
| `enclosure_closed` | the input of the enclosure: true when it is closed (true by default; the simulator takes it from here) |
| `require_air_assist` | no job and no test fire while the air assist input says off (true by default) |
| `air_assist_on` | the input of the air assist: true when it blows (true by default) |
| `focus_check` | no job while the focus input says the head is not focused (true by default; a test fire does not need it) |
| `focused` | the input of the focus: true when the head is focused (true by default) |
| `latch_on_door_open` | the enclosure opened while a job runs ends the job and latches the machine (true by default) |
| `sim_rate` | how many times faster than real time the simulated machine works, 0 to 100000 (1 by default; 0 is no waiting) |

Raising `power_limit_pct` or `test_fire_max_pct`, switching `over_limit` to `clamp`, `latch_on_stop`, `latch_on_door_open`, a `require_*` or `focus_check` off loosens a protection; they are the settings of the machine and only an administrator changes them. The inputs of the enclosure, of the air assist and of the focus (`enclosure_closed`, `air_assist_on`, `focused`) are settings in the simulator, where a real machine will bring them from its pins.

A job is one of three kinds (the old kind `rearm` is gone: the latch is released by an administrator, see below):

* `{"kind": "gcode", "text": "...", "power_scale_pct": 50}` is a G-code program. It is read word by word and written again from the words that were understood (comments and anything else are dropped; `$` lines, real-time characters, `G28`, `G92`, tool changes and expressions are refused with `422 bad_gcode`, as are a word or two codes of one group written twice, a comment that is not closed, a carriage return inside a line and code after `M2`/`M30`). Every `S` is held to the power limit (`422 laser_power_limit`, or cut when the machine clamps), every feed to `max_feed_mm_min` (`422 laser_feed_limit`), every move and every arc to the bed (`422 outside_course`), and a rapid move with the beam on in constant mode (`M3`) is refused. A program that ends with the beam on gets an `M5`. `power_scale_pct` (1 to 100) lowers every `S` before the limit is looked at. The `view` has the number of lines, the length of the cut and of all the moves, the estimate in seconds, the highest `S`, how many were cut, the box of the cut and a drawing of at most 2000 points. The head starts where the machine left it.
* `{"kind": "vector", "paths": [{"layer": "cut", "points": [[10, 10], [30, 10]], "closed": true}], "operations": [{"layer": "cut", "mode": "cut", "power_pct": 40, "speed": 600, "passes": 2}]}` makes a G-code program from the paths and the operations (`mode` is `cut` or `engrave`, `speed` in mm/min) and reads it like any other, so the limits of power, feed and bed hold. The paths and the operations may be the JSON of an artifact (`artifact`). A bad operation is `422 bad_operation`. The formats of a vector drawing (SVG, DXF) are not read yet.
* `{"kind": "test_fire", "power_pct": 1, "seconds": 0.5}` is a short pulse to focus, with the limits of a program and of `test_fire_max_pct` (`422 test_fire_refused` above it).

**The emergency stop** is `POST /work/jobs/{id}/cancel` on a job that runs or is held: the job ends at once, the beam goes off and, unless `latch_on_stop` is off, the machine is **latched**. A latched machine refuses to prepare or start any job (`409 laser_estop_latched`) until an administrator releases it with `POST /work/machines/{id}/laser/release` (an operator cannot). An enclosure opened while a job runs ends the job and latches the machine too (`latch_on_door_open`). A job can be held and resumed (the beam is off while it is held). Progress is counted in lines. `GET /work/machines/{id}` has `detail`: `estop_latched`, `position_mm`, `beam_on`, the power in force and the bed.

**The routes of the laser.** `POST /work/machines/{id}/laser/test-fire` (operator, `jobs:start`) fires a pulse `{"power_pct", "seconds"}` at once, with no job: refused while a job runs (`409 machine_busy`), while the machine is latched, and while the enclosure input says open (`409 enclosure_open`) or the air assist input says off (`409 air_assist_off`) when the machine requires them. A program or a vector job is refused the same way, and also when the focus input says the head is not focused (`409 focus_not_checked`). `POST /work/machines/{id}/laser/release` (administrator, `machines:manage`) releases the latch.

### `pnp`

Pick-and-place machines for the pick-and-place program. A **simulator**; nothing has been tried on a real machine. The machines are made with `POST /work/machines` (`"plugin": "pnp"`). Settings: `feeders` (slots, 1 to 200, 40 by default), `bed_x_mm`, `bed_y_mm`, `feeder_x0_mm`, `feeder_pitch_mm`, `feeder_y_mm` (where the slots are picked from), `travel_mm_s`, `pick_ms`, `place_ms`, `max_placements`, `require_stock`, `sim_fail_pick_every` and `sim_rate`. A machine has feeders in numbered slots, each with a part (its name) and how many are left.

* `{"kind": "load_feeders", "feeders": [{"slot": 1, "part": "R_0603_10k", "count": 150}], "replace": false}` says what is in the slots, as a person put it there; `replace: true` empties the slots that are not in the list (an empty list with `replace: true` empties them all).
* `{"kind": "place", "placements": [{"ref": "R1", "part": "R_0603_10k", "x": 20, "y": 30, "rot": 90, "side": "top"}], "offset": {"x": 0, "y": 0}}` places parts. It is checked when it is prepared: the references are different, every place (moved by the offset) is on the bed (`422 outside_course`), and every part is in a feeder with enough left (`409 pnp_feeder_short`; a machine with `require_stock` off lets a job start whose feeders hold too few, and the job fails when one runs dry). The parts are taken from the first slot that has some left. The `view` has the placements with their slot, the count of each part, the travel and the estimate.

The placements may be an artifact of the store: `{"kind": "place", "job": "<artifact id>"}`, a JSON list (or an object with `placements`) or a CSV with a header row whose columns are `ref` (or `designator`), `part` (or `val`, `value`), `x` (or `posx`), `y` (or `posy`) and, if they are there, `rot` and `side`; one that does not read is `422 bad_placement_list`. A request does not bring the placements as data and as an artifact at once.

**Dry run.** A `place` request with `"dry_run": true` (or `"options": {"dry_run": true}`) is planned and checked like any other, and with the same demand on the stock, and then the simulated head goes through it without taking a part off a feeder, without losing the vacuum and without counting a placement in `placed_total`; the `view` says `dry_run`. A value that is not true or false is `422 bad_plugin_request`.

**Calibrate.** `{"kind": "calibrate", "what": "feeder" | "nozzle" | "camera", "target": "3", "offset_mm": {"x": 0.5, "y": -0.25}}` is a job like the others (it is prepared and started). The target of a feeder is its slot; the one of a nozzle or a camera is a name. The offset (up to 5 mm on each axis, 0 when it is not given) is the one the request brings: the simulated machine measures nothing. The offset of a feeder is added to the place the head picks from it, in the plan of every job that starts afterwards and in `detail.feeders`; `0` and `0` puts it back. Each calibration is kept in `detail.calibrations` (`what:target`, with `evidence: simulator`).

**The feeders.** `GET /work/machines/{id}/feeders` (viewer, `machines:view`) lists the slots that hold a part, with where each is picked from and its calibration offset, and the free slots. `PUT /work/machines/{id}/feeders` (operator, `jobs:prepare`) declares or refills one slot at once, with no job: `{"slot": 3, "part": "R_0603_10k", "count": 150}`, or empties it: `{"slot": 3, "clear": true}`. It is refused while a job runs on the machine (`409 machine_busy`) and an answer that does not fit is `422 bad_plugin_request`.

The simulated head takes the part off its feeder when it picks, so a feeder that is empty stops the job as `failed` (`feeder 3 is empty at R12`), and a machine with `sim_fail_pick_every` set to N loses the vacuum at the Nth placement of a job (the part is gone and the job fails there) to try how a program deals with it. The feeders are looked at again when the job starts, so a table changed in between is not trusted. `detail` has the `feeders` (with where each is picked from), the `head`, `placed_total` and `failed_picks`. Progress is counted in placements.

**The nodes of a machine.** The feeder node and the head node that stand behind a `pnp` machine are registered with `POST /api/v1/pnp/nodes` (`docs/API.md`, off until `pnp_nodes_enabled`) and linked to the machine by its id. While a node of the machine that is switched on is not ready (never asked, unreachable, silent, refusing the token, safe or failing its own check) a job of the machine is refused at start with `409 pnp_node_not_ready`, and `detail.nodes` lists them with their health. A machine with no node registered behaves as before. Simulator only, like the rest of the plugin: the nodes are asked in the tests as servers in memory.

### `pcb`

Board stations that make reports and checks and move nothing. A **simulator** of the time a station takes; the reports themselves are made from the data the job brings. The machines are made with `POST /work/machines` (`"plugin": "pcb"`). Settings: the rules of the maker `min_track_mm`, `min_gap_mm`, `min_drill_mm`, `max_drill_mm` and `min_edge_mm`, `drill_hit_s`, `max_items`, `fail_on_violation`, `item_ms` and `sim_rate`.

* `rules_check`: `{"board": {"outline": {"width_mm", "height_mm"}, "tracks": [{"width_mm"}], "clearances": [{"gap_mm"}], "drills": [{"dia_mm", "x", "y"}]}, "rules": {...}}` against the rules (the request may put its own over the ones of the station). It lists every `track_width`, `clearance`, `drill_small`, `drill_outside` and `drill_edge` error and every `drill_large`, `drill_duplicate` and `no_outline` warning with the item, the value and the limit, and gives the verdict `pass` or `fail`.
* `drill_report`: the tools (holes of the same size), how many hits and how many are plated, the travel and an estimate of the time.
* `bom_report`: the parts grouped by value and footprint, the references that repeat (error) and the parts with no value or footprint (warning).
* `inspect`: `{"measurements": [{"name", "nominal", "tolerance", "value"}]}` each judged against its tolerance.

The report is in the `view` of the job as soon as it is prepared; the job then runs for the time the station takes to go over its items. A job whose report has errors ends `done` with the verdict `fail`, unless the station has `fail_on_violation`, and then it ends `failed`. `detail` has how many jobs were done, the kind and the verdict of the last one and the rules in force. Progress is counted in items.

### `cnc`

CNC machines: routers, mills and engravers driven by G-code. A **simulator**: the program is read against the machine and run in a model, and nothing has been tried on a real machine. The machines are made with `POST /work/machines` (`"plugin": "cnc"`). The settings are checked, and a key that is not one of these is refused:

| Setting | Meaning |
|---|---|
| `connection` | how the machine is reached: {"kind": "demo"} (the simulator, the default); `grbl` and `node` are not served yet |
| `travel_x_mm` | the travel of X, 0 to 5000: a move outside 0 to this is refused (not set: the check is off and the job says so) |
| `travel_y_mm` | the travel of Y, 0 to 5000: a move outside 0 to this is refused (not set: the check is off) |
| `travel_z_mm` | the travel of Z, 0 to 5000: a move outside minus this to plus this is refused (not set: the check is off) |
| `reject_out_of_travel` | a program that leaves the travel is refused (true by default); false lets it in and the job lists the lines |
| `jog_max_mm` | the longest jog, 0.01 to 500 (50 by default) |
| `jog_max_feed` | the fastest jog feed in mm/min, 1 to 30000 (1000 by default) |
| `spindle_max_rpm` | the most turns a program may ask of the spindle, 0 to 65535 (12000 by default) |
| `rapid_mm_min` | the speed of a rapid move for the estimate, 1 to 100000 (3000 by default) |
| `latch_on_stop` | an emergency stop latches the machine until an administrator releases it (true by default) |
| `step_ms` | the milliseconds one line takes in the simulator, 0 to 3600000 (20 by default; 0 ends a job as soon as it runs) |

A job is `{"kind": "gcode", "text": "...", "skip": 0, "restore_outputs": false}` or `{"kind": "file", "artifact": "<id>"}` (a G-code artifact of the store). The program is cleaned by the same reader the `bungard` plugin uses (`422 bad_gcode`, `422 too_big`, `422 no_work`), then every move is looked at against the travel: an axis with its travel set is held to `0` to the travel (Z from minus the travel to the travel), an arc is looked at along its whole way and not only at its ends, and a move outside is `422 out_of_travel` (or listed in the job when `reject_out_of_travel` is off). `S` above `spindle_max_rpm` is `422 spindle_limit`. `G28`, `G30`, `G10` and `G92` are refused, because the server cannot follow where they put the coordinates, and so is an arc outside the XY plane (after `G18` or `G19`). The job can be held and resumed, a stop ends it (feed hold and reset) and does not latch the machine. Progress is counted in lines, each taking `step_ms` in the simulator. The `view` has the number of lines, the moves, the box, the travel, the estimate, the highest feed and turns, and a drawing of at most 2000 points.

**Compatibility routes (Moonraker and OctoPrint).** A program written for Moonraker or OctoPrint can read the state of a 3D printer of the plugin `printer3d` at the root of the address, without knowing this interface. They are read-only and off by default, and three things must be true for one of them to answer:

1. the switch `compat_api` of the security page is on (`PUT /api/v1/auth/policy`, `{"key": "compat_api", "value": true, "confirm_weak": true}`; or `compat_api = true` in `server.cfg`); while it is off every one of them is `404 compat_off`;
2. the machine has an API key: an administrator makes it with `POST /api/v1/work/machines/{id}/compat/key` (shown once; the server keeps only its SHA-256 in `compat-keys.json` of the data folder; a new one replaces the old; `DELETE` takes it away; it goes with the machine). The program sends it in the header `X-Api-Key`; a missing or wrong key is `401 unauthorized`, and the key of a machine opens that machine and no other (and nothing of `/api/v1/` besides);
3. the machine is set to give the shape (its setting `compat`: `moonraker`, `octoprint` or `both`; `off` by default), else `501 not_supported`.

Moonraker: `GET /server/info`, `GET /printer/info` (`state` is `ready` or `shutdown`, `hostname` is the id of the machine) and `GET /printer/objects/query` (the heaters, `print_stats`, `virtual_sdcard` and `webhooks`). OctoPrint: `GET /api/version`, `GET /api/printer` and `GET /api/job`. These routes read; the routes that write are the next paragraph. G-code sent by hand and the websocket are not served (use `/api/v1/work/`). They are not in `docs/openapi.yaml`, which describes `/api/v1/` only. What they say comes from the state of the printer; nothing is made up, and a printer that has only run in the simulator is told so in the detail of its machine like every simulated one.

**Compatibility routes that write (Moonraker and OctoPrint).** A second group of routes lets a program written for those two do what it does most: send a file and say pause, resume or cancel. They have a switch of their own, **off by default**, and the three conditions above plus two more:

4. the switch `compat_write` of the security page is on (`PUT /api/v1/auth/policy`, `{"key": "compat_write", "value": true, "confirm_weak": true}`; or `compat_write = true` in `server.cfg`); while it is off each of them is `404 compat_write_off`, and when `compat_api` is off they are `404 compat_off` like the reads;
5. the key of the machine was made to write: `POST /work/machines/{id}/compat/key` with `{"write": true}` (a key made without it, which is the default, only reads; a new key replaces what the old one could do). A key that does not write is `403 forbidden` on these routes.

The work is done as an operator that exists only for that machine (`compat:<machine>`): its jobs are its own, it sees nothing of the rest of the interface, and every write is a line of the audit (`compat_write`: the machine, what and the file). The routes: Moonraker `POST /printer/print/pause`, `/resume`, `/cancel`, `/printer/print/start?filename=` and `POST /server/files/upload`; OctoPrint `POST /api/files/local` and `POST /api/job` (`{"command": "start"}`, `{"command": "cancel"}`, `{"command": "pause", "action": "pause" | "resume" | "toggle"}`; the answer is `204`). Each answers only for the shape the machine gives (`compat`), else `501 not_supported`.

**Nothing starts by itself.** An uploaded file (`multipart/form-data`, the part `file`; `print` and `select` in the body are read as a wish and not obeyed) is checked against the limits of the model and prepared like any job, and **waits in the queue of the printer** for a person to start it with `POST /work/jobs/{id}/start`, which is the manual confirmation of the plugin; a file with the name of one that waits replaces it. A program may start a waiting print itself only when the machine says so with its setting `compat_start` (false by default: the start then is `403 forbidden`). Pause, resume and cancel act on the job that runs or is held on the machine (`409 not_in_state` when there is none). The OpenAPI document does not describe these routes, as it does not the reads.

**The routes of the CNC machine.** `GET /work/machines/{id}/position` (viewer, `machines:view`): the machine and work position, the state word (`Idle`, `Run`, `Alarm`), the turns of the spindle and the overrides. `POST /work/machines/{id}/jog` (operator, `jobs:start`): `{"axis", "distance_mm", "feed"}`, inside `jog_max_mm` and `jog_max_feed` and inside the travel, refused while a job runs (`409 jog_while_running`) and while the machine is latched. `POST /work/machines/{id}/emergency-stop` (operator, `jobs:cancel`, no body): the job that runs ends at once, the spindle goes off and the machine is latched. A latched machine refuses to prepare, start or jog (`409 machine_alarm`) until `POST /work/machines/{id}/release` (administrator, `machines:manage`). The connections `grbl` and `node` are not served yet; the machine is the simulator.

### `gerber`

Workers that read Gerber and drill files and check a board; they move nothing. A **simulator** of the time of the checks; the reports are made from the files the job brings. The machines are made with `POST /work/machines` (`"plugin": "gerber"`):

| Setting | Meaning |
|---|---|
| `min_track_mm` | the narrowest track, 0.01 to 5 (0.15 by default) |
| `min_clearance_mm` | the least gap between two pads of one layer, 0.01 to 5 (0.15 by default) |
| `min_hole_mm` | the smallest hole, 0.1 to 10 (0.3 by default) |
| `min_annular_ring_mm` | the least ring of copper around a hole, 0.01 to 2 (0.15 by default) |
| `edge_distance_mm` | the least distance from a hole or a piece of copper to the edge of the board, 0 to 10 (0.3 by default) |
| `max_findings` | the most findings a report lists, 1 to 10000 (500 by default) |
| `max_input_mib` | the most text a job may bring, 1 to 64 MiB (64 by default) |
| `item_ms` | the milliseconds one check takes in the simulator, 0 to 3600000 (0 by default: the job ends as soon as it runs) |

The files come in the request (`"files": [{"name": "board.gtl", "text": "..."}]`, up to 64) or are one artifact (`"artifact"`). The readers are small: RS-274X with its format, units, apertures (`C`, `R`, `O`, `P`), draws, flashes, regions and `%TF.FileFunction`, and Excellon with its tools and hits; the function of a file comes from its attribute or from its name (`.gtl`, `-F_Cu.gbr`, `-Edge_Cuts.gbr`, `.drl`...). An arc is taken as the straight line between its ends and an aperture made by a macro has no size, so the checks that need a size skip it. A file that does not read is `422 bad_gerber` with its name, one that uses coordinates before it says its units is `422 unit_unknown`, and a check with no copper layer or a job with no file is `422 missing_layer`.

* `{"kind": "load", ...}` lists the layers (function, units, box, apertures, tracks, pads, regions, holes, tools) and the size of the board.
* `{"kind": "drc", ..., "rules": {...}}` goes over the rules (the request may put its own over the worker's; one out of its range is `422 bad_rules`): `track_width`, `clearance` (between pads of one layer), `track_clearance` (between two tracks of one layer, from the copper edge of one to the copper edge of the other; tracks that touch or cross are taken as one net, because the check has no nets, and a layer with more than 2500 tracks gets the warning `track_clearance_skipped` instead), `hole_small`, `hole_outside`, `hole_edge`, `annular_ring` (against the pad at the same place), `copper_edge`, and the warnings `no_outline` and `clearance_skipped`. The verdict is `pass` or `fail`; findings past `max_findings` are counted and the list says it is cut.
* `{"kind": "report", ..., "format": "text" | "json"}` is both, with the text or the data of the report in the `view`.

Progress counts the checks done (one for `load`, six for the others), each taking `item_ms`. The result is in the `view` of the job; the files are not kept.

**The layers and the findings of a job.** `GET /work/jobs/{id}/layers` (viewer, `machines:view`) gives the layers a gerber job read and the size of the board, and `GET /work/jobs/{id}/findings` (viewer) gives the findings of a `drc` or `report` job a page at a time: `?severity=error|warning`, `?rule=`, `?layer=` (the name of the file), `?limit=` (1 to 1000, 200 by default) and `?offset=`; the answer says how many findings matched and whether there are more. A job of another plugin is `501 not_supported`, a `load` job has no findings (`409 not_in_state`) and a job that is not there is `404 unknown_job`.

### `cam`

Workers that make G-code and check a program; they move nothing. The machines are made with `POST /work/machines` (`"plugin": "cam"`):

| Setting | Meaning |
|---|---|
| `max_depth_mm` | the deepest cut a job may ask, 0.01 to 20 (3 by default) |
| `max_feed_mm_min` | the fastest feed a job may ask, 1 to 30000 (3000 by default) |
| `max_spindle_rpm` | the most turns a job may ask, 0 to 65535 (30000 by default) |
| `max_safe_z_mm` | the highest safe height a job may ask, 0.1 to 100 (50 by default) |
| `travel_x_mm` | the travel of X, 0 to 5000: the work has to fit 0 to this (not set: not checked) |
| `travel_y_mm` | the travel of Y, 0 to 5000 (not set: not checked) |
| `max_gcode_lines` | the most lines of G-code a job may make or check, 100 to 1000000 (200000 by default) |
| `max_grid_cells` | the most cells of the grid of an isolation, 10000 to 20000000 (4000000 by default) |
| `max_input_mib` | the most text a job may bring, 1 to 64 MiB (64 by default) |
| `item_ms` | the milliseconds one step takes in the simulator, 0 to 3600000 (0 by default: the job ends as soon as it runs) |

**The program as an artifact.** A request with `"output": "artifact"` (and, if wanted, `"artifact_name"`, the kind of the job and `.nc` by default) keeps the G-code the job made as an artifact of the caller's store (`format` `text/x-gcode`, the producer `eh3d-server cam`, the artifact it was made from as its input) and the `view` has `artifact` (`artifact_id`, `blob_sha256`, `size`, `name`, `format`) instead of the text of the program; `"output": "view"` is the default. A job that makes no program (`verify`, `inspect`, a probe map) refuses it, and so does any other value (`422 bad_plugin_request`).

Everything a job asks is held to the limits of the worker and a request over one is `422 bad_params`, never cut down. The files are read as in `gerber` (inline or an artifact). `params` take `depth_mm`, `feed_mm_min`, `spindle_rpm` and `safe_z_mm` in every kind.

* `{"kind": "drill", ..., "params": {"peck_mm", "offset_x_mm", "offset_y_mm", "tool_change": "pause" | "none"}}` drills the holes of the Excellon file, tool by tool from the smallest, nearest hole first, with an `M0` to change the drill; the box of the holes has to fit the travel (`422 out_of_travel`).
* `{"kind": "outline", ..., "params": {"tool_dia_mm", "pass_depth_mm"}}` cuts the outside of a board whose outline layer is a rectangle (any other shape is `422 bad_params`), in passes.
* `{"kind": "isolate", "layer": "...", ..., "params": {"tool_dia_mm", "passes", "overlap_pct", "resolution_mm"}}` puts the copper of the layer on a grid, grows it by the radius of the tool and follows the edge of what it grew to: the path keeps at least the radius of the tool from the copper, and is a few cells farther than that. A layer with copper pours or with pads made by a macro is refused, because the grid would leave them out; a grid over `max_grid_cells` is `422 too_big`.
* `{"kind": "stencil", "files": [...], "layer": "...", "params": {"tool_dia_mm", "shrink_pct", "mirror", "skip_small"}}` cuts the openings of a stencil from a paste layer: for every pad the path of the center of the tool runs inside the edge of the pad by the radius of the tool (a rectangle for a rectangular pad, a full circle with `G2` for a round one), `shrink_pct` (0 to 30) makes every opening smaller, `mirror` mirrors the foil around the middle of the layer, and the depth is the thickness of the foil (`depth_mm`, 0.12 by default). A pad that is smaller than the tool, or made by a macro, is `422 bad_params`; with `skip_small` the pads that are too small are left out and counted in `skipped_too_small`, which makes a stencil with fewer openings. The pads are cut nearest first.
* `{"kind": "autolevel", "params": {"x_mm", "y_mm", "width_mm", "height_mm", "columns", "rows", "depth_mm", "probe_feed_mm_min"}}` makes the program that probes a grid over the board (`G38.2`, row by row and back, 2 to 50 points on a side, inside the travel, with no spindle). With `"mode": "map"` and `"heights"` (the rows of the heights the probe found, as many numbers per row as `columns` and as many rows as `rows`) it gives instead the account of the map: lowest, highest, mean and range, the tilt in X and Y, the height in the middle by linear interpolation, and `flat` or `uneven` against `tolerance_mm` (0.1 by default). It does not change a program for the map.
* `{"kind": "inspect", "text": "..."}` (`artifact` for a G-code artifact; a picture is not read) gives the account of a toolpath without judging it: the cutting and rapid lengths, the plunges, the length and the number of moves at each depth (`levels`, up to 50), the moves that cut above the surface (`air_moves`), the time estimate and the bounds.
* `{"kind": "verify", "text": "..."}` reads a program that somebody else made (`artifact` for a G-code artifact) and reports its box, its moves, its feed and turns, and the findings `feed_over_limit`, `rpm_over_limit`, `too_deep`, `rapid_into_material`, `cut_with_spindle_off`, `out_of_travel`, `no_program_end` and `spindle_left_on`, with the verdict. The motion and the unit in force count for the lines that do not name them (a feed in inches is judged in mm/min). A program the controller could not read, or would read in another way, is not judged but refused (`422 bad_gcode`, as in `docs/API.md`).

The G-code and the report are in the `view` (`gcode`, `result`). `GET /work/machines/{id}/tools` (viewer) lists what the worker has built in. A CAM job can be cancelled and not held.

### `printer3d`

3D printers: a queue of prints that a person starts, the limits of temperature of each model, the heaters watched while they work, and an emergency stop that latches. The printer behind a machine is the **simulator** (a first-order model of the heaters that can be made to fail), **Klipper through Moonraker** or **OctoPrint**; the last two have only been tried against servers that live in memory and a socket of the computer the tests run on, never against a real printer. Two more connections are simulators only: a **Marlin** printer on a simulated serial line (`marlin`) and, as an **experiment**, a **Bambu Lab printer in LAN mode** (`bambu_lan`). The machines are made with `POST /work/machines` (`"plugin": "printer3d"`):

| Setting | Meaning |
|---|---|
| `model` | the model: generic (the default), bambu_a1, bambu_a1_mini or bambu_p1s; its profile gives the limits of temperature |
| `connection` | how the printer is reached: {"kind": "demo"} (the simulator, the default), {"kind": "moonraker", "url": "http://host:7125", "api_key": "..."} or {"kind": "octoprint", "url": ..., "api_key": ...}; the key is never shown again; {"kind": "marlin", "port": "sim", "baud": 115200} is a Marlin printer on a serial line (only the simulated line is served); {"kind": "bambu_lan", "host": "sim", "serial": "...", "fingerprint": "<sha256 of the certificate>", "api_key": "<access code>"} is a Bambu Lab printer in LAN mode, an EXPERIMENT: host "sim" is the simulated printer and the address of a real printer is reached only when the server allows it (`bambu_lan_enabled`, off by default); a certificate is never accepted unless it is the pinned one |
| `hotend_max_c` | the hottest the hotend may be asked, 0 to 450 (the profile of the model by default) |
| `bed_max_c` | the hottest the bed may be asked, 0 to 150 (the profile by default) |
| `chamber_max_c` | the hottest the chamber may be asked, 0 to 100 (the profile by default; 0 is a printer with no chamber heater) |
| `heat_timeout_s` | a heater that does not get to its target in this time stops the job, 30 to 3600 (900 by default) |
| `temp_deviation_c` | a heater this far from its target for deviation_s latches the machine, 1 to 50 (15 by default) |
| `deviation_s` | how long the deviation lasts before it latches, 1 to 120 (10 by default) |
| `min_temp_c` | a reading below this is a broken sensor, -50 to 50 (5 by default) |
| `max_feed_mm_s` | the fastest feed a program may ask, 1 to 500 (200 by default) |
| `bed_x_mm` | the bed along X, 10 to 2000: a program that goes past it is refused (not set: not checked) |
| `bed_y_mm` | the bed along Y, 10 to 2000 (not set: not checked) |
| `bed_z_mm` | the height of the printer, 10 to 2000 (not set: not checked) |
| `preheat_default_hotend_c` | the hotend target of a preheat that names none, 0 to 450 (200 by default) |
| `preheat_default_bed_c` | the bed target of a preheat that names none, 0 to 150 (60 by default) |
| `poll_ms` | how often the printer is asked, 200 to 10000 (1000 by default) |
| `auto_start_queue` | the next queued job starts by itself when the printer is free (false by default: a person starts every print) |
| `compat` | the answers shaped like another program's (GET .../compat/moonraker and .../compat/octoprint): off (the default), moonraker, octoprint or both |
| `compat_start` | a program that uses the write routes of Moonraker or OctoPrint may start a print that waits in the queue (false by default: a person starts it with the routes of the server; pausing and cancelling work for any key that may write) |
| `step_ms` | the milliseconds one line of a program takes in the simulator, 0 to 3600000 (20 by default) |
| `sim_rate` | how many times faster than real time the simulator works, 0 to 100000 (1 by default; 0 is no waiting) |
| `sim_fault` | makes the simulator fail: none (the default), thermal_runaway, sensor_lost or disconnect |

The profiles are the limits of temperature the makers publish: `generic` 260 C hotend, 100 C bed, 60 C chamber (the conservative limits of the server), `bambu_a1` 300/100/none, `bambu_a1_mini` 300/80/none and `bambu_p1s` 300/100/none (hotend, bed and chamber; the P1S chamber has no heater). The three Bambu lines were compared with the public specifications of the maker (hotend 300 C on the three; bed 100 C on the A1 and the P1S, and 80 C on the A1 mini, as the maker's specification sheet and FAQ say; the community has reported that the A1 mini reaches 100 C, so a person who owns one and wants that sets `bed_max_c` by hand). The settings `hotend_max_c`, `bed_max_c` and `chamber_max_c` change a profile up to a hard maximum (450, 150 and 100 C).

**Marlin on a serial line (simulated).** The driver (`src/printer_lan.rs`, the firmware in memory in `src/marlin.rs`) speaks the protocol of Marlin: it reads the answer to `M105` for the heaters and to `M27` for the progress of a print from the card, writes the program to the card (`M28`, `M29`), selects it (`M23`) and starts, holds and aborts it (`M24`, `M25`, `M524`); `M112` halts the firmware, whose state then reads `emergency` until a reset (`M999`). The firmware in memory refuses a target over its own maximum, answers `Resend` to a numbered line whose checksum is wrong and can fail the way the other simulators do (`sim_fault`). Only the port `sim` is served; the layers of a print are counted from the `;LAYER_CHANGE` marks in the bytes the card counts.

**Bambu Lab in LAN mode (an experiment, simulated).** The driver reads what the printer reports over MQTT (`gcode_state`, the temperatures, `mc_percent`, `layer_num`) as the normalized state, gives it a file over FTPS and sends it `pause`, `resume`, `stop`, `gcode_line` and `project_file` (`src/bambu.rs` has the printer in memory). **The chain of trust is explicit.** A Bambu printer shows a certificate it signed itself, so there is no authority to ask; the server never accepts a certificate unchecked. The connection names the SHA-256 fingerprint of the certificate it expects (`connection.fingerprint`, 64 hexadecimal digits with or without colons); before anything else is sent the driver compares it with the one the printer shows, and a printer that shows another, or a connection with none pinned, is refused with `502 printer_untrusted`. The refusal without a pin says the fingerprint the printer showed, so its owner can compare it with the one the printer gives and set it; the server does not pin it by itself. The access code of the printer goes in `connection.api_key` and is never shown again. `detail.bambu_lan` says `experimental: true` and whether a fingerprint is pinned. **The real link.** With the switch `bambu_lan_enabled` of the security page (and `server.cfg`) on, a machine whose `connection.host` is the address of a printer is reached over MQTT on TLS (port `bambu_lan_port`, 8883 by default; the user `bblp` and the access code of the printer): `src/tls_pin.rs` makes the connection and compares the SHA-256 fingerprint of the certificate the printer shows with `connection.fingerprint` in constant time, and nothing is written to a connection whose certificate is not the pinned one (the checks of the library on the signature of the handshake stay on; what is not checked is the chain of authorities, the name and the dates, because a pinned certificate is the identity). With the switch off the answer is `409 bambu_lan_off`; with no fingerprint pinned, or another, `502 printer_untrusted` with the fingerprint the printer showed; a printer that does not answer, `502 printer_disconnected`. The link reads the reports of `device/<serial>/report` (a few fields at a time, merged) and sends `pause`, `resume`, `stop` and one line of G-code on `device/<serial>/request`; **starting a file is not served**, because it needs the file to be given over FTPS, which is not written (a print on a real printer is refused with `502 plugin_failed` and the reason). The limits of temperature of the model (`bambu_a1` 300/100, `bambu_a1_mini` 300/80, `bambu_p1s` 300/100, each editable with `hotend_max_c` and `bed_max_c` up to the hard maximum) are held before a line is sent. All of this has been tried only against a printer in memory behind a TLS server the tests make: the machine says `evidence: simulator`, and nothing has run on a real A1, P1S or other Bambu printer. An emergency stop stops the print and switches the heaters off, because the LAN interface has no emergency stop of its own.

**A farm of printers.** `POST /api/v1/work/farm/jobs` (operator, `jobs:prepare`) takes `{"request": {...}, "copies": 1, "machines": ["p1", "p2"], "name", "app"}` and shares the print among the 3D printers that are **free** (not running or held, not latched and with no job waiting for a person), in the order of the list or of the machines. Each copy goes to the first free printer that accepts it, and a printer that refuses it because of the limits of its model is skipped and told in `tried`; the copies that find no printer are counted in `unplaced`, and `409 farm_no_free_printer` says that none could be placed. Every copy **waits in the queue of its printer** for a person to start it with `POST /work/jobs/{id}/start`: the farm starts nothing (a printer whose own setting `auto_start_queue` is on starts its queue as it always does).

A job is `{"kind": "print", "text": "...", "preheat": false}` (or `"artifact"`), `{"kind": "preheat", "hotend_c": 200, "bed_c": 60}` (no number names the defaults) or `{"kind": "gcode", "text": "M104 S150"}` (a short list of up to 50 commands). A program is read before it is accepted: a temperature (`M104`, `M109`, `M140`, `M190`, `M141`, `M191`) over a limit refuses the whole program (`422 program_over_limit`), so does a feed over `max_feed_mm_s`; `M112` is `422 bad_gcode` (the emergency stop is a route), a move past the bed is `422 out_of_travel`. The `R` of `M109`, `M190` and `M191` and the `S` of `M303` are temperatures too, a feed over the limit is refused also on a line that only sets it, and the end of an arc is held to the bed. A line that moves or heats is refused (`422 bad_gcode`) when it cannot be read whole, says a word twice or has parentheses, which a firmware need not take for a comment; so are a carriage return inside a line, control characters, and `M23`, `M28`, `M29` and `M32`, which write or start a file of the card that the server cannot judge. Commands with names of their own (the macros of Klipper) are not judged: their limits are those of the firmware. Layers are counted from `;LAYER_CHANGE` and `;LAYER:` comments. A print is **queued** and a person starts it; the queue starts the next job of a free printer by itself only when the machine has `auto_start_queue` (off by default). A print can be held, resumed and stopped (the print is cancelled and the heaters go off); progress is the percent of the program. States are `idle` and `busy` (a latched machine is `busy` with `detail.latched`).

**The heaters are watched** every time the printer is asked (every half second while a job runs): a reading below `min_temp_c` is a broken sensor (`sensor_fault`), a reading past the limit by more than 10 C or a heater `temp_deviation_c` from its target for `deviation_s` after it had got there is `thermal_runaway`, a target the printer has past the limit (set from outside the server) is switched off, and each of those stops the printer, fails the job and latches the machine. A heater that does not get to its target in `heat_timeout_s` fails the job (`heater_timeout`) without latching. A printer that does not answer three polls in a row fails the job (`printer_disconnected`). The simulator takes the faults `thermal_runaway`, `sensor_lost` and `disconnect` from the setting `sim_fault`.

**The routes of the printer.** `GET /work/machines/{id}/temperatures` (viewer): each heater with its reading, target, power and limit, and a short history. `PUT /work/machines/{id}/temperatures` (operator, `jobs:start`): `{"heater", "target_c"}`, `422 temperature_over_limit` above the limit whoever asks and `422 bad_heater` for a heater that does not exist. `GET /work/machines/{id}/layer` (viewer): the layer, the layers, the height and an estimate of the time left. `POST /work/machines/{id}/emergency-stop` (operator, `jobs:cancel`, no body): heaters off and motors released at once, the job ends, the machine is latched; the answer says whether the printer was reached. A latched machine refuses to prepare, start or heat (`409 printer_latched`) until `POST /work/machines/{id}/release` (administrator, `machines:manage`), which also tells the printer to start again (Klipper restarts its firmware).

**Answers shaped like Moonraker's and OctoPrint's.** With `compat` set to `moonraker`, `octoprint` or `both` (off by default), `GET /work/machines/{id}/compat/moonraker` gives the state in the shape of `GET /printer/objects/query` and `GET /work/machines/{id}/compat/octoprint` the printer and the job of `GET /api/printer` and `GET /api/job`, so that a program that reads those shapes can be pointed at the server's own routes. The server does not answer at the root paths of Moonraker and OctoPrint, and does not take their uploads: that needs a way for those programs to log in that the server does not have yet.

### `vision`

The results of the camera programs as artifacts of the store, and a station that makes them up in the simulator. Nothing looks at a picture: a request gives the size and the hash of a picture and the station gives findings that depend on them and on a `seed`, always the same for the same input, so a program is tried against the whole chain (job, result, artifact) without a camera. The machines are made with `POST /work/machines` (`"plugin": "vision"`); settings `max_findings` (1 to 1000, 50 by default) and `sim_rate`.

A job is `{"kind": "detect", "what": "fiducials" | "barcode" | "focus" | "blobs", "image": {"width": 1280, "height": 960, "sha256": "<hex>"}, "seed": 0, "output": "view" | "artifact", "artifact_name"}`. **The convention of a result** is the one EH3D-VISION-STUDIO already follows: a JSON file named `vision-<kind>.json`, format `application/json`, `schema_version` `eh3d.vision.result/1`, with `{"schema", "kind", "source": {"width", "height", "sha256"}, "findings": [...], "units": "px", "evidence", "producer": {"tool", "version"}}`. The findings are, by kind: `fiducials` `{x, y, radius_px, score}`, `barcode` `{format, text, x, y}`, `focus` `{score, sharp}` and `blobs` `{x, y, area_px}`. With `"output": "artifact"` the station keeps the document in the store of the caller under `vision-<kind>.json` (or `artifact_name`) and the view of the job has `artifact` instead of `result`; with the default `view` the document is in `view.result`. A program that looks through a camera keeps sending its own `vision-<kind>.json` to the store, as it does now: the document is the same, with `evidence: "camera"` (or what the program proves) in place of `simulator`.

### `cad`

Checks and reports of the project file of EH3D-PHOENIX-CAD, made by a simulated station that has **no geometry kernel**: it reads the structure of the file (the parts, the sketches and the features and how they point to one another), counts and checks that, and says so (`evidence: simulator`, and a `note` in the report). Settings: `max_items` (the most parts, sketches and features a project may have to be read, 5000 by default) and `sim_rate`.

A job is `{"kind": "check" | "report", "project": {...}}` or `{"kind": ..., "artifact": "<id>"}` (the text of the artifact is the file), with the same `output` and `artifact_name` as `vision` (`cad-check.json`, `cad-report.json`). The project file has `schema` `eh3d.cad.project/1`, `units` (`mm` or `in`), `sketches` `[{id}]`, `features` `[{id, kind, sketch | target}]` and `parts` `[{id, name, features}]`; a feature of kind `extrude`, `revolve`, `pocket` or `hole` points to a `sketch`, a `fillet`, `chamfer`, `mirror` or `pattern` to the feature it works on in `target`. The check lists `error`s (`bad_schema`, `unknown_units`, `missing_id`, `duplicate_id`, `missing_ref`) and `warning`s (`unknown_kind`, `empty_part`), and its verdict is `pass` while there is no error; a project that fails still ends the job `done`: the verdict is the answer. The report adds the counts (parts, sketches, features and the features by kind), the list of parts and the check.

**Where a CAD project lives.** The server keeps no project of its own for this plugin: PHOENIX-CAD saves its project in the project store under the application `phoenix-cad` (`/api/v1/apps/phoenix-cad/projects/{id}/files/content?name=project.json`), like every program of the family. A file that exists is written with `If-Match` (the SHA-256 of the version that was read): without it `428 precondition_required`, with another one `412 precondition_failed`, so two people never overwrite each other; the same store gives the versions, the editing lock and the `.eh3dproj` container (`docs/PROJECT_STORE.md`). The file that was read from there is what a request carries in `project`.

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

**Authorizing a job by its hash.** Every job of the queue has a `toolpath_sha256`, made when it is prepared: the SHA-256 of the plugin, the machine and the request as the server resolved it (a named artifact is already its text), with the keys in order. A person who has been shown the job says that it is what they mean to run with `POST /work/jobs/{id}/authorize` (operator, scope `jobs:authorize`, body `{"toolpath_sha256": "..."}`): the server records who authorised which hash (`authorization` in the job and a line in its history; audited as `work_job_authorize`). A hash that is not the job's is `409 stale_authorization` and the job stays as it was; only a queued job can be authorised (`409 not_ready`). `POST /work/jobs/{id}/start` may name the hash it expects (`{"toolpath_sha256": "..."}`, no body at all is as before): a start that names another hash than the job's, or than the authorised one, is `409 stale_authorization` and nothing starts; a body that does not read is `400 bad_request`, never ignored. With the switch `work_require_authorization` on (off by default, read when the server starts), a job of `laser`, `pnp` or `cnc` starts only if it has an authorization of its present hash: `409 authorization_required` otherwise. The scope `jobs:authorize` can never be given to a token of the `phoenix` or `cam` template. This is the hash binding of the computing/machine split of the API additions; it does not change what the plugins check.

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

Of these plugins every one has a first version that the server serves today (the sections above): simulators that run on the generic routes and, for `cnc`, `laser` and `printer3d`, a few routes of their own. They are narrower than their sheets and differ from them in places, so each sheet says what the plugin will be and the line in italics under its title says how the first version differs. The first versions of `laser`, `pnp` and `pcb` were narrower than their sheets and have been brought nearer to them; `pcb` is still the first version (the jobs that run on a worker over a project of the store are not served). The order of work was `cnc`, `pnp`, `laser`, `cam`, `gerber`, `pcb`, `printer3d`. Every plugin follows the same rules: it is a value of a type that implements `MachinePlugin` and is added to `Registry::standard()`; it has a simulator that needs no hardware (`connection.kind = "demo"` or its own `sim`) so that every route is tested; its routes are described through `tools/make_openapi.py` (so `docs/openapi.*`, `docs/ROUTES.md` and `docs/API.md` carry them); its refusals are codes of `docs/error-codes.json` with their texts in the eight languages of the suite (en, es, it, de, fr, pt of Portugal, zh, ja); its settings have a range and a conservative default and are checked by `check_settings`; whatever is a safety limit is a setting, never a constant; and its job requests that move a machine are the operator's (`jobs:prepare`, `jobs:start`, `jobs:cancel`). A machine that can move has a stop that does not wait for the queue (`POST /machines/{id}/stop` of the machine family, as `bungard` has; `emergency-stop` for `cnc` and `printer3d`).

The generic routes (`/work/machines`, `/work/jobs`, the events and the notifications) serve every plugin. A plugin adds only what they cannot say: its `request` kinds, its `detail` and, where the plugin needs a read that is not a job, a route under `/api/v1/work/machines/{id}/...` with the role and scope written in its sheet.

### `cnc`

*First version: served (see the section `cnc`). The connections `grbl` and `node` are not served, only `demo`; `file` and `gcode` are; the state `alarm` and the route `unlock` are not (an emergency stop latches the machine and an administrator releases it with `release`); `resume_safe_z_mm` is not a setting yet (a held job goes on from its line, the simulator does not move the head); the settings `rapid_mm_min` and `latch_on_stop` are added. The six codes of the sheet are served.*

For PHOENIX-CNC and any router, mill or engraver driven by G-code. Request kinds: `gcode` (`{"kind": "gcode", "text": "...", "skip": 0, "restore_outputs": false}`, the request PHOENIX-CNC already sends to `/machines/{id}/jobs`) and `file` (`{"kind": "file", "artifact": "<id>"}`, a G-code artifact of the store). Machines (`POST /work/machines`, `"plugin": "cnc"`) have a connection `grbl` (serial), `node` (a node of the suite) or `demo` (the simulator). The plugin plans nothing: it checks the program against the travel of the machine (`reject_out_of_travel`), counts lines for the progress and sends it through the same G-code runner `bungard` uses, so a held job resumes from its line. The job can be held and resumed, and a stop sends the controller's feed hold and reset.
* Routes: the generic ones, plus `GET /work/machines/{id}/position` (viewer, `machines:view`: the machine and work position, the state word of the controller, the override values) and `POST /work/machines/{id}/jog` (operator, `jobs:start`: `{"axis", "distance_mm", "feed"}`, refused while a job runs).
* States: `idle`, `busy`, `alarm` (shown as `busy` with `detail.alarm` until cleared by `POST /work/machines/{id}/unlock`, operator).
* Settings (defaults in brackets): `travel_x_mm`, `travel_y_mm`, `travel_z_mm` (the machine's own, 0 to 5000; unset means the check is off and the response says so), `jog_max_mm` (50, 0.01 to 500), `jog_max_feed` (1000 mm/min, 1 to 30000), `spindle_max_rpm` (12000, 0 to 65535), `reject_out_of_travel` (on), `resume_safe_z_mm` (5), `step_ms` of the simulator (20).
* Codes to add: `bad_gcode`, `out_of_travel`, `machine_alarm`, `jog_while_running`, `bad_axis`, `bad_feed`.

### `pnp`

*First version: served (see the section `pnp`) with the requests `load_feeders` and `place`; the list of placements may be an artifact (`job`, CSV or JSON). `calibrate` (simulated: the offset comes in the request), `dry_run` (in the request, not as a default of the machine) and the routes `GET` and `PUT .../feeders` (one path for all the feeders, the slot in the body) are served; the route of the vision, the state `waiting` with the dialogs of the operator and `dry_run_default` are not. Of the codes of the sheet, `bad_placement_list` is served (`pnp_feeder_short` stands for `feeder_empty`).*

For PHOENIX-PNP, a pick-and-place machine. Request kinds: `place` (`{"kind": "place", "job": "<artifact id of the placement list>", "board": {...}, "options": {"dry_run": true}}`) and `calibrate` (`{"kind": "calibrate", "what": "nozzle|camera|feeder", "target": "..."}`). A placement list is an artifact of the store (`csv` or `json`, the positions, rotations, packages and feeders). The plugin checks the list against the feeders and nozzles the machine has declared, makes the job ready with the count of components (`total`) and runs it step by step: pick, check, place; progress counts components. The job can be held and resumed between two components.
* Routes: the generic ones, plus `GET /work/machines/{id}/feeders` (viewer: each feeder with its component, remaining count and state), `PUT /work/machines/{id}/feeders/{feeder}` (operator, `jobs:prepare`: declare or refill) and `GET /work/machines/{id}/vision/last` (viewer: the last picture check, as the artifact id of the picture and the verdict).
* States: `idle`, `busy`, `waiting` (a dialog of the operator: a feeder is empty, a component failed its check).
* Settings: `feeders_max` (60, 1 to 200), `nozzles` (list, empty), `place_height_mm` (0.2, 0 to 5), `pick_retries` (2, 0 to 5), `vision_check` (on), `dry_run_default` (on: a new job moves nothing until the person turns it off), `step_ms` of the simulator.
* Codes to add: `bad_placement_list`, `unknown_feeder`, `feeder_empty`, `no_nozzle`, `vision_failed`, `component_missing`.

### `laser`

*First version: served (see the section `laser`) with the requests `gcode`, `vector` and `test_fire`, the routes `laser/test-fire` and `laser/release`, the switches of the enclosure, of the air assist and of the focus, and the latch on a door opened during a job. Differences: the limit is `power_limit_pct` (40 by default and rejecting) and not `power_max_pct`; the test fire is limited by `test_fire_max_pct` and `test_fire_max_s` (not `test_fire_ms`); `vector` takes paths and operations as data or as the JSON of an artifact, not a drawing file; the codes are `laser_power_limit`, `laser_estop_latched`, `enclosure_open`, `air_assist_off`, `bad_operation` and `test_fire_refused`, plus `focus_not_checked`.*

For PHOENIX-LASER, a laser cutter or engraver. Request kinds: `gcode` and `vector` (`{"kind": "vector", "artifact": "<id>", "operations": [{"layer", "mode": "cut|engrave", "power_pct", "speed", "passes"}]}`). The plugin enforces power limits in the server (a request above `power_max_pct` is refused, never clamped silently), keeps an emergency stop latched until an administrator releases it, and reports the limits in force in `detail`.
* Routes: the generic ones, plus `POST /work/machines/{id}/laser/test-fire` (operator, `jobs:start`: a short pulse at `power_pct` no higher than `test_fire_max_pct`, refused while a job runs, never when the enclosure input says open) and `POST /work/machines/{id}/laser/release` (administrator, `machines:manage`: releases a latched stop).
* States: `idle`, `busy`, `latched` (shown as `busy` with `detail.latched`).
* Settings: `power_max_pct` (60, 1 to 100), `test_fire_max_pct` (1, 0 to 10), `test_fire_ms` (50, 1 to 500), `require_enclosure_closed` (on), `require_air_assist` (on), `focus_check` (on), `latch_on_door_open` (on), `step_ms` of the simulator.
* Codes to add: `power_over_limit`, `laser_latched`, `enclosure_open`, `air_assist_off`, `bad_operation`, `test_fire_refused`.

### `cam`

*First version: served (see the section `cam`), but not as the sheet says: the worker runs nothing outside the server, so the kinds are `drill`, `outline`, `isolate`, `stencil`, `autolevel`, `inspect` and `verify` (`inspect` reads a toolpath, not a picture), the result is in the `view` of the job and the program can also be kept as an artifact of the store with `output: "artifact"`, `GET .../tools` lists what is built in, and the settings are those of the section (`max_concurrent`, `timeout_s`, `keep_results` and `sandbox_dir` are not needed). The codes `tool_not_installed`, `cam_failed` and `cam_timeout` are not used; `bad_params` and `input_too_large` are.*

For ISOCAM, STENCILCAM, AUTOLEVEL, CAM-DEBUGGER and VISION: the programs that turn a design into toolpaths or check a result. It does not move a machine: it is a plugin of computed jobs, with machines of the kind `worker` (the CM5 itself, or a node that runs a tool). Request kinds: `isolate` (`{"kind": "isolate", "artifact": "<gerber>", "params": {...}}`), `stencil`, `autolevel` (a probe map from a run) and `inspect` (`{"kind": "inspect", "artifact": "<picture or toolpath>"}`); each makes an artifact of the store as its result (`result_type`), reusing the tasks and artifacts of `docs/PROJECT_STORE.md`. Progress is the share of the algorithm done. A job can be cancelled, not held.
* Routes: the generic ones, plus `GET /work/machines/{id}/tools` (viewer: which of the kinds the worker has installed and their versions).
* Settings: `max_concurrent` (1, 1 to 4), `timeout_s` (600, 10 to 86400), `max_input_mib` (64, 1 to 512), `keep_results` (50, 1 to 1000), `sandbox_dir` (the folder the tool may write in).
* Codes to add: `tool_not_installed`, `bad_params`, `input_too_large`, `cam_failed`, `cam_timeout`.

### `gerber`

*First version: served (see the section `gerber`) with `load`, `drc` and `report`. The files come inline or as one artifact; the layers and the findings are in the `view` of the job (the routes `GET /work/jobs/{id}/layers` and `.../findings` read them) and the report is not an artifact. The checks are the track width, the gap between pads, the gap between tracks, the size of the holes, the ring around a hole and the distance to the edge; the copper pours are not looked at. `too_many_findings` is not a code (the list says it is cut).*

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

*First version: served (see the section `printer3d`) with the connections `demo`, `moonraker`, `octoprint`, `marlin` (simulated line only) and `bambu_lan` (an experiment: the simulator, and a real printer over TLS behind a switch that is off), the requests `print`, `preheat` and `gcode`, the routes `temperatures`, `layer`, `emergency-stop`, `release` and the optional `compat/...` answers, and the eight codes of the sheet. Differences: the states are `idle` and `busy`; the setting `max_accel_mm_s2` of the sheet is not served, and `model`, `auto_start_queue`, `compat`, `compat_start`, `sim_rate` and `sim_fault` are added; the limits of temperature come from the profile of the model.*

For PHOENIX-3DP (a new application, `phoenix-3dp` among the known applications of `docs/AUTH.md`), a 3D printer driven by G-code with Klipper (Moonraker), Marlin (serial) or the simulator. Machines have a connection `moonraker` (`{"url": "http://host:7125", "api_key": "..."}`, the key kept like the token of a node: never shown), `marlin` (serial port and baud rate) or `demo`. Request kinds: `print` (`{"kind": "print", "artifact": "<gcode>", "preheat": true}`), `preheat` (`{"kind": "preheat", "bed_c", "hotend_c"}`) and `gcode` (a short manual command list, operator). A print can be held and resumed (pause at a layer change), cancelled, and the progress is the share of the file sent; the layer is read from the `;LAYER:` comments or from the controller's report when it gives it.
* Routes: the generic ones, plus `GET /work/machines/{id}/temperatures` (viewer: for each heater `current_c`, `target_c`, `power_pct` and the `limit_c` in force; a short history), `PUT /work/machines/{id}/temperatures` (operator, `jobs:start`: `{"heater", "target_c"}`, refused above the limit), `GET /work/machines/{id}/layer` (viewer: `layer`, `layers`, `z_mm`, `eta_s` when known) and `POST /work/machines/{id}/emergency-stop` (operator, `jobs:cancel`: heaters off and motors released at once, the machine latched until `POST /work/machines/{id}/release` by an administrator).
* States: `idle`, `busy`, `waiting` (a held print or a filament change), `latched` (after an emergency stop or a thermal fault, shown as `busy` with `detail.latched`).
* Settings, all of them limits an administrator can change and none a constant: `hotend_max_c` (260, 0 to 450), `bed_max_c` (100, 0 to 150), `chamber_max_c` (60, 0 to 100), `heat_timeout_s` (900, 30 to 3600: a heater that does not reach its target in this time stops the job), `temp_deviation_c` (15, 1 to 50: a reading this far from the target for `deviation_s` (10, 1 to 120) latches the machine), `min_temp_c` (5: a reading below it is a broken sensor), `max_feed_mm_s` (200, 1 to 500), `max_accel_mm_s2` (3000, 100 to 20000), `bed_x_mm`, `bed_y_mm`, `bed_z_mm` (the machine's own; a program that goes past them is refused), `preheat_default_hotend_c` (200) and `preheat_default_bed_c` (60), `poll_ms` (1000, 200 to 10000), `step_ms` of the simulator. A target above a limit is refused with `422 temperature_over_limit` whoever asks; a limit changed above the hard maximum written here is refused at `check_settings`.
* The G-code is read before it is accepted: a temperature command (`M104`, `M109`, `M140`, `M190`, `M141`) above a limit refuses the whole program (`422 program_over_limit`), and `M112` in a file is refused (the emergency stop is the route's).
* The simulator heats and cools with a first-order model, can inject a thermal runaway, a lost sensor and a disconnection, and counts layers from the file, so every route and every code is tested without a printer.
* Codes to add: `temperature_over_limit`, `program_over_limit`, `heater_timeout`, `thermal_runaway`, `sensor_fault`, `printer_latched`, `printer_disconnected`, `bad_heater`.

## What has been tried

Everything above runs in the tests against the `demo` plugin, the simulator of the `bungard` plugin and the simulators `laser`, `pnp`, `pcb`, `cnc`, `gerber`, `cam`, `printer3d`, `vision` and `cad`. Nothing has been tried with a second kind of real machine: the evidence of the new plugins is `simulator`, and the `bungard` plugin has run on a real machine only as far as the old routes have. The drivers for Moonraker and OctoPrint were tried against servers that live in memory and against a socket of the computer of the tests, not against a real Klipper or a real OctoPrint, and the limits of temperature of the models are the makers' figures as the project knows them. The jobs of `demo`, `laser`, `pnp`, `pcb`, `cnc`, `gerber`, `cam`, `printer3d`, `vision` and `cad` live in memory: one that was open when the server stopped is marked as failed.
