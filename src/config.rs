//! The server's own settings: one small text file of `key = value` lines, as the machine file of the programs. Every key is optional, an
//! unknown key or a value out of range is an error (nothing is silently replaced by a default).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    /// Where the server listens. The default port, 18200, is the EH3D server's own (ARMOR and HYDRA-UMC use others on the same machine).
    pub listen: SocketAddr,
    /// Where users, tokens, machines and jobs are kept.
    pub data_dir: PathBuf,
    /// How long a login lasts, in hours.
    pub session_hours: u32,
    /// The largest job (or file) a request may carry, in megabytes.
    pub max_body_mb: usize,
    /// How many finished jobs are kept in the list.
    pub keep_jobs: usize,
    /// A folder with the pages of EH3D-STUDIO: when it is set the server serves them at `/` (on the same address, so the pages need no
    /// cross-origin permission). Not set: the server answers only the API.
    pub studio_dir: Option<PathBuf>,
    /// The folder where the server and the update helper meet (see `updates`). Not set: the updates are not offered.
    pub updates_dir: Option<PathBuf>,
    /// How long, in seconds, a G-code job waits for the machine to finish its last moves (the controller takes lines ahead of the movement)
    /// before it is marked as failed.
    pub gcode_finish_wait_s: u64,
    /// How long, in seconds, a GRBL machine may take to answer an ordinary line of a G-code job before the job is stopped and marked as failed.
    pub grbl_line_timeout_s: u64,
    /// The same for the homing cycle (`$H`), which answers only when the machine has finished.
    pub grbl_homing_timeout_s: u64,
    /// The same for a jog (`$J=...`).
    pub grbl_jog_timeout_s: u64,
    /// How many lines before the one a stopped G-code job had reached a resumed job starts again: the controller takes lines ahead of the movement, so the last ones sent were not all done.
    pub resume_margin_lines: usize,
    /// How many wrong passwords in a row, with any names, block an address (the one of the socket) for a while; 0 turns it off.
    pub login_ip_limit: u32,
    /// Accept the token of the session in the address (`?token=`) of the camera and of the websocket, as the first versions of the clients sent it. Off: only a ticket.
    pub url_token: bool,
    /// While the server has no user yet, answer only the connections that come from this very computer (127.0.0.1): the first administrator is made from here. Off by default: the server answers whoever reaches it, as it always did.
    pub local_until_setup: bool,
    /// What happens when any thread of the server panics: true stops the machines that are running and exits (the service manager starts the server again); false
    /// only writes the panic to the console and goes on, with the locks recovered.
    pub exit_on_panic: bool,
    /// Take the address of a client from the `X-Forwarded-For` header, but only on a connection that comes from 127.0.0.1 (the reverse proxy on this computer). Off by default:
    /// the address of the socket is the one that counts.
    pub trust_forwarded_for: bool,
    /// Keep the logins in the data folder (only the SHA-256 of each token), so that a restart of the server does not log anybody out.
    pub persist_sessions: bool,
    /// How many days a token of a program lasts when the request that makes it gives no number; 0 is for ever.
    pub token_default_days: u32,
    /// How often, in seconds, the websocket of the events sends a ping.
    pub events_ping_s: u64,
    /// How long, in seconds, a websocket may stay without sending anything (a pong counts) before the server closes it.
    pub events_idle_s: u64,
    /// Write a line for every event of the audit (who did what) to `audit.jsonl` in the data folder, besides the console.
    pub audit_file: bool,
    /// Write a line at the start and at the end of every job to `eh3d-jobs.log` in the data folder.
    pub job_log: bool,
    /// The size, in kilobytes, at which `audit.jsonl` and `eh3d-jobs.log` are set aside (one older copy is kept).
    pub log_max_kb: u64,
    /// Whether the server takes computing tasks at all (`/api/v1/tasks`). The tasks have no effect on any machine.
    pub tasks_enabled: bool,
    /// Whether the engine that exercises the task system (`server.selftest`) is there. Off: only the engines of the design suite are.
    pub tasks_selftest: bool,
    /// How many tasks may be waiting or running, all users together.
    pub max_tasks: usize,
    /// How many finished tasks the list keeps.
    pub keep_tasks: usize,
    /// How many tasks run at the same time.
    pub max_concurrent_tasks: usize,
    /// The longest a task may run, in seconds.
    pub task_timeout_s: u64,
    /// How many tasks one user may have waiting or running.
    pub tasks_per_user: usize,
    /// The largest parameters of a task, in kilobytes.
    pub task_params_kb: usize,
    /// How long, in seconds, a request that carries an idempotency key is remembered.
    pub idempotency_ttl_s: u64,
    /// The most the artifact store holds, in gigabytes (each blob counted once).
    pub artifact_store_gb: u64,
    /// The largest artifact or upload, in megabytes.
    pub max_upload_mb: u64,
    /// How long, in seconds, an upload nobody touches is kept.
    pub upload_ttl_s: u64,
    /// How many uploads one user may have open.
    pub uploads_per_user: usize,
    /// How many days an artifact nothing points to is kept (0: for ever).
    pub artifact_unreferenced_days: u64,
    /// Whether a project or an artifact can be shared with other people at all. Off: only the owner and the administrators see anything.
    pub shares_enabled: bool,
    /// The most places a request may name in the `rack` of a drilling or routing job (`rack.first` and the entries of `rack.slots`): the tool changer of the machines has fourteen.
    pub rack_max_slots: usize,
    /// The largest file of a project of the project store, in megabytes.
    pub project_max_file_mb: u64,
    /// The most a project of the store may hold, in megabytes (every distinct version counted once).
    pub project_max_mb: u64,
    /// The most files one project of the store may have.
    pub project_max_files: usize,
    /// The most projects one application may have in the store.
    pub project_max_projects: usize,
    /// How many versions of a file are kept (0: all of them).
    pub project_versions_kept: usize,
    /// The longest an editing lock of a project lasts without being renewed, in seconds.
    pub project_lock_ttl_s: u64,
    /// A write over an existing file needs `If-Match` (else `428 precondition_required`).
    pub project_require_if_match: bool,
    /// Team mode: every person with access to the store sees and writes the projects of the application, not only their owner and the administrators.
    pub project_shared: bool,
    /// The largest dataset an administrator may upload to a catalog, in megabytes.
    pub catalog_max_mb: u64,
    /// The most items a dataset of a catalog may have.
    pub catalog_max_items: usize,
    /// The most items one page of a catalog answers with.
    pub catalog_page_max: usize,
    /// How many jobs of the generic queue may be waiting, all machines together.
    pub work_queue_max: usize,
    /// How many finished jobs of the generic queue are kept.
    pub work_keep: usize,
    /// How many clients may listen to the stream of events (`/api/v1/events/stream`) at once.
    pub events_clients_max: usize,
    /// How many events the stream keeps for a client that reconnects with `Last-Event-ID`, and how many a client may fall behind.
    pub events_queue: usize,
    /// How many notifications are kept.
    pub notifications_keep: usize,
    /// Only the applications named in the table of docs/AUTH.md (and `auth_extra_apps`) may log in or have a store.
    pub auth_known_apps_only: bool,
    /// More application names, besides the known ones.
    pub auth_extra_apps: Vec<String>,
    /// How many logins of the applications one person may have open at once (0: no limit); the oldest is closed when a new one comes.
    pub auth_max_sessions: usize,
    /// How many wrong passwords in a row block a name for a while.
    pub auth_max_failures: u32,
    /// The first block after too many wrong passwords, in seconds; it doubles for as long as they go on.
    pub auth_lockout_s: u64,
    /// The longest a login can be renewed in all, in hours counted from the moment it was made.
    pub auth_refresh_max_hours: u64,
    /// The shortest password, in characters.
    pub password_min_len: usize,
    /// How many of the four classes (lower case, upper case, digits, other) a password needs.
    pub password_classes: usize,
    /// A password may not contain the name of the person (names of four characters or more).
    pub password_deny_name: bool,
}

