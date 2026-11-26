# The single login

The fourth of the four common services (`docs/SERVICES.md`): one place where every application of EH3D logs in, with the role the person has and the application the login is for. The routes are under `/api/v1/auth`; the contract is the `auth` tag of `docs/openapi.yaml`. The code is `src/auth.rs` (the rules) and `src/api_auth.rs` (the routes).

**Nothing of the first version changes.** `POST /login`, `POST /logout`, `GET /session`, the users, the tokens of programs and their scopes work as they did. A login made with `POST /login` is the same kind of login with no application; a token from either login is a token for every route.

## The applications

A login says which application it is for. The server knows these by name; `auth_extra_apps` in `server.cfg` adds more and `auth_known_apps_only = false` accepts any well-formed name (lower case letters, digits and hyphens, up to 32):

| App | What it is |
|---|---|
| `bungard-gui` | the program of the drilling and routing machine |
| `studio` | EH3D-STUDIO, the web and desktop control center |
| `android`, `ios`, `watch` | the mobile apps and the watch |
| `hmi` | the panel next to the machine |
| `pcb`, `cam`, `laser`, `pnp`, `cad`, `tool-manager` | the design and production tools |
| `phoenix-cnc`, `phoenix-pnp`, `phoenix-pcb`, `phoenix-laser`, `phoenix-cad`, `phoenix-3dp` | the PHOENIX programs, each by its own name |
| `drillpro`, `routepro`, `isocam`, `vision-studio`, `stencilcam`, `autolevel`, `cam-debugger` | the programs of the Bungard drilling and routing machine and the CAM and vision tools |
| `cli` | scripts and the command line |

`GET /auth/apps` lists the ones the server accepts. The application is also the folder of the projects in the store (`docs/PROJECT_STORE.md`) and the default `app` of the jobs a login prepares (`docs/MACHINE_PLUGINS.md`). A name the server does not accept is refused with `422 bad_app`.

## Logging in

`POST /auth/login` with `{"name", "password", "app"}` gives `{"token", "name", "role", "app", "session_id", "expires_s"}`. The token is the bearer token of every request. The role is the one of the person (`viewer`, `operator`, `admin`); what each may do is in `docs/API.md`. A login lasts `session_hours`; it survives a restart of the server only with `persist_sessions`.

**The claims.** `GET /auth/session` says who the token is: `{"name", "role", "is_token", "app", "session_id", "created", "last_seen", "expires", "token_id", "template", "scopes"}`. For a person `app` and `session_id` are those of the login; a token of a program (`POST /tokens`) has no login, so they are null and its `scopes` are the ones it was given. An application reads its role from here and shows only what the role allows; the server enforces it anyway.

**Renewing.** `POST /auth/refresh` (a person) makes the login last again as long as a login does, with the same token, but never past `auth_refresh_max_hours` from the moment it was made (168 by default); after that the person logs in again. It answers `{"expires_s"}`, the seconds left. A login that expired answers `404 unknown_session`.

**Ending.** `POST /auth/logout` ends the login that asks; `POST /auth/logout-all` ends every login of the person. Changing the role or the password of a user, disabling or deleting them also ends their logins (as it did).

## Who a user is

Each user has, besides the name, an identifier (`uid` in `users.json`) that is given once and never again. What a person makes (projects and their revisions, shares, artifacts, uploads, tasks, jobs of the queue, locks and notifications) is filed under it, and the API shows the name. Deleting a user and making another with the same name gives a different person: nothing is inherited, and what the deleted one made is left to the administrators.

**Users from before the identifiers.** Their identifier is their name, which is what their things were already filed under, so nothing has to be moved or rewritten and they keep everything; `users.json` gets the field the next time it is saved. A user deleted before this and not made again leaves things that no new user gets. If a server is taken back to a version from before the identifiers and users are changed there, that version writes `users.json` without them: the users made in the meantime should then be checked, because they come back identified by their name.

What a token with scopes, and a token that expires, may do with users, tokens and keys is in `docs/API.md`: users are made, passwords set, users enabled and roles raised only from a login of a person or with a token that does not expire.

## The open logins

`GET /auth/sessions` lists the open logins of the caller, the newest first: `{"id", "name", "role", "app", "created", "last_seen", "expires", "ip", "current"}`. The `id` is not the token and cannot be used as one; `current` marks the login that asks; `ip` is the address of the connection (never a header a client can write). An administrator lists everybody's with `?all=true`. `DELETE /auth/sessions/{id}` closes one: a person closes their own (the phone that was lost), an administrator closes anybody's; the token of that login stops working at once. Another person's id answers `404 unknown_session`, the same as an id that never existed. `auth_max_sessions` limits how many logins a person has open at once (20 by default, 0 for no limit): the oldest is closed when a new one comes.

