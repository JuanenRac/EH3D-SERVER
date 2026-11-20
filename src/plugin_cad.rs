//! The `cad` plugin: checks and reports of the project file of EH3D-PHOENIX-CAD, made by a simulated station. There is no geometry kernel here: the station reads the structure of the
//! file (the parts, the sketches and the features and how they point to one another) and counts and checks that, so a program is tried against the whole chain (a project, a check,
//! a report as an artifact) without the program that draws. It says `evidence: simulator` everywhere.
//!
//! **Where a project lives.** PHOENIX-CAD keeps its project in the store of projects of the server under the application `phoenix-cad` (`/api/v1/apps/phoenix-cad/projects/{id}/files/
//! content?name=project.json`), like every program of the family: a file that exists is written only with `If-Match` (`428 precondition_required` without it, `412 precondition_failed`
//! when it is not the version the program read), so two people never overwrite each other; the same store gives the versions, the lock and the `.eh3dproj` container. This plugin
//! does not keep projects of its own. A request names the file as an artifact (`artifact`) or carries it in `project`.
//!
//! **The project file** is JSON: `{"schema": "eh3d.cad.project/1", "name", "units": "mm" | "in", "sketches": [{"id"}], "features": [{"id", "kind", "sketch", "target"}], "parts":
//! [{"id", "name", "features": ["<feature id>"]}]}`. A feature of kind `extrude`, `revolve`, `pocket` or `hole` points to the `sketch` it is made from; a `fillet`, `chamfer`, `mirror` or
//! `pattern` points to the feature it works on in `target`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, PluginError, PluginResult,
    PluginStatus, Prepared,
};
use crate::simjobs::{evenly, known_keys, number_in, SimBook};
use crate::state::AppState;

/// The `schema` of a project.
pub const PROJECT_SCHEMA: &str = "eh3d.cad.project/1";
/// The `schema` of a report.
pub const REPORT_SCHEMA: &str = "eh3d.cad.report/1";
/// The kinds of feature that are made from a sketch.
const FROM_SKETCH: &[&str] = &["extrude", "revolve", "pocket", "hole"];
/// The kinds of feature that work on another feature.
const ON_FEATURE: &[&str] = &["fillet", "chamfer", "mirror", "pattern"];
/// The settings of a station: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("max_items", "the most parts, sketches and features a project may have to be read, 1 to 100000 (5000 by default)"),
    ("sim_rate", "how many times faster than real time the simulated station works, 0 to 100000 (1 by default; 0 is no waiting)"),
];
/// The biggest project file the station reads, in bytes.
const MAX_FILE: usize = 4 * 1024 * 1024;

/// The settings of a station, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct CadConfig {
    pub max_items: usize,
    pub sim_rate: f64,
}

impl Default for CadConfig {
    fn default() -> Self {
        CadConfig {
            max_items: 5000,
            sim_rate: 1.0,
        }
    }
}

impl CadConfig {
    pub fn from_settings(settings: &Value) -> Result<CadConfig, String> {
        let keys: Vec<&str> = SETTINGS_HELP.iter().map(|(k, _)| *k).collect();
        known_keys(settings, &keys, "a CAD station")?;
        let mut c = CadConfig::default();
        if let Some(n) = number_in(settings, "max_items", 1.0, 100_000.0)? {
            c.max_items = n as usize;
        }
        if let Some(n) = number_in(settings, "sim_rate", 0.0, 100_000.0)? {
            c.sim_rate = n;
        }
        Ok(c)
    }
}

/// One thing the check found.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// `error` or `warning`.
    pub severity: &'static str,
    pub code: &'static str,
    pub id: String,
    pub message: String,
}

impl Issue {
    fn error(code: &'static str, id: &str, message: String) -> Issue {
        Issue {
            severity: "error",
            code,
            id: id.to_string(),
            message,
        }
    }

    fn warning(code: &'static str, id: &str, message: String) -> Issue {
        Issue {
            severity: "warning",
            code,
            id: id.to_string(),
            message,
        }
    }

    fn view(&self) -> Value {
        json!({"severity": self.severity, "code": self.code, "id": self.id, "message": self.message})
    }
}

