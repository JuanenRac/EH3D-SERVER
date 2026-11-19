//! The pick-and-place machines with no hardware behind: the table of feeders of each machine, the queue of placement jobs, the state of the head and the simulated run. The
//! plugin `pnp` (`src/plugin_pnp.rs`) only hands the machines of the server to this engine.
//!
//! A machine has feeders in numbered slots; each holds a part (its name) and how many are left. The jobs are of two kinds: `load_feeders` says what is in the slots (what a
//! person did by hand at the machine) and `place` lists the placements of a board. A `place` job is checked when it is prepared: the parts have to be in some feeder and,
//! unless the machine is set otherwise, there have to be enough of them (`pnp_feeder_short`), every place has to be on the bed, a reference may not repeat. Each pick takes one part off its
//! feeder as the simulated machine reaches it, so a feeder that runs dry stops the job as failed. A machine can be told to lose the vacuum at every Nth placement of a job
//! to try how the programs deal with a failure.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::plugins::{PluginError, PluginResult, PluginStatus, Prepared, WorkState};
use crate::simclock::{finished_items, SimClock};
use crate::sync::Locked;

/// The most feeders a machine may have.
pub const MAX_SLOTS: u32 = 200;
/// The most parts a feeder may hold.
pub const MAX_COUNT: u64 = 100_000;
/// The most placements the view of a job lists.
const VIEW_ROWS: usize = 2000;

/// The settings a pick-and-place machine may have: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("feeders", "how many slots of feeders, 1 to 200 (40 by default)"),
    ("bed_x_mm", "width of the bed, 10 to 5000 (300 by default)"),
    ("bed_y_mm", "depth of the bed, 10 to 5000 (300 by default)"),
    ("feeder_x0_mm", "where the first slot is picked from, in x (10 by default)"),
    ("feeder_pitch_mm", "the distance between two slots, 1 to 100 (8 by default)"),
    ("feeder_y_mm", "where the slots are picked from, in y (-20 by default)"),
    ("travel_mm_s", "the speed of the head, 1 to 5000 (300 by default)"),
    ("pick_ms", "the time of a pick, 0 to 10000 (250 by default)"),
    ("place_ms", "the time of a placement, 0 to 10000 (300 by default)"),
    ("max_placements", "the most placements of a job, 1 to 200000 (20000 by default)"),
    ("require_stock", "a job is refused when a feeder does not hold as many parts as the job needs (true by default); false lets it start and fail when the feeder runs dry"),
    ("sim_fail_pick_every", "the simulated machine loses the vacuum at every Nth placement of a job, 0 to 1000000 (0 by default: never)"),
    ("sim_rate", "how many times faster than real time the simulated machine works, 0 to 100000 (1 by default; 0 is no waiting)"),
];

/// The settings of a pick-and-place machine, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct PnpConfig {
    pub slots: u32,
    pub bed_x: f64,
    pub bed_y: f64,
    pub feeder_x0: f64,
    pub feeder_pitch: f64,
    pub feeder_y: f64,
    pub travel_mm_s: f64,
    pub pick_ms: f64,
    pub place_ms: f64,
    pub max_placements: usize,
    pub require_stock: bool,
    pub fail_every: usize,
    pub sim_rate: f64,
}

impl Default for PnpConfig {
    fn default() -> Self {
        PnpConfig {
            slots: 40,
            bed_x: 300.0,
            bed_y: 300.0,
            feeder_x0: 10.0,
            feeder_pitch: 8.0,
            feeder_y: -20.0,
            travel_mm_s: 300.0,
            pick_ms: 250.0,
            place_ms: 300.0,
            max_placements: 20_000,
            require_stock: true,
            fail_every: 0,
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

fn whole_in(settings: &Value, key: &str, low: u64, high: u64) -> Result<Option<u64>, String> {
    match settings.get(key) {
        None => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if (low..=high).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{key} is a whole number from {low} to {high}")),
        },
    }
}

impl PnpConfig {
    /// Reads the settings of a machine: a key that is not one of [`SETTINGS_HELP`] or a value out of its range is refused.
    pub fn from_settings(settings: &Value) -> Result<PnpConfig, String> {
        let mut cfg = PnpConfig::default();
        if let Some(map) = settings.as_object() {
            for key in map.keys() {
                if !SETTINGS_HELP.iter().any(|(k, _)| k == key) {
                    return Err(format!(
                        "{key} is not a setting of a pick-and-place machine"
                    ));
                }
            }
        }
        if let Some(n) = whole_in(settings, "feeders", 1, u64::from(MAX_SLOTS))? {
            cfg.slots = n as u32;
        }
        if let Some(n) = number_in(settings, "bed_x_mm", 10.0, 5000.0)? {
            cfg.bed_x = n;
        }
        if let Some(n) = number_in(settings, "bed_y_mm", 10.0, 5000.0)? {
            cfg.bed_y = n;
        }
        if let Some(n) = number_in(settings, "feeder_x0_mm", -5000.0, 5000.0)? {
            cfg.feeder_x0 = n;
        }
        if let Some(n) = number_in(settings, "feeder_pitch_mm", 1.0, 100.0)? {
            cfg.feeder_pitch = n;
        }
        if let Some(n) = number_in(settings, "feeder_y_mm", -5000.0, 5000.0)? {
            cfg.feeder_y = n;
        }
        if let Some(n) = number_in(settings, "travel_mm_s", 1.0, 5000.0)? {
            cfg.travel_mm_s = n;
        }
        if let Some(n) = number_in(settings, "pick_ms", 0.0, 10_000.0)? {
            cfg.pick_ms = n;
        }
        if let Some(n) = number_in(settings, "place_ms", 0.0, 10_000.0)? {
            cfg.place_ms = n;
        }
        if let Some(n) = whole_in(settings, "max_placements", 1, 200_000)? {
            cfg.max_placements = n as usize;
        }
        if let Some(n) = whole_in(settings, "sim_fail_pick_every", 0, 1_000_000)? {
            cfg.fail_every = n as usize;
        }
        if let Some(n) = number_in(settings, "sim_rate", 0.0, 100_000.0)? {
            cfg.sim_rate = n;
        }
        match settings.get("require_stock") {
            None => {}
            Some(Value::Bool(b)) => cfg.require_stock = *b,
            Some(_) => return Err("require_stock is true or false".into()),
        }
        Ok(cfg)
    }

    /// Where the head picks from slot `slot`.
    pub fn pick_pos(&self, slot: u32) -> (f64, f64) {
        (
            self.feeder_x0 + f64::from(slot.saturating_sub(1)) * self.feeder_pitch,
            self.feeder_y,
        )
    }
}

/// What one feeder holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Feeder {
    pub part: String,
    pub count: u64,
}

/// One placement of a job, with its place already moved by the offset of the board.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub reference: String,
    pub part: String,
    pub x: f64,
    pub y: f64,
    pub rot: f64,
    pub side: &'static str,
}