## Passwords and lockout

| Setting | Default | What it does |
|---|---|---|
| `password_min_len` | 8 | the shortest password (8 to 128; the longest is always 128) |
| `password_classes` | 1 | how many of lower case, upper case, digits and anything else a password needs (1 to 4) |
| `password_deny_name` | true | a password may not contain the name of the person (names of four characters or more), in any letter case |
| `auth_max_failures` | 5 | wrong passwords in a row that block a name |
| `auth_lockout_s` | 30 | the first block, in seconds; it doubles while the wrong passwords go on (up to 64 times) |
| `login_ip_limit` | 10 | wrong passwords in a row, with any names, that block an address |

The policy holds wherever a password is set: the first administrator, `POST /users`, `PUT /users/{name}` and `PUT /account`. A refused password answers `422 weak_password`. A blocked name or address answers `429 throttled` even to the right password; the time that the hash of a name that does not exist takes is the same as that of one that does, so the answer does not tell which names exist. A wrong old password in `PUT /account` counts like a wrong password of a login, so an open session cannot be used to guess it.

## The switches of security

`GET /auth/policy` (administrator) is what the security page of Studio shows: every switch of security of `server.cfg` with its value and whether it is as strict as recommended (`safe`), and in `attention` the keys that deserve a look. No secret is ever in it: no token, no hash, no key. The switches are `url_token`, `trust_forwarded_for`, `local_until_setup`, `persist_sessions`, `exit_on_panic`, `login_ip_limit`, `session_hours`, `token_default_days`, `auth_known_apps_only`, `auth_max_sessions`, `auth_max_failures`, `auth_lockout_s`, `auth_refresh_max_hours`, `password_min_len`, `password_classes`, `password_deny_name`, `audit_file`, `audit_denials`, `project_require_if_match` and `project_shared`. Each has a conservative default; the ones that stay as they were in the first version so that nothing breaks (`token_default_days = 0`, `password_classes = 1`) are the ones `attention` names on a new install. `design_run` is one more switch of the page: whether the server may run the design program (`docs/DESIGN_RUN.md`). `network_open` is another: the server listens on this computer only (`listen = 127.0.0.1:18200`) and the switch, off by default, opens it to the network (the same port on every address of the computer) without writing the file; it is kept at once and read when the server starts, which the list says in `restart_required`, and an address written in `listen` on purpose is never narrowed. `update_allow_unsigned` is the last one: the helper of the updates installs only releases signed by one of the allowed signers of the machine, and with this switch on it may also install one that has no signature when its hash is right (a signature that is published must verify in any case); the server writes the switch in the request it leaves for the helper. `update_keep_logins` is one more: the update of the server restarts it and the logins are kept in memory (unless `persist_sessions` is on), so the update would log everybody out and the pages could not follow it to the end; with the switch on, an update (or an undoing) of the server writes the SHA-256 of each login (never the token) to `sessions-update.json` of the data folder, rewritten while the logins change, and the server that starts takes them up only if it starts within `update_keep_logins_s` seconds (300 by default, 30 to 900) of the request; the file is used once and removed in every case. `persist_sessions` keeps its own, wider meaning. `mdns_announce` and `compat_api` are two more, both off by default and both taking effect at once: the first makes the server announce itself on the local network as `_eh3d._tcp` (`docs/DEPLOY.md`, "Finding the server on the network"), the second lets the routes of Moonraker and OctoPrint at the root of the address answer for the 3D printers that have a key for them (`docs/MACHINE_PLUGINS.md`, "Compatibility routes"). The list also has `editable`, the keys that can change while the server runs; the others are changed in the file and the server is started again.

`compat_write`, `slice_enabled` and `bambu_lan_enabled` are three more switches of the page, all off by default and as strict as recommended while off, and all take effect at once. `compat_write` lets the routes of Moonraker and OctoPrint at the root of the address WRITE (pause, resume and cancel a print, start one that waits, upload a file to the queue of a 3D printer): it matters only while `compat_api` is on, only a key made with `write: true` may use them, and an uploaded file waits in the queue for a person to start it (`docs/MACHINE_PLUGINS.md`). `slice_enabled` lets the server slice a mesh into G-code inside the limits of a printer profile (`POST /api/v1/slice`); the keys `slice_phoenix_bin`, `slice_timeout_s`, `slice_max_mb`, `slice_max_layers` and `slice_max_gcode_mb` of `server.cfg` bound it and are read when the server starts; the same switch lets `POST /api/v1/slice` give the program as a binary G-code file and `POST /api/v1/slice/bgcode` read one. `bambu_lan_enabled` lets the server reach a real Bambu Lab printer over the LAN (MQTT over TLS, an experiment): with it off only the simulated printer is served and a machine with the address of a real printer answers `409 bambu_lan_off`; with it on, the connection is made only when the fingerprint of the certificate of the printer is pinned in the settings of the machine, and `bambu_lan_port` and `bambu_lan_timeout_s` of `server.cfg` (read when the server starts) give the port of the broker and the time a connection and each answer may take.

