//! The CNC machines with no hardware behind: routers, mills and engravers driven by G-code, held to the travel of the machine, with a jog that has limits and an emergency
//! stop that latches the machine until an administrator releases it. The plugin `cnc` (`src/plugin_cnc.rs`) only hands the machines of the server to this engine.
//!
//! The program is cleaned and read by the same reader the `bungard` plugin uses (`src/gcode.rs`), so a line that one refuses the other refuses too. What this engine adds is
//! the look at every move against the travel of the machine, the turns of the spindle against its limit, and the simulated run: each line takes `step_ms` milliseconds, and
//! the position of the head follows the lines that were done. A connection other than the simulator (`grbl`, `node`) is not served yet.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::gcode::{self, Entry};
use crate::plugins::{PluginError, PluginResult, PluginStatus, Prepared, WorkState};
use crate::simjobs::{evenly, flag_in, known_keys, num, number_in, round2, SimBook};
use crate::sync::Locked;

/// The settings a CNC machine may have: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("connection", "how the machine is reached: {\"kind\": \"demo\"} (the simulator, the default); `grbl` and `node` are not served yet"),
    ("travel_x_mm", "the travel of X, 0 to 5000: a move outside 0 to this is refused (not set: the check is off and the job says so)"),
    ("travel_y_mm", "the travel of Y, 0 to 5000: a move outside 0 to this is refused (not set: the check is off)"),
    ("travel_z_mm", "the travel of Z, 0 to 5000: a move outside minus this to plus this is refused (not set: the check is off)"),
    ("reject_out_of_travel", "a program that leaves the travel is refused (true by default); false lets it in and the job lists the lines"),
    ("jog_max_mm", "the longest jog, 0.01 to 500 (50 by default)"),
    ("jog_max_feed", "the fastest jog feed in mm/min, 1 to 30000 (1000 by default)"),
    ("spindle_max_rpm", "the most turns a program may ask of the spindle, 0 to 65535 (12000 by default)"),
    ("rapid_mm_min", "the speed of a rapid move for the estimate, 1 to 100000 (3000 by default)"),
    ("latch_on_stop", "an emergency stop latches the machine until an administrator releases it (true by default)"),
    ("step_ms", "the milliseconds one line takes in the simulator, 0 to 3600000 (20 by default; 0 ends a job as soon as it runs)"),
];

/// The most lines of code a job may have.
pub const MAX_LINES: usize = 200_000;
/// The slack on the limits of the travel, in mm (rounding of the numbers of a program).
const EDGE_EPS: f64 = 1e-6;
/// The axes a jog may move.
const AXES: [char; 3] = ['x', 'y', 'z'];

/// The settings of a CNC machine, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct CncConfig {
    pub travel: [Option<f64>; 3],
    pub reject_out_of_travel: bool,
    pub jog_max_mm: f64,
    pub jog_max_feed: f64,
    pub spindle_max_rpm: f64,
    pub rapid_mm_min: f64,
    pub latch_on_stop: bool,
    pub step_ms: f64,
}

impl Default for CncConfig {
    fn default() -> Self {
        CncConfig {
            travel: [None; 3],
            reject_out_of_travel: true,
            jog_max_mm: 50.0,
            jog_max_feed: 1000.0,
            spindle_max_rpm: 12000.0,
            rapid_mm_min: 3000.0,
            latch_on_stop: true,
            step_ms: 20.0,
        }
    }
}

impl CncConfig {
    /// Reads the settings of a machine: a key that is not one of [`SETTINGS_HELP`] or a value out of its range is refused.
    pub fn from_settings(settings: &Value) -> Result<CncConfig, String> {
        let keys: Vec<&str> = SETTINGS_HELP.iter().map(|(k, _)| *k).collect();
        known_keys(settings, &keys, "a CNC machine")?;
        let mut cfg = CncConfig::default();
        for (i, key) in ["travel_x_mm", "travel_y_mm", "travel_z_mm"]
            .into_iter()
            .enumerate()
        {
            cfg.travel[i] = number_in(settings, key, 0.0, 5000.0)?;
        }
        if let Some(n) = number_in(settings, "jog_max_mm", 0.01, 500.0)? {
            cfg.jog_max_mm = n;
        }
        if let Some(n) = number_in(settings, "jog_max_feed", 1.0, 30000.0)? {
            cfg.jog_max_feed = n;
        }
        if let Some(n) = number_in(settings, "spindle_max_rpm", 0.0, 65535.0)? {
            cfg.spindle_max_rpm = n;
        }
        if let Some(n) = number_in(settings, "rapid_mm_min", 1.0, 100_000.0)? {
            cfg.rapid_mm_min = n;
        }
        if let Some(n) = number_in(settings, "step_ms", 0.0, 3_600_000.0)? {
            cfg.step_ms = n;
        }
        if let Some(b) = flag_in(settings, "reject_out_of_travel")? {
            cfg.reject_out_of_travel = b;
        }
        if let Some(b) = flag_in(settings, "latch_on_stop")? {
            cfg.latch_on_stop = b;
        }
        match settings.get("connection") {
            None => {}
            Some(c) => match c.get("kind").and_then(Value::as_str) {
                Some("demo") => {}
                Some("grbl" | "node") => {
                    return Err("the connections `grbl` and `node` are not served yet: use {\"kind\": \"demo\"}".into())
                }
                _ => return Err("connection is {\"kind\": \"demo\"}".into()),
            },
        }
        Ok(cfg)
    }

