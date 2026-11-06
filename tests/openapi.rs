//! The contract of the interface: `docs/openapi.json` (and `docs/openapi.yaml`, the same document in YAML, and the table `docs/ROUTES.md`) must say what the router registers.
//! Every route of the files of the router is in the document with the same method, and the document has no other; every operation has a summary, a tag, a role and
//! responses; the error codes an operation names are in the catalog `docs/error-codes.json` with a status the operation answers; every reference resolves.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

const METHODS: [&str; 5] = ["get", "post", "put", "delete", "head"];
const ROUTER_FILES: [&str; 9] = [
    "api.rs",
    "api_tasks.rs",
    "api_artifacts.rs",
    "api_projects.rs",
    "api_pstore.rs",
    "api_catalogs.rs",
    "api_work.rs",
    "api_auth.rs",
    "design.rs",
];

fn read(path: &str) -> String {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()))
}

fn document() -> Value {
    serde_json::from_str(&read("docs/openapi.json")).expect("docs/openapi.json is valid JSON")
}

/// Whether `text` calls `method(` (and not a longer name that ends the same way, like `inspect_profile_put(`).
fn has_call(text: &str, method: &str) -> bool {
    let needle = format!("{method}(");
    text.match_indices(needle.as_str()).any(|(i, _)| {
        text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// `(METHOD, path)` of every `.route("path", get(..).post(..))` that the files of the router register.
fn router_routes() -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for file in ROUTER_FILES {
        let text = read(&format!("src/{file}"));
        let code = text.split("#[cfg(test)]").next().unwrap();
        for (i, _) in code.match_indices(".route(") {
            let rest = &code[i + ".route(".len()..];
            let mut depth = 1usize;
            let mut end = rest.len();
            for (j, c) in rest.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = j;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(args) = rest[..end].trim_start().strip_prefix('"') else {
                continue;
            };
            let Some(quote) = args.find('"') else {
                continue;
            };
            let (path, handlers) = args.split_at(quote);
            for method in METHODS {
                if has_call(handlers, method) {
                    out.insert((method.to_uppercase(), path.to_string()));
                }
            }
        }
    }
    out
}

/// The path of the document (`/jobs/{id}`) as the router writes it (`/api/v1/jobs/:id`).
fn router_path(path: &str) -> String {
    let parts: Vec<String> = path
        .split('/')
        .map(
            |part| match part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                Some(name) => format!(":{name}"),
                None => part.to_string(),
            },
        )
        .collect();
    format!("/api/v1{}", parts.join("/"))
}

fn operations(doc: &Value) -> Vec<(String, String, &Value)> {
    let mut out = Vec::new();
    for (path, item) in doc["paths"].as_object().expect("paths is an object") {
        for (method, operation) in item.as_object().expect("a path holds operations") {
            out.push((method.to_uppercase(), path.clone(), operation));
        }
    }
    out
}

fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                match (key.as_str(), item) {
                    ("$ref", Value::String(target)) => out.push(target.clone()),
                    _ => collect_refs(item, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
        _ => {}
    }
}

#[test]
fn the_document_is_openapi_31_with_the_title_and_the_version_of_the_server() {
    let doc = document();
    assert!(doc["openapi"].as_str().unwrap_or("").starts_with("3.1"));
    assert_eq!(doc["info"]["title"], "EH3D Server API");
    assert_eq!(doc["info"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(doc["components"]["schemas"]
        .as_object()
        .is_some_and(|m| !m.is_empty()));
}

#[test]
fn every_route_of_the_router_is_in_the_openapi_and_the_openapi_has_no_other() {
    let doc = document();
    let routes = router_routes();
    assert!(
        routes.len() > 50,
        "the scan of the router found only {} routes: it is broken",
        routes.len()
    );
    let described: BTreeSet<(String, String)> = operations(&doc)
        .into_iter()
        .map(|(method, path, _)| (method, router_path(&path)))
        .collect();
    let missing: Vec<_> = routes.difference(&described).collect();
    assert!(
        missing.is_empty(),
        "routes that docs/openapi.json does not describe: {missing:?}"
    );
    let invented: Vec<_> = described.difference(&routes).collect();
    assert!(
        invented.is_empty(),
        "operations of docs/openapi.json that no route registers: {invented:?}"
    );
}

#[test]
fn every_operation_has_a_summary_a_tag_a_role_and_responses() {
    let doc = document();
    let tags: BTreeSet<&str> = doc["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    let mut ids = BTreeSet::new();
    for (method, path, operation) in operations(&doc) {
        let label = format!("{method} {path}");
        assert!(
            METHODS.contains(&method.to_lowercase().as_str()),
            "{label}: not a method of the router"
        );
        assert!(
            operation["summary"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "{label} has no summary"
        );
        assert!(
            operation["responses"]
                .as_object()
                .is_some_and(|m| !m.is_empty()),
            "{label} has no responses"
        );
        let own_tags = operation["tags"].as_array().expect("tags of an operation");
        assert!(!own_tags.is_empty(), "{label} has no tag");
        for tag in own_tags {
            assert!(
                tags.contains(tag.as_str().unwrap_or("")),
                "{label}: the tag {tag} is not declared"
            );
        }
        let role = operation["x-role"].as_str().unwrap_or("");
        assert!(
            ["none", "any", "person", "viewer", "operator", "admin"].contains(&role),
            "{label} does not say who may call it"
        );
        let id = operation["operationId"].as_str().unwrap_or("");
        assert!(
            !id.is_empty() && ids.insert(id.to_string()),
            "{label} has no operationId, or one that another operation has"
        );
        let declared: BTreeSet<&str> = operation["parameters"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter(|p| p["in"] == "path")
                    .filter_map(|p| p["name"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        let in_path: BTreeSet<&str> = path
            .split('/')
            .filter_map(|part| part.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
            .collect();
        assert_eq!(declared, in_path, "{label}: the parameters of the path");
    }
}

#[test]
fn the_error_codes_of_the_operations_are_in_the_catalog_with_a_status_they_answer() {
    let doc = document();
    let catalog: Value = serde_json::from_str(&read("docs/error-codes.json")).unwrap();
    let status_of = |code: &str| -> Option<u64> {
        catalog
            .as_array()?
            .iter()
            .find(|item| item["code"] == code)
            .and_then(|item| item["status"].as_u64())
    };
    let mut named = 0;
    for (method, path, operation) in operations(&doc) {
        for code in operation["x-error-codes"]
            .as_array()
            .expect("x-error-codes")
        {
            let code = code.as_str().unwrap_or("");
            let status = status_of(code)
                .unwrap_or_else(|| panic!("{method} {path} names {code}, which the catalog lacks"));
            assert!(
                operation["responses"].get(status.to_string()).is_some(),
                "{method} {path} names {code} but has no response for {status}"
            );
            named += 1;
        }
    }
    assert!(
        named > 100,
        "only {named} error codes are named: the check is broken"
    );
}

#[test]
fn every_reference_of_the_document_resolves() {
    let doc = document();
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    assert!(!refs.is_empty());
    for target in refs {
        let pointer = target.strip_prefix('#').unwrap_or(&target);
        assert!(doc.pointer(pointer).is_some(), "{target} does not resolve");
    }
}

#[test]
fn the_yaml_and_the_table_of_routes_carry_the_routes_of_the_json() {
    let doc = document();
    let yaml = read("docs/openapi.yaml");
    let table = read("docs/ROUTES.md");
    assert!(yaml.contains("\nopenapi: \"3.1"));
    assert!(yaml.contains("title: EH3D Server API"));
    for (method, path, _) in operations(&doc) {
        assert!(
            yaml.contains(&format!("\n  \"{path}\":")),
            "docs/openapi.yaml lacks the path {path}"
        );
        let row = format!("| `{}` | {method} |", router_path(&path));
        assert!(table.contains(&row), "docs/ROUTES.md lacks the row {row}");
    }
}