fn bad<T>(why: impl Into<String>) -> PluginResult<T> {
    Err(PluginError::Bad(why.into()))
}

fn label_ok(text: &str, max: usize, extra: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= max
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// `(slot, part, count)` of each feeder a request names.
pub type FeederList = Vec<(u32, String, u64)>;

/// The feeders of a `load_feeders` request: `(slot, part, count)` each, and whether the table is replaced.
pub fn parse_load(request: &Value, cfg: &PnpConfig) -> PluginResult<(FeederList, bool)> {
    let replace = match request.get("replace") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return bad("replace is true or false"),
    };
    let list = match request.get("feeders") {
        Some(Value::Array(a)) => a,
        _ => return bad("a load_feeders request has the list `feeders`"),
    };
    if list.is_empty() && !replace {
        return bad("the list of feeders is empty: say replace: true to empty the table");
    }
    let mut out: Vec<(u32, String, u64)> = Vec::new();
    for (n, item) in list.iter().enumerate() {
        let slot = item
            .get("slot")
            .and_then(Value::as_u64)
            .filter(|s| (1..=u64::from(cfg.slots)).contains(s))
            .ok_or_else(|| {
                PluginError::Bad(format!(
                    "feeder {}: slot is a whole number from 1 to {}",
                    n + 1,
                    cfg.slots
                ))
            })? as u32;
        if out.iter().any(|(s, _, _)| *s == slot) {
            return bad(format!("slot {slot} is in the list twice"));
        }
        let part = item
            .get("part")
            .and_then(Value::as_str)
            .filter(|p| label_ok(p, 64, "_-./+:"))
            .ok_or_else(|| {
                PluginError::Bad(format!(
                    "feeder {}: part is a name of up to 64 letters, digits and _-./+:",
                    n + 1
                ))
            })?;
        let count = item
            .get("count")
            .and_then(Value::as_u64)
            .filter(|c| *c <= MAX_COUNT)
            .ok_or_else(|| {
                PluginError::Bad(format!(
                    "feeder {}: count is a whole number from 0 to {MAX_COUNT}",
                    n + 1
                ))
            })?;
        out.push((slot, part.to_string(), count));
    }
    Ok((out, replace))
}

