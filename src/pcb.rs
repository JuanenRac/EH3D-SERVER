//! The board station with no machine behind: jobs that make a report about a board and move nothing. The plugin `pcb` (`src/plugin_pcb.rs`) only hands the stations of the
//! server to this engine.
//!
//! Four kinds of job, all of them checks and reports made from data the client sends:
//!
//! * `rules_check`: the tracks, the gaps and the holes of a board against the rules of the maker (`min_track_mm`, `min_gap_mm`, `min_drill_mm`, `max_drill_mm`, `min_edge_mm`);
//! * `drill_report`: the tools of a drilling (how many holes of each size, the travel and an estimate of the time);
//! * `bom_report`: the list of parts grouped by value and footprint, with the references that repeat;
//! * `inspect`: measurements against their nominal value and tolerance.
//!
//! The report is made when the job is prepared (it is in the `view` of the job) and the job then runs for the time the simulated station takes to go over its items. A job whose
//! report has errors ends `done` with the verdict `fail`, unless the station is set to `fail_on_violation`, and then it ends `failed`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::plugins::{PluginError, PluginResult, PluginStatus, Prepared, WorkState};
use crate::simclock::SimClock;
use crate::sync::Locked;

/// The most violations the report lists.
const MAX_LISTED: usize = 500;
/// The speed of the rapid moves of a drilling, for the estimate (mm/s).
const DRILL_RAPID_MM_S: f64 = 200.0;
/// The biggest board the station takes, in mm.
const MAX_BOARD_MM: f64 = 2000.0;

/// The settings a board station may have: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("min_track_mm", "the narrowest track the maker takes, 0.01 to 5 (0.15 by default)"),
    ("min_gap_mm", "the smallest gap between copper, 0.01 to 5 (0.15 by default)"),
    ("min_drill_mm", "the smallest hole, 0.05 to 10 (0.3 by default)"),
    ("max_drill_mm", "the biggest hole that is drilled and not routed, 0.1 to 50 (6.5 by default)"),
    ("min_edge_mm", "the smallest distance from the wall of a hole to the edge of the board, 0 to 10 (0.3 by default)"),
    ("drill_hit_s", "the seconds of one hole, for the estimate, 0 to 60 (0.5 by default)"),
    ("max_items", "the most items of a job, 1 to 200000 (50000 by default)"),
    ("fail_on_violation", "a job whose report has errors ends failed and not done (false by default)"),
    ("item_ms", "the milliseconds the simulated station takes for one item, 0 to 1000 (2 by default)"),
    ("sim_rate", "how many times faster than real time the simulated station works, 0 to 100000 (1 by default; 0 is no waiting)"),
];

/// The settings of a board station, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct PcbConfig {
    pub min_track: f64,
    pub min_gap: f64,
    pub min_drill: f64,
    pub max_drill: f64,
    pub min_edge: f64,
    pub drill_hit_s: f64,
    pub max_items: usize,
    pub fail_on_violation: bool,
    pub item_ms: f64,
    pub sim_rate: f64,
}

impl Default for PcbConfig {
    fn default() -> Self {
        PcbConfig {
            min_track: 0.15,
            min_gap: 0.15,
            min_drill: 0.3,
            max_drill: 6.5,
            min_edge: 0.3,
            drill_hit_s: 0.5,
            max_items: 50_000,
            fail_on_violation: false,
            item_ms: 2.0,
            sim_rate: 1.0,
        }
    }
}

fn number_in(settings: &Value, key: &str, low: f64, high: f64) -> Result<Option<f64>, String> {
    match settings.get(key) {
        None => Ok(None),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && (low..=high).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{key} is a number from {low} to {high}")),
        },
    }
}

impl PcbConfig {
    /// Reads the settings of a station: a key that is not one of [`SETTINGS_HELP`] or a value out of its range is refused.
    pub fn from_settings(settings: &Value) -> Result<PcbConfig, String> {
        let mut cfg = PcbConfig::default();
        if let Some(map) = settings.as_object() {
            for key in map.keys() {
                if !SETTINGS_HELP.iter().any(|(k, _)| k == key) {
                    return Err(format!("{key} is not a setting of a board station"));
                }
            }
        }
        let fields: [(&str, f64, f64, &mut f64); 8] = [
            ("min_track_mm", 0.01, 5.0, &mut cfg.min_track),
            ("min_gap_mm", 0.01, 5.0, &mut cfg.min_gap),
            ("min_drill_mm", 0.05, 10.0, &mut cfg.min_drill),
            ("max_drill_mm", 0.1, 50.0, &mut cfg.max_drill),
            ("min_edge_mm", 0.0, 10.0, &mut cfg.min_edge),
            ("drill_hit_s", 0.0, 60.0, &mut cfg.drill_hit_s),
            ("item_ms", 0.0, 1000.0, &mut cfg.item_ms),
            ("sim_rate", 0.0, 100_000.0, &mut cfg.sim_rate),
        ];
        for (key, low, high, slot) in fields {
            if let Some(n) = number_in(settings, key, low, high)? {
                *slot = n;
            }
        }
        if let Some(v) = settings.get("max_items") {
            cfg.max_items = v
                .as_u64()
                .filter(|n| (1..=200_000).contains(n))
                .ok_or("max_items is a whole number from 1 to 200000")?
                as usize;
        }
        match settings.get("fail_on_violation") {
            None => {}
            Some(Value::Bool(b)) => cfg.fail_on_violation = *b,
            Some(_) => return Err("fail_on_violation is true or false".into()),
        }
        if cfg.min_drill > cfg.max_drill {
            return Err("min_drill_mm is above max_drill_mm".into());
        }
        Ok(cfg)
    }
}

