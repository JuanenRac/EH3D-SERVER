//! The `vision` plugin: the results of the camera programs (EH3D-VISION-STUDIO and the programs that look through a camera) as artifacts of the store, and a station that makes
//! them up in the simulator. Nothing here looks at a picture: the station is given the size and the hash of a picture and gives findings that depend on them and on a seed, always
//! the same for the same input, so a program is tried against the whole chain (job, result, artifact) without a camera. It says `evidence: simulator` everywhere.
//!
//! **The convention of a result.** A program that looks at a picture keeps its answer in the store of artifacts as a JSON file named `vision-<kind>.json` (`kind` is one of
//! [`KINDS`]), of format `application/json` and `schema_version` [`RESULT_SCHEMA`], with this shape (`schema` is [`RESULT_SCHEMA`]):
//! `{"schema", "kind", "source": {"width", "height", "sha256"}, "findings": [...], "units": "px", "evidence", "producer": {"tool", "version"}}`. The findings of each kind are in
//! `docs/MACHINE_PLUGINS.md`. The station of this plugin makes the same document, so a client reads the result of a camera program and the one of the simulator the same way; the
//! programs keep sending their own `vision-<kind>.json` to the store, as they do now, and they can move to ask the station nothing at all.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::plugins::{
    work_machine_ids, work_machine_settings, MachineInfo, MachinePlugin, PluginError, PluginResult,
    PluginStatus, Prepared,
};
use crate::simjobs::{evenly, known_keys, number_in, SimBook};
use crate::state::AppState;

/// The `schema` of a result.
pub const RESULT_SCHEMA: &str = "eh3d.vision.result/1";
/// The kinds of result.
pub const KINDS: &[&str] = &["fiducials", "barcode", "focus", "blobs"];
/// The settings of a station: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("max_findings", "the most findings a result has, 1 to 1000 (50 by default)"),
    ("sim_rate", "how many times faster than real time the simulated station works, 0 to 100000 (1 by default; 0 is no waiting)"),
];

/// The settings of a station, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct VisionConfig {
    pub max_findings: usize,
    pub sim_rate: f64,
}

impl Default for VisionConfig {
    fn default() -> Self {
        VisionConfig {
            max_findings: 50,
            sim_rate: 1.0,
        }
    }
}

impl VisionConfig {
    pub fn from_settings(settings: &Value) -> Result<VisionConfig, String> {
        let keys: Vec<&str> = SETTINGS_HELP.iter().map(|(k, _)| *k).collect();
        known_keys(settings, &keys, "a vision station")?;
        let mut c = VisionConfig::default();
        if let Some(n) = number_in(settings, "max_findings", 1.0, 1000.0)? {
            c.max_findings = n as usize;
        }
        if let Some(n) = number_in(settings, "sim_rate", 0.0, 100_000.0)? {
            c.sim_rate = n;
        }
        Ok(c)
    }
}

/// A hash of the pieces, the same on every computer.
fn mix(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in part.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h | 1
}

