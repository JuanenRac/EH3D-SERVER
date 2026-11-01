# Changelog

All notable changes to this project are documented here.

## [Unreleased]

- **The secrets of the tuning text stay on the server:** `GET /machines` and `GET /machines/:id` show `head_token`, `node_token` and any other `*_token`, `*password` or `*_secret` key with an empty value, and a `PUT` that sends them empty keeps the stored ones.
- **A started job cannot be undone by a late start:** a job cancelled or removed while it was being planned is not started any more, and `DELETE /jobs/:id` checks and removes under one lock.
- **A machine with a job running is not edited:** `PUT /machines/:id` answers `409 machine_busy`, and `POST /updates/apply` answers `409 machines_busy` while any job is active (the update restarts the server).
- **A node without a token can be saved** (the token is optional in the connection) and a request body that does not read comes back as `{error, detail}` (`bad_request`, `too_big`) like every other refusal.
- **Guessing the setup code is slowed down** like the passwords, and the table of wrong passwords per name no longer grows without limit.
- **The websocket of the events** asks every 30 seconds whether its session is still good and closes when it is not.
- **The published binaries are built with `--features serial`** and the CI builds and tests that variant too.

## [0.0.7] - Times of the GRBL lines and the homing

- **Waits of a GRBL machine by the kind of line:** `grbl_line_timeout_s` (5 s), `grbl_homing_timeout_s` (120 s) and `grbl_jog_timeout_s` (60 s) in `server.cfg` (each from 1 to 3600), so that a `$H` of a machine that allows the `$` lines has time to finish while an ordinary line still fails quickly. A line that does not end in time holds the machine (a homing cycle is reset, a jog cancelled) and fails the job with the line. Needs the controller crate of EH3D-COMMON with the same change.
- 1 test more (the three settings, their defaults and their range), and the job that carries `$H` and `$J=` runs on the simulator.

## [0.0.6] - Resume a stopped job from its saved request, and a switch for the dollar lines of GRBL

- **Going on from a stopped job without asking for the file again:** the server keeps the request of every job as it came (the file, the tool table, the options, the program text) and gives it back with `GET /api/v1/jobs/:id/request` (operator or administrator; it goes with the job and is no bigger than a request may be). `POST /api/v1/jobs/:id/resume-from` makes a new job, ready and never started, from a cancelled or failed one with its `skip` brought forward to what it had done: the holes done for drilling, the paths finished for routing (the one cut short is cut again) and the `line` for G-code, adding up over several stops. The body may name `skip`, `skip_tools`, `restore_outputs` and `name`; `409 not_stopped` for a job that did not stop. The new job waits for a person to start it, and a G-code one waits for the operator before its first move. Jobs carry `resumed_from`.
- **A switch per machine for the `$` lines of GRBL:** the machine member `allow_dollar` (off by default, changed only by an administrator, kept when an update does not name it) lets the G-code jobs of that machine carry lines such as `$H`, `$X`, `$G` and `$J=...`. `$RST`, the real-time characters and any `$` line with odd signs are refused with `bad_gcode` even then, and a ready job is judged again when it starts, so turning the switch off stops a job made while it was on.
- 5 tests more (the dollar lines and what is refused with them, going on from a drilling, a routing and a G-code job, the roles, the sums over two stops, the switch of a machine).

## [0.0.5] - The original spindle starts from the speed of the maker

- **The original spindle starts from the speed of the maker:** 3000 to 63000 rpm and 150 W (the KaVo spindle of the CCD, as the manual and the catalogue of Bungard say) instead of 10000 to 60000 rpm and 200 W; the settings of a machine still replace them.

## [0.0.4] - G-code jobs, pause and resume, GRBL machines and the spindle of the machine

- **Free G-code jobs:** a job request of `"kind": "gcode"` carries the text of a program; the server cleans it into the lines it sends (no comments, no settings or real-time commands of the controller: `bad_gcode`), judges its `S` and `F` against the spindle of the machine like any other job, and runs it line by line on a machine whose connection is `demo` (the simulator) or the new `grbl` (a GRBL controller on a serial port of the server, `port` and `baud`; build with `--features serial`). The view gives the bounds, the length and the time of the moves; the job reports `items_done`, `items_total` and the `line` it has got to.
- **Hold, resume and go on after a stop:** `POST /jobs/:id/pause` and `/resume` hold and release a running G-code job (`confirm` releases it too); a job made with `skip` (the lines already done) gives the codes in force back and waits for the operator before its first move, and `restore_outputs` starts the spindle and the coolant again only when asked. A line the controller refuses, an alarm or a link that fails ends the job as failed, says which line, and stops the controller. A machine of one kind refuses the jobs of the other (`wrong_machine`).
- **A setting for the end of a job:** `gcode_finish_wait_s` in `server.cfg` (900 by default) is how long a G-code job waits for the machine to be still after its last line.
- 20 tests more (the cleaning and reading of a program, the run on the simulator, hold and resume, the line a failed job starts from, the machines that take each kind of job, the spindle switch, and the excesses of the spindle reaching the program that sent the job).

## [0.0.3] - Tools switched off, the spindle of the machine and a switch for its limits

- **Tools switched off and jobs that go on, in the job request:** a drilling or routing job takes `skip_tools` (the tools or pens that are off: their holes or paths are left out and they need not be in the tool table) and `skip` (how many holes, in the order the machine drills them, or paths are already done), and the view tells what was left out (`skipped_tools`, `skipped`); a job with nothing left is refused with `no_work`. A drilling job and a routing job one after the other, each with its tools off, run on the simulator through the real programs. 7 tests.

