<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center">🇺🇸 <b>English</b> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### The center of the EH3D programs: it takes jobs from DRILLPRO, ROUTEPRO and the clients to come, plans them, runs them on the machine through its node and tells everybody how they go

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Honesty check - what actually runs today:** a Rust server (axum) with a REST and websocket interface that keeps users, machines and jobs, plans a drilling or routing job (holes and paths, or an Excellon or HP-GL file with its tool table) with the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO, shows a preview, and runs it on a machine through its EH3D-NODE-BUNGARD or on a simulator, with the dialogs of the operator, a stop and every change told as it happens. 55 tests run on a computer, the jobs against the simulator. **Nothing has run on a real machine through it yet**, it does not terminate TLS, and of the clients besides the programs, EH3D-STUDIO exists (never tried against a real machine) and the Android app and the panel next to the machine do not exist yet.

---

> **Safety and security status:** this is not a safety system. A job never starts by itself: a person with the role of operator starts it, one job at a time on a machine, and the planning refuses holes and paths outside the table and tools that are missing or out of range. Every request needs a token (a viewer reads, an operator runs jobs, an administrator manages users, tokens and machines); passwords are kept as Argon2 hashes, session and program tokens only as SHA-256, and the token of a node is never shown by the API. The server is for the house or a VPN into it: put HTTPS in front before anything outside reaches it. The stop of the machine does not depend on the server alone: the node stops by itself when its link goes quiet. The project is independent: it is not made, endorsed or supported by Bungard Elektronik.

## 📋 At a glance

| | |
|---|---|
| Version | `0.0.1` |
| Maturity | Scaffolding |
| Tests | 55 |
| Stack | Rust (axum, tokio) |
| On real hardware | No: tested against the simulator; nothing has run on a real machine through the server yet |

