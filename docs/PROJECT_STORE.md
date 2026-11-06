# The project store

The first of the four common services (`docs/SERVICES.md`): the projects and the files of every application of EH3D, kept by the server. An application (the Studio, the mobile apps, the panel next to the machine, the PCB, CAM and laser tools) keeps its projects on its own computer and uses the store when it wants them on the server: to open them from another device, to hand them to the machine, to keep the versions.

The routes are under `/api/v1/apps/{app}/projects`; the contract (schemas, errors, examples) is the `store` tag of `docs/openapi.yaml`, and this page explains how it behaves. The code is `src/pstore.rs` (the rules) and `src/api_pstore.rs` (the routes).

The older `/api/v1/projects` (a name and a line of revisions that point to artifacts) is not touched; the store is a second, richer way to keep a project and the two do not share records.

## Who may do what

| Request | Role | Scope |
|---|---|---|
| list, read, list files, read a file, versions, see the lock, export | viewer | `projects:read` |
| make, rename, write, remove a file, take and release the lock, import | operator | `projects:write` |
| hide a project | operator, and the owner or an administrator | `projects:write` |
| purge a hidden project, take a lock away from somebody (`force`) | administrator | `projects:write` |

A token with scopes does what the intersection of its role and its scopes allows (`docs/API.md`). A project is seen by its **owner** (the user who made it, or `token:<id>` for a program's token) and by the administrators; for anybody else it does not exist (`404 unknown_project`). With `project_shared = true` the store works as a team: everybody who has access to the store sees and writes the projects of the application. The application is part of the address: a project of `studio` does not exist under `android`.

The application has to be a name the server accepts (`422 bad_app`): lower case letters, digits and hyphens, and one of the known applications or one of `auth_extra_apps` (see `docs/AUTH.md`).

## Files, versions and the tag

A project holds files with a name that may have folders (`layers/top.gbr`). **A file is never overwritten**: every change of its bytes is a new version with a number (1, 2, 3...), the SHA-256 of the bytes, the size, the time and who wrote it. Only the last `project_versions_kept` versions stay (0: all of them); the numbers keep going up.

The **tag** of a file is the SHA-256 of its current version, which is also its `ETag`. A write over a file that exists must say which version it was made from, with `If-Match`: the server answers `428 precondition_required` when the header is missing and `412 precondition_failed` when it is not the current version, so two people never overwrite each other without knowing. A new file takes no tag. The same rule holds for removing a file and, with the revision number as tag, for renaming a project. `project_require_if_match = false` turns the rule off; it is on by default.

Writing the same bytes as the current version makes no version (`200` with `"unchanged": true`); a new version answers `201`. Removing a file takes it off the list and keeps its versions; writing it again brings it back with the next number.

```
PUT /api/v1/apps/studio/projects/p-.../files/content?name=layers/top.gbr&comment=first+layer
If-Match: "<sha256 of the version it was made from>"      (not for a new file)
Content-Type: application/octet-stream
<the bytes>
```

The name goes in the query (`?name=`) because it may have slashes. `GET .../files/content?name=...&version=2` reads an older version; `GET .../files` lists the current files; `GET .../files/versions?name=...` lists the versions of one file, the newest first.

### Names that are refused

`422 bad_filename` for every name that could leave the folder or does not exist on every system: empty, absolute, with a backslash or a colon (drive letters), with `.` or `..` parts or empty parts (`a//b`), with control characters, not in Unicode normal form C, with a part that ends in a dot or a space, a reserved device name of Windows (`CON`, `NUL`, `COM1`, `LPT1`..., with or without extension), longer than 240 bytes, `integrity.json` (the container writes it) and a name that differs from another file only in letter case. It is the strict name rule of the `.eh3dproj` container of EH3D-COMMON, used as a library (`eh3d-container`).

### Limits

All of them are in `server.cfg`: `project_max_file_mb` (`413 file_too_big`), `project_max_mb` (the bytes of every distinct version of the project, `507 project_quota`), `project_max_files`, `project_max_projects` per application, `project_versions_kept`. A request body is also bounded by the size it could carry.

## The lock

`PUT .../lock` takes the editing lock of a project (`?ttl_s=` for a shorter one; at most `project_lock_ttl_s`). While it lasts the others cannot write, remove, rename or import: `423 locked`, with the name of the holder in the detail. The holder renews it by calling again; it expires by itself, so a program that crashed does not hold a project for ever. `DELETE .../lock` lets it go (`409 not_locked` when there is none); an administrator takes it away from somebody else with `?force=true`. The lock is a courtesy between people, not a security boundary: `If-Match` is what prevents the lost update.

## What is kept on the disk

Under the data folder: `pstore/apps/<app>/<project>/meta.json` (the record, written through a temporary file so a power cut leaves the old one or the new one whole, with the previous kept as `.bak`) and `blobs/<sha256>` (the bytes of a version, written the same way, once per distinct content in the project). Every read of the bytes checks their size and SHA-256: bytes that changed on the disk are never given back (`500 blob_damaged`). Hiding a project keeps everything; purging removes the folder and is audited. A record that does not read stops the server at the start, as the other files do.

## The `.eh3dproj` container

`POST .../import` takes the bytes of an `.eh3dproj` and makes every entry a new version of the file of that name (`project.json`, `layers/top.gbr`...). The package is opened with the reader of `eh3d-container`, so its limits hold (names, sizes, number of entries, ZIP structure) and its integrity record is verified; it is read as 1.0 and, if that fails, as 1.1. Everything or nothing: one entry over a limit refuses the whole container (`422 bad_container`, `413`, `507`) and writes nothing. Entries that did not change make no version.

`GET .../export` writes the current files into a container with its integrity record (`?spec=1.0` or `1.1`; without it 1.0 and then 1.1) and sends it as `application/zip`. The project has to hold the `project.json` of the format, which the application that saved it wrote; the server does not invent one.

## Events and audit

Every change is told on the stream (`GET /api/v1/events/stream`, `docs/MACHINE_PLUGINS.md`): `project.created`, `project.changed`, `project.removed`, `project.file`, `project.file_removed`, `project.locked` and `project.unlocked`, with the application and the project, to the owner and the administrators (to everybody with `project_shared`). The audit has a line for each creation, rename, hiding, purge, write, removal, lock, unlock, import and export, with who, the application, the project and the file; no bytes are ever written to it.

## Types for the clients

The shapes of the answers are defined here, in `docs/openapi.yaml` (`StoreProject`, `StoreFile`, `StoreVersion`, `StoreLock`...). EH3D-COMMON has no REST types of its own yet; when it does, these are the ones that would move there.
