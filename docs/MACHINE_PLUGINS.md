# Machine plugins, the queue of jobs and the stream of events

The third of the four common services (`docs/SERVICES.md`). Until now the server knew one kind of machine, the Bungard drilling and routing machine, and its routes (`/api/v1/machines`, `/api/v1/jobs`) were written for it. An application that is not for that machine (a laser, a pick-and-place, the stencil printer) needs the same things: a machine to choose, a job to prepare, start, hold and stop, how far it is, and to be told when it ends. This service gives them without the server knowing which machine is behind.

The contract is the `work` tag of `docs/openapi.yaml`. The code is `src/plugins.rs` (the plugins), `src/work.rs` (the machines of the plugins, the queue, the history and the notifications), `src/feed.rs` (the events) and `src/api_work.rs` (the routes).

**Nothing of the first version changes.** `/api/v1/machines`, `/api/v1/jobs` and the websocket `/api/v1/events` answer as they did, with the same tests. The generic queue is a new family beside them.

## The plugin

A machine plugin is a value of a type that implements `MachinePlugin` (`src/plugins.rs`):

| Operation | What it does |
|---|---|
| `kind`, `title` | the short name (`bungard`, `demo`) that `plugin` says in machines and jobs, and a line of text |
| `capabilities` | what the machines can do: the kinds of request they take, whether they can be held, how progress is counted |
| `machines` | the machines of this kind, with their state (`idle` or `busy`) |
| `prepare` | checks a request and makes a job ready on a machine; nothing moves. Gives the id the plugin knows the job by and what a client draws of it |
| `start`, `pause`, `resume`, `stop` | the operations on a job; a plugin that cannot do one answers `not_supported` |
| `status` | the state of a job (`queued`, `running`, `paused`, `done`, `failed`, `cancelled`) and how far it is (`done` of `total`) |
| `discard` | a job that was prepared and will not start: the plugin lets go of what it kept |
| `manages_machines`, `check_settings` | whether the machines of the plugin are made with `POST /work/machines`, and what their settings may be |

To add a plugin: write the type, add it to `Registry::standard()` and describe it in `docs/openapi.yaml` through `tools/make_openapi.py`. The generic layer, the history, the events, the notifications, the roles and the audit come with it.

### `bungard`

The machines of DRILLPRO and ROUTEPRO: the ones of `/api/v1/machines`, whatever their connection (a node, a GRBL controller, the simulator). `prepare` takes the same requests as `POST /machines/{id}/jobs` (`drill`, `route`, `gcode`, with the `rack` of the second version), plans them with the same libraries and puts the job in **the same book** as the old route does, so `GET /jobs/{native}` shows it and `POST /jobs/{native}/confirm` answers its dialogs. `start` and `stop` are the old start and cancel; a free G-code job can be held and resumed, a drilling or routing job cannot (`501 not_supported`). The dialogs of the operator (change the tool, switch the spindle on) show as `running` with the message `waiting for the operator (...)` and make a notification; they are confirmed with the old `/jobs/{native}/confirm`. Progress is counted in items.

The generic queue lists the jobs made through it; a job made with the old routes stays in the old book and is not in the queue.

### `demo`

A machine with no hardware, for the tests of the applications and of the server. Its machines are made with `POST /work/machines` (`"plugin": "demo"`, settings `{"step_ms": 20}`, 0 to 3600000). A job is `{"kind": "demo", "steps": 10}` (1 to 100000); it takes `step_ms` milliseconds per step, so with `step_ms = 0` it ends as soon as it runs and with a long step it stays where it is put. It can be held and resumed (the clock stops while it is held). It lives in memory: a demo job that was open when the server stopped is marked as failed.

## The machines

`GET /plugins` lists the plugins. `GET /work/machines` lists the machines of every plugin, each with its `plugin`, its state, the job of the generic queue that has it and its capabilities. `POST /work/machines` (administrator, scope `machines:manage`) makes a machine for a plugin that keeps its machines here; its id may not be the id of any other machine, of any plugin (`409 machine_exists`). `PUT` and `DELETE` change and remove it (not while a job is open on it, `409 machine_busy`). The machines of `bungard` are made, changed and removed with the old routes.

## The queue and the history