impl ServerConfig {
    /// The times a GRBL controller waits, by the kind of line.
    pub fn grbl_timeouts(&self) -> eh3d_controller::grbl::Timeouts {
        use std::time::Duration;
        eh3d_controller::grbl::Timeouts {
            line: Duration::from_secs(self.grbl_line_timeout_s),
            homing: Duration::from_secs(self.grbl_homing_timeout_s),
            jog: Duration::from_secs(self.grbl_jog_timeout_s),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:18200".parse().expect("a valid default address"),
            data_dir: PathBuf::from("data"),
            session_hours: 12,
            max_body_mb: 8,
            keep_jobs: 200,
            studio_dir: None,
            updates_dir: None,
            gcode_finish_wait_s: 900,
            grbl_line_timeout_s: 5,
            grbl_homing_timeout_s: 120,
            grbl_jog_timeout_s: 60,
            resume_margin_lines: 16,
            login_ip_limit: 10,
            url_token: false,
            local_until_setup: false,
            exit_on_panic: true,
            trust_forwarded_for: false,
            persist_sessions: false,
            token_default_days: 0,
            events_ping_s: 30,
            events_idle_s: 90,
            audit_file: true,
            job_log: true,
            log_max_kb: 1024,
            tasks_enabled: true,
            tasks_selftest: false,
            max_tasks: 100,
            keep_tasks: 200,
            max_concurrent_tasks: 2,
            task_timeout_s: 600,
            tasks_per_user: 10,
            task_params_kb: 64,
            idempotency_ttl_s: 86_400,
            artifact_store_gb: 4,
            max_upload_mb: 512,
            upload_ttl_s: 86_400,
            uploads_per_user: 4,
            artifact_unreferenced_days: 30,
            shares_enabled: true,
            rack_max_slots: 14,
            project_max_file_mb: 64,
            project_max_mb: 512,
            project_max_files: 2000,
            project_max_projects: 1000,
            project_versions_kept: 50,
            project_lock_ttl_s: 300,
            project_require_if_match: true,
            project_shared: false,
            catalog_max_mb: 16,
            catalog_max_items: 100_000,
            catalog_page_max: 200,
            work_queue_max: 200,
            work_keep: 500,
            events_clients_max: 32,
            events_queue: 256,
            notifications_keep: 200,
            auth_known_apps_only: true,
            auth_extra_apps: Vec::new(),
            auth_max_sessions: 20,
            auth_max_failures: 5,
            auth_lockout_s: 30,
            auth_refresh_max_hours: 168,
            password_min_len: 8,
            password_classes: 1,
            password_deny_name: true,
        }
    }
}

