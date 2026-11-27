# Safe degradation

The rule: **when something goes wrong, the server goes towards what is safer, never towards what is looser.**

An error path may refuse, stop, latch or keep the old value. It may never relax a restriction, and it may never go on as if the thing had worked.

## What it means

| When this happens | The server does | It never does |
|---|---|---|
| A plugin fails while a job runs (a heater sensor is lost, a driver breaks) | Ends the job `failed`, latches the machine, refuses new jobs on it (`409`) | Ends the job `done`, or lets the next job start on a machine that has just failed |
| A value of a safety setting is not understood (`"yes"` for an interlock, `null`, a number out of range) | Refuses the change (`4xx`) and keeps the value in force | Reads it as "off", "closed" or "true", or keeps half of it |
| An interlock input says open | Refuses the job (`409 enclosure_open`) | Starts and hopes the input is wrong |
| A write is refused (wrong role, a looser policy value without `confirm_weak`, a key that does not exist) | Changes nothing and keeps nothing on disk | Applies it in memory only, or applies the part that was allowed |
| A login is closed, expired or unknown | Answers `401`; the latch, the policy and the machines stay as they were | Treats it as the anonymous user with some rights, or as the last user |
| The emergency stop is asked for a machine that does not exist | `404` | Answers that it stopped |
| A stopped GRBL machine does not say in time that it stands still, or goes into alarm | Writes the reset all the same, and writes down that the position of the machine is not known | Takes the position it last had as good, or goes on by itself from it |
| A job that goes on finds the controller not idle, unreadable or away from where the job left it | Fails the job with the reason before anything is sent | Moves the machine or starts the spindle to find out |
| The file of the changes of the security page (`policy.json`) is missing | Starts from `server.cfg`, whose defaults are the conservative ones | Starts with a switch on |
| That file has a value of the wrong type or a key this version does not know | Leaves that value out and keeps the one of `server.cfg` | Reads `"yes"` or `1` as true |
| That file cannot be read at all | Does not start (`--check` exits 2 and says which file) | Guesses, or replaces it with defaults and overwrites the old file |

## For the code

* A function that decides whether something is allowed answers `false` or an error when it cannot tell. `Option` and `Result` are turned into permission only on the `Some` and `Ok` side.
* A parser of a safety setting takes exactly the type it documents (`as_bool`, not "truthy"), and an unknown key or value is an error (`config::parse`, the checks of each plugin).
* A latch is lifted only by the route made for it, by the role that may, and a refused attempt leaves it in place.
* A change is validated whole before any of it is applied, and written whole or not at all (`store::write_durable`).
* A new error path gets a test in `tests/safe_degradation.rs` that fails if it loosens a restriction or reports success.

## What checks it

`tests/safe_degradation.rs` goes through the error paths above end to end through the real router (and `AppState::open` for the policy file). `tests/machine_feasibility.rs` makes every simulated machine configuration and checks that each starts, answers, keeps limits that make sense and says it is only a simulator. Both run in the CI with the rest of `cargo test`.

## Exit codes of the command line

`eh3d-server --check [--config FILE]` verifies the file of the settings, the changes of the security page and the contract of the settings without starting the server and without writing anything.

| Code | Meaning |
|---|---|
| `0` | Every check passed (or the server stopped as asked) |
| `1` | The server could not start (the port is taken, the data cannot be opened) |
| `2` | The verification was not passed: a check of `--check` failed, or the command line makes no sense |

A check that cannot be made counts as not passed: "I could not tell" is never "ok".
