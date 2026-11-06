//! The shared catalogs: data the applications read and do not own, the same for all of them. Materials of the boards (the database of `eh3d-materials`, built in), the tools
//! of the tool store of EH3D-TOOL-MANAGER, the profiles of the machines, the libraries of footprints and of symbols and the components base of the PCB tool
//! (`phoenix-parts/1`), the last five as datasets that an administrator uploads as JSON or CSV.
//!
//! Everybody who is logged in reads them (search, filters, sorting, pages, `ETag`); only an administrator writes, and the built-in one is not written at all. A dataset
//! is replaced whole, as a new version, through a temporary file; the previous file stays beside it as `.bak`. The server does not interpret the items beyond the field that
//! names each one: it keeps what the applications agree on and gives it back.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::config::ServerConfig;
use crate::store;

/// What a catalog is.
pub struct Def {
    pub name: &'static str,
    pub title: &'static str,
    /// The keys of a JSON document that may hold the array of items (a document may also be the array itself).
    pub array_keys: &'static [&'static str],
    /// The field that names each item; it has to be there, a text, and unique.
    pub id_field: &'static str,
    /// The `format` a JSON document has to declare, when the catalog has one.
    pub format: Option<&'static str>,
    /// Built in: read only, even for an administrator.
    pub builtin: bool,
}

pub const DEFS: &[Def] = &[
    Def {
        name: "materials",
        title: "Materials of the boards (laminates, prepregs, flex, metal core, PTFE)",
        array_keys: &["materials"],
        id_field: "id",
        format: None,
        builtin: true,
    },
    Def {
        name: "tools",
        title: "Tools of the tool store (drills, end mills, engravers)",
        array_keys: &["tools", "items"],
        id_field: "id",
        format: None,
        builtin: false,
    },
    Def {
        name: "machine-profiles",
        title: "Profiles of machines",
        array_keys: &["profiles", "machines", "items"],
        id_field: "id",
        format: None,
        builtin: false,
    },
    Def {
        name: "footprints",
        title: "Library of footprints",
        array_keys: &["footprints", "items"],
        id_field: "name",
        format: None,
        builtin: false,
    },
    Def {
        name: "symbols",
        title: "Library of symbols",
        array_keys: &["symbols", "items"],
        id_field: "name",
        format: None,
        builtin: false,
    },
    Def {
        name: "components",
        title: "Components base of the PCB tool (phoenix-parts/1)",
        array_keys: &["parts"],
        id_field: "id",
        format: Some("phoenix-parts/1"),
        builtin: false,
    },
];

pub fn def_of(name: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.name == name)
}

#[derive(Debug, PartialEq)]
pub enum CError {
    Unknown,
    UnknownItem,
    Bad(String),
    ReadOnly,
    TooBig,
    Storage,
}

impl CError {
    pub fn code(&self) -> &'static str {
        match self {
            CError::Unknown => "unknown_catalog",
            CError::UnknownItem => "unknown_item",
            CError::Bad(_) => "bad_catalog",
            CError::ReadOnly => "catalog_readonly",
            CError::TooBig => "too_big",
            CError::Storage => "storage",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            CError::Unknown | CError::UnknownItem => 404,
            CError::Bad(_) | CError::TooBig => 422,
            CError::ReadOnly => 409,
            CError::Storage => 500,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            CError::Bad(why) => why.clone(),
            _ => String::new(),
        }
    }
}

/// A dataset as it is kept on the disk.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Stored {
    version: u64,
    sha256: String,
    updated: u64,
    by: String,
    source: String,
    items: Vec<Value>,
}

pub struct Dataset {
    pub version: u64,
    pub sha256: String,
    pub updated: u64,
    pub by: String,
    pub source: String,
    pub items: Vec<Value>,
    index: HashMap<String, usize>,
}

fn sha_of_items(items: &[Value]) -> String {
    let bytes = serde_json::to_vec(items).unwrap_or_default();
    hex::encode(Sha256::digest(&bytes))
}

/// The text of the field that names an item.
fn id_of<'a>(def: &Def, item: &'a Value) -> Option<&'a str> {
    item.get(def.id_field).and_then(Value::as_str)
}

impl Dataset {
    fn build(def: &Def, stored: Stored) -> Dataset {
        let mut index = HashMap::new();
        for (i, item) in stored.items.iter().enumerate() {
            if let Some(id) = id_of(def, item) {
                index.insert(id.to_string(), i);
            }
        }
        Dataset {
            version: stored.version,
            sha256: stored.sha256,
            updated: stored.updated,
            by: stored.by,
            source: stored.source,
            items: stored.items,
            index,
        }
    }

