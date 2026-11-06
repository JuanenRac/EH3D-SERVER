# The design run

The pages for design and factory work of Studio ask the server to run the design program on a board of a project and to keep the report with the project. The server does not do the design work itself: it runs the program that is installed on the computer (`phoenix`), and only when the settings say so.

## The request

`POST /api/v1/apps/pcb/jobs` (operator, scopes `jobs:prepare` and `projects:write`):

```json
{ "kind": "check", "project": "<id of a project of the pcb application>", "file": "boards/main.json", "options": {} }
```

* `kind` is `check`, `bom`, `fabpack` or `panel`.
* `file` is the board file of the project (`options.file` also works). A name that is not safe is `422 bad_filename`, one the project does not have is `404 unknown_file`, and a project the caller cannot see is `404 unknown_project`.
* `options` is empty for `check`, `bom` and `panel`. `fabpack` takes `profile` and `lang` (short plain names), `panel_boards` (1 to 1000) and `dnp` (a list of designators). Anything else, or a value that could be taken for another argument of the program, is `422 bad_design_request`.

The answer is `201` with a job of the generic queue (`GET /api/v1/work/jobs/{id}`; plugin `phoenix`, application `pcb`). It is `running` at once and ends `done` or `failed`; `message` says how. The stream of events tells `job.created` and `job.state` like for any other job. A job that is open when the server stops is marked `failed` when it starts again.

## The report

The report is written to the project as `reports/<kind>.json`, through the project store: it has versions, the quotas of the store and the editing lock (a project that somebody else holds locked fails the job with `the report was not kept (locked)` in its message). The file is a JSON object:

| Field | Meaning |
|---|---|
| `kind`, `file` | what was run, and on which file |
| `ok` | the program ended by itself with code 0 |
| `exit_code` | the code of the program; `null` when it was stopped |
| `timed_out`, `truncated` | the time limit stopped it, or its output passed the limit and the rest was dropped |
| `report` | for `check`, `bom` and `panel`, the JSON the program wrote (they are run with `--json`); otherwise `null` |
| `text` | what the program wrote, when it was not JSON (`fabpack` is a dry run: it writes no package and says what it would write) |
| `stderr` | the first 4000 characters of what the program said on its error output |

## The settings

The run is off by default and the program is never found by itself:

| Key | Default | Meaning |
|---|---|---|
| `design_run` | `false` | the server may run the program. It is a switch of the security page (`GET /auth/policy`, `PUT /auth/policy`): turning it on while the server runs needs `confirm_weak: true` |
| `design_program` | not set | the full path of the program. A bare name is refused when the file is read, so the search path of the system is never used |
| `design_timeout_s` | `120` | the program is stopped after this many seconds (1 to 3600) |
| `design_max_output_kb` | `4096` | the most it may write to each output, in kilobytes; the rest is dropped |

Without both `design_run` and `design_program` the route answers `501 design_run_unavailable`, and nothing else is looked at, so a client can tell it is not set up.

## What the server does to run it

* No shell: the program is started directly with its arguments, never through a command line.
* An empty environment (on Windows only `SystemRoot`), no input, and a folder of its own under `design-scratch` in the data folder that holds nothing but a copy of the board file; the folder is removed when the run ends.
* A time limit and an output limit, as above. A program that does not start fails the job (`the design program did not start`) and no report is written.
* The audit has a line for the request (`design_job_create`) and one for the end of the run (`design_run`: who, project, kind, result and milliseconds). Nothing of the board is written to it.

What the program can do with the copy of the board is up to the program: the server does not confine it beyond the above. Install only a program you trust, and leave `design_run` off where nobody needs it.
