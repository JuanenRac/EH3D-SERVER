//! The catalog of the error codes (`src/errors.rs`) against the source of the server: a code that the server can answer with and the catalog does not have, or a code of
//! the catalog that no longer exists, fails here. The catalog is also written to `docs/error-codes.json` for the clients, and `docs/API.md` has to name every code.
//!
//! To write the file again after a deliberate change:  EH3D_WRITE_EXAMPLES=1 cargo test --test errors

use std::collections::BTreeSet;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The text of the source files, without their tests.
fn sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root().join("src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap().to_string();
            out.push((path.file_name().unwrap().to_string_lossy().into(), code));
        }
    }
    out
}

/// The text of a string literal that starts at the first non-blank character of `at`, if there is one and it is a code (lowercase and underscores).
fn literal(at: &str) -> Option<String> {
    let rest = at.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    let word = &rest[..end];
    (!word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .then(|| word.to_string())
}

/// What follows the first argument of a call whose text starts right after the opening parenthesis.
fn after_first_argument(text: &str) -> &str {
    let (mut depth, mut in_text) = (0i32, false);
    for (i, c) in text.char_indices() {
        match c {
            '"' => in_text = !in_text,
            '(' | '[' | '{' if !in_text => depth += 1,
            ')' | ']' | '}' if !in_text => {
                if depth == 0 {
                    return &text[i..];
                }
                depth -= 1;
            }
            ',' if !in_text && depth == 0 => return &text[i + 1..],
            _ => {}
        }
    }
    ""
}

/// Every code the source can answer with: the second argument of `ApiError::new`, the first of `fail(`, a `code: "..."` and the answers of the `fn code(` of the errors.
fn used() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (_, text) in sources() {
        for (i, _) in text.match_indices("ApiError::new(") {
            let args = &text[i + "ApiError::new(".len()..];
            if let Some(code) = literal(after_first_argument(args)) {
                found.insert(code);
            }
        }
        for (i, _) in text.match_indices("fail(") {
            let before = text[..i].chars().last().unwrap_or(' ');
            if before.is_alphanumeric() || before == '_' {
                continue;
            }
            if let Some(code) = literal(&text[i + "fail(".len()..]) {
                found.insert(code);
            }
        }
        for (i, _) in text.match_indices("code:") {
            if let Some(code) = literal(&text[i + "code:".len()..]) {
                found.insert(code);
            }
        }
        for (i, _) in text.match_indices("fn code(") {
            let body = &text[i..];
            let open = body.find('{').unwrap();
            let (mut depth, mut end) = (0i32, body.len());
            for (j, c) in body[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + j;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            for (j, _) in body[open..end].match_indices("=>") {
                if let Some(code) = literal(&body[open + j + 2..end]) {
                    found.insert(code);
                }
            }
        }
    }
    found
}

#[test]
fn the_catalog_has_every_code_the_server_uses_and_only_those() {
    let catalog: BTreeSet<String> = eh3d_server::errors::ALL
        .iter()
        .map(|(c, _, _)| c.to_string())
        .collect();
    let used = used();
    assert!(used.len() > 40, "the scan finds the codes: {}", used.len());
    let missing: Vec<_> = used.difference(&catalog).collect();
    assert!(
        missing.is_empty(),
        "codes the server uses that are not in src/errors.rs: {missing:?}"
    );
    let stale: Vec<_> = catalog.difference(&used).collect();
    assert!(
        stale.is_empty(),
        "codes of src/errors.rs that the server no longer uses: {stale:?}"
    );
}

#[test]
fn docs_error_codes_json_is_the_catalog() {
    let mut text = serde_json::to_string_pretty(&eh3d_server::errors::json()).unwrap();
    text.push('\n');
    let path = root().join("docs").join("error-codes.json");
    if std::env::var("EH3D_WRITE_EXAMPLES").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert_eq!(
        on_disk, text,
        "docs/error-codes.json is not the catalog: EH3D_WRITE_EXAMPLES=1 cargo test --test errors"
    );
}

#[test]
fn the_api_document_names_every_code() {
    let doc = std::fs::read_to_string(root().join("docs").join("API.md")).unwrap();
    for (code, status, _) in eh3d_server::errors::ALL {
        assert!(
            doc.contains(&format!("| `{code}` | {status} |")),
            "docs/API.md has no row for {code} with its status {status}"
        );
    }
}
