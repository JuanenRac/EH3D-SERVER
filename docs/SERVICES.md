# The common services

EH3D-SERVER started as the server of the Bungard programs. It is now the server of **all the applications of EH3D**: the Studio, the mobile apps and the watch, the panel next to the machine, the PCB, CAM, laser and pick-and-place tools. For them it gives four services that every one of them would otherwise have to build for itself.

| Service | What an application gets | Routes | Document |
|---|---|---|---|
| **The project store** | its projects and their files on the server, with every version kept and checked, a tag (`If-Match`) so two people never overwrite each other, an editing lock, and import and export of the `.eh3dproj` container | `/api/v1/apps/{app}/projects` | [PROJECT_STORE.md](PROJECT_STORE.md) |
| **The shared catalogs** | materials, tools, machine profiles, libraries of footprints and symbols and the components base, the same for everybody, with search, filters, pages and `ETag` | `/api/v1/catalogs` | [CATALOGS.md](CATALOGS.md) |
| **Machine plugins and jobs** | any kind of machine behind one interface: prepare, start, hold, stop and follow a job; a queue with history; live events; notifications | `/api/v1/plugins`, `/api/v1/work/...`, `/api/v1/events/stream`, `/api/v1/notifications` | [MACHINE_PLUGINS.md](MACHINE_PLUGINS.md) |
| **The single login** | one login for every application, with the role and the application as claims, sessions that can be listed and closed, renewal, a policy for passwords and lockout, and an audit | `/api/v1/auth/...` | [AUTH.md](AUTH.md) |

The exact shape of every request and answer, with the roles, the scopes and the error codes of each route, is the OpenAPI contract in [openapi.yaml](openapi.yaml) (the tags `store`, `catalogs`, `work` and `auth`), with the table of routes in [ROUTES.md](ROUTES.md). The other documents explain how each service behaves.

## How an application uses them

1. **Log in** with `POST /auth/login` and its own name (`studio`, `android`, `pcb`...). The answer has the token, the role and the session. Everything that follows carries `Authorization: Bearer <token>`. Read the role with `GET /auth/session` and show only what it allows.
2. **Read the catalogs** it needs (`GET /catalogs/materials?q=fr4`) and keep a copy with the tag it came with; ask again with `If-None-Match` and the server answers `304` when nothing changed.
3. **Open and save a project.** `GET /apps/studio/projects` lists them; `PUT .../files/content?name=...` with `If-Match` saves a file as a new version; `PUT .../lock` says "I am editing" while the window is open; `GET .../export` gives the `.eh3dproj`. An application that works on its own computer does not need any of this: the store is for when the project should be on the server.
4. **Send work to a machine.** `GET /work/machines` lists the machines of every plugin and what each can do; `POST /work/jobs` prepares a job from the request the plugin takes; a person starts it with `POST /work/jobs/{id}/start`.
5. **Listen.** `GET /events/stream` (an `EventSource` in a browser) tells what happens to its projects, its jobs and the machines, with numbers so it can reconnect without losing anything; `GET /notifications` has what needs a person.

## What every route has

* **A named operation** in the OpenAPI contract, which a test compares with the router: a route that is not described, or a description with no route, fails the build.
* **Roles and scopes.** The three roles (viewer reads, operator prepares, starts and writes, administrator manages) and, for a token of a program, the scopes that narrow the role: `projects:read`, `projects:write`, `catalog:read`, `catalog:write`, `machines:view`, `machines:manage`, `jobs:prepare`, `jobs:start`, `jobs:cancel`. A scope narrows a role and never widens it.
* **Stable error codes**, the same catalog for the whole server (`docs/error-codes.json`), each with its status; the clients turn them into a sentence of their language.
* **Limits in `server.cfg`**, none in the code: file and project sizes, number of files, versions kept, lock time, page sizes, queue length, clients of the stream, lockout and passwords. `server.cfg.example` explains every key.
* **Audit.** Every change of the services writes a line (who, what, which project, machine or job) to the console and to `audit.jsonl`; no password, token or file content is ever written.
* **Tests through the real router**, in `tests/store.rs`, `tests/catalogs.rs`, `tests/work.rs` and `tests/auth_apps.rs`, besides the tests of each module.

## Compatibility

Everything that existed keeps its routes and its behaviour: `/api/v1/machines`, `/api/v1/jobs`, `/api/v1/artifacts`, `/api/v1/tasks`, `/api/v1/projects` (the revisions of artifacts), the login, the tokens, the websocket `/api/v1/events` and the request of the second version with the `rack` (`docs/ATC_CHAIN.md`). The new routes live under their own names. The Bungard machines are the machines of the `bungard` plugin, and a job prepared through the generic queue is the same job, in the same book, as one made with the old routes.

Three things change by default on a server that is updated, all of them in `server.cfg` and all of them on the side of caution: a password may not contain the name of its user (`password_deny_name`), the requests refused with `401` and `403` go to the audit (`audit_denials`), and a person has at most twenty logins of applications open at once (`auth_max_sessions`, only for `/auth/login`).

## Types

The shapes of the answers (`StoreProject`, `CatalogPage`, `WorkJob`, `AuthClaims`...) are defined in the contract of this server. EH3D-COMMON has the data model of the projects and of the machine telemetry, but no types for the requests and answers of a REST interface yet; the project container (`eh3d-container`) and the database of materials (`eh3d-materials`) are used from it as libraries. When COMMON gets a place for the shared REST types, the ones above are the candidates to move there, with the same names.

## What has been tried

All of it runs on a computer, in the tests, against the `demo` plugin, the simulator of the Bungard machines and files made by the tests. No application has been changed to use these routes yet, nothing has run on a real machine through the generic queue, the tool store, the machine profiles and the libraries have not been loaded with the real data of their programs, and the server still does not terminate TLS (put HTTPS in front of it).