fn number(key: &str, value: &str, low: u64, high: u64) -> Result<u64, String> {
    let n: u64 = value
        .parse()
        .map_err(|_| format!("{key} must be a whole number, not '{value}'"))?;
    if n < low || n > high {
        return Err(format!("{key} must be from {low} to {high}"));
    }
    Ok(n)
}

fn flag(key: &str, value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(format!("{key} must be true or false, not '{value}'")),
    }
}

pub fn parse(text: &str) -> Result<ServerConfig, String> {
    let mut c = ServerConfig::default();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {}: expected key = value", n + 1));
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        let at = |e: String| format!("line {}: {e}", n + 1);
        match key.as_str() {
            "listen" => {
                c.listen = value.parse().map_err(|_| {
                    at(format!(
                        "listen must be an address like 0.0.0.0:18200, not '{value}'"
                    ))
                })?
            }
            "data_dir" => {
                if value.is_empty() {
                    return Err(at("data_dir must not be empty".into()));
                }
                c.data_dir = PathBuf::from(value);
            }
            "session_hours" => c.session_hours = number(&key, value, 1, 720).map_err(at)? as u32,
            "max_body_mb" => c.max_body_mb = number(&key, value, 1, 256).map_err(at)? as usize,
            "keep_jobs" => c.keep_jobs = number(&key, value, 10, 100_000).map_err(at)? as usize,
            "studio_dir" => {
                c.studio_dir = if value.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(value))
                }
            }
            "updates_dir" => {
                c.updates_dir = if value.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(value))
                }
            }
            "gcode_finish_wait_s" => {
                c.gcode_finish_wait_s = number(&key, value, 1, 86_400).map_err(at)?
            }
            "grbl_line_timeout_s" => {
                c.grbl_line_timeout_s = number(&key, value, 1, 3_600).map_err(at)?
            }
            "grbl_homing_timeout_s" => {
                c.grbl_homing_timeout_s = number(&key, value, 1, 3_600).map_err(at)?
            }
            "grbl_jog_timeout_s" => {
                c.grbl_jog_timeout_s = number(&key, value, 1, 3_600).map_err(at)?
            }
            "local_until_setup" => {
                c.local_until_setup = match value.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" | "1" => true,
                    "false" | "no" | "off" | "0" => false,
                    _ => {
                        return Err(at(format!(
                            "local_until_setup must be true or false, not '{value}'"
                        )))
                    }
                }
            }
            "url_token" => {
                c.url_token = match value.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" | "1" => true,
                    "false" | "no" | "off" | "0" => false,
                    _ => {
                        return Err(at(format!(
                            "url_token must be true or false, not '{value}'"
                        )))
                    }
                }
            }
            "login_ip_limit" => {
                c.login_ip_limit = number(&key, value, 0, 1_000).map_err(at)? as u32
            }
            "resume_margin_lines" => {
                c.resume_margin_lines = number(&key, value, 0, 1_000).map_err(at)? as usize
            }
            "exit_on_panic" => c.exit_on_panic = flag(&key, value).map_err(at)?,
            "trust_forwarded_for" => c.trust_forwarded_for = flag(&key, value).map_err(at)?,
            "persist_sessions" => c.persist_sessions = flag(&key, value).map_err(at)?,
            "audit_file" => c.audit_file = flag(&key, value).map_err(at)?,
            "job_log" => c.job_log = flag(&key, value).map_err(at)?,
            "token_default_days" => {
                c.token_default_days = number(&key, value, 0, 3_650).map_err(at)? as u32
            }
            "events_ping_s" => c.events_ping_s = number(&key, value, 1, 3_600).map_err(at)?,
            "events_idle_s" => c.events_idle_s = number(&key, value, 2, 86_400).map_err(at)?,
            "log_max_kb" => c.log_max_kb = number(&key, value, 16, 1_048_576).map_err(at)?,
            "tasks_enabled" => c.tasks_enabled = flag(&key, value).map_err(at)?,
            "tasks_selftest" => c.tasks_selftest = flag(&key, value).map_err(at)?,
            "max_tasks" => c.max_tasks = number(&key, value, 1, 100_000).map_err(at)? as usize,
            "keep_tasks" => c.keep_tasks = number(&key, value, 0, 100_000).map_err(at)? as usize,
            "max_concurrent_tasks" => {
                c.max_concurrent_tasks = number(&key, value, 1, 64).map_err(at)? as usize
            }
            "task_timeout_s" => c.task_timeout_s = number(&key, value, 1, 604_800).map_err(at)?,
            "tasks_per_user" => {
                c.tasks_per_user = number(&key, value, 1, 10_000).map_err(at)? as usize
            }
            "task_params_kb" => {
                c.task_params_kb = number(&key, value, 1, 65_536).map_err(at)? as usize
            }
            "idempotency_ttl_s" => {
                c.idempotency_ttl_s = number(&key, value, 60, 2_592_000).map_err(at)?
            }
            "artifact_store_gb" => {
                c.artifact_store_gb = number(&key, value, 1, 100_000).map_err(at)?
            }
            "max_upload_mb" => c.max_upload_mb = number(&key, value, 1, 65_536).map_err(at)?,
            "upload_ttl_s" => c.upload_ttl_s = number(&key, value, 60, 2_592_000).map_err(at)?,
            "uploads_per_user" => {
                c.uploads_per_user = number(&key, value, 1, 1_000).map_err(at)? as usize
            }
            "shares_enabled" => c.shares_enabled = flag(&key, value).map_err(at)?,
            "rack_max_slots" => c.rack_max_slots = number(&key, value, 1, 14).map_err(at)? as usize,
            "artifact_unreferenced_days" => {
                c.artifact_unreferenced_days = number(&key, value, 0, 36_500).map_err(at)?
            }
            "project_max_file_mb" => {
                c.project_max_file_mb = number(&key, value, 1, 4_096).map_err(at)?
            }
            "project_max_mb" => c.project_max_mb = number(&key, value, 1, 65_536).map_err(at)?,
            "project_max_files" => {
                c.project_max_files = number(&key, value, 1, 100_000).map_err(at)? as usize
            }
            "project_max_projects" => {
                c.project_max_projects = number(&key, value, 1, 1_000_000).map_err(at)? as usize
            }
            "project_versions_kept" => {
                c.project_versions_kept = number(&key, value, 0, 10_000).map_err(at)? as usize
            }
            "project_lock_ttl_s" => {
                c.project_lock_ttl_s = number(&key, value, 10, 86_400).map_err(at)?
            }
            "project_require_if_match" => {
                c.project_require_if_match = flag(&key, value).map_err(at)?
            }
            "project_shared" => c.project_shared = flag(&key, value).map_err(at)?,
            "catalog_max_mb" => c.catalog_max_mb = number(&key, value, 1, 1_024).map_err(at)?,
            "catalog_max_items" => {
                c.catalog_max_items = number(&key, value, 1, 10_000_000).map_err(at)? as usize
            }
            "catalog_page_max" => {
                c.catalog_page_max = number(&key, value, 1, 10_000).map_err(at)? as usize
            }
            "work_queue_max" => {
                c.work_queue_max = number(&key, value, 1, 100_000).map_err(at)? as usize
            }
            "work_keep" => c.work_keep = number(&key, value, 0, 1_000_000).map_err(at)? as usize,
            "events_clients_max" => {
                c.events_clients_max = number(&key, value, 1, 10_000).map_err(at)? as usize
            }
            "events_queue" => {
                c.events_queue = number(&key, value, 16, 65_536).map_err(at)? as usize
            }
            "notifications_keep" => {
                c.notifications_keep = number(&key, value, 0, 100_000).map_err(at)? as usize
            }
            "auth_known_apps_only" => c.auth_known_apps_only = flag(&key, value).map_err(at)?,
            "auth_extra_apps" => {
                let mut names = Vec::new();
                for part in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    if !crate::apps::valid_app(part) {
                        return Err(at(format!(
                            "auth_extra_apps: '{part}' is not the name of an application (lower case letters, digits and hyphens, up to 32)"
                        )));
                    }
                    names.push(part.to_string());
                }
                c.auth_extra_apps = names;
            }
            "auth_max_sessions" => {
                c.auth_max_sessions = number(&key, value, 0, 10_000).map_err(at)? as usize
            }
            "auth_max_failures" => {
                c.auth_max_failures = number(&key, value, 1, 100).map_err(at)? as u32
            }
            "auth_lockout_s" => c.auth_lockout_s = number(&key, value, 1, 86_400).map_err(at)?,
            "auth_refresh_max_hours" => {
                c.auth_refresh_max_hours = number(&key, value, 1, 8_760).map_err(at)?
            }
            "password_min_len" => {
                c.password_min_len = number(&key, value, 8, 128).map_err(at)? as usize
            }
            "password_classes" => {
                c.password_classes = number(&key, value, 1, 4).map_err(at)? as usize
            }
            "password_deny_name" => c.password_deny_name = flag(&key, value).map_err(at)?,
            other => return Err(at(format!("unknown key '{other}'"))),
        }
    }
    if c.events_idle_s <= c.events_ping_s {
        return Err("events_idle_s must be longer than events_ping_s".into());
    }
    Ok(c)
}