    /// Whether a point is inside the travel (an axis with no travel set is not looked at).
    pub fn inside(&self, p: [f64; 3]) -> bool {
        self.travel.iter().enumerate().all(|(i, t)| match t {
            None => true,
            Some(t) => {
                let low = if i == 2 { -t } else { 0.0 };
                (low - EDGE_EPS..=t + EDGE_EPS).contains(&p[i])
            }
        })
    }

    /// Whether any axis has its travel set.
    pub fn travel_known(&self) -> bool {
        self.travel.iter().any(Option::is_some)
    }
}

/// A refusal of the reading of a job: one of the codes of the catalog of errors and what is wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct CncError {
    pub code: &'static str,
    pub detail: String,
}

fn fail<T>(code: &'static str, detail: String) -> Result<T, CncError> {
    Err(CncError { code, detail })
}

impl From<CncError> for PluginError {
    fn from(e: CncError) -> PluginError {
        PluginError::Coded(e.code, e.detail)
    }
}

/// A program that was read: what it asks of the machine, line by line.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    /// The lines that would be sent.
    pub lines: Vec<String>,
    /// Where the head is after each line, and the turns of the spindle in force.
    pub after: Vec<([f64; 3], f64)>,
    pub rapid_moves: usize,
    pub cut_moves: usize,
    pub arcs: usize,
    pub travel_mm: f64,
    pub estimate_s: f64,
    pub max_feed: f64,
    pub max_rpm: f64,
    pub bounds: Option<([f64; 3], [f64; 3])>,
    /// The lines (counted from 1 in the text of the program) that leave the travel, when the machine lets them in.
    pub out_of_travel: Vec<usize>,
}

fn arc_extremes(
    from: [f64; 3],
    to: [f64; 3],
    center: (f64, f64),
    clockwise: bool,
) -> Vec<(f64, f64)> {
    let tau = std::f64::consts::TAU;
    let r = (from[0] - center.0).hypot(from[1] - center.1);
    let a0 = (from[1] - center.1).atan2(from[0] - center.0);
    let a1 = (to[1] - center.1).atan2(to[0] - center.0);
    let mut sweep = if clockwise { a0 - a1 } else { a1 - a0 }.rem_euclid(tau);
    if sweep < 1e-12 {
        sweep = tau;
    }
    let mut out = Vec::new();
    for k in 0..4 {
        let a = f64::from(k) * std::f64::consts::FRAC_PI_2;
        let from_start = if clockwise { a0 - a } else { a - a0 }.rem_euclid(tau);
        if from_start <= sweep {
            out.push((center.0 + r * a.cos(), center.1 + r * a.sin()));
        }
    }
    out
}

/// The center of an arc given by its radius `r` (the `R` word), as G-code reads it: a positive radius is the shorter arc.
fn center_from_radius(
    from: [f64; 3],
    to: [f64; 3],
    r: f64,
    clockwise: bool,
) -> Result<(f64, f64), String> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let d = dx.hypot(dy);
    if d < 1e-9 {
        return Err("an arc given by its radius cannot start and end at one point".into());
    }
    let h2 = r * r - d * d / 4.0;
    if h2 < -1e-6 {
        return Err("the radius of an arc is shorter than half the distance of its ends".into());
    }
    let h = h2.max(0.0).sqrt() * if r < 0.0 { -1.0 } else { 1.0 };
    let side = if clockwise { 1.0 } else { -1.0 };
    Ok((
        (from[0] + to[0]) / 2.0 + side * h * dy / d,
        (from[1] + to[1]) / 2.0 - side * h * dx / d,
    ))
}

