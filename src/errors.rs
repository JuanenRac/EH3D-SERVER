//! The catalog of the error codes of the API: every refusal is `{"error": "<code>", "detail": "..."}` and the code is one of these. The catalog is the one list: the
//! test `tests/errors.rs` reads the source of the server and fails when a code is used that is not here (or is here and no longer used), and writes
//! `docs/error-codes.json`, which the clients read in their own tests to be sure that each code reaches the person as a sentence of their language.
//!
//! The status is the one the code comes with in the usual case; a few codes arrive with another status where the situation is different (`bad_request` is 400 for a body
//! that does not read and 422 for a body that reads but does not fit). The description is for whoever reads the documentation, in English.

/// `(code, status, description)`.
pub const ALL: &[(&str, u16, &str)] = &[
    ("artifact_in_use", 409, "A task still works on this artifact or made it: remove the task first."),
    ("bad_address", 422, "The address of a machine or of its drill head node is missing or is not host:port."),
    ("bad_app", 422, "The name of the application is not one this server accepts (lower case letters, digits and hyphens, one of the known applications or of auth_extra_apps)."),
    ("bad_artifact", 422, "The description of an artifact does not fit (name, format, result_type, inputs or params)."),
    ("bad_board", 422, "board_mm is not from 0 to 50."),
    ("bad_cfg", 422, "The tuning text of the machine does not read."),
    ("bad_container", 422, "The .eh3dproj container cannot be read or written: structure, names, sizes, integrity record, or the project has no project.json."),
    ("bad_credentials", 401, "The name or the password is wrong."),
    ("bad_days", 422, "The days a token lasts are not a number from 1 to 3650."),
    ("bad_dcf", 422, "The text of DRILLPRO.DCF or ROUTEPRO.RCF does not read."),
    ("bad_filename", 422, "The name of a file is not safe: empty, absolute, with .. or a drive, with control characters, a reserved name of Windows, too long, or the same as another in other letter case."),
    ("bad_gcode", 422, "A line of the G-code program cannot be sent (control characters, a $ line that is not allowed, a line too long)."),
    ("bad_hole", 422, "A hole has a coordinate that is not a number."),
    ("bad_id", 422, "The id of a machine is not made of lowercase letters, digits and hyphens (up to 32)."),
    ("bad_light", 422, "The query of the ring of LEDs of the drill head is not valid."),
    ("bad_name", 422, "A name is empty, too long or has characters that are not allowed."),
    ("bad_ncd", 422, "The drilling file does not read."),
    ("bad_params", 422, "The parameters of a task do not fit its engine, are not an object, or the idempotency key is not valid."),
    ("bad_path", 422, "A point of a path has a coordinate that is not a number."),
    ("bad_picture", 502, "The picture the camera of the drill head sent cannot be read."),
    ("bad_profile", 422, "The inspection profile is not valid."),
    ("bad_rack", 422, "The rack of a job request of the second version does not fit: places that repeat, the test pin as a place, more places than rack_max_slots, a carry_slot without carry_in, or a place the machine does not define or reach."),
    ("bad_request", 400, "The body of the request does not read as the request needs."),
    ("bad_samples", 422, "The number of samples of a measurement is out of range."),
    ("bad_scope", 422, "A scope of a token is not one the server knows."),
    ("bad_setup", 403, "The set-up code is wrong, or the server already has a user."),
    ("bad_share", 422, "The sharing does not fit: give a user or a token, not the owner, with an access that exists for it."),
    ("bad_sorting", 422, "The sorting of the holes is not one the program knows."),
    ("bad_spindle", 422, "The spindle, the switch or the limits of a spindle are not valid."),
    ("bad_station", 422, "The station of a machine is not made of lowercase letters, digits and hyphens (up to 32)."),
    ("bad_template", 422, "The template of a token is not phoenix, cam, client or custom."),
    ("bad_tool", 422, "A tool has values out of range."),
    ("bad_upload", 422, "The declaration of an upload or one of its parts does not fit (hash, empty part, a part past the declared size)."),
    ("blob_damaged", 500, "The bytes of an artifact are missing or are not what their hash says."),
    ("both_sources", 422, "The job brings the work as data and as files; it must be one of them."),
    ("engine_missing", 422, "The kind of task is known but this build has no engine for it."),
    ("file_too_big", 413, "The file is bigger than project_max_file_mb."),
    ("forbidden", 403, "The role of the caller is not enough for this request."),
    ("forbidden_scope", 403, "The token is limited to scopes that do not include this one, or a token of a design program was asked for a scope of the machines."),
    ("hash_mismatch", 422, "The bytes of the upload do not have the SHA-256 that was declared; the upload is discarded."),
    ("head_refused", 502, "The node of the drill head answered with a refusal."),
    ("head_unreachable", 502, "The node of the drill head does not answer."),
    ("idempotency_conflict", 409, "The idempotency key was already used by this caller for a request with another body."),
    ("internal", 500, "A task of the server failed; the server is still up."),
    ("job_active", 409, "The job is running: stop it first."),
    ("last_admin", 409, "The last administrator cannot be deleted, disabled or demoted."),
    ("local_only", 403, "While the server has no user, only the computer it runs on may make the first administrator."),
    ("locked", 423, "Somebody else is editing the project (its lock has not expired)."),
    ("machine_busy", 409, "A job is running on the machine."),
    ("machine_exists", 409, "A machine with this id already exists."),
    ("machines_busy", 409, "A job is running: the update would restart the server and stop it."),
    ("no_head", 404, "The machine has no node of the drill head."),
    ("no_hole", 409, "The picture of the drill head shows no hole to calibrate with."),
    ("no_inspection", 404, "No quality check has been made on this machine since the server started."),
    ("no_project", 422, "The artifact a task works on does not exist or cannot be seen."),
    ("no_work", 422, "The job has no holes, no paths and no lines to run."),
    ("not_cancellable", 409, "The job is neither ready nor running."),
    ("not_in_state", 409, "A job is held while it runs and resumed while it is held."),
    ("not_locked", 409, "The project has no lock to release."),
    ("not_pausable", 409, "Only a G-code job can be held."),
    ("not_ready", 409, "The job already ran or is running."),
    ("not_running", 409, "The job is not running."),
    ("not_stopped", 409, "Only a cancelled or failed job can be gone on from."),
    ("out_of_table", 422, "A hole or a path is outside the table of the machine."),
    ("outside_course", 422, "The work goes past the course of an axis (the machine's switch is on reject)."),
    ("precondition_failed", 412, "The If-Match tag is not the one of the current version: read the file again before writing."),
    ("precondition_required", 428, "A write over an existing file needs If-Match with the tag (SHA-256) of the version it was made from."),
    ("project_quota", 507, "The project, the application or the store would pass its limit (project_max_mb, project_max_files, project_max_projects)."),
    ("queue_full", 429, "As many tasks are waiting or running as the server accepts (max_tasks)."),
    ("quota", 429, "The caller already has as many tasks waiting or running as one user may (tasks_per_user)."),
    ("shares_off", 501, "Sharing is switched off on this server (shares_enabled)."),
    ("spindle_limit", 422, "The job asks for more than the spindle of the machine gives (the machine's switch is on reject)."),
    ("stale_parent", 409, "The revision does not follow the last one of the project: look at what changed first."),
    ("storage", 500, "The data folder could not be written."),
    ("store", 500, "The inspection profile could not be saved."),
    ("store_full", 507, "The artifact store would pass artifact_store_gb."),
    ("task_active", 409, "The task has not finished: cancel it first."),
    ("tasks_off", 501, "The computing tasks are switched off on this server (tasks_enabled)."),
    ("throttled", 429, "Too many wrong passwords, or too many tickets waiting: try again later."),
    ("too_big", 422, "The request or the work is bigger than the server accepts (413 for a body over max_body_mb)."),
    ("too_many_holes", 422, "More holes than one inspection takes."),
    ("unauthorized", 401, "The token is missing, not valid or has expired."),
    ("unknown_artifact", 404, "There is no artifact with this id, or it belongs to somebody else."),
    ("unknown_file", 404, "The project has no such file."),
    ("unknown_job", 404, "There is no job with this id."),
    ("unknown_kind", 422, "There is no such kind of task."),
    ("unknown_machine", 404, "There is no machine with this id."),
    ("unknown_project", 404, "There is no project with this id, or it belongs to somebody else."),
    ("unknown_task", 404, "There is no task with this id, or it belongs to somebody else."),
    ("unknown_token", 404, "There is no token with this id."),
    ("unknown_tool", 422, "The job uses a tool that it does not describe."),
    ("unknown_upload", 404, "There is no upload with this id, or it belongs to somebody else."),
    ("unknown_user", 404, "There is no user with this name."),
    ("unknown_version", 404, "The file has no such version (or it was dropped past project_versions_kept)."),
    ("update_refused", 409, "The update helper refused the request."),
    ("updates_off", 501, "The updates are not set up on this server (updates_dir)."),
    ("upload_conflict", 409, "A part carries other bytes than the ones already received at the same place."),
    ("upload_incomplete", 409, "Parts are still missing: the upload cannot be completed."),
    ("user_exists", 409, "A user with this name already exists."),
    ("weak_password", 422, "The password is not from 8 to 128 characters."),
    ("wrong_machine", 422, "The job is of a kind the machine does not take."),
];

/// Whether `code` is in the catalog.
pub fn known(code: &str) -> bool {
    ALL.iter().any(|(c, _, _)| *c == code)
}

/// The catalog as the clients read it: a list of `{code, status, description}` in the order of the codes.
pub fn json() -> serde_json::Value {
    serde_json::Value::Array(
        ALL.iter()
            .map(|(code, status, description)| {
                serde_json::json!({ "code": code, "status": status, "description": description })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_are_sorted_and_not_repeated() {
        for pair in ALL.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} before {}", pair[0].0, pair[1].0);
        }
        for (code, status, description) in ALL {
            assert!(code.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            assert!((400..600).contains(status), "{code}");
            assert!(description.ends_with('.'), "{code}");
        }
        assert!(known("unauthorized") && !known("made_up"));
    }
}