/// The next number of a sequence that starts from `h`.
fn next(h: &mut u64) -> u64 {
    *h ^= *h << 13;
    *h ^= *h >> 7;
    *h ^= *h << 17;
    *h
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The size of the picture a request names.
fn dimension(image: &Value, key: &str) -> Result<u64, PluginError> {
    image
        .get(key)
        .and_then(Value::as_u64)
        .filter(|v| (1..=20_000).contains(v))
        .ok_or_else(|| {
            PluginError::Bad(format!(
                "image.{key} is a whole number of pixels from 1 to 20000"
            ))
        })
}

/// The result of a request: the document a client reads, made up from the size and the hash of the picture and the seed.
pub fn detect(request: &Value, cfg: &VisionConfig) -> PluginResult<Value> {
    if request.get("kind").and_then(Value::as_str) != Some("detect") {
        return Err(PluginError::Bad(
            "the only kind of request is `detect`".into(),
        ));
    }
    let what = request
        .get("what")
        .and_then(Value::as_str)
        .filter(|w| KINDS.contains(w))
        .ok_or_else(|| PluginError::Bad(format!("what is one of {}", KINDS.join(", "))))?;
    let image = request
        .get("image")
        .filter(|i| i.is_object())
        .ok_or_else(|| PluginError::Bad("image is {\"width\", \"height\", \"sha256\"}: the size of the picture and, if there is one, its hash".into()))?;
    let (width, height) = (dimension(image, "width")?, dimension(image, "height")?);
    let sha = match image.get("sha256") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if crate::artifacts::is_sha256(s) => Some(s.as_str()),
        Some(_) => {
            return Err(PluginError::Bad(
                "image.sha256 is 64 hexadecimal digits in lower case".into(),
            ))
        }
    };
    let seed = match request.get("seed") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| PluginError::Bad("seed is a whole number".into()))?,
    };
    let (w, h, s) = (width.to_string(), height.to_string(), seed.to_string());
    let mut state = mix(&[what, &w, &h, sha.unwrap_or(""), &s]);
    let point = |state: &mut u64| ((next(state) % width) as f64, (next(state) % height) as f64);
    let mut findings: Vec<Value> = Vec::new();
    match what {
        "fiducials" => {
            for _ in 0..(1 + next(&mut state) % 3) {
                let (x, y) = point(&mut state);
                findings.push(json!({
                    "x": x, "y": y, "radius_px": 12 + next(&mut state) % 17,
                    "score": round2(0.8 + (next(&mut state) % 20) as f64 / 100.0),
                }));
            }
        }
        "barcode" => {
            let (x, y) = point(&mut state);
            findings.push(json!({
                "format": "code128", "text": format!("SIM-{:08x}", next(&mut state) as u32), "x": x, "y": y,
            }));
        }
        "focus" => {
            let score = (next(&mut state) % 1000) as f64 / 10.0;
            findings.push(json!({"score": score, "sharp": score >= 50.0}));
        }
        _ => {
            for _ in 0..(1 + next(&mut state) % 12) {
                let (x, y) = point(&mut state);
                findings.push(json!({"x": x, "y": y, "area_px": 20 + next(&mut state) % 4000}));
            }
        }
    }
    findings.truncate(cfg.max_findings);
    Ok(json!({
        "schema": RESULT_SCHEMA,
        "kind": what,
        "source": {"width": width, "height": height, "sha256": sha},
        "findings": findings,
        "units": "px",
        "evidence": "simulator",
        "producer": {"tool": "eh3d-server vision", "version": env!("CARGO_PKG_VERSION")},
    }))
}

/// The stations of the server.
#[derive(Default)]
pub struct VisionPlugin {
    book: SimBook,
}

impl VisionPlugin {
    fn config(st: &Arc<AppState>, machine: &str) -> PluginResult<VisionConfig> {
        let settings =
            work_machine_settings(st, machine, "vision").ok_or(PluginError::UnknownMachine)?;
        VisionConfig::from_settings(&settings).map_err(PluginError::Bad)
    }
}