**Jump to:** [Overview](#-overview) · [Architecture](#-architecture) · [Quick start](#-quick-start) · [Testing](#-testing) · [Structure](#-repository-structure) · [Related](#-related-projects)

## 🚦 Project status

Each line is one part of this repository and the light says how far it is today. It is written from what the repository can show (tests, builds, runs), not from what is planned.

🟢 Done and verified · 🟡 Works, with known limits · 🟠 Partly done or not verified yet · 🔴 Not done or not working

| Area | State | What it means here |
|---|---|---|
| Software | 🟡 | A Rust server (axum) keeps users, machines and jobs, plans drilling and routing jobs with the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO, shows a preview and runs the job with operator dialogs and a stop. |
| Tests | 🟢 | 55 tests pass (`cargo test`): 38 in the modules and 17 on the interface end to end, with jobs running on the simulator (12 in `tests/api.rs`, 5 with the real programs in `tests/programs.rs`). |
| Interface for clients (API) | 🟡 | The REST requests and the websocket events are written down in `docs/API.md` and covered by the tests; only the programs' own server connection uses them so far. |
| Clients | 🔴 | EH3D-STUDIO uses it (never tried against a real machine); the Android app and the touch panel next to the machine do not exist yet. |
| Security | 🟡 | Every request needs a token with one of three roles, passwords are Argon2 hashes and wrong passwords are slowed down; the server does not terminate TLS (put HTTPS in front) and has had no independent review. |
| Real hardware | 🟠 | The jobs have only run against the simulator: nothing has run on a real machine through the server. |
| Deployment | 🟠 | A systemd unit and `docs/DEPLOY.md` describe the service on the CM5; this repository does not record a deployment as done. |
| Integration | 🟡 | It plans with the same libraries as EH3D-DRILLPRO and EH3D-ROUTEPRO (their folders must sit next to this one) and talks to the link of EH3D-NODE-BUNGARD, which it has only reached through the simulator. |
| Documentation | 🟢 | README in eight languages, the API reference and the guide to running it as a service. |

## 🎯 Overview

* The **jobs**: a drilling job (holes and tools, or a `.NCD` file with its tool table) or a routing job (paths and tools, or a `.PLT` file with its tool table), planned with the libraries of the programs, so a client draws and a machine does exactly the same thing; refused with a code when a hole is outside the table or a tool is missing.
* The **preview**: the work in the order the machine does it, the tools with their count, the bounding box, an estimate of the time and the item at which each hole is finished - what a screen needs to draw the board and follow the job.
* The **run**: a job is ready until a person starts it; it runs on the library of the programs (the handshake, the arming, the reference run, the holes, the stop), waits at the dialogs of the operator (change the tool, confirm the spindle) until someone confirms, and can be stopped; the machine stops with the drivers off.
* The **machines**: for each one how to reach it (an EH3D-NODE-BUNGARD over the network, or the simulator), the text of its configuration files and the values tuned on it, so the calibration of a machine lives in one place for every client.
* The **fleet and the stations**: every machine belongs to a station (the bench, the room, the workshop) and `GET /api/v1/fleet` answers all of them at once, grouped by station, with what each one is doing (free, busy, waiting for the operator), its job with the progress and the number of jobs ready; the machines run at the same time, one job on each.
* The **people and the programs**: users with three roles, tokens for programs shown once, a slowdown on wrong passwords, and a set-up code on the console for the first administrator.
* The **events**: a websocket that sends every change of a job as it happens.
* The **service**: one binary and a small text file of settings, on port 18200 by default (not ARMOR's nor HYDRA-UMC's), with a systemd unit for the CM5.
* **Planned, not built yet**: the clients (the Android app, the Studio, the panel with a touch screen next to the machine), HTTPS in the server itself, and the use of the node's lamps and its buffer to finish a held job when the server goes away.

## 🧩 Role in EH3D

EH3D-SERVER is the center of the EH3D family: the programs send their jobs to it, it talks to the machines through their nodes, and the other clients see and send the same jobs through the same interface. It uses the libraries of [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) and [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) and speaks to [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 🏛️ Architecture

<p align="center">
  <img src="images/architecture.svg" alt="EH3D-SERVER architecture: who uses it, what it holds and what it depends on" width="100%">
</p>

How the parts of this repository fit together: who uses it, what it holds and what it depends on. The picture is drawn from `docs/architecture.json` and checked against the code.

## ✨ Features

* **A preview that is the plan:** the preview and the real run come from the same planning, so what a client draws is what the machine does.
* **A job never starts by itself:** it is created ready, and only an operator starts it; one job at a time on a machine (`409 machine_busy`).
* **Operator dialogs over the network:** the job pauses at the tool change and the spindle confirmation until a client confirms.
* **Live events:** a websocket tells every change of a job as it happens, with the summaries of the jobs on connect.
* **Calibration in one place:** the configuration files and the tuned values of each machine are kept on the server, so every client plans with the same numbers.
* **A job that was running when the server stopped is marked failed:** nobody knows where the machine was, so nothing resumes by itself.
* **Safe files:** users, tokens, machines and jobs are JSON files written through a temporary file; a damaged file is an error, never a silent restart from nothing.
* **Updates from GitHub:** with `updates_dir` the server offers, to an administrator, the check of the latest releases of itself and of EH3D-STUDIO and their update (`/api/v1/updates`); a small helper started by systemd (`deploy/eh3d-update.py`) does the downloading, checks the SHA-256 of each file, refuses an archive with names outside its folder, replaces the installed folder (the old one is kept as `.previous`) and restarts the service. It has been tested with GitHub and systemd replaced by stand-ins, never on a real installation.
* **The node of the drill head through the server:** when the configuration of a machine names the node (`head_address`, `head_token`), the server hands on its status (`GET /api/v1/machines/:id/head/status`), its camera as MJPEG (`.../head/stream`, with the token in the address for a browser) and a measure of the table (`POST .../head/measure`, operator), so a program on another network reaches it with the same login. Tested against a stand-in node, never against a real one.
* **The quality check through the camera of the head:** the server turns the ring of LEDs on, takes a picture from the node, judges it with the analyser of the shop (the profile of each machine is kept by the server), makes the ring say how it went (green, or red blinking) and keeps the last check so the screen next to the machine and the phones show it: `GET/POST .../head/light`, `GET .../head/snapshot`, `POST/GET .../head/inspect`, `GET/PUT .../head/inspect/profile` and `POST .../head/inspect/calibrate`.

## 🔒 Security & Safety

| Threat | Defense |
|---|---|
| Anyone on the network sending requests | every request but the health check, the first set-up and the login needs a token (`Authorization: Bearer`, or `?token=` for the websocket); three roles: viewer reads, operator runs jobs, administrator manages users, tokens and machines |
| Guessing a password | five wrong passwords block the name, for 30 s and then 60 s, 120 s and so on while they go on (`429 throttled`); the answer does not say which part was wrong, and an unknown name costs as much as a known one |
| A copy of the data folder | passwords are Argon2id hashes; session and program tokens are random and only their SHA-256 is kept, so a copy of the folder logs nobody in; the token of a node is never shown by the API |
| Taking over a fresh server | the first administrator is made with a set-up code that only the console prints |
| Locking everybody out | the last administrator cannot be deleted, disabled or demoted (`409 last_admin`) |
| A job that starts by itself or a second job on a machine | a job is created ready; only an operator starts it, and a machine runs one job at a time |
| A plan that would hit the table or use a missing tool | the planning refuses holes and paths outside the table, and tools that are missing or out of range, with a code |
| The server dies while the machine moves | the node stops by itself when its link goes quiet; the job is marked failed and never resumes by itself; the service stops its jobs on SIGINT |
| A huge request | the body is limited (`max_body_mb`, 8 MB by default) |
| A damaged data file | an error, never a silent start from nothing |
| A service with too many rights | the systemd unit runs as its own user with `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp` and one writable folder |

**What is not defended:** the server does not terminate TLS, so tokens and passwords cross the network in the clear unless HTTPS is put in front; the token of the websocket is in the address; it has had no independent review. Report a vulnerability privately (see [SECURITY.md](SECURITY.md)).

## 🔌 Interfaces

| Interface | What it is | Where |
|---|---|---|
| REST | `/api/v1/`: `health`, `setup`, `login`, `logout`, `session`, `account`, `users`, `tokens`, `updates`, `fleet`, `fleet`, `machines` (with `preview` and `jobs`), `jobs` (with `start`, `confirm`, `cancel`); JSON in and out, errors as `{"error","detail"}` | [docs/API.md](docs/API.md), `src/api.rs` |
| Websocket | `GET /api/v1/events?token=...`: a `hello` with the summaries of the jobs, then `job` and `job_removed` for every change | [docs/API.md](docs/API.md) |
| To the machine | a machine's `connection` is `node` (an EH3D-NODE-BUNGARD at an address with its token) or `demo` (the simulator) | [docs/API.md](docs/API.md), `src/machines.rs` |
| Command line | `eh3d-server [--config FILE]`; `--help` lists it | `src/main.rs` |
| Service | one binary, port 18200 by default, a systemd unit | [docs/DEPLOY.md](docs/DEPLOY.md), `deploy/eh3d-server.service` |

## ⚙️ Configuration

`server.cfg` (or the file given with `--config`) holds five optional keys; an unknown key or a value out of range is an error at start. `server.cfg.example` explains them.

| Key | Default | Range | What it does |
|---|---|---|---|
| `listen` | `0.0.0.0:18200` | an address and a port | where the server listens (18200 is its own port) |
| `data_dir` | `data` | a folder | where users, tokens, machines and jobs are kept; back it up |
| `session_hours` | 12 | 1 to 720 | how long a login lasts |
| `max_body_mb` | 8 | 1 to 256 | the largest request, in megabytes |
| `keep_jobs` | 200 | 10 to 100,000 | how many finished jobs the list keeps |

Names of users and tokens: letters, digits, `.`, `_` and `-`, up to 32 characters; passwords: 8 to 128 characters. The values tuned on each machine are not here: they live in the machine record, in the format of the machine file of the programs.

## 🚀 Quick start

You need a stable Rust toolchain, and EH3D-DRILLPRO and EH3D-ROUTEPRO next to this folder (the server uses their libraries).

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Run it with `run.bat` (Windows) or `./run.sh` (Linux); it builds first if needed. While it has no user the console shows a set-up code: make the administrator with `POST /api/v1/setup`, log in, add the machine with its node's address and token, and make a token for each program. `server.cfg.example` lists the settings; `docs/DEPLOY.md` installs it as a service on the CM5.

## 🧪 Testing

The suite has 55 tests (`cargo test`, run on Windows and Linux in the CI with COMMON, DRILLPRO and ROUTEPRO checked out next to the server): 38 in the modules (settings and their ranges, users, roles, tokens, the slowdown on wrong passwords, the files and the job book, planning), 12 on the interface end to end through the real router (`tests/api.rs`), and 5 with the real programs sending their jobs to a real server on a real port, with the machine on the simulator (`tests/programs.rs`). The CI also runs `cargo fmt --all --check`, `cargo clippy` with warnings as errors and `python -I tools/ci_validate.py`. Nothing here has been tested against a real node or machine. The helper that does the updates has 10 more tests of its own (`python -I tests/test_update_helper.py`, with GitHub and systemd replaced by stand-ins).

## 🏗️ Build & Release

The version lives in `Cargo.toml` and in `eh3d.project.json`, and each version has its entry in [CHANGELOG.md](CHANGELOG.md). `build.sh` / `build.bat` build the release binary (`target/release/eh3d-server`) in a live panel, and `test.sh` / `test.bat` run only the tests; neither changes the version. No release has been published yet. On a CM5 (aarch64 Linux) build it on the machine itself; `docs/DEPLOY.md` installs it as a service.

## 📂 Repository Structure

<p align="center">
  <img src="images/repository-structure.svg" alt="Repository structure of EH3D-SERVER" width="100%">
</p>

<details>
<summary>Text version of the tree</summary>

```text
EH3D-SERVER/
├── src/
│   ├── api.rs                                   the REST routes and the websocket
│   ├── auth.rs                                  users, roles, tokens and the slowdown on wrong passwords
│   ├── plan.rs                                  from the request to the items the machine runs
│   ├── jobs.rs state.rs                         the jobs and what the server is made of while it runs
│   ├── machines.rs store.rs                     the machines, and the files written through a temporary file
│   ├── config.rs lib.rs main.rs                 the settings file, the library root and the program
│   ├── updates.rs  head.rs                      the updates of the services
│   └── inspect.rs                               the quality check through the camera of the head
├── tests/                                       the interface end to end on the simulator, and the real programs against a real server
├── docs/                                        the API, the guide to running the server and the data of the two pictures
├── deploy/                                      the systemd unit of the service
├── server.cfg.example                           the settings file with every key explained
├── tools/                                       CI check, README check, the panel of the scripts and the picture tools
├── images/                                      banner and the Architecture and Repository Structure pictures
├── .github/                                     CI and release workflows, issue and pull-request templates
├── build.sh build.bat                           build the release binary in a live panel
├── run.sh run.bat                               start the server (builds first if needed)
├── eh3d.project.json                            manifest: version, role, maturity and build command
├── Cargo.toml Cargo.lock                        the Rust package, its dependencies and the libraries of the two programs
├── CHANGELOG.md                                 what changed in each version
├── SECURITY.md SUPPORT.md                       how to report a vulnerability and where to ask
└── CONTRIBUTING.md CODE_OF_CONDUCT.md LICENSE   how to contribute, the code of conduct and the licence
```

</details>

## 🗺️ Roadmap & Known limits

**Known limits today**

* Nothing has run on a real machine through the server: the jobs have only run against the simulator, and the link to EH3D-NODE-BUNGARD has not been reached for real.
* The server does not terminate TLS; put HTTPS in front before anything outside reaches it. It has had no independent review.
* Of the clients besides the programs, EH3D-STUDIO exists; the Android app and the touch panel do not exist yet.
* It needs the folders of EH3D-DRILLPRO and EH3D-ROUTEPRO next to it, because it uses their libraries.
* A job that was running when the server stopped is marked failed and does not resume.

**Next**

* Run a whole job on a real machine through the server and the node.
* HTTPS in the server itself.
* Use the node's lamps and its buffer to finish a held job when the server goes away.
* Build the first client besides the programs.

## 🌍 Languages

README and texts exist in English, Spanish, French, Italian, German, Chinese, Japanese and European Portuguese.

## 🔗 Related Projects

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): the drilling program; its library plans and runs the drilling jobs
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): the routing program; its library plans the routing jobs
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): the node in the machine's box that the server talks to
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): the common parts of the family

## 📚 Documentation

* [The API](docs/API.md): every request, the job and the view a client draws, the states and the websocket.
* [Running the server](docs/DEPLOY.md): the build, the first start, the service on the CM5 and the safety of the deployment.
* [Changelog](CHANGELOG.md)

## 🤝 Contributing & Community

* Before a pull request run `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `python -I tools/ci_validate.py`: all must pass. See [CONTRIBUTING.md](CONTRIBUTING.md) and the [code of conduct](CODE_OF_CONDUCT.md).
* The README exists in eight languages: a change to the text is made in all eight files, and the two pictures are drawn from `docs/architecture.json` and `docs/structure.json` by `tools/make_architecture.py` and `tools/make_structure.py`.
* A change to a route of the API needs its entry in `docs/API.md`, a test through the real router and, for a request that changes something, the role that may do it.
* Report a vulnerability privately: [SECURITY.md](SECURITY.md). Questions, ideas and reports: electrohobby3d@gmail.com ([SUPPORT.md](SUPPORT.md)).

## 👤 AUTHOR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENSE

Software: GPL-3.0-or-later. See [LICENSE](LICENSE). Hardware designs of the suite use CERN OHL-S.