fn list<'a>(project: &'a Value, key: &str) -> Vec<&'a Value> {
    project
        .get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn id_of(item: &Value) -> Option<&str> {
    item.get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Checks the structure of a project. The ids of each list are different, a feature points to a sketch or to a feature that is there and a part to features that are there.
pub fn check_project(project: &Value) -> Vec<Issue> {
    let mut issues = Vec::new();
    if project.get("schema").and_then(Value::as_str) != Some(PROJECT_SCHEMA) {
        issues.push(Issue::error(
            "bad_schema",
            "",
            format!("schema is {PROJECT_SCHEMA}"),
        ));
    }
    if let Some(units) = project.get("units") {
        if !matches!(units.as_str(), Some("mm" | "in")) {
            issues.push(Issue::error(
                "unknown_units",
                "",
                "units is mm or in".into(),
            ));
        }
    }
    let mut ids: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for key in ["sketches", "features", "parts"] {
        let seen = ids.entry(key).or_default();
        for item in list(project, key) {
            match id_of(item) {
                None => issues.push(Issue::error(
                    "missing_id",
                    "",
                    format!("an item of {key} has no id"),
                )),
                Some(id) => {
                    if !seen.insert(id) {
                        issues.push(Issue::error(
                            "duplicate_id",
                            id,
                            format!("{id} is twice in {key}"),
                        ));
                    }
                }
            }
        }
    }
    let empty = BTreeSet::new();
    let sketches = ids.get("sketches").unwrap_or(&empty);
    let features = ids.get("features").unwrap_or(&empty);
    for f in list(project, "features") {
        let id = id_of(f).unwrap_or("");
        let kind = f.get("kind").and_then(Value::as_str).unwrap_or("");
        if FROM_SKETCH.contains(&kind) {
            match f.get("sketch").and_then(Value::as_str) {
                Some(s) if sketches.contains(s) => {}
                other => issues.push(Issue::error(
                    "missing_ref",
                    id,
                    format!(
                        "{id} ({kind}) points to the sketch {} and there is none",
                        other.unwrap_or("(none)")
                    ),
                )),
            }
        } else if ON_FEATURE.contains(&kind) {
            match f.get("target").and_then(Value::as_str) {
                Some(t) if features.contains(t) && t != id => {}
                other => issues.push(Issue::error(
                    "missing_ref",
                    id,
                    format!(
                        "{id} ({kind}) works on the feature {} and there is none",
                        other.unwrap_or("(none)")
                    ),
                )),
            }
        } else {
            issues.push(Issue::warning(
                "unknown_kind",
                id,
                format!("{id} is of the kind {kind:?}, which the station does not know"),
            ));
        }
    }
    for p in list(project, "parts") {
        let id = id_of(p).unwrap_or("");
        let used: Vec<&str> = p
            .get("features")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if used.is_empty() {
            issues.push(Issue::warning(
                "empty_part",
                id,
                format!("the part {id} has no feature"),
            ));
        }
        for f in used {
            if !features.contains(f) {
                issues.push(Issue::error(
                    "missing_ref",
                    id,
                    format!("the part {id} uses the feature {f} and there is none"),
                ));
            }
        }
    }
    issues
}

/// The verdict of a list of issues: `pass` while there is no error.
fn verdict(issues: &[Issue]) -> &'static str {
    if issues.iter().any(|i| i.severity == "error") {
        "fail"
    } else {
        "pass"
    }
}

/// The project a request carries (`project`) or names (`artifact`, whose text the server gave in `artifact_text`).
fn project_of(request: &Value, cfg: &CadConfig) -> PluginResult<Value> {
    let project = match (request.get("project"), request.get("artifact_text")) {
        (Some(p), None) if p.is_object() => p.clone(),
        (None, Some(Value::String(text))) => {
            if text.len() > MAX_FILE {
                return Err(PluginError::Bad(format!(
                    "the project is bigger than {} MiB",
                    MAX_FILE / (1024 * 1024)
                )));
            }
            serde_json::from_str(text)
                .map_err(|e| PluginError::Bad(format!("the project is not JSON: {e}")))?
        }
        (Some(_), Some(_)) => {
            return Err(PluginError::Bad(
                "the project is in `project` or in an artifact, not in both".into(),
            ))
        }
        _ => {
            return Err(PluginError::Bad(
                "a request carries the project in `project` or names it as an `artifact`".into(),
            ))
        }
    };
    if !project.is_object() {
        return Err(PluginError::Bad("the project is a JSON object".into()));
    }
    let items: usize = ["sketches", "features", "parts"]
        .into_iter()
        .map(|k| list(&project, k).len())
        .sum();
    if items > cfg.max_items {
        return Err(PluginError::Bad(format!(
            "the project has {items} parts, sketches and features, more than the {} the station reads",
            cfg.max_items
        )));
    }
    Ok(project)
}

