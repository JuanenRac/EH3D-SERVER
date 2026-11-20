# Routes of the server

Every route of the router, with the method, who may call it and what it does. This table is written by `tools/make_openapi.py` from the same table as `docs/openapi.yaml`, the contract of the interface (with the schemas, the examples and the error codes of each route); do not edit it by hand.

| Route | Method | Auth | Description |
|---|---|---|---|
| `/api/v1/account` | PUT | a person | Change the own password |
| `/api/v1/apps/:app/projects` | GET | viewer + `projects:read` | List the projects of an application |
| `/api/v1/apps/:app/projects` | POST | operator + `projects:write` | Make a project of an application |
| `/api/v1/apps/:app/projects/:id` | GET | viewer + `projects:read` | Read a project with its files |
| `/api/v1/apps/:app/projects/:id` | PUT | operator + `projects:write` | Rename a project |
| `/api/v1/apps/:app/projects/:id` | DELETE | operator + `projects:write` | Hide a project |
| `/api/v1/apps/:app/projects/:id/export` | GET | viewer + `projects:read` | Export a project as an .eh3dproj container |
| `/api/v1/apps/:app/projects/:id/files` | GET | viewer + `projects:read` | List the files of a project |
| `/api/v1/apps/:app/projects/:id/files/content` | GET | viewer + `projects:read` | Read a file |
| `/api/v1/apps/:app/projects/:id/files/content` | PUT | operator + `projects:write` | Write a file as a new version |
| `/api/v1/apps/:app/projects/:id/files/content` | DELETE | operator + `projects:write` | Remove a file from the list |
| `/api/v1/apps/:app/projects/:id/files/versions` | GET | viewer + `projects:read` | List the versions of a file |
| `/api/v1/apps/:app/projects/:id/import` | POST | operator + `projects:write` | Import an .eh3dproj container into a project |
| `/api/v1/apps/:app/projects/:id/lock` | GET | viewer + `projects:read` | See who is editing a project |
| `/api/v1/apps/:app/projects/:id/lock` | PUT | operator + `projects:write` | Take or renew the editing lock of a project |
| `/api/v1/apps/:app/projects/:id/lock` | DELETE | operator + `projects:write` | Let the editing lock go |
| `/api/v1/apps/:app/projects/:id/purge` | POST | admin + `projects:write` | Remove a hidden project for good |
| `/api/v1/apps/:app/quota` | GET | viewer + `projects:read` | The limits of the store and what the application uses |
| `/api/v1/apps/pcb/jobs` | POST | operator + `jobs:prepare` | Run the design program on a board of a project |
| `/api/v1/artifacts` | GET | viewer + `artifacts:read` | List the artifacts the caller can see, newest first |
| `/api/v1/artifacts` | POST | operator + `artifacts:write` | Keep a file as an artifact |
| `/api/v1/artifacts/:id` | GET | viewer + `artifacts:read` | The bytes of an artifact, streamed |
| `/api/v1/artifacts/:id` | DELETE | admin + `artifacts:write` | Remove an artifact |
| `/api/v1/artifacts/:id/meta` | GET | viewer + `artifacts:read` | The record of an artifact |
| `/api/v1/artifacts/:id/retention` | PUT | admin + `artifacts:write` | Set how long an artifact is kept |
| `/api/v1/artifacts/:id/shares` | GET | viewer + `artifacts:read` | Who an artifact is shared with |
| `/api/v1/artifacts/:id/shares` | POST | operator + `artifacts:write` | Share an artifact for reading |
| `/api/v1/artifacts/:id/shares/:who` | DELETE | operator + `artifacts:write` | Take the sharing of an artifact back |
| `/api/v1/artifacts/verify` | POST | admin + `artifacts:read` | Read every blob again against its hash |
| `/api/v1/audit` | GET | admin + `admin:policy` | Read the audit |
| `/api/v1/auth/apps` | GET | any login or token | The applications the server accepts |
| `/api/v1/auth/keys` | GET | admin + `admin:tokens` | The identifiers of the keys and the fingerprint of the certificate |
| `/api/v1/auth/login` | POST | none | Log in for an application |
| `/api/v1/auth/logout` | POST | any login or token | End the login |
| `/api/v1/auth/logout-all` | POST | a person | End every login of the caller |
| `/api/v1/auth/policy` | GET | admin + `admin:policy` | See the switches of security |
| `/api/v1/auth/policy` | PUT | admin + `admin:policy` | Change a switch of security while the server runs |
| `/api/v1/auth/refresh` | POST | a person | Renew the login |
| `/api/v1/auth/session` | GET | any login or token | The claims of the login |
| `/api/v1/auth/sessions` | GET | any login or token | List the open logins of the caller |
| `/api/v1/auth/sessions/:id` | DELETE | a person | Close one login |
| `/api/v1/blobs/:sha` | HEAD | viewer + `artifacts:read` | Ask whether the caller can see bytes with this hash |
| `/api/v1/catalogs` | GET | viewer + `catalog:read` | List the catalogs |
| `/api/v1/catalogs/:catalog` | GET | viewer + `catalog:read` | Search a catalog |
| `/api/v1/catalogs/:catalog` | PUT | admin + `catalog:write` | Replace the dataset of a catalog |
| `/api/v1/catalogs/:catalog` | DELETE | admin + `catalog:write` | Empty a catalog |
| `/api/v1/catalogs/:catalog/items/:item` | GET | viewer + `catalog:read` | Read one item of a catalog |
| `/api/v1/events` | GET | viewer | Open the websocket of the events |
| `/api/v1/events/stream` | GET | viewer | Open the stream of events (server-sent events) |
| `/api/v1/fleet` | GET | viewer + `machines:view` | Every machine at once, grouped by station |
| `/api/v1/health` | GET | none | Health check |
| `/api/v1/jobs` | GET | viewer + `machines:view` | List the jobs, newest first |
| `/api/v1/jobs/:id` | GET | viewer + `machines:view` | Read a job with its view |
| `/api/v1/jobs/:id` | DELETE | operator + `jobs:cancel` | Remove a job that is not running |
| `/api/v1/jobs/:id/cancel` | POST | operator + `jobs:cancel` | Stop a running job or cancel a ready one |
| `/api/v1/jobs/:id/confirm` | POST | operator + `jobs:confirm` | Answer the dialog the job waits at |
| `/api/v1/jobs/:id/pause` | POST | operator + `jobs:start` | Hold a running G-code job |
| `/api/v1/jobs/:id/request` | GET | operator + `machines:view` | Read what was asked of the job, as it came |
| `/api/v1/jobs/:id/resume` | POST | operator + `jobs:start` | Let a held G-code job go on |
| `/api/v1/jobs/:id/resume-from` | POST | operator + `jobs:prepare` | Make a new ready job from a cancelled or failed one |
| `/api/v1/jobs/:id/start` | POST | operator + `jobs:start` | Plan again and run the job |
| `/api/v1/login` | POST | none | Log in |
| `/api/v1/logout` | POST | any login or token | End the login |
| `/api/v1/machines` | GET | viewer + `machines:view` | List the machines |
| `/api/v1/machines` | POST | admin + `machines:manage` | Create a machine |
| `/api/v1/machines/:id` | GET | viewer + `machines:view` | Read a machine |
| `/api/v1/machines/:id` | PUT | admin + `machines:manage` | Change a machine |
| `/api/v1/machines/:id` | DELETE | admin + `machines:manage` | Remove a machine |
| `/api/v1/machines/:id/head/inspect` | GET | viewer + `machines:view` | The last check of the machine |
| `/api/v1/machines/:id/head/inspect` | POST | operator + `jobs:prepare` | Check the quality of the work with the camera |
| `/api/v1/machines/:id/head/inspect/calibrate` | POST | operator + `jobs:prepare` | Set the scale of the picture from the first hole found |
| `/api/v1/machines/:id/head/inspect/profile` | GET | viewer + `machines:view` | The profile of the analyser |
| `/api/v1/machines/:id/head/inspect/profile` | PUT | operator + `jobs:prepare` | Change the profile of the analyser |
| `/api/v1/machines/:id/head/light` | GET | viewer + `machines:view` | The ring of LEDs as the node says it |
| `/api/v1/machines/:id/head/light` | POST | operator + `jobs:prepare` | Turn the ring on or off and change how it shines |
| `/api/v1/machines/:id/head/measure` | POST | operator + `jobs:prepare` | One measure of the table under the head |
| `/api/v1/machines/:id/head/snapshot` | GET | viewer + `machines:view` | One picture of the camera |
| `/api/v1/machines/:id/head/status` | GET | viewer + `machines:view` | Status of the node of the drill head |
| `/api/v1/machines/:id/head/stream` | GET | viewer + `machines:view` | The camera of the node as MJPEG |
| `/api/v1/machines/:id/jobs` | POST | operator + `jobs:prepare` | Plan a job and keep it ready |
| `/api/v1/machines/:id/preview` | POST | operator + `jobs:prepare` | Plan a job and show its view, creating nothing |
| `/api/v1/machines/:id/spindle` | GET | viewer + `machines:view` | Read the spindle and its limits |
| `/api/v1/machines/:id/spindle` | PUT | admin + `machines:manage` | Set the spindle, its switch and its limits |
| `/api/v1/machines/:id/stop` | POST | operator + `jobs:cancel` | Stop the machine now |
| `/api/v1/notifications` | GET | viewer + `machines:view` | List the notifications of the caller |
| `/api/v1/notifications/read` | POST | viewer + `machines:view` | Mark notifications as read |
| `/api/v1/openapi.json` | GET | viewer | The contract of the interface this server has |
| `/api/v1/plugins` | GET | viewer + `machines:view` | List the machine plugins |
| `/api/v1/projects` | GET | viewer + `projects:read` | List the projects the caller sees |
| `/api/v1/projects` | POST | operator + `projects:write` | Make a project |
| `/api/v1/projects/:id` | GET | viewer + `projects:read` | Read a project with its revisions |
| `/api/v1/projects/:id` | DELETE | operator + `projects:write` | Hide a project |
| `/api/v1/projects/:id/purge` | POST | admin + `projects:write` | Remove a hidden project for good |
| `/api/v1/projects/:id/revisions` | POST | operator + `projects:write` | Add a revision |
| `/api/v1/projects/:id/shares` | POST | operator + `projects:write` | Share a project with one person or token |
| `/api/v1/projects/:id/shares/:who` | DELETE | operator + `projects:write` | Take a sharing back |
| `/api/v1/session` | GET | any login or token | Say who is logged in |
| `/api/v1/setup` | GET | none | Ask whether the first administrator is still needed |
| `/api/v1/setup` | POST | none | Make the first administrator |
| `/api/v1/tasks` | GET | viewer + `tasks:read` | List the tasks the caller may see, newest first |
| `/api/v1/tasks` | POST | operator + `tasks:run` | Make a task |
| `/api/v1/tasks/:id` | GET | viewer + `tasks:read` | Read a task with its result |
| `/api/v1/tasks/:id` | DELETE | operator + `tasks:run` | Remove a finished task |
| `/api/v1/tasks/:id/cancel` | POST | operator + `tasks:run` | Cancel a task |
| `/api/v1/tasks/kinds` | GET | viewer + `tasks:read` | The kinds of task and whether this build has an engine for each |
| `/api/v1/tickets` | POST | any login or token | Make a ticket for the camera or the websocket |
| `/api/v1/tokens` | GET | admin + `admin:tokens` | List the tokens of programs |
| `/api/v1/tokens` | POST | admin + `admin:tokens` | Make a token for a program |
| `/api/v1/tokens/:id` | DELETE | admin + `admin:tokens` | Revoke a token |
| `/api/v1/tokens/:id/rotate` | POST | admin + `admin:tokens` | Give a token a new secret |
| `/api/v1/updates` | GET | admin + `admin:updates` | See the updates of the services |
| `/api/v1/updates/apply` | POST | admin + `admin:updates` | Ask the helper to update components |
| `/api/v1/updates/check` | POST | admin + `admin:updates` | Ask the helper to look for the latest releases |
| `/api/v1/updates/clear-history` | POST | admin + `admin:updates` | Ask the helper to empty its history |
| `/api/v1/updates/undo` | POST | admin + `admin:updates` | Ask the helper to put back the previous version |
| `/api/v1/uploads` | POST | operator + `artifacts:write` | Begin an upload in parts |
| `/api/v1/uploads/:id` | PUT | operator + `artifacts:write` | Send a part of an upload |
| `/api/v1/uploads/:id` | DELETE | operator + `artifacts:write` | Throw an upload away |
| `/api/v1/uploads/:id/complete` | POST | operator + `artifacts:write` | Finish an upload and make its artifact |
| `/api/v1/users` | GET | admin + `admin:users` | List the users |
| `/api/v1/users` | POST | admin + `admin:users` | Create a user |
| `/api/v1/users/:name` | PUT | admin + `admin:users` | Change a user |
| `/api/v1/users/:name` | DELETE | admin + `admin:users` | Delete a user |
| `/api/v1/work/jobs` | GET | viewer + `machines:view` | List the jobs of the generic queue, newest first |
| `/api/v1/work/jobs` | POST | operator + `jobs:prepare` | Prepare a job on a machine of any plugin |
| `/api/v1/work/jobs/:id` | GET | viewer + `machines:view` | Read a job with its history |
| `/api/v1/work/jobs/:id` | DELETE | operator + `jobs:cancel` | Remove a finished job |
| `/api/v1/work/jobs/:id/cancel` | POST | operator + `jobs:cancel` | Cancel a queued job or stop a running one |
| `/api/v1/work/jobs/:id/findings` | GET | viewer + `machines:view` | The findings a gerber job made |
| `/api/v1/work/jobs/:id/layers` | GET | viewer + `machines:view` | The layers a gerber job read |
| `/api/v1/work/jobs/:id/pause` | POST | operator + `jobs:start` | Hold a running job |
| `/api/v1/work/jobs/:id/resume` | POST | operator + `jobs:start` | Let a held job go on |
| `/api/v1/work/jobs/:id/start` | POST | operator + `jobs:start` | Start a queued job |
| `/api/v1/work/machines` | GET | viewer + `machines:view` | List the machines of every plugin |
| `/api/v1/work/machines` | POST | admin + `machines:manage` | Make a machine of a plugin |
| `/api/v1/work/machines/:id` | GET | viewer + `machines:view` | Read a machine of a plugin |
| `/api/v1/work/machines/:id` | PUT | admin + `machines:manage` | Change a machine made through the plugins |
| `/api/v1/work/machines/:id` | DELETE | admin + `machines:manage` | Remove a machine made through the plugins |
| `/api/v1/work/machines/:id/compat/key` | GET | admin + `machines:manage` | Whether a 3D printer has a key for the compatibility routes |
| `/api/v1/work/machines/:id/compat/key` | POST | admin + `machines:manage` | Make the key of a 3D printer for the compatibility routes |
| `/api/v1/work/machines/:id/compat/key` | DELETE | admin + `machines:manage` | Take the compatibility key of a 3D printer away |
| `/api/v1/work/machines/:id/compat/moonraker` | GET | viewer + `machines:view` | The state of a 3D printer shaped as Moonraker answers it |
| `/api/v1/work/machines/:id/compat/octoprint` | GET | viewer + `machines:view` | The state of a 3D printer shaped as OctoPrint answers it |
| `/api/v1/work/machines/:id/emergency-stop` | POST | operator + `jobs:cancel` | Stop a CNC machine or a 3D printer at once and latch it |
| `/api/v1/work/machines/:id/feeders` | GET | viewer + `machines:view` | The feeders of a pick-and-place machine |
| `/api/v1/work/machines/:id/feeders` | PUT | operator + `jobs:prepare` | Declare, refill or empty a feeder at once |
| `/api/v1/work/machines/:id/jog` | POST | operator + `jobs:start` | Jog a CNC machine |
| `/api/v1/work/machines/:id/laser/release` | POST | admin + `machines:manage` | Release the latch of an emergency stop of a laser machine |
| `/api/v1/work/machines/:id/laser/test-fire` | POST | operator + `jobs:start` | Fire a laser machine for a moment, to focus |
| `/api/v1/work/machines/:id/layer` | GET | viewer + `machines:view` | The layer a 3D printer is at |
| `/api/v1/work/machines/:id/position` | GET | viewer + `machines:view` | Where a CNC machine is |
| `/api/v1/work/machines/:id/release` | POST | admin + `machines:manage` | Release the latch of an emergency stop of a CNC machine or a 3D printer |
| `/api/v1/work/machines/:id/temperatures` | GET | viewer + `machines:view` | The heaters of a 3D printer |
| `/api/v1/work/machines/:id/temperatures` | PUT | operator + `jobs:start` | Set the target of a heater of a 3D printer |
| `/api/v1/work/machines/:id/tools` | GET | viewer + `machines:view` | What a CAM worker has built in |
