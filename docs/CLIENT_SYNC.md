# Keeping the clients in step with the server

Studio, the Android and iOS apps, the panel next to the machine and the programs of the suite must be able to show, turn on and turn off **every switch and every setting of the
server**, and must follow each new one the server gets. Two files, both written from the code of this repository, carry that. A client copies them and checks its copy against
the server; it does not keep a list of its own.

| What the clients need | The file they copy | The generator that writes it and checks it | What a client checks |
| --- | --- | --- | --- |
| The sentence for every error code | `docs/error-codes.json` (code, status, description) | `tests/errors.rs` (`EH3D_WRITE_EXAMPLES=1 cargo test --test errors` writes it) | that each code has its text in the eight languages of the client |
| Every setting and switch, with its texts | `docs/settings-contract.json` (`eh3d.settings.contract/1`) | `python tools/gen_settings_contract.py` (`--check` in the CI) | that its copy is the file of the server and that every key it shows is in it |
| Every route | `docs/openapi.json` | `python tools/make_openapi.py` (`--check` in the CI) | that every route it calls is in the document |

## The contract of the settings

`docs/settings-contract.json` lists every key of `server.cfg` (89 today): its group, type (`boolean`, `integer`, `string`, `list`), range, default and unit, whether a change is in
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

### How a client uses it

1. Copy `docs/settings-contract.json` of the server it is built for into its own tree (the same way it copies `docs/error-codes.json`).
2. Draw the settings page from it: the groups in order, then the settings of each group, with `label[lang]`, `help[lang]` and `warning[lang]`; a switch for a `boolean`, a number with
   `min`, `max` and `unit` for an `integer`, a text for a `string`, a list for a `list`. Show `warning` before a change whose `risk` is not `none` or whose `apply` is `restart`.
3. Read the values from `GET /api/v1/settings/contract` (`values`) and write with the `write` of each entry. Do not write a key the contract does not list, and do not hide one it
   does list.
4. Keep a **parity guard** like the one of the error codes: a test that fails when the copy of the contract differs from the file of the server (it compares the files, or runs
   `python tools/gen_settings_contract.py --check` of the server it is next to), and when a key the client shows is not in the contract or a key of the contract is not shown. A
   client may hide a key on purpose only by naming it in a list of exceptions with its reason.

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
