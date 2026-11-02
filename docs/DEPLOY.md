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

`server.cfg` (or the file given with `--config`) holds `listen`, `data_dir`, `session_hours`, `max_body_mb`, `keep_jobs` and the settings of G-code jobs (`gcode_finish_wait_s`, `grbl_line_timeout_s`, `grbl_homing_timeout_s`, `grbl_jog_timeout_s`), `resume_margin_lines` (16), `local_until_setup` (off: with it on, a server that has no user yet answers only 127.0.0.1, so the first administrator is made from the computer itself), `url_token` (off) and `login_ip_limit` (10, 0 turns it off; behind a reverse proxy the address is the proxy's, so turn it off there); see `server.cfg.example`. The default port is **18200**, which is not one of ARMOR's (18080 to 18090) nor HYDRA-UMC's, so the three servers can share the CM5.

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

The unit stops the running jobs when the service stops (`systemctl stop` sends SIGTERM and the server stops the machines first, as it does on Ctrl-C; it has 15 seconds), restarts the server if it dies, and keeps it from writing anywhere but its data folder and the folder of the updates. It is also hardened: the files it writes can be read by its user only (`UMask=0077`), it belongs to the group `dialout` so it can open the serial ports of GRBL controllers, it cannot see the home folders, change the kernel, take new privileges or open sockets other than IP and unix ones. `PrivateDevices` is left off on purpose, because a controller on `/dev/ttyUSB0` is a device. If your system has no group `dialout`, change `SupplementaryGroups` to the group that owns the serial ports. A panic of one thread does not leave the server half dead: a lock that the panic left poisoned is taken over again and `GET /api/v1/health` says `degraded` from then on, which is the signal to look at the journal and restart.

## HTTPS

The server does not terminate TLS: put Caddy in front of it. With its own certificate authority (`tls internal`) it needs no domain and no internet, which fits a machine in a workshop. The file to copy is `deploy/Caddyfile.example`.

1. In `server.cfg` make the server listen only on this computer, so that the only door from the network is the proxy: `listen = 127.0.0.1:18200`.
2. Install Caddy (`sudo apt install caddy`), copy `deploy/Caddyfile.example` to `/etc/caddy/Caddyfile`, put the name or the address of the computer where it says `cm5.local`, and `sudo systemctl reload caddy`. Caddy makes its certificate authority the first time.
3. Make the devices trust it: copy `/var/lib/caddy/.local/share/caddy/pki/authorities/local/root.crt` to each device that opens the server and install it as a trusted authority (the browser, the phone). Open `https://cm5.local/`; the Studio, the API and the websocket of the events are all there.
4. Close the old port to the network (the line of step 1 does it) and change the address in the programs and the apps from `http://cm5:18200` to `https://cm5.local`.

Behind the proxy every connection reaches the server from `127.0.0.1`, so `login_ip_limit` (wrong passwords counted by address) would count all the people together: set `login_ip_limit = 0`; the slowdown of wrong passwords by name stays. The configuration is written down and checked by `tests/test_deploy_files.py`, but it has not been run on the CM5: nothing of this has been tested on a real installation.

## Backups

The data folder is the whole memory of the server. A systemd timer copies it every night to `/var/backups/eh3d-server` as one `.tar.gz` (readable by its owner only, checked by reading it back, the files that are being written left out) and keeps the newest 14:

```bash
sudo install -d -o eh3d -m 700 /var/backups/eh3d-server
sudo install -d /usr/local/lib/eh3d
sudo install deploy/eh3d-backup.py /usr/local/lib/eh3d/
sudo install -m 644 deploy/backup.cfg.example /etc/eh3d-server/backup.cfg      # source, dest, keep
sudo install deploy/eh3d-backup.service deploy/eh3d-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now eh3d-backup.timer
sudo systemctl start eh3d-backup.service && ls -l /var/backups/eh3d-server       # one now, to see it works
```

To restore: stop the server, move the data folder aside, unpack a copy (`tar -xzf eh3d-data-....tar.gz -C /var/lib/eh3d-server --strip-components=1`), give it back to the user `eh3d` and start the server. The copies hold the password hashes and the tokens of the nodes: keep them as private as the data. A copy on the same disk does not protect against the disk: copy them elsewhere too. `python3 -I tests/test_backup.py` tests the script without systemd.

## The pages of Studio

`studio_dir = /opt/eh3d-studio` in `server.cfg` makes the server serve the pages of EH3D-STUDIO (its `dist` folder) at `/` of the same address, so the configuration panel opens at `http://cm5:18200/` with no other server and no cross-origin setting. The API stays under `/api/v1/`.

## Safety of the deployment

- The server is for the house or a VPN into it. It does not terminate TLS: put the proxy of the HTTPS section in front before anything outside the network reaches it.
- The data folder holds the users (Argon2 hashes), the tokens (SHA-256 hashes), the machines with the tokens of their nodes, and the jobs. Back it up (see Backups) and keep it private.
- A job never starts by itself, and the node stops on its own when its link goes quiet (unless its setting for finishing a held job is on), so a crashed server does not leave a machine moving.

## Health check

`GET /api/v1/health` needs no login and says the name, the version, `api_revision` (the revision of the interface of `docs/API.md`) and `status`: `ok`, or `degraded` when the data folder does not take a file (a full or read-only disk) or a thread panicked while it held a lock. Whoever is logged in also gets `uptime_s`, `storage`, `recovered_locks` and `features` (serial port support, the Studio, the updates, `url_token`, `local_until_setup`). A monitor can poll it and alert on anything but `ok`.

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

## A GRBL controller on a serial port

A machine of the kind `grbl` (`{"kind":"grbl","port":"/dev/ttyUSB0","baud":115200}`) takes free G-code jobs through a serial port of the computer the server runs on. The release binary has no serial port support; build the server with it for such a machine:

```bash
cargo build --release --features serial
```

The user the service runs as must be allowed to open the port (on Linux the group `dialout`). Without the feature a G-code job for a `grbl` machine is marked failed when it starts, with a message that says so. `gcode_finish_wait_s` in `server.cfg` is how long a job waits for the machine to be still after its last line.

A GRBL machine answers an ordinary line at once, but a homing cycle (`$H`) and a jog (`$J=...`) answer only when the machine has finished moving, so each kind of line has its own wait in `server.cfg`: `grbl_line_timeout_s` (5 seconds by default), `grbl_homing_timeout_s` (120) and `grbl_jog_timeout_s` (60), each from 1 to 3600. Give the homing more than your machine takes. When a line does not end in time the machine is stopped (a homing cycle is reset, a jog is cancelled, anything else is held, and the job then stops the controller) and the job is marked `failed` with the line that did not end; the machine is never left moving.

## Who made the update

The update helper checks every download against the SHA-256 that the release publishes, which proves the file arrived whole. Because the hash is published in the same place as the file, whoever can replace the file can replace the hash too. To also know who made it, give the machine a file of allowed signers and sign the releases:

1. The owner makes a key pair that is only for releases (`ssh-keygen -t ed25519 -f eh3d-release`) and keeps the private half off the servers.
2. On the machine, `/etc/eh3d-update.signers` holds one line: `eh3d-release <the contents of eh3d-release.pub>`.
3. For every file of a release, the owner signs it (`ssh-keygen -Y sign -f eh3d-release -n eh3d-release FILE.tar.gz`) and publishes `FILE.tar.gz.sig` next to it.

From then on the helper installs only a release that has the `.sig` and whose signature verifies with `ssh-keygen -Y verify`; a release without it, or signed by another key, is refused and nothing is replaced. Without the signers file the helper works as before and its log says that the files are not checked for who made them.
