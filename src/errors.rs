//! The catalog of the error codes of the API: every refusal is `{"error": "<code>", "detail": "..."}` and the code is one of these. The catalog is the one list: the
//! test `tests/errors.rs` reads the source of the server and fails when a code is used that is not here (or is here and no longer used), and writes
//! `docs/error-codes.json`, which the clients read in their own tests to be sure that each code reaches the person as a sentence of their language.
//!
//! The status is the one the code comes with in the usual case; a few codes arrive with another status where the situation is different (`bad_request` is 400 for a body
//! that does not read and 422 for a body that reads but does not fit). The description is for whoever reads the documentation, in English.

/// `(code, status, description)`.
pub const ALL: &[(&str, u16, &str)] = &[
    ("bad_address", 422, "The address of a machine or of its drill head node is missing or is not host:port."),
    ("bad_board", 422, "board_mm is not from 0 to 50."),
    ("bad_cfg", 422, "The tuning text of the machine does not read."),
    ("bad_credentials", 401, "The name or the password is wrong."),
    ("bad_days", 422, "The days a token lasts are not a number from 1 to 3650."),
    ("bad_dcf", 422, "The text of DRILLPRO.DCF or ROUTEPRO.RCF does not read."),
    ("bad_gcode", 422, "A line of the G-code program cannot be sent (control characters, a $ line that is not allowed, a line too long)."),
    ("bad_hole", 422, "A hole has a coordinate that is not a number."),
    ("bad_id", 422, "The id of a machine is not made of lowercase letters, digits and hyphens (up to 32)."),
    ("bad_light", 422, "The query of the ring of LEDs of the drill head is not valid."),
    ("bad_name", 422, "A name is empty, too long or has characters that are not allowed."),
    ("bad_ncd", 422, "The drilling file does not read."),
    ("bad_path", 422, "A point of a path has a coordinate that is not a number."),
    ("bad_picture", 502, "The picture the camera of the drill head sent cannot be read."),
    ("bad_profile", 422, "The inspection profile is not valid."),
    ("bad_request", 400, "The body of the request does not read as the request needs."),
    ("bad_samples", 422, "The number of samples of a measurement is out of range."),
    ("bad_setup", 403, "The set-up code is wrong, or the server already has a user."),
    ("bad_sorting", 422, "The sorting of the holes is not one the program knows."),
    ("bad_spindle", 422, "The spindle, the switch or the limits of a spindle are not valid."),
    ("bad_station", 422, "The station of a machine is not made of lowercase letters, digits and hyphens (up to 32)."),
    ("bad_tool", 422, "A tool has values out of range."),
    ("both_sources", 422, "The job brings the work as data and as files; it must be one of them."),
    ("forbidden", 403, "The role of the caller is not enough for this request."),
    ("head_refused", 502, "The node of the drill head answered with a refusal."),
    ("head_unreachable", 502, "The node of the drill head does not answer."),
    ("internal", 500, "A task of the server failed; the server is still up."),
    ("job_active", 409, "The job is running: stop it first."),
    ("last_admin", 409, "The last administrator cannot be deleted, disabled or demoted."),
    ("local_only", 403, "While the server has no user, only the computer it runs on may make the first administrator."),
    ("machine_busy", 409, "A job is running on the machine."),
    ("machine_exists", 409, "A machine with this id already exists."),
    ("machines_busy", 409, "A job is running: the update would restart the server and stop it."),
    ("no_head", 404, "The machine has no node of the drill head."),
    ("no_hole", 409, "The picture of the drill head shows no hole to calibrate with."),
    ("no_inspection", 404, "No quality check has been made on this machine since the server started."),
    ("no_work", 422, "The job has no holes, no paths and no lines to run."),
    ("not_cancellable", 409, "The job is neither ready nor running."),
    ("not_in_state", 409, "A job is held while it runs and resumed while it is held."),
    ("not_pausable", 409, "Only a G-code job can be held."),
    ("not_ready", 409, "The job already ran or is running."),
    ("not_running", 409, "The job is not running."),
    ("not_stopped", 409, "Only a cancelled or failed job can be gone on from."),
    ("out_of_table", 422, "A hole or a path is outside the table of the machine."),
    ("outside_course", 422, "The work goes past the course of an axis (the machine's switch is on reject)."),
    ("spindle_limit", 422, "The job asks for more than the spindle of the machine gives (the machine's switch is on reject)."),
    ("storage", 500, "The data folder could not be written."),
    ("store", 500, "The inspection profile could not be saved."),
    ("throttled", 429, "Too many wrong passwords, or too many tickets waiting: try again later."),
    ("too_big", 422, "The request or the work is bigger than the server accepts (413 for a body over max_body_mb)."),
    ("too_many_holes", 422, "More holes than one inspection takes."),
    ("unauthorized", 401, "The token is missing, not valid or has expired."),
    ("unknown_job", 404, "There is no job with this id."),
    ("unknown_machine", 404, "There is no machine with this id."),
    ("unknown_token", 404, "There is no token with this id."),
    ("unknown_tool", 422, "The job uses a tool that it does not describe."),
    ("unknown_user", 404, "There is no user with this name."),
    ("update_refused", 409, "The update helper refused the request."),
    ("updates_off", 501, "The updates are not set up on this server (updates_dir)."),
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
