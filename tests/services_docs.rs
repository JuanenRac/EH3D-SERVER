//! The four common services against their documents: the overview names them, the contract tags them, and the settings they add are in `server.cfg.example` with the
//! values the server starts with.

use std::path::PathBuf;

use eh3d_server::config::{parse, ServerConfig};
use serde_json::Value;

fn read(path: &str) -> String {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()))
}

#[test]
fn the_overview_names_the_four_services_and_each_has_its_document() {
    let overview = read("docs/SERVICES.md");
    let api = read("docs/API.md");
    for document in [
        "PROJECT_STORE.md",
        "CATALOGS.md",
        "MACHINE_PLUGINS.md",
        "AUTH.md",
    ] {
        assert!(
            overview.contains(document),
            "docs/SERVICES.md does not name {document}"
        );
        assert!(
            api.contains(document),
            "docs/API.md does not name {document}"
        );
        assert!(read(&format!("docs/{document}")).len() > 1000, "{document}");
    }
}

#[test]
fn every_route_of_the_services_has_the_tag_of_its_family_in_the_contract() {
    let doc: Value = serde_json::from_str(&read("docs/openapi.json")).unwrap();
    let mut seen = std::collections::BTreeMap::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        let wanted = if path.starts_with("/apps/") && path.ends_with("/jobs") {
            // the run of a design program is work of the machines, though it keeps its report in the store
            Some("work")
        } else if path.starts_with("/apps/") {
            Some("store")
        } else if path.starts_with("/catalogs") {
            Some("catalogs")
        } else if path.starts_with("/work/")
            || path == "/plugins"
            || path == "/events/stream"
            || path.starts_with("/notifications")
        {
            Some("work")
        } else if path.starts_with("/auth/") {
            Some("auth")
        } else {
            None
        };
        let Some(wanted) = wanted else { continue };
        for (method, operation) in item.as_object().unwrap() {
            assert_eq!(
                operation["tags"][0], wanted,
                "{method} {path} is not under the tag {wanted}"
            );
            *seen.entry(wanted).or_insert(0) += 1;
        }
    }
    for (tag, least) in [("store", 16), ("catalogs", 5), ("work", 17), ("auth", 9)] {
        assert!(
            seen.get(tag).copied().unwrap_or(0) >= least,
            "the tag {tag} has {:?} operations, not {least}",
            seen.get(tag)
        );
    }
}

#[test]
fn the_new_settings_are_in_the_example_with_the_values_the_server_starts_with() {
    let text = read("server.cfg.example");
    let from_the_example = parse(&text).expect("server.cfg.example reads");
    let default = ServerConfig::default();
    for key in [
        "project_max_file_mb",
        "project_max_mb",
        "project_max_files",
        "project_max_projects",
        "project_versions_kept",
        "project_lock_ttl_s",
        "project_require_if_match",
        "project_shared",
        "catalog_max_mb",
        "catalog_max_items",
        "catalog_page_max",
        "work_queue_max",
        "work_keep",
        "events_clients_max",
        "events_queue",
        "notifications_keep",
        "auth_known_apps_only",
        "auth_extra_apps",
        "auth_max_sessions",
        "auth_max_failures",
        "auth_lockout_s",
        "auth_refresh_max_hours",
        "password_min_len",
        "password_classes",
        "password_deny_name",
        "audit_denials",
        "design_run",
        "design_program",
        "design_timeout_s",
        "design_max_output_kb",
        "tls_cert_file",
        "tasks_enabled",
        "tasks_selftest",
        "max_tasks",
        "keep_tasks",
        "max_concurrent_tasks",
        "task_timeout_s",
        "tasks_per_user",
        "task_params_kb",
        "idempotency_ttl_s",
        "artifact_store_gb",
        "max_upload_mb",
        "upload_ttl_s",
        "uploads_per_user",
        "artifact_unreferenced_days",
        "shares_enabled",
    ] {
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with(&format!("{key} ="))),
            "server.cfg.example has no line for {key}"
        );
    }
    assert_eq!(
        from_the_example.project_max_file_mb,
        default.project_max_file_mb
    );
    assert_eq!(from_the_example.project_max_mb, default.project_max_mb);
    assert_eq!(
        from_the_example.project_versions_kept,
        default.project_versions_kept
    );
    assert_eq!(
        from_the_example.project_require_if_match,
        default.project_require_if_match
    );
    assert_eq!(from_the_example.project_shared, default.project_shared);
    assert_eq!(from_the_example.catalog_max_mb, default.catalog_max_mb);
    assert_eq!(from_the_example.work_queue_max, default.work_queue_max);
    assert_eq!(from_the_example.events_queue, default.events_queue);
    assert_eq!(
        from_the_example.auth_known_apps_only,
        default.auth_known_apps_only
    );
    assert_eq!(from_the_example.auth_extra_apps, default.auth_extra_apps);
    assert_eq!(
        from_the_example.auth_max_sessions,
        default.auth_max_sessions
    );
    assert_eq!(from_the_example.password_min_len, default.password_min_len);
    assert_eq!(
        from_the_example.password_deny_name,
        default.password_deny_name
    );
    assert_eq!(from_the_example.audit_denials, default.audit_denials);
    assert_eq!(from_the_example.design_run, default.design_run);
    assert_eq!(from_the_example.design_program, default.design_program);
    assert_eq!(from_the_example.design_timeout_s, default.design_timeout_s);
    assert_eq!(
        from_the_example.design_max_output_kb,
        default.design_max_output_kb
    );
    assert_eq!(from_the_example.tls_cert_file, default.tls_cert_file);
}
