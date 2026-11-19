//! The laser machines with no hardware behind: the reading and checking of a G-code job, the limits of power, feed and bed, the latch of the emergency stop and the
//! simulated run. The plugin `laser` (`src/plugin_laser.rs`) only hands the machines of the server to this engine.
//!
//! A job never reaches a machine here, so everything that could hurt is checked when the job is prepared: the text is read word by word and written again from the
//! words it was understood from (nothing else passes), every `S` is held to the power limit of the machine (refused, or cut to the limit when the machine says
//! `clamp`), the feed to its limit, every move to the bed, and a rapid move with the beam on in constant mode (`M3`) is refused. A program that ends with the beam on gets
//! an `M5`. The emergency stop (`stop` of a job that runs or is held) ends the job at once and, unless the machine is set not to, latches the machine: nothing starts on it
//! until a job of the kind `rearm` has run, so a person has to say that the machine was looked at.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::plugins::{PluginError, PluginResult, PluginStatus, Prepared, WorkState};
use crate::simclock::{finished_items, SimClock};
use crate::sync::Locked;

/// The most lines of code a laser job may have.
pub const MAX_LINES: usize = 200_000;
/// The longest line, in characters.
pub const MAX_LINE_LEN: usize = 200;
/// The longest dwell (`G4`) of a job, in seconds.
pub const MAX_DWELL_S: f64 = 600.0;
/// The most points of the drawing of a job that is sent to the client.
const PREVIEW_POINTS: usize = 2000;
/// The slack on the edges of the bed, in mm (rounding of the numbers of a program).
const EDGE_EPS: f64 = 1e-6;

/// The settings a laser machine may have: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("bed_x_mm", "width of the bed, 10 to 5000 (400 by default)"),
    ("bed_y_mm", "depth of the bed, 10 to 5000 (400 by default)"),
    ("max_s", "the value of S that is full power, 1 to 65535 (1000 by default)"),
    ("power_limit_pct", "the most power a job may ask, in percent of max_s, 1 to 100 (40 by default)"),
    ("over_limit", "`reject` (default) refuses a job that asks for more power than the limit, `clamp` cuts it to the limit"),
    ("max_feed_mm_min", "the fastest cutting feed, 1 to 100000 (6000 by default)"),
    ("rapid_mm_min", "the speed of a rapid move, 1 to 100000 (12000 by default)"),
    ("latch_on_stop", "an emergency stop latches the machine until a `rearm` job runs (true by default)"),
    ("test_fire_max_s", "the longest test fire, 0.01 to 10 seconds (1 by default)"),
    ("sim_rate", "how many times faster than real time the simulated machine works, 0 to 100000 (1 by default; 0 is no waiting)"),
];

/// How the machine takes a program that asks for more power than its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverLimit {
    Reject,
    Clamp,
}

/// The settings of a laser machine, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct LaserConfig {
    pub bed_x: f64,
    pub bed_y: f64,
    pub max_s: f64,
    pub power_limit_pct: f64,
    pub over_limit: OverLimit,
    pub max_feed: f64,
    pub rapid: f64,
    pub latch_on_stop: bool,
    pub test_fire_max_s: f64,
    pub sim_rate: f64,
}

impl Default for LaserConfig {
    fn default() -> Self {
        LaserConfig {
            bed_x: 400.0,
            bed_y: 400.0,
            max_s: 1000.0,
            power_limit_pct: 40.0,
            over_limit: OverLimit::Reject,
            max_feed: 6000.0,
            rapid: 12000.0,
            latch_on_stop: true,
            test_fire_max_s: 1.0,
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

impl LaserConfig {
    /// Reads the settings of a machine: a key that is not one of [`SETTINGS_HELP`] or a value out of its range is refused.
    pub fn from_settings(settings: &Value) -> Result<LaserConfig, String> {
        let mut cfg = LaserConfig::default();
        if let Some(map) = settings.as_object() {
            for key in map.keys() {
                if !SETTINGS_HELP.iter().any(|(k, _)| k == key) {
                    return Err(format!("{key} is not a setting of a laser machine"));
                }
            }
        }
        if let Some(n) = number_in(settings, "bed_x_mm", 10.0, 5000.0)? {
            cfg.bed_x = n;
        }
        if let Some(n) = number_in(settings, "bed_y_mm", 10.0, 5000.0)? {
            cfg.bed_y = n;
        }
        if let Some(n) = number_in(settings, "max_s", 1.0, 65535.0)? {
            cfg.max_s = n;
        }
        if let Some(n) = number_in(settings, "power_limit_pct", 1.0, 100.0)? {
            cfg.power_limit_pct = n;
        }
        if let Some(n) = number_in(settings, "max_feed_mm_min", 1.0, 100_000.0)? {
            cfg.max_feed = n;
        }
        if let Some(n) = number_in(settings, "rapid_mm_min", 1.0, 100_000.0)? {
            cfg.rapid = n;
        }
        if let Some(n) = number_in(settings, "test_fire_max_s", 0.01, 10.0)? {
            cfg.test_fire_max_s = n;
        }
        if let Some(n) = number_in(settings, "sim_rate", 0.0, 100_000.0)? {
            cfg.sim_rate = n;
        }
        match settings.get("over_limit").map(Value::as_str) {
            None => {}
            Some(Some("reject")) => cfg.over_limit = OverLimit::Reject,
            Some(Some("clamp")) => cfg.over_limit = OverLimit::Clamp,
            Some(_) => return Err("over_limit is `reject` or `clamp`".into()),
        }
        match settings.get("latch_on_stop") {
            None => {}
            Some(Value::Bool(b)) => cfg.latch_on_stop = *b,
            Some(_) => return Err("latch_on_stop is true or false".into()),
        }
        Ok(cfg)
    }

    /// The highest value of `S` a job may ask.
    pub fn limit_s(&self) -> f64 {
        self.max_s * self.power_limit_pct / 100.0
    }
}

/// A refusal of the reading of a job: one of the codes of the catalog of errors and what is wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct LaserError {
    pub code: &'static str,
    pub detail: String,
}

fn fail<T>(code: &'static str, detail: String) -> Result<T, LaserError> {
    Err(LaserError { code, detail })
}

impl From<LaserError> for PluginError {
    fn from(e: LaserError) -> PluginError {
        PluginError::Coded(e.code, e.detail)
    }
}

/// What a line of the program leaves behind: where the head is, the power in force, whether the beam is on and the seconds since the start.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub x: f64,
    pub y: f64,
    pub s: f64,
    pub beam: bool,
    pub t: f64,
}

/// A program that was read and is inside every limit.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    /// The lines that would be sent, written again from the words that were understood.
    pub lines: Vec<String>,
    /// One step per line.
    pub steps: Vec<Step>,
    pub total_s: f64,
    pub length_mm: f64,
    pub cut_mm: f64,
    pub max_s_seen: f64,
    pub clamped: usize,
    pub beam_off_added: bool,
    /// The box of the moves with the beam on: `(min_x, min_y, max_x, max_y)`.
    pub cut_box: Option<(f64, f64, f64, f64)>,
}