/// Reads the file at `path`; with no path, `server.cfg` in the working folder, and the defaults when it is not there.
pub fn load(path: Option<&Path>) -> Result<ServerConfig, String> {
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            parse(&text)
        }
        None => match std::fs::read_to_string("server.cfg") {
            Ok(text) => parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ServerConfig::default()),
            Err(e) => Err(format!("server.cfg: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_studio_folder_is_optional() {
        assert_eq!(parse("").unwrap().studio_dir, None);
        assert_eq!(
            parse(
                "studio_dir =
"
            )
            .unwrap()
            .studio_dir,
            None
        );
        assert_eq!(
            parse(
                "studio_dir = /opt/eh3d-studio
"
            )
            .unwrap()
            .studio_dir,
            Some(PathBuf::from("/opt/eh3d-studio"))
        );
    }

    #[test]
    fn the_defaults_are_the_servers_own_port_and_a_data_folder() {
        let c = parse("").unwrap();
        assert_eq!(c.listen.port(), 18200);
        assert_eq!(c.data_dir, PathBuf::from("data"));
        assert_eq!((c.session_hours, c.max_body_mb, c.keep_jobs), (12, 8, 200));
    }

    #[test]
    fn every_key_is_read_and_comments_are_ignored() {
        let c = parse("# a comment\nlisten = 127.0.0.1:18300\ndata_dir = /var/lib/eh3d\nsession_hours = 4\nmax_body_mb = 16 \n; another\nkeep_jobs = 50\n").unwrap();
        assert_eq!(c.listen, "127.0.0.1:18300".parse().unwrap());
        assert_eq!(c.data_dir, PathBuf::from("/var/lib/eh3d"));
        assert_eq!((c.session_hours, c.max_body_mb, c.keep_jobs), (4, 16, 50));
    }

    #[test]
    fn the_wait_for_the_end_of_a_gcode_job_is_a_setting() {
        assert_eq!(parse("").unwrap().gcode_finish_wait_s, 900);
        assert_eq!(
            parse(
                "gcode_finish_wait_s = 60
"
            )
            .unwrap()
            .gcode_finish_wait_s,
            60
        );
        assert!(parse(
            "gcode_finish_wait_s = 0
"
        )
        .is_err());
    }

    #[test]
    fn the_waits_of_a_grbl_machine_are_settings_with_safe_defaults() {
        let t = parse("").unwrap().grbl_timeouts();
        assert_eq!(
            (t.line.as_secs(), t.homing.as_secs(), t.jog.as_secs()),
            (5, 120, 60)
        );
        let c = parse(
            "grbl_line_timeout_s = 10\ngrbl_homing_timeout_s = 300\ngrbl_jog_timeout_s = 90\n",
        )
        .unwrap();
        let t = c.grbl_timeouts();
        assert_eq!(
            (t.line.as_secs(), t.homing.as_secs(), t.jog.as_secs()),
            (10, 300, 90)
        );
        assert!(parse("grbl_homing_timeout_s = 0\n").is_err());
        assert!(parse("grbl_jog_timeout_s = 99999\n").is_err());
        assert!(parse("grbl_line_timeout_s = fast\n").is_err());
    }

    #[test]
    fn the_margin_of_a_resumed_gcode_job_is_a_setting() {
        assert_eq!(parse("").unwrap().resume_margin_lines, 16);
        assert_eq!(
            parse("resume_margin_lines = 0\n")
                .unwrap()
                .resume_margin_lines,
            0
        );
        assert!(parse("resume_margin_lines = 5000\n").is_err());
    }

    #[test]
    fn the_token_in_the_address_is_off_unless_asked_for() {
        assert!(!parse("").unwrap().url_token);
        assert!(parse("url_token = yes\n").unwrap().url_token);
        assert!(parse("url_token = maybe\n").is_err());
    }

    #[test]
    fn the_new_switches_have_conservative_defaults() {
        let c = parse("").unwrap();
        assert!(c.exit_on_panic, "a panic stops the machines and restarts");
        assert!(!c.trust_forwarded_for, "the proxy header is not trusted");
        assert!(!c.persist_sessions && c.audit_file && c.job_log);
        assert_eq!(
            (c.token_default_days, c.events_ping_s, c.events_idle_s),
            (0, 30, 90)
        );
        let c = parse(
            "exit_on_panic = off
trust_forwarded_for = yes
persist_sessions = yes
audit_file = 0
job_log = false
token_default_days = 90
events_ping_s = 10
events_idle_s = 40
log_max_kb = 64
",
        )
        .unwrap();
        assert!(!c.exit_on_panic && c.trust_forwarded_for && c.persist_sessions);
        assert!(!c.audit_file && !c.job_log);
        assert_eq!(
            (
                c.token_default_days,
                c.events_ping_s,
                c.events_idle_s,
                c.log_max_kb
            ),
            (90, 10, 40, 64)
        );
        assert!(parse(
            "exit_on_panic = maybe
"
        )
        .is_err());
        assert!(parse(
            "token_default_days = 99999
"
        )
        .is_err());
        assert!(parse(
            "events_ping_s = 30
events_idle_s = 30
"
        )
        .is_err());
        assert!(parse(
            "log_max_kb = 1
"
        )
        .is_err());
    }

    #[test]
    fn the_task_settings_are_conservative_and_checked() {
        let c = parse("").unwrap();
        assert!(c.tasks_enabled && !c.tasks_selftest);
        assert_eq!(
            (c.max_tasks, c.keep_tasks, c.max_concurrent_tasks),
            (100, 200, 2)
        );
        assert_eq!(
            (c.task_timeout_s, c.tasks_per_user, c.task_params_kb),
            (600, 10, 64)
        );
        assert_eq!(c.idempotency_ttl_s, 86_400);
        let c = parse(
            "tasks_enabled = off\ntasks_selftest = on\nmax_concurrent_tasks = 4\ntask_timeout_s = 30\n",
        )
        .unwrap();
        assert!(!c.tasks_enabled && c.tasks_selftest);
        assert_eq!((c.max_concurrent_tasks, c.task_timeout_s), (4, 30));
        assert!(parse("max_concurrent_tasks = 0\n").is_err());
        assert!(parse("task_timeout_s = 0\n").is_err());
        assert!(parse("idempotency_ttl_s = 5\n").is_err());
        assert!(parse("tasks_selftest = perhaps\n").is_err());
    }

    #[test]
    fn the_artifact_settings_are_conservative_and_checked() {
        let c = parse("").unwrap();
        assert_eq!((c.artifact_store_gb, c.max_upload_mb), (4, 512));
        assert_eq!((c.upload_ttl_s, c.uploads_per_user), (86_400, 4));
        assert_eq!(c.artifact_unreferenced_days, 30);
        let c = parse(
            "artifact_store_gb = 1\nmax_upload_mb = 16\nupload_ttl_s = 600\nuploads_per_user = 2\nartifact_unreferenced_days = 0\n",
        )
        .unwrap();
        assert_eq!((c.artifact_store_gb, c.max_upload_mb), (1, 16));
        assert_eq!((c.upload_ttl_s, c.uploads_per_user), (600, 2));
        assert_eq!(c.artifact_unreferenced_days, 0, "0 keeps them for ever");
        assert!(c.shares_enabled || !parse("shares_enabled = off\n").unwrap().shares_enabled);
        assert!(parse("shares_enabled = sometimes\n").is_err());
        assert!(parse("artifact_store_gb = 0\n").is_err());
        assert!(parse("upload_ttl_s = 10\n").is_err());
        assert!(parse("uploads_per_user = 0\n").is_err());
        assert!(parse("max_upload_mb = lots\n").is_err());
    }

    #[test]
    fn the_most_places_of_a_rack_request_is_a_setting() {
        assert_eq!(parse("").unwrap().rack_max_slots, 14);
        assert_eq!(parse("rack_max_slots = 6\n").unwrap().rack_max_slots, 6);
        assert!(parse("rack_max_slots = 0\n").is_err());
        assert!(parse("rack_max_slots = 15\n").is_err());
        assert!(parse("rack_max_slots = many\n").is_err());
    }

    #[test]
    fn the_settings_of_the_project_store_are_conservative_and_checked() {
        let c = parse("").unwrap();
        assert_eq!(c.project_max_file_mb, 64);
        assert_eq!(c.project_max_mb, 512);
        assert_eq!(c.project_versions_kept, 50);
        assert_eq!(c.project_lock_ttl_s, 300);
        // a write over a file needs its tag, and the projects are their owners' only
        assert!(c.project_require_if_match);
        assert!(!c.project_shared);
        let set = parse(
            "project_max_file_mb = 8
project_max_mb = 64
project_max_files = 10
project_versions_kept = 0
project_shared = on
project_require_if_match = no
",
        )
        .unwrap();
        assert_eq!(set.project_max_file_mb, 8);
        assert_eq!(set.project_versions_kept, 0);
        assert!(set.project_shared && !set.project_require_if_match);
        assert!(parse(
            "project_max_file_mb = 0
"
        )
        .is_err());
        assert!(parse(
            "project_lock_ttl_s = 5
"
        )
        .is_err());
        assert!(parse(
            "project_max_files = lots
"
        )
        .is_err());
        assert!(parse(
            "project_shared = maybe
"
        )
        .is_err());
    }

    #[test]
    fn the_settings_of_the_catalogs_are_checked() {
        let c = parse("").unwrap();
        assert_eq!(
            (c.catalog_max_mb, c.catalog_max_items, c.catalog_page_max),
            (16, 100_000, 200)
        );
        let set = parse(
            "catalog_max_mb = 2
catalog_page_max = 10
",
        )
        .unwrap();
        assert_eq!((set.catalog_max_mb, set.catalog_page_max), (2, 10));
        assert!(parse(
            "catalog_max_mb = 0
"
        )
        .is_err());
        assert!(parse(
            "catalog_page_max = 0
"
        )
        .is_err());
        assert!(parse(
            "catalog_max_items = many
"
        )
        .is_err());
    }

    #[test]
    fn the_settings_of_the_queue_and_the_stream_are_checked() {
        let c = parse("").unwrap();
        assert_eq!(
            (
                c.work_queue_max,
                c.work_keep,
                c.events_clients_max,
                c.events_queue,
                c.notifications_keep
            ),
            (200, 500, 32, 256, 200)
        );
        let set = parse(
            "work_queue_max = 3
events_clients_max = 1
events_queue = 16
",
        )
        .unwrap();
        assert_eq!(
            (set.work_queue_max, set.events_clients_max, set.events_queue),
            (3, 1, 16)
        );
        assert!(parse(
            "work_queue_max = 0
"
        )
        .is_err());
        assert!(parse(
            "events_clients_max = 0
"
        )
        .is_err());
        assert!(parse(
            "events_queue = 8
"
        )
        .is_err());
        assert!(parse(
            "notifications_keep = many
"
        )
        .is_err());
    }

    #[test]
    fn mistakes_are_errors_not_silent_defaults() {
        assert!(parse("lisen = 1.2.3.4:5\n")
            .unwrap_err()
            .contains("unknown key"));
        assert!(parse("listen = nowhere\n").is_err());
        assert!(parse("session_hours = 0\n").is_err());
        assert!(parse("max_body_mb = lots\n").is_err());
        assert!(parse("keep_jobs = 5\n").is_err());
        assert!(parse("just words\n").is_err());
        assert!(parse("data_dir =\n").is_err());
    }
}