/// Reads the lines of a program against the limits of the machine, from the place the head is at.
pub fn analyze(entries: &[Entry], start: [f64; 3], cfg: &CncConfig) -> Result<Analysis, CncError> {
    let mut a = Analysis {
        lines: Vec::with_capacity(entries.len()),
        after: Vec::with_capacity(entries.len()),
        rapid_moves: 0,
        cut_moves: 0,
        arcs: 0,
        travel_mm: 0.0,
        estimate_s: 0.0,
        max_feed: 0.0,
        max_rpm: 0.0,
        bounds: None,
        out_of_travel: Vec::new(),
    };
    let mut pos = start;
    let (mut absolute, mut scale, mut motion) = (true, 1.0f64, 0i32);
    let (mut feed, mut rpm) = (0.0f64, 0.0f64);
    for e in entries {
        let line_no = e.raw + 1;
        let words = gcode::words(&e.text);
        let mut target: [Option<f64>; 3] = [None; 3];
        let (mut ij, mut radius) = ((None, None), None);
        for &(letter, v) in &words {
            match letter {
                'G' => match (v * 10.0).round() as i32 {
                    0 => motion = 0,
                    10 => motion = 1,
                    20 => motion = 2,
                    30 => motion = 3,
                    200 => scale = 25.4,
                    210 => scale = 1.0,
                    900 => absolute = true,
                    910 => absolute = false,
                    280 | 300 | 100 | 920..=923 => {
                        return fail(
                            "bad_gcode",
                            format!(
                                "line {line_no}: G{} moves or shifts the coordinates where the server cannot follow it",
                                num(v)
                            ),
                        )
                    }
                    _ => {}
                },
                'F' => {
                    feed = v * scale;
                    a.max_feed = a.max_feed.max(feed);
                }
                'S' => {
                    rpm = v;
                    a.max_rpm = a.max_rpm.max(v);
                    if v > cfg.spindle_max_rpm {
                        return fail(
                            "spindle_limit",
                            format!(
                                "line {line_no}: S{} is over the {} turns the spindle of the machine may be asked",
                                num(v),
                                num(cfg.spindle_max_rpm)
                            ),
                        );
                    }
                }
                'X' => target[0] = Some(v * scale),
                'Y' => target[1] = Some(v * scale),
                'Z' => target[2] = Some(v * scale),
                'I' => ij.0 = Some(v * scale),
                'J' => ij.1 = Some(v * scale),
                'R' => radius = Some(v * scale),
                _ => {}
            }
        }
        let mut to = pos;
        let mut moved = false;
        for i in 0..3 {
            if let Some(v) = target[i] {
                to[i] = if absolute { v } else { pos[i] + v };
                moved = true;
            }
        }
        if moved {
            let mut seen: Vec<[f64; 3]> = vec![to];
            if motion >= 2 {
                a.arcs += 1;
                let clockwise = motion == 2;
                let center = match (ij, radius) {
                    ((None, None), Some(r)) => {
                        center_from_radius(pos, to, r, clockwise).map_err(|why| CncError {
                            code: "bad_gcode",
                            detail: format!("line {line_no}: {why}"),
                        })?
                    }
                    ((i, j), _) => (pos[0] + i.unwrap_or(0.0), pos[1] + j.unwrap_or(0.0)),
                };
                for (x, y) in arc_extremes(pos, to, center, clockwise) {
                    seen.push([x, y, to[2]]);
                }
            }
            if seen.iter().any(|p| !cfg.inside(*p)) {
                if cfg.reject_out_of_travel {
                    return fail(
                        "out_of_travel",
                        format!(
                            "line {line_no}: the move goes outside the travel of the machine (X {}, Y {}, Z {})",
                            travel_text(cfg.travel[0], false),
                            travel_text(cfg.travel[1], false),
                            travel_text(cfg.travel[2], true)
                        ),
                    );
                }
                a.out_of_travel.push(line_no);
            }
            for p in seen {
                a.bounds = Some(match a.bounds {
                    None => (p, p),
                    Some((lo, hi)) => (
                        [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                        [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
                    ),
                });
            }
            let d =
                ((to[0] - pos[0]).powi(2) + (to[1] - pos[1]).powi(2) + (to[2] - pos[2]).powi(2))
                    .sqrt();
            a.travel_mm += d;
            let speed = if motion == 0 {
                a.rapid_moves += 1;
                cfg.rapid_mm_min
            } else {
                a.cut_moves += 1;
                if feed > 0.0 {
                    feed
                } else {
                    1000.0
                }
            };
            a.estimate_s += d / speed * 60.0;
            pos = to;
        }
        a.lines.push(e.text.clone());
        a.after.push((pos, rpm));
    }
    Ok(a)
}

fn travel_text(t: Option<f64>, z: bool) -> String {
    match t {
        None => "not checked".into(),
        Some(t) if z => format!("{} to {}", num(-t), num(t)),
        Some(t) => format!("0 to {}", num(t)),
    }
}

/// What a client draws and reads of a job that was prepared.
fn view_of(kind: &str, a: &Analysis, cfg: &CncConfig, skipped: usize) -> Value {
    let stride = a.after.len().div_ceil(2000).max(1);
    let path: Vec<Value> = a
        .after
        .iter()
        .step_by(stride)
        .map(|(p, _)| json!([round2(p[0]), round2(p[1]), round2(p[2])]))
        .collect();
    let bounds = a.bounds.map(|(lo, hi)| {
        json!({
            "min": { "x": lo[0], "y": lo[1], "z": lo[2] },
            "max": { "x": hi[0], "y": hi[1], "z": hi[2] },
        })
    });
    json!({
        "kind": kind,
        "lines": a.lines.len(),
        "skipped_lines": skipped,
        "rapid_moves": a.rapid_moves,
        "cut_moves": a.cut_moves,
        "arcs": a.arcs,
        "travel_mm": round2(a.travel_mm),
        "estimate_s": round2(a.estimate_s),
        "max_feed_mm_min": a.max_feed,
        "max_rpm": a.max_rpm,
        "bounds": bounds,
        "travel_check": if cfg.travel_known() { "on" } else { "off" },
        "out_of_travel_lines": a.out_of_travel,
        "path": path,
    })
}

// ---- the machines and their jobs -----------------------------------------------------------------------------------------------------

/// What a CNC machine is at the moment.
#[derive(Debug, Clone, Default)]
struct MachineNow {
    pos: [f64; 3],
    rpm: f64,
    latched: bool,
}

struct JobPath {
    after: Vec<([f64; 3], f64)>,
}

/// The CNC machines of the server: their jobs and what each one is now.
#[derive(Default)]
pub struct CncEngine {
    book: SimBook,
    paths: Mutex<HashMap<String, JobPath>>,
    now: Mutex<HashMap<String, MachineNow>>,
}

fn latched_error() -> PluginError {
    PluginError::Coded(
        "machine_alarm",
        "an emergency stop latched the machine: an administrator has to release it".into(),
    )
}

impl CncEngine {
    pub fn busy(&self, machine: &str) -> bool {
        self.book.busy(machine)
    }

    pub fn latched(&self, machine: &str) -> bool {
        self.now.locked().get(machine).is_some_and(|m| m.latched)
    }

    fn here(&self, machine: &str) -> [f64; 3] {
        self.now.locked().get(machine).map_or([0.0; 3], |m| m.pos)
    }

    /// Brings the position of the head up to the lines the running job has done.
    fn follow(&self, machine: &str) {
        let Some(id) = self.book.active_on(machine) else {
            return;
        };
        let _ = self.status(&id);
    }

    /// What a client sees of the machine besides its state.
    pub fn detail(&self, machine: &str, cfg: &CncConfig) -> Value {
        self.follow(machine);
        let now = self.now.locked().get(machine).cloned().unwrap_or_default();
        json!({
            "latched": now.latched,
            "position_mm": { "x": now.pos[0], "y": now.pos[1], "z": now.pos[2] },
            "spindle_rpm": now.rpm,
            "travel_mm": {
                "x": cfg.travel[0], "y": cfg.travel[1], "z": cfg.travel[2],
                "check": if cfg.travel_known() { "on" } else { "off" },
            },
            "reject_out_of_travel": cfg.reject_out_of_travel,
            "spindle_max_rpm": cfg.spindle_max_rpm,
            "latch_on_stop": cfg.latch_on_stop,
        })
    }

    pub fn prepare(
        &self,
        machine: &str,
        cfg: &CncConfig,
        request: &Value,
    ) -> PluginResult<Prepared> {
        if self.latched(machine) {
            return Err(latched_error());
        }
        let kind = request.get("kind").and_then(Value::as_str).unwrap_or("");
        let text = match kind {
            "gcode" => request
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    PluginError::Bad("a gcode request has the program in `text`".into())
                })?,
            "file" => request
                .get("artifact_text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    PluginError::Bad(
                        "a file request names a G-code artifact of the store in `artifact`".into(),
                    )
                })?,
            _ => {
                return Err(PluginError::Bad(
                    "the request has to be {\"kind\": \"gcode\" | \"file\", ...}".into(),
                ))
            }
        };
        let skip = match request.get("skip") {
            None => 0,
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| PluginError::Bad("skip is a whole number of lines".into()))?,
        };
        let restore = match request.get("restore_outputs") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(PluginError::Bad("restore_outputs is true or false".into())),
        };
        let program = gcode::parse(&text, skip, restore).map_err(|e| CncError {
            code: e.code,
            detail: e.detail,
        })?;
        if program.entries.len() > MAX_LINES {
            return Err(PluginError::Coded(
                "too_big",
                format!("a job may have {MAX_LINES} lines at the most"),
            ));
        }
        let analysis = analyze(&program.entries, self.here(machine), cfg)?;
        let view = view_of(kind, &analysis, cfg, skip);
        let cumulative = evenly(analysis.lines.len(), cfg.step_ms / 1000.0);
        let native_id = self.book.add(machine, "gcode", cumulative, 1.0);
        self.paths.locked().insert(
            native_id.clone(),
            JobPath {
                after: analysis.after,
            },
        );
        Ok(Prepared { native_id, view })
    }

    pub fn start(&self, machine: &str, native_id: &str) -> PluginResult<()> {
        if self.latched(machine) {
            return Err(latched_error());
        }
        self.book.start(machine, native_id)
    }

    pub fn pause(&self, native_id: &str) -> PluginResult<()> {
        self.book.pause(native_id)?;
        let _ = self.status(native_id);
        Ok(())
    }

    pub fn resume(&self, native_id: &str) -> PluginResult<()> {
        self.book.resume(native_id)
    }

    /// The stop of a job: the feed hold and the reset. The machine is not latched by it.
    pub fn stop(&self, native_id: &str) -> PluginResult<()> {
        let machine = self.book.peek(native_id).map(|p| p.machine);
        let done = self
            .book
            .stop(native_id, "stopped: feed hold and reset sent")?;
        if let Some(machine) = machine {
            self.land(&machine, native_id, done, false);
        }
        Ok(())
    }

    /// The head is where the last line that was done left it, and the spindle is off when the job is not running.
    fn land(&self, machine: &str, id: &str, done: usize, running: bool) {
        let paths = self.paths.locked();
        let mut now = self.now.locked();
        let rt = now.entry(machine.to_string()).or_default();
        if let Some(path) = paths.get(id) {
            if let Some((pos, rpm)) = done.checked_sub(1).and_then(|i| path.after.get(i)) {
                rt.pos = *pos;
                rt.rpm = if running { *rpm } else { 0.0 };
                return;
            }
        }
        if !running {
            rt.rpm = 0.0;
        }
    }

    pub fn status(&self, native_id: &str) -> PluginResult<PluginStatus> {
        let status = self.book.status(native_id)?;
        if let Some(p) = self.book.peek(native_id) {
            let running = status.state == WorkState::Running;
            if running || status.state == WorkState::Done || status.state == WorkState::Paused {
                self.land(&p.machine, native_id, p.done, running);
            }
        }
        Ok(status)
    }

    pub fn discard(&self, native_id: &str) {
        self.book.discard(native_id);
        self.paths.locked().remove(native_id);
    }

    // ---- the operations that are not a job ----

    /// Where the machine is: the machine and the work position, the state word and the values of the overrides.
    pub fn position(&self, machine: &str) -> Value {
        self.follow(machine);
        let now = self.now.locked().get(machine).cloned().unwrap_or_default();
        let state = if now.latched {
            "Alarm"
        } else if self.book.busy(machine) {
            "Run"
        } else {
            "Idle"
        };
        json!({
            "machine_mm": { "x": now.pos[0], "y": now.pos[1], "z": now.pos[2] },
            "work_mm": { "x": now.pos[0], "y": now.pos[1], "z": now.pos[2] },
            "state": state,
            "spindle_rpm": now.rpm,
            "overrides": { "feed_pct": 100, "rapid_pct": 100, "spindle_pct": 100 },
            "latched": now.latched,
        })
    }

    /// One jog: an axis, a signed distance and a feed, inside the limits of the machine; refused while a job runs or the machine is latched.
    pub fn jog(&self, machine: &str, cfg: &CncConfig, body: &Value) -> PluginResult<Value> {
        let axis = body
            .get("axis")
            .and_then(Value::as_str)
            .and_then(|s| s.chars().next().filter(|_| s.chars().count() == 1))
            .map(|c| c.to_ascii_lowercase())
            .filter(|c| AXES.contains(c))
            .ok_or_else(|| PluginError::Coded("bad_axis", "axis is one of x, y or z".into()))?;
        let distance = body
            .get("distance_mm")
            .and_then(Value::as_f64)
            .filter(|d| d.is_finite() && *d != 0.0 && d.abs() <= cfg.jog_max_mm)
            .ok_or_else(|| {
                PluginError::Bad(format!(
                    "distance_mm is a number that is not 0 and is at most {} (jog_max_mm)",
                    num(cfg.jog_max_mm)
                ))
            })?;
        let feed = match body.get("feed") {
            None => cfg.jog_max_feed.min(1000.0),
            Some(v) => v
                .as_f64()
                .filter(|f| f.is_finite() && *f > 0.0 && *f <= cfg.jog_max_feed)
                .ok_or_else(|| {
                    PluginError::Coded(
                        "bad_feed",
                        format!(
                            "feed is a number above 0 and at most {} mm/min (jog_max_feed)",
                            num(cfg.jog_max_feed)
                        ),
                    )
                })?,
        };
        if self.latched(machine) {
            return Err(latched_error());
        }
        if self.book.busy(machine) {
            return Err(PluginError::Coded(
                "jog_while_running",
                "a job is running or held on the machine: stop it first".into(),
            ));
        }
        let mut now = self.now.locked();
        let rt = now.entry(machine.to_string()).or_default();
        let i = AXES.iter().position(|c| *c == axis).unwrap_or(0);
        let mut to = rt.pos;
        to[i] += distance;
        if !cfg.inside(to) {
            return Err(PluginError::Coded(
                "out_of_travel",
                format!("the jog would leave the travel of the axis {axis}"),
            ));
        }
        rt.pos = to;
        Ok(json!({
            "axis": axis.to_string(), "distance_mm": distance, "feed": feed,
            "position_mm": { "x": to[0], "y": to[1], "z": to[2] },
        }))
    }

    /// The emergency stop: the job that runs ends at once and the machine is latched (unless the machine is set not to latch).
    pub fn emergency_stop(&self, machine: &str, cfg: &CncConfig) -> Value {
        let mut stopped = false;
        if let Some(id) = self.book.active_on(machine) {
            if let Ok(done) = self.book.stop(
                &id,
                "emergency stop: the machine is latched until an administrator releases it",
            ) {
                self.land(machine, &id, done, false);
                stopped = true;
            }
        }
        let mut now = self.now.locked();
        let rt = now.entry(machine.to_string()).or_default();
        rt.rpm = 0.0;
        if cfg.latch_on_stop {
            rt.latched = true;
        }
        json!({ "stopped": stopped, "latched": rt.latched })
    }

    /// An administrator looked at the machine: the latch is released.
    pub fn release(&self, machine: &str) -> Value {
        let mut now = self.now.locked();
        let rt = now.entry(machine.to_string()).or_default();
        let was = rt.latched;
        rt.latched = false;
        json!({ "released": was, "latched": false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(text: &str) -> Vec<Entry> {
        gcode::parse(text, 0, false).unwrap().entries
    }

    fn cfg_with(travel: f64) -> CncConfig {
        CncConfig {
            travel: [Some(travel), Some(travel), Some(20.0)],
            step_ms: 0.0,
            ..CncConfig::default()
        }
    }

    fn request(text: &str) -> Value {
        json!({"kind": "gcode", "text": text})
    }

    #[test]
    fn the_settings_have_ranges_and_the_defaults_are_the_conservative_ones() {
        let d = CncConfig::from_settings(&json!({})).unwrap();
        assert_eq!(d, CncConfig::default());
        assert!(d.reject_out_of_travel && d.latch_on_stop);
        assert!(!d.travel_known());
        let c = CncConfig::from_settings(&json!({
            "travel_x_mm": 300, "travel_y_mm": 200, "travel_z_mm": 40, "jog_max_mm": 10,
            "connection": {"kind": "demo"}, "step_ms": 5
        }))
        .unwrap();
        assert_eq!(c.travel, [Some(300.0), Some(200.0), Some(40.0)]);
        assert!(c.travel_known());
        for bad in [
            json!({"travel_x_mm": 6000}),
            json!({"jog_max_mm": 0}),
            json!({"jog_max_feed": 40000}),
            json!({"spindle_max_rpm": -1}),
            json!({"latch_on_stop": "yes"}),
            json!({"reject_out_of_travel": 1}),
            json!({"nonsense": 1}),
            json!({"connection": {"kind": "grbl"}}),
            json!({"connection": {"kind": "node"}}),
            json!({"connection": "demo"}),
        ] {
            assert!(CncConfig::from_settings(&bad).is_err(), "{bad}");
        }
        assert_eq!(SETTINGS_HELP.len(), 11);
    }

    #[test]
    fn a_program_is_read_for_its_moves_and_its_box() {
        let text = "G21 G90\nG0 X10 Y10 Z5\nG1 Z-1 F300 S9000\nG1 X30 Y10\nG2 X40 Y20 I0 J10\n";
        let a = analyze(&entries(text), [0.0; 3], &cfg_with(100.0)).unwrap();
        assert_eq!((a.rapid_moves, a.cut_moves, a.arcs), (1, 3, 1));
        assert_eq!(a.max_feed, 300.0);
        assert_eq!(a.max_rpm, 9000.0);
        let (lo, hi) = a.bounds.unwrap();
        assert_eq!((lo[2], hi[2]), (-1.0, 5.0));
        assert!(hi[0] >= 40.0 && hi[1] >= 20.0);
        assert!(a.travel_mm > 40.0 && a.estimate_s > 0.0);
        assert_eq!(a.lines.len(), a.after.len());
        assert_eq!(a.after[1].0, [10.0, 10.0, 5.0]);
        assert!(a.out_of_travel.is_empty());
    }

    #[test]
    fn relative_moves_and_inches_are_worked_out() {
        let a = analyze(
            &entries("G20\nG91\nG1 X1 F10\nG1 X1\n"),
            [0.0; 3],
            &cfg_with(100.0),
        )
        .unwrap();
        assert_eq!(a.after[2].0[0], 25.4);
        assert_eq!(a.after[3].0[0], 50.8);
        assert!((a.max_feed - 254.0).abs() < 1e-9);
        let from = analyze(
            &entries("G91\nG1 X5 F100\n"),
            [10.0, 0.0, 0.0],
            &cfg_with(100.0),
        )
        .unwrap();
        assert_eq!(from.after[1].0[0], 15.0);
    }

    #[test]
    fn a_move_outside_the_travel_is_refused_or_listed() {
        let cfg = cfg_with(100.0);
        for text in ["G1 X101 F100", "G1 Y-1 F100", "G1 Z21 F100", "G1 Z-21 F100"] {
            let e = analyze(&entries(text), [0.0; 3], &cfg).unwrap_err();
            assert_eq!(e.code, "out_of_travel", "{text}");
            assert!(e.detail.contains("line 1"));
        }
        assert!(analyze(&entries("G1 X100 Y100 Z-20 F100"), [0.0; 3], &cfg).is_ok());
        let lax = CncConfig {
            reject_out_of_travel: false,
            ..cfg_with(100.0)
        };
        let a = analyze(&entries("G1 X1 F100\nG1 X200\n"), [0.0; 3], &lax).unwrap();
        assert_eq!(a.out_of_travel, vec![2]);
        let none = CncConfig::default();
        assert!(analyze(&entries("G1 X9999 F100"), [0.0; 3], &none).is_ok());
    }

    #[test]
    fn an_arc_that_bulges_out_of_the_travel_is_refused_though_its_ends_are_inside() {
        let cfg = CncConfig {
            travel: [Some(100.0), Some(60.0), Some(20.0)],
            step_ms: 0.0,
            ..CncConfig::default()
        };
        // from (30,50) to (70,50) around (50,50): clockwise it goes over the top to y 70, counter-clockwise under to y 30
        let over = analyze(
            &entries(
                "G0 X30 Y50
G2 X70 Y50 I20 J0",
            ),
            [0.0; 3],
            &cfg,
        );
        assert_eq!(over.unwrap_err().code, "out_of_travel");
        let under = analyze(
            &entries(
                "G0 X30 Y50
G3 X70 Y50 I20 J0",
            ),
            [0.0; 3],
            &cfg,
        )
        .unwrap();
        let (lo, hi) = under.bounds.unwrap();
        assert!((lo[1] - 30.0).abs() < 1e-9 && (hi[1] - 50.0).abs() < 1e-9);
        let full = analyze(
            &entries(
                "G0 X30 Y50
G2 X30 Y50 I20 J0",
            ),
            [0.0; 3],
            &cfg,
        );
        assert_eq!(
            full.unwrap_err().code,
            "out_of_travel",
            "a circle comes back to its start and passes every side"
        );
    }

    #[test]
    fn an_arc_given_by_its_radius_finds_its_center() {
        let c = center_from_radius([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 1.0, false).unwrap();
        assert!(c.0.abs() < 1e-9 && c.1.abs() < 1e-9);
        let cw = center_from_radius([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], 1.0, true).unwrap();
        assert!(cw.0.abs() < 1e-9 && cw.1.abs() < 1e-9);
        assert!(center_from_radius([0.0; 3], [10.0, 0.0, 0.0], 1.0, true).is_err());
        assert!(center_from_radius([0.0; 3], [0.0; 3], 1.0, true).is_err());
        let a = analyze(
            &entries("G0 X1 Y0\nG3 X0 Y1 R1"),
            [0.0; 3],
            &cfg_with(100.0),
        )
        .unwrap();
        assert_eq!(a.arcs, 1);
        assert!((a.bounds.unwrap().1[0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_spindle_and_the_words_that_shift_the_coordinates_are_refused() {
        let cfg = cfg_with(100.0);
        let e = analyze(&entries("M3 S20000"), [0.0; 3], &cfg).unwrap_err();
        assert_eq!(e.code, "spindle_limit");
        for text in ["G28", "G30", "G92 X0", "G10 L2 P1 X0"] {
            let e = analyze(&entries(text), [0.0; 3], &cfg).unwrap_err();
            assert_eq!(e.code, "bad_gcode", "{text}");
        }
        assert!(analyze(&entries("M3 S12000"), [0.0; 3], &cfg).is_ok());
    }

    #[test]
    fn a_job_runs_on_the_simulator_and_the_head_follows_the_lines() {
        let engine = CncEngine::default();
        let cfg = cfg_with(100.0);
        let p = engine
            .prepare("m1", &cfg, &request("G0 X10\nG0 X20\nG0 X30\n"))
            .unwrap();
        assert_eq!(p.view["lines"], 3);
        assert_eq!(p.view["travel_check"], "on");
        assert_eq!(
            engine.status(&p.native_id).unwrap().state,
            WorkState::Queued
        );
        engine.start("m1", &p.native_id).unwrap();
        let st = engine.status(&p.native_id).unwrap();
        assert_eq!((st.state, st.done, st.total), (WorkState::Done, 3, 3));
        assert_eq!(engine.position("m1")["machine_mm"]["x"], 30.0);
        assert!(!engine.busy("m1"));
        engine.discard(&p.native_id);
        assert!(engine.status(&p.native_id).is_err());
    }

    #[test]
    fn a_stop_ends_the_job_and_does_not_latch_but_an_emergency_stop_does() {
        let engine = CncEngine::default();
        let cfg = CncConfig {
            step_ms: 3_600_000.0,
            ..cfg_with(100.0)
        };
        let p = engine
            .prepare("m1", &cfg, &request("G0 X10\nG0 X20\n"))
            .unwrap();
        engine.start("m1", &p.native_id).unwrap();
        assert!(engine.busy("m1"));
        assert_eq!(engine.position("m1")["state"], "Run");
        engine.pause(&p.native_id).unwrap();
        engine.resume(&p.native_id).unwrap();
        engine.stop(&p.native_id).unwrap();
        assert_eq!(
            engine.status(&p.native_id).unwrap().state,
            WorkState::Cancelled
        );
        assert!(!engine.latched("m1"));
        let q = engine.prepare("m1", &cfg, &request("G0 X5\n")).unwrap();
        engine.start("m1", &q.native_id).unwrap();
        let out = engine.emergency_stop("m1", &cfg);
        assert_eq!(out, json!({"stopped": true, "latched": true}));
        assert_eq!(
            engine.status(&q.native_id).unwrap().state,
            WorkState::Cancelled
        );
        assert_eq!(engine.position("m1")["state"], "Alarm");
        let refused = engine
            .prepare("m1", &cfg, &request("G0 X5\n"))
            .err()
            .unwrap();
        assert_eq!(refused.code(), "machine_alarm");
        assert_eq!(refused.status(), 409);
        assert!(engine
            .jog("m1", &cfg, &json!({"axis": "x", "distance_mm": 1}))
            .is_err());
        assert_eq!(
            engine.release("m1"),
            json!({"released": true, "latched": false})
        );
        assert_eq!(
            engine.release("m1"),
            json!({"released": false, "latched": false})
        );
        assert!(engine.prepare("m1", &cfg, &request("G0 X5\n")).is_ok());
    }

    #[test]
    fn an_emergency_stop_with_no_job_latches_and_a_machine_set_not_to_latch_does_not() {
        let engine = CncEngine::default();
        let cfg = cfg_with(100.0);
        assert_eq!(
            engine.emergency_stop("m1", &cfg),
            json!({"stopped": false, "latched": true})
        );
        let loose = CncConfig {
            latch_on_stop: false,
            ..cfg_with(100.0)
        };
        assert_eq!(
            engine.emergency_stop("m2", &loose),
            json!({"stopped": false, "latched": false})
        );
        assert!(!engine.latched("m2"));
    }

    #[test]
    fn a_latched_machine_does_not_start_a_job_that_was_prepared_before() {
        let engine = CncEngine::default();
        let cfg = cfg_with(100.0);
        let p = engine.prepare("m1", &cfg, &request("G0 X5\n")).unwrap();
        engine.emergency_stop("m1", &cfg);
        let e = engine.start("m1", &p.native_id).unwrap_err();
        assert_eq!(e.code(), "machine_alarm");
    }

    #[test]
    fn the_jog_has_its_limits() {
        let engine = CncEngine::default();
        let cfg = CncConfig {
            jog_max_mm: 10.0,
            jog_max_feed: 500.0,
            ..cfg_with(100.0)
        };
        let ok = engine
            .jog(
                "m1",
                &cfg,
                &json!({"axis": "X", "distance_mm": 5, "feed": 400}),
            )
            .unwrap();
        assert_eq!(ok["position_mm"]["x"], 5.0);
        assert_eq!(ok["axis"], "x");
        for (body, code) in [
            (json!({"axis": "w", "distance_mm": 1}), "bad_axis"),
            (json!({"axis": "xy", "distance_mm": 1}), "bad_axis"),
            (json!({"distance_mm": 1}), "bad_axis"),
            (
                json!({"axis": "x", "distance_mm": 11}),
                "bad_plugin_request",
            ),
            (json!({"axis": "x", "distance_mm": 0}), "bad_plugin_request"),
            (
                json!({"axis": "x", "distance_mm": "1"}),
                "bad_plugin_request",
            ),
            (
                json!({"axis": "x", "distance_mm": 1, "feed": 501}),
                "bad_feed",
            ),
            (
                json!({"axis": "x", "distance_mm": 1, "feed": 0}),
                "bad_feed",
            ),
            (json!({"axis": "x", "distance_mm": -10}), "out_of_travel"),
        ] {
            let e = engine.jog("m1", &cfg, &body).unwrap_err();
            assert_eq!(e.code(), code, "{body}");
        }
        let p = engine
            .prepare(
                "m1",
                &CncConfig {
                    step_ms: 3_600_000.0,
                    ..cfg.clone()
                },
                &request("G0 X10\n"),
            )
            .unwrap();
        engine.start("m1", &p.native_id).unwrap();
        let e = engine
            .jog("m1", &cfg, &json!({"axis": "x", "distance_mm": 1}))
            .unwrap_err();
        assert_eq!(e.code(), "jog_while_running");
        assert_eq!(e.status(), 409);
    }

    #[test]
    fn a_request_that_is_not_a_program_is_refused() {
        let engine = CncEngine::default();
        let cfg = cfg_with(100.0);
        for (req, why) in [
            (json!({"kind": "gcode"}), "text"),
            (json!({"kind": "file"}), "artifact"),
            (json!({"kind": "drill"}), "kind"),
            (
                json!({"kind": "gcode", "text": "G0 X1", "skip": -1}),
                "skip",
            ),
            (
                json!({"kind": "gcode", "text": "G0 X1", "restore_outputs": 1}),
                "restore",
            ),
        ] {
            let e = engine.prepare("m1", &cfg, &req).err().unwrap();
            assert_eq!(e.code(), "bad_plugin_request", "{why}");
        }
        let e = engine.prepare("m1", &cfg, &request("$H")).err().unwrap();
        assert_eq!(e.code(), "bad_gcode");
        let e = engine
            .prepare("m1", &cfg, &request("(just a comment)"))
            .err()
            .unwrap();
        assert_eq!(e.code(), "no_work");
        let file = engine
            .prepare(
                "m1",
                &cfg,
                &json!({"kind": "file", "artifact": "a-1", "artifact_text": "G0 X3\n"}),
            )
            .unwrap();
        assert_eq!(file.view["kind"], "file");
    }

    #[test]
    fn a_program_that_goes_on_after_a_stop_starts_where_the_head_is() {
        let engine = CncEngine::default();
        let big = CncConfig {
            jog_max_mm: 50.0,
            ..cfg_with(100.0)
        };
        engine
            .jog("m1", &big, &json!({"axis": "x", "distance_mm": 40}))
            .unwrap();
        let p = engine
            .prepare(
                "m1",
                &big,
                &json!({"kind": "gcode", "text": "G91\nG1 X70 F100\n"}),
            )
            .err()
            .unwrap();
        assert_eq!(p.code(), "out_of_travel", "40 plus 70 is past 100");
        let ok = engine
            .prepare(
                "m1",
                &big,
                &json!({"kind": "gcode", "text": "G91\nG1 X50 F100\n"}),
            )
            .unwrap();
        assert_eq!(ok.view["bounds"]["max"]["x"], 90.0);
        let d = engine.detail("m1", &big);
        assert_eq!(d["position_mm"]["x"], 40.0);
        assert_eq!(d["travel_mm"]["check"], "on");
        assert_eq!(d["latched"], false);
    }
}