/// The placements of a `place` request.
pub fn parse_place(request: &Value, cfg: &PnpConfig) -> PluginResult<Vec<Placement>> {
    let list = match request.get("placements") {
        Some(Value::Array(a)) => a,
        _ => return bad("a place request has the list `placements`"),
    };
    if list.is_empty() {
        return bad("the job has no placements");
    }
    if list.len() > cfg.max_placements {
        return Err(PluginError::Coded(
            "too_big",
            format!(
                "a job may have {} placements at the most on this machine",
                cfg.max_placements
            ),
        ));
    }
    let (ox, oy) = match request.get("offset") {
        None | Some(Value::Null) => (0.0, 0.0),
        Some(o) => {
            let n = |k: &str| o.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
            match (n("x"), n("y")) {
                (Some(x), Some(y)) => (x, y),
                _ => return bad("offset is {\"x\": number, \"y\": number}"),
            }
        }
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for (n, item) in list.iter().enumerate() {
        let row = n + 1;
        let reference = item
            .get("ref")
            .and_then(Value::as_str)
            .filter(|r| label_ok(r, 32, "_-."))
            .ok_or_else(|| {
                PluginError::Bad(format!(
                    "placement {row}: ref is a name of up to 32 letters, digits and _-."
                ))
            })?;
        if !seen.insert(reference.to_string()) {
            return bad(format!(
                "placement {row}: the reference {reference} is in the list twice"
            ));
        }
        let part = item
            .get("part")
            .and_then(Value::as_str)
            .filter(|p| label_ok(p, 64, "_-./+:"))
            .ok_or_else(|| {
                PluginError::Bad(format!(
                    "placement {row}: part is a name of up to 64 letters, digits and _-./+:"
                ))
            })?;
        let coordinate = |k: &str| {
            item.get(k)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
        };
        let (Some(x), Some(y)) = (coordinate("x"), coordinate("y")) else {
            return bad(format!("placement {row}: x and y are numbers"));
        };
        let rot = match item.get("rot") {
            None => 0.0,
            Some(v) => v
                .as_f64()
                .filter(|r| (-360.0..=360.0).contains(r))
                .ok_or_else(|| {
                    PluginError::Bad(format!("placement {row}: rot is from -360 to 360 degrees"))
                })?,
        };
        let side = match item.get("side").map(Value::as_str) {
            None | Some(Some("top")) => "top",
            Some(Some("bottom")) => "bottom",
            Some(_) => return bad(format!("placement {row}: side is top or bottom")),
        };
        let (px, py) = (x + ox, y + oy);
        if !(0.0..=cfg.bed_x).contains(&px) || !(0.0..=cfg.bed_y).contains(&py) {
            return Err(PluginError::Coded(
                "outside_course",
                format!(
                    "placement {row} ({reference}) is at X{px:.2} Y{py:.2}, outside the bed of {} by {} mm",
                    cfg.bed_x, cfg.bed_y
                ),
            ));
        }
        out.push(Placement {
            reference: reference.to_string(),
            part: part.to_string(),
            x: px,
            y: py,
            rot,
            side,
        });
    }
    Ok(out)
}

fn fail(code: &'static str, detail: String) -> PluginError {
    PluginError::Coded(code, detail)
}

fn short(why: String) -> PluginError {
    fail("pnp_feeder_short", why)
}

/// The slot each placement is picked from, in the order of the placements. A part that is in no feeder is always refused; one that is in feeders that hold too few is
/// refused when `require_stock` is set and otherwise goes to the first of its slots (the job fails when it runs dry).
pub fn assign(
    placements: &[Placement],
    feeders: &BTreeMap<u32, Feeder>,
    require_stock: bool,
) -> PluginResult<Vec<u32>> {
    let mut needed: BTreeMap<&str, u64> = BTreeMap::new();
    for p in placements {
        *needed.entry(p.part.as_str()).or_insert(0) += 1;
    }
    for (part, need) in &needed {
        let holders: Vec<&Feeder> = feeders.values().filter(|f| f.part == *part).collect();
        if holders.is_empty() {
            return Err(short(format!("no feeder holds the part {part}")));
        }
        let have: u64 = holders.iter().map(|f| f.count).sum();
        if require_stock && have < *need {
            return Err(short(format!(
                "the job needs {need} of {part} and the feeders hold {have}"
            )));
        }
    }
    let mut left: BTreeMap<u32, u64> = feeders.iter().map(|(s, f)| (*s, f.count)).collect();
    let mut plan = Vec::with_capacity(placements.len());
    for p in placements {
        let slot = feeders
            .iter()
            .find(|(s, f)| f.part == p.part && left.get(*s).copied().unwrap_or(0) > 0)
            .or_else(|| feeders.iter().find(|(_, f)| f.part == p.part))
            .map(|(s, _)| *s)
            .ok_or_else(|| short(format!("no feeder holds the part {}", p.part)))?;
        if let Some(n) = left.get_mut(&slot) {
            *n = n.saturating_sub(1);
        }
        plan.push(slot);
    }
    Ok(plan)
}

/// The seconds at which each pick and each placement is finished, and the millimetres the head travels.
pub struct Schedule {
    pub pick_t: Vec<f64>,
    pub place_t: Vec<f64>,
    pub travel_mm: f64,
}

pub fn schedule(
    placements: &[Placement],
    plan: &[u32],
    cfg: &PnpConfig,
    start: (f64, f64),
) -> Schedule {
    let mut t = 0.0;
    let mut pos = start;
    let mut travel_mm = 0.0;
    let mut pick_t = Vec::with_capacity(placements.len());
    let mut place_t = Vec::with_capacity(placements.len());
    for (p, slot) in placements.iter().zip(plan) {
        let pick = cfg.pick_pos(*slot);
        let d1 = (pick.0 - pos.0).hypot(pick.1 - pos.1);
        t += d1 / cfg.travel_mm_s + cfg.pick_ms / 1000.0;
        pick_t.push(t);
        let d2 = (p.x - pick.0).hypot(p.y - pick.1);
        t += d2 / cfg.travel_mm_s + cfg.place_ms / 1000.0;
        place_t.push(t);
        travel_mm += d1 + d2;
        pos = (p.x, p.y);
    }
    Schedule {
        pick_t,
        place_t,
        travel_mm,
    }
}

// ---- the machines and their jobs -----------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct PnpMachine {
    feeders: BTreeMap<u32, Feeder>,
    x: f64,
    y: f64,
    placed_total: u64,
    failed_picks: u64,
}