/// What a job made of its request: the report, how many items it goes over and the verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub kind: &'static str,
    /// `pass` or `fail` for the kinds that judge, `None` for a report that only counts.
    pub verdict: Option<&'static str>,
    pub errors: usize,
    pub warnings: usize,
    pub items: usize,
    pub body: Value,
}

fn bad<T>(why: impl Into<String>) -> PluginResult<T> {
    Err(PluginError::Bad(why.into()))
}

fn too_many(cfg: &PcbConfig) -> PluginError {
    PluginError::Coded(
        "too_big",
        format!(
            "a job may have {} items at the most on this station",
            cfg.max_items
        ),
    )
}

fn field(item: &Value, key: &str) -> Option<f64> {
    item.get(key)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
}

fn list<'a>(request: &'a Value, key: &str, cfg: &PcbConfig) -> PluginResult<&'a [Value]> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) if a.len() > cfg.max_items => Err(too_many(cfg)),
        Some(Value::Array(a)) => Ok(a),
        Some(_) => bad(format!("{key} is a list")),
    }
}

fn name_of(item: &Value, kind: &str, index: usize) -> String {
    match item.get("name").and_then(Value::as_str) {
        Some(n) if !n.is_empty() && n.chars().count() <= 64 && !n.chars().any(char::is_control) => {
            n.to_string()
        }
        _ => format!("{kind} {}", index + 1),
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// The rules in force: the settings of the station, over which the request may put its own.
fn rules_of(request: &Value, cfg: &PcbConfig) -> PluginResult<PcbConfig> {
    let mut rules = cfg.clone();
    let Some(over) = request.get("rules") else {
        return Ok(rules);
    };
    let Some(map) = over.as_object() else {
        return bad("rules is an object");
    };
    for (key, value) in map {
        let (low, high, slot): (f64, f64, &mut f64) = match key.as_str() {
            "min_track_mm" => (0.01, 5.0, &mut rules.min_track),
            "min_gap_mm" => (0.01, 5.0, &mut rules.min_gap),
            "min_drill_mm" => (0.05, 10.0, &mut rules.min_drill),
            "max_drill_mm" => (0.1, 50.0, &mut rules.max_drill),
            "min_edge_mm" => (0.0, 10.0, &mut rules.min_edge),
            other => return bad(format!("{other} is not a rule of a board")),
        };
        match value.as_f64().filter(|v| (low..=high).contains(v)) {
            Some(v) => *slot = v,
            None => return bad(format!("{key} is a number from {low} to {high}")),
        }
    }
    if rules.min_drill > rules.max_drill {
        return bad("min_drill_mm is above max_drill_mm");
    }
    Ok(rules)
}

struct Violations {
    errors: usize,
    warnings: usize,
    listed: Vec<Value>,
}

impl Violations {
    fn add(
        &mut self,
        severity: &str,
        rule: &str,
        item: &str,
        message: String,
        value: f64,
        limit: f64,
    ) {
        if severity == "error" {
            self.errors += 1;
        } else {
            self.warnings += 1;
        }
        if self.listed.len() < MAX_LISTED {
            self.listed.push(json!({
                "rule": rule, "severity": severity, "item": item, "message": message,
                "value": round3(value), "limit": round3(limit),
            }));
        }
    }
}

/// `rules_check`: the tracks, the gaps and the holes of `board` against the rules.
pub fn rules_check(request: &Value, cfg: &PcbConfig) -> PluginResult<Report> {
    let rules = rules_of(request, cfg)?;
    let board = request
        .get("board")
        .filter(|b| b.is_object())
        .ok_or_else(|| PluginError::Bad("a rules_check request has the object `board`".into()))?;
    let tracks = list(board, "tracks", cfg)?;
    let gaps = list(board, "clearances", cfg)?;
    let drills = list(board, "drills", cfg)?;
    let items = tracks.len() + gaps.len() + drills.len();
    if items == 0 {
        return bad("the board has no tracks, clearances or drills to check");
    }
    if items > cfg.max_items {
        return Err(too_many(cfg));
    }
    let outline = match board.get("outline") {
        None | Some(Value::Null) => None,
        Some(o) => match (field(o, "width_mm"), field(o, "height_mm")) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 && w <= MAX_BOARD_MM && h <= MAX_BOARD_MM => {
                Some((w, h))
            }
            _ => {
                return bad(format!(
                "outline is {{\"width_mm\", \"height_mm\"}}, each above 0 and up to {MAX_BOARD_MM}"
            ))
            }
        },
    };
    let mut v = Violations {
        errors: 0,
        warnings: 0,
        listed: Vec::new(),
    };
    for (i, t) in tracks.iter().enumerate() {
        let Some(w) = field(t, "width_mm").filter(|w| *w > 0.0) else {
            return bad(format!("track {}: width_mm is a number above 0", i + 1));
        };
        if w < rules.min_track {
            let name = name_of(t, "track", i);
            v.add(
                "error",
                "track_width",
                &name,
                format!(
                    "{name} is {w} mm wide, the maker takes {} mm",
                    rules.min_track
                ),
                w,
                rules.min_track,
            );
        }
    }
    for (i, g) in gaps.iter().enumerate() {
        let Some(gap) = field(g, "gap_mm").filter(|g| *g >= 0.0) else {
            return bad(format!("clearance {}: gap_mm is a number from 0", i + 1));
        };
        if gap < rules.min_gap {
            let name = name_of(g, "clearance", i);
            v.add(
                "error",
                "clearance",
                &name,
                format!(
                    "{name} leaves {gap} mm, the maker takes {} mm",
                    rules.min_gap
                ),
                gap,
                rules.min_gap,
            );
        }
    }
    let mut at: HashMap<(i64, i64), usize> = HashMap::new();
    for (i, d) in drills.iter().enumerate() {
        let (Some(dia), Some(x), Some(y)) = (field(d, "dia_mm"), field(d, "x"), field(d, "y"))
        else {
            return bad(format!("drill {}: dia_mm, x and y are numbers", i + 1));
        };
        if dia <= 0.0 || dia > 50.0 {
            return bad(format!("drill {}: dia_mm is above 0 and up to 50", i + 1));
        }
        let name = name_of(d, "drill", i);
        if dia < rules.min_drill {
            v.add(
                "error",
                "drill_small",
                &name,
                format!(
                    "{name} is {dia} mm, the smallest hole is {} mm",
                    rules.min_drill
                ),
                dia,
                rules.min_drill,
            );
        }
        if dia > rules.max_drill {
            v.add(
                "warning",
                "drill_large",
                &name,
                format!(
                    "{name} is {dia} mm, over {} mm it is routed and not drilled",
                    rules.max_drill
                ),
                dia,
                rules.max_drill,
            );
        }
        if let Some((w, h)) = outline {
            if !(0.0..=w).contains(&x) || !(0.0..=h).contains(&y) {
                v.add(
                    "error",
                    "drill_outside",
                    &name,
                    format!("{name} is at X{x} Y{y}, outside the board"),
                    0.0,
                    0.0,
                );
            } else {
                let edge = x.min(w - x).min(y).min(h - y) - dia / 2.0;
                if edge < rules.min_edge {
                    v.add(
                        "error",
                        "drill_edge",
                        &name,
                        format!(
                            "{name} leaves {} mm to the edge, the maker takes {} mm",
                            round3(edge),
                            rules.min_edge
                        ),
                        edge,
                        rules.min_edge,
                    );
                }
            }
        }
        let key = ((x * 100.0).round() as i64, (y * 100.0).round() as i64);
        if let Some(first) = at.insert(key, i) {
            v.add(
                "warning",
                "drill_duplicate",
                &name,
                format!("{name} is at the same place as drill {}", first + 1),
                0.0,
                0.0,
            );
        }
    }
    if outline.is_none() && !drills.is_empty() {
        v.add(
            "warning",
            "no_outline",
            "board",
            "the board has no outline: the holes were not checked against its edge".into(),
            0.0,
            0.0,
        );
    }
    let verdict = if v.errors == 0 { "pass" } else { "fail" };
    let body = json!({
        "kind": "rules_check", "verdict": verdict, "errors": v.errors, "warnings": v.warnings,
        "checked": { "tracks": tracks.len(), "clearances": gaps.len(), "drills": drills.len() },
        "rules": {
            "min_track_mm": rules.min_track, "min_gap_mm": rules.min_gap, "min_drill_mm": rules.min_drill,
            "max_drill_mm": rules.max_drill, "min_edge_mm": rules.min_edge,
        },
        "violations": v.listed, "violations_cut": v.errors + v.warnings > MAX_LISTED,
    });
    Ok(Report {
        kind: "rules_check",
        verdict: Some(verdict),
        errors: v.errors,
        warnings: v.warnings,
        items,
        body,
    })
}

/// `drill_report`: the tools of a drilling, the travel and an estimate of the time.
pub fn drill_report(request: &Value, cfg: &PcbConfig) -> PluginResult<Report> {
    let drills = list(request, "drills", cfg)?;
    if drills.is_empty() {
        return bad("a drill_report request has the list `drills`");
    }
    // by size in hundredths of a millimetre: the holes of the same tool
    let mut tools: BTreeMap<i64, Vec<(f64, f64, bool)>> = BTreeMap::new();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (i, d) in drills.iter().enumerate() {
        let (Some(dia), Some(x), Some(y)) = (field(d, "dia_mm"), field(d, "x"), field(d, "y"))
        else {
            return bad(format!("drill {}: dia_mm, x and y are numbers", i + 1));
        };
        if dia <= 0.0 || dia > 50.0 {
            return bad(format!("drill {}: dia_mm is above 0 and up to 50", i + 1));
        }
        let plated = match d.get("plated") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return bad(format!("drill {}: plated is true or false", i + 1)),
        };
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        tools
            .entry((dia * 100.0).round() as i64)
            .or_default()
            .push((x, y, plated));
    }
    let mut rows = Vec::new();
    let (mut travel_total, mut at) = (0.0, (0.0f64, 0.0f64));
    for (size, holes) in &tools {
        let mut left = holes.clone();
        let mut travel = 0.0;
        while !left.is_empty() {
            let mut best = 0;
            let mut best_d = f64::MAX;
            for (k, h) in left.iter().enumerate() {
                let d = (h.0 - at.0).hypot(h.1 - at.1);
                if d < best_d {
                    best = k;
                    best_d = d;
                }
            }
            travel += best_d;
            at = (left[best].0, left[best].1);
            left.swap_remove(best);
        }
        travel_total += travel;
        rows.push(json!({
            "dia_mm": *size as f64 / 100.0,
            "hits": holes.len(),
            "plated": holes.iter().filter(|h| h.2).count(),
            "travel_mm": round3(travel),
        }));
    }
    let estimate = drills.len() as f64 * cfg.drill_hit_s + travel_total / DRILL_RAPID_MM_S;
    let body = json!({
        "kind": "drill_report", "hits": drills.len(), "tools": rows,
        "travel_mm": round3(travel_total), "estimate_s": round3(estimate),
        "box": { "min_x": min_x, "min_y": min_y, "max_x": max_x, "max_y": max_y },
    });
    Ok(Report {
        kind: "drill_report",
        verdict: None,
        errors: 0,
        warnings: 0,
        items: drills.len(),
        body,
    })
}