impl MachinePlugin for VisionPlugin {
    fn kind(&self) -> &'static str {
        "vision"
    }

    fn title(&self) -> &'static str {
        "Vision stations: the results of the camera programs as artifacts (vision-<kind>.json) and a simulated station that makes them up from the size and the hash of a picture"
    }

    fn capabilities(&self) -> Value {
        let mut settings = Map::new();
        for (key, help) in SETTINGS_HELP {
            settings.insert((*key).to_string(), json!(help));
        }
        json!({
            "requests": ["detect"],
            "kinds": KINDS,
            "pause": "all",
            "confirm": false,
            "stop": true,
            "progress": "findings",
            "moves": false,
            "settings": settings,
            "result": {
                "schema": RESULT_SCHEMA,
                "artifact_name": "vision-<kind>.json",
                "format": "application/json",
                "schema_version": RESULT_SCHEMA,
                "output": "a request with \"output\": \"artifact\" keeps the result in the store of the caller under that name",
            },
            "evidence": "simulator",
        })
    }

    fn manages_machines(&self) -> bool {
        true
    }

    fn check_settings(&self, settings: &Value) -> PluginResult<()> {
        VisionConfig::from_settings(settings)
            .map(|_| ())
            .map_err(PluginError::Bad)
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        work_machine_ids(st, "vision")
            .into_iter()
            .map(|(id, name)| {
                let state = if self.book.busy(&id) { "busy" } else { "idle" };
                MachineInfo { id, name, state }
            })
            .collect()
    }

    fn detail(&self, _st: &Arc<AppState>, _machine: &str) -> Value {
        json!({"kinds": KINDS, "schema": RESULT_SCHEMA})
    }

    fn prepare(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        request: &Value,
        _by: &str,
    ) -> PluginResult<Prepared> {
        let cfg = Self::config(st, machine)?;
        let result = detect(request, &cfg)?;
        let findings = result["findings"].as_array().map_or(1, Vec::len).max(1);
        let what = result["kind"].as_str().unwrap_or("fiducials").to_string();
        let native_id = self
            .book
            .add(machine, "detect", evenly(findings, 0.02), cfg.sim_rate);
        Ok(Prepared {
            native_id,
            view: json!({
                "kind": "detect",
                "what": what,
                "artifact_name": format!("vision-{what}.json"),
                "findings": findings,
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

    fn request(what: &str, seed: u64) -> Value {
        json!({"kind": "detect", "what": what, "image": {"width": 1280, "height": 960, "sha256": "ab".repeat(32)}, "seed": seed})
    }

    #[test]
    fn the_settings_are_checked_and_a_typo_is_refused() {
        assert_eq!(
            VisionConfig::from_settings(&json!({})).unwrap(),
            VisionConfig::default()
        );
        let c = VisionConfig::from_settings(&json!({"max_findings": 2, "sim_rate": 0})).unwrap();
        assert_eq!((c.max_findings, c.sim_rate), (2, 0.0));
        for bad in [
            json!({"max_findings": 0}),
            json!({"max_findings": 1001}),
            json!({"sim_rate": -1}),
            json!({"zoom": 2}),
        ] {
            assert!(VisionConfig::from_settings(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_result_has_the_shape_of_the_convention_and_is_the_same_for_the_same_input() {
        let cfg = VisionConfig::default();
        for what in KINDS {
            let a = detect(&request(what, 3), &cfg).unwrap();
            assert_eq!(a, detect(&request(what, 3), &cfg).unwrap(), "{what}");
            assert_ne!(
                a["findings"],
                detect(&request(what, 4), &cfg).unwrap()["findings"],
                "{what}"
            );
            assert_eq!(a["schema"], RESULT_SCHEMA);
            assert_eq!(a["kind"], *what);
            assert_eq!(a["units"], "px");
            assert_eq!(a["evidence"], "simulator");
            assert_eq!(a["source"]["width"], 1280);
            assert_eq!(a["source"]["sha256"], "ab".repeat(32));
            assert!(!a["findings"].as_array().unwrap().is_empty(), "{what}");
        }
        let code = detect(&request("barcode", 0), &cfg).unwrap();
        assert_eq!(code["findings"][0]["format"], "code128");
        assert!(code["findings"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("SIM-"));
        let focus = detect(&request("focus", 0), &cfg).unwrap();
        let score = focus["findings"][0]["score"].as_f64().unwrap();
        assert_eq!(focus["findings"][0]["sharp"], score >= 50.0);
        // the points are inside the picture
        let blobs = detect(&request("blobs", 9), &cfg).unwrap();
        for f in blobs["findings"].as_array().unwrap() {
            assert!(f["x"].as_f64().unwrap() < 1280.0 && f["y"].as_f64().unwrap() < 960.0);
        }
        // no hash is allowed
        let bare = json!({"kind": "detect", "what": "focus", "image": {"width": 10, "height": 10}});
        assert_eq!(
            detect(&bare, &cfg).unwrap()["source"]["sha256"],
            Value::Null
        );
    }

    #[test]
    fn the_findings_are_cut_to_the_limit_of_the_station() {
        let few = VisionConfig {
            max_findings: 1,
            ..VisionConfig::default()
        };
        for seed in 0..20 {
            let r = detect(&request("blobs", seed), &few).unwrap();
            assert_eq!(r["findings"].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn a_request_that_does_not_fit_is_refused_with_what_is_wrong() {
        let cfg = VisionConfig::default();
        for bad in [
            json!({"kind": "track", "what": "focus", "image": {"width": 1, "height": 1}}),
            json!({"kind": "detect", "what": "faces", "image": {"width": 1, "height": 1}}),
            json!({"kind": "detect", "what": "focus"}),
            json!({"kind": "detect", "what": "focus", "image": {"width": 0, "height": 1}}),
            json!({"kind": "detect", "what": "focus", "image": {"width": 1, "height": 20001}}),
            json!({"kind": "detect", "what": "focus", "image": {"width": 1, "height": 1, "sha256": "xyz"}}),
            json!({"kind": "detect", "what": "focus", "image": {"width": 1, "height": 1}, "seed": "a"}),
        ] {
            assert!(
                matches!(detect(&bad, &cfg), Err(PluginError::Bad(_))),
                "{bad}"
            );
        }
    }
}
