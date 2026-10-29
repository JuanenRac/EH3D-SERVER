# Changelog

All notable changes to this project are documented here.

## [Unreleased]

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