| Request | Role | Scope |
|---|---|---|
| `POST /work/jobs` prepare a job: `{"machine", "request", "name", "app"}` | operator | `jobs:prepare` |
| `GET /work/jobs` list (filters `machine`, `state`, `app`, `mine`; `limit`, `offset`); `GET /work/jobs/{id}` read with its history | viewer | `machines:view` |
| `POST /work/jobs/{id}/start`, `/pause`, `/resume` | operator | `jobs:start` |
| `POST /work/jobs/{id}/cancel`, `DELETE /work/jobs/{id}` (a finished one) | operator | `jobs:cancel` |

A job is **queued** after `POST /work/jobs` (its `position` says its place among the queued jobs of the machine; at most `work_queue_max` wait) and starts only when a person starts it, one job at a time on a machine (`409 machine_busy`). It goes to `running`, may be `paused`, and ends `done`, `failed` or `cancelled`. The `history` of the job keeps every change with the time, the state, who made it (a person, or `machine` when the plugin said it) and a note. The state is read from the plugin every half second by a thread of the server and again whenever a job is asked for, so a client that only polls `GET /work/jobs/{id}` sees the real state. The jobs are kept in the data folder (the last `work_keep` finished ones); `view`, the drawing the plugin made when it prepared the job, stays in memory until the server restarts (for a `bungard` job `GET /jobs/{native}` has it).

## The stream of events

`GET /api/v1/events/stream` is a stream of server-sent events (`text/event-stream`), which every browser reads with `EventSource` and every platform with a plain HTTP client. Each event is

```
id: 17
event: job.state
data: {"id":17,"kind":"job.state","machine":"demo-1","project":null,"job":"w-...","app":"studio","data":{...}}
```

The first one is `hello` (no `id`): `{"last_id", "resumed", "resync", "clients"}`.

| Kind | When |
|---|---|
| `project.created`, `project.changed`, `project.removed`, `project.file`, `project.file_removed`, `project.locked`, `project.unlocked` | the project store (`docs/PROJECT_STORE.md`); heard by the owner and the administrators, by everybody with `project_shared` |
| `job.created`, `job.state`, `job.progress` | the generic queue; `job.progress` at every tenth of the work |
| `machine.created`, `machine.changed`, `machine.removed` | the machines of the plugins |
| `catalog.changed` | an administrator replaced a catalog (`docs/CATALOGS.md`) |
| `notification` | something for the owner of a job (`id`, `kind`, `title`, `data`); heard by the owner and the administrators |

**Filters** (query): `machine`, `project`, `job`, `app` (the event has to be about it) and `kinds` (separated by commas; `job` also matches `job.state`). A token with scopes hears the families its scopes allow (`projects:read`, `machines:view`, `catalog:read`).

**Reconnecting.** Every event has a number that goes up by one. The server keeps the last `events_queue`; a client that reconnects with `Last-Event-ID: <last number it got>` (a browser sends it by itself; `?last_event_id=` does the same where a header cannot be set) gets the events it missed first, in order, and the live ones after, without repeats. If more were missed than the server keeps, the `hello` says `"resync": true` and the client has to ask for the state again (`GET /work/jobs`, `GET /work/machines`...); a client that is listening but falls behind the live flow by more than `events_queue` gets the event `resync` with the number it missed.

**Limits.** At most `events_clients_max` clients listen at once (`429 too_many_clients`); a comment line `: ping` goes out every `events_ping_s` seconds to keep proxies from closing the connection; when the login of a client ends (logout, expiry, the user taken off) its stream ends within half a minute.

**Authentication.** `Authorization: Bearer <token>`; an `EventSource` cannot set a header, so it can send a one-use ticket from `POST /tickets` in `?ticket=` (the token in the address only with `url_token = true`).

## The notifications

A job that ends, fails, is held by the machine or waits at a dialog of the operator makes a notification for the person who asked for it: `GET /notifications` (`?unread=true`; the administrators see all), `POST /notifications/read` with `{"ids": [...]}` or `{"all": true}`. The `title` is in English; a client shows its own text for the `kind`. They are kept in memory, the last `notifications_keep`. The stream carries a `notification` event at the same moment, so an application can show it without polling.

## Roles, scopes and audit

Operations that move a machine are the operator's and need `jobs:prepare`, `jobs:start` or `jobs:cancel`; the machines of the plugins are the administrator's (`machines:manage`, a scope that a token of the design programs can never hold). The audit has a line for every machine made, changed or removed and for every job prepared, started, held, resumed, cancelled or removed, with who and the ids.

## What has been tried

Everything above runs in the tests against the `demo` plugin and the simulator of the `bungard` plugin. Nothing has been tried with a second kind of real machine, and the `bungard` plugin has run on a real machine only as far as the old routes have.