struct PlaceJob {
    placements: Vec<Placement>,
    cfg: PnpConfig,
    plan: Vec<u32>,
    pick_t: Vec<f64>,
    place_t: Vec<f64>,
    fail_at: Option<usize>,
    /// Picks that already took their part off a feeder.
    applied: usize,
    /// Placements finished.
    placed: usize,
}

enum JobKind {
    Load {
        feeders: Vec<(u32, String, u64)>,
        replace: bool,
    },
    Place(Box<PlaceJob>),
}

struct PnpJob {
    machine: String,
    kind: JobKind,
    state: WorkState,
    clock: SimClock,
    rate: f64,
    require_stock: bool,
    message: String,
}

/// The pick-and-place machines of the server: their feeders and their jobs.
#[derive(Default)]
pub struct PnpEngine {
    jobs: Mutex<HashMap<String, PnpJob>>,
    machines: Mutex<HashMap<String, PnpMachine>>,
}

fn lost() -> PluginError {
    PluginError::Failed("the pick-and-place machine lost the job (the server restarted)".into())
}

fn missing() -> PluginError {
    PluginError::Bad("the job is not there".into())
}

/// Moves a running job to where the clock says it is: takes the parts off the feeders, counts the placements and ends the job when it is done or fails.
fn advance(job: &mut PnpJob, m: &mut PnpMachine) {
    if job.state != WorkState::Running {
        return;
    }
    let work = job.clock.work_s(job.rate);
    let PnpJob {
        kind,
        state,
        clock,
        message,
        ..
    } = job;
    let JobKind::Place(p) = kind else {
        *state = WorkState::Done;
        clock.hold();
        return;
    };
    let picks = finished_items(&p.pick_t, work);
    let mut stopped: Option<usize> = None;
    while p.applied < picks {
        let i = p.applied;
        let slot = p.plan[i];
        let taken = match m.feeders.get_mut(&slot) {
            Some(f) if f.count > 0 => {
                f.count -= 1;
                true
            }
            _ => false,
        };
        let reference = &p.placements[i].reference;
        if !taken {
            *message = format!("feeder {slot} is empty at {reference}");
            stopped = Some(i);
            break;
        }
        p.applied += 1;
        if p.fail_at == Some(i) {
            m.failed_picks += 1;
            *message = format!("the vacuum was lost picking {reference} from feeder {slot}");
            stopped = Some(i);
            break;
        }
    }
    let placed = match stopped {
        Some(i) => i,
        None => finished_items(&p.place_t, work).min(p.placements.len()),
    };
    if placed > p.placed {
        m.placed_total += (placed - p.placed) as u64;
        p.placed = placed;
    }
    if placed > 0 {
        if let Some(last) = p.placements.get(placed - 1) {
            m.x = last.x;
            m.y = last.y;
        }
    }
    if let Some(i) = stopped {
        let (px, py) = p.cfg.pick_pos(p.plan[i]);
        m.x = px;
        m.y = py;
        *state = WorkState::Failed;
        clock.hold();
    } else if p.placed >= p.placements.len() {
        *state = WorkState::Done;
        clock.hold();
    }
}

impl PnpEngine {
    pub fn busy(&self, machine: &str) -> bool {
        self.jobs
            .locked()
            .values()
            .any(|j| j.machine == machine && j.state.active())
    }

    /// What a client sees of the machine besides its state: the feeders, the head and the counters.
    pub fn detail(&self, machine: &str, cfg: &PnpConfig) -> Value {
        let m = self
            .machines
            .locked()
            .get(machine)
            .cloned()
            .unwrap_or_default();
        let feeders: Vec<Value> = m
            .feeders
            .iter()
            .map(|(slot, f)| {
                let (x, y) = cfg.pick_pos(*slot);
                json!({ "slot": slot, "part": f.part, "count": f.count, "x_mm": x, "y_mm": y })
            })
            .collect();
        json!({
            "feeders": feeders,
            "slots": cfg.slots,
            "head": { "x_mm": m.x, "y_mm": m.y },
            "placed_total": m.placed_total,
            "failed_picks": m.failed_picks,
            "bed_mm": { "x": cfg.bed_x, "y": cfg.bed_y },
            "require_stock": cfg.require_stock,
        })
    }

