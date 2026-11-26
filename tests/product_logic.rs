//! Edge cases of the pure parts of the server that decide things: the file of the settings and its ranges, the switches of the security page, the feed of events, the shared
//! catalogs (the upload, the search and the order), the ranges of an upload, the names, the versions, the announcement on the network and the readers of STL and 3MF files.
//! Nothing here starts a server; what is written goes to a folder of its own that is removed at the end.

use eh3d_server::apps;
use eh3d_server::artifacts::{self, NewArtifact};
use eh3d_server::auth;
use eh3d_server::catalogs::{self, CBook, CError, Query};
use eh3d_server::config::{self, ServerConfig};
use eh3d_server::feed::{Feed, FeedEvent, Filter, Visibility};
use eh3d_server::ids::{self, Kind};
use eh3d_server::mdns::{self, Question};
use eh3d_server::policy::{Hot, Policy, PolicyError, KEYS, RESTART_KEYS};
use eh3d_server::slicer::{self, Mesh};
use eh3d_server::updates;
use serde_json::{json, Value};

/// A small seeded generator, so every run sees the same bytes.
fn lcg(seed: u32, n: usize, alphabet: u32) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((s >> 16) % alphabet) as u8
        })
        .collect()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("eh3d-r5-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------- the file of the settings

fn cfg(text: &str) -> ServerConfig {
    config::parse(text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
}

fn refused(text: &str) -> String {
    config::parse(text).unwrap_err()
}

fn field(c: &ServerConfig, key: &str) -> Value {
    serde_json::to_value(c).unwrap()[key].clone()
}

/// A number setting takes its two ends and refuses the whole numbers just outside them.
fn bounds(key: &str, low: u64, high: u64) {
    for edge in [low, high] {
        let c = cfg(&format!("{key} = {edge}"));
        assert_eq!(field(&c, key).as_u64(), Some(edge), "{key} = {edge}");
    }
    let why = format!("line 1: {key} must be from {low} to {high}");
    assert_eq!(refused(&format!("{key} = {}", high + 1)), why);
    if low > 0 {
        assert_eq!(refused(&format!("{key} = {}", low - 1)), why);
    }
}

#[test]
fn an_empty_file_and_a_file_of_comments_are_the_defaults() {
    assert_eq!(cfg(""), ServerConfig::default());
    assert_eq!(cfg("# a\n; b\n\n   \n\t\n"), ServerConfig::default());
}

#[test]
fn a_line_with_no_equal_sign_is_refused_with_its_number() {
    assert_eq!(refused("bad"), "line 1: expected key = value");
    assert_eq!(refused("\n\nbad"), "line 3: expected key = value");
    assert_eq!(
        refused("session_hours = 5\n# c\nlisten"),
        "line 3: expected key = value"
    );
}

#[test]
fn a_key_nobody_knows_is_refused_by_its_name_in_lower_case() {
    assert_eq!(refused("nope = 1"), "line 1: unknown key 'nope'");
    assert_eq!(refused("NOPE = 1"), "line 1: unknown key 'nope'");
    assert_eq!(refused("= 1"), "line 1: unknown key ''");
}

#[test]
fn the_keys_are_read_in_any_letter_case_and_the_blanks_around_are_ignored() {
    assert_eq!(cfg("SESSION_HOURS = 5").session_hours, 5);
    assert_eq!(cfg("  session_hours   =   7  ").session_hours, 7);
    assert_eq!(cfg("\tsession_hours\t=\t6").session_hours, 6);
    assert_eq!(cfg("session_hours = 5\r\n").session_hours, 5);
}

#[test]
fn the_last_line_of_a_key_wins() {
    assert_eq!(cfg("session_hours = 5\nsession_hours = 9").session_hours, 9);
}

#[test]
fn a_value_that_is_not_a_whole_number_is_refused_and_shown() {
    assert_eq!(
        refused("session_hours = x"),
        "line 1: session_hours must be a whole number, not 'x'"
    );
    assert_eq!(
        refused("session_hours = -1"),
        "line 1: session_hours must be a whole number, not '-1'"
    );
    assert_eq!(
        refused("session_hours ="),
        "line 1: session_hours must be a whole number, not ''"
    );
    assert_eq!(
        refused("session_hours = 1.5"),
        "line 1: session_hours must be a whole number, not '1.5'"
    );
    assert_eq!(
        refused("session_hours = 99999999999999999999"),
        "line 1: session_hours must be a whole number, not '99999999999999999999'"
    );
}

#[test]
fn a_comment_after_a_value_is_part_of_the_value() {
    assert_eq!(
        refused("session_hours = 5 # five"),
        "line 1: session_hours must be a whole number, not '5 # five'"
    );
}

#[test]
fn a_file_that_begins_with_a_byte_order_mark_is_read_as_if_it_did_not() {
    assert_eq!(cfg("\u{feff}session_hours = 5").session_hours, 5);
    assert_eq!(
        cfg("\u{feff}# the first line is a comment\nsession_hours = 6").session_hours,
        6
    );
    assert_eq!(cfg("\u{feff}"), ServerConfig::default());
    assert_eq!(refused("\u{feff}nope = 1"), "line 1: unknown key 'nope'");
}

#[test]
fn the_file_is_read_from_disk_with_or_without_a_byte_order_mark() {
    let dir = scratch("cfg-load");
    let plain = dir.join("a.cfg");
    let marked = dir.join("b.cfg");
    std::fs::write(&plain, "session_hours = 9\n").unwrap();
    std::fs::write(&marked, "\u{feff}session_hours = 9\r\n").unwrap();
    assert_eq!(
        config::load(Some(plain.as_path())).unwrap().session_hours,
        9
    );
    assert_eq!(
        config::load(Some(marked.as_path())).unwrap().session_hours,
        9
    );
    let none = dir.join("none.cfg");
    let missing = config::load(Some(none.as_path())).unwrap_err();
    assert!(missing.contains("none.cfg"), "{missing}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_is_not_text_is_refused_by_its_name() {
    let dir = scratch("cfg-binary");
    let path = dir.join("bin.cfg");
    std::fs::write(&path, [0xFFu8, 0xFE, 0x00, 0x80]).unwrap();
    let why = config::load(Some(path.as_path())).unwrap_err();
    assert!(why.contains("bin.cfg"), "{why}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_numbers_of_the_session_and_the_body_have_their_ranges() {
    bounds("session_hours", 1, 720);
    bounds("max_body_mb", 1, 256);
    bounds("keep_jobs", 10, 100_000);
    bounds("token_default_days", 0, 3_650);
    bounds("login_ip_limit", 0, 1_000);
    bounds("resume_margin_lines", 0, 1_000);
}

#[test]
fn the_waits_of_the_machine_have_their_ranges() {
    bounds("gcode_finish_wait_s", 1, 86_400);
    bounds("grbl_line_timeout_s", 1, 3_600);
    bounds("grbl_homing_timeout_s", 1, 3_600);
    bounds("grbl_jog_timeout_s", 1, 3_600);
    bounds("log_max_kb", 16, 1_048_576);
}

#[test]
fn the_numbers_of_the_tasks_have_their_ranges() {
    bounds("max_tasks", 1, 100_000);
    bounds("keep_tasks", 0, 100_000);
    bounds("max_concurrent_tasks", 1, 64);
    bounds("task_timeout_s", 1, 604_800);
    bounds("tasks_per_user", 1, 10_000);
    bounds("task_params_kb", 1, 65_536);
    bounds("idempotency_ttl_s", 60, 2_592_000);
}

#[test]
fn the_numbers_of_the_artifacts_and_the_uploads_have_their_ranges() {
    bounds("artifact_store_gb", 1, 100_000);
    bounds("max_upload_mb", 1, 65_536);
    bounds("upload_ttl_s", 60, 2_592_000);
    bounds("uploads_per_user", 1, 1_000);
    bounds("rack_max_slots", 1, 14);
    bounds("artifact_unreferenced_days", 0, 36_500);
}

#[test]
fn the_numbers_of_the_projects_and_the_catalogs_have_their_ranges() {
    bounds("project_max_file_mb", 1, 4_096);
    bounds("project_max_mb", 1, 65_536);
    bounds("project_max_files", 1, 100_000);
    bounds("project_max_projects", 1, 1_000_000);
    bounds("project_versions_kept", 0, 10_000);
    bounds("project_lock_ttl_s", 10, 86_400);
    bounds("catalog_max_mb", 1, 1_024);
    bounds("catalog_max_items", 1, 10_000_000);
    bounds("catalog_page_max", 1, 10_000);
}

#[test]
fn the_numbers_of_the_queue_and_the_events_have_their_ranges() {
    bounds("work_queue_max", 1, 100_000);
    bounds("work_keep", 0, 1_000_000);
    bounds("events_clients_max", 1, 10_000);
    bounds("events_queue", 16, 65_536);
    bounds("notifications_keep", 0, 100_000);
}

#[test]
fn the_numbers_of_the_logins_and_the_passwords_have_their_ranges() {
    bounds("auth_max_sessions", 0, 10_000);
    bounds("auth_max_failures", 1, 100);
    bounds("auth_lockout_s", 1, 86_400);
    bounds("auth_refresh_max_hours", 1, 8_760);
    bounds("password_min_len", 8, 128);
    bounds("password_classes", 1, 4);
}

#[test]
fn the_numbers_of_the_printer_and_the_nodes_have_their_ranges() {
    bounds("bambu_lan_port", 1, 65_535);
    bounds("bambu_lan_timeout_s", 1, 60);
    bounds("printers_max", 1, 64);
    bounds("printer_poll_s", 1, 3_600);
    bounds("printer_stale_s", 5, 3_600);
    bounds("printer_timeout_s", 1, 30);
    bounds("pnp_nodes_max", 1, 200);
    bounds("pnp_node_poll_s", 1, 3_600);
    bounds("pnp_node_silence_s", 2, 3_600);
    bounds("pnp_node_timeout_s", 1, 30);
}

#[test]
fn the_numbers_of_the_slicer_and_the_design_program_have_their_ranges() {
    bounds("slice_timeout_s", 1, 600);
    bounds("slice_max_mb", 1, 200);
    bounds("slice_max_layers", 10, 20_000);
    bounds("slice_max_gcode_mb", 1, 200);
    bounds("update_keep_logins_s", 30, 900);
    bounds("design_timeout_s", 1, 3_600);
    bounds("design_max_output_kb", 1, 65_536);
    bounds("design_max_parallel", 1, 64);
}

const SWITCHES: [&str; 27] = [
    "exit_on_panic",
    "trust_forwarded_for",
    "persist_sessions",
    "audit_file",
    "audit_denials",
    "job_log",
    "tasks_enabled",
    "tasks_selftest",
    "shares_enabled",
    "project_require_if_match",
    "project_shared",
    "work_require_authorization",
    "auth_known_apps_only",
    "password_deny_name",
    "network_open",
    "update_allow_unsigned",
    "update_keep_logins",
    "mdns_announce",
    "compat_api",
    "compat_write",
    "slice_enabled",
    "bambu_lan_enabled",
    "printers_enabled",
    "pnp_nodes_enabled",
    "design_run",
    "url_token",
    "local_until_setup",
];

#[test]
fn every_switch_takes_the_words_of_yes_and_of_no_in_any_letter_case() {
    for key in SWITCHES {
        for word in ["true", "yes", "on", "1", "TRUE", "Yes", "ON"] {
            let c = cfg(&format!("{key} = {word}"));
            assert_eq!(field(&c, key), json!(true), "{key} = {word}");
        }
        for word in ["false", "no", "off", "0", "FALSE", "No", "Off"] {
            let c = cfg(&format!("{key} = {word}"));
            assert_eq!(field(&c, key), json!(false), "{key} = {word}");
        }
    }
}

#[test]
fn a_switch_with_any_other_word_is_refused_and_the_word_is_shown() {
    for key in SWITCHES {
        for word in ["maybe", "2", "", "truee", "t"] {
            assert_eq!(
                refused(&format!("{key} = {word}")),
                format!("line 1: {key} must be true or false, not '{word}'"),
                "{key}"
            );
        }
    }
}

#[test]
fn the_switches_of_the_security_page_are_off_by_default_except_the_careful_ones() {
    let c = ServerConfig::default();
    for key in [
        "network_open",
        "update_allow_unsigned",
        "compat_api",
        "slice_enabled",
        "design_run",
        "url_token",
    ] {
        assert_eq!(field(&c, key), json!(false), "{key}");
    }
    for key in [
        "audit_denials",
        "password_deny_name",
        "auth_known_apps_only",
        "exit_on_panic",
    ] {
        assert_eq!(field(&c, key), json!(true), "{key}");
    }
}

#[test]
fn the_line_of_local_until_setup_is_remembered_even_when_it_says_no() {
    assert!(!cfg("").local_until_setup_set);
    let off = cfg("local_until_setup = no");
    assert!(off.local_until_setup_set && !off.local_until_setup);
    let on = cfg("local_until_setup = 1");
    assert!(on.local_until_setup_set && on.local_until_setup);
}

#[test]
fn the_first_administrator_is_made_from_this_computer_when_the_server_can_be_reached() {
    let local = ServerConfig::default();
    assert!(!local.local_until_setup_in_force(false));
    assert!(local.local_until_setup_in_force(true));
    let open = cfg("listen = 0.0.0.0:18200");
    assert!(open.local_until_setup_in_force(false));
    let said_no = cfg("listen = 0.0.0.0:18200\nlocal_until_setup = false");
    assert!(!said_no.local_until_setup_in_force(true));
    let said_yes = cfg("local_until_setup = true");
    assert!(said_yes.local_until_setup_in_force(false));
}

#[test]
fn the_address_is_widened_only_for_a_listen_address_of_this_computer() {
    let local = ServerConfig::default();
    assert_eq!(local.bind_address(false).to_string(), "127.0.0.1:18200");
    assert_eq!(local.bind_address(true).to_string(), "0.0.0.0:18200");
    let six = cfg("listen = [::1]:5000");
    assert_eq!(six.bind_address(true).to_string(), "[::]:5000");
    assert_eq!(six.bind_address(false).to_string(), "[::1]:5000");
    let lan = cfg("listen = 192.168.0.5:7000");
    assert_eq!(lan.bind_address(true).to_string(), "192.168.0.5:7000");
}

#[test]
fn the_address_to_listen_on_has_to_be_an_address() {
    assert_eq!(cfg("listen = 0.0.0.0:1234").listen.port(), 1234);
    for bad in ["nope", "127.0.0.1", "127.0.0.1:99999", ":80", ""] {
        let why = refused(&format!("listen = {bad}"));
        assert!(why.contains("listen must be an address"), "{bad}: {why}");
    }
}

#[test]
fn the_folders_are_taken_as_they_are_and_an_empty_data_folder_is_refused() {
    assert_eq!(cfg("data_dir = a=b").data_dir.to_string_lossy(), "a=b");
    assert_eq!(
        cfg("data_dir = my data").data_dir.to_string_lossy(),
        "my data"
    );
    assert_eq!(refused("data_dir ="), "line 1: data_dir must not be empty");
    assert!(cfg("studio_dir =").studio_dir.is_none());
    assert!(cfg("studio_dir = s").studio_dir.is_some());
    assert!(cfg("updates_dir =").updates_dir.is_none());
    assert!(cfg("tls_cert_file =").tls_cert_file.is_none());
    assert!(cfg("tls_cert_file = c.pem").tls_cert_file.is_some());
}

#[test]
fn a_program_to_run_has_to_be_named_by_its_full_path() {
    for key in ["slice_phoenix_bin", "design_program"] {
        let why = refused(&format!("{key} = phoenix"));
        assert!(why.contains("full path"), "{key}: {why}");
        assert!(refused(&format!("{key} = ./phoenix")).contains("full path"));
    }
    let full = if cfg!(windows) {
        "C:\\bin\\phoenix.exe"
    } else {
        "/usr/bin/phoenix"
    };
    assert!(cfg(&format!("slice_phoenix_bin = {full}"))
        .slice_phoenix_bin
        .is_some());
    assert!(cfg(&format!("design_program = {full}"))
        .design_program
        .is_some());
    assert!(cfg("slice_phoenix_bin =").slice_phoenix_bin.is_none());
    assert!(cfg("design_program =").design_program.is_none());
}

#[test]
fn the_name_on_the_network_is_short_and_has_no_dots_or_control_characters() {
    assert_eq!(cfg("mdns_name = Bench server").mdns_name, "Bench server");
    assert_eq!(
        cfg(&format!("mdns_name = {}", "n".repeat(63)))
            .mdns_name
            .len(),
        63
    );
    assert_eq!(
        cfg(&format!("mdns_name = {}", "é".repeat(63)))
            .mdns_name
            .chars()
            .count(),
        63
    );
    for bad in ["n".repeat(64), "a.b".to_string(), "a\u{7}b".to_string()] {
        let why = refused(&format!("mdns_name = {bad}"));
        assert!(why.contains("mdns_name is a name"), "{why}");
    }
}

#[test]
fn the_extra_applications_are_a_list_of_names_between_commas() {
    assert_eq!(
        cfg("auth_extra_apps = a, b-c ,,d").auth_extra_apps,
        ["a", "b-c", "d"]
    );
    assert!(cfg("auth_extra_apps =").auth_extra_apps.is_empty());
    assert!(cfg("auth_extra_apps = ,,").auth_extra_apps.is_empty());
    let long = "x".repeat(33);
    for bad in ["Bad", "-x", "x-", "a b", "é", long.as_str()] {
        let why = refused(&format!("auth_extra_apps = ok, {bad}"));
        assert!(
            why.contains("is not the name of an application"),
            "{bad}: {why}"
        );
    }
}

#[test]
fn the_ping_of_the_events_has_to_be_shorter_than_the_time_to_idle() {
    assert_eq!(
        refused("events_ping_s = 100"),
        "events_idle_s must be longer than events_ping_s"
    );
    assert_eq!(
        refused("events_ping_s = 90"),
        "events_idle_s must be longer than events_ping_s"
    );
    let c = cfg("events_ping_s = 100\nevents_idle_s = 101");
    assert_eq!((c.events_ping_s, c.events_idle_s), (100, 101));
    assert_eq!(
        refused("events_idle_s = 30"),
        "events_idle_s must be longer than events_ping_s"
    );
    assert_eq!(cfg("events_ping_s = 89").events_ping_s, 89);
}

#[test]
fn the_waits_of_the_controller_are_the_ones_of_the_file() {
    let c = cfg("grbl_line_timeout_s = 7\ngrbl_homing_timeout_s = 200\ngrbl_jog_timeout_s = 9");
    let t = c.grbl_timeouts();
    assert_eq!(t.line.as_secs(), 7);
    assert_eq!(t.homing.as_secs(), 200);
    assert_eq!(t.jog.as_secs(), 9);
    assert_eq!(
        ServerConfig::default().grbl_timeouts().homing.as_secs(),
        120
    );
}

#[test]
fn a_file_made_of_any_bytes_of_a_small_alphabet_never_stops_the_reader() {
    let alphabet = b"abc =\n#;_09";
    for seed in 1..200u32 {
        let raw = lcg(seed, (seed as usize * 3) % 90, alphabet.len() as u32);
        let text: String = raw.iter().map(|b| alphabet[*b as usize] as char).collect();
        let _ = config::parse(&text);
    }
}

// ---------------------------------------------------------------- the switches of the security page

fn hot() -> Hot {
    Hot::from_config(&ServerConfig::default())
}

const FLAGS: [&str; 13] = [
    "url_token",
    "audit_denials",
    "design_run",
    "network_open",
    "update_allow_unsigned",
    "update_keep_logins",
    "mdns_announce",
    "compat_api",
    "compat_write",
    "slice_enabled",
    "bambu_lan_enabled",
    "persist_sessions",
    "password_deny_name",
];

fn range_of(key: &str, low: u64, high: u64) {
    let mut h = hot();
    h.set(key, &json!(low)).unwrap();
    assert_eq!(h.value(key), Some(json!(low)), "{key}");
    h.set(key, &json!(high)).unwrap();
    assert_eq!(h.value(key), Some(json!(high)), "{key}");
    let why = PolicyError::Bad(format!("{key} must be from {low} to {high}"));
    assert_eq!(h.set(key, &json!(high + 1)), Err(why));
    assert_eq!(h.value(key), Some(json!(high)), "a refusal changes nothing");
    if low > 0 {
        let why = PolicyError::Bad(format!("{key} must be from {low} to {high}"));
        assert_eq!(h.set(key, &json!(low - 1)), Err(why));
    }
}

#[test]
fn the_keys_of_the_security_page_are_all_different_and_the_restart_ones_are_among_them() {
    let mut seen = std::collections::HashSet::new();
    for key in KEYS {
        assert!(seen.insert(*key), "{key} is listed twice");
    }
    for key in RESTART_KEYS {
        assert!(KEYS.contains(key), "{key}");
    }
}

#[test]
fn a_value_that_is_set_back_the_way_it_is_shown_changes_nothing() {
    let mut h = hot();
    for key in KEYS {
        let shown = h.value(key).unwrap();
        h.set(key, &shown).unwrap();
        assert_eq!(h.value(key), Some(shown), "{key}");
    }
    assert_eq!(h, hot());
}

#[test]
fn a_switch_takes_a_true_or_a_false_and_nothing_else() {
    for key in FLAGS {
        let mut h = hot();
        for bad in [
            json!(1),
            json!(0),
            json!("true"),
            json!(null),
            json!([true]),
            json!(1.5),
        ] {
            assert_eq!(
                h.set(key, &bad),
                Err(PolicyError::Bad(format!("{key} must be true or false"))),
                "{key} {bad}"
            );
        }
        h.set(key, &json!(true)).unwrap();
        assert_eq!(h.value(key), Some(json!(true)));
        h.set(key, &json!(false)).unwrap();
        assert_eq!(h.value(key), Some(json!(false)));
    }
}

#[test]
fn a_number_that_is_not_a_whole_number_is_refused_whatever_its_form() {
    let mut h = hot();
    for bad in [
        json!(1.5),
        json!(-1),
        json!("5"),
        json!(null),
        json!(true),
        json!(5.0),
        json!(1e30),
    ] {
        assert_eq!(
            h.set("login_ip_limit", &bad),
            Err(PolicyError::Bad(
                "login_ip_limit must be a whole number".to_string()
            )),
            "{bad}"
        );
    }
    assert_eq!(h.value("login_ip_limit"), Some(json!(10)));
}

#[test]
fn the_biggest_number_a_json_can_hold_is_out_of_range_and_not_a_panic() {
    let mut h = hot();
    let why = h.set("auth_lockout_s", &json!(u64::MAX)).unwrap_err();
    assert_eq!(
        why,
        PolicyError::Bad("auth_lockout_s must be from 1 to 86400".to_string())
    );
}

#[test]
fn a_key_that_is_not_one_of_the_page_cannot_be_changed_there() {
    let mut h = hot();
    for key in ["session_hours", "data_dir", "", "URL_TOKEN", "listen"] {
        assert_eq!(
            h.set(key, &json!(1)),
            Err(PolicyError::Bad(format!(
                "{key} cannot be changed while the server runs"
            ))),
            "{key:?}"
        );
        assert_eq!(h.value(key), None);
    }
    assert_eq!(h, hot());
}

#[test]
fn the_login_limit_per_address_has_its_range() {
    range_of("login_ip_limit", 0, 1_000);
}

#[test]
fn the_failures_before_a_block_have_their_range() {
    range_of("auth_max_failures", 1, 100);
}

#[test]
fn the_seconds_of_a_block_have_their_range() {
    range_of("auth_lockout_s", 1, 86_400);
}

#[test]
fn the_hours_of_a_renewal_have_their_range() {
    range_of("auth_refresh_max_hours", 1, 8_760);
}

#[test]
fn the_length_of_a_password_has_its_range() {
    range_of("password_min_len", 8, 128);
}

#[test]
fn the_classes_of_a_password_have_their_range() {
    range_of("password_classes", 1, 4);
}

#[test]
fn a_value_is_safe_at_its_edge_and_not_one_step_past_it() {
    let mut h = hot();
    h.set("auth_max_failures", &json!(10)).unwrap();
    assert!(h.is_safe("auth_max_failures"));
    h.set("auth_max_failures", &json!(11)).unwrap();
    assert!(!h.is_safe("auth_max_failures"));
    h.set("auth_lockout_s", &json!(10)).unwrap();
    assert!(h.is_safe("auth_lockout_s"));
    h.set("auth_lockout_s", &json!(9)).unwrap();
    assert!(!h.is_safe("auth_lockout_s"));
    h.set("auth_refresh_max_hours", &json!(720)).unwrap();
    assert!(h.is_safe("auth_refresh_max_hours"));
    h.set("auth_refresh_max_hours", &json!(721)).unwrap();
    assert!(!h.is_safe("auth_refresh_max_hours"));
}

#[test]
fn the_length_the_classes_and_the_limit_of_logins_are_safe_from_their_floor() {
    let mut h = hot();
    h.set("password_classes", &json!(2)).unwrap();
    assert!(h.is_safe("password_classes"));
    h.set("password_classes", &json!(1)).unwrap();
    assert!(!h.is_safe("password_classes"));
    h.set("login_ip_limit", &json!(0)).unwrap();
    assert!(!h.is_safe("login_ip_limit"));
    h.set("login_ip_limit", &json!(1)).unwrap();
    assert!(h.is_safe("login_ip_limit"));
}

#[test]
fn a_key_the_page_does_not_know_is_not_unsafe() {
    assert!(hot().is_safe("whatever"));
}

#[test]
fn a_looser_value_is_refused_until_it_is_confirmed() {
    let dir = scratch("policy-weak");
    let cfg = ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    };
    let policy = Policy::open(&cfg, &dir).unwrap();
    assert_eq!(
        policy
            .change("compat_api", &json!(true), false)
            .unwrap_err(),
        PolicyError::WeakNeedsConfirm
    );
    assert!(!policy.get().compat_api);
    assert!(
        !dir.join("policy.json").exists(),
        "nothing is kept for a refusal"
    );
    let (before, after) = policy.change("compat_api", &json!(true), true).unwrap();
    assert!(!before.compat_api && after.compat_api);
    assert!(policy.get().compat_api);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stricter_value_needs_no_confirmation_and_is_kept_for_the_next_start() {
    let dir = scratch("policy-keep");
    let cfg = ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    };
    let policy = Policy::open(&cfg, &dir).unwrap();
    policy.change("password_classes", &json!(3), false).unwrap();
    policy.change("login_ip_limit", &json!(5), false).unwrap();
    let again = Policy::open(&cfg, &dir).unwrap().get();
    assert_eq!((again.password_classes, again.login_ip_limit), (3, 5));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_change_that_is_out_of_range_or_unknown_is_refused_before_it_is_confirmed() {
    let dir = scratch("policy-bad");
    let cfg = ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    };
    let policy = Policy::open(&cfg, &dir).unwrap();
    assert!(matches!(
        policy.change("login_ip_limit", &json!(1001), true),
        Err(PolicyError::Bad(_))
    ));
    assert!(matches!(
        policy.change("nope", &json!(1), true),
        Err(PolicyError::Bad(_))
    ));
    assert_eq!(policy.get(), Hot::from_config(&cfg));
    assert!(!dir.join("policy.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_of_changes_with_a_key_or_a_value_that_does_not_fit_is_read_as_far_as_it_fits() {
    let dir = scratch("policy-file");
    std::fs::write(
        dir.join("policy.json"),
        r#"{"login_ip_limit": 99999, "nope": 1, "compat_api": true, "password_min_len": "x", "auth_max_failures": 7}"#,
    )
    .unwrap();
    let cfg = ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    };
    let h = Policy::open(&cfg, &dir).unwrap().get();
    assert_eq!(h.login_ip_limit, 10);
    assert_eq!(h.password_min_len, 8);
    assert!(h.compat_api);
    assert_eq!(h.auth_max_failures, 7);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the feed of events

fn event(kind: &str) -> FeedEvent {
    FeedEvent::new(kind, Visibility::Everyone, json!({"k": kind}))
}

#[test]
fn an_event_that_has_not_been_published_has_no_number_and_no_target() {
    let e = event("job.state");
    assert_eq!(e.id, 0);
    assert!(e.machine.is_none() && e.project.is_none() && e.job.is_none() && e.app.is_none());
    assert_eq!(e.visibility, Visibility::Everyone);
}

#[test]
fn the_builders_of_an_event_fill_one_field_each() {
    let e = event("job.state")
        .machine("m1")
        .project("p1")
        .job("j1")
        .app("studio");
    assert_eq!(e.machine.as_deref(), Some("m1"));
    assert_eq!(e.project.as_deref(), Some("p1"));
    assert_eq!(e.job.as_deref(), Some("j1"));
    assert_eq!(e.app.as_deref(), Some("studio"));
}

#[test]
fn the_scope_an_event_asks_for_comes_from_the_family_of_its_kind() {
    let cases = [
        ("project.file", Some("projects:read")),
        ("project", Some("projects:read")),
        ("job.state", Some("machines:view")),
        ("machine.status", Some("machines:view")),
        ("notification.new", Some("machines:view")),
        ("catalog.changed", Some("catalog:read")),
        ("other.thing", None),
        ("projects.x", None),
        ("jobs", None),
        ("", None),
        (".job", None),
    ];
    for (kind, scope) in cases {
        assert_eq!(event(kind).scope(), scope, "{kind:?}");
    }
}

#[test]
fn the_text_of_an_event_is_a_json_with_every_field_even_the_empty_ones() {
    let e = event("job.state").machine("m1");
    let v: Value = serde_json::from_str(&e.text()).unwrap();
    assert_eq!(v["id"], 0);
    assert_eq!(v["kind"], "job.state");
    assert_eq!(v["machine"], "m1");
    assert!(v["project"].is_null() && v["job"].is_null() && v["app"].is_null());
    assert_eq!(v["data"]["k"], "job.state");
    assert!(
        v.get("visibility").is_none(),
        "who may hear it is not said to the one who hears"
    );
}

#[test]
fn the_text_of_an_event_keeps_quotes_line_breaks_and_letters_from_everywhere() {
    let e = FeedEvent::new(
        "job.note",
        Visibility::Everyone,
        json!({"text": "a \"b\"\nc é 日本"}),
    );
    let v: Value = serde_json::from_str(&e.text()).unwrap();
    assert_eq!(v["data"]["text"], "a \"b\"\nc é 日本");
    assert!(!e.text().contains('\n'));
}

#[test]
fn a_filter_with_nothing_in_it_lets_everything_through() {
    let f = Filter::default();
    assert!(f.passes(&event("job.state")));
    assert!(f.passes(&event("").machine("m").job("j")));
}

#[test]
fn a_kind_without_a_dot_matches_the_family_and_not_a_longer_name() {
    let f = Filter {
        kinds: vec!["job".into()],
        ..Filter::default()
    };
    assert!(f.passes(&event("job")));
    assert!(f.passes(&event("job.state")));
    assert!(f.passes(&event("job.state.deep")));
    assert!(!f.passes(&event("jobs.state")));
    assert!(!f.passes(&event("jobx")));
    assert!(!f.passes(&event("machine.job")));
}

#[test]
fn a_kind_with_a_dot_matches_itself_and_what_goes_under_it() {
    let f = Filter {
        kinds: vec!["job.state".into()],
        ..Filter::default()
    };
    assert!(f.passes(&event("job.state")));
    assert!(f.passes(&event("job.state.x")));
    assert!(!f.passes(&event("job.progress")));
    assert!(!f.passes(&event("job")));
}

#[test]
fn several_kinds_are_alternatives_and_a_field_given_has_to_match() {
    let f = Filter {
        kinds: vec!["job".into(), "project".into()],
        machine: Some("m1".into()),
        ..Filter::default()
    };
    assert!(f.passes(&event("job.a").machine("m1")));
    assert!(f.passes(&event("project.b").machine("m1")));
    assert!(!f.passes(&event("machine.c").machine("m1")));
    assert!(!f.passes(&event("job.a").machine("m2")));
    assert!(
        !f.passes(&event("job.a")),
        "an event with no machine does not match a machine"
    );
}

#[test]
fn every_field_of_a_filter_is_checked_on_its_own() {
    let e = event("job.x").machine("m").project("p").job("j").app("a");
    for f in [
        Filter {
            machine: Some("m".into()),
            ..Filter::default()
        },
        Filter {
            project: Some("p".into()),
            ..Filter::default()
        },
        Filter {
            job: Some("j".into()),
            ..Filter::default()
        },
        Filter {
            app: Some("a".into()),
            ..Filter::default()
        },
    ] {
        assert!(f.passes(&e));
    }
    for f in [
        Filter {
            machine: Some("M".into()),
            ..Filter::default()
        },
        Filter {
            project: Some("".into()),
            ..Filter::default()
        },
        Filter {
            job: Some("j ".into()),
            ..Filter::default()
        },
        Filter {
            app: Some("b".into()),
            ..Filter::default()
        },
    ] {
        assert!(!f.passes(&e));
    }
}

#[test]
fn the_events_get_the_numbers_one_two_three() {
    let feed = Feed::new(256, 32);
    assert_eq!(feed.last_id(), 0);
    assert_eq!(feed.publish(event("a.b")), 1);
    assert_eq!(feed.publish(event("a.b")), 2);
    assert_eq!(feed.publish(event("a.b")), 3);
    assert_eq!(feed.last_id(), 3);
}

#[test]
fn a_client_that_lost_its_connection_gets_what_came_after_its_last_number() {
    let feed = Feed::new(256, 32);
    for _ in 0..5 {
        feed.publish(event("job.state"));
    }
    let ids = |last: u64| feed.since(last).iter().map(|e| e.id).collect::<Vec<u64>>();
    assert_eq!(ids(0), [1, 2, 3, 4, 5]);
    assert_eq!(ids(3), [4, 5]);
    assert_eq!(ids(5), Vec::<u64>::new());
    assert_eq!(ids(u64::MAX), Vec::<u64>::new());
}

#[test]
fn only_the_last_events_are_kept_and_the_room_is_never_below_sixteen() {
    let feed = Feed::new(4, 32);
    for _ in 0..40 {
        feed.publish(event("job.state"));
    }
    let kept = feed.since(0);
    assert_eq!(kept.len(), 16);
    assert_eq!(kept[0].id, 25);
    assert_eq!(kept[15].id, 40);
    assert_eq!(feed.last_id(), 40);
    let big = Feed::new(100, 32);
    for _ in 0..150 {
        big.publish(event("job.state"));
    }
    assert_eq!(big.since(0).len(), 100);
}

#[test]
fn a_client_that_listens_gets_the_event_with_its_number() {
    let feed = Feed::new(16, 32);
    let mut rx = feed.subscribe();
    feed.publish(event("job.state").job("j9"));
    let got = rx.try_recv().unwrap();
    assert_eq!((got.id, got.job.as_deref()), (1, Some("j9")));
    assert!(rx.try_recv().is_err());
}

#[test]
fn publishing_with_nobody_listening_is_not_a_problem() {
    let feed = Feed::new(16, 32);
    assert_eq!(feed.publish(event("a.b")), 1);
}

#[test]
fn the_places_for_clients_are_counted_and_given_back() {
    let feed = Feed::new(16, 2);
    let a = feed.join().unwrap();
    let b = feed.join().unwrap();
    assert_eq!(feed.clients(), 2);
    assert!(feed.join().is_none());
    assert_eq!(feed.clients(), 2, "a refusal does not take a place");
    drop(a);
    assert_eq!(feed.clients(), 1);
    let c = feed.join();
    assert!(c.is_some());
    assert_eq!(feed.clients(), 2);
    drop(b);
    drop(c);
    assert_eq!(feed.clients(), 0);
}

#[test]
fn a_feed_that_takes_no_clients_takes_none() {
    let feed = Feed::new(16, 0);
    assert!(feed.join().is_none());
    assert!(feed.join().is_none());
    assert_eq!(feed.clients(), 0);
}

// ---------------------------------------------------------------- the shared catalogs

fn tools() -> &'static catalogs::Def {
    catalogs::def_of("tools").unwrap()
}

fn items_of(def: &catalogs::Def, text: &str) -> Result<Vec<Value>, CError> {
    catalogs::parse_upload(def, text.as_bytes(), false, 1_000).map(|(items, _)| items)
}

fn csv_items(text: &str) -> Result<Vec<Value>, CError> {
    catalogs::parse_upload(tools(), text.as_bytes(), true, 1_000).map(|(items, _)| items)
}

fn bad_text(result: Result<Vec<Value>, CError>) -> String {
    match result {
        Err(CError::Bad(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn the_six_catalogs_are_known_by_name_and_only_the_materials_are_built_in() {
    let names: Vec<&str> = catalogs::DEFS.iter().map(|d| d.name).collect();
    assert_eq!(
        names,
        [
            "materials",
            "tools",
            "machine-profiles",
            "footprints",
            "symbols",
            "components"
        ]
    );
    for d in catalogs::DEFS {
        assert_eq!(d.builtin, d.name == "materials", "{}", d.name);
        assert_eq!(catalogs::def_of(d.name).unwrap().name, d.name);
    }
    assert!(catalogs::def_of("nope").is_none());
    assert!(catalogs::def_of("").is_none());
    assert!(catalogs::def_of("TOOLS").is_none());
}

#[test]
fn every_refusal_of_a_catalog_has_its_code_status_and_detail() {
    let cases = [
        (CError::Unknown, "unknown_catalog", 404, ""),
        (CError::UnknownItem, "unknown_item", 404, ""),
        (CError::Bad("why".into()), "bad_catalog", 422, "why"),
        (CError::ReadOnly, "catalog_readonly", 409, ""),
        (CError::TooBig, "too_big", 422, ""),
        (CError::Storage, "storage", 500, ""),
    ];
    for (e, code, status, detail) in cases {
        assert_eq!(
            (e.code(), e.status(), e.detail().as_str()),
            (code, status, detail)
        );
    }
}

#[test]
fn a_list_of_items_or_an_object_with_the_list_in_it_is_taken() {
    let by_list =
        catalogs::parse_upload(tools(), br#"[{"id":"a"},{"id":"b"}]"#, false, 10).unwrap();
    assert_eq!((by_list.0.len(), by_list.1), (2, "json"));
    assert_eq!(
        items_of(tools(), r#"{"tools":[{"id":"a"}]}"#)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        items_of(tools(), r#"{"items":[{"id":"a"}]}"#)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(items_of(tools(), "[]").unwrap().len(), 0);
}

#[test]
fn the_first_key_that_holds_a_list_is_the_one_that_counts() {
    let first = items_of(
        tools(),
        r#"{"tools":[{"id":"a"}],"items":[{"id":"b"},{"id":"c"}]}"#,
    );
    assert_eq!(first.unwrap().len(), 1);
    let skipped = items_of(tools(), r#"{"tools":5,"items":[{"id":"b"},{"id":"c"}]}"#);
    assert_eq!(skipped.unwrap().len(), 2);
}

#[test]
fn a_document_that_is_not_a_list_says_which_keys_would_do() {
    for text in [r#"{"other":[]}"#, "5", r#""text""#, "null", "true", "{}"] {
        let why = bad_text(items_of(tools(), text));
        assert_eq!(
            why, "give a list of items, or an object with one of tools, items",
            "{text}"
        );
    }
}

#[test]
fn a_document_that_is_not_json_or_not_text_is_refused() {
    assert!(bad_text(items_of(tools(), "{")).starts_with("JSON: "));
    assert!(bad_text(items_of(tools(), "")).starts_with("JSON: "));
    let why = catalogs::parse_upload(tools(), &[0xFF, 0xFE], false, 10).unwrap_err();
    assert_eq!(why, CError::Bad("the text is not UTF-8".into()));
}

#[test]
fn an_item_has_to_be_an_object_with_a_name_that_is_text_and_not_blank() {
    assert_eq!(bad_text(items_of(tools(), "[1]")), "item 1: not an object");
    assert_eq!(
        bad_text(items_of(tools(), r#"[{"id":"a"},"x"]"#)),
        "item 2: not an object"
    );
    let needs = "item 1: it needs a id (text, up to 200 characters)";
    for text in [
        r#"[{}]"#,
        r#"[{"id":5}]"#,
        r#"[{"id":null}]"#,
        r#"[{"id":""}]"#,
        r#"[{"id":"   "}]"#,
        r#"[{"name":"a"}]"#,
    ] {
        assert_eq!(bad_text(items_of(tools(), text)), needs, "{text}");
    }
}

#[test]
fn the_name_of_an_item_has_up_to_two_hundred_characters_not_bytes() {
    let ok = format!(r#"[{{"id":"{}"}}]"#, "é".repeat(200));
    assert_eq!(items_of(tools(), &ok).unwrap().len(), 1);
    let long = format!(r#"[{{"id":"{}"}}]"#, "é".repeat(201));
    assert!(bad_text(items_of(tools(), &long)).starts_with("item 1: it needs a id"));
}

#[test]
fn two_items_with_the_same_name_are_refused_and_two_that_differ_in_case_are_not() {
    assert_eq!(
        bad_text(items_of(tools(), r#"[{"id":"a"},{"id":"a"}]"#)),
        "item 2: the id \"a\" is repeated"
    );
    assert_eq!(
        items_of(tools(), r#"[{"id":"a"},{"id":"A"}]"#)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        items_of(tools(), r#"[{"id":"a"},{"id":"a "}]"#)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn the_field_that_names_an_item_depends_on_the_catalog() {
    let footprints = catalogs::def_of("footprints").unwrap();
    assert_eq!(items_of(footprints, r#"[{"name":"f1"}]"#).unwrap().len(), 1);
    assert!(bad_text(items_of(footprints, r#"[{"id":"f1"}]"#)).contains("needs a name"));
}

#[test]
fn more_items_than_the_limit_are_refused_and_the_limit_itself_is_not() {
    let two = br#"[{"id":"a"},{"id":"b"}]"#;
    assert_eq!(
        catalogs::parse_upload(tools(), two, false, 2)
            .unwrap()
            .0
            .len(),
        2
    );
    let why = catalogs::parse_upload(tools(), two, false, 1).unwrap_err();
    assert_eq!(
        why,
        CError::Bad("more than 1 items (catalog_max_items)".into())
    );
}

#[test]
fn the_components_are_json_that_declares_its_format() {
    let parts = catalogs::def_of("components").unwrap();
    let good = r#"{"format":"phoenix-parts/1","parts":[{"id":"p1"}]}"#;
    assert_eq!(items_of(parts, good).unwrap().len(), 1);
    for bad in [
        r#"{"parts":[{"id":"p1"}]}"#,
        r#"{"format":"x","parts":[]}"#,
        r#"[{"id":"p1"}]"#,
    ] {
        assert!(
            bad_text(items_of(parts, bad)).contains("phoenix-parts/1"),
            "{bad}"
        );
    }
    let csv = catalogs::parse_upload(parts, b"id\np1\n", true, 10).unwrap_err();
    assert!(matches!(csv, CError::Bad(m) if m.contains("kept as JSON")));
}

#[test]
fn a_csv_becomes_items_with_text_values_and_leaves_out_the_empty_ones() {
    let items = csv_items("id,name\nt1,Drill\nt2,\n").unwrap();
    assert_eq!(
        items,
        [json!({"id":"t1","name":"Drill"}), json!({"id":"t2"})]
    );
    let (_, source) = catalogs::parse_upload(tools(), b"id\nt1\n", true, 10).unwrap();
    assert_eq!(source, "csv");
}

#[test]
fn a_csv_may_use_semicolons_a_byte_order_mark_and_any_line_break() {
    let want = [json!({"id":"t1","name":"A"}), json!({"id":"t2","name":"B"})];
    assert_eq!(csv_items("id;name\nt1;A\nt2;B").unwrap(), want);
    assert_eq!(csv_items("\u{feff}id,name\nt1,A\nt2,B\n").unwrap(), want);
    assert_eq!(csv_items("id,name\r\nt1,A\r\nt2,B\r\n").unwrap(), want);
    assert_eq!(csv_items("id,name\rt1,A\rt2,B").unwrap(), want);
}

#[test]
fn a_csv_value_in_quotes_may_hold_the_separator_a_quote_and_a_line_break() {
    let items =
        csv_items("id,name\nt1,\"a,b\"\nt2,\"say \"\"hi\"\"\"\nt3,\"two\nlines\"\n").unwrap();
    assert_eq!(items[0]["name"], "a,b");
    assert_eq!(items[1]["name"], "say \"hi\"");
    assert_eq!(items[2]["name"], "two\nlines");
}

#[test]
fn blank_lines_and_blanks_around_the_values_do_not_count() {
    let items = csv_items("id\n\n  t1  \n\nt2\n").unwrap();
    assert_eq!(items, [json!({"id":"t1"}), json!({"id":"t2"})]);
    assert_eq!(csv_items("id,name\n").unwrap().len(), 0);
}

#[test]
fn a_csv_with_a_wrong_header_or_row_is_refused_and_the_row_is_named() {
    assert_eq!(bad_text(csv_items("")), "the CSV has no header line");
    assert_eq!(
        bad_text(csv_items("id,id\n1,2")),
        "the header of the CSV has an empty or repeated column"
    );
    assert_eq!(
        bad_text(csv_items("id,\n1,2")),
        "the header of the CSV has an empty or repeated column"
    );
    assert_eq!(
        bad_text(csv_items("id,name\nt1\n")),
        "row 2: 1 values, the header has 2"
    );
    assert_eq!(
        bad_text(csv_items("id,name\nt1,a\nt2,b,c\n")),
        "row 3: 3 values, the header has 2"
    );
    assert_eq!(
        bad_text(csv_items("id\n\"abc")),
        "a quoted value is not closed"
    );
}

#[test]
fn a_csv_without_the_name_of_its_items_is_refused_like_a_json() {
    assert!(bad_text(csv_items("name\nx\n")).contains("it needs a id"));
    assert!(bad_text(csv_items("id\nt1\nt1\n")).contains("is repeated"));
}

#[test]
fn any_text_of_a_small_alphabet_is_read_as_csv_or_json_without_a_panic() {
    let alphabet = b"id,;\n\r\"{}[]:a1 ";
    for seed in 1..300u32 {
        let raw = lcg(seed, (seed as usize * 3) % 80, alphabet.len() as u32);
        let bytes: Vec<u8> = raw.iter().map(|b| alphabet[*b as usize]).collect();
        let _ = catalogs::parse_upload(tools(), &bytes, true, 50);
        let _ = catalogs::parse_upload(tools(), &bytes, false, 50);
    }
}

fn book(name: &str) -> (CBook, ServerConfig, std::path::PathBuf) {
    let dir = scratch(name);
    let config = ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    };
    (CBook::open(&config).unwrap(), config, dir)
}

const TOOLS: &str = r#"[
  {"id":"d08","kind":"drill","dia_mm":0.8,"tags":["fr4","hss"],"vendor":{"name":"Acme"}},
  {"id":"d10","kind":"Drill","dia_mm":1.0,"tags":["alu"]},
  {"id":"m20","kind":"mill","dia_mm":2.0,"tags":["fr4"]},
  {"id":"m05","kind":"mill","dia_mm":10}
]"#;

fn found(book: &CBook, name: &str, query: &Query) -> (usize, Vec<String>) {
    let (def, ds) = book.dataset(name).ok().unwrap();
    let (total, page) = catalogs::search(ds, query);
    let ids = page
        .iter()
        .map(|i| i[def.id_field].as_str().unwrap().to_string())
        .collect();
    (total, ids)
}

fn page(limit: usize) -> Query {
    Query {
        limit,
        ..Query::default()
    }
}

fn loaded(name: &str) -> (CBook, std::path::PathBuf) {
    let (mut b, _, dir) = book(name);
    b.replace("tools", TOOLS.as_bytes(), false, "t", 5).unwrap();
    (b, dir)
}

#[test]
fn a_new_book_lists_the_six_catalogs_and_the_tools_are_empty() {
    let (b, _, dir) = book("cat-new");
    let list = b.list();
    assert_eq!(list.len(), 6);
    assert_eq!(list[1]["name"], "tools");
    assert_eq!(list[1]["items"], 0);
    assert_eq!(list[1]["version"], 0);
    assert!(list[0]["items"].as_u64().unwrap() > 0);
    assert_eq!(b.page_max, 200);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_catalog_that_is_not_there_is_unknown_and_the_built_in_one_is_read_only() {
    let (mut b, _, dir) = book("cat-refuse");
    assert!(matches!(b.dataset("nope"), Err(CError::Unknown)));
    assert_eq!(
        b.replace("nope", b"[]", false, "t", 1).unwrap_err(),
        CError::Unknown
    );
    assert_eq!(
        b.replace("materials", b"[]", false, "t", 1).unwrap_err(),
        CError::ReadOnly
    );
    assert_eq!(b.clear("materials", "t", 1).unwrap_err(), CError::ReadOnly);
    assert_eq!(b.clear("nope", "t", 1).unwrap_err(), CError::Unknown);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_upload_is_a_new_version_with_its_hash_and_tag_and_the_same_text_gives_the_same_hash() {
    let (mut b, _, dir) = book("cat-version");
    let one = b
        .replace("tools", TOOLS.as_bytes(), false, "ann", 5)
        .unwrap();
    assert_eq!(
        (one["version"].as_u64(), one["items"].as_u64()),
        (Some(1), Some(4))
    );
    assert_eq!(
        (
            one["by"].as_str(),
            one["updated"].as_u64(),
            one["source"].as_str()
        ),
        (Some("ann"), Some(5), Some("json"))
    );
    let etag = one["etag"].as_str().unwrap();
    assert!(
        etag.starts_with("\"tools-") && etag.ends_with('"') && etag.len() == 24,
        "{etag}"
    );
    let two = b
        .replace("tools", TOOLS.as_bytes(), false, "bob", 9)
        .unwrap();
    assert_eq!(two["version"].as_u64(), Some(2));
    assert_eq!(one["sha256"], two["sha256"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_upload_that_is_too_big_is_refused_before_it_is_read() {
    let dir = scratch("cat-big");
    let config = ServerConfig {
        data_dir: dir.clone(),
        catalog_max_mb: 1,
        ..ServerConfig::default()
    };
    let mut b = CBook::open(&config).unwrap();
    let big = vec![b' '; 1024 * 1024 + 1];
    assert_eq!(
        b.replace("tools", &big, false, "t", 1).unwrap_err(),
        CError::TooBig
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refused_upload_leaves_the_catalog_as_it_was() {
    let (mut b, dir) = loaded("cat-keep");
    assert!(b
        .replace("tools", br#"[{"id":"a"},{"id":"a"}]"#, false, "t", 6)
        .is_err());
    let list = b.list();
    assert_eq!(
        (list[1]["version"].as_u64(), list[1]["items"].as_u64()),
        (Some(1), Some(4))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_was_uploaded_is_there_after_the_book_is_opened_again() {
    let (mut b, config, dir) = book("cat-reopen");
    b.replace("tools", TOOLS.as_bytes(), false, "t", 5).unwrap();
    let again = CBook::open(&config).unwrap();
    let (def, ds) = again.dataset("tools").ok().unwrap();
    assert_eq!((ds.version, ds.items.len()), (1, 4));
    assert!(ds.get("d08").is_some());
    assert!(ds.get("zzz").is_none());
    assert_eq!(def.name, "tools");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_catalog_that_is_cleared_is_a_new_version_with_no_items() {
    let (mut b, dir) = loaded("cat-clear");
    let s = b.clear("tools", "t", 7).unwrap();
    assert_eq!(
        (s["version"].as_u64(), s["items"].as_u64()),
        (Some(2), Some(0))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_search_with_no_words_gives_every_item_in_the_order_they_came() {
    let (b, dir) = loaded("cat-all");
    assert_eq!(
        found(&b, "tools", &page(100)),
        (
            4,
            vec!["d08".to_string(), "d10".into(), "m20".into(), "m05".into()]
        )
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_query_with_a_limit_of_zero_gives_an_empty_page_and_still_counts() {
    let (b, dir) = loaded("cat-limit0");
    assert_eq!(found(&b, "tools", &page(0)), (4, vec![]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_page_starts_after_the_offset_and_goes_as_far_as_the_limit() {
    let (b, dir) = loaded("cat-page");
    let q = |offset, limit| Query {
        offset,
        limit,
        ..Query::default()
    };
    assert_eq!(
        found(&b, "tools", &q(1, 2)),
        (4, vec!["d10".to_string(), "m20".into()])
    );
    assert_eq!(found(&b, "tools", &q(3, 10)), (4, vec!["m05".to_string()]));
    assert_eq!(found(&b, "tools", &q(4, 10)), (4, vec![]));
    assert_eq!(found(&b, "tools", &q(usize::MAX, 10)), (4, vec![]));
    assert_eq!(found(&b, "tools", &q(0, usize::MAX)).1.len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_words_of_a_search_are_found_in_any_letter_case_at_any_depth() {
    let (b, dir) = loaded("cat-words");
    let q = |text: &str| Query {
        q: text.into(),
        limit: 100,
        ..Query::default()
    };
    assert_eq!(found(&b, "tools", &q("DRILL")).1, ["d08", "d10"]);
    assert_eq!(found(&b, "tools", &q("  mill ")).1, ["m20", "m05"]);
    assert_eq!(found(&b, "tools", &q("acme")).1, ["d08"]);
    assert_eq!(found(&b, "tools", &q("fr4")).1, ["d08", "m20"]);
    assert_eq!(found(&b, "tools", &q("10")).1, ["d10", "m05"]);
    assert_eq!(found(&b, "tools", &q("zzz")), (0, vec![]));
    assert_eq!(found(&b, "tools", &q("   ")).0, 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_filter_is_equal_or_contains_and_reads_lists_and_nested_fields() {
    let (b, dir) = loaded("cat-filter");
    let f = |field: &str, want: &str| Query {
        filters: vec![(field.into(), want.into())],
        limit: 100,
        ..Query::default()
    };
    assert_eq!(found(&b, "tools", &f("kind", "DRILL")).1, ["d08", "d10"]);
    assert_eq!(found(&b, "tools", &f("kind", "~ill")).0, 4);
    assert_eq!(found(&b, "tools", &f("kind", "dril")).0, 0);
    assert_eq!(found(&b, "tools", &f("tags", "fr4")).1, ["d08", "m20"]);
    assert_eq!(found(&b, "tools", &f("vendor.name", "acme")).1, ["d08"]);
    assert_eq!(found(&b, "tools", &f("vendor.name", "~")).1, ["d08"]);
    assert_eq!(found(&b, "tools", &f("dia_mm", "1.0")).1, ["d10"]);
    assert_eq!(found(&b, "tools", &f("dia_mm", "10")).1, ["m05"]);
    assert_eq!(found(&b, "tools", &f("nothing", "x")).0, 0);
    assert_eq!(found(&b, "tools", &f("vendor.name.deeper", "x")).0, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_filters_have_to_hold_together() {
    let (b, dir) = loaded("cat-filter2");
    let q = Query {
        filters: vec![
            ("kind".into(), "mill".into()),
            ("tags".into(), "fr4".into()),
        ],
        limit: 100,
        ..Query::default()
    };
    assert_eq!(found(&b, "tools", &q).1, ["m20"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn numbers_are_ordered_as_numbers_and_the_order_can_be_turned() {
    let (b, dir) = loaded("cat-sort");
    let q = |descending| Query {
        sort: Some("dia_mm".into()),
        descending,
        limit: 100,
        ..Query::default()
    };
    assert_eq!(
        found(&b, "tools", &q(false)).1,
        ["d08", "d10", "m20", "m05"]
    );
    assert_eq!(found(&b, "tools", &q(true)).1, ["m05", "m20", "d10", "d08"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn text_is_ordered_without_regard_to_the_case_and_equal_ones_keep_their_place() {
    let (b, dir) = loaded("cat-sort-text");
    let q = Query {
        sort: Some("kind".into()),
        limit: 100,
        ..Query::default()
    };
    assert_eq!(found(&b, "tools", &q).1, ["d08", "d10", "m20", "m05"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_item_without_the_field_to_sort_by_goes_last() {
    let (b, dir) = loaded("cat-sort-missing");
    let q = Query {
        sort: Some("vendor.name".into()),
        limit: 100,
        ..Query::default()
    };
    assert_eq!(found(&b, "tools", &q).1, ["d08", "d10", "m20", "m05"]);
    let (b2, dir2) = loaded("cat-sort-missing2");
    let q2 = Query {
        sort: Some("tags".into()),
        limit: 100,
        ..Query::default()
    };
    assert_eq!(found(&b2, "tools", &q2).0, 4);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn numbers_come_before_text_when_a_field_holds_both() {
    // "2" comes after "1x" as text and before "10" as a number: without a rule of its own the order has a cycle
    let (mut b, _, dir) = book("cat-sort-mixed");
    let text = r#"[{"id":"a","n":"1x"},{"id":"b","n":2},{"id":"c","n":10}]"#;
    b.replace("tools", text.as_bytes(), false, "t", 1).unwrap();
    let q = Query {
        sort: Some("n".into()),
        limit: 100,
        ..Query::default()
    };
    assert_eq!(found(&b, "tools", &q).1, ["b", "c", "a"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_long_list_with_numbers_and_text_in_the_same_field_is_ordered_without_a_panic() {
    let (mut b, _, dir) = book("cat-sort-many");
    let mut rows = Vec::new();
    for (i, r) in lcg(7, 80, 50).iter().enumerate() {
        let n = if i.is_multiple_of(2) {
            json!(r)
        } else {
            json!(format!("{r}x"))
        };
        rows.push(json!({"id": format!("i{i}"), "n": n}));
    }
    let text = serde_json::to_string(&rows).unwrap();
    b.replace("tools", text.as_bytes(), false, "t", 1).unwrap();
    let (_, ds) = b.dataset("tools").ok().unwrap();
    let q = Query {
        sort: Some("n".into()),
        limit: 1000,
        ..Query::default()
    };
    let (total, page) = catalogs::search(ds, &q);
    assert_eq!(total, 80);
    let numbers = page.iter().take_while(|i| i["n"].is_number()).count();
    assert_eq!(numbers, 40);
    let nums: Vec<f64> = page[..numbers]
        .iter()
        .map(|i| i["n"].as_f64().unwrap())
        .collect();
    assert!(nums.windows(2).all(|w| w[0] <= w[1]), "{nums:?}");
    let words: Vec<String> = page[numbers..]
        .iter()
        .map(|i| i["n"].as_str().unwrap().to_lowercase())
        .collect();
    assert!(words.windows(2).all(|w| w[0] <= w[1]), "{words:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- artifacts, uploads, names and versions

fn art(extra: Value) -> Result<(), String> {
    let mut base = json!({"name": "board.phoenix", "format": "application/x-phoenixproj"});
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    let a: NewArtifact = serde_json::from_value(base).unwrap();
    artifacts::check_new(&a)
}

#[test]
fn the_hash_of_nothing_and_of_abc_are_the_known_ones() {
    assert_eq!(
        artifacts::sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        artifacts::sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_hash_is_sixty_four_digits_in_lower_case_and_nothing_else() {
    let good = artifacts::sha256_hex(b"x");
    assert!(artifacts::is_sha256(&good));
    assert!(!artifacts::is_sha256(&good.to_uppercase()));
    assert!(!artifacts::is_sha256(&good[1..]));
    assert!(!artifacts::is_sha256(&format!("{good}0")));
    assert!(!artifacts::is_sha256(&format!("g{}", &good[1..])));
    assert!(!artifacts::is_sha256(""));
    assert!(!artifacts::is_sha256(&"é".repeat(32)));
}

#[test]
fn a_description_with_a_name_and_a_format_is_enough() {
    assert_eq!(art(json!({})), Ok(()));
}

#[test]
fn the_name_of_an_artifact_is_up_to_one_hundred_and_twenty_characters_with_no_slash() {
    let why = "name is from 1 to 120 characters, without control characters or slashes";
    assert_eq!(art(json!({"name": "é".repeat(120)})), Ok(()));
    assert_eq!(art(json!({"name": "é".repeat(121)})), Err(why.into()));
    for bad in ["", "a/b", "a\\b", "a\nb", "a\u{0}b", "a\u{7f}b"] {
        assert_eq!(art(json!({"name": bad})), Err(why.into()), "{bad:?}");
    }
    assert_eq!(art(json!({"name": " "})), Ok(()));
}

#[test]
fn the_format_of_an_artifact_is_a_lower_case_media_type() {
    let why = "format is a media type in lower case, such as application/x-phoenixproj";
    for good in [
        "a/b",
        "application/x-phoenixproj",
        "text/plain+x",
        "a.b/c_d-e",
    ] {
        assert_eq!(art(json!({"format": good})), Ok(()), "{good}");
    }
    let long = "a".repeat(101);
    for bad in ["", "Text/Plain", "a b", "a;b", "é", long.as_str()] {
        assert_eq!(art(json!({"format": bad})), Err(why.into()), "{bad:?}");
    }
    assert_eq!(art(json!({"format": "a".repeat(100)})), Ok(()));
}

#[test]
fn the_version_of_the_schema_and_the_type_of_the_result_have_their_rules() {
    assert_eq!(art(json!({"schema_version": "1".repeat(40)})), Ok(()));
    assert_eq!(
        art(json!({"schema_version": "1".repeat(41)})),
        Err("schema_version is at most 40 characters".into())
    );
    for t in ["T1", "T4", "T7"] {
        assert_eq!(art(json!({"result_type": t})), Ok(()), "{t}");
    }
    for t in ["T0", "T8", "t1", "", "T11"] {
        assert_eq!(
            art(json!({"result_type": t})),
            Err("result_type is from T1 to T7".into()),
            "{t}"
        );
    }
}

#[test]
fn the_params_of_an_artifact_are_an_object_of_at_most_sixty_four_kilobytes() {
    let why = "params is an object of at most 64 KB";
    assert_eq!(art(json!({"params": {"a": 1}})), Ok(()));
    for bad in [json!([1]), json!("x"), json!(5)] {
        assert_eq!(art(json!({"params": bad})), Err(why.into()));
    }
    assert_eq!(art(json!({"params": {"a": "x".repeat(60_000)}})), Ok(()));
    assert_eq!(
        art(json!({"params": {"a": "x".repeat(70_000)}})),
        Err(why.into())
    );
}

#[test]
fn the_inputs_of_an_artifact_are_at_most_two_hundred_and_fifty_six_and_each_has_a_real_id_and_hash()
{
    let id = ids::make(Kind::Artifact, 1_700_000_000_000, [3; 10]);
    let sha = artifacts::sha256_hex(b"input");
    let good = json!({"artifact_id": id, "blob_sha256": sha});
    assert_eq!(art(json!({"inputs": [good.clone()]})), Ok(()));
    let many: Vec<Value> = (0..256).map(|_| good.clone()).collect();
    assert_eq!(art(json!({"inputs": many})), Ok(()));
    let too_many: Vec<Value> = (0..257).map(|_| good.clone()).collect();
    assert_eq!(
        art(json!({"inputs": too_many})),
        Err("at most 256 inputs".into())
    );
    let why = "an input is {artifact_id, blob_sha256}";
    for bad in [
        json!({"artifact_id": "a-nope", "blob_sha256": sha}),
        json!({"artifact_id": id, "blob_sha256": "abc"}),
        json!({"artifact_id": ids::make(Kind::Task, 1, [0; 10]), "blob_sha256": sha}),
    ] {
        assert_eq!(art(json!({"inputs": [bad]})), Err(why.into()));
    }
}

#[test]
fn the_project_an_artifact_came_from_has_a_name_of_at_most_eighty_characters() {
    assert_eq!(art(json!({"origin": {"project": "p".repeat(80)}})), Ok(()));
    assert_eq!(
        art(json!({"origin": {"project": "p".repeat(81)}})),
        Err("origin.project is too long".into())
    );
}

fn stretches(list: &[(u64, u64)]) -> Vec<(u64, u64)> {
    list.to_vec()
}

#[test]
fn stretches_that_touch_or_overlap_become_one() {
    let mut r = Vec::new();
    artifacts::add_range(&mut r, 0, 10);
    assert_eq!(r, stretches(&[(0, 10)]));
    artifacts::add_range(&mut r, 10, 20);
    assert_eq!(r, stretches(&[(0, 20)]));
    artifacts::add_range(&mut r, 5, 15);
    assert_eq!(r, stretches(&[(0, 20)]));
    artifacts::add_range(&mut r, 30, 40);
    assert_eq!(r, stretches(&[(0, 20), (30, 40)]));
    artifacts::add_range(&mut r, 21, 29);
    assert_eq!(r, stretches(&[(0, 20), (21, 29), (30, 40)]));
    artifacts::add_range(&mut r, 20, 30);
    assert_eq!(r, stretches(&[(0, 40)]));
}

#[test]
fn a_stretch_that_covers_the_others_swallows_them_and_the_same_stretch_twice_changes_nothing() {
    let mut r = vec![(2, 4), (6, 8), (10, 12)];
    artifacts::add_range(&mut r, 0, 100);
    assert_eq!(r, stretches(&[(0, 100)]));
    artifacts::add_range(&mut r, 0, 100);
    artifacts::add_range(&mut r, 10, 20);
    assert_eq!(r, stretches(&[(0, 100)]));
}

#[test]
fn a_stretch_up_to_the_biggest_number_is_kept_whole() {
    let mut r = Vec::new();
    artifacts::add_range(&mut r, 0, u64::MAX);
    artifacts::add_range(&mut r, 5, 6);
    artifacts::add_range(&mut r, u64::MAX - 1, u64::MAX);
    assert_eq!(r, stretches(&[(0, u64::MAX)]));
}

#[test]
fn the_parts_of_an_upload_make_the_same_stretches_in_any_order() {
    let parts: Vec<(u64, u64)> = lcg(11, 60, 200)
        .chunks(2)
        .map(|c| (c[0] as u64, c[0] as u64 + c[1] as u64 % 20 + 1))
        .collect();
    let mut forward = Vec::new();
    for (s, e) in &parts {
        artifacts::add_range(&mut forward, *s, *e);
    }
    let mut backward = Vec::new();
    for (s, e) in parts.iter().rev() {
        artifacts::add_range(&mut backward, *s, *e);
    }
    assert_eq!(forward, backward);
    assert!(forward.windows(2).all(|w| w[0].1 < w[1].0), "{forward:?}");
    assert!(forward.iter().all(|(s, e)| s < e));
}

#[test]
fn a_range_is_split_into_what_was_received_and_what_is_missing() {
    let have = [(10, 20), (30, 40)];
    let (covered, missing) = artifacts::split_range(&have, 0, 100);
    assert_eq!(covered, stretches(&[(10, 20), (30, 40)]));
    assert_eq!(missing, stretches(&[(0, 10), (20, 30), (40, 100)]));
    let (covered, missing) = artifacts::split_range(&have, 15, 35);
    assert_eq!(covered, stretches(&[(15, 20), (30, 35)]));
    assert_eq!(missing, stretches(&[(20, 30)]));
}

#[test]
fn a_range_inside_a_stretch_is_all_received_and_one_outside_is_all_missing() {
    let have = [(0, 100)];
    assert_eq!(
        artifacts::split_range(&have, 10, 20),
        (stretches(&[(10, 20)]), vec![])
    );
    assert_eq!(
        artifacts::split_range(&have, 100, 120),
        (vec![], stretches(&[(100, 120)]))
    );
    assert_eq!(
        artifacts::split_range(&[], 5, 9),
        (vec![], stretches(&[(5, 9)]))
    );
}

#[test]
fn the_covered_and_the_missing_parts_make_up_the_range_exactly() {
    for seed in 1..40u32 {
        let mut have = Vec::new();
        for c in lcg(seed, 12, 100).chunks(2) {
            artifacts::add_range(&mut have, c[0] as u64, c[0] as u64 + c[1] as u64 % 10 + 1);
        }
        let (start, end) = (seed as u64 % 30, 40 + seed as u64 % 70);
        let (covered, missing) = artifacts::split_range(&have, start, end);
        let sum: u64 = covered
            .iter()
            .chain(missing.iter())
            .map(|(s, e)| e - s)
            .sum();
        assert_eq!(sum, end - start, "{have:?} {start}..{end}");
    }
}

#[test]
fn a_version_is_newer_when_a_number_of_it_is_bigger_from_the_left() {
    assert!(updates::is_newer("0.2.2", "0.2.3"));
    assert!(updates::is_newer("0.2.9", "0.3.0"));
    assert!(updates::is_newer("1.9", "1.10"));
    assert!(updates::is_newer("0.9.9", "1.0.0"));
    assert!(!updates::is_newer("0.2.3", "0.2.2"));
    assert!(!updates::is_newer("0.2.2", "0.2.2"));
    assert!(!updates::is_newer("1.10", "1.9"));
}

#[test]
fn a_version_with_a_v_or_blanks_or_a_missing_part_is_read_as_the_numbers_in_it() {
    assert!(updates::is_newer("v1.0.0", "1.0.1"));
    assert!(updates::is_newer("1.0.0", "v1.0.1"));
    assert!(updates::is_newer(" 1.2.3 ", " 1.2.4 "));
    assert!(!updates::is_newer("1.0", "1.0.0"));
    assert!(!updates::is_newer("1.0.0", "1.0"));
    assert!(updates::is_newer("1.0", "1.0.1"));
    assert!(updates::is_newer("", "0.0.1"));
}

#[test]
fn a_part_that_is_not_a_number_counts_as_zero_and_a_suffix_is_ignored() {
    assert!(!updates::is_newer("x", "y"));
    assert!(updates::is_newer("1.x", "1.1"));
    assert!(!updates::is_newer("1.0.0-rc1", "1.0.0"));
    assert!(updates::is_newer("1.0.0-rc1", "1.0.1"));
    assert!(!updates::is_newer("1.0.0", "garbage"));
}

#[test]
fn any_two_versions_are_never_each_newer_than_the_other() {
    let alphabet = b"0123456789.v-x ";
    for seed in 1..200u32 {
        let a: String = lcg(seed, 8, alphabet.len() as u32)
            .iter()
            .map(|b| alphabet[*b as usize] as char)
            .collect();
        let b: String = lcg(seed + 500, 8, alphabet.len() as u32)
            .iter()
            .map(|b| alphabet[*b as usize] as char)
            .collect();
        assert!(
            !(updates::is_newer(&a, &b) && updates::is_newer(&b, &a)),
            "{a:?} {b:?}"
        );
        assert!(!updates::is_newer(&a, &a), "{a:?}");
    }
}

#[test]
fn the_name_of_an_application_is_lower_case_digits_and_hyphens_up_to_thirty_two() {
    let long = "x".repeat(32);
    for good in ["a", "studio", "bungard-gui", "pcb2", "a-b-c", long.as_str()] {
        assert!(apps::valid_app(good), "{good}");
    }
    let long = "x".repeat(33);
    for bad in [
        "",
        "Studio",
        "-a",
        "a-",
        "a_b",
        "a b",
        "é",
        "a.b",
        long.as_str(),
    ] {
        assert!(!apps::valid_app(bad), "{bad}");
    }
}

#[test]
fn the_known_applications_are_well_formed_and_not_repeated() {
    let mut seen = std::collections::HashSet::new();
    for app in apps::KNOWN_APPS {
        assert!(apps::valid_app(app), "{app}");
        assert!(seen.insert(app), "{app} twice");
    }
    assert!(apps::KNOWN_APPS.contains(&"studio"));
}

#[test]
fn an_application_is_accepted_when_it_is_known_or_added_or_the_check_is_off() {
    let c = ServerConfig::default();
    assert!(apps::allowed(&c, "studio"));
    assert!(!apps::allowed(&c, "mystery"));
    assert!(!apps::allowed(&c, "Studio"));
    let extra = cfg("auth_extra_apps = mystery");
    assert!(apps::allowed(&extra, "mystery"));
    let open = cfg("auth_known_apps_only = no");
    assert!(apps::allowed(&open, "mystery"));
    assert!(!apps::allowed(&open, "Not Well Formed"));
    assert!(!apps::allowed(&open, ""));
}

#[test]
fn the_list_of_applications_has_the_known_ones_and_then_the_added_ones_once() {
    let c = cfg("auth_extra_apps = zed, studio, zed");
    let list = apps::list(&c);
    assert_eq!(list.len(), apps::KNOWN_APPS.len() + 1);
    assert_eq!(list.last().map(String::as_str), Some("zed"));
    assert_eq!(list[0], apps::KNOWN_APPS[0]);
}

#[test]
fn a_label_for_a_person_has_one_to_the_limit_characters_with_no_control_characters() {
    assert!(apps::valid_label("Kitchen printer", 20));
    assert!(apps::valid_label("é", 1));
    assert!(apps::valid_label(&"é".repeat(20), 20));
    assert!(!apps::valid_label(&"é".repeat(21), 20));
    for bad in ["", "   ", "\t", "a\nb", "a\u{0}b", "a\u{7f}b"] {
        assert!(!apps::valid_label(bad, 20), "{bad:?}");
    }
    assert!(!apps::valid_label("a", 0));
}

#[test]
fn the_name_of_a_user_is_up_to_thirty_two_plain_characters() {
    let long = "x".repeat(32);
    for good in ["ann", "a.b_c-d", "A1", long.as_str()] {
        assert!(auth::valid_name(good), "{good}");
    }
    let long = "x".repeat(33);
    for bad in ["", "a b", "é", "a/b", "a\\b", "a@b", "a\n", long.as_str()] {
        assert!(!auth::valid_name(bad), "{bad:?}");
    }
}

#[test]
fn a_password_has_eight_to_one_hundred_and_twenty_eight_characters_not_bytes() {
    assert!(auth::valid_password("12345678"));
    assert!(auth::valid_password(&"é".repeat(8)));
    assert!(auth::valid_password(&"x".repeat(128)));
    assert!(!auth::valid_password("1234567"));
    assert!(!auth::valid_password(&"é".repeat(7)));
    assert!(!auth::valid_password(&"x".repeat(129)));
    assert!(!auth::valid_password(""));
}

// ---------------------------------------------------------------- the announcement on the network

fn name(labels: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for l in labels {
        out.push(l.len() as u8);
        out.extend_from_slice(l.as_bytes());
    }
    out.push(0);
    out
}

fn header(flags: u16, questions: u16) -> Vec<u8> {
    let mut h = vec![0, 0];
    h.extend_from_slice(&flags.to_be_bytes());
    h.extend_from_slice(&questions.to_be_bytes());
    h.extend_from_slice(&[0; 6]);
    h
}

fn question(labels: &[&str], qtype: u16, class: u16) -> Vec<u8> {
    let mut q = name(labels);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&class.to_be_bytes());
    q
}

fn packet(flags: u16, questions: &[Vec<u8>]) -> Vec<u8> {
    let mut p = header(flags, questions.len() as u16);
    for q in questions {
        p.extend_from_slice(q);
    }
    p
}

#[test]
fn the_constants_of_the_announcement_are_the_ones_of_the_protocol() {
    assert_eq!(mdns::MDNS_PORT, 5353);
    assert_eq!(mdns::GROUP.octets(), [224, 0, 0, 251]);
    assert_eq!(mdns::SERVICE, ["_eh3d", "_tcp", "local"]);
    assert_eq!(
        (mdns::MAX_PACKET, mdns::MAX_QUESTIONS, mdns::TTL),
        (1500, 16, 120)
    );
}

#[test]
fn a_query_for_the_service_is_read_with_its_type_and_its_reply_address() {
    let p = packet(0, &[question(&["_eh3d", "_tcp", "local"], 12, 1)]);
    assert_eq!(
        mdns::parse_query(&p),
        Some(vec![Question {
            name: vec!["_eh3d".into(), "_tcp".into(), "local".into()],
            qtype: 12,
            unicast: false
        }])
    );
    let p = packet(0, &[question(&["a"], 255, 0x8001)]);
    let q = mdns::parse_query(&p).unwrap();
    assert_eq!((q[0].qtype, q[0].unicast), (255, true));
}

#[test]
fn the_labels_of_a_question_are_in_lower_case_and_bytes_that_are_not_text_are_kept_as_a_mark() {
    let p = packet(0, &[question(&["_EH3D", "_Tcp", "LOCAL"], 12, 1)]);
    assert_eq!(
        mdns::parse_query(&p).unwrap()[0].name,
        ["_eh3d", "_tcp", "local"]
    );
    let mut raw = header(0, 1);
    raw.extend_from_slice(&[1, 0xFF, 0, 0, 12, 0, 1]);
    assert_eq!(mdns::parse_query(&raw).unwrap()[0].name, ["\u{fffd}"]);
}

#[test]
fn an_answer_or_a_packet_that_is_not_a_query_is_not_read() {
    let q = question(&["a"], 12, 1);
    assert!(mdns::parse_query(&packet(0x8000, std::slice::from_ref(&q))).is_none());
    assert!(mdns::parse_query(&packet(0x0800, std::slice::from_ref(&q))).is_none());
    assert!(mdns::parse_query(&packet(0x7800, std::slice::from_ref(&q))).is_none());
    assert!(mdns::parse_query(&packet(0x0100, std::slice::from_ref(&q))).is_some());
}

#[test]
fn a_query_with_no_question_or_too_many_is_not_read() {
    assert!(mdns::parse_query(&header(0, 0)).is_none());
    assert!(mdns::parse_query(&header(0, 17)).is_none());
    let q = question(&["a"], 12, 1);
    let sixteen: Vec<Vec<u8>> = (0..16).map(|_| q.clone()).collect();
    assert_eq!(mdns::parse_query(&packet(0, &sixteen)).unwrap().len(), 16);
    let seventeen: Vec<Vec<u8>> = (0..17).map(|_| q.clone()).collect();
    assert!(mdns::parse_query(&packet(0, &seventeen)).is_none());
}

#[test]
fn a_packet_that_is_too_short_too_long_or_cut_is_not_read() {
    assert!(mdns::parse_query(&[]).is_none());
    assert!(mdns::parse_query(&[0; 11]).is_none());
    assert!(mdns::parse_query(&vec![0; mdns::MAX_PACKET + 1]).is_none());
    assert!(mdns::parse_query(&header(0, 1)).is_none());
    let whole = packet(0, &[question(&["a", "b"], 12, 1)]);
    for len in 12..whole.len() {
        assert!(mdns::parse_query(&whole[..len]).is_none(), "{len}");
    }
    assert!(mdns::parse_query(&whole).is_some());
}

#[test]
fn a_name_that_points_back_to_an_earlier_name_is_followed_and_one_that_points_to_itself_is_not() {
    let first = question(&["_eh3d", "_tcp", "local"], 12, 1);
    let mut second = vec![0xC0, 12];
    second.extend_from_slice(&[0, 12, 0, 1]);
    let two = mdns::parse_query(&packet(0, &[first, second])).unwrap();
    assert_eq!(two[0].name, two[1].name);
    let mut looping = header(0, 1);
    looping.extend_from_slice(&[0xC0, 12, 0, 12, 0, 1]);
    assert!(mdns::parse_query(&looping).is_none());
    let mut cut = header(0, 1);
    cut.push(0xC0);
    assert!(mdns::parse_query(&cut).is_none());
}

#[test]
fn a_label_that_is_too_long_or_a_name_that_is_too_long_is_not_read() {
    let sixty_three = "a".repeat(63);
    let sixty_four = "a".repeat(64);
    assert!(mdns::parse_query(&packet(0, &[question(&[sixty_three.as_str()], 1, 1)])).is_some());
    assert!(mdns::parse_query(&packet(0, &[question(&[sixty_four.as_str()], 1, 1)])).is_none());
    let three = [sixty_three.as_str(); 3];
    let four = [sixty_three.as_str(); 4];
    assert!(mdns::parse_query(&packet(0, &[question(&three, 1, 1)])).is_some());
    assert!(mdns::parse_query(&packet(0, &[question(&four, 1, 1)])).is_none());
}

#[test]
fn a_name_has_at_most_thirty_two_labels() {
    let a = ["a"; 32];
    let b = ["a"; 33];
    assert!(mdns::parse_query(&packet(0, &[question(&a, 1, 1)])).is_some());
    assert!(mdns::parse_query(&packet(0, &[question(&b, 1, 1)])).is_none());
}

#[test]
fn bytes_of_any_kind_are_read_as_a_query_without_a_panic() {
    for seed in 1..400u32 {
        let mut p = lcg(seed, 12 + (seed as usize * 5) % 90, 256);
        p[2] = 0;
        p[3] = 0;
        p[4] = 0;
        p[5] = 1 + (seed % 3) as u8;
        let _ = mdns::parse_query(&p);
    }
}

#[test]
fn a_name_for_the_network_has_no_dots_or_control_characters_and_is_cut_on_a_letter() {
    assert_eq!(mdns::clean_label("a.b", "fb"), "ab");
    assert_eq!(mdns::clean_label("My Server", "fb"), "My Server");
    assert_eq!(mdns::clean_label("a\nb\tc", "fb"), "abc");
    assert_eq!(mdns::clean_label("  a  ", "fb"), "a");
    for nothing in ["", "...", "   ", "\n\t"] {
        assert_eq!(mdns::clean_label(nothing, "fb"), "fb", "{nothing:?}");
    }
    assert_eq!(mdns::clean_label(&"x".repeat(100), "fb").len(), 63);
    let cut = mdns::clean_label(&"é".repeat(40), "fb");
    assert_eq!(cut.len(), 62);
    assert_eq!(cut.chars().count(), 31);
}

#[test]
fn the_name_of_the_computer_is_a_label_of_letters_digits_and_hyphens() {
    assert_eq!(mdns::host_label("My Server_1"), "myserver1");
    assert_eq!(mdns::host_label("host.local"), "hostlocal");
    assert_eq!(mdns::host_label("A-B"), "a-b");
    assert_eq!(mdns::host_label("--a--"), "a");
    for nothing in ["", "é", "---", "...", "日本"] {
        assert_eq!(mdns::host_label(nothing), "eh3d-server", "{nothing:?}");
    }
    assert_eq!(mdns::host_label(&"a".repeat(100)).len(), 63);
    assert_eq!(mdns::host_label(&format!("{}-b", "a".repeat(62))).len(), 62);
}

// ---------------------------------------------------------------- the readers of STL and 3MF files

fn stl(triangles: &[[f32; 9]]) -> Vec<u8> {
    let mut b = vec![0u8; 80];
    b.extend_from_slice(&(triangles.len() as u32).to_le_bytes());
    for t in triangles {
        b.extend_from_slice(&[0u8; 12]);
        for v in t {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[0u8; 2]);
    }
    b
}

const TRI: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

const TEXT_TRI: &str = "solid x\n facet normal 0 0 0\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 1 0\n  endloop\n endfacet\nendsolid x\n";

fn why(result: Result<Mesh, slicer::SliceError>) -> (&'static str, String) {
    let e = result.unwrap_err();
    (e.code, e.detail)
}

#[test]
fn a_binary_stl_with_one_triangle_is_read_exactly() {
    let mesh = slicer::parse_stl(&stl(&[TRI]), 10).unwrap();
    assert_eq!(
        mesh.triangles,
        [[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]]
    );
}

#[test]
fn a_binary_stl_whose_header_says_solid_is_still_binary_when_its_size_fits() {
    let mut bytes = stl(&[TRI, TRI]);
    bytes[..5].copy_from_slice(b"solid");
    assert_eq!(slicer::parse_stl(&bytes, 10).unwrap().triangles.len(), 2);
}

#[test]
fn a_binary_stl_with_no_triangle_is_refused() {
    assert_eq!(
        why(slicer::parse_stl(&stl(&[]), 10)),
        ("bad_mesh", "the file has no triangle".to_string())
    );
}

#[test]
fn more_triangles_than_the_limit_are_over_the_limit_in_binary_and_in_text() {
    assert_eq!(
        why(slicer::parse_stl(&stl(&[TRI, TRI]), 1)),
        (
            "slice_limit",
            "the mesh has 2 triangles, more than the 1 allowed".to_string()
        )
    );
    assert_eq!(
        slicer::parse_stl(&stl(&[TRI, TRI]), 2)
            .unwrap()
            .triangles
            .len(),
        2
    );
    let two = format!("{TEXT_TRI}{TEXT_TRI}");
    assert_eq!(
        why(slicer::parse_stl(two.as_bytes(), 1)),
        (
            "slice_limit",
            "the mesh has more than the 1 triangles allowed".to_string()
        )
    );
}

#[test]
fn a_vertex_that_is_not_a_usable_number_is_refused() {
    for bad in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1.0e6,
        -1.0e6,
        3.0e38,
    ] {
        let mut t = TRI;
        t[4] = bad;
        let (code, detail) = why(slicer::parse_stl(&stl(&[t]), 10));
        assert_eq!(code, "bad_mesh", "{bad}");
        assert_eq!(detail, "a vertex is not a number the slicer can use");
    }
    let mut edge = TRI;
    edge[4] = 999_999.0;
    assert!(slicer::parse_stl(&stl(&[edge]), 10).is_ok());
}

#[test]
fn a_binary_file_that_is_one_byte_short_or_long_is_not_a_mesh() {
    let whole = stl(&[TRI, TRI]);
    assert_eq!(
        why(slicer::parse_stl(&whole[..whole.len() - 1], 10)).0,
        "bad_mesh"
    );
    let mut long = whole.clone();
    long.push(0);
    assert_eq!(why(slicer::parse_stl(&long, 10)).0, "bad_mesh");
}

#[test]
fn a_count_that_the_file_cannot_hold_is_not_a_reason_to_allocate_or_to_panic() {
    let mut bytes = vec![0u8; 84];
    bytes[80..84].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(why(slicer::parse_stl(&bytes, usize::MAX)).0, "bad_mesh");
    assert_eq!(why(slicer::parse_stl(&bytes[..83], 10)).0, "bad_mesh");
    assert_eq!(why(slicer::parse_stl(&[], 10)).0, "bad_mesh");
}

#[test]
fn a_text_stl_is_read_with_any_indentation_and_line_ends() {
    let mesh = slicer::parse_stl(TEXT_TRI.as_bytes(), 10).unwrap();
    assert_eq!(mesh.triangles.len(), 1);
    let crlf = TEXT_TRI.replace('\n', "\r\n");
    assert_eq!(slicer::parse_stl(crlf.as_bytes(), 10).unwrap(), mesh);
    let padded = format!("\n  \t{TEXT_TRI}");
    assert_eq!(slicer::parse_stl(padded.as_bytes(), 10).unwrap(), mesh);
}

#[test]
fn a_text_stl_with_an_exponent_is_read_as_a_number() {
    let text = TEXT_TRI.replace("vertex 1 0 0", "vertex 1e2 -2.5E-1 0");
    let mesh = slicer::parse_stl(text.as_bytes(), 10).unwrap();
    assert_eq!(mesh.triangles[0][1], [100.0, -0.25, 0.0]);
}

#[test]
fn a_text_stl_that_is_cut_or_empty_or_wrong_is_refused() {
    let two_vertices = "solid x\nvertex 0 0 0\nvertex 1 0 0\nendsolid x\n";
    assert_eq!(
        why(slicer::parse_stl(two_vertices.as_bytes(), 10)),
        (
            "bad_mesh",
            "a triangle of the text STL has not three vertices".to_string()
        )
    );
    assert_eq!(
        why(slicer::parse_stl(b"solid x\nendsolid x\n", 10)),
        ("bad_mesh", "the file has no triangle".to_string())
    );
    let short = "solid x\nvertex 0 0\n";
    assert_eq!(why(slicer::parse_stl(short.as_bytes(), 10)).0, "bad_mesh");
    let too_many = "solid x\nvertex 0 0 0 1\n";
    assert_eq!(
        why(slicer::parse_stl(too_many.as_bytes(), 10)).0,
        "bad_mesh"
    );
    assert_eq!(why(slicer::parse_stl(b"hello", 10)).0, "bad_mesh");
    assert_eq!(why(slicer::parse_stl(b"SOLID x\n", 10)).0, "bad_mesh");
}

#[test]
fn a_text_stl_with_a_vertex_that_is_not_finite_or_is_huge_is_refused() {
    for bad in ["nan", "inf", "-inf", "1e400", "1e6", "-1e6"] {
        let text = TEXT_TRI.replace("vertex 0 1 0", &format!("vertex 0 {bad} 0"));
        assert_eq!(
            why(slicer::parse_stl(text.as_bytes(), 10)).0,
            "bad_mesh",
            "{bad}"
        );
    }
}

#[test]
fn the_box_around_a_mesh_has_its_lowest_and_highest_corner() {
    assert_eq!(Mesh::default().bounds(), None);
    let mesh = Mesh {
        triangles: vec![[[1.0, -2.0, 3.0], [-4.0, 5.0, 0.0], [0.5, 0.5, 9.0]]],
    };
    assert_eq!(mesh.bounds(), Some(([-4.0, -2.0, 0.0], [1.0, 5.0, 9.0])));
}

#[test]
fn a_3mf_that_is_not_a_zip_is_refused_by_what_it_is() {
    for bytes in [
        vec![],
        vec![0u8; 21],
        vec![0u8; 22],
        b"not a zip at all, not even close".to_vec(),
        vec![0x50, 0x4b, 5, 6],
    ] {
        let e = slicer::parse_3mf(&bytes, 10).unwrap_err();
        assert_eq!(e.code, "bad_mesh");
        assert_eq!(e.detail, "the 3MF file is not a zip");
    }
}

#[test]
fn bytes_that_look_like_the_end_of_a_zip_never_stop_the_reader() {
    for seed in 1..300u32 {
        let len = 30 + (seed as usize * 7) % 200;
        let mut bytes = lcg(seed, len, 256);
        let at = len - 22;
        bytes[at..at + 4].copy_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
        let _ = slicer::parse_3mf(&bytes, 100);
        let _ = slicer::parse_stl(&bytes, 100);
    }
}