/// The document a request gives: the verdict of the check, or the report with the counts, the list of parts and the check.
pub fn run(request: &Value, cfg: &CadConfig) -> PluginResult<Value> {
    let kind = request.get("kind").and_then(Value::as_str);
    if !matches!(kind, Some("check" | "report")) {
        return Err(PluginError::Bad("kind is check or report".into()));
    }
    let project = project_of(request, cfg)?;
    let issues = check_project(&project);
    let check = json!({
        "verdict": verdict(&issues),
        "errors": issues.iter().filter(|i| i.severity == "error").count(),
        "warnings": issues.iter().filter(|i| i.severity == "warning").count(),
        "issues": issues.iter().map(Issue::view).collect::<Vec<_>>(),
    });
    if kind == Some("check") {
        return Ok(
            json!({"schema": REPORT_SCHEMA, "kind": "check", "check": check, "evidence": "simulator"}),
        );
    }
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    for f in list(&project, "features") {
        let k = f.get("kind").and_then(Value::as_str).unwrap_or("unknown");
        *by_kind.entry(k.to_string()).or_default() += 1;
    }
    let parts: Vec<Value> = list(&project, "parts")
        .into_iter()
        .map(|p| {
            json!({
                "id": id_of(p).unwrap_or(""),
                "name": p.get("name").and_then(Value::as_str).unwrap_or(""),
                "features": p.get("features").and_then(Value::as_array).map_or(0, Vec::len),
            })
        })
        .collect();
    Ok(json!({
        "schema": REPORT_SCHEMA,
        "kind": "report",
        "name": project.get("name").and_then(Value::as_str).unwrap_or(""),
        "units": project.get("units").and_then(Value::as_str).unwrap_or("mm"),
        "counts": {
            "parts": parts.len(),
            "sketches": list(&project, "sketches").len(),
            "features": list(&project, "features").len(),
            "features_by_kind": by_kind,
        },
        "parts": parts,
        "check": check,
        "evidence": "simulator",
        "note": "the station counts and checks the file; it has no geometry kernel, so there is no volume, mass or box",
    }))
}

/// The stations of the server.
#[derive(Default)]
pub struct CadPlugin {
    book: SimBook,
}

impl CadPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<CadConfig> {
        let settings =
            work_machine_settings(st, machine, "cad").ok_or(PluginError::UnknownMachine)?;
        CadConfig::from_settings(&settings).map_err(PluginError::Bad)
    }
}