/// `bom_report`: the parts grouped by value and footprint.
pub fn bom_report(request: &Value, cfg: &PcbConfig) -> PluginResult<Report> {
    let parts = list(request, "components", cfg)?;
    if parts.is_empty() {
        return bad("a bom_report request has the list `components`");
    }
    let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let (mut errors, mut warnings) = (0usize, 0usize);
    let mut issues: Vec<Value> = Vec::new();
    let mut total = 0usize;
    for (i, c) in parts.iter().enumerate() {
        let Some(reference) = c.get("ref").and_then(Value::as_str).filter(|r| {
            !r.is_empty() && r.chars().count() <= 32 && !r.chars().any(char::is_whitespace)
        }) else {
            return bad(format!(
                "component {}: ref is a name of up to 32 characters with no spaces",
                i + 1
            ));
        };
        let text = |k: &str| {
            c.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        let (value, footprint) = (text("value"), text("footprint"));
        let qty = match c.get("qty") {
            None => 1,
            Some(q) => q
                .as_u64()
                .filter(|q| (1..=100_000).contains(q))
                .ok_or_else(|| {
                    PluginError::Bad(format!(
                        "component {}: qty is a whole number from 1 to 100000",
                        i + 1
                    ))
                })? as usize,
        };
        if let Some(first) = seen.insert(reference.to_string(), i) {
            errors += 1;
            if issues.len() < MAX_LISTED {
                issues.push(json!({"severity": "error", "rule": "duplicate_ref", "item": reference, "message": format!("{reference} is in the list twice (rows {} and {})", first + 1, i + 1)}));
            }
        }
        if footprint.is_empty() {
            warnings += 1;
            if issues.len() < MAX_LISTED {
                issues.push(json!({"severity": "warning", "rule": "no_footprint", "item": reference, "message": format!("{reference} has no footprint")}));
            }
        }
        if value.is_empty() {
            warnings += 1;
            if issues.len() < MAX_LISTED {
                issues.push(json!({"severity": "warning", "rule": "no_value", "item": reference, "message": format!("{reference} has no value")}));
            }
        }
        total += qty;
        groups
            .entry((value, footprint))
            .or_default()
            .push(reference.to_string());
        if qty > 1 {
            // a line with a quantity stands for several parts of the same kind: they count, their reference is the one of the line
            let group = groups
                .entry((text("value"), text("footprint")))
                .or_default();
            for _ in 1..qty {
                group.push(format!("{reference}*"));
            }
        }
    }
    let rows: Vec<Value> = groups
        .iter()
        .map(|((value, footprint), refs)| {
            let mut sorted = refs.clone();
            sorted.sort();
            json!({ "value": value, "footprint": footprint, "qty": refs.len(), "refs": sorted })
        })
        .collect();
    let verdict = if errors == 0 { "pass" } else { "fail" };
    let body = json!({
        "kind": "bom_report", "verdict": verdict, "errors": errors, "warnings": warnings,
        "lines": rows.len(), "parts": total, "groups": rows, "issues": issues,
    });
    Ok(Report {
        kind: "bom_report",
        verdict: Some(verdict),
        errors,
        warnings,
        items: parts.len(),
        body,
    })
}

/// `inspect`: measurements against their nominal value and tolerance.
pub fn inspect_report(request: &Value, cfg: &PcbConfig) -> PluginResult<Report> {
    let rows = list(request, "measurements", cfg)?;
    if rows.is_empty() {
        return bad("an inspect request has the list `measurements`");
    }
    let (mut errors, mut warnings) = (0usize, 0usize);
    let mut out = Vec::with_capacity(rows.len());
    for (i, m) in rows.iter().enumerate() {
        let (Some(nominal), Some(tol), Some(value)) = (
            field(m, "nominal"),
            field(m, "tolerance"),
            field(m, "value"),
        ) else {
            return bad(format!(
                "measurement {}: nominal, tolerance and value are numbers",
                i + 1
            ));
        };
        if tol < 0.0 {
            return bad(format!("measurement {}: tolerance is from 0", i + 1));
        }
        let deviation = value - nominal;
        let pass = deviation.abs() <= tol + 1e-12;
        if !pass {
            errors += 1;
        } else if tol > 0.0 && deviation.abs() > 0.8 * tol {
            warnings += 1;
        }
        out.push(json!({
            "name": name_of(m, "measurement", i), "nominal": nominal, "value": value,
            "tolerance": tol, "deviation": round3(deviation), "pass": pass,
            "unit": m.get("unit").and_then(Value::as_str).filter(|u| u.chars().count() <= 8).unwrap_or(""),
        }));
    }
    let verdict = if errors == 0 { "pass" } else { "fail" };
    let body = json!({
        "kind": "inspect", "verdict": verdict, "errors": errors, "warnings": warnings,
        "measurements": out,
    });
    Ok(Report {
        kind: "inspect",
        verdict: Some(verdict),
        errors,
        warnings,
        items: rows.len(),
        body,
    })
}

// ---- the stations and their jobs -----------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Station {
    jobs_done: u64,
    last_kind: Option<&'static str>,
    last_verdict: Option<&'static str>,
}

