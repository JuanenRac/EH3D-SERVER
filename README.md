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

**Honesty check - what actually runs today:** a Rust server (axum) with a REST and websocket interface that keeps users, machines and jobs, plans a drilling or routing job (holes and paths, or an Excellon or HP-GL file with its tool table) with the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO, shows a preview, and runs it on a machine through its EH3D-NODE-BUNGARD, with the dialogs of the operator, a stop and every change told as it happens. 39 tests run on a computer, the jobs against the simulator. **Nothing has run on a real machine through it yet**, it does not terminate TLS, and the clients that will use it besides the programs (the Android app, the Studio, the panel next to the machine) do not exist yet.

---

> **Safety and security status:** this is not a safety system. A job never starts by itself: a person with the role of operator starts it, one job at a time on a machine, and the planning refuses holes and paths outside the table and tools that are missing or out of range. Every request needs a token (a viewer reads, an operator runs jobs, an administrator manages users, tokens and machines); passwords are kept as Argon2 hashes, session and program tokens only as SHA-256, and the token of a node is never shown by the API. The server is for the house or a VPN into it: put HTTPS in front before anything outside reaches it. The stop of the machine does not depend on the server alone: the node stops by itself when its link goes quiet. The project is independent: it is not made, endorsed or supported by Bungard Elektronik.

## 📋 At a glance

| | |
|---|---|
| Version | `0.0.1` |
| Maturity | Scaffolding |
| Tests | 39 |
| Stack | Rust |
| On real hardware | No: tested against the simulator; nothing has run on a real machine through the server yet |

## 🎯 Overview

* The **jobs**: a drilling job (holes and tools, or a `.NCD` file with its tool table) or a routing job (paths and tools, or a `.PLT` file with its tool table), planned with the libraries of the programs, so a client draws and a machine does exactly the same thing; refused with a code when a hole is outside the table or a tool is missing.
* The **preview**: the work in the order the machine does it, the tools with their count, the bounding box, an estimate of the time and the item at which each hole is finished - what a screen needs to draw the board and follow the job.
* The **run**: a job is ready until a person starts it; it runs on the library of the programs (the handshake, the arming, the reference run, the holes, the stop), waits at the dialogs of the operator (change the tool, confirm the spindle) until someone confirms, and can be stopped; the machine stops with the drivers off.
* The **machines**: for each one how to reach it (an EH3D-NODE-BUNGARD over the network, or the simulator), the text of its configuration files and the values tuned on it, so the calibration of a machine lives in one place for every client.
* The **people and the programs**: users with three roles, tokens for programs shown once, a slowdown on wrong passwords, and a set-up code on the console for the first administrator.
* The **events**: a websocket that sends every change of a job as it happens.
* The **service**: one binary and a small text file of settings, on port 18200 by default (not ARMOR's nor HYDRA-UMC's), with a systemd unit for the CM5.
* **Planned, not built yet**: the clients (the Android app, the Studio, the panel with a touch screen next to the machine), HTTPS in the server itself, and the use of the node's lamps and its buffer to finish a held job when the server goes away.

## 🧩 Role in EH3D

EH3D-SERVER is the center of the EH3D family: the programs send their jobs to it, it talks to the machines through their nodes, and the other clients see and send the same jobs through the same interface. It uses the libraries of [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) and [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) and speaks to [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 📂 Repository Structure

```text
EH3D-SERVER/
├── src/          the server: settings, users, machines, planning, jobs, the interface and the websocket
├── tests/        the interface end to end, with the jobs running on the simulator
├── docs/         API.md and DEPLOY.md
├── deploy/       the systemd unit
├── tools/        ci_validate.py: manifest, versions, changelog and README checks
├── images/       banner
└── eh3d.project.json
```

## 🛠️ BUILD & RUN

You need a stable Rust toolchain, and EH3D-DRILLPRO and EH3D-ROUTEPRO next to this folder.

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Run it with `run.bat` (Windows) or `./run.sh` (Linux); it builds first if needed. While it has no user the console shows a set-up code: make the administrator with `POST /api/v1/setup`, log in, add the machine with its node's address and token, and make a token for each program. `server.cfg.example` lists the settings; `docs/DEPLOY.md` installs it as a service on the CM5.

## 🔗 Related Projects

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): the drilling program; its library plans and runs the drilling jobs
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): the routing program; its library plans the routing jobs
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): the node in the machine's box that the server talks to
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): the common parts of the family

## 📚 Documentation & Community

* [The API](docs/API.md): every request, the job and the view a client draws, the states and the websocket.
* [Running the server](docs/DEPLOY.md): the build, the first start, the service on the CM5 and the safety of the deployment.
* [Changelog](CHANGELOG.md)
* [Contributing](CONTRIBUTING.md)
* [Security policy](SECURITY.md)
* [Support](SUPPORT.md)

## 👤 AUTHOR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENSE

Software: GPL-3.0-or-later. See [LICENSE](LICENSE). Hardware designs of the suite use CERN OHL-S.
