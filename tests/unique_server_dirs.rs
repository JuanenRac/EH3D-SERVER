//! The tests of one file run at the same time and `common::server(name)` keeps its data in a folder named after `name` (and the process), so two tests of a file that give
//! the same name share one folder: the one that starts later empties it while the other writes, and a write fails with `500 storage`. This test reads the files of `tests/`
//! and fails when a file builds two servers with the same name.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The names given to `server(` and `server_with(` in a text, as `(name, position)`.
fn names(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for call in ["server(", "server_with("] {
        for (i, _) in text.match_indices(call) {
            let before = text[..i].chars().last().unwrap_or(' ');
            if before.is_alphanumeric() {
                continue;
            }
            let rest = text[i + call.len()..].trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    found.push(rest[..end].to_string());
                }
            }
        }
    }
    found
}

#[test]
fn no_file_of_tests_builds_two_servers_with_the_same_name() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.extension().is_none_or(|e| e != "rs") || name == "unique_server_dirs.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let mut count: BTreeMap<String, usize> = BTreeMap::new();
        for n in names(&text) {
            *count.entry(n).or_default() += 1;
        }
        let repeated: Vec<_> = count.iter().filter(|(_, n)| **n > 1).collect();
        assert!(
            repeated.is_empty(),
            "{name} builds two servers with the same name, so two tests share one data folder: {repeated:?}"
        );
        checked += 1;
    }
    assert!(checked > 20, "the scan reads the test files: {checked}");
}

#[test]
fn the_scan_sees_a_repeated_name() {
    let text = "let a = common::server(\"x\");\nlet b = server_with(\"x\", |_| {});\nlet c = mkserver(\"x\");";
    assert_eq!(names(text), ["x", "x"]);
}
