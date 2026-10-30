# Running the server

## Build

The server uses the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO, so the three repositories sit next to each other:

```text
EH3D-DRILLPRO/
EH3D-ROUTEPRO/
EH3D-SERVER/
```

`build.bat` (Windows) or `./build.sh` (Linux) runs `cargo build --release`; the program is `target/release/eh3d-server`. `cargo test` runs the unit tests and the tests of the interface, which use the simulator. On a CM5 (aarch64 Linux) build it on the machine itself, or cross-compile with the target `aarch64-unknown-linux-gnu`; nothing needs the serial-port library to be built with system packages.

## Settings

`server.cfg` (or the file given with `--config`) holds `listen`, `data_dir`, `session_hours`, `max_body_mb` and `keep_jobs`; see `server.cfg.example`. The default port is **18200**, which is not one of ARMOR's (18080 to 18090) nor HYDRA-UMC's, so the three servers can share the CM5.

## First start

The server prints a set-up code on its console while it has no user:

```text
SETUP: this server has no user yet. Make the administrator with POST /api/v1/setup and the set-up code 12345678
```

```bash
curl -X POST http://cm5:18200/api/v1/setup -H 'content-type: application/json' \
     -d '{"code":"12345678","name":"juan","password":"a-long-password"}'
```

Then log in, add the machine (`docs/API.md`) with the address and the token of its node, and make a token for each program that will send jobs.

## On the CM5, as a service

```bash
sudo useradd --system --home /var/lib/eh3d-server eh3d
sudo install -d -o eh3d /var/lib/eh3d-server /etc/eh3d-server /opt/eh3d-server
sudo install target/release/eh3d-server /opt/eh3d-server/
sudo install -m 600 -o eh3d server.cfg /etc/eh3d-server/server.cfg     # with data_dir = /var/lib/eh3d-server
sudo install deploy/eh3d-server.service /etc/systemd/system/
sudo systemctl enable --now eh3d-server
```

The unit stops the running jobs when the service stops (the server stops the machines first), restarts the server if it dies, and keeps it from writing anywhere but its data folder.

## The pages of Studio

`studio_dir = /opt/eh3d-studio` in `server.cfg` makes the server serve the pages of EH3D-STUDIO (its `dist` folder) at `/` of the same address, so the configuration panel opens at `http://cm5:18200/` with no other server and no cross-origin setting. The API stays under `/api/v1/`.

## Safety of the deployment

- The server is for the house or a VPN into it. It does not terminate TLS yet: put a reverse proxy with HTTPS in front before anything outside the network reaches it.
- The data folder holds the users (Argon2 hashes), the tokens (SHA-256 hashes), the machines with the tokens of their nodes, and the jobs. Back it up and keep it private.
- A job never starts by itself, and the node stops on its own when its link goes quiet (unless its setting for finishing a held job is on), so a crashed server does not leave a machine moving.

## Updates from GitHub

With `updates_dir` in `server.cfg` the Studio (Updates page, for an administrator) checks the releases of GitHub for newer versions of the server and of the Studio and updates them. The server does not talk to GitHub and does not replace itself: it leaves a request in a shared folder, and a small helper started by systemd does the rest.

```bash
sudo install -d -o eh3d /var/lib/eh3d-updates                      # the shared folder: updates_dir = /var/lib/eh3d-updates
sudo install -d /usr/local/lib/eh3d
sudo install deploy/eh3d-update.py /usr/local/lib/eh3d/
sudo install deploy/eh3d-update.service deploy/eh3d-update.path /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now eh3d-update.path
```

The helper runs as root (it replaces `/opt/eh3d-server` and `/opt/eh3d-studio` and restarts the service), takes the file of the release (`eh3d-server-VERSION-linux-ARCH.tar.gz`, `eh3d-studio-VERSION.tar.gz`), checks it against the SHA-256 that the release publishes beside it, refuses an archive with names outside its folder, and keeps the old folder as `.previous` so that a bad update can be put back by hand. `python3 -I tests/test_update_helper.py` tests it without network or systemd.