/// The limits a program is read against.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub bed_x: f64,
    pub bed_y: f64,
    pub limit_s: f64,
    pub max_s: f64,
    pub max_feed: f64,
    pub rapid: f64,
    pub clamp: bool,
}

impl Limits {
    pub fn of(cfg: &LaserConfig) -> Limits {
        Limits {
            bed_x: cfg.bed_x,
            bed_y: cfg.bed_y,
            limit_s: cfg.limit_s(),
            max_s: cfg.max_s,
            max_feed: cfg.max_feed,
            rapid: cfg.rapid,
            clamp: cfg.over_limit == OverLimit::Clamp,
        }
    }

    fn inside(&self, x: f64, y: f64) -> bool {
        (-EDGE_EPS..=self.bed_x + EDGE_EPS).contains(&x)
            && (-EDGE_EPS..=self.bed_y + EDGE_EPS).contains(&y)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Beam {
    Off,
    Constant,
    Dynamic,
}

/// The code of a line: comments in parentheses and after `;` removed.
fn strip(line: &str) -> String {
    let mut out = String::new();
    let mut in_comment = false;
    for c in line.chars() {
        match c {
            ';' if !in_comment => break,
            '(' if !in_comment => in_comment = true,
            ')' if in_comment => in_comment = false,
            _ if in_comment => {}
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

/// The words of a line: a letter and a number each.
fn parse_words(code: &str) -> Result<Vec<(char, f64)>, String> {
    let chars: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            return Err(format!("`{c}` is not part of a word of G-code"));
        }
        let letter = c.to_ascii_uppercase();
        i += 1;
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_digit() || matches!(chars[i], '+' | '-' | '.'))
        {
            i += 1;
        }
        let text: String = chars[start..i].iter().collect();
        match text.parse::<f64>() {
            Ok(v) if v.is_finite() => out.push((letter, v)),
            _ => return Err(format!("the word {letter} has no number")),
        }
    }
    Ok(out)
}

/// A number as short as it can be written.
fn num(v: f64) -> String {
    let text = format!("{v:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-0" {
        "0".to_string()
    } else {
        text.to_string()
    }
}

fn whole(v: f64) -> Option<i32> {
    (v.fract() == 0.0 && v.abs() < 1000.0).then_some(v as i32)
}

struct Reader<'a> {
    lim: &'a Limits,
    x: f64,
    y: f64,
    absolute: bool,
    inch: bool,
    motion: Option<i32>,
    feed: f64,
    s: f64,
    beam: Beam,
    time: f64,
    length: f64,
    cut: f64,
    cut_box: Option<(f64, f64, f64, f64)>,
}

impl Reader<'_> {
    fn unit(&self) -> f64 {
        if self.inch {
            25.4
        } else {
            1.0
        }
    }

    fn firing(&self) -> bool {
        self.beam != Beam::Off && self.s > 0.0
    }

    fn add_cut_point(&mut self, x: f64, y: f64) {
        self.cut_box = Some(match self.cut_box {
            None => (x, y, x, y),
            Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
        });
    }

    /// One move to `(tx, ty)`; `arc` is the center and the way round for `G2` and `G3`.
    fn travel(
        &mut self,
        n: usize,
        motion: i32,
        target: (f64, f64),
        arc: Option<(f64, f64)>,
    ) -> Result<(), LaserError> {
        let (tx, ty) = target;
        if !self.lim.inside(tx, ty) {
            return fail(
                "outside_course",
                format!(
                    "line {n}: the move goes to X{} Y{}, outside the bed of {} by {} mm",
                    num(tx),
                    num(ty),
                    num(self.lim.bed_x),
                    num(self.lim.bed_y)
                ),
            );
        }
        let firing = motion != 0 && self.firing();
        let (length, points) = match arc {
            None => (
                ((tx - self.x).powi(2) + (ty - self.y).powi(2)).sqrt(),
                vec![(self.x, self.y), (tx, ty)],
            ),
            Some((cx, cy)) => {
                let r0 = (self.x - cx).hypot(self.y - cy);
                let r1 = (tx - cx).hypot(ty - cy);
                if r0 < 1e-9 {
                    return fail(
                        "bad_gcode",
                        format!("line {n}: an arc needs a center that is not its start"),
                    );
                }
                if (r0 - r1).abs() > 0.5 {
                    return fail(
                        "bad_gcode",
                        format!("line {n}: the end of the arc is not on its circle"),
                    );
                }
                let a0 = (self.y - cy).atan2(self.x - cx);
                let a1 = (ty - cy).atan2(tx - cx);
                let tau = std::f64::consts::TAU;
                let mut sweep = if motion == 2 { a0 - a1 } else { a1 - a0 };
                while sweep <= 1e-12 {
                    sweep += tau;
                }
                while sweep > tau + 1e-12 {
                    sweep -= tau;
                }
                let signed = if motion == 2 { -sweep } else { sweep };
                let mut pts = Vec::with_capacity(33);
                for k in 0..=32 {
                    let f = f64::from(k) / 32.0;
                    let a = a0 + signed * f;
                    let r = r0 + (r1 - r0) * f;
                    pts.push((cx + r * a.cos(), cy + r * a.sin()));
                }
                for (px, py) in &pts {
                    if !self.lim.inside(*px, *py) {
                        return fail(
                            "outside_course",
                            format!("line {n}: the arc goes outside the bed"),
                        );
                    }
                }
                (r0.max(r1) * sweep, pts)
            }
        };
        let speed = if motion == 0 {
            self.lim.rapid
        } else {
            self.feed
        };
        self.time += length / (speed / 60.0);
        self.length += length;
        if firing {
            self.cut += length;
            for (px, py) in points {
                self.add_cut_point(px, py);
            }
        }
        self.x = tx;
        self.y = ty;
        Ok(())
    }
}

/// Reads a program against the limits of the machine. `start` is where the head is when the job starts, `scale_pct` (1 to 100) lowers every `S`.
pub fn analyze(
    text: &str,
    lim: &Limits,
    start: (f64, f64),
    scale_pct: f64,
) -> Result<Analysis, LaserError> {
    let raw: Vec<&str> = text.lines().collect();
    if raw.len() > MAX_LINES {
        return fail(
            "too_big",
            format!("a laser job may have {MAX_LINES} lines at the most"),
        );
    }
    let scale = scale_pct / 100.0;
    let mut rd = Reader {
        lim,
        x: start.0,
        y: start.1,
        absolute: true,
        inch: false,
        motion: None,
        feed: 0.0,
        s: 0.0,
        beam: Beam::Off,
        time: 0.0,
        length: 0.0,
        cut: 0.0,
        cut_box: None,
    };
    let mut lines: Vec<String> = Vec::new();
    let mut steps: Vec<Step> = Vec::new();
    let (mut max_seen, mut clamped) = (0.0f64, 0usize);
    for (index, line) in raw.iter().enumerate() {
        let n = index + 1;
        let code = strip(line);
        if code.is_empty() || code == "%" {
            continue;
        }
        if code.chars().any(char::is_control) {
            return fail(
                "bad_gcode",
                format!("line {n}: control characters are not G-code"),
            );
        }
        if code.starts_with(['$', '!', '~', '?']) {
            return fail(
                "bad_gcode",
                format!("line {n}: a settings or real-time command of the controller is not part of a job"),
            );
        }
        if code.chars().count() > MAX_LINE_LEN {
            return fail(
                "bad_gcode",
                format!("line {n}: longer than {MAX_LINE_LEN} characters"),
            );
        }
        let words = parse_words(&code).or_else(|e| fail("bad_gcode", format!("line {n}: {e}")))?;
        let (mut gs, mut ms) = (Vec::new(), Vec::new());
        let (mut ax, mut ay, mut ai, mut aj) = (None, None, None, None);
        let (mut af, mut asv, mut ap) = (None, None, None);
        for (letter, v) in &words {
            match letter {
                'G' | 'M' => {
                    let Some(k) = whole(*v) else {
                        return fail(
                            "bad_gcode",
                            format!(
                                "line {n}: {letter}{} is not a code this machine takes",
                                num(*v)
                            ),
                        );
                    };
                    if *letter == 'G' {
                        gs.push(k);
                    } else {
                        ms.push(k);
                    }
                }
                'X' => ax = Some(*v),
                'Y' => ay = Some(*v),
                'I' => ai = Some(*v),
                'J' => aj = Some(*v),
                'F' => af = Some(*v),
                'S' => asv = Some(*v),
                'P' => ap = Some(*v),
                'Z' | 'N' => {}
                other => {
                    return fail(
                        "bad_gcode",
                        format!("line {n}: the word {other} is not taken by a laser machine"),
                    )
                }
            }
        }
        let mut dwell = false;
        for g in gs {
            match g {
                20 => rd.inch = true,
                21 => rd.inch = false,
                90 => rd.absolute = true,
                91 => rd.absolute = false,
                17 | 94 | 54..=59 => {}
                0..=3 => rd.motion = Some(g),
                4 => dwell = true,
                _ => {
                    return fail(
                        "bad_gcode",
                        format!("line {n}: G{g} is not taken by a laser machine"),
                    )
                }
            }
        }
        let mut finish = false;
        for m in ms {
            match m {
                3 => rd.beam = Beam::Constant,
                4 => rd.beam = Beam::Dynamic,
                5 => rd.beam = Beam::Off,
                8 | 9 => {}
                2 | 30 => finish = true,
                _ => {
                    return fail(
                        "bad_gcode",
                        format!("line {n}: M{m} is not taken by a laser machine"),
                    )
                }
            }
        }
        let unit = rd.unit();
        if let Some(f) = af {
            let mm = f * unit;
            if mm <= 0.0 {
                return fail(
                    "bad_gcode",
                    format!("line {n}: the feed has to be above zero"),
                );
            }
            if mm > lim.max_feed + 1e-9 {
                return fail(
                    "laser_feed_limit",
                    format!(
                        "line {n}: the feed {} mm/min is over the limit of {} mm/min",
                        num(mm),
                        num(lim.max_feed)
                    ),
                );
            }
            rd.feed = mm;
        }
        let mut power_word = None;
        if let Some(sv) = asv {
            if sv < 0.0 {
                return fail("bad_gcode", format!("line {n}: S cannot be negative"));
            }
            let mut v = sv * scale;
            if v > lim.limit_s + 1e-9 {
                if lim.clamp {
                    v = lim.limit_s;
                    clamped += 1;
                } else {
                    return fail(
                        "laser_power_limit",
                        format!(
                            "line {n}: S{} is over the limit of S{} ({}% of full power S{})",
                            num(v),
                            num(lim.limit_s),
                            num(lim.limit_s / lim.max_s * 100.0),
                            num(lim.max_s)
                        ),
                    );
                }
            }
            rd.s = v;
            max_seen = max_seen.max(v);
            power_word = Some(v);
        }
        if dwell {
            let Some(p) = ap else {
                return fail("bad_gcode", format!("line {n}: G4 needs P, the seconds"));
            };
            if !(0.0..=MAX_DWELL_S).contains(&p) {
                return fail(
                    "bad_gcode",
                    format!("line {n}: a dwell is from 0 to {MAX_DWELL_S} seconds"),
                );
            }
            rd.time += p;
        } else if ax.is_some() || ay.is_some() {
            let Some(motion) = rd.motion else {
                return fail(
                    "bad_gcode",
                    format!("line {n}: a move before any of G0, G1, G2 or G3"),
                );
            };
            let tx = ax.map_or(rd.x, |v| {
                if rd.absolute {
                    v * unit
                } else {
                    rd.x + v * unit
                }
            });
            let ty = ay.map_or(rd.y, |v| {
                if rd.absolute {
                    v * unit
                } else {
                    rd.y + v * unit
                }
            });
            if motion == 0 {
                if rd.beam == Beam::Constant && rd.s > 0.0 {
                    return fail(
                        "bad_gcode",
                        format!("line {n}: a rapid move with the beam on in constant mode (M3); use M4 or switch the beam off first"),
                    );
                }
            } else if rd.feed <= 0.0 {
                return fail(
                    "bad_gcode",
                    format!("line {n}: a cutting move before any feed (F)"),
                );
            }
            let arc = if motion >= 2 {
                if ai.is_none() && aj.is_none() {
                    return fail(
                        "bad_gcode",
                        format!("line {n}: an arc needs the center as I and J"),
                    );
                }
                Some((
                    rd.x + ai.unwrap_or(0.0) * unit,
                    rd.y + aj.unwrap_or(0.0) * unit,
                ))
            } else {
                None
            };
            rd.travel(n, motion, (tx, ty), arc)?;
        }
        // the line as it is sent: from the words that were understood, with the power as it was held
        let mut out: Vec<String> = Vec::with_capacity(words.len());
        for (letter, v) in &words {
            if *letter == 'N' {
                continue;
            }
            let value = match (*letter, power_word) {
                ('S', Some(p)) => p,
                _ => *v,
            };
            out.push(format!("{letter}{}", num(value)));
        }
        lines.push(out.join(" "));
        steps.push(Step {
            x: rd.x,
            y: rd.y,
            s: rd.s,
            beam: rd.firing(),
            t: rd.time,
        });
        if finish {
            break;
        }
    }
    if lines.is_empty() {
        return fail("no_work", "the program has no line of code to send".into());
    }
    let beam_off_added = rd.beam != Beam::Off;
    if beam_off_added {
        lines.push("M5".into());
        steps.push(Step {
            x: rd.x,
            y: rd.y,
            s: rd.s,
            beam: false,
            t: rd.time,
        });
    }
    Ok(Analysis {
        lines,
        steps,
        total_s: rd.time,
        length_mm: rd.length,
        cut_mm: rd.cut,
        max_s_seen: max_seen,
        clamped,
        beam_off_added,
        cut_box: rd.cut_box,
    })
}

// ---- the machines and their jobs -----------------------------------------------------------------------------------------------------

/// What a laser machine is at the moment.
#[derive(Debug, Clone, Default)]
struct MachineNow {
    latched: bool,
    x: f64,
    y: f64,
    s: f64,
    beam: bool,
}

struct LaserJob {
    machine: String,
    kind: &'static str,
    steps: Vec<Step>,
    cumulative: Vec<f64>,
    state: WorkState,
    clock: SimClock,
    rate: f64,
    latch_on_stop: bool,
    message: String,
}

impl LaserJob {
    fn work_s(&self) -> f64 {
        self.clock.work_s(self.rate)
    }