    pub fn prepare(
        &self,
        machine: &str,
        cfg: &PnpConfig,
        request: &Value,
    ) -> PluginResult<Prepared> {
        let (kind, view) = match request.get("kind").and_then(Value::as_str) {
            Some("load_feeders") => {
                let (feeders, replace) = parse_load(request, cfg)?;
                let view =
                    json!({ "kind": "load_feeders", "slots": feeders.len(), "replace": replace });
                (JobKind::Load { feeders, replace }, view)
            }
            Some("place") => {
                let placements = parse_place(request, cfg)?;
                let (feeders, start) = {
                    let machines = self.machines.locked();
                    let m = machines.get(machine).cloned().unwrap_or_default();
                    (m.feeders, (m.x, m.y))
                };
                let plan = assign(&placements, &feeders, cfg.require_stock)?;
                let sched = schedule(&placements, &plan, cfg, start);
                let view = place_view(&placements, &plan, &sched);
                let job = PlaceJob {
                    placements,
                    cfg: cfg.clone(),
                    plan: Vec::new(),
                    pick_t: Vec::new(),
                    place_t: Vec::new(),
                    fail_at: None,
                    applied: 0,
                    placed: 0,
                };
                (JobKind::Place(Box::new(job)), view)
            }
            _ => {
                return bad("the request has to be {\"kind\": \"place\" | \"load_feeders\", ...}");
            }
        };
        let native_id = crate::ids::new(crate::ids::Kind::Work);
        self.jobs.locked().insert(
            native_id.clone(),
            PnpJob {
                machine: machine.to_string(),
                kind,
                state: WorkState::Queued,
                clock: SimClock::new(),
                rate: cfg.sim_rate,
                require_stock: cfg.require_stock,
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
        let mut machines = self.machines.locked();
        let m = machines.entry(machine.to_string()).or_default();
        match &mut job.kind {
            JobKind::Load { feeders, replace } => {
                if *replace {
                    m.feeders.clear();
                }
                for (slot, part, count) in feeders.iter() {
                    m.feeders.insert(
                        *slot,
                        Feeder {
                            part: part.clone(),
                            count: *count,
                        },
                    );
                }
            }
            JobKind::Place(p) => {
                // the feeders may have changed since the job was prepared: the plan is made again from what they hold now
                let plan = assign(&p.placements, &m.feeders, job.require_stock)?;
                let sched = schedule(&p.placements, &plan, &p.cfg, (m.x, m.y));
                p.fail_at = (p.cfg.fail_every > 0 && p.placements.len() >= p.cfg.fail_every)
                    .then(|| p.cfg.fail_every - 1);
                p.plan = plan;
                p.pick_t = sched.pick_t;
                p.place_t = sched.place_t;
                p.applied = 0;
                p.placed = 0;
            }
        }
        job.state = WorkState::Running;
        job.clock.run();
        Ok(())
    }

    /// Runs `f` on a job and on its machine after the job was moved to where the clock says it is.
    fn with_job<T>(
        &self,
        native_id: &str,
        f: impl FnOnce(&mut PnpJob, &mut PnpMachine) -> PluginResult<T>,
        lost_is_failure: bool,
    ) -> PluginResult<T> {
        let mut jobs = self.jobs.locked();
        let job =
            jobs.get_mut(native_id)
                .ok_or_else(|| if lost_is_failure { lost() } else { missing() })?;
        let mut machines = self.machines.locked();
        let m = machines.entry(job.machine.clone()).or_default();
        advance(job, m);
        f(job, m)
    }

    pub fn pause(&self, native_id: &str) -> PluginResult<()> {
        self.with_job(
            native_id,
            |job, _| {
                if job.state != WorkState::Running {
                    return Err(PluginError::NotInState);
                }
                job.clock.hold();
                job.state = WorkState::Paused;
                Ok(())
            },
            false,
        )
    }

    pub fn resume(&self, native_id: &str) -> PluginResult<()> {
        self.with_job(
            native_id,
            |job, _| {
                if job.state != WorkState::Paused {
                    return Err(PluginError::NotInState);
                }
                job.clock.run();
                job.state = WorkState::Running;
                Ok(())
            },
            false,
        )
    }

    /// Stops the job where it is: what was placed stays placed and what was picked is gone from its feeder.
    pub fn stop(&self, native_id: &str) -> PluginResult<()> {
        self.with_job(
            native_id,
            |job, _| {
                if !job.state.active() {
                    return Err(PluginError::NotInState);
                }
                job.clock.hold();
                job.state = WorkState::Cancelled;
                job.message = "stopped".into();
                Ok(())
            },
            false,
        )
    }

    pub fn status(&self, native_id: &str) -> PluginResult<PluginStatus> {
        self.with_job(
            native_id,
            |job, _| {
                let (done, total) = match &job.kind {
                    JobKind::Load { .. } => (u64::from(job.state == WorkState::Done), 1),
                    JobKind::Place(p) => (p.placed as u64, p.placements.len() as u64),
                };
                Ok(PluginStatus {
                    state: job.state,
                    done,
                    total,
                    message: job.message.clone(),
                })
            },
            true,
        )
    }

    pub fn discard(&self, native_id: &str) {
        self.jobs.locked().remove(native_id);
    }
}

/// What a client reads of a `place` job that was prepared.
fn place_view(placements: &[Placement], plan: &[u32], sched: &Schedule) -> Value {
    let mut parts: BTreeMap<&str, u64> = BTreeMap::new();
    for p in placements {
        *parts.entry(p.part.as_str()).or_insert(0) += 1;
    }
    let mut slots: BTreeMap<u32, u64> = BTreeMap::new();
    for slot in plan {
        *slots.entry(*slot).or_insert(0) += 1;
    }
    let rows: Vec<Value> = placements
        .iter()
        .zip(plan)
        .take(VIEW_ROWS)
        .map(|(p, slot)| {
            json!({ "ref": p.reference, "part": p.part, "slot": slot, "x": p.x, "y": p.y, "rot": p.rot, "side": p.side })
        })
        .collect();
    json!({
        "kind": "place",
        "placements": placements.len(),
        "parts": parts,
        "slots_used": slots,
        "travel_mm": (sched.travel_mm * 100.0).round() / 100.0,
        "estimate_s": (sched.place_t.last().copied().unwrap_or(0.0) * 100.0).round() / 100.0,
        "rows": rows,
        "rows_cut": placements.len() > VIEW_ROWS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick() -> PnpConfig {
        PnpConfig {
            sim_rate: 0.0,
            ..PnpConfig::default()
        }
    }

    fn load(e: &PnpEngine, cfg: &PnpConfig, feeders: Value, replace: bool) {
        let p = e
            .prepare(
                "p1",
                cfg,
                &json!({"kind": "load_feeders", "feeders": feeders, "replace": replace}),
            )
            .unwrap();
        e.start("p1", &p.native_id).unwrap();
        assert_eq!(e.status(&p.native_id).unwrap().state, WorkState::Done);
    }

    fn two_parts() -> Value {
        json!([
            {"ref": "R1", "part": "R_0603_10k", "x": 20, "y": 30, "rot": 90},
            {"ref": "R2", "part": "R_0603_10k", "x": 25, "y": 30},
            {"ref": "C1", "part": "C_0603_100n", "x": 40.5, "y": 50, "side": "bottom"},
        ])
    }

    fn feeders() -> Value {
        json!([
            {"slot": 1, "part": "R_0603_10k", "count": 5},
            {"slot": 3, "part": "C_0603_100n", "count": 100},
        ])
    }

    #[test]
    fn the_settings_are_checked_and_a_typo_is_refused() {
        assert_eq!(
            PnpConfig::from_settings(&json!({})).unwrap(),
            PnpConfig::default()
        );
        let c = PnpConfig::from_settings(&json!({
            "feeders": 12, "require_stock": false, "sim_fail_pick_every": 3, "sim_rate": 0,
            "feeder_y_mm": -30.5,
        }))
        .unwrap();
        assert_eq!(c.slots, 12);
        assert!(!c.require_stock);
        assert_eq!(c.fail_every, 3);
        assert_eq!(c.pick_pos(3), (26.0, -30.5));
        for bad in [
            json!({"feeder": 3}),
            json!({"feeders": 0}),
            json!({"feeders": 201}),
            json!({"feeders": 2.5}),
            json!({"travel_mm_s": 0}),
            json!({"require_stock": "yes"}),
            json!({"sim_fail_pick_every": -1}),
            json!({"max_placements": 0}),
        ] {
            assert!(PnpConfig::from_settings(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_load_request_is_checked_slot_by_slot() {
        let cfg = quick();
        let ok = parse_load(
            &json!({"feeders": [{"slot": 2, "part": "R_0603_10k", "count": 0}]}),
            &cfg,
        )
        .unwrap();
        assert_eq!(ok.0, vec![(2, "R_0603_10k".to_string(), 0)]);
        assert!(!ok.1);
        for bad in [
            json!({"feeders": []}),
            json!({}),
            json!({"feeders": [{"slot": 0, "part": "a", "count": 1}]}),
            json!({"feeders": [{"slot": 41, "part": "a", "count": 1}]}),
            json!({"feeders": [{"slot": 1, "part": "a b", "count": 1}]}),
            json!({"feeders": [{"slot": 1, "part": "", "count": 1}]}),
            json!({"feeders": [{"slot": 1, "part": "a", "count": 100001}]}),
            json!({"feeders": [{"slot": 1, "part": "a", "count": -1}]}),
            json!({"feeders": [{"slot": 1, "part": "a", "count": 1}, {"slot": 1, "part": "b", "count": 1}]}),
            json!({"feeders": [{"slot": 1, "part": "a", "count": 1}], "replace": 1}),
        ] {
            assert!(parse_load(&bad, &cfg).is_err(), "{bad}");
        }
        // an empty list is how the table is emptied
        assert!(parse_load(&json!({"feeders": [], "replace": true}), &cfg).is_ok());
    }

    #[test]
    fn a_place_request_is_checked_and_the_offset_moves_the_places() {
        let cfg = quick();
        let list = parse_place(
            &json!({"placements": two_parts(), "offset": {"x": 10, "y": -5}}),
            &cfg,
        )
        .unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!((list[0].x, list[0].y, list[0].rot), (30.0, 25.0, 90.0));
        assert_eq!(list[2].side, "bottom");
        assert_eq!(list[1].side, "top");
        let outside = parse_place(
            &json!({"placements": two_parts(), "offset": {"x": 290, "y": 0}}),
            &cfg,
        );
        assert!(matches!(
            outside,
            Err(PluginError::Coded("outside_course", _))
        ));
        for bad in [
            json!({"placements": []}),
            json!({}),
            json!({"placements": [{"ref": "R1", "part": "a", "x": 1}]}),
            json!({"placements": [{"ref": "R1", "part": "a", "x": "1", "y": 1}]}),
            json!({"placements": [{"ref": "", "part": "a", "x": 1, "y": 1}]}),
            json!({"placements": [{"ref": "R 1", "part": "a", "x": 1, "y": 1}]}),
            json!({"placements": [{"ref": "R1", "part": "a", "x": 1, "y": 1, "rot": 361}]}),
            json!({"placements": [{"ref": "R1", "part": "a", "x": 1, "y": 1, "side": "left"}]}),
            json!({"placements": [{"ref": "R1", "part": "a", "x": 1, "y": 1}, {"ref": "R1", "part": "a", "x": 2, "y": 2}]}),
            json!({"placements": [{"ref": "R1", "part": "a", "x": 1, "y": 1}], "offset": {"x": 1}}),
        ] {
            assert!(parse_place(&bad, &cfg).is_err(), "{bad}");
        }
        let many = PnpConfig {
            max_placements: 2,
            ..quick()
        };
        assert!(matches!(
            parse_place(&json!({"placements": two_parts()}), &many),
            Err(PluginError::Coded("too_big", _))
        ));
    }

    #[test]
    fn the_parts_are_taken_from_the_first_feeder_that_has_some_left() {
        let mut table = BTreeMap::new();
        table.insert(
            1,
            Feeder {
                part: "A".into(),
                count: 1,
            },
        );
        table.insert(
            2,
            Feeder {
                part: "A".into(),
                count: 5,
            },
        );
        table.insert(
            3,
            Feeder {
                part: "B".into(),
                count: 1,
            },
        );
        let p = |r: &str, part: &str| Placement {
            reference: r.into(),
            part: part.into(),
            x: 1.0,
            y: 1.0,
            rot: 0.0,
            side: "top",
        };
        let list = vec![p("1", "A"), p("2", "A"), p("3", "B"), p("4", "A")];
        assert_eq!(assign(&list, &table, true).unwrap(), vec![1, 2, 3, 2]);
        // too few of B
        let more = vec![p("1", "B"), p("2", "B")];
        assert!(matches!(
            assign(&more, &table, true),
            Err(PluginError::Coded("pnp_feeder_short", _))
        ));
        // the machine that does not require stock lets it start: the second B goes to the dry feeder
        assert_eq!(assign(&more, &table, false).unwrap(), vec![3, 3]);
        // a part that is in no feeder is refused in both cases
        for require in [true, false] {
            assert!(matches!(
                assign(&[p("1", "Z")], &table, require),
                Err(PluginError::Coded("pnp_feeder_short", _))
            ));
        }
    }

    #[test]
    fn the_schedule_counts_the_travel_and_the_times_of_each_pick_and_place() {
        let cfg = PnpConfig {
            feeder_x0: 0.0,
            feeder_y: 0.0,
            travel_mm_s: 100.0,
            pick_ms: 500.0,
            place_ms: 1000.0,
            ..PnpConfig::default()
        };
        let p = vec![Placement {
            reference: "R1".into(),
            part: "A".into(),
            x: 30.0,
            y: 40.0,
            rot: 0.0,
            side: "top",
        }];
        let s = schedule(&p, &[1], &cfg, (0.0, 0.0));
        assert!((s.pick_t[0] - 0.5).abs() < 1e-9);
        assert!((s.place_t[0] - (0.5 + 0.5 + 1.0)).abs() < 1e-9);
        assert!((s.travel_mm - 50.0).abs() < 1e-9);
    }

    #[test]
    fn a_job_places_everything_and_takes_the_parts_off_the_feeders() {
        let e = PnpEngine::default();
        let cfg = quick();
        load(&e, &cfg, feeders(), false);
        let p = e
            .prepare(
                "p1",
                &cfg,
                &json!({"kind": "place", "placements": two_parts()}),
            )
            .unwrap();
        assert_eq!(p.view["placements"], 3);
        assert_eq!(p.view["parts"]["R_0603_10k"], 2);
        assert_eq!(p.view["slots_used"]["3"], 1);
        assert_eq!(p.view["rows"].as_array().unwrap().len(), 3);
        e.start("p1", &p.native_id).unwrap();
        assert!(e.busy("p1"));
        let st = e.status(&p.native_id).unwrap();
        assert_eq!((st.state, st.done, st.total), (WorkState::Done, 3, 3));
        let d = e.detail("p1", &cfg);
        assert_eq!(d["feeders"][0]["count"], 3);
        assert_eq!(d["feeders"][1]["count"], 99);
        assert_eq!(d["placed_total"], 3);
        assert_eq!(d["head"]["x_mm"], 40.5);
        assert!(!e.busy("p1"));
    }

    #[test]
    fn a_job_that_needs_more_than_the_feeders_hold_is_refused_unless_the_machine_allows_it() {
        let e = PnpEngine::default();
        let cfg = quick();
        load(
            &e,
            &cfg,
            json!([{"slot": 1, "part": "R_0603_10k", "count": 1}, {"slot": 3, "part": "C_0603_100n", "count": 1}]),
            false,
        );
        let r = e.prepare(
            "p1",
            &cfg,
            &json!({"kind": "place", "placements": two_parts()}),
        );
        assert!(matches!(r, Err(PluginError::Coded("pnp_feeder_short", _))));
        let lax = PnpConfig {
            require_stock: false,
            ..quick()
        };
        let p = e
            .prepare(
                "p1",
                &lax,
                &json!({"kind": "place", "placements": two_parts()}),
            )
            .unwrap();
        e.start("p1", &p.native_id).unwrap();
        let st = e.status(&p.native_id).unwrap();
        assert_eq!(st.state, WorkState::Failed);
        assert_eq!(st.done, 1);
        assert!(
            st.message.contains("feeder 1 is empty at R2"),
            "{}",
            st.message
        );
        assert!(!e.busy("p1"));
    }

    #[test]
    fn the_vacuum_lost_at_every_nth_placement_fails_the_job_there() {
        let e = PnpEngine::default();
        let cfg = PnpConfig {
            fail_every: 2,
            ..quick()
        };
        load(&e, &cfg, feeders(), false);
        let p = e
            .prepare(
                "p1",
                &cfg,
                &json!({"kind": "place", "placements": two_parts()}),
            )
            .unwrap();
        e.start("p1", &p.native_id).unwrap();
        let st = e.status(&p.native_id).unwrap();
        assert_eq!((st.state, st.done), (WorkState::Failed, 1));
        assert!(st.message.contains("vacuum"), "{}", st.message);
        let d = e.detail("p1", &cfg);
        assert_eq!(d["failed_picks"], 1);
        // the part of the failed pick is gone from its feeder
        assert_eq!(d["feeders"][0]["count"], 3);
        // a job shorter than N never fails
        let short = e
            .prepare("p1", &cfg, &json!({"kind": "place", "placements": [{"ref": "R9", "part": "R_0603_10k", "x": 5, "y": 5}]}))
            .unwrap();
        e.start("p1", &short.native_id).unwrap();
        assert_eq!(e.status(&short.native_id).unwrap().state, WorkState::Done);
    }

    #[test]
    fn the_feeders_that_changed_between_the_prepare_and_the_start_are_looked_at_again() {
        let e = PnpEngine::default();
        let cfg = quick();
        load(&e, &cfg, feeders(), false);
        let p = e
            .prepare(
                "p1",
                &cfg,
                &json!({"kind": "place", "placements": two_parts()}),
            )
            .unwrap();
        // somebody empties the table before the job starts
        load(&e, &cfg, json!([]), true);
        let r = e.start("p1", &p.native_id);
        assert!(matches!(r, Err(PluginError::Coded("pnp_feeder_short", _))));
        assert_eq!(e.status(&p.native_id).unwrap().state, WorkState::Queued);
        assert!(!e.busy("p1"));
    }

    #[test]
    fn one_job_at_a_time_hold_resume_and_stop() {
        let e = PnpEngine::default();
        let slow = PnpConfig {
            sim_rate: 1.0,
            ..PnpConfig::default()
        };
        load(&e, &quick(), feeders(), false);
        let a = e
            .prepare(
                "p1",
                &slow,
                &json!({"kind": "place", "placements": two_parts()}),
            )
            .unwrap()
            .native_id;
        let b = e
            .prepare(
                "p1",
                &slow,
                &json!({"kind": "place", "placements": two_parts()}),
            )
            .unwrap()
            .native_id;
        assert_eq!(e.pause(&a), Err(PluginError::NotInState));
        e.start("p1", &a).unwrap();
        assert_eq!(e.start("p1", &b), Err(PluginError::Busy));
        e.pause(&a).unwrap();
        assert_eq!(e.status(&a).unwrap().state, WorkState::Paused);
        e.resume(&a).unwrap();
        e.stop(&a).unwrap();
        let st = e.status(&a).unwrap();
        assert_eq!(st.state, WorkState::Cancelled);
        assert_eq!(e.stop(&a), Err(PluginError::NotInState));
        assert!(e.start("p1", &b).is_ok());
    }

    #[test]
    fn the_feeder_table_is_merged_or_replaced_and_discarded_jobs_are_lost() {
        let e = PnpEngine::default();
        let cfg = quick();
        load(&e, &cfg, feeders(), false);
        load(
            &e,
            &cfg,
            json!([{"slot": 1, "part": "X", "count": 7}]),
            false,
        );
        let d = e.detail("p1", &cfg);
        assert_eq!(d["feeders"].as_array().unwrap().len(), 2);
        assert_eq!(d["feeders"][0]["part"], "X");
        load(
            &e,
            &cfg,
            json!([{"slot": 5, "part": "Y", "count": 1}]),
            true,
        );
        let d = e.detail("p1", &cfg);
        assert_eq!(d["feeders"].as_array().unwrap().len(), 1);
        assert_eq!(d["feeders"][0]["slot"], 5);
        let p = e
            .prepare(
                "p1",
                &cfg,
                &json!({"kind": "load_feeders", "feeders": [{"slot": 2, "part": "Z", "count": 1}]}),
            )
            .unwrap();
        e.discard(&p.native_id);
        assert!(matches!(
            e.status(&p.native_id),
            Err(PluginError::Failed(_))
        ));
        assert!(matches!(
            e.start("p1", &p.native_id),
            Err(PluginError::Bad(_))
        ));
        assert!(matches!(
            e.prepare("p1", &cfg, &json!({"kind": "weld"})),
            Err(PluginError::Bad(_))
        ));
    }

    #[test]
    fn the_view_is_cut_at_two_thousand_rows_and_says_so() {
        let e = PnpEngine::default();
        let cfg = quick();
        load(
            &e,
            &cfg,
            json!([{"slot": 1, "part": "A", "count": 5000}]),
            false,
        );
        let list: Vec<Value> = (0..2500)
            .map(|i| json!({"ref": format!("R{i}"), "part": "A", "x": i % 200, "y": 10}))
            .collect();
        let p = e
            .prepare("p1", &cfg, &json!({"kind": "place", "placements": list}))
            .unwrap();
        assert_eq!(p.view["rows"].as_array().unwrap().len(), 2000);
        assert_eq!(p.view["rows_cut"], true);
        assert_eq!(p.view["placements"], 2500);
    }
}