- **The spindle of the machine, and a safety switch for the limits of the spindle:** a machine says which spindle it carries (`spindle`: `original` or `vfd`) and the server knows the limits of each (the slowest and the fastest turns, the feed along the cut and of the plunge, the power, the thickest bit; the starting values are the ones of the programs and a machine changes only the members it names). `GET` and `PUT /api/v1/machines/:id/spindle` (the change is for an administrator) show and set the spindle, the switch `guard` and the limits, and answer the top speed the node of the machine should be set to (`node_max_rpm`). With a spindle chosen, a job whose turns or feeds go beyond its limits is refused with `422 spindle_limit` (and says which tool and by how much), both when the job is made and when it starts, so a job made for one spindle does not run on the other; the switch `guard` is `reject` by default, and with `warn` the job goes through with the turns cut to the top of the spindle, so the node is never asked for more, and every excess is listed in `spindle_warnings` of the view. A machine with no spindle chosen is judged by nothing, as before, and an update of a machine from a client that does not name the spindle leaves it as it was. 13 tests.

## [0.0.2] - The quality check through the camera of the drill head

- **The quality check through the camera of the head:** `GET` and `POST /api/v1/machines/:id/head/light` (the ring of LEDs), `GET .../head/snapshot` (one picture), `POST .../head/inspect` (the light, the picture, the analysis with the profile of the machine and the colour of the ring that says how it went), `GET .../head/inspect` (the last check), `GET` and `PUT .../head/inspect/profile` (tolerances and presets) and `POST .../head/inspect/calibrate` (the scale from the hole that was measured). The profile of each machine is kept by the server; a query for the ring that has signs the node does not use is refused (`422 bad_light`).

- **The node of the drill head through the server:** `GET /api/v1/machines/:id/head/status`, `POST .../head/measure` and `GET .../head/stream` hand on the status, a measure of the table and the camera (MJPEG) of the node that the machine's configuration names (`head_address`, `head_token`), with the roles viewer, operator and viewer; the token of the node never leaves the server. Tested against a stand-in node.

## [0.0.1] - First release: the server of the machines, the fleet, the pages of Studio and the updates from GitHub

- **Updates from GitHub:** with `updates_dir` the server offers (`GET /api/v1/updates`, `POST /api/v1/updates/check` and `/apply`, administrator only) the check of the latest releases and the update of the server and of Studio; a small helper started by systemd (`deploy/eh3d-update.py`) does the downloading, checks the SHA-256, replaces the installed folder (keeping the old one) and restarts the service. A workflow builds the packages of the server for Linux (x86_64 and aarch64), Windows and macOS.
- **The pages of Studio from the server:** the setting `studio_dir` names a folder with the pages of EH3D-STUDIO and the server serves it at `/` on its own address (a page that is not a file is the start page, so a reload works); without it the server answers only the API.
- **Several machines and stations at once:** a machine has a `station` (the bench, the room or the workshop it belongs to), and `GET /api/v1/fleet` answers for every machine in one request, grouped by station, with what each one is doing (free, busy, waiting for the operator), its job with the progress and the number of jobs ready; the machines run at the same time, one job on each (a test runs jobs on two machines of two stations together).
- **A lighter debug build:** the `dev` profile keeps the file and line of a panic (`debug = "line-tables-only"`) but not the full debug symbols, so the `target` folder of a test run is about a tenth of its size.
- **README in the universal layout, in eight languages:** the README has the same sections in the same order as the other repositories of the family (status, overview, architecture, features, security, interfaces, configuration, quick start, testing, structure, roadmap and known limits), with figures taken from the code and from the tests that were run. The Architecture and Repository Structure pictures are drawn from `docs/architecture.json` and `docs/structure.json` by `tools/make_architecture.py` and `tools/make_structure.py`, and the CI checks that they match.
- **The build, build-test and test scripts show a live panel:** a banner with the name and version, one line per step with its time, the number of tests that passed and a summary at the end, with the whole output kept in a log file. `test.sh` and `test.bat` run only the tests and never change the version; without `tools/pretty_run.py` the scripts behave as they always did.
- **The layout of the README is checked in the CI:** the validator runs `tools/universal_readme_check.py` and lists, as a warning, what differs from the universal layout of the README.

## [0.0.1] - The server: users, machines, jobs and the run through a node

- **Jobs:** a drilling job (holes and tools, or an Excellon file with its tool table) or a routing job (paths and tools, or an HP-GL file with its tool table), planned with the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO and the constants and tuning of the machine; refused with a code when a hole or a point is outside the table, a tool is missing or out of range, or the request is too big.
- **Preview:** the work in the order the machine does it, the tools with their count, the bounds, an estimate of the time and the item at which each hole ends.
- **Run:** a job is ready until a person starts it, one job at a time on a machine; it runs on the library of the programs, waits at the dialogs of the operator, can be stopped and is marked failed if the server stopped while it ran.
- **Machines:** an EH3D-NODE-BUNGARD over the network or the simulator, the text of the configuration files and the tuned values; the token of a node is never shown.
- **People and programs:** users with three roles, tokens for programs shown once, Argon2 password hashes, SHA-256 token hashes, a slowdown on wrong passwords and a set-up code on the console for the first administrator.
- **Events:** a websocket with every change of a job.
- **Service:** one binary, a settings file, port 18200 by default, a systemd unit for the CM5.
- 39 tests (unit tests and the interface end to end against the simulator), `cargo clippy -D warnings` and `cargo fmt --check` in CI on Linux and Windows, and a repository check of the manifest, the versions, the changelog and the eight README files.
