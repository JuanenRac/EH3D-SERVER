# Keeping the clients in step with the server

Studio, the Android and iOS apps, the panel next to the machine and the programs of the suite must be able to show, turn on and turn off **every switch and every setting of the
server**, and must follow each new one the server gets. Two files, both written from the code of this repository, carry that. A client copies them and checks its copy against
the server; it does not keep a list of its own.

| What the clients need | The file they copy | The generator that writes it and checks it | What a client checks |
| --- | --- | --- | --- |
| The sentence for every error code | `docs/error-codes.json` (code, status, description) | `tests/errors.rs` (`EH3D_WRITE_EXAMPLES=1 cargo test --test errors` writes it) | that each code has its text in the eight languages of the client |
| Every setting and switch, with its texts | `docs/settings-contract.json` (`eh3d.settings.contract/1`) | `python tools/gen_settings_contract.py` (`--check` in the CI) | that its copy is the file of the server and that every key it shows is in it |
| Every route | `docs/openapi.json` | `python tools/make_openapi.py` (`--check` in the CI) | that every route it calls is in the document |

## A job that goes on after a stop: the further questions

A G-code job that goes on may wait up to three times, each time with `pause.kind = "resume"`. When `pause.power_on` is `true`, confirming switches the spindle or the laser on again; when
`pause.plunge` is `true`, the head is over the point and confirming takes the tool down into the work. A
client says so on the screen for each of the two, sends back the `pause.question` it shows (an answer without it is refused with `409 question_required`, whatever `answer_needs_question` says) and never
answers it by itself. A client that edits machines offers the member `tool_head` (`laser`, `spindle` or empty) and, not sending it, leaves it as it was.

## The contract of the settings

`docs/settings-contract.json` lists every key of `server.cfg` (106 today): its group, type (`boolean`, `integer`, `string`, `list`), range, default and unit, whether a change is in
force at once (`apply: "now"`) or read when the server starts (`apply: "restart"`), the route that writes it, the role and scope it needs, whether it is a secret, the kind of
risk (`none`, `security`, `exposure`, `capacity`, `machine`, `data`) and three texts in each of the eight languages (en, es, de, fr, it, ja, zh and pt of Portugal): the `label`,
a short `help` and the `warning` (how a change is applied and what it risks). The same document, with the value in force of every key that is not a secret in `values` and the
version of the server in `server_version`, is the answer of `GET /api/v1/settings/contract` (administrator, scope `admin:policy`).

How a key is written is in its `write`:

* `write.via = "policy"`: `PUT /api/v1/auth/policy` with `{"key": ..., "value": ..., "confirm_weak": ...}`, administrator with `admin:policy`. A value looser than recommended needs
  `confirm_weak: true` (`409 weak_confirm_required`). A key or a value that does not fit is `422 bad_policy` and its `detail` names the key. The answer is the new policy and, in
  `changed`, the key, the old and the new value, `apply` and `restart_required`. The change is in the audit (`policy_change`: who, the key, the old and the new value; a secret
  key is written as `***`).
* `write.via = "file"`: the key is set in `server.cfg` and read when the server starts; there is no route. The contract lists it so that a client shows its value and says where it
  is set. A `PUT /auth/policy` that names it is refused with `bad_policy` and says so.

### The light view for the screens of the machines

The full contract needs an administrator with `admin:policy` and is some 250 KB, because of its texts in eight languages. A screen of a machine (the HMI of a node, which logs in
as the application `hmi` with the role of an operator) can read neither, so it keeps the copy of the contract and needs only the values. `GET /api/v1/settings/values` is for it:
read only, any login or token (viewer, operator or administrator; no scope), a few kilobytes and never more than 16 KiB, with no text, no secret and no path of the computer.

```json
{ "schema": "eh3d.settings.values/1", "server_version": "0.2.0", "contract_hash": "<sha-256>", "count": 89,
  "settings": { "session_hours": {"v": 12, "origin": "file", "editable": false},
                "password_min_len": {"v": 12, "origin": "override", "editable": true},
                "data_dir": {"hidden": true, "origin": "file", "editable": false} } }
```

* `v` is the value in force; `origin` is `override` when a change made through `PUT /api/v1/auth/policy` differs from `server.cfg`, and `file` otherwise (the built-in default
  included); `editable` says whether that route changes the key (only an administrator may). A key marked `secret` in the contract has `"v": "***"` and `"secret": true` and no
  `origin`, whatever its value; a key whose `format` is `path` has `"hidden": true` and no `v`. A long text or list is cut (`truncated: true`; the limits are in `read_light.limits`
  of the contract).
* `contract_hash` is the SHA-256 of `docs/settings-contract.json` with LF line ends (the full answer has it too). A client that keeps a copy of the contract compares it with the
  hash of its copy: the same hash, same keys and texts; another hash, the copy is out of date and the client says so (and an administrator reads the full contract).
* The answer has an `ETag` (the SHA-256 of its text in quotes) and `Cache-Control: private, no-cache`; `If-None-Match` with the tag, a list of tags or `*` is answered `304` with
  no body. A client asks every few seconds and reads the body only when the tag changed.