`persist_sessions` keeps the logins across a restart of the server: with it on, the server writes to `sessions.json` of the data folder only the SHA-256 of each token with the user, the role, the application and the expiry (never the token, and the file is private to its account and written whole or not at all), and takes up at start the ones that have not expired and whose user still counts; a closed login or a deleted user is not written again, and a file that does not read means that everybody signs in again. It is off by default, takes effect at once (turning it on writes the logins that are open, turning it off removes the file) and is a switch of the security page of Studio.

`PUT /auth/policy` (administrator) changes one switch while the server runs: `{"key": "password_min_len", "value": 12}`. The keys that can change are `url_token`, `audit_denials`, `design_run`, `network_open`, `update_allow_unsigned`, `update_keep_logins`, `mdns_announce`, `compat_api`, `compat_write`, `slice_enabled`, `bambu_lan_enabled`, `persist_sessions`, `login_ip_limit`, `auth_max_failures`, `auth_lockout_s`, `auth_refresh_max_hours`, `password_min_len`, `password_classes` and `password_deny_name`, with the ranges `server.cfg` has; the new values are used by the next request (`network_open` is read when the server starts). A key that is not one of them, or a value that does not fit, is `422 bad_policy`. A value looser than recommended (`safe: false` in the answer) changes nothing and answers `409 weak_confirm_required` unless the body also says `"confirm_weak": true`; a stricter value needs no confirmation. The answer is the new policy. A change is written to the audit (`policy_change`: who, the key, the old and the new value) and kept in `policy.json` of the data folder, where it wins over `server.cfg` for that key the next time the server starts; `server.cfg` itself is never rewritten.

`GET /settings/contract` (administrator, scope `admin:policy`) is the contract of ALL the settings of `server.cfg`, the switches of this page and the rest: for each key its group, type, range, default and unit, whether a change is in force at once or read when the server starts, which route writes it (`PUT /auth/policy` for the 19 keys above, `server.cfg` for the others, which a `PUT /auth/policy` refuses with `bad_policy` and the name of the key), the role and scope it needs, whether it is a secret (its value is then never given back and the audit writes `***`), the kind of risk, and a label, a short help and a warning in eight languages. The answer is `docs/settings-contract.json` with `values` (the value in force of every key that is not a secret) and `server_version`. The answer to `PUT /auth/policy` also has `changed` (the key, the old and the new value, `apply`: `now` or `restart`, and `restart_required`). Studio and the apps draw their settings pages from it and keep a copy of the file with a parity guard (`docs/CLIENT_SYNC.md`). `GET /settings/values` is the light view for a viewer or an operator (the value in force, `origin` `file` or `override`, `editable`; no secret, no text, no path; `ETag`), read only: it changes nothing of the switches above.

`GET /auth/keys` (administrator) lists the tokens of programs by identifier, name, role, age, last use and expiry, and tells the SHA-256 fingerprint of the certificate named by `tls_cert_file` in `server.cfg` (the certificate the reverse proxy in front of the server shows; the server does not terminate TLS itself). `tls.sha256` is `null` when no certificate is named or the file holds none. No secret is ever in it.

## The audit

Every login (`login`: who, role, application, session, address), every failed login (`login_failed`: the code, never the password), every login refused for its application (`login_denied`), every logout, closing and renewal of a session, and with `audit_denials` (on by default) every request refused with `401` or `403` (`denied`: method, path, address and whether a token came; the query is never written, because a ticket may be in it). The lines go to the console and to `audit.jsonl`. No password and no token is ever written, whole or in part.

`GET /audit?since=&until=&who=&event=&limit=` (administrator) reads them back: the lines of `audit.jsonl` and of the older copy the file was set aside as, the newest first, as objects with `at` (seconds since 1970), `event` and the fields the event was written with. `limit` is 100 by default and at most 1000. With `audit_file` off the answer says `"enabled": false` and has no lines.

## What has been tried

All of it runs in the tests through the real router. The mobile apps, the panel and the watch have not been changed to use these routes yet.
