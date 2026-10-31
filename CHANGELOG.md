# Changelog

All notable changes to this project are documented here.

## [Unreleased]

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