* `?keys=a,b` and `?group=id` narrow it; a key or group that the contract does not have, a query that does not decode and a query over 2048 bytes are `422 bad_request` and the detail
  names what was wrong. A parameter it does not know is ignored.
* A refusal (no login, a token that is not valid) is in the audit as `denied`, like every other route; a read that is allowed writes no line, like the other reads. The server has no
  limit of requests of its own for the reads; the limit of wrong passwords of the login is the only one, and it applies as it does everywhere.
* It opens no write: `PUT /api/v1/auth/policy` is as it was, administrator with `admin:policy`.

What a client regenerates: it copies the new `docs/settings-contract.json` (it has `read_light`; no key was changed or moved) and its parity guard passes as before. A client that has
no copy of the contract in its tree (the screens of the nodes keep the embedded one) may read `contract_hash` to know whether its copy is the one of the server. No new error code
was added (`bad_request` already exists), so `docs/error-codes.json` and the tables of texts do not change.

### How a client uses it

1. Copy `docs/settings-contract.json` of the server it is built for into its own tree (the same way it copies `docs/error-codes.json`).
2. Draw the settings page from it: the groups in order, then the settings of each group, with `label[lang]`, `help[lang]` and `warning[lang]`; a switch for a `boolean`, a number with
   `min`, `max` and `unit` for an `integer`, a text for a `string`, a list for a `list`. Show `warning` before a change whose `risk` is not `none` or whose `apply` is `restart`.
3. Read the values from `GET /api/v1/settings/contract` (`values`; an administrator) or, for any other role, from `GET /api/v1/settings/values`, and write with the `write` of each entry. Do not write a key the contract does not list, and do not hide one it
   does list.
4. Keep a **parity guard** like the one of the error codes: a test that fails when the copy of the contract differs from the file of the server (it compares the files, or runs
   `python tools/gen_settings_contract.py --check` of the server it is next to), and when a key the client shows is not in the contract or a key of the contract is not shown. A
   client may hide a key on purpose only by naming it in a list of exceptions with its reason.

### What the clients take from the nodes of the pick-and-place machines and from the authorization of a job

Six keys are new in the contract (`pnp_nodes_enabled`, `pnp_nodes_max`, `pnp_node_poll_s`, `pnp_node_silence_s`, `pnp_node_timeout_s` and `work_require_authorization`, all with
`write.via = "file"`, so a client shows them and says where they are set), and the group `pnp_nodes` is new. A client copies the new `docs/settings-contract.json` and
`docs/error-codes.json` (nine codes: `pnp_nodes_off`, `unknown_pnp_node`, `bad_pnp_node`, `pnp_node_exists`, `pnp_nodes_full`, `pnp_node_disabled`, `pnp_node_not_ready`,
`stale_authorization` and `authorization_required`), gives each its sentence in the eight languages, and regenerates its client of `docs/openapi.json`. Then:

* the screens of the machines and Studio read `GET /api/v1/pnp/nodes` (viewer, `machines:view`) and show each node with its `health` and `ready`; the fleet has `pnp_nodes` while the
  switch is on; the websocket sends `{"type":"pnp_node","id","health","ready"}` when a node changes (a client that listens asks the list again, it does not poll the nodes);
* only an administrator registers, changes or removes a node, an operator can ask a node now (`POST /pnp/nodes/{id}/poll`), and the token is never in an answer (`********`);
* every job of the generic queue has `toolpath_sha256` and `authorization`; a client that starts a job of a laser, a pick-and-place machine or a CNC machine shows the hash, lets
  a person with `jobs:authorize` authorise it (`POST /work/jobs/{id}/authorize`) and may send the same hash with the start (`409 stale_authorization` when the job is not the one
  that was shown; `409 authorization_required` while `work_require_authorization` is on and there is none; `409 pnp_node_not_ready` when a node of the machine is not ready).

## The rule for the server

**A key, a switch or a route that is added to the server needs its entry in the contract in the same commit.**

* A new key of `server.cfg` (an arm of `parse` in `src/config.rs`): its default in `ServerConfig::default()`, its line in `server.cfg.example`, its entry in `SETTINGS` of
  `tools/settings_contract_texts.py` (group, risk, label and help in the eight languages), then `python tools/gen_settings_contract.py` and the new `docs/settings-contract.json`.
* A key that becomes a switch of the security page (`policy::KEYS`): the same, and the generator marks it `write.via = "policy"` by itself; name it in `docs/AUTH.md`.
* A new route: `tools/make_openapi.py` (and `docs/ROUTES.md`), as before; a new error code: `src/errors.rs`, `docs/API.md`, `docs/error-codes.json` and its eight texts where the clients keep them.
* The CI fails if it is forgotten: `python tools/gen_settings_contract.py --check` (a key of the code that is not in the contract, a key of the contract that the code does not
  have, a missing language, an example file or a default that disagrees, a file on disk that is not what the sources give), and the tests `src/settings.rs` and
  `tests/settings_contract.rs`, which read the source of `src/config.rs` and `src/policy.rs` and the contract itself.

A key that holds a secret is marked `"secret": true` (`SECRET_KEYS` in the same file): the server never gives its value back and the audit writes it as `***`. No key of `server.cfg`
holds a secret today; a password, a token or a key never goes into the file.
