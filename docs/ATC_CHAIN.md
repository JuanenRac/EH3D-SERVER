# Chained tool changes (`rack`)

A machine with the automatic tool changer keeps its tools in a bar (the rack): place 0 is the test pin, places 1 to 14 hold the tools. A drilling job followed by a routing job on such a machine can be **chained**: the last drill is not put back in its place, and the routing job makes one change from the place of that drill to the place of the first cutter, without asking again to switch the spindle on. The tools of a job do not have to be in the bar in the order the job uses them either: the person says where each one is.

The server plans this for the requests that ask for it. This page is the contract; the schemas are `JobRackV2` and `RackSlot` in `docs/openapi.yaml`, and the answer is `JobResponse` with its `rack_applied`.

## The request

`POST /api/v1/machines/{id}/jobs` and `POST /api/v1/machines/{id}/preview`, for `"kind": "drill"` and for `"kind": "route"`, take two optional fields:

```json
{
  "kind": "route",
  "request_version": 2,
  "rack": {
    "first": 2,
    "carry_in": true,
    "carry_out": false,
    "slots": [ { "tool": 1, "place": 6 } ],
    "carry_slot": 4
  }
}
```

| Field | Default | Meaning |
|---|---|---|
| `request_version` | absent (the first version) | `2` when `rack` is present. A number above 2 is taken as 2 and the answer says so (see below). 0 is refused with `bad_request`. |
| `rack.first` | `0` | How many places of the rack the tools of the jobs before this one take; the tools of this job wait from the next place. |
| `rack.carry_in` | `false` | The chuck comes holding the last tool of the job before: the first change of this job takes it to its place and picks the first tool of this one. Needs `first` above 0 (as in the programs, with `first` 0 there is nothing to carry). |
| `rack.carry_out` | `false` | This job leaves its last tool in the chuck for the next one: it does not put it back. |
| `rack.slots` | `[]` | The place of each tool of this job, as `{ "tool": n, "place": p }`. A tool that is not listed is taken from the place the order of the job gives it. |
| `rack.carry_slot` | `first` | With `carry_in`, the place of the tool the chuck comes holding. |

A request without `rack`, and a request of the first version (no `request_version`, or 1), is planned exactly as before: the same steps, the same view. A `rack` in a request of the first version is left out and the answer says so.

## What the server does

1. Judges the `rack` against the request and the machine (below).
2. Plans the job on the rack of the machine file (`ATC` places, `Tool change = Auto`): each tool is taken from its place, the tool in the chuck is put back in its own place first, the last tool is put back at the end unless `carry_out`, and a job that comes with `carry_in` does not ask for the spindle again.
3. Answers with `rack_applied`.

A machine that changes its tools by hand has no rack to plan with: the job is planned as it would be without the field and the answer says `rack_applied: false` with the warning `rack_no_changer`.

## The answer

The job (`JobResponse`) and its `view` carry `rack_applied` only when the request had a `rack`:

| `rack_applied` | Meaning |
|---|---|
| `true` | The plan used the `rack`. |
| `false` | The `rack` was left out; `view.warnings` says why. |
| absent | The request had no `rack`. |

The warnings are entries of `view.warnings` (`code`, `values`, and `detail` when a word is needed):

| `code` | When | `values` / `detail` |
|---|---|---|
| `rack_needs_v2` | `rack` came in a request of the first version | |
| `rack_no_changer` | the machine changes its tools by hand | |
| `rack_unknown_field` | a field of `rack` that this server does not know (from a later version); it is ignored | `detail`: the name of the field |
| `request_version_newer` | `request_version` above 2; the request is read as the second version | `values`: the version asked for |

## What is refused

A `rack` that does not fit is refused with `422 bad_rack` before any job exists; the `detail` is a stable sentence:

| Case | `detail` |
|---|---|
| a place given to two tools | `rack.slots: place P is given to two tools` |
| a tool given two places | `rack.slots: tool T is given two places` |
| place 0 (the test pin) given to a tool | `rack.slots: place 0 is the test pin, not a place for tool T` |
| `carry_slot` without `carry_in` | `rack.carry_slot needs rack.carry_in` |
| `carry_slot` 0 | `rack.carry_slot: place 0 is the test pin` |
| `carry_slot` that is also the place of a tool | `rack.carry_slot: place P is also the place of a tool of this job` |
| `first` above the limit | `rack.first may be up to N places` |
| more entries in `slots` than the limit | `rack.slots may name up to N places` |
| a place the machine file does not define | `place P of the rack is not defined in the configuration file of the machine` |
| a place the machine does not reach | `place P of the rack is out of the reach of the machine` |
| more tools than the rack holds | `the rack of the machine holds N tools at the most` |

A field of the wrong type (`"first": "two"`) is a request that does not read (`422 bad_request`); only a body that is not JSON at all is `400`.

The limit N is `rack_max_slots` of `server.cfg` (1 to 14, default 14).

## Who may

The same as for any job: an operator or an administrator whose scopes include `jobs:prepare` (to plan) and `jobs:start` (to start); a viewer gets `403`. The audit line of the job creation (`event=job_create`) carries `rack=applied` or `rack=left_out` when the request had a `rack`.

## The programs

EH3D-DRILLPRO, EH3D-ROUTEPRO and the window that runs both send `request_version: 2` and the `rack` when a job is one of a chain or has places for its tools, and read `rack_applied`: an older server that refuses the field gets the job again without it, and a server that answers without `rack_applied: true` is told to the person in the run window.