    fn empty() -> Dataset {
        Dataset {
            version: 0,
            sha256: sha_of_items(&[]),
            updated: 0,
            by: String::new(),
            source: String::new(),
            items: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn etag(&self, name: &str) -> String {
        format!("\"{}-{}\"", name, &self.sha256[..16])
    }

    pub fn get(&self, id: &str) -> Option<&Value> {
        self.index.get(id).and_then(|i| self.items.get(*i))
    }

    pub fn summary(&self, def: &Def) -> Value {
        json!({
            "name": def.name, "title": def.title, "builtin": def.builtin,
            "items": self.items.len(), "version": self.version, "sha256": self.sha256,
            "updated": self.updated, "by": self.by, "source": self.source,
            "etag": self.etag(def.name),
        })
    }
}

/// What a request for a page of a catalog asks.
#[derive(Debug, Default, Clone)]
pub struct Query {
    pub q: String,
    /// `(field, value)`: the field (a path with dots for a nested one) equals the value in any letter case; a value that starts with `~` only has to contain the rest.
    pub filters: Vec<(String, String)>,
    pub sort: Option<String>,
    pub descending: bool,
    pub offset: usize,
    pub limit: usize,
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn path_get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for part in path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

fn has_text(v: &Value, needle: &str, depth: u8) -> bool {
    match v {
        Value::String(s) => s.to_lowercase().contains(needle),
        Value::Number(n) => n.to_string().contains(needle),
        Value::Array(list) => depth < 4 && list.iter().any(|x| has_text(x, needle, depth + 1)),
        Value::Object(map) => depth < 4 && map.values().any(|x| has_text(x, needle, depth + 1)),
        _ => false,
    }
}

fn filter_ok(item: &Value, field: &str, want: &str) -> bool {
    let Some(found) = path_get(item, field) else {
        return false;
    };
    let (contains, want) = match want.strip_prefix('~') {
        Some(rest) => (true, rest.to_lowercase()),
        None => (false, want.to_lowercase()),
    };
    let test = |x: &Value| {
        let text = text_of(x).to_lowercase();
        if contains {
            text.contains(&want)
        } else {
            text == want
        }
    };
    match found {
        Value::Array(list) => list.iter().any(test),
        other => test(other),
    }
}

fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => text_of(a).to_lowercase().cmp(&text_of(b).to_lowercase()),
    }
}

/// The items that match, sorted, and the page asked for; the first number is how many matched in all.
pub fn search<'a>(ds: &'a Dataset, query: &Query) -> (usize, Vec<&'a Value>) {
    let needle = query.q.trim().to_lowercase();
    let mut found: Vec<&'a Value> = ds
        .items
        .iter()
        .filter(|item| needle.is_empty() || has_text(item, &needle, 0))
        .filter(|item| {
            query
                .filters
                .iter()
                .all(|(field, want)| filter_ok(item, field, want))
        })
        .collect();
    if let Some(field) = &query.sort {
        found.sort_by(|a, b| match (path_get(a, field), path_get(b, field)) {
            (Some(x), Some(y)) => compare(x, y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        if query.descending {
            found.reverse();
        }
    }
    let total = found.len();
    let page = found
        .into_iter()
        .skip(query.offset)
        .take(query.limit)
        .collect();
    (total, page)
}

/// The rows of a CSV text: values separated by commas (or semicolons, when the header line has more of them), quotes with `""` inside, line breaks inside quotes.
fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first = text.lines().next().unwrap_or("");
    let delim = if first.matches(';').count() > first.matches(',').count() {
        ';'
    } else {
        ','
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == delim {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut field));
            if row.len() == 1 && row[0].is_empty() {
                row.clear();
            } else {
                rows.push(std::mem::take(&mut row));
            }
        } else {
            field.push(c);
        }
    }
    if quoted {
        return Err("a quoted value is not closed".into());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn items_from_csv(text: &str) -> Result<Vec<Value>, String> {
    let rows = parse_csv(text)?;
    let Some((header, body)) = rows.split_first() else {
        return Err("the CSV has no header line".into());
    };
    let names: Vec<String> = header.iter().map(|h| h.trim().to_string()).collect();
    let mut seen = HashSet::new();
    for name in &names {
        if name.is_empty() || !seen.insert(name.clone()) {
            return Err("the header of the CSV has an empty or repeated column".into());
        }
    }
    let mut items = Vec::with_capacity(body.len());
    for (n, row) in body.iter().enumerate() {
        if row.len() != names.len() {
            return Err(format!(
                "row {}: {} values, the header has {}",
                n + 2,
                row.len(),
                names.len()
            ));
        }
        let mut map = Map::new();
        for (name, value) in names.iter().zip(row) {
            if !value.trim().is_empty() {
                map.insert(name.clone(), Value::String(value.trim().to_string()));
            }
        }
        items.push(Value::Object(map));
    }
    Ok(items)
}

/// The items of an uploaded dataset, checked: objects, each with the field that names it, text and unique.
pub fn parse_upload(
    def: &Def,
    bytes: &[u8],
    csv: bool,
    max_items: usize,
) -> Result<(Vec<Value>, &'static str), CError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| CError::Bad("the text is not UTF-8".into()))?;
    let (items, source) = if csv {
        if def.format.is_some() {
            return Err(CError::Bad(format!(
                "{} is kept as JSON with its format field, not as CSV",
                def.name
            )));
        }
        (items_from_csv(text).map_err(CError::Bad)?, "csv")
    } else {
        let doc: Value =
            serde_json::from_str(text).map_err(|e| CError::Bad(format!("JSON: {e}")))?;
        if let Some(format) = def.format {
            if doc.get("format").and_then(Value::as_str) != Some(format) {
                return Err(CError::Bad(format!(
                    "the document has to declare \"format\": \"{format}\""
                )));
            }
        }
        let list = match &doc {
            Value::Array(list) => Some(list),
            Value::Object(_) => def
                .array_keys
                .iter()
                .find_map(|key| doc.get(*key).and_then(Value::as_array)),
            _ => None,
        };
        let list = list.ok_or_else(|| {
            CError::Bad(format!(
                "give a list of items, or an object with one of {}",
                def.array_keys.join(", ")
            ))
        })?;
        (list.clone(), "json")
    };
    if items.len() > max_items {
        return Err(CError::Bad(format!(
            "more than {max_items} items (catalog_max_items)"
        )));
    }
    let mut seen = HashSet::new();
    for (n, item) in items.iter().enumerate() {
        if !item.is_object() {
            return Err(CError::Bad(format!("item {}: not an object", n + 1)));
        }
        let id = id_of(def, item).filter(|id| !id.trim().is_empty() && id.chars().count() <= 200);
        let Some(id) = id else {
            return Err(CError::Bad(format!(
                "item {}: it needs a {} (text, up to 200 characters)",
                n + 1,
                def.id_field
            )));
        };
        if !seen.insert(id.to_string()) {
            return Err(CError::Bad(format!(
                "item {}: the {} \"{id}\" is repeated",
                n + 1,
                def.id_field
            )));
        }
    }
    Ok((items, source))
}

pub struct Catalogs {
    pub book: Mutex<CBook>,
}

impl Catalogs {
    pub fn open(cfg: &ServerConfig) -> io::Result<Catalogs> {
        Ok(Catalogs {
            book: Mutex::new(CBook::open(cfg)?),
        })
    }
}

pub struct CBook {
    dir: PathBuf,
    max_bytes: usize,
    max_items: usize,
    pub page_max: usize,
    sets: HashMap<&'static str, Dataset>,
}

/// The materials of the database that `eh3d-materials` carries.
fn materials() -> Dataset {
    let items: Vec<Value> = match eh3d_materials::Database::embedded() {
        Ok(db) => db
            .materials()
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect(),
        Err(e) => {
            eprintln!("eh3d-server: the database of materials does not load ({e:?}); the catalog is empty");
            Vec::new()
        }
    };
    let def = &DEFS[0];
    Dataset::build(
        def,
        Stored {
            version: 1,
            sha256: sha_of_items(&items),
            updated: 0,
            by: String::new(),
            source: "embedded".into(),
            items,
        },
    )
}

impl CBook {
    pub fn open(cfg: &ServerConfig) -> io::Result<CBook> {
        let dir = cfg.data_dir.join("catalogs");
        let mut sets = HashMap::new();
        for def in DEFS {
            let ds = if def.builtin {
                materials()
            } else {
                match store::read_json::<Stored>(&dir.join(format!("{}.json", def.name)))? {
                    Some(stored) => Dataset::build(def, stored),
                    None => Dataset::empty(),
                }
            };
            sets.insert(def.name, ds);
        }
        Ok(CBook {
            dir,
            max_bytes: cfg.catalog_max_mb as usize * 1024 * 1024,
            max_items: cfg.catalog_max_items,
            page_max: cfg.catalog_page_max,
            sets,
        })
    }

    pub fn list(&self) -> Vec<Value> {
        DEFS.iter()
            .filter_map(|def| self.sets.get(def.name).map(|ds| ds.summary(def)))
            .collect()
    }

    pub fn dataset(&self, name: &str) -> Result<(&'static Def, &Dataset), CError> {
        let def = def_of(name).ok_or(CError::Unknown)?;
        let ds = self.sets.get(def.name).ok_or(CError::Unknown)?;
        Ok((def, ds))
    }

    /// Replaces the dataset of a catalog by the one uploaded, as a new version. The summary of the new dataset.
    pub fn replace(
        &mut self,
        name: &str,
        bytes: &[u8],
        csv: bool,
        by: &str,
        at: u64,
    ) -> Result<Value, CError> {
        let def = def_of(name).ok_or(CError::Unknown)?;
        if def.builtin {
            return Err(CError::ReadOnly);
        }
        if bytes.len() > self.max_bytes {
            return Err(CError::TooBig);
        }
        let (items, source) = parse_upload(def, bytes, csv, self.max_items)?;
        self.install(def, items, source, by, at)
    }

    /// Empties the dataset of a catalog (a new version with no items).
    pub fn clear(&mut self, name: &str, by: &str, at: u64) -> Result<Value, CError> {
        let def = def_of(name).ok_or(CError::Unknown)?;
        if def.builtin {
            return Err(CError::ReadOnly);
        }
        self.install(def, Vec::new(), "json", by, at)
    }

    fn install(
        &mut self,
        def: &'static Def,
        items: Vec<Value>,
        source: &str,
        by: &str,
        at: u64,
    ) -> Result<Value, CError> {
        let version = self.sets.get(def.name).map_or(0, |d| d.version) + 1;
        let stored = Stored {
            version,
            sha256: sha_of_items(&items),
            updated: at,
            by: by.to_string(),
            source: source.to_string(),
            items,
        };
        let bytes = serde_json::to_vec(&stored).map_err(|_| CError::Storage)?;
        store::write_durable(&self.dir.join(format!("{}.json", def.name)), &bytes, true)
            .map_err(|_| CError::Storage)?;
        let ds = Dataset::build(def, stored);
        let summary = ds.summary(def);
        self.sets.insert(def.name, ds);
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eh3d-server-cat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn book(name: &str) -> (CBook, PathBuf) {
        let d = dir(name);
        let cfg = ServerConfig {
            data_dir: d.clone(),
            ..ServerConfig::default()
        };
        (CBook::open(&cfg).unwrap(), d)
    }

    fn query() -> Query {
        Query {
            limit: 100,
            ..Query::default()
        }
    }

    #[test]
    fn a_csv_is_read_with_quotes_semicolons_and_a_bom() {
        let rows =
            parse_csv("\u{feff}id,name,note\r\nt1,\"Drill, 0.8\",\"say \"\"hi\"\"\"\n\nt2,Mill,\n")
                .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], vec!["t1", "Drill, 0.8", "say \"hi\""]);
        assert_eq!(rows[2], vec!["t2", "Mill", ""]);
        let semi = parse_csv("id;name\nt1;Drill\n").unwrap();
        assert_eq!(semi[1], vec!["t1", "Drill"]);
        assert!(parse_csv("id,name\n\"open,x\n").is_err());
    }

    #[test]
    fn an_upload_needs_items_with_an_id_that_is_a_text_and_unique() {
        let tools = def_of("tools").unwrap();
        let ok = parse_upload(
            tools,
            br#"{"tools":[{"id":"a"},{"id":"b","dia":0.8}]}"#,
            false,
            10,
        )
        .unwrap();
        assert_eq!((ok.0.len(), ok.1), (2, "json"));
        assert!(parse_upload(tools, br#"[{"id":"a"}]"#, false, 10).is_ok());
        for bad in [
            r#"{"tools":[{"id":"a"},{"id":"a"}]}"#,
            r#"{"tools":[{"name":"a"}]}"#,
            r#"{"tools":[{"id":5}]}"#,
            r#"{"tools":["a"]}"#,
            r#"{"other":[]}"#,
            "[1]",
            "not json",
        ] {
            assert!(
                matches!(
                    parse_upload(tools, bad.as_bytes(), false, 10),
                    Err(CError::Bad(_))
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            parse_upload(tools, br#"[{"id":"a"},{"id":"b"}]"#, false, 1),
            Err(CError::Bad(_))
        ));
        // the footprints are named by `name`
        let foot = def_of("footprints").unwrap();
        assert!(parse_upload(foot, b"name,pads\nR0603,2\nC0603,2\n", true, 10).is_ok());
        assert!(parse_upload(foot, b"id,pads\nR0603,2\n", true, 10).is_err());
        // the components base declares its format and is not a CSV here
        let parts = def_of("components").unwrap();
        assert!(parse_upload(
            parts,
            br#"{"format":"phoenix-parts/1","parts":[{"id":"r-10k"}]}"#,
            false,
            10
        )
        .is_ok());
        assert!(parse_upload(parts, br#"{"parts":[{"id":"r-10k"}]}"#, false, 10).is_err());
        assert!(parse_upload(parts, b"id\nr-10k\n", true, 10).is_err());
    }

    #[test]
    fn the_search_filters_sorts_and_pages() {
        let (mut b, d) = book("search");
        b.replace(
            "tools",
            br#"[{"id":"d08","kind":"drill","dia":0.8,"tags":["fr4","hss"]},
                 {"id":"d10","kind":"drill","dia":1.0,"tags":["alu"]},
                 {"id":"m20","kind":"mill","dia":2.0,"tags":["fr4"]}]"#,
            false,
            "juan",
            5,
        )
        .unwrap();
        let (_, ds) = b.dataset("tools").unwrap();
        let (total, page) = search(ds, &query());
        assert_eq!((total, page.len()), (3, 3));
        let by_text = Query {
            q: "MILL".into(),
            ..query()
        };
        assert_eq!(search(ds, &by_text).0, 1);
        let drills = Query {
            filters: vec![("kind".into(), "Drill".into())],
            sort: Some("dia".into()),
            descending: true,
            ..query()
        };
        let (n, page) = search(ds, &drills);
        assert_eq!(n, 2);
        assert_eq!(page[0]["id"], "d10");
        // a filter on a list holds if one of the values does; ~ is "contains"
        let tag = Query {
            filters: vec![("tags".into(), "fr4".into())],
            ..query()
        };
        assert_eq!(search(ds, &tag).0, 2);
        let part = Query {
            filters: vec![("id".into(), "~d".into())],
            ..query()
        };
        assert_eq!(search(ds, &part).0, 2);
        let paged = Query {
            offset: 1,
            limit: 1,
            sort: Some("id".into()),
            ..Query::default()
        };
        let (n, page) = search(ds, &paged);
        assert_eq!((n, page.len(), page[0]["id"].as_str()), (3, 1, Some("d10")));
        assert_eq!(ds.get("m20").unwrap()["kind"], "mill");
        assert!(ds.get("zzz").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_dataset_is_a_new_version_and_the_built_in_one_is_not_written() {
        let (mut b, d) = book("versions");
        let (_, empty) = b.dataset("tools").unwrap();
        assert_eq!(empty.version, 0);
        let first = b
            .replace("tools", br#"[{"id":"a"}]"#, false, "juan", 5)
            .unwrap();
        assert_eq!(first["version"], 1);
        let tag_one = b.dataset("tools").unwrap().1.etag("tools");
        let second = b
            .replace("tools", b"id,dia\na,0.8\nb,1.0\n", true, "juan", 6)
            .unwrap();
        assert_eq!(
            (second["version"].as_u64(), second["items"].as_u64()),
            (Some(2), Some(2))
        );
        assert_eq!(second["source"], "csv");
        assert_ne!(b.dataset("tools").unwrap().1.etag("tools"), tag_one);
        // the same text again is a new version with the same tag: the tag is of the content
        let third = b.clear("tools", "juan", 7).unwrap();
        assert_eq!(third["items"], 0);
        assert_eq!(
            b.replace("materials", b"[]", false, "juan", 8),
            Err(CError::ReadOnly)
        );
        assert_eq!(b.clear("materials", "juan", 8), Err(CError::ReadOnly));
        assert_eq!(
            b.replace("nothing", b"[]", false, "juan", 8),
            Err(CError::Unknown)
        );
        assert!(matches!(b.dataset("nothing"), Err(CError::Unknown)));
        // read again from the disk
        let cfg = ServerConfig {
            data_dir: d.clone(),
            ..ServerConfig::default()
        };
        let again = CBook::open(&cfg).unwrap();
        assert_eq!(again.dataset("tools").unwrap().1.version, 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_materials_of_the_database_are_there() {
        let (b, d) = book("materials");
        let (def, ds) = b.dataset("materials").unwrap();
        assert!(def.builtin);
        assert!(!ds.items.is_empty());
        let first = ds.items[0]["id"].as_str().unwrap().to_string();
        assert!(ds.get(&first).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }
}