    fn done(&self) -> usize {
        match self.state {
            WorkState::Queued => 0,
            WorkState::Done => self.steps.len(),
            _ => finished_items(&self.cumulative, self.work_s()),
        }
    }
}

/// The laser machines of the server: their jobs and what each one is now.
#[derive(Default)]
pub struct LaserEngine {
    jobs: Mutex<HashMap<String, LaserJob>>,
    now: Mutex<HashMap<String, MachineNow>>,
}

/// The refusal of a job on a machine that an emergency stop latched.
fn estop_latched() -> PluginError {
    PluginError::from(LaserError {
        code: "laser_estop_latched",
        detail: "an emergency stop latched the machine: run a job of the kind rearm first".into(),
    })
}

fn lost() -> PluginError {
    PluginError::Failed("the laser machine lost the job (the server restarted)".into())
}

fn missing() -> PluginError {
    PluginError::Bad("the job is not there".into())
}

fn text_of(request: &Value) -> PluginResult<String> {
    request
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| PluginError::Bad("a gcode request has the program in `text`".into()))
}

impl LaserEngine {
    /// Whether a job is running or held on the machine.
    pub fn busy(&self, machine: &str) -> bool {
        self.jobs
            .locked()
            .values()
            .any(|j| j.machine == machine && j.state.active())
    }