struct PcbJob {
    machine: String,
    kind: &'static str,
    items: usize,
    item_s: f64,
    verdict: Option<&'static str>,
    errors: usize,
    fail_on_violation: bool,
    state: WorkState,
    clock: SimClock,
    rate: f64,
    message: String,
    counted: bool,
}

impl PcbJob {
    fn done(&self) -> usize {
        match self.state {
            WorkState::Queued => 0,
            WorkState::Done | WorkState::Failed => self.items,
            _ => {
                if self.item_s <= 0.0 {
                    return self.items;
                }
                let n = self.clock.work_s(self.rate) / self.item_s;
                if n >= self.items as f64 {
                    self.items
                } else {
                    n as usize
                }
            }
        }
    }
}

/// The board stations of the server: their jobs and what each one did last.
#[derive(Default)]
pub struct PcbEngine {
    jobs: Mutex<HashMap<String, PcbJob>>,
    stations: Mutex<HashMap<String, Station>>,
}

fn lost() -> PluginError {
    PluginError::Failed("the board station lost the job (the server restarted)".into())
}

fn missing() -> PluginError {
    PluginError::Bad("the job is not there".into())
}

impl PcbEngine {
    pub fn busy(&self, machine: &str) -> bool {
        self.jobs
            .locked()
            .values()
            .any(|j| j.machine == machine && j.state.active())
    }

