//! The catalog of the error codes of the API: every refusal is `{"error": "<code>", "detail": "..."}` and the code is one of these. The catalog is the one list: the
//! test `tests/errors.rs` reads the source of the server and fails when a code is used that is not here (or is here and no longer used), and writes
//! `docs/error-codes.json`, which the clients read in their own tests to be sure that each code reaches the person as a sentence of their language.
//!
//! The status is the one the code comes with in the usual case; a few codes arrive with another status where the situation is different (`bad_request` is 400 for a body
//! that does not read and 422 for a body that reads but does not fit). The description is for whoever reads the documentation, in English.

/// `(code, status, description)`.
pub const ALL: &[(&str, u16, &str)] = &[
    ("air_assist_off", 409, "The air assist input of the laser machine says off and the machine requires it on."),
    ("artifact_in_use", 409, "A task still works on this artifact or made it: remove the task first."),
    ("authorization_required", 409, "The server asks for the authorization of a job of this plugin before it starts (work_require_authorization) and the job has none for its present hash."),
    ("bad_address", 422, "The address of a machine or of its drill head node is missing or is not host:port."),
    ("bad_app", 422, "The name of the application is not one this server accepts (lower case letters, digits and hyphens, one of the known applications or of auth_extra_apps)."),
    ("bad_artifact", 422, "The description of an artifact does not fit (name, format, result_type, inputs or params)."),
    ("bad_axis", 422, "The axis of a jog is not one of x, y or z."),
    ("bad_bgcode", 422, "A binary G-code file (.bgcode) cannot be read or written: it is cut, its checksum is wrong, a block is out of order, of a type or a compression the format does not have, or it is bigger than the limit."),
    ("bad_board", 422, "board_mm is not from 0 to 50."),
    ("bad_catalog", 422, "The dataset uploaded to a catalog does not fit: not JSON or CSV that reads, items that are not objects, an item without the field that names it (or repeated), a missing format, or more items than catalog_max_items."),
    ("bad_cfg", 422, "The tuning text of the machine does not read."),
    ("bad_container", 422, "The .eh3dproj container cannot be read or written: structure, names, sizes, integrity record, or the project has no project.json."),
    ("bad_credentials", 401, "The name or the password is wrong."),
    ("bad_days", 422, "The days a token lasts are not a number from 1 to 3650."),
    ("bad_dcf", 422, "The text of DRILLPRO.DCF or ROUTEPRO.RCF does not read."),
    ("bad_design_request", 422, "The request to run the design program does not fit: the kind is not one of check, bom, fabpack or panel, the board file is missing, or the options are not accepted for the kind."),
    ("bad_feed", 422, "The feed of a jog is not a number above 0 and up to jog_max_feed of the machine."),
    ("bad_filename", 422, "The name of a file is not safe: empty, absolute, with .. or a drive, with control characters, a reserved name of Windows, too long, or the same as another in other letter case."),
    ("bad_gcode", 422, "A line of the G-code program cannot be sent (control characters, a $ line that is not allowed, a line too long)."),
    ("bad_gerber", 422, "A Gerber or Excellon file cannot be read: the format, a coordinate or a command is wrong, or the file holds no data."),
    ("bad_heater", 422, "The heater is not hotend, bed or chamber."),
    ("bad_hole", 422, "A hole has a coordinate that is not a number."),
    ("bad_id", 422, "The id of a machine is not made of lowercase letters, digits and hyphens (up to 32)."),
    ("bad_light", 422, "The query of the ring of LEDs of the drill head is not valid."),
    ("bad_mesh", 422, "The mesh to slice is not a closed STL or 3MF the slicer can read, or has no closed outline."),
    ("bad_name", 422, "A name is empty, too long or has characters that are not allowed."),
    ("bad_ncd", 422, "The drilling file does not read."),
    ("bad_operation", 422, "An operation of a vector job for a laser is not valid: its layer, its mode (cut or engrave), its power, its speed or its passes."),
    ("bad_params", 422, "The parameters of a task do not fit its engine, are not an object, or the idempotency key is not valid."),
    ("bad_path", 422, "A point of a path has a coordinate that is not a number."),
    ("bad_picture", 502, "The picture the camera of the drill head sent cannot be read."),
    ("bad_placement_list", 422, "A list of placements that came as an artifact cannot be read as JSON or as CSV with the columns ref, part, x and y."),
    ("bad_plugin_request", 422, "The request does not fit what the plugin of the machine takes, or the settings of a machine of a plugin are not valid."),
    ("bad_pnp_node", 422, "A node of a pick-and-place machine does not fit: its kind (feeder or head), its id, its name, its address (host:port), its token (no spaces or control characters) or its machine (a machine of the plugin pnp)."),
    ("bad_policy", 422, "The switch of security is not one that can change while the server runs, or its value is not of the right type or is out of range."),
    ("bad_printer", 422, "A printer of the list does not fit: its family (bambu_lan or moonraker), its link (sim or lan), its id, its name, its address (host or host:port), its serial, its fingerprint or its access code."),
    ("bad_profile", 422, "The inspection profile is not valid."),
    ("bad_rack", 422, "The rack of a job request of the second version does not fit: places that repeat, the test pin as a place, more places than rack_max_slots, a carry_slot without carry_in, or a place the machine does not define or reach."),
    ("bad_request", 400, "The body of the request does not read as the request needs."),
    ("bad_rules", 422, "The rules of a check are not an object of known rules with values inside their ranges."),
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
    ("bambu_lan_off", 409, "The LAN link to a real Bambu Lab printer is off on this server (the switch bambu_lan_enabled of server.cfg), so only the simulated printer (host sim) is reached."),
    ("bgcode_unsupported", 422, "A binary G-code file uses something the server does not read: another version of the format, or G-code packed with MeatPack."),
    ("blob_damaged", 500, "The bytes of an artifact are missing or are not what their hash says."),
    ("both_sources", 422, "The job brings the work as data and as files; it must be one of them."),
    ("catalog_readonly", 409, "The catalog is built in and is not written, not even by an administrator."),
    ("compat_off", 404, "The routes of Moonraker and OctoPrint at the root of the address are off: the switch compat_api of the security page is not on."),
    ("compat_write_off", 404, "The write routes of Moonraker and OctoPrint are off: the switch compat_write of the security page is not on."),
    ("design_run_unavailable", 501, "The server is not set up to run the design program: design_run is off or design_program is not set."),
    ("enclosure_open", 409, "The enclosure input of the laser machine says open and the machine requires it closed."),
    ("engine_missing", 422, "The kind of task is known but this build has no engine for it."),
    ("farm_no_free_printer", 409, "No printer of the list is free, unlatched and able to take the job."),
    ("file_too_big", 413, "The file is bigger than project_max_file_mb."),
    ("focus_not_checked", 409, "The focus input of the laser machine says the head is not focused and the machine requires it to be."),
    ("forbidden", 403, "The role of the caller is not enough for this request."),
    ("forbidden_scope", 403, "The token is limited to scopes that do not include this one, the request would give more than the token has (or outlive it), or a token of a design program was asked for a scope of the machines."),
    ("hash_mismatch", 422, "The bytes of the upload do not have the SHA-256 that was declared; the upload is discarded."),
    ("head_refused", 502, "The node of the drill head answered with a refusal."),
    ("head_unreachable", 502, "The node of the drill head does not answer."),
    ("heater_timeout", 409, "A heater did not get to its target in heat_timeout_s: the job stops and the heaters go off."),
    ("idempotency_conflict", 409, "The idempotency key was already used by this caller for a request with another body."),
    ("input_too_large", 413, "The files of a job are bigger than max_input_mib of the worker."),
    ("internal", 500, "A task of the server failed; the server is still up."),
    ("job_active", 409, "The job is running: stop it first."),
    ("jog_while_running", 409, "A job is running or held on the machine: stop it before a jog."),
    ("laser_estop_latched", 409, "An emergency stop latched the laser machine: nothing is prepared or started on it until an administrator releases it."),
    ("laser_feed_limit", 422, "A line of the G-code program asks for a cutting feed over max_feed_mm_min of the laser machine."),
    ("laser_power_limit", 422, "The job asks for more power (S) than the limit of the laser machine (power_limit_pct of max_s) and the machine is set to reject it."),
    ("last_admin", 409, "The last administrator cannot be deleted, disabled or demoted."),
    ("local_only", 403, "While the server has no user, only the computer it runs on may make the first administrator."),
    ("locked", 423, "Somebody else is editing the project (its lock has not expired)."),
    ("machine_alarm", 409, "The machine is latched by an emergency stop (or in alarm): nothing is prepared, started or jogged until an administrator releases it."),
    ("machine_busy", 409, "A job is running on the machine."),
    ("machine_exists", 409, "A machine with this id already exists."),
    ("machines_busy", 409, "A job is running: the update would restart the server and stop it."),
    ("missing_layer", 422, "The job brings no file, or the check needs a copper layer and none of the files is one."),
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
    ("not_supported", 501, "The plugin of the machine does not support this operation (for example, holding a drilling job), or its machines are made elsewhere."),
    ("out_of_table", 422, "A hole or a path is outside the table of the machine."),
    ("out_of_travel", 422, "A move of the program or of a jog goes outside the travel of the machine (the machine's switch is on reject)."),
    ("outside_course", 422, "The work goes past the course of an axis (the machine's switch is on reject)."),
    ("plugin_failed", 502, "The plugin or the machine behind it failed while doing this."),
    ("pnp_feeder_short", 409, "A part of the placement job is in no feeder of the pick-and-place machine, or the feeders hold fewer than the job needs while the machine requires the stock."),
    ("pnp_node_disabled", 409, "The node of the pick-and-place machine is switched off, so it is not asked."),
    ("pnp_node_exists", 409, "A node with this id or this address is already registered."),
    ("pnp_node_not_ready", 409, "A node of this pick-and-place machine is silent, refuses the token, says it is safe or fails its own check, so the job does not start."),
    ("pnp_nodes_full", 409, "The list of the nodes of the pick-and-place machines is full (pnp_nodes_max)."),
    ("pnp_nodes_off", 501, "The nodes of the pick-and-place machines are switched off on this server (pnp_nodes_enabled)."),
    ("precondition_failed", 412, "The If-Match tag is not the one of the current version: read the file again before writing."),
    ("precondition_required", 428, "A write over an existing file needs If-Match with the tag (SHA-256) of the version it was made from."),
    ("printer_disconnected", 502, "The printer does not answer."),
    ("printer_exists", 409, "A printer with this id, this address or this serial is already registered."),
    ("printer_latched", 409, "An emergency stop or a fault of a heater latched the printer: nothing is prepared, started or heated on it until an administrator releases it."),
    ("printer_untrusted", 502, "The certificate of the printer is not the pinned one, or none is pinned: the connection is refused."),
    ("printers_full", 409, "The list of the printers is full (printers_max)."),
    ("printers_off", 501, "The list of the printers is switched off on this server (printers_enabled)."),
    ("program_over_limit", 422, "A program for a 3D printer asks for a temperature or a feed over a limit of the machine, so the whole program is refused."),
    ("project_quota", 507, "The project, the application or the store would pass its limit (project_max_mb, project_max_files, project_max_projects)."),
    ("queue_full", 429, "As many tasks are waiting or running as the server accepts (max_tasks)."),
    ("quota", 429, "The caller already has as many tasks waiting or running as one user may (tasks_per_user)."),
    ("sensor_fault", 409, "A heater reads below min_temp_c, which is a broken sensor: the printer was stopped and the machine is latched until an administrator releases it."),
    ("shares_off", 501, "Sharing is switched off on this server (shares_enabled)."),
    ("slice_failed", 502, "The program that slices did not give a result."),
    ("slice_limit", 422, "The object or the profile goes past a limit of the printer or of the slicer; nothing is cut down to fit."),
    ("slice_off", 404, "Slicing on the server is off: the switch slice of the security page is not on."),
    ("spindle_limit", 422, "The job asks for more than the spindle of the machine gives (the machine's switch is on reject)."),
    ("stale_authorization", 409, "The hash a person authorised, or the one a start names, is not the hash of the job as it is now, so nothing starts."),
    ("stale_parent", 409, "The revision does not follow the last one of the project: look at what changed first."),
    ("storage", 500, "The data folder could not be written."),
    ("store", 500, "The inspection profile could not be saved."),
    ("store_full", 507, "The artifact store would pass artifact_store_gb."),
    ("task_active", 409, "The task has not finished: cancel it first."),
    ("tasks_off", 501, "The computing tasks are switched off on this server (tasks_enabled)."),
    ("temperature_over_limit", 422, "A target of temperature is over the limit of the heater (the profile of the model of the printer, or the limit set for the machine)."),
    ("test_fire_refused", 422, "A test fire asks for more power than test_fire_max_pct of the laser machine."),
    ("thermal_runaway", 409, "A heater is far from its target for too long or is past its limit: the printer was stopped and the machine is latched until an administrator releases it."),
    ("throttled", 429, "Too many wrong passwords, or too many tickets waiting: try again later."),
    ("too_big", 422, "The request or the work is bigger than the server accepts (413 for a body over max_body_mb)."),
    ("too_many_clients", 429, "As many clients listen to the stream of events as events_clients_max allows."),
    ("too_many_holes", 422, "More holes than one inspection takes."),
    ("unauthorized", 401, "The token is missing, not valid or has expired."),
    ("unit_unknown", 422, "A Gerber or Excellon file uses coordinates before it says its units."),
    ("unknown_artifact", 404, "There is no artifact with this id, or it belongs to somebody else."),
    ("unknown_catalog", 404, "There is no catalog with this name."),
    ("unknown_file", 404, "The project has no such file."),
    ("unknown_item", 404, "The catalog has no item with this name."),
    ("unknown_job", 404, "There is no job with this id."),
    ("unknown_kind", 422, "There is no such kind of task."),
    ("unknown_machine", 404, "There is no machine with this id."),
    ("unknown_plugin", 422, "There is no plugin of this kind."),
    ("unknown_pnp_node", 404, "There is no node of a pick-and-place machine with this id."),
    ("unknown_printer", 404, "There is no printer with this id in the list."),
    ("unknown_project", 404, "There is no project with this id, or it belongs to somebody else."),
    ("unknown_session", 404, "There is no open login with this id, or it is not the caller's, or the login cannot be renewed any more."),
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
    ("weak_confirm_required", 409, "The new value of the switch of security is looser than recommended: send confirm_weak: true to apply it."),
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