    /// Whether the machine is latched by an emergency stop.
    pub fn latched(&self, machine: &str) -> bool {
        self.now.locked().get(machine).is_some_and(|m| m.latched)
    }

    /// What a client sees of the machine besides its state: the latch, where the head and the beam are, and the limits in force.
    pub fn detail(&self, machine: &str, cfg: &LaserConfig) -> Value {
        let now = self.now.locked().get(machine).cloned().unwrap_or_default();
        json!({
            "estop_latched": now.latched,
            "position_mm": { "x": now.x, "y": now.y },
            "beam_on": now.beam,
            "power": {
                "s": now.s, "max_s": cfg.max_s, "limit_s": cfg.limit_s(),
                "limit_pct": cfg.power_limit_pct,
                "over_limit": if cfg.over_limit == OverLimit::Clamp { "clamp" } else { "reject" },
            },
            "bed_mm": { "x": cfg.bed_x, "y": cfg.bed_y },
            "max_feed_mm_min": cfg.max_feed,
            "latch_on_stop": cfg.latch_on_stop,
        })
    }

    pub fn prepare(
        &self,
        machine: &str,
        cfg: &LaserConfig,
        request: &Value,
    ) -> PluginResult<Prepared> {
        let kind = request.get("kind").and_then(Value::as_str).unwrap_or("");
        let limits = Limits::of(cfg);
        let latched = self.latched(machine);
        let start = {
            let now = self.now.locked();
            now.get(machine).map_or((0.0, 0.0), |m| (m.x, m.y))
        };
        if latched && kind != "rearm" {
            return Err(estop_latched());
        }
        let (kind_name, analysis): (&'static str, Analysis) =
            match kind {
                "gcode" => {
                    let scale = match request.get("power_scale_pct") {
                        None => 100.0,
                        Some(v) => v
                            .as_f64()
                            .filter(|p| (1.0..=100.0).contains(p))
                            .ok_or_else(|| {
                                PluginError::Bad("power_scale_pct is a number from 1 to 100".into())
                            })?,
                    };
                    ("gcode", analyze(&text_of(request)?, &limits, start, scale)?)
                }
                "test_fire" => {
                    let pct = request
                        .get("power_pct")
                        .and_then(Value::as_f64)
                        .filter(|p| *p > 0.0 && *p <= 100.0)
                        .ok_or_else(|| {
                            PluginError::Bad("power_pct is a number above 0 and up to 100".into())
                        })?;
                    let seconds = request
                        .get("seconds")
                        .and_then(Value::as_f64)
                        .filter(|s| (0.01..=cfg.test_fire_max_s).contains(s))
                        .ok_or_else(|| {
                            PluginError::Bad(format!(
                                "seconds is a number from 0.01 to {} (test_fire_max_s)",
                                num(cfg.test_fire_max_s)
                            ))
                        })?;
                    let program = format!(
                        "G21\nG90\nM3 S{}\nG4 P{}\nM5\n",
                        num(cfg.max_s * pct / 100.0),
                        num(seconds)
                    );
                    ("test_fire", analyze(&program, &limits, start, 100.0)?)
                }
                "rearm" => (
                    "rearm",
                    Analysis {
                        lines: vec!["(rearm)".into()],
                        steps: vec![Step {
                            x: start.0,
                            y: start.1,
                            s: 0.0,
                            beam: false,
                            t: 0.0,
                        }],
                        total_s: 0.0,
                        length_mm: 0.0,
                        cut_mm: 0.0,
                        max_s_seen: 0.0,
                        clamped: 0,
                        beam_off_added: false,
                        cut_box: None,
                    },
                ),
                _ => return Err(PluginError::Bad(
                    "the request has to be {\"kind\": \"gcode\" | \"test_fire\" | \"rearm\", ...}"
                        .into(),
                )),
            };
        let native_id = crate::ids::new(crate::ids::Kind::Work);
        let view = view_of(kind_name, &analysis, &limits);
        let cumulative: Vec<f64> = analysis.steps.iter().map(|s| s.t).collect();
        self.jobs.locked().insert(
            native_id.clone(),
            LaserJob {
                machine: machine.to_string(),
                kind: kind_name,
                steps: analysis.steps,
                cumulative,
                state: WorkState::Queued,
                clock: SimClock::new(),
                rate: cfg.sim_rate,
                latch_on_stop: cfg.latch_on_stop,
                message: String::new(),
            },
        );
        Ok(Prepared { native_id, view })
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
        let mut now = self.now.locked();
        let rt = now.entry(machine.to_string()).or_default();
        if rt.latched && job.kind != "rearm" {
            return Err(estop_latched());
        }
        if job.kind == "rearm" {
            rt.latched = false;
        }
        job.state = WorkState::Running;
        job.clock.run();
        Ok(())
    }

    pub fn pause(&self, native_id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(native_id).ok_or_else(missing)?;
        if job.state != WorkState::Running {
            return Err(PluginError::NotInState);
        }
        job.clock.hold();
        job.state = WorkState::Paused;
        let mut now = self.now.locked();
        if let Some(rt) = now.get_mut(&job.machine) {
            rt.beam = false;
        }
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

    /// The emergency stop: the job ends at once, the beam goes off and the machine is latched when its settings say so.
    pub fn stop(&self, native_id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(native_id).ok_or_else(missing)?;
        if !job.state.active() {
            return Err(PluginError::NotInState);
        }
        let done = job.done();
        job.clock.hold();
        job.state = WorkState::Cancelled;
        let latch = job.latch_on_stop && job.kind != "rearm";
        job.message = if latch {
            "emergency stop: the machine is latched until a rearm job runs".into()
        } else {
            "stopped".into()
        };
        let mut now = self.now.locked();
        let rt = now.entry(job.machine.clone()).or_default();
        if done > 0 {
            if let Some(step) = job.steps.get(done - 1) {
                rt.x = step.x;
                rt.y = step.y;
            }
        }
        rt.s = 0.0;
        rt.beam = false;
        if latch {
            rt.latched = true;
        }
        Ok(())
    }

    pub fn status(&self, native_id: &str) -> PluginResult<PluginStatus> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(native_id).ok_or_else(lost)?;
        if job.state == WorkState::Running && job.done() >= job.steps.len() {
            job.clock.hold();
            job.state = WorkState::Done;
        }
        let done = job.done();
        if job.state.active() || job.state == WorkState::Done {
            let mut now = self.now.locked();
            let rt = now.entry(job.machine.clone()).or_default();
            if let Some(step) = done.checked_sub(1).and_then(|i| job.steps.get(i)) {
                rt.x = step.x;
                rt.y = step.y;
                rt.s = step.s;
                rt.beam = step.beam && job.state == WorkState::Running;
            }
            if job.state == WorkState::Done {
                rt.s = 0.0;
                rt.beam = false;
            }
        }
        Ok(PluginStatus {
            state: job.state,
            done: done as u64,
            total: job.steps.len() as u64,
            message: job.message.clone(),
        })
    }

    pub fn discard(&self, native_id: &str) {
        self.jobs.locked().remove(native_id);
    }
}

/// What a client draws and reads of a job that was prepared.
fn view_of(kind: &str, a: &Analysis, lim: &Limits) -> Value {
    let stride = a.steps.len().div_ceil(PREVIEW_POINTS).max(1);
    let path: Vec<Value> = a
        .steps
        .iter()
        .step_by(stride)
        .map(|s| json!([round2(s.x), round2(s.y), u8::from(s.beam)]))
        .collect();
    let cut_box = a
        .cut_box
        .map(|(x0, y0, x1, y1)| json!({"min_x": x0, "min_y": y0, "max_x": x1, "max_y": y1}));
    json!({
        "kind": kind,
        "lines": a.lines.len(),
        "length_mm": round2(a.length_mm),
        "cut_mm": round2(a.cut_mm),
        "estimate_s": round2(a.total_s),
        "max_s": a.max_s_seen,
        "limit_s": lim.limit_s,
        "clamped": a.clamped,
        "beam_off_added": a.beam_off_added,
        "cut_box": cut_box,
        "path": path,
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lim() -> Limits {
        Limits::of(&LaserConfig::default())
    }

    fn ok(text: &str) -> Analysis {
        analyze(text, &lim(), (0.0, 0.0), 100.0).unwrap()
    }

    fn err(text: &str) -> LaserError {
        analyze(text, &lim(), (0.0, 0.0), 100.0).unwrap_err()
    }

    #[test]
    fn a_square_is_read_with_its_length_its_time_and_its_box() {
        let a = ok("G21 G90\nM4 S300\nG0 X10 Y10\nG1 X30 Y10 F600\nG1 X30 Y30\nG1 X10 Y30\nG1 X10 Y10\nM5\n");
        assert_eq!(a.lines.len(), 8);
        assert!((a.cut_mm - 80.0).abs() < 1e-6, "{}", a.cut_mm);
        assert!((a.length_mm - (200.0f64.sqrt() + 80.0)).abs() < 1e-6);
        // 80 mm at 10 mm/s and the rapid move at 200 mm/s
        assert!((a.total_s - (8.0 + 200.0f64.sqrt() / 200.0)).abs() < 1e-6);
        assert_eq!(a.cut_box, Some((10.0, 10.0, 30.0, 30.0)));
        assert!(!a.beam_off_added);
        assert_eq!(a.max_s_seen, 300.0);
        assert_eq!(a.steps.len(), a.lines.len());
        assert!(a.steps.windows(2).all(|w| w[0].t <= w[1].t));
    }

    #[test]
    fn the_lines_are_written_again_from_the_words_and_the_comments_go() {
        let a = ok("; header\n(note) g1 x1.50 y2 f100 ; tail\n%\nN10 G1 X3\n");
        assert_eq!(a.lines, vec!["G1 X1.5 Y2 F100", "G1 X3"]);
    }

    #[test]
    fn power_over_the_limit_is_refused_by_default_with_its_own_code() {
        // the default limit is 40 percent of S1000
        assert!(analyze("M4 S400\nG1 X1 F100", &lim(), (0.0, 0.0), 100.0).is_ok());
        let e = err("M4 S401\nG1 X1 F100");
        assert_eq!(e.code, "laser_power_limit");
        assert!(e.detail.contains("line 1"), "{}", e.detail);
        // S on its own line, before the beam is on, counts too
        assert_eq!(err("G1 F100\nS900\nM3").code, "laser_power_limit");
    }

    #[test]
    fn a_machine_set_to_clamp_cuts_the_power_to_the_limit_and_counts_it() {
        let mut l = lim();
        l.clamp = true;
        let a = analyze("M4 S900\nG1 X5 F100\nS100\nG1 X6", &l, (0.0, 0.0), 100.0).unwrap();
        assert_eq!(a.lines[0], "M4 S400");
        assert_eq!(a.clamped, 1);
        assert_eq!(a.max_s_seen, 400.0);
    }

    #[test]
    fn the_scale_of_power_lowers_every_s_and_never_raises_it() {
        let a = analyze("M4 S200\nG1 X5 F100", &lim(), (0.0, 0.0), 50.0).unwrap();
        assert_eq!(a.lines[0], "M4 S100");
        // the scale is applied before the limit: S900 at 50 percent is S450, still over
        let e = analyze("M4 S900\nG1 X5 F100", &lim(), (0.0, 0.0), 50.0).unwrap_err();
        assert_eq!(e.code, "laser_power_limit");
        assert!(analyze("M4 S900\nG1 X5 F100", &lim(), (0.0, 0.0), 40.0).is_ok());
    }

    #[test]
    fn the_feed_has_a_limit_and_inches_are_converted_before_it_is_checked() {
        assert_eq!(err("G1 X1 F6001").code, "laser_feed_limit");
        assert!(analyze("G1 X1 F6000", &lim(), (0.0, 0.0), 100.0).is_ok());
        // 300 in/min is 7620 mm/min
        assert_eq!(err("G20\nG1 X0.1 F300").code, "laser_feed_limit");
        assert_eq!(err("G1 X1 F0").code, "bad_gcode");
        assert_eq!(err("G1 X1").code, "bad_gcode");
    }

    #[test]
    fn a_move_outside_the_bed_is_refused_also_in_relative_mode_and_for_an_arc() {
        assert_eq!(err("G0 X401 Y0").code, "outside_course");
        assert_eq!(err("G0 Y-0.1").code, "outside_course");
        assert!(analyze("G0 X400 Y400", &lim(), (0.0, 0.0), 100.0).is_ok());
        assert_eq!(err("G91\nG0 X300\nG0 X101").code, "outside_course");
        // a half circle of radius 20 from (10,100) to (50,100) bulges up to y = 120 and, going the other way, down to y = 80: both fit; one that starts near the edge does not
        assert!(analyze(
            "G0 X10 Y100\nG2 X50 Y100 I20 J0 F100",
            &lim(),
            (0.0, 0.0),
            100.0
        )
        .is_ok());
        assert_eq!(
            err("G0 X10 Y5\nG3 X50 Y5 I20 J0 F100").code,
            "outside_course"
        );
    }

    #[test]
    fn a_rapid_move_with_the_beam_on_in_constant_mode_is_refused() {
        assert_eq!(err("M3 S100\nG0 X10").code, "bad_gcode");
        // dynamic mode switches the beam off in a rapid move, and S0 does not fire
        assert!(analyze("M4 S100\nG0 X10", &lim(), (0.0, 0.0), 100.0).is_ok());
        assert!(analyze("M3 S0\nG0 X10", &lim(), (0.0, 0.0), 100.0).is_ok());
    }

    #[test]
    fn a_program_that_ends_with_the_beam_on_gets_an_m5_and_m2_ends_the_reading() {
        let a = ok("M4 S100\nG1 X10 F600");
        assert!(a.beam_off_added);
        assert_eq!(a.lines.last().unwrap(), "M5");
        assert!(!a.steps.last().unwrap().beam);
        let b = ok("M4 S100\nG1 X10 F600\nM2\nG1 X20");
        assert_eq!(b.lines, vec!["M4 S100", "G1 X10 F600", "M2", "M5"]);
    }

    #[test]
    fn what_is_not_g_code_or_not_for_a_laser_is_refused() {
        for bad in [
            "$H",
            "!",
            "G28",
            "G92 X0",
            "M6",
            "M0",
            "T1",
            "G1 X#1",
            "G1.1 X1",
            "G 1",
            "G1 X",
            "G1 R5 X1 F100",
            "G2 X10 Y10 F100",
        ] {
            assert_eq!(err(bad).code, "bad_gcode", "{bad}");
        }
        assert_eq!(err("(only a comment)\n").code, "no_work");
        assert_eq!(err("").code, "no_work");
        assert_eq!(err("G4").code, "bad_gcode");
        assert_eq!(err("G4 P601").code, "bad_gcode");
        assert_eq!(err("M3 S-1").code, "bad_gcode");
        assert_eq!(err("G1 X1\u{7} F10").code, "bad_gcode");
        assert_eq!(
            err(&format!("G1 X1 F10 {}", "X1 ".repeat(100))).code,
            "bad_gcode"
        );
    }

    #[test]
    fn a_dwell_adds_its_seconds_and_a_full_circle_has_the_length_of_the_circle() {
        let a = ok("G4 P2.5");
        assert!((a.total_s - 2.5).abs() < 1e-9);
        let c = ok("G0 X100 Y100\nG2 X100 Y100 I10 J0 F600");
        let want = 2.0 * std::f64::consts::PI * 10.0;
        // no S: the beam is off, so the circle is not a cut, but it is a move with its time
        assert_eq!(c.cut_mm, 0.0);
        assert!((c.length_mm - 100.0 * 2.0f64.sqrt() - want).abs() < 1e-6);
    }

    #[test]
    fn the_start_of_the_head_is_where_a_relative_program_begins() {
        let e = analyze("G91\nG0 X10", &lim(), (395.0, 0.0), 100.0).unwrap_err();
        assert_eq!(e.code, "outside_course");
        assert!(analyze("G91\nG0 X-10", &lim(), (395.0, 0.0), 100.0).is_ok());
    }

    #[test]
    fn the_settings_are_checked_and_a_typo_is_refused() {
        let cfg = LaserConfig::from_settings(&json!({})).unwrap();
        assert_eq!(cfg, LaserConfig::default());
        assert!((cfg.limit_s() - 400.0).abs() < 1e-9);
        let cfg = LaserConfig::from_settings(&json!({
            "bed_x_mm": 300, "power_limit_pct": 100, "over_limit": "clamp",
            "latch_on_stop": false, "sim_rate": 0, "max_s": 255,
        }))
        .unwrap();
        assert_eq!(cfg.bed_x, 300.0);
        assert_eq!(cfg.over_limit, OverLimit::Clamp);
        assert!(!cfg.latch_on_stop);
        assert_eq!(cfg.limit_s(), 255.0);
        for bad in [
            json!({"bed_z_mm": 1}),
            json!({"bed_x_mm": 5}),
            json!({"bed_x_mm": "400"}),
            json!({"power_limit_pct": 0}),
            json!({"power_limit_pct": 101}),
            json!({"over_limit": "ignore"}),
            json!({"latch_on_stop": 1}),
            json!({"sim_rate": -1}),
            json!({"test_fire_max_s": 11}),
        ] {
            assert!(LaserConfig::from_settings(&bad).is_err(), "{bad}");
        }
    }

    fn engine_cfg() -> LaserConfig {
        LaserConfig {
            sim_rate: 0.0,
            ..LaserConfig::default()
        }
    }

    fn job(e: &LaserEngine, cfg: &LaserConfig, request: Value) -> String {
        e.prepare("l1", cfg, &request).unwrap().native_id
    }

    #[test]
    fn a_job_runs_to_the_end_in_the_simulator_and_the_machine_is_left_where_it_ended() {
        let e = LaserEngine::default();
        let cfg = engine_cfg();
        let id = job(
            &e,
            &cfg,
            json!({"kind": "gcode", "text": "M4 S100\nG1 X10 Y20 F600\nM5"}),
        );
        assert_eq!(e.status(&id).unwrap().state, WorkState::Queued);
        e.start("l1", &id).unwrap();
        assert!(e.busy("l1"));
        let st = e.status(&id).unwrap();
        assert_eq!((st.state, st.done, st.total), (WorkState::Done, 3, 3));
        assert!(!e.busy("l1"));
        let d = e.detail("l1", &cfg);
        assert_eq!(d["position_mm"]["x"], 10.0);
        assert_eq!(d["position_mm"]["y"], 20.0);
        assert_eq!(d["beam_on"], false);
        assert_eq!(d["estop_latched"], false);
    }

    #[test]
    fn one_job_at_a_time_and_another_machine_is_free() {
        let e = LaserEngine::default();
        let slow = LaserConfig {
            sim_rate: 1.0,
            ..LaserConfig::default()
        };
        let a = job(&e, &slow, json!({"kind": "gcode", "text": "G1 X100 F60"}));
        let b = job(&e, &slow, json!({"kind": "gcode", "text": "G1 X10 F60"}));
        e.start("l1", &a).unwrap();
        assert_eq!(e.start("l1", &b), Err(PluginError::Busy));
        let other = e
            .prepare("l2", &slow, &json!({"kind": "gcode", "text": "G1 X10 F60"}))
            .unwrap()
            .native_id;
        assert!(e.start("l2", &other).is_ok());
    }

    #[test]
    fn a_held_job_stops_counting_and_goes_on() {
        let e = LaserEngine::default();
        let slow = LaserConfig {
            sim_rate: 1.0,
            ..LaserConfig::default()
        };
        let id = job(
            &e,
            &slow,
            json!({"kind": "gcode", "text": "G1 X100 F60\nG1 X0"}),
        );
        assert_eq!(e.pause(&id), Err(PluginError::NotInState));
        e.start("l1", &id).unwrap();
        e.pause(&id).unwrap();
        assert_eq!(e.status(&id).unwrap().state, WorkState::Paused);
        assert_eq!(e.pause(&id), Err(PluginError::NotInState));
        e.resume(&id).unwrap();
        assert_eq!(e.status(&id).unwrap().state, WorkState::Running);
        assert_eq!(e.resume(&id), Err(PluginError::NotInState));
    }

    #[test]
    fn the_emergency_stop_latches_the_machine_until_a_rearm_job_runs() {
        let e = LaserEngine::default();
        let slow = LaserConfig {
            sim_rate: 1.0,
            ..LaserConfig::default()
        };
        let id = job(
            &e,
            &slow,
            json!({"kind": "gcode", "text": "M4 S100\nG1 X100 F60"}),
        );
        e.start("l1", &id).unwrap();
        e.stop(&id).unwrap();
        let st = e.status(&id).unwrap();
        assert_eq!(st.state, WorkState::Cancelled);
        assert!(st.message.contains("latched"), "{}", st.message);
        assert!(e.latched("l1"));
        assert_eq!(e.detail("l1", &slow)["estop_latched"], true);
        assert_eq!(e.detail("l1", &slow)["beam_on"], false);
        // nothing else is prepared on it, and what was prepared before cannot start
        let late = e.prepare("l1", &slow, &json!({"kind": "gcode", "text": "G1 X1 F60"}));
        assert!(matches!(
            late,
            Err(PluginError::Coded("laser_estop_latched", _))
        ));
        // another machine is not latched
        assert!(e
            .prepare("l2", &slow, &json!({"kind": "gcode", "text": "G1 X1 F60"}))
            .is_ok());
        let re = job(&e, &slow, json!({"kind": "rearm"}));
        e.start("l1", &re).unwrap();
        assert!(!e.latched("l1"));
        assert_eq!(e.status(&re).unwrap().state, WorkState::Done);
        assert!(e
            .prepare("l1", &slow, &json!({"kind": "gcode", "text": "G1 X1 F60"}))
            .is_ok());
    }

    #[test]
    fn a_job_prepared_before_the_stop_cannot_start_on_the_latched_machine() {
        let e = LaserEngine::default();
        let slow = LaserConfig {
            sim_rate: 1.0,
            ..LaserConfig::default()
        };
        let running = job(&e, &slow, json!({"kind": "gcode", "text": "G1 X100 F60"}));
        let waiting = job(&e, &slow, json!({"kind": "gcode", "text": "G1 X10 F60"}));
        e.start("l1", &running).unwrap();
        e.stop(&running).unwrap();
        let r = e.start("l1", &waiting);
        assert!(matches!(
            r,
            Err(PluginError::Coded("laser_estop_latched", _))
        ));
        assert_eq!(e.status(&waiting).unwrap().state, WorkState::Queued);
    }

    #[test]
    fn a_machine_set_not_to_latch_goes_on_after_a_stop() {
        let e = LaserEngine::default();
        let cfg = LaserConfig {
            sim_rate: 1.0,
            latch_on_stop: false,
            ..LaserConfig::default()
        };
        let id = job(&e, &cfg, json!({"kind": "gcode", "text": "G1 X100 F60"}));
        e.start("l1", &id).unwrap();
        e.stop(&id).unwrap();
        assert!(!e.latched("l1"));
        assert_eq!(e.stop(&id), Err(PluginError::NotInState));
    }

    #[test]
    fn a_test_fire_is_held_to_the_limits_and_makes_a_short_job() {
        let e = LaserEngine::default();
        let cfg = engine_cfg();
        let r = |req: Value| e.prepare("l1", &cfg, &req);
        assert!(r(json!({"kind": "test_fire", "power_pct": 10, "seconds": 0.5})).is_ok());
        assert!(matches!(
            r(json!({"kind": "test_fire", "power_pct": 41, "seconds": 0.5})),
            Err(PluginError::Coded("laser_power_limit", _))
        ));
        for bad in [
            json!({"kind": "test_fire", "power_pct": 10, "seconds": 1.5}),
            json!({"kind": "test_fire", "power_pct": 0, "seconds": 0.5}),
            json!({"kind": "test_fire", "seconds": 0.5}),
            json!({"kind": "test_fire", "power_pct": 10}),
            json!({"kind": "burn"}),
            json!({"text": "G1 X1"}),
            json!({"kind": "gcode"}),
            json!({"kind": "gcode", "text": "G1 X1 F10", "power_scale_pct": 0}),
        ] {
            assert!(matches!(r(bad.clone()), Err(PluginError::Bad(_))), "{bad}");
        }
    }

    #[test]
    fn the_view_has_the_numbers_and_a_drawing_of_at_most_two_thousand_points() {
        let e = LaserEngine::default();
        let cfg = engine_cfg();
        let mut text = String::from("M4 S100\n");
        for i in 0..5000 {
            text.push_str(&format!("G1 X{} Y{} F600\n", i % 300, (i / 300) % 300));
        }
        let p = e
            .prepare("l1", &cfg, &json!({"kind": "gcode", "text": text}))
            .unwrap();
        assert_eq!(p.view["lines"], 5002);
        assert_eq!(p.view["beam_off_added"], true);
        assert!(p.view["path"].as_array().unwrap().len() <= 2000);
        assert!(p.view["estimate_s"].as_f64().unwrap() > 0.0);
        assert_eq!(p.view["limit_s"], 400.0);
    }

    #[test]
    fn a_discarded_job_is_lost_and_an_unknown_one_is_refused() {
        let e = LaserEngine::default();
        let cfg = engine_cfg();
        let id = job(&e, &cfg, json!({"kind": "gcode", "text": "G1 X1 F60"}));
        e.discard(&id);
        assert!(matches!(e.status(&id), Err(PluginError::Failed(_))));
        assert!(matches!(e.start("l1", &id), Err(PluginError::Bad(_))));
        assert!(matches!(e.stop("nope"), Err(PluginError::Bad(_))));
    }
}