    pub fn detail(&self, machine: &str, cfg: &PcbConfig) -> Value {
        let s = self
            .stations
            .locked()
            .get(machine)
            .cloned()
            .unwrap_or_default();
        json!({
            "jobs_done": s.jobs_done,
            "last_kind": s.last_kind,
            "last_verdict": s.last_verdict,
            "rules": {
                "min_track_mm": cfg.min_track, "min_gap_mm": cfg.min_gap, "min_drill_mm": cfg.min_drill,
                "max_drill_mm": cfg.max_drill, "min_edge_mm": cfg.min_edge,
            },
            "fail_on_violation": cfg.fail_on_violation,
        })
    }

    pub fn prepare(
        &self,
        machine: &str,
        cfg: &PcbConfig,
        request: &Value,
    ) -> PluginResult<Prepared> {
        let report = match request.get("kind").and_then(Value::as_str) {
            Some("rules_check") => rules_check(request, cfg)?,
            Some("drill_report") => drill_report(request, cfg)?,
            Some("bom_report") => bom_report(request, cfg)?,
            Some("inspect") => inspect_report(request, cfg)?,
            _ => {
                return bad("the request has to be {\"kind\": \"rules_check\" | \"drill_report\" | \"bom_report\" | \"inspect\", ...}");
            }
        };
        let native_id = crate::ids::new(crate::ids::Kind::Work);
        self.jobs.locked().insert(
            native_id.clone(),
            PcbJob {
                machine: machine.to_string(),
                kind: report.kind,
                items: report.items,
                item_s: cfg.item_ms / 1000.0,
                verdict: report.verdict,
                errors: report.errors,
                fail_on_violation: cfg.fail_on_violation,
                state: WorkState::Queued,
                clock: SimClock::new(),
                rate: cfg.sim_rate,
                message: String::new(),
                counted: false,
            },
        );
        Ok(Prepared {
            native_id,
            view: report.body,
        })
    }

    pub fn start(&self, machine: &str, native_id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        if jobs
            .values()
            .any(|j| j.machine == machine && j.state.active())
        {
            return Err(PluginError::Busy);
        }
        let job = jobs.get_mut(native_id).ok_or_else(missing)?;
        if job.state != WorkState::Queued {
            return Err(PluginError::NotInState);
        }
        job.state = WorkState::Running;
        job.clock.run();
        Ok(())
    }

    /// Ends a running job whose items were all gone over.
    fn settle(&self, job: &mut PcbJob) {
        if job.state == WorkState::Running && job.done() >= job.items {
            job.clock.hold();
            if job.fail_on_violation && job.errors > 0 {
                job.state = WorkState::Failed;
                job.message = format!("the report has {} errors", job.errors);
            } else {
                job.state = WorkState::Done;
            }
        }
        if job.state.finished() && job.state != WorkState::Cancelled && !job.counted {
            job.counted = true;
            let mut stations = self.stations.locked();
            let s = stations.entry(job.machine.clone()).or_default();
            s.jobs_done += 1;
            s.last_kind = Some(job.kind);
            s.last_verdict = job.verdict;
        }
    }

    pub fn pause(&self, native_id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(native_id).ok_or_else(missing)?;
        self.settle(job);
        if job.state != WorkState::Running {
            return Err(PluginError::NotInState);
        }
        job.clock.hold();
        job.state = WorkState::Paused;
        Ok(())
    }

    pub fn resume(&self, native_id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(native_id).ok_or_else(missing)?;
        if job.state != WorkState::Paused {
            return Err(PluginError::NotInState);
        }
        job.clock.run();
        job.state = WorkState::Running;
        Ok(())
    }

