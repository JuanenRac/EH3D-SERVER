//! The checklist of the routes: every route that the files of the tasks, the artifacts and the projects register has to be written down in `docs/API.md`, with the same shape
//! (the names of the parameters may differ), and the document must not describe a request of these families that is not there. The contract of all the routes is the
//! OpenAPI document (`docs/openapi.yaml`, with `docs/openapi.json` and the table `docs/ROUTES.md`), which `tests/openapi.rs` compares with the router; this file keeps
//! the text of `docs/API.md` in step with it.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A path with every `:name` replaced by `:`, without the `/api/v1` start.
fn shape(path: &str) -> String {
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    path.split('/')
        .map(|part| if part.starts_with(':') { ":" } else { part })
        .collect::<Vec<_>>()
        .join("/")
}

/// The shapes of the paths a source file registers with `.route("...")`.
fn registered(file: &str) -> BTreeSet<String> {
    let text = std::fs::read_to_string(root().join("src").join(file)).unwrap();
    let code = text.split("#[cfg(test)]").next().unwrap();
    let mut out = BTreeSet::new();
    for (i, _) in code.match_indices(".route(") {
        let rest = code[i + ".route(".len()..].trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        if let Some(end) = rest.find('"') {
            out.insert(shape(&rest[..end]));
        }
    }
    out
}

/// The shapes of the paths the document writes (in the code spans of its tables), for the families asked for.
fn documented(prefixes: &[&str]) -> BTreeSet<String> {
    let doc = std::fs::read_to_string(root().join("docs").join("API.md")).unwrap();
    let mut out = BTreeSet::new();
    for span in doc.split('`') {
        for word in span.split_whitespace() {
            let word = word.trim_end_matches([',', ';', '.']);
            let word = word.split('?').next().unwrap_or(word);
            if word.starts_with('/') && prefixes.iter().any(|p| word.starts_with(p)) {
                out.insert(shape(word));
            }
        }
    }
    out
}

#[test]
fn every_route_of_the_new_families_is_in_the_api_document() {
    let mut routes = BTreeSet::new();
    for file in ["api_tasks.rs", "api_artifacts.rs", "api_projects.rs"] {
        let found = registered(file);
        assert!(
            !found.is_empty(),
            "{file} registers no route: the scan is broken"
        );
        routes.extend(found);
    }
    let documented = documented(&["/tasks", "/artifacts", "/blobs", "/uploads", "/projects"]);
    let missing: Vec<_> = routes.difference(&documented).collect();
    assert!(
        missing.is_empty(),
        "routes that docs/API.md does not describe: {missing:?}"
    );
    let invented: Vec<_> = documented.difference(&routes).collect();
    assert!(
        invented.is_empty(),
        "paths docs/API.md describes that no route registers: {invented:?}"
    );
}

#[test]
fn the_rotation_of_a_token_is_documented() {
    let doc = std::fs::read_to_string(root().join("docs").join("API.md")).unwrap();
    assert!(doc.contains("/tokens/:id/rotate"));
    let api = std::fs::read_to_string(root().join("src").join("api.rs")).unwrap();
    assert!(api.contains("\"/api/v1/tokens/:id/rotate\""));
}

#[test]
fn the_api_document_names_the_openapi_contract_and_its_files_exist() {
    let doc = std::fs::read_to_string(root().join("docs").join("API.md")).unwrap();
    assert!(doc.contains("docs/openapi.yaml"));
    assert!(doc.contains("docs/ROUTES.md"));
    for file in ["openapi.json", "openapi.yaml", "ROUTES.md"] {
        assert!(
            root().join("docs").join(file).is_file(),
            "docs/{file} is missing"
        );
    }
}