impl MachinePlugin for CadPlugin {
    fn kind(&self) -> &'static str {
        "cad"
    }

    fn title(&self) -> &'static str {
        "CAD stations: checks and reports of the project file of PHOENIX-CAD (the structure of the file; no geometry kernel), kept as artifacts (simulation)"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        json!({
            "requests": ["check", "report"],
            "pause": "all",
            "confirm": false,
            "stop": true,
            "progress": "items",
            "moves": false,
            "settings": settings,
            "project": {
                "schema": PROJECT_SCHEMA,
                "store": "/api/v1/apps/phoenix-cad/projects/{id}/files/content?name=project.json",
                "write": "a file that exists is written with If-Match: 428 precondition_required without it, 412 precondition_failed when it is not the version that was read",
                "request": "the project in `project`, or an `artifact` that holds the file",
            },
            "result": {
                "schema": REPORT_SCHEMA,
                "artifact_name": "cad-check.json or cad-report.json",
                "format": "application/json",
                "output": "a request with \"output\": \"artifact\" keeps the result in the store of the caller under that name",
            },
            "evidence": "simulator",
        })
    }

    fn manages_machines(&self) -> bool {
        true
    }

    fn check_settings(&self, settings: &Value) -> PluginResult<()> {
        CadConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "cad")
            .into_iter()
            .map(|(id, name)| {
                let state = if self.book.busy(&id) { "busy" } else { "idle" };
                MachineInfo { id, name, state }
            })
            .collect()
    }

    fn detail(&self, _st: &Arc<AppState>, _machine: &str) -> Value {
        json!({"project_schema": PROJECT_SCHEMA, "report_schema": REPORT_SCHEMA})
    }

    fn prepare(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        request: &Value,
        _by: &str,
    ) -> PluginResult<Prepared> {
        let cfg = Self::config(st, machine)?;
        let result = run(request, &cfg)?;
        let kind = result["kind"].as_str().unwrap_or("check").to_string();
        let items = result["check"]["issues"]
            .as_array()
            .map_or(1, Vec::len)
            .max(1);
        let native_id = self.book.add(
            machine,
            if kind == "report" { "report" } else { "check" },
            evenly(items, 0.02),
            cfg.sim_rate,
        );
        Ok(Prepared {
            native_id,
            view: json!({
                "kind": kind,
                "artifact_name": format!("cad-{kind}.json"),
                "verdict": result["check"]["verdict"],
                "result": result,
            }),
        })
    }

    fn start(
        &self,
        _st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        self.book.start(machine, native_id)
    }

    fn pause(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str) -> PluginResult<()> {
        self.book.pause(native_id)
    }

    fn resume(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str) -> PluginResult<()> {
        self.book.resume(native_id)
    }

    fn stop(
        &self,
        _st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        self.book.stop(native_id, "stopped").map(|_| ())
    }

    fn status(
        &self,
        _st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
    ) -> PluginResult<PluginStatus> {
        self.book.status(native_id)
    }

    fn discard(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str, _by: &str) {
        self.book.discard(native_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Value {
        json!({
            "schema": PROJECT_SCHEMA, "name": "bracket", "units": "mm",
            "sketches": [{"id": "s1"}, {"id": "s2"}],
            "features": [
                {"id": "f1", "kind": "extrude", "sketch": "s1"},
                {"id": "f2", "kind": "hole", "sketch": "s2"},
                {"id": "f3", "kind": "fillet", "target": "f1"},
            ],
            "parts": [{"id": "p1", "name": "Bracket", "features": ["f1", "f2", "f3"]}],
        })
    }

    fn codes(project: &Value) -> Vec<&'static str> {
        check_project(project).iter().map(|i| i.code).collect()
    }

    #[test]
    fn the_settings_are_checked_and_a_typo_is_refused() {
        assert_eq!(
            CadConfig::from_settings(&json!({})).unwrap(),
            CadConfig::default()
        );
        let c = CadConfig::from_settings(&json!({"max_items": 3, "sim_rate": 0})).unwrap();
        assert_eq!((c.max_items, c.sim_rate), (3, 0.0));
        for bad in [
            json!({"max_items": 0}),
            json!({"max_items": 100001}),
            json!({"sim_rate": "fast"}),
            json!({"kernel": 1}),
        ] {
            assert!(CadConfig::from_settings(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_project_that_is_well_made_passes_and_the_report_counts_it() {
        assert!(check_project(&project()).is_empty());
        let report = run(
            &json!({"kind": "report", "project": project()}),
            &CadConfig::default(),
        )
        .unwrap();
        assert_eq!(report["schema"], REPORT_SCHEMA);
        assert_eq!(report["check"]["verdict"], "pass");
        assert_eq!(report["counts"]["parts"], 1);
        assert_eq!(report["counts"]["sketches"], 2);
        assert_eq!(report["counts"]["features"], 3);
        assert_eq!(report["counts"]["features_by_kind"]["extrude"], 1);
        assert_eq!(report["parts"][0]["features"], 3);
        assert_eq!(report["units"], "mm");
        assert_eq!(report["evidence"], "simulator");
        let check = run(
            &json!({"kind": "check", "project": project()}),
            &CadConfig::default(),
        )
        .unwrap();
        assert_eq!(check["check"]["verdict"], "pass");
        assert!(check.get("counts").is_none());
    }

    #[test]
    fn the_check_finds_what_is_wrong_in_the_structure() {
        let mut p = project();
        p["schema"] = json!("other");
        p["units"] = json!("cm");
        assert_eq!(codes(&p), vec!["bad_schema", "unknown_units"]);
        let mut p = project();
        p["features"][1]["sketch"] = json!("s9");
        p["features"][2]["target"] = json!("f3");
        p["sketches"][1]["id"] = json!("s1");
        assert_eq!(
            codes(&p),
            vec!["duplicate_id", "missing_ref", "missing_ref"]
        );
        let mut p = project();
        p["features"][0]["kind"] = json!("loft");
        p["parts"][0]["features"] = json!(["f9"]);
        p["parts"].as_array_mut().unwrap().push(json!({"id": "p2"}));
        assert_eq!(codes(&p), vec!["unknown_kind", "missing_ref", "empty_part"]);
        let mut p = project();
        p["parts"][0].as_object_mut().unwrap().remove("id");
        assert_eq!(codes(&p), vec!["missing_id"]);
        let report = run(
            &json!({"kind": "check", "project": p}),
            &CadConfig::default(),
        )
        .unwrap();
        assert_eq!(report["check"]["verdict"], "fail");
        assert_eq!(report["check"]["errors"], 1);
    }

    #[test]
    fn the_project_comes_in_the_request_or_as_the_text_of_an_artifact_and_nowhere_else() {
        let cfg = CadConfig::default();
        let text = project().to_string();
        let from_artifact = run(&json!({"kind": "check", "artifact_text": text}), &cfg).unwrap();
        assert_eq!(from_artifact["check"]["verdict"], "pass");
        for bad in [
            json!({"kind": "check"}),
            json!({"kind": "check", "project": project(), "artifact_text": text}),
            json!({"kind": "check", "project": "text"}),
            json!({"kind": "check", "artifact_text": "{nope"}),
            json!({"kind": "check", "artifact_text": "[1]"}),
            json!({"kind": "weigh", "project": project()}),
            json!({"project": project()}),
        ] {
            assert!(matches!(run(&bad, &cfg), Err(PluginError::Bad(_))), "{bad}");
        }
        let small = CadConfig {
            max_items: 3,
            ..CadConfig::default()
        };
        let refused = run(&json!({"kind": "check", "project": project()}), &small).unwrap_err();
        assert!(matches!(refused, PluginError::Bad(why) if why.contains("more than the 3")));
    }
}