    pub fn stop(&self, native_id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(native_id).ok_or_else(missing)?;
        self.settle(job);
        if !job.state.active() {
            return Err(PluginError::NotInState);
        }
        job.clock.hold();
        job.state = WorkState::Cancelled;
        job.message = "stopped".into();
        Ok(())
    }

    pub fn status(&self, native_id: &str) -> PluginResult<PluginStatus> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(native_id).ok_or_else(lost)?;
        self.settle(job);
        Ok(PluginStatus {
            state: job.state,
            done: job.done() as u64,
            total: job.items as u64,
            message: job.message.clone(),
        })
    }

    pub fn discard(&self, native_id: &str) {
        self.jobs.locked().remove(native_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PcbConfig {
        PcbConfig::default()
    }

    fn violations(r: &Report) -> Vec<(String, String)> {
        r.body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["rule"].as_str().unwrap().to_string(),
                    v["item"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn the_settings_are_checked_and_a_typo_is_refused() {
        assert_eq!(PcbConfig::from_settings(&json!({})).unwrap(), cfg());
        let c = PcbConfig::from_settings(&json!({"min_track_mm": 0.1, "fail_on_violation": true, "max_items": 10, "sim_rate": 0})).unwrap();
        assert_eq!(c.min_track, 0.1);
        assert!(c.fail_on_violation);
        assert_eq!(c.max_items, 10);
        for bad in [
            json!({"min_track": 0.1}),
            json!({"min_track_mm": 0}),
            json!({"min_drill_mm": 8, "max_drill_mm": 2}),
            json!({"fail_on_violation": "no"}),
            json!({"max_items": 0}),
            json!({"max_items": 200001}),
            json!({"item_ms": -1}),
        ] {
            assert!(PcbConfig::from_settings(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_board_that_fits_the_rules_passes() {
        let r = rules_check(
            &json!({"board": {
                "outline": {"width_mm": 100, "height_mm": 80},
                "tracks": [{"width_mm": 0.2}, {"width_mm": 0.15}],
                "clearances": [{"gap_mm": 0.2}],
                "drills": [{"dia_mm": 0.8, "x": 10, "y": 10}, {"dia_mm": 3.2, "x": 90, "y": 70}],
            }}),
            &cfg(),
        )
        .unwrap();
        assert_eq!(
            (r.verdict, r.errors, r.warnings, r.items),
            (Some("pass"), 0, 0, 5)
        );
        assert_eq!(r.body["checked"]["drills"], 2);
        assert_eq!(r.body["violations"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn every_rule_that_is_broken_is_listed_with_its_item_and_its_numbers() {
        let r = rules_check(
            &json!({"board": {
                "outline": {"width_mm": 50, "height_mm": 40},
                "tracks": [{"width_mm": 0.1, "name": "VCC"}, {"width_mm": 0.3}],
                "clearances": [{"gap_mm": 0.05}],
                "drills": [
                    {"dia_mm": 0.2, "x": 10, "y": 10},
                    {"dia_mm": 7, "x": 25, "y": 20},
                    {"dia_mm": 1, "x": 60, "y": 10},
                    {"dia_mm": 2, "x": 1, "y": 20},
                    {"dia_mm": 1, "x": 10, "y": 10.001},
                ],
            }}),
            &cfg(),
        )
        .unwrap();
        assert_eq!(r.verdict, Some("fail"));
        let got = violations(&r);
        let has = |rule: &str, item: &str| got.iter().any(|(a, b)| a == rule && b == item);
        assert!(has("track_width", "VCC"), "{got:?}");
        assert!(has("clearance", "clearance 1"));
        assert!(has("drill_small", "drill 1"));
        assert!(has("drill_large", "drill 2"));
        assert!(has("drill_outside", "drill 3"));
        assert!(has("drill_edge", "drill 4"));
        assert!(has("drill_duplicate", "drill 5"));
        assert_eq!((r.errors, r.warnings), (5, 2));
        let first = &r.body["violations"][0];
        assert_eq!(
            (first["value"].as_f64(), first["limit"].as_f64()),
            (Some(0.1), Some(0.15))
        );
    }

    #[test]
    fn the_request_can_put_its_own_rules_over_the_ones_of_the_station() {
        let board = json!({"tracks": [{"width_mm": 0.1}]});
        let strict = rules_check(&json!({"board": board}), &cfg()).unwrap();
        assert_eq!(strict.verdict, Some("fail"));
        let loose = rules_check(
            &json!({"board": board, "rules": {"min_track_mm": 0.1}}),
            &cfg(),
        )
        .unwrap();
        assert_eq!(loose.verdict, Some("pass"));
        assert_eq!(loose.body["rules"]["min_track_mm"], 0.1);
        for bad in [
            json!({"board": board, "rules": {"min_track": 1}}),
            json!({"board": board, "rules": {"min_track_mm": 9}}),
            json!({"board": board, "rules": []}),
            json!({"board": board, "rules": {"min_drill_mm": 9, "max_drill_mm": 1}}),
        ] {
            assert!(rules_check(&bad, &cfg()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_board_without_outline_warns_that_the_edges_were_not_checked() {
        let r = rules_check(
            &json!({"board": {"drills": [{"dia_mm": 1, "x": 0, "y": 0}]}}),
            &cfg(),
        )
        .unwrap();
        assert_eq!(r.verdict, Some("pass"));
        assert_eq!(r.warnings, 1);
        assert_eq!(violations(&r)[0].0, "no_outline");
    }

    #[test]
    fn a_rules_request_that_does_not_fit_is_refused() {
        for bad in [
            json!({}),
            json!({"board": []}),
            json!({"board": {}}),
            json!({"board": {"tracks": [{"width_mm": 0}]}}),
            json!({"board": {"tracks": [{}]}}),
            json!({"board": {"clearances": [{"gap_mm": -1}]}}),
            json!({"board": {"drills": [{"dia_mm": 1, "x": 1}]}}),
            json!({"board": {"drills": [{"dia_mm": 51, "x": 1, "y": 1}]}}),
            json!({"board": {"tracks": "many"}}),
            json!({"board": {"tracks": [{"width_mm": 1}], "outline": {"width_mm": 0, "height_mm": 5}}}),
            json!({"board": {"tracks": [{"width_mm": 1}], "outline": {"width_mm": 5001, "height_mm": 5}}}),
        ] {
            assert!(
                matches!(rules_check(&bad, &cfg()), Err(PluginError::Bad(_))),
                "{bad}"
            );
        }
        let small = PcbConfig {
            max_items: 2,
            ..cfg()
        };
        let three =
            json!({"board": {"tracks": [{"width_mm": 1}, {"width_mm": 1}, {"width_mm": 1}]}});
        assert!(matches!(
            rules_check(&three, &small),
            Err(PluginError::Coded("too_big", _))
        ));
    }

    #[test]
    fn the_drill_report_groups_the_holes_by_tool_and_estimates_the_time() {
        let r = drill_report(
            &json!({"drills": [
                {"dia_mm": 0.8, "x": 10, "y": 0, "plated": true},
                {"dia_mm": 0.8, "x": 20, "y": 0},
                {"dia_mm": 3.2, "x": 20, "y": 30},
                {"dia_mm": 0.80001, "x": 30, "y": 0, "plated": true},
            ]}),
            &cfg(),
        )
        .unwrap();
        assert_eq!((r.items, r.verdict), (4, None));
        let tools = r.body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["dia_mm"], 0.8);
        assert_eq!(tools[0]["hits"], 3);
        assert_eq!(tools[0]["plated"], 2);
        assert_eq!(tools[1]["hits"], 1);
        // the first tool goes 10, 20, 30 along x from the origin: 30 mm; then 30 mm up and over to the big hole is 10 and 30
        assert_eq!(tools[0]["travel_mm"], 30.0);
        let travel = r.body["travel_mm"].as_f64().unwrap();
        assert!(
            (travel - (30.0 + (10.0f64.powi(2) + 30.0f64.powi(2)).sqrt())).abs() < 1e-2,
            "{travel}"
        );
        let est = r.body["estimate_s"].as_f64().unwrap();
        assert!((est - (4.0 * 0.5 + travel / 200.0)).abs() < 1e-2);
        assert_eq!(r.body["box"]["max_x"], 30.0);
        for bad in [
            json!({}),
            json!({"drills": []}),
            json!({"drills": [{"dia_mm": 1, "x": 1, "y": 1, "plated": 1}]}),
            json!({"drills": [{"dia_mm": 0, "x": 1, "y": 1}]}),
        ] {
            assert!(drill_report(&bad, &cfg()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_bom_groups_the_parts_and_reports_the_references_that_repeat() {
        let r = bom_report(
            &json!({"components": [
                {"ref": "R2", "value": "10k", "footprint": "0603"},
                {"ref": "R1", "value": "10k", "footprint": "0603"},
                {"ref": "C1", "value": "100n", "footprint": "0603"},
                {"ref": "J1", "value": "Header", "footprint": "", "qty": 2},
            ]}),
            &cfg(),
        )
        .unwrap();
        assert_eq!((r.verdict, r.errors, r.warnings), (Some("pass"), 0, 1));
        assert_eq!(r.body["parts"], 5);
        assert_eq!(r.body["lines"], 3);
        let groups = r.body["groups"].as_array().unwrap();
        let tens = groups.iter().find(|g| g["value"] == "10k").unwrap();
        assert_eq!(tens["qty"], 2);
        assert_eq!(tens["refs"], json!(["R1", "R2"]));
        let dup = bom_report(&json!({"components": [{"ref": "R1", "value": "1k", "footprint": "0603"}, {"ref": "R1", "value": "1k", "footprint": "0603"}]}), &cfg()).unwrap();
        assert_eq!((dup.verdict, dup.errors), (Some("fail"), 1));
        assert!(dup.body["issues"][0]["message"]
            .as_str()
            .unwrap()
            .contains("rows 1 and 2"));
        for bad in [
            json!({}),
            json!({"components": []}),
            json!({"components": [{"value": "1k"}]}),
            json!({"components": [{"ref": "R 1"}]}),
            json!({"components": [{"ref": "R1", "qty": 0}]}),
        ] {
            assert!(bom_report(&bad, &cfg()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_inspection_judges_each_measurement_against_its_tolerance() {
        let r = inspect_report(
            &json!({"measurements": [
                {"name": "width", "nominal": 100, "tolerance": 0.1, "value": 100.05},
                {"name": "hole", "nominal": 0.8, "tolerance": 0.05, "value": 0.845, "unit": "mm"},
                {"name": "thickness", "nominal": 1.6, "tolerance": 0.1, "value": 1.75},
            ]}),
            &cfg(),
        )
        .unwrap();
        assert_eq!(
            (r.verdict, r.errors, r.warnings, r.items),
            (Some("fail"), 1, 1, 3)
        );
        let rows = r.body["measurements"].as_array().unwrap();
        assert_eq!(rows[0]["pass"], true);
        assert_eq!(rows[2]["pass"], false);
        assert_eq!(rows[2]["deviation"], 0.15);
        assert_eq!(rows[1]["unit"], "mm");
        // on the limit is inside it
        let edge = inspect_report(
            &json!({"measurements": [{"nominal": 1, "tolerance": 0.5, "value": 1.5}]}),
            &cfg(),
        )
        .unwrap();
        assert_eq!(edge.verdict, Some("pass"));
        for bad in [
            json!({}),
            json!({"measurements": []}),
            json!({"measurements": [{"nominal": 1, "tolerance": -1, "value": 1}]}),
            json!({"measurements": [{"nominal": 1, "value": 1}]}),
        ] {
            assert!(inspect_report(&bad, &cfg()).is_err(), "{bad}");
        }
    }

    fn quick() -> PcbConfig {
        PcbConfig {
            sim_rate: 0.0,
            ..cfg()
        }
    }

    fn rules_job() -> Value {
        json!({"kind": "rules_check", "board": {"tracks": [{"width_mm": 0.05}, {"width_mm": 0.2}]}})
    }

    #[test]
    fn a_job_ends_done_with_its_verdict_and_the_station_remembers_it() {
        let e = PcbEngine::default();
        let c = quick();
        let p = e.prepare("s1", &c, &rules_job()).unwrap();
        assert_eq!(p.view["verdict"], "fail");
        assert_eq!(e.status(&p.native_id).unwrap().state, WorkState::Queued);
        e.start("s1", &p.native_id).unwrap();
        assert!(e.busy("s1"));
        let st = e.status(&p.native_id).unwrap();
        assert_eq!((st.state, st.done, st.total), (WorkState::Done, 2, 2));
        assert!(!e.busy("s1"));
        let d = e.detail("s1", &c);
        assert_eq!(d["jobs_done"], 1);
        assert_eq!(d["last_verdict"], "fail");
        assert_eq!(d["last_kind"], "rules_check");
        // asking again does not count the job twice
        e.status(&p.native_id).unwrap();
        assert_eq!(e.detail("s1", &c)["jobs_done"], 1);
    }

    #[test]
    fn a_station_set_to_fail_on_violation_ends_the_job_failed() {
        let e = PcbEngine::default();
        let c = PcbConfig {
            fail_on_violation: true,
            ..quick()
        };
        let p = e.prepare("s1", &c, &rules_job()).unwrap();
        e.start("s1", &p.native_id).unwrap();
        let st = e.status(&p.native_id).unwrap();
        assert_eq!(st.state, WorkState::Failed);
        assert!(st.message.contains("1 errors"), "{}", st.message);
        // a clean report is done even then
        let ok = e
            .prepare(
                "s1",
                &c,
                &json!({"kind": "rules_check", "board": {"tracks": [{"width_mm": 0.5}]}}),
            )
            .unwrap();
        e.start("s1", &ok.native_id).unwrap();
        assert_eq!(e.status(&ok.native_id).unwrap().state, WorkState::Done);
    }

    #[test]
    fn one_job_at_a_time_hold_resume_stop_and_discard() {
        let e = PcbEngine::default();
        let slow = PcbConfig {
            item_ms: 1000.0,
            sim_rate: 1.0,
            ..cfg()
        };
        let a = e.prepare("s1", &slow, &rules_job()).unwrap().native_id;
        let b = e.prepare("s1", &slow, &rules_job()).unwrap().native_id;
        assert_eq!(e.pause(&a), Err(PluginError::NotInState));
        e.start("s1", &a).unwrap();
        assert_eq!(e.start("s1", &b), Err(PluginError::Busy));
        e.pause(&a).unwrap();
        assert_eq!(e.status(&a).unwrap().state, WorkState::Paused);
        e.resume(&a).unwrap();
        e.stop(&a).unwrap();
        assert_eq!(e.status(&a).unwrap().state, WorkState::Cancelled);
        assert_eq!(e.stop(&a), Err(PluginError::NotInState));
        // a stopped job is not a job that was done
        assert_eq!(e.detail("s1", &slow)["jobs_done"], 0);
        e.discard(&b);
        assert!(matches!(e.status(&b), Err(PluginError::Failed(_))));
        assert!(matches!(e.start("s1", &b), Err(PluginError::Bad(_))));
        assert!(matches!(
            e.prepare("s1", &slow, &json!({"kind": "etch"})),
            Err(PluginError::Bad(_))
        ));
    }

    #[test]
    fn the_progress_follows_the_clock_in_items() {
        let mut job = PcbJob {
            machine: "s".into(),
            kind: "inspect",
            items: 10,
            item_s: 1.0,
            verdict: None,
            errors: 0,
            fail_on_violation: false,
            state: WorkState::Running,
            clock: SimClock::new(),
            rate: 1.0,
            message: String::new(),
            counted: false,
        };
        job.clock.set_ran_s(3.5);
        assert_eq!(job.done(), 3);
        job.clock.run();
        assert!(job.done() <= 10);
        job.rate = 2.0;
        job.clock.hold();
        job.clock.set_ran_s(3.5);
        job.state = WorkState::Paused;
        assert_eq!(job.done(), 7);
        job.clock.set_ran_s(100.0);
        assert_eq!(job.done(), 10);
        job.state = WorkState::Queued;
        assert_eq!(job.done(), 0);
    }
}
