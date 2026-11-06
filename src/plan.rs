//! From what a client sends to the items the machine runs: a drilling job (holes and tools, or an Excellon file and a tool table) or a
//! routing job (paths and tools, or an HP-GL file and a tool table) is planned here with the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO,
//! using the constants and the tuning of the machine it is for. The same planning is what a preview shows, so what a client draws is what the
//! machine will do.

use eh3d_drillpro::config::{self, MachineSettings};
use eh3d_drillpro::dryrun;
use eh3d_drillpro::job::{
    geometry_from, hole_end_indexes, job_machine, job_rack_for, sort_holes, JobOptions,
    RackRefusal, Sorting,
};
use eh3d_drillpro::ncd::{self, MIL_MM};
use eh3d_drillpro::protocol::job::{
    plan_drill_job, plan_drill_job_marked, Item, JobHole, JobTool, Rack,
};
use eh3d_drillpro::protocol::motion::Geometry;
use eh3d_drillpro::protocol::tuning::Tuning;
use eh3d_drillpro::summary;
use eh3d_drillpro::tools as drill_tools;
use eh3d_routepro::hpgl::{self, Polyline, UNIT_MM};
use eh3d_routepro::job::plan_route_job_atc_marked;
use eh3d_routepro::tools as route_tools;
use eh3d_summary::dryrun::Course;
use eh3d_summary::summary::{course_warnings, Warning};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::machines::MachineRecord;

/// The most holes or points one job may carry.
pub const MAX_POINTS: usize = 200_000;

/// The version of the job request that carries the chaining of the tool changer (`rack`); a request without a `request_version` is the first one.
pub const RACK_REQUEST_VERSION: u32 = 2;

/// The most places a `rack` may name (`rack_max_slots` of `server.cfg`; the tool changer of the machines has fourteen).
static RACK_MAX_SLOTS: AtomicUsize = AtomicUsize::new(14);

/// Sets the most places a `rack` may name; the server does it once when it opens.
pub fn set_rack_max_slots(most: usize) {
    RACK_MAX_SLOTS.store(most, Ordering::Relaxed);
}

/// The place of one tool of the job in the rack (`rack.slots` of a request of the second version).
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct RackSlotIn {
    pub tool: u8,
    pub place: usize,
}

/// The chaining of the tool changer in a request of the second version (`JobRackV2` of the OpenAPI document). Every field is optional and the defaults are the
/// behaviour of a request without it. A field this server does not know (from a later version) is kept in `extra` to be named in a warning, never refused.
#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
pub struct JobRackV2 {
    /// How many places of the rack the tools of the jobs before this one take.
    #[serde(default)]
    pub first: usize,
    /// The chuck comes holding the last tool of the job before.
    #[serde(default)]
    pub carry_in: bool,
    /// This job leaves its last tool in the chuck for the next one.
    #[serde(default)]
    pub carry_out: bool,
    /// The place of each tool of this job, when the person said where each one is.
    #[serde(default)]
    pub slots: Vec<RackSlotIn>,
    /// With `carry_in`, the place of the tool the chuck comes holding.
    #[serde(default)]
    pub carry_slot: Option<usize>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum JobRequest {
    Drill(DrillRequest),
    Route(RouteRequest),
    /// Free G-code, for the machines that take it (the simulator and GRBL).
    Gcode(GcodeRequest),
}

impl JobRequest {
    pub fn name(&self) -> Option<&str> {
        match self {
            JobRequest::Drill(d) => d.name.as_deref(),
            JobRequest::Route(r) => r.name.as_deref(),
            JobRequest::Gcode(g) => g.name.as_deref(),
        }
    }
}

#[derive(Deserialize, Debug, Default)]
pub struct GcodeRequest {
    pub name: Option<String>,
    /// The text of the program.
    pub gcode: String,
    /// How many lines of the text (counting the blank and the comment ones) are already done: a job that goes on after a stop does not send them
    /// again, gives the codes they put in force back and waits for the operator before the first move.
    #[serde(default)]
    pub skip: usize,
    /// When the job goes on after a stop, also start the spindle and the coolant again as they were. Off by default: starting a spindle is for a person to decide.
    #[serde(default)]
    pub restore_outputs: bool,
}

#[derive(Deserialize, Debug, Default)]
pub struct DrillRequest {
    pub name: Option<String>,
    /// Thickness of the board in mm; the machine's configuration file says when it is left out.
    pub board_mm: Option<f64>,
    /// `none`, `x`, `y` or `special` (only for holes that come from a file).
    pub sorting: Option<String>,
    pub tools: Option<Vec<DrillToolIn>>,
    pub holes: Option<Vec<HoleIn>>,
    pub files: Option<DrillFiles>,
    /// Tools that are switched off: their holes are left out of the job.
    #[serde(default)]
    pub skip_tools: Vec<u8>,
    /// How many holes, counted in the order the machine drills them, are already done: a job that goes on after a stop leaves them out.
    #[serde(default)]
    pub skip: usize,
    /// `2` when `rack` is present; absent in the requests of the first version (which take no `rack`).
    pub request_version: Option<u32>,
    /// The chaining of the tool changer and the places of the tools in the rack.
    pub rack: Option<JobRackV2>,
}

#[derive(Deserialize, Debug)]
pub struct DrillToolIn {
    pub number: u8,
    #[serde(default)]
    pub extra_stroke_mm: f64,
    pub feed_mm_min: u32,
    pub spindle_rpm: u32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct HoleIn {
    pub tool: u8,
    pub x_mm: f64,
    pub y_mm: f64,
}

#[derive(Deserialize, Debug)]
pub struct DrillFiles {
    /// The text of an Excellon `.NCD` file.
    pub ncd: String,
    /// The text of the tool table `.DTD`.
    pub dtd: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
pub struct RouteRequest {
    pub name: Option<String>,
    pub tools: Option<Vec<RouteToolIn>>,
    pub paths: Option<Vec<PathIn>>,
    pub files: Option<RouteFiles>,
    /// Pens (tools) that are switched off: their paths are left out of the job.
    #[serde(default)]
    pub skip_tools: Vec<u8>,
    /// How many paths, in the order they are sent, are already done: a job that goes on after a stop leaves them out.
    #[serde(default)]
    pub skip: usize,
    /// Only the first paths that are left (used to find how many paths a stopped job had done); never read from a client.
    #[serde(skip)]
    pub only_first: Option<usize>,
    /// `2` when `rack` is present; absent in the requests of the first version (which take no `rack`).
    pub request_version: Option<u32>,
    /// The chaining of the tool changer and the places of the tools in the rack.
    pub rack: Option<JobRackV2>,
}

#[derive(Deserialize, Debug)]
pub struct RouteToolIn {
    pub number: u8,
    pub depth_mm: f64,
    pub iterations: u32,
    pub feed_xy_mm_min: u32,
    pub feed_z_mm_min: u32,
    pub spindle_rpm: u32,
}

#[derive(Deserialize, Debug)]
pub struct PathIn {
    pub pen: u8,
    pub points_mm: Vec<[f64; 2]>,
}

#[derive(Deserialize, Debug)]
pub struct RouteFiles {
    /// The text of an HP-GL `.PLT` file.
    pub plt: String,
    /// The text of the tool table `.RTD`.
    pub rtd: Option<String>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct PlanError {
    pub code: &'static str,
    pub detail: String,
}

fn fail<T>(code: &'static str, detail: impl Into<String>) -> Result<T, PlanError> {
    Err(PlanError {
        code,
        detail: detail.into(),
    })
}

/// What a client draws and follows: the work, in mm and in the order the machine does it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct JobView {
    pub kind: String,
    pub name: String,
    pub tools: Vec<ToolView>,
    pub holes: Vec<HoleView>,
    pub paths: Vec<PathView>,
    /// min x, min y, max x, max y of the work, in mm.
    pub bounds_mm: Option<[f64; 4]>,
    /// How many items the machine runs, and at which of them each hole is finished.
    pub items: usize,
    pub hole_ends: Vec<usize>,
    /// An estimate of how long the movements take, in seconds, without the dialogs of the operator.
    pub estimate_s: f64,
    pub travel_mm: f64,
    /// The tools that were switched off for this job and how many holes or paths were left out (by them or because they were already done).
    #[serde(default)]
    pub skipped_tools: Vec<u8>,
    #[serde(default)]
    pub skipped: usize,
    /// What the job asks for beyond the spindle the machine carries, when the machine's switch is on `warn` (with `reject` such a job is refused with `spindle_limit`).
    #[serde(default)]
    pub spindle_warnings: Vec<crate::spindle::Finding>,
    /// The dry run of the exact plan: how far each axis goes and how long the movements take. Absent for a G-code job.
    #[serde(default)]
    pub dry_run: Option<DryRunView>,
    /// What looks wrong in the work that was loaded (a unit that is probably wrong, a work bigger than the table, a scale that is not 1, an axis that
    /// goes past its course when the machine's switch is on `warn`). Each one has a `code` and the numbers it is about, in mm.
    #[serde(default)]
    pub warnings: Vec<WorkWarning>,
    /// Whether the plan used the `rack` of the request: `true` when it did, `false` when it was left out (see the `rack_*` warnings); absent when the request had none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rack_applied: Option<bool>,
}

/// The dry run of a plan, from the switches of the machine, in mm.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DryRunView {
    pub min_mm: [f64; 3],
    pub max_mm: [f64; 3],
    pub end_mm: [f64; 3],
    pub steps: u64,
    pub seconds: f64,
    pub within_course: bool,
}

/// One thing that looks wrong in the work: `tiny` (`values`: width, height), `too_big` (width, height, table x, table y), `far` (distance from the
/// corner), `scale` (x, y) or `out_of_course` (`axis` is `X`, `Y` or `Z`; `values`: how far past the course).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WorkWarning {
    pub code: String,
    #[serde(default)]
    pub axis: String,
    pub values: Vec<f64>,
    /// A word the warning is about (the name of a field that was left out); empty for the others.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

fn warning_view(w: &Warning) -> WorkWarning {
    let (code, axis, values) = match w {
        Warning::Tiny {
            width_mm,
            height_mm,
        } => ("tiny", String::new(), vec![*width_mm, *height_mm]),
        Warning::TooBig {
            width_mm,
            height_mm,
            table_x_mm,
            table_y_mm,
        } => (
            "too_big",
            String::new(),
            vec![*width_mm, *height_mm, *table_x_mm, *table_y_mm],
        ),
        Warning::Far { from_mm } => ("far", String::new(), vec![*from_mm]),
        Warning::Scale { x, y } => ("scale", String::new(), vec![*x, *y]),
        Warning::OutOfCourse { axis, excess_mm } => {
            ("out_of_course", axis.letter().to_string(), vec![*excess_mm])
        }
    };
    WorkWarning {
        code: code.into(),
        axis,
        values,
        detail: String::new(),
    }
}

/// The warning of a part of the request that was left out.
fn rack_warning(code: &str, detail: &str, values: Vec<f64>) -> WorkWarning {
    WorkWarning {
        code: code.into(),
        axis: String::new(),
        values,
        detail: detail.into(),
    }
}

/// What the `rack` of a request came to once judged against the machine: the rack to plan with (if any), whether it was applied and what was left out.
struct RackDecision {
    rack: Option<Rack>,
    applied: Option<bool>,
    warnings: Vec<WorkWarning>,
}

/// Judges the chaining of the tool changer of a request. Without `rack` nothing changes. A request of the first version (or with no version) keeps the plan it always
/// had and says the field was left out. A request of the second version is checked (`422 bad_rack`: places that repeat, a place that is the test pin, more places
/// than `rack_max_slots`, a `carry_slot` that does not fit, a place the machine file does not define or the machine does not reach) and planned with the rack; a machine
/// that changes its tools by hand cannot chain them, so the plan is the old one and the answer says so. `tools` is how many tools the job uses.
fn decide_rack(
    settings: Option<&MachineSettings>,
    version: Option<u32>,
    rack: Option<&JobRackV2>,
    tools: usize,
) -> Result<RackDecision, PlanError> {
    let mut warnings = Vec::new();
    if let Some(v) = version {
        if v == 0 {
            return fail("bad_request", "request_version must be 1 or more");
        }
        if v > RACK_REQUEST_VERSION {
            warnings.push(rack_warning(
                "request_version_newer",
                "",
                vec![f64::from(v)],
            ));
        }
    }
    let Some(rack) = rack else {
        return Ok(RackDecision {
            rack: None,
            applied: None,
            warnings,
        });
    };
    if version.unwrap_or(1) < RACK_REQUEST_VERSION {
        warnings.push(rack_warning("rack_needs_v2", "", Vec::new()));
        return Ok(RackDecision {
            rack: None,
            applied: Some(false),
            warnings,
        });
    }
    let most = RACK_MAX_SLOTS.load(Ordering::Relaxed);
    if rack.first > most {
        return fail("bad_rack", format!("rack.first may be up to {most} places"));
    }
    if rack.slots.len() > most {
        return fail(
            "bad_rack",
            format!("rack.slots may name up to {most} places"),
        );
    }
    let mut seen_tools: Vec<u8> = Vec::new();
    let mut seen_places: Vec<usize> = Vec::new();
    for slot in &rack.slots {
        if slot.place == 0 {
            return fail(
                "bad_rack",
                format!(
                    "rack.slots: place 0 is the test pin, not a place for tool {}",
                    slot.tool
                ),
            );
        }
        if seen_tools.contains(&slot.tool) {
            return fail(
                "bad_rack",
                format!("rack.slots: tool {} is given two places", slot.tool),
            );
        }
        if seen_places.contains(&slot.place) {
            return fail(
                "bad_rack",
                format!("rack.slots: place {} is given to two tools", slot.place),
            );
        }
        seen_tools.push(slot.tool);
        seen_places.push(slot.place);
    }
    if let Some(carried) = rack.carry_slot {
        if !rack.carry_in {
            return fail("bad_rack", "rack.carry_slot needs rack.carry_in");
        }
        if carried == 0 {
            return fail("bad_rack", "rack.carry_slot: place 0 is the test pin");
        }
        if seen_places.contains(&carried) {
            return fail(
                "bad_rack",
                format!("rack.carry_slot: place {carried} is also the place of a tool of this job"),
            );
        }
    }
    for key in rack.extra.keys() {
        warnings.push(rack_warning("rack_unknown_field", key, Vec::new()));
    }
    let options = JobOptions {
        rack_first: rack.first,
        rack_carry_in: rack.carry_in,
        rack_carry_out: rack.carry_out,
        rack_slots: rack.slots.iter().map(|s| (s.tool, s.place)).collect(),
        rack_carry_slot: rack.carry_slot,
        ..JobOptions::default()
    };
    match job_rack_for(settings, tools, &options) {
        Ok(Some(planned)) => Ok(RackDecision {
            rack: Some(planned),
            applied: Some(true),
            warnings,
        }),
        Ok(None) => {
            warnings.push(rack_warning("rack_no_changer", "", Vec::new()));
            Ok(RackDecision {
                rack: None,
                applied: Some(false),
                warnings,
            })
        }
        Err(RackRefusal::TooManyTools(n)) => fail(
            "bad_rack",
            format!("the rack of the machine holds {n} tools at the most"),
        ),
        Err(RackRefusal::Gap(place)) => fail(
            "bad_rack",
            format!(
                "place {place} of the rack is not defined in the configuration file of the machine"
            ),
        ),
        Err(RackRefusal::Outside(place)) => fail(
            "bad_rack",
            format!("place {place} of the rack is out of the reach of the machine"),
        ),
    }
}

/// The dry run of the exact plan against the course of the machine, and the sanity checks of the work. A plan that goes past the course is
/// refused with `outside_course` when the machine's tuning says `dry_run = reject` (the default) and only listed when it says `warn`. Without a
/// configuration file of the machine its course is not known and nothing is judged.
fn measure(
    items: &[Item],
    geometry: &Geometry,
    settings: Option<&MachineSettings>,
    tuning: &Tuning,
    bounds: Option<[f64; 4]>,
    count: usize,
) -> Result<(DryRunView, Vec<WorkWarning>), PlanError> {
    let course = settings.map_or(
        Course {
            x_mm: f64::INFINITY,
            y_mm: f64::INFINITY,
            z_mm: f64::INFINITY,
        },
        Course::from_settings,
    );
    let dry = dryrun::run(items, geometry, &course, tuning.dry_run_margin_mm);
    let mut warnings = summary::check_work(bounds, count, settings, tuning);
    let out = course_warnings(&dry);
    if tuning.dry_run_reject {
        if let Some(Warning::OutOfCourse { axis, excess_mm }) = out.first() {
            return fail(
                "outside_course",
                format!(
                    "the exact steps of the plan take axis {} {:.1} mm past its course (the machine's tuning decides with dry_run: reject or warn)",
                    axis.letter(),
                    excess_mm
                ),
            );
        }
    }
    warnings.extend(out);
    let view = DryRunView {
        min_mm: [dry.min_mm.0, dry.min_mm.1, dry.min_mm.2],
        max_mm: [dry.max_mm.0, dry.max_mm.1, dry.max_mm.2],
        end_mm: [dry.end_mm.0, dry.end_mm.1, dry.end_mm.2],
        steps: dry.steps,
        seconds: dry.seconds,
        within_course: dry.within_course(),
    };
    Ok((view, warnings.iter().map(warning_view).collect()))
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ToolView {
    pub number: u8,
    pub spindle_rpm: u32,
    /// Drilling: the holes with this tool. Routing: the paths with this pen.
    pub count: usize,
    pub feed_mm_min: u32,
    pub depth_mm: Option<f64>,
    pub iterations: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HoleView {
    pub tool: u8,
    pub x_mm: f64,
    pub y_mm: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PathView {
    pub pen: u8,
    pub points_mm: Vec<[f64; 2]>,
}

pub struct Planned {
    pub items: Vec<Item>,
    pub view: JobView,
    /// The lines of a G-code job (`items` is empty then).
    pub gcode: Option<crate::gcode::Program>,
}

fn finite(v: f64) -> bool {
    v.is_finite() && v.abs() < 1.0e6
}

/// Seconds the movements of the items take (the searches at their full length, the dialogs not counted).
pub fn estimate_seconds(items: &[Item]) -> f64 {
    use eh3d_drillpro::protocol::latches::Op;
    let mut us: f64 = 0.0;
    for item in items {
        match item {
            Item::Steps(s) | Item::Search(s) | Item::Seek(s) => {
                us += s.iter().map(|x| x.delay_us).sum::<f64>()
            }
            Item::Unstick(u) => {
                us +=
                    u.x.iter()
                        .chain(u.y.iter())
                        .map(|x| x.delay_us)
                        .sum::<f64>()
            }
            Item::Latch(ops) => {
                for op in ops {
                    if let Op::WaitUs(w) = op {
                        us += *w as f64;
                    }
                }
            }
            Item::Spindle { wait_ms, .. } => us += f64::from(*wait_ms) * 1000.0,
            Item::Pause(_) => {}
        }
    }
    us / 1.0e6
}

fn settings_of(text: &str) -> Option<MachineSettings> {
    if text.trim().is_empty() {
        None
    } else {
        config::parse(text).and_then(|c| c.settings())
    }
}

fn sorting_of(
    name: Option<&str>,
    settings: Option<&MachineSettings>,
) -> Result<Sorting, PlanError> {
    Ok(match name.map(|n| n.to_ascii_lowercase()).as_deref() {
        Some("none") => Sorting::None,
        Some("x") => Sorting::XFirst,
        Some("y") => Sorting::YFirst,
        Some("special") => Sorting::Special,
        Some(other) => {
            return fail(
                "bad_sorting",
                format!("sorting must be none, x, y or special, not '{other}'"),
            )
        }
        None => settings.map_or(Sorting::Special, |m| match m.sorting {
            config::SortMode::None => Sorting::None,
            config::SortMode::XFirst => Sorting::XFirst,
            config::SortMode::YFirst => Sorting::YFirst,
            config::SortMode::Special => Sorting::Special,
        }),
    })
}

/// A point of the work must fall on the table: with the offset of the configuration it has to be inside the course of the machine.
fn on_table(settings: Option<&MachineSettings>, x: f64, y: f64) -> bool {
    match settings {
        Some(m) => {
            x >= 0.0
                && y >= 0.0
                && x + m.offset_mm.0 <= m.clip_mm.0
                && y + m.offset_mm.1 <= m.clip_mm.1
        }
        None => x >= 0.0 && y >= 0.0,
    }
}

/// What to do with the excesses found against the spindle: with the switch on `reject` the job is refused; with `warn` they go in the job.
fn spindle_verdict(
    guard: &Option<crate::spindle::Guard>,
    excess: Vec<crate::spindle::Finding>,
) -> Result<Vec<crate::spindle::Finding>, PlanError> {
    match guard {
        Some(g) if g.mode == crate::spindle::Mode::Reject && !excess.is_empty() => fail(
            "spindle_limit",
            format!(
                "the job asks for more than the spindle of the machine gives: {}",
                crate::spindle::describe(&excess)
            ),
        ),
        _ => Ok(excess),
    }
}

pub fn plan(machine: &MachineRecord, request: &JobRequest) -> Result<Planned, PlanError> {
    match request {
        JobRequest::Drill(_) | JobRequest::Route(_) if !machine.takes_drill_and_route() => fail(
            "wrong_machine",
            "this machine takes G-code jobs, not drilling or routing jobs",
        ),
        JobRequest::Gcode(_) if !machine.takes_gcode() => fail(
            "wrong_machine",
            "this machine takes drilling and routing jobs, not G-code",
        ),
        JobRequest::Drill(d) => plan_drill(machine, d),
        JobRequest::Route(r) => plan_route(machine, r),
        JobRequest::Gcode(g) => plan_gcode(machine, g),
    }
}

/// A G-code job: the lines that are sent, what they ask of the spindle judged against the machine's, and the view a client follows (no holes or paths: the
/// program is the work; the bounds and the time are read from its straight moves).
fn plan_gcode(machine: &MachineRecord, req: &GcodeRequest) -> Result<Planned, PlanError> {
    let guard = machine.spindle_guard_for_jobs();
    let mut program = crate::gcode::parse_with(
        &req.gcode,
        req.skip,
        req.restore_outputs,
        machine.allow_dollar,
    )
    .map_err(|e| PlanError {
        code: e.code,
        detail: e.detail,
    })?;
    // the whole text is read, also the part that was done: its codes are given again when the job goes on
    let all =
        crate::gcode::parse_with(&req.gcode, 0, false, machine.allow_dollar).map_err(|e| {
            PlanError {
                code: e.code,
                detail: e.detail,
            }
        })?;
    let rapid = guard
        .as_ref()
        .map_or(crate::gcode::DEFAULT_RAPID_MM_MIN, |g| {
            f64::from(g.limits.max_xy_mm_min)
        });
    let reading = crate::gcode::read(&all.entries, rapid);
    let excess = match &guard {
        Some(g) => g.check(
            0,
            reading.max_rpm.round().min(f64::from(u32::MAX)) as u32,
            Some(reading.max_feed_mm_min.round().min(f64::from(u32::MAX)) as u32)
                .filter(|f| *f > 0),
            None,
        ),
        None => Vec::new(),
    };
    let spindle_warnings = spindle_verdict(&guard, excess)?;
    if let Some(g) = guard.filter(|g| g.mode == crate::spindle::Mode::Warn) {
        let top = f64::from(g.limits.max_rpm);
        for e in &mut program.entries {
            e.text = crate::gcode::cut_speed(&e.text, top);
        }
    }
    let work = program.work();
    let view = JobView {
        kind: "gcode".into(),
        name: req.name.clone().unwrap_or_default(),
        tools: Vec::new(),
        holes: Vec::new(),
        paths: Vec::new(),
        bounds_mm: bounds_of(reading.points.iter().copied()),
        items: work,
        hole_ends: Vec::new(),
        estimate_s: reading.estimate_s,
        travel_mm: reading.travel_mm,
        skipped_tools: Vec::new(),
        skipped: req.skip,
        spindle_warnings,
        dry_run: None,
        warnings: Vec::new(),
        rack_applied: None,
    };
    Ok(Planned {
        items: Vec::new(),
        view,
        gcode: Some(program),
    })
}

fn plan_drill(machine: &MachineRecord, req: &DrillRequest) -> Result<Planned, PlanError> {
    let settings = settings_of(&machine.dcf);
    let connection = machine.connection_config().map_err(|e| PlanError {
        code: e.code(),
        detail: "the machine's tuning does not parse".into(),
    })?;
    let geometry = geometry_from(settings.as_ref());
    let board_mm = req
        .board_mm
        .or(settings.as_ref().map(|m| m.board_mm))
        .unwrap_or(1.0);
    if !finite(board_mm) || board_mm <= 0.0 || board_mm > 50.0 {
        return fail("bad_board", "board_mm must be from 0 to 50");
    }

    // the holes, in mm from the file's zero, and the tool table
    let mut holes: Vec<HoleIn> = Vec::new();
    let mut tool_rows: Vec<DrillToolIn> = Vec::new();
    match (&req.files, &req.holes) {
        (Some(files), None) => {
            let file = ncd::parse(&files.ncd).map_err(|e| PlanError {
                code: "bad_ncd",
                detail: format!("{e:?}"),
            })?;
            let unit = settings.as_ref().map_or(MIL_MM, |m| m.unit.mm());
            let scale = settings.as_ref().map_or((1.0, 1.0), |m| m.scale);
            let sorting = sorting_of(req.sorting.as_deref(), settings.as_ref())?;
            for h in sort_holes(&file, sorting) {
                holes.push(HoleIn {
                    tool: h.tool,
                    x_mm: h.x_mil as f64 * unit * scale.0,
                    y_mm: h.y_mil as f64 * unit * scale.1,
                });
            }
            if let Some(dtd) = &files.dtd {
                for t in drill_tools::parse(dtd) {
                    tool_rows.push(DrillToolIn {
                        number: t.number,
                        extra_stroke_mm: t.extra_stroke_mm,
                        feed_mm_min: t.feed_mm_min,
                        spindle_rpm: t.spindle_rpm,
                    });
                }
            }
        }
        (None, Some(list)) => {
            for h in list {
                holes.push(HoleIn {
                    tool: h.tool,
                    x_mm: h.x_mm,
                    y_mm: h.y_mm,
                });
            }
        }
        (Some(_), Some(_)) => return fail("both_sources", "send the holes or the files, not both"),
        (None, None) => return fail("no_work", "the job has no holes and no file"),
    }
    if let Some(list) = &req.tools {
        tool_rows = list
            .iter()
            .map(|t| DrillToolIn {
                number: t.number,
                extra_stroke_mm: t.extra_stroke_mm,
                feed_mm_min: t.feed_mm_min,
                spindle_rpm: t.spindle_rpm,
            })
            .collect();
    }
    let before = holes.len();
    holes.retain(|h| !req.skip_tools.contains(&h.tool));
    if req.skip > 0 {
        // the holes already done are counted in the order the machine drills them: by tool in order of first use
        let mut order: Vec<u8> = Vec::new();
        for h in &holes {
            if !order.contains(&h.tool) {
                order.push(h.tool);
            }
        }
        let mut ordered: Vec<HoleIn> = Vec::with_capacity(holes.len());
        for &number in &order {
            ordered.extend(holes.iter().filter(|h| h.tool == number).cloned());
        }
        ordered.drain(..req.skip.min(ordered.len()));
        holes = ordered;
    }
    let skipped = before - holes.len();
    if holes.is_empty() {
        return fail(
            "no_work",
            if before > 0 {
                "every hole is left out by the tools that are switched off or by the holes already done"
            } else {
                "the job has no holes"
            },
        );
    }
    if holes.len() > MAX_POINTS {
        return fail(
            "too_big",
            format!("a job may have up to {MAX_POINTS} holes"),
        );
    }
    for h in &holes {
        if !finite(h.x_mm) || !finite(h.y_mm) {
            return fail("bad_hole", "a hole has a coordinate that is not a number");
        }
        if !on_table(settings.as_ref(), h.x_mm, h.y_mm) {
            return fail(
                "out_of_table",
                format!(
                    "the hole at {:.2}, {:.2} mm is outside the table",
                    h.x_mm, h.y_mm
                ),
            );
        }
    }
    // every tool the work uses must be in the table, with sane numbers: the planner would otherwise take cautious guesses that nobody chose
    let mut used: Vec<u8> = Vec::new();
    for h in &holes {
        if !used.contains(&h.tool) {
            used.push(h.tool);
        }
    }
    let mut job_tools: Vec<JobTool> = Vec::new();
    let guard = machine.spindle_guard_for_jobs();
    let mut excess: Vec<crate::spindle::Finding> = Vec::new();
    for &number in &used {
        let Some(row) = tool_rows.iter().find(|t| t.number == number) else {
            return fail(
                "unknown_tool",
                format!("tool {number} is not in the tool table"),
            );
        };
        if row.feed_mm_min == 0
            || row.feed_mm_min > 20_000
            || row.spindle_rpm > 200_000
            || !finite(row.extra_stroke_mm)
            || row.extra_stroke_mm < 0.0
        {
            return fail("bad_tool", format!("tool {number} has values out of range"));
        }
        let mut rpm = row.spindle_rpm;
        if let Some(g) = &guard {
            // the drilling feed is the plunge of the spindle
            excess.extend(g.check(number, rpm, None, Some(row.feed_mm_min)));
            rpm = g.turns(rpm);
        }
        job_tools.push(JobTool {
            number,
            extra_stroke_mm: row.extra_stroke_mm,
            feed_mm_min: row.feed_mm_min,
            spindle_rpm: rpm,
        });
    }
    let spindle_warnings = spindle_verdict(&guard, excess)?;
    let job_holes: Vec<JobHole> = holes
        .iter()
        .map(|h| JobHole {
            tool: h.tool,
            x_mm: h.x_mm,
            y_mm: h.y_mm,
        })
        .collect();

    let decision = decide_rack(
        settings.as_ref(),
        req.request_version,
        req.rack.as_ref(),
        used.len(),
    )?;
    let jm = job_machine(settings.as_ref(), geometry, &connection, board_mm);
    let (items, hole_ends) = match &decision.rack {
        Some(rack) => plan_drill_job_marked(&jm, &job_tools, &job_holes, &[], Some(rack)),
        None => {
            let items = plan_drill_job(&jm, &job_tools, &job_holes);
            let ends = hole_end_indexes(&items);
            (items, ends)
        }
    };

    // the holes in the order the machine drills them: by tool in order of first use, the order given inside each tool
    let mut ordered: Vec<HoleView> = Vec::with_capacity(holes.len());
    for &number in &used {
        ordered.extend(holes.iter().filter(|h| h.tool == number).map(|h| HoleView {
            tool: h.tool,
            x_mm: h.x_mm,
            y_mm: h.y_mm,
        }));
    }
    let mut travel = 0.0;
    for pair in ordered.windows(2) {
        if pair[0].tool == pair[1].tool {
            travel += ((pair[1].x_mm - pair[0].x_mm).powi(2)
                + (pair[1].y_mm - pair[0].y_mm).powi(2))
            .sqrt();
        }
    }
    let bounds = bounds_of(ordered.iter().map(|h| (h.x_mm, h.y_mm)));
    let (dry_run, mut warnings) = measure(
        &items,
        &geometry,
        settings.as_ref(),
        &connection.tuning,
        bounds,
        holes.len(),
    )?;
    warnings.extend(decision.warnings);
    let tools = job_tools
        .iter()
        .map(|t| ToolView {
            number: t.number,
            spindle_rpm: t.spindle_rpm,
            count: holes.iter().filter(|h| h.tool == t.number).count(),
            feed_mm_min: t.feed_mm_min,
            depth_mm: None,
            iterations: None,
        })
        .collect();
    let view = JobView {
        kind: "drill".into(),
        name: req.name.clone().unwrap_or_default(),
        tools,
        holes: ordered,
        paths: Vec::new(),
        bounds_mm: bounds,
        items: items.len(),
        hole_ends,
        estimate_s: estimate_seconds(&items),
        travel_mm: travel,
        skipped_tools: req.skip_tools.clone(),
        skipped,
        spindle_warnings,
        dry_run: Some(dry_run),
        warnings,
        rack_applied: decision.applied,
    };
    Ok(Planned {
        items,
        view,
        gcode: None,
    })
}

fn bounds_of(points: impl Iterator<Item = (f64, f64)>) -> Option<[f64; 4]> {
    let mut it = points;
    let first = it.next()?;
    let mut b = [first.0, first.1, first.0, first.1];
    for (x, y) in it {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    Some(b)
}

fn plan_route(machine: &MachineRecord, req: &RouteRequest) -> Result<Planned, PlanError> {
    // a machine keeps its routing constants in ROUTEPRO.RCF; without one the drilling file's are used
    let settings = settings_of(&machine.rcf).or_else(|| settings_of(&machine.dcf));
    let connection = machine.connection_config().map_err(|e| PlanError {
        code: e.code(),
        detail: "the machine's tuning does not parse".into(),
    })?;
    let geometry = geometry_from(settings.as_ref());

    let mut paths: Vec<Polyline> = Vec::new();
    let mut tool_rows: Vec<route_tools::Tool> = Vec::new();
    match (&req.files, &req.paths) {
        (Some(files), None) => {
            paths = hpgl::parse(&files.plt).paths;
            if let Some(rtd) = &files.rtd {
                tool_rows = route_tools::parse(rtd);
            }
        }
        (None, Some(list)) => {
            for p in list {
                let mut points = Vec::with_capacity(p.points_mm.len());
                for pt in &p.points_mm {
                    if !finite(pt[0]) || !finite(pt[1]) {
                        return fail("bad_path", "a point has a coordinate that is not a number");
                    }
                    points.push((
                        (pt[0] / UNIT_MM).round() as i32,
                        (pt[1] / UNIT_MM).round() as i32,
                    ));
                }
                paths.push(Polyline { pen: p.pen, points });
            }
        }
        (Some(_), Some(_)) => return fail("both_sources", "send the paths or the files, not both"),
        (None, None) => return fail("no_work", "the job has no paths and no file"),
    }
    if let Some(list) = &req.tools {
        tool_rows = list
            .iter()
            .map(|t| route_tools::Tool {
                number: t.number,
                depth_mm: t.depth_mm,
                iterations: t.iterations,
                feed_xy_mm_min: t.feed_xy_mm_min,
                feed_z_mm_min: t.feed_z_mm_min,
                spindle_rpm: t.spindle_rpm,
            })
            .collect();
    }
    paths.retain(|p| !p.points.is_empty());
    let before = paths.len();
    paths.retain(|p| !req.skip_tools.contains(&p.pen));
    paths.drain(..req.skip.min(paths.len()));
    let skipped = before - paths.len();
    if let Some(n) = req.only_first {
        paths.truncate(n);
    }
    if paths.is_empty() {
        return fail(
            "no_work",
            if before > 0 {
                "every path is left out by the tools that are switched off or by the paths already done"
            } else {
                "the job has no paths"
            },
        );
    }
    let points: usize = paths.iter().map(|p| p.points.len()).sum();
    if points > MAX_POINTS {
        return fail(
            "too_big",
            format!("a job may have up to {MAX_POINTS} points"),
        );
    }
    for p in &paths {
        for &(x, y) in &p.points {
            let (mx, my) = (f64::from(x) * UNIT_MM, f64::from(y) * UNIT_MM);
            if !on_table(settings.as_ref(), mx, my) {
                return fail(
                    "out_of_table",
                    format!("the point at {mx:.2}, {my:.2} mm is outside the table"),
                );
            }
        }
    }
    let mut pens: Vec<u8> = Vec::new();
    for p in &paths {
        if !pens.contains(&p.pen) {
            pens.push(p.pen);
        }
    }
    let guard = machine.spindle_guard_for_jobs();
    let mut excess: Vec<crate::spindle::Finding> = Vec::new();
    for &pen in &pens {
        let Some(t) = tool_rows.iter().find(|t| t.number == pen) else {
            return fail(
                "unknown_tool",
                format!("pen {pen} has no tool in the tool table"),
            );
        };
        if !finite(t.depth_mm)
            || t.depth_mm <= 0.0
            || t.depth_mm > 40.0
            || t.iterations == 0
            || t.iterations > 100
            || t.feed_xy_mm_min == 0
            || t.feed_xy_mm_min > 20_000
            || t.feed_z_mm_min == 0
            || t.feed_z_mm_min > 20_000
            || t.spindle_rpm > 200_000
        {
            return fail(
                "bad_tool",
                format!("the tool of pen {pen} has values out of range"),
            );
        }
    }
    if let Some(g) = &guard {
        for &pen in &pens {
            if let Some(t) = tool_rows.iter().find(|t| t.number == pen) {
                excess.extend(g.check(
                    pen,
                    t.spindle_rpm,
                    Some(t.feed_xy_mm_min),
                    Some(t.feed_z_mm_min),
                ));
            }
        }
        for t in tool_rows.iter_mut() {
            t.spindle_rpm = g.turns(t.spindle_rpm);
        }
    }
    let spindle_warnings = spindle_verdict(&guard, excess)?;
    let board = settings.as_ref().map_or(1.6, |m| m.board_mm);
    let jm = job_machine(settings.as_ref(), geometry, &connection, board);
    // the item that ends each path, so that a run tells how many paths are done from the item it has reached
    let decision = decide_rack(
        settings.as_ref(),
        req.request_version,
        req.rack.as_ref(),
        pens.len(),
    )?;
    let (items, path_ends) =
        plan_route_job_atc_marked(&paths, &tool_rows, &jm, None, decision.rack.as_ref());

    let mut length = 0.0;
    let view_paths: Vec<PathView> = paths
        .iter()
        .map(|p| {
            let pts: Vec<[f64; 2]> = p
                .points
                .iter()
                .map(|&(x, y)| [f64::from(x) * UNIT_MM, f64::from(y) * UNIT_MM])
                .collect();
            length += pts
                .windows(2)
                .map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt())
                .sum::<f64>();
            PathView {
                pen: p.pen,
                points_mm: pts,
            }
        })
        .collect();
    let bounds = bounds_of(
        view_paths
            .iter()
            .flat_map(|p| p.points_mm.iter().map(|q| (q[0], q[1]))),
    );
    let (dry_run, mut warnings) = measure(
        &items,
        &geometry,
        settings.as_ref(),
        &connection.tuning,
        bounds,
        paths.len(),
    )?;
    warnings.extend(decision.warnings);
    let tools = pens
        .iter()
        .filter_map(|&pen| tool_rows.iter().find(|t| t.number == pen))
        .map(|t| ToolView {
            number: t.number,
            spindle_rpm: t.spindle_rpm,
            count: paths.iter().filter(|p| p.pen == t.number).count(),
            feed_mm_min: t.feed_xy_mm_min,
            depth_mm: Some(t.depth_mm),
            iterations: Some(t.iterations),
        })
        .collect();
    let view = JobView {
        kind: "route".into(),
        name: req.name.clone().unwrap_or_default(),
        tools,
        holes: Vec::new(),
        paths: view_paths,
        bounds_mm: bounds,
        items: items.len(),
        hole_ends: path_ends,
        estimate_s: estimate_seconds(&items),
        travel_mm: length,
        skipped_tools: req.skip_tools.clone(),
        skipped,
        spindle_warnings,
        dry_run: Some(dry_run),
        warnings,
        rack_applied: decision.applied,
    };
    Ok(Planned {
        items,
        view,
        gcode: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::Connection;

    fn machine() -> MachineRecord {
        MachineRecord {
            id: "m".into(),
            name: "M".into(),
            model: String::new(),
            station: String::new(),
            connection: Connection::Demo,
            dcf: String::new(),
            rcf: String::new(),
            cfg: String::new(),
            spindle: String::new(),
            spindle_guard: String::new(),
            spindle_original: None,
            spindle_vfd: None,
            allow_dollar: false,
        }
    }

    const TWO_TOOLS: &str = r#"{"kind":"drill","name":"board","tools":[{"number":1,"feed_mm_min":1000,"spindle_rpm":24000},{"number":2,"extra_stroke_mm":0.5,"feed_mm_min":800,"spindle_rpm":24000}],
        "holes":[{"tool":1,"x_mm":10,"y_mm":10},{"tool":2,"x_mm":20,"y_mm":10},{"tool":1,"x_mm":30,"y_mm":20}]}"#;

    #[test]
    fn a_drilling_job_from_holes_is_planned_and_viewed_in_the_machines_order() {
        let request: JobRequest = serde_json::from_str(TWO_TOOLS).unwrap();
        let planned = plan(&machine(), &request).unwrap();
        let v = &planned.view;
        assert_eq!((v.kind.as_str(), v.name.as_str()), ("drill", "board"));
        // tool 1 first (two holes), then tool 2
        assert_eq!(
            v.holes.iter().map(|h| h.tool).collect::<Vec<_>>(),
            vec![1, 1, 2]
        );
        assert_eq!(
            v.tools
                .iter()
                .map(|t| (t.number, t.count))
                .collect::<Vec<_>>(),
            vec![(1, 2), (2, 1)]
        );
        assert_eq!(v.bounds_mm, Some([10.0, 10.0, 30.0, 20.0]));
        assert!(v.items > 10 && v.estimate_s > 1.0);
        assert_eq!(v.hole_ends.len(), 3, "every hole ends at a known item");
        assert!(
            v.hole_ends.windows(2).all(|w| w[0] < w[1]) && *v.hole_ends.last().unwrap() < v.items
        );
        // the planner's own result and the preview agree
        assert_eq!(planned.items.len(), v.items);
    }

    #[test]
    fn the_nodes_spindle_in_the_machines_tuning_makes_a_job_that_asks_the_node() {
        let mut m = machine();
        m.cfg = "spindle = node\n".into();
        // only a real machine takes the tuned values: the simulator runs the plain job
        m.connection = Connection::Node {
            address: "10.0.0.9:5800".into(),
            token: "t".into(),
        };
        let planned = plan(&m, &serde_json::from_str(TWO_TOOLS).unwrap()).unwrap();
        let rpms: Vec<u32> = planned
            .items
            .iter()
            .filter_map(|i| {
                if let Item::Spindle { rpm, .. } = i {
                    Some(*rpm)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(rpms, vec![24000, 0, 24000, 0]);
    }

    #[test]
    fn a_drilling_job_from_files_uses_the_tool_table_and_the_sorting() {
        let ncd = "M48\nINCH\nT1C0.8\n%\nT1\nX1000Y1000\nX2000Y1000\nX1000Y2000\n";
        let dtd = format!("{}1 0.0 1000 24000\n", "# comment\n".repeat(19));
        let request =
            serde_json::json!({"kind": "drill", "files": {"ncd": ncd, "dtd": dtd}, "sorting": "x"})
                .to_string();
        let planned = plan(&machine(), &serde_json::from_str(&request).unwrap()).unwrap();
        assert_eq!(planned.view.holes.len(), 3);
        // 1000 mils = 25.4 mm
        assert!(planned
            .view
            .holes
            .iter()
            .any(|h| (h.x_mm - 25.4).abs() < 1e-9 && (h.y_mm - 50.8).abs() < 1e-9));
        let bad = serde_json::json!({"kind": "drill", "files": {"ncd": ncd, "dtd": dtd}, "sorting": "diagonal"}).to_string();
        assert_eq!(
            plan(&machine(), &serde_json::from_str(&bad).unwrap())
                .err()
                .unwrap()
                .code,
            "bad_sorting"
        );
    }

    #[test]
    fn mistakes_are_refused_with_a_code_before_anything_runs() {
        let m = machine();
        let code = |json: &str| {
            plan(&m, &serde_json::from_str(json).unwrap())
                .err()
                .map(|e| e.code)
        };
        assert_eq!(code(r#"{"kind":"drill"}"#), Some("no_work"));
        assert_eq!(code(r#"{"kind":"drill","holes":[]}"#), Some("no_work"));
        assert_eq!(
            code(r#"{"kind":"drill","holes":[{"tool":1,"x_mm":1,"y_mm":1}]}"#),
            Some("unknown_tool")
        );
        assert_eq!(
            code(
                r#"{"kind":"drill","tools":[{"number":1,"feed_mm_min":0,"spindle_rpm":1}],"holes":[{"tool":1,"x_mm":1,"y_mm":1}]}"#
            ),
            Some("bad_tool")
        );
        assert_eq!(
            code(
                r#"{"kind":"drill","tools":[{"number":1,"feed_mm_min":900,"spindle_rpm":1}],"holes":[{"tool":1,"x_mm":-1,"y_mm":1}]}"#
            ),
            Some("out_of_table")
        );
        assert_eq!(
            code(
                r#"{"kind":"drill","board_mm":0,"tools":[{"number":1,"feed_mm_min":900,"spindle_rpm":1}],"holes":[{"tool":1,"x_mm":1,"y_mm":1}]}"#
            ),
            Some("bad_board")
        );
        assert_eq!(
            code(r#"{"kind":"drill","holes":[{"tool":1,"x_mm":1,"y_mm":1}],"files":{"ncd":"x"}}"#),
            Some("both_sources")
        );
        assert_eq!(
            code(r#"{"kind":"drill","files":{"ncd":"garbage that is not a drill file"}}"#),
            Some("bad_ncd")
        );
        assert_eq!(code(r#"{"kind":"route"}"#), Some("no_work"));
        assert_eq!(
            code(r#"{"kind":"route","paths":[{"pen":1,"points_mm":[[1,1],[2,2]]}]}"#),
            Some("unknown_tool")
        );
    }

    #[test]
    fn a_hole_outside_the_course_in_the_machines_configuration_is_refused() {
        // the sample configuration file of the programs: course 555 x 405 mm
        let dcf = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../EH3D-DRILLPRO/testdata/DRILLPRO.DCF"
        ));
        let Ok(dcf) = dcf else { return };
        let mut m = machine();
        m.dcf = dcf;
        if settings_of(&m.dcf).is_none() {
            return;
        }
        let ok = r#"{"kind":"drill","tools":[{"number":1,"feed_mm_min":900,"spindle_rpm":24000}],"holes":[{"tool":1,"x_mm":10,"y_mm":10}]}"#;
        assert!(plan(&m, &serde_json::from_str(ok).unwrap()).is_ok());
        let far = ok.replace("\"x_mm\":10", "\"x_mm\":600");
        assert_eq!(
            plan(&m, &serde_json::from_str(&far).unwrap())
                .err()
                .unwrap()
                .code,
            "out_of_table"
        );
    }

    const ONE_HOLE: &str = r#"{"kind":"drill","tools":[{"number":1,"feed_mm_min":900,"spindle_rpm":24000}],"holes":[{"tool":1,"x_mm":10,"y_mm":10},{"tool":1,"x_mm":540,"y_mm":390}]}"#;

    /// The sample configuration file of the programs, when the sibling repository is next to this one.
    fn sample_dcf() -> Option<String> {
        let dcf = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../EH3D-DRILLPRO/testdata/DRILLPRO.DCF"
        ))
        .ok()?;
        settings_of(&dcf).map(|_| dcf)
    }

    #[test]
    fn a_job_has_a_dry_run_of_its_exact_plan_that_stays_inside_the_course() {
        let planned = plan(&machine(), &serde_json::from_str(TWO_TOOLS).unwrap()).unwrap();
        let d = planned.view.dry_run.as_ref().expect("the dry run");
        assert!(d.within_course && d.steps > 100);
        assert!(d.max_mm[0] >= 29.9 && d.max_mm[1] >= 19.9, "{:?}", d.max_mm);
        assert!(d.min_mm[0] >= -0.5 && d.min_mm[1] >= -0.5, "{:?}", d.min_mm);
        assert!(d.seconds > 1.0 && (d.seconds - planned.view.estimate_s).abs() < 1.0);
        assert!(planned.view.warnings.is_empty());
        // a routing job has one too
        let json = r#"{"kind":"route","tools":[{"number":1,"depth_mm":1.7,"iterations":2,"feed_xy_mm_min":800,"feed_z_mm_min":300,"spindle_rpm":24000}],
            "paths":[{"pen":1,"points_mm":[[10,10],[40,10],[40,30],[10,30],[10,10]]}]}"#;
        let routed = plan(&machine(), &serde_json::from_str(json).unwrap()).unwrap();
        assert!(routed.view.dry_run.as_ref().unwrap().within_course);
    }

    #[test]
    fn a_work_at_the_far_corner_of_the_table_stays_inside_the_course() {
        let Some(dcf) = sample_dcf() else { return };
        let mut m = machine();
        m.dcf = dcf;
        let planned = plan(&m, &serde_json::from_str(ONE_HOLE).unwrap()).unwrap();
        let d = planned.view.dry_run.unwrap();
        assert!(d.within_course, "{d:?}");
        assert!(d.max_mm[0] > 540.0 && d.max_mm[1] > 390.0, "{:?}", d.max_mm);
        assert!(d.max_mm[2] > 1.0, "the drill goes down: {:?}", d.max_mm);
    }

    #[test]
    fn a_plan_that_goes_past_the_course_is_refused_unless_the_tuning_says_warn() {
        let Some(dcf) = sample_dcf() else { return };
        let mut m = machine();
        // a course of Z shorter than the drilling needs
        let short = dcf.replace(
            "37.30         * Max. Z depth",
            "5.00          * Max. Z depth",
        );
        assert_ne!(short, dcf, "the line of the sample file is as expected");
        m.dcf = short;
        let err = plan(&m, &serde_json::from_str(ONE_HOLE).unwrap())
            .err()
            .unwrap();
        assert_eq!(err.code, "outside_course");
        assert!(err.detail.contains("axis Z"), "{}", err.detail);
        m.cfg = "dry_run = warn
"
        .into();
        let planned = plan(&m, &serde_json::from_str(ONE_HOLE).unwrap()).unwrap();
        assert!(!planned.view.dry_run.as_ref().unwrap().within_course);
        let w = planned
            .view
            .warnings
            .iter()
            .find(|w| w.code == "out_of_course");
        assert_eq!(w.map(|w| w.axis.as_str()), Some("Z"));
    }

    #[test]
    fn a_work_that_looks_wrong_is_listed_in_the_view() {
        let Some(dcf) = sample_dcf() else { return };
        let mut m = machine();
        m.dcf = dcf;
        // two holes 2 mm apart: the file is probably in another unit
        let tiny = r#"{"kind":"drill","tools":[{"number":1,"feed_mm_min":900,"spindle_rpm":24000}],"holes":[{"tool":1,"x_mm":10,"y_mm":10},{"tool":1,"x_mm":11,"y_mm":12}]}"#;
        let planned = plan(&m, &serde_json::from_str(tiny).unwrap()).unwrap();
        let codes: Vec<&str> = planned
            .view
            .warnings
            .iter()
            .map(|w| w.code.as_str())
            .collect();
        assert_eq!(codes, vec!["tiny"]);
        assert_eq!(planned.view.warnings[0].values, vec![1.0, 2.0]);
        // the thresholds are the machine's tuning
        m.cfg = "sanity_tiny_mm = 0
"
        .into();
        let planned = plan(&m, &serde_json::from_str(tiny).unwrap()).unwrap();
        assert!(planned.view.warnings.is_empty());
        // far from the corner
        m.cfg = "sanity_far_mm = 100
"
        .into();
        let far = r#"{"kind":"drill","tools":[{"number":1,"feed_mm_min":900,"spindle_rpm":24000}],"holes":[{"tool":1,"x_mm":200,"y_mm":10},{"tool":1,"x_mm":300,"y_mm":50}]}"#;
        let planned = plan(&m, &serde_json::from_str(far).unwrap()).unwrap();
        assert_eq!(
            planned
                .view
                .warnings
                .iter()
                .map(|w| w.code.as_str())
                .collect::<Vec<_>>(),
            vec!["far"]
        );
    }

    #[test]
    fn a_routing_job_from_paths_is_planned_and_viewed() {
        let json = r#"{"kind":"route","name":"outline","tools":[{"number":1,"depth_mm":1.7,"iterations":2,"feed_xy_mm_min":800,"feed_z_mm_min":300,"spindle_rpm":24000}],
            "paths":[{"pen":1,"points_mm":[[10,10],[40,10],[40,30],[10,30],[10,10]]}]}"#;
        let planned = plan(&machine(), &serde_json::from_str(json).unwrap()).unwrap();
        let v = &planned.view;
        assert_eq!(
            (v.kind.as_str(), v.paths.len(), v.holes.len()),
            ("route", 1, 0)
        );
        assert!((v.travel_mm - 100.0).abs() < 1.0);
        assert_eq!(v.bounds_mm, Some([10.0, 10.0, 40.0, 30.0]));
        assert_eq!(v.tools[0].depth_mm, Some(1.7));
        assert!(v.estimate_s > 1.0);
    }

    #[test]
    fn a_routing_job_tells_the_item_that_ends_each_path() {
        let json = r#"{"kind":"route","name":"two","tools":[{"number":1,"depth_mm":1.7,"iterations":2,"feed_xy_mm_min":800,"feed_z_mm_min":300,"spindle_rpm":24000}],
            "paths":[{"pen":1,"points_mm":[[10,10],[40,10],[40,30]]},{"pen":1,"points_mm":[[50,10],[60,10]]}]}"#;
        let planned = plan(&machine(), &serde_json::from_str(json).unwrap()).unwrap();
        let ends = &planned.view.hole_ends;
        assert_eq!(ends.len(), 2, "one end for each path");
        assert!(ends[0] < ends[1] && ends[1] < planned.view.items);
        assert!(ends[0] > 0, "the first path ends after it begins");
    }

    #[test]
    fn a_routing_job_from_an_hpgl_file_uses_the_plot_and_the_tool_table() {
        let plt = "IN;SP1;PU400,400;PD1600,400,1600,1200,400,1200,400,400;PU;";
        let rtd = format!("{}1 1.7 2 800 300 24000\n", "# comment\n".repeat(19));
        let request =
            serde_json::json!({"kind": "route", "files": {"plt": plt, "rtd": rtd}}).to_string();
        let planned = plan(&machine(), &serde_json::from_str(&request).unwrap()).unwrap();
        // 400 plotter units are 10 mm
        assert_eq!(planned.view.bounds_mm, Some([10.0, 10.0, 40.0, 30.0]));
    }

    #[test]
    fn the_tools_switched_off_leave_their_holes_out_and_the_view_says_so() {
        let mut v: serde_json::Value = serde_json::from_str(TWO_TOOLS).unwrap();
        v["skip_tools"] = serde_json::json!([2]);
        let planned = plan(&machine(), &serde_json::from_value(v).unwrap()).unwrap();
        let view = &planned.view;
        assert_eq!(
            view.holes.iter().map(|h| h.tool).collect::<Vec<_>>(),
            vec![1, 1]
        );
        assert_eq!(
            view.tools.iter().map(|t| t.number).collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!((view.skipped_tools.clone(), view.skipped), (vec![2], 1));
        // the machine is never asked for the tool that is off: one change of tool less
        let all = plan(&machine(), &serde_json::from_str(TWO_TOOLS).unwrap()).unwrap();
        assert!(planned.items.len() < all.items.len());
        assert_eq!(view.hole_ends.len(), 2);
    }

    #[test]
    fn a_tool_that_is_off_does_not_have_to_be_in_the_tool_table() {
        let json = r#"{"kind":"drill","skip_tools":[7],"tools":[{"number":1,"feed_mm_min":1000,"spindle_rpm":24000}],
            "holes":[{"tool":1,"x_mm":10,"y_mm":10},{"tool":7,"x_mm":20,"y_mm":10}]}"#;
        let planned = plan(&machine(), &serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(planned.view.holes.len(), 1);
        let without = json.replace(r#""skip_tools":[7],"#, "");
        assert_eq!(
            plan(&machine(), &serde_json::from_str(&without).unwrap())
                .err()
                .map(|e| e.code),
            Some("unknown_tool")
        );
    }

    #[test]
    fn a_job_that_goes_on_after_a_stop_leaves_out_the_holes_already_done_in_the_machines_order() {
        let mut v: serde_json::Value = serde_json::from_str(TWO_TOOLS).unwrap();
        // the machine drills tool 1 (two holes, at x 10 and 30) and then tool 2 (x 20): two done leaves the hole of tool 2
        v["skip"] = serde_json::json!(2);
        let planned = plan(&machine(), &serde_json::from_value(v.clone()).unwrap()).unwrap();
        assert_eq!(
            planned
                .view
                .holes
                .iter()
                .map(|h| (h.tool, h.x_mm))
                .collect::<Vec<_>>(),
            vec![(2, 20.0)]
        );
        assert_eq!(planned.view.skipped, 2);
        // all of it done is refused with a code instead of running nothing
        v["skip"] = serde_json::json!(3);
        assert_eq!(
            plan(&machine(), &serde_json::from_value(v).unwrap())
                .err()
                .map(|e| e.code),
            Some("no_work")
        );
    }

    #[test]
    fn every_hole_off_is_refused_with_a_code() {
        let mut v: serde_json::Value = serde_json::from_str(TWO_TOOLS).unwrap();
        v["skip_tools"] = serde_json::json!([1, 2]);
        assert_eq!(
            plan(&machine(), &serde_json::from_value(v).unwrap())
                .err()
                .map(|e| e.code),
            Some("no_work")
        );
    }

    #[test]
    fn the_pens_switched_off_leave_their_paths_out_of_a_routing_job() {
        let json = r#"{"kind":"route","skip_tools":[2],
            "tools":[{"number":1,"depth_mm":1.7,"iterations":1,"feed_xy_mm_min":800,"feed_z_mm_min":300,"spindle_rpm":24000},
                     {"number":2,"depth_mm":0.3,"iterations":1,"feed_xy_mm_min":800,"feed_z_mm_min":300,"spindle_rpm":24000}],
            "paths":[{"pen":1,"points_mm":[[10,10],[40,10]]},{"pen":2,"points_mm":[[10,20],[40,20]]},{"pen":1,"points_mm":[[10,30],[40,30]]}]}"#;
        let planned = plan(&machine(), &serde_json::from_str(json).unwrap()).unwrap();
        let v = &planned.view;
        assert_eq!(
            v.paths.iter().map(|p| p.pen).collect::<Vec<_>>(),
            vec![1, 1]
        );
        assert_eq!(
            v.tools.iter().map(|t| t.number).collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!((v.skipped_tools.clone(), v.skipped), (vec![2], 1));
        let resumed = json.replace(r#""skip_tools":[2],"#, r#""skip":1,"#);
        let planned = plan(&machine(), &serde_json::from_str(&resumed).unwrap()).unwrap();
        assert_eq!(planned.view.paths.len(), 2);
        assert_eq!(planned.view.skipped, 1);
    }

    // ---- the chaining of the tool changer (request_version 2, `rack`) ----------------------------------------------------------------

    use eh3d_drillpro::protocol::job::Pause;

    /// A machine whose file asks for the automatic tool change (the sample file of the second machine), when the sibling repository is next to this one.
    fn changer_machine() -> Option<MachineRecord> {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../EH3D-DRILLPRO/testdata/DRILLPRO_CNCA.DCF"
        ))
        .ok()?;
        let dcf = String::from_utf8_lossy(&bytes).into_owned();
        settings_of(&dcf).filter(|s| s.auto_change)?;
        let mut m = machine();
        m.dcf = dcf;
        // the course is not what these tests are about
        m.cfg = "dry_run = warn\n".into();
        Some(m)
    }

    const ONE_PEN: &str = r#"{"kind":"route","name":"outline","tools":[{"number":1,"depth_mm":1.7,"iterations":1,"feed_xy_mm_min":800,"feed_z_mm_min":300,"spindle_rpm":24000}],
        "paths":[{"pen":1,"points_mm":[[10,10],[40,10],[40,30]]}]}"#;

    /// The request `base` with the fields of `extra` set over it.
    fn asked(base: &str, extra: serde_json::Value) -> JobRequest {
        let mut v: serde_json::Value = serde_json::from_str(base).unwrap();
        for (key, value) in extra.as_object().unwrap() {
            v[key] = value.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    fn spindle_asks(items: &[Item]) -> usize {
        items
            .iter()
            .filter(|i| matches!(i, Item::Pause(Pause::SpindleStart)))
            .count()
    }

    fn refused(m: &MachineRecord, request: &JobRequest) -> (&'static str, String) {
        let e = plan(m, request).err().expect("the job is refused");
        (e.code, e.detail)
    }

    #[test]
    fn a_request_without_a_rack_or_of_the_first_version_keeps_its_plan() {
        let Some(m) = changer_machine() else { return };
        let plain = plan(&m, &serde_json::from_str(TWO_TOOLS).unwrap()).unwrap();
        assert_eq!(plain.view.rack_applied, None);
        assert!(plain.view.warnings.is_empty());
        // the version alone changes nothing
        let versioned = plan(
            &m,
            &asked(TWO_TOOLS, serde_json::json!({"request_version": 2})),
        )
        .unwrap();
        assert_eq!(versioned.view, plain.view);
        assert_eq!(versioned.items.len(), plain.items.len());
        // a rack in a request of the first version (or with no version) is left out, and the plan is the old one
        for version in [
            serde_json::json!({}),
            serde_json::json!({"request_version": 1}),
        ] {
            let mut extra = version.as_object().unwrap().clone();
            extra.insert(
                "rack".into(),
                serde_json::json!({"first": 2, "carry_in": true}),
            );
            let old = plan(&m, &asked(TWO_TOOLS, extra.into())).unwrap();
            assert_eq!(old.view.rack_applied, Some(false));
            assert_eq!(
                old.view
                    .warnings
                    .iter()
                    .map(|w| w.code.as_str())
                    .collect::<Vec<_>>(),
                vec!["rack_needs_v2"]
            );
            assert_eq!(old.items.len(), plain.items.len());
            assert_eq!(old.view.hole_ends, plain.view.hole_ends);
            assert!((old.view.estimate_s - plain.view.estimate_s).abs() < 1e-9);
        }
        assert_eq!(
            refused(
                &m,
                &asked(TWO_TOOLS, serde_json::json!({"request_version": 0}))
            )
            .0,
            "bad_request"
        );
    }

    #[test]
    fn a_drilling_job_followed_by_a_routing_job_hands_the_chuck_on() {
        let Some(m) = changer_machine() else { return };
        // alone, on the rack: the spindle is asked for and the tool goes back
        let alone = asked(
            TWO_TOOLS,
            serde_json::json!({"request_version": 2, "rack": {}}),
        );
        let alone = plan(&m, &alone).unwrap();
        assert_eq!(alone.view.rack_applied, Some(true));
        assert!(alone.view.warnings.is_empty());
        assert_eq!(spindle_asks(&alone.items), 1);
        // the drilling leaves its last tool in the chuck: no change back to the test pin
        let carried_out = asked(
            TWO_TOOLS,
            serde_json::json!({"request_version": 2, "rack": {"first": 0, "carry_out": true}}),
        );
        let carried_out = plan(&m, &carried_out).unwrap();
        assert_eq!(carried_out.view.rack_applied, Some(true));
        assert!(carried_out.items.len() < alone.items.len());
        // the routing after it takes the chuck with the last drill (the second place) and asks for the spindle no more
        let unchained = asked(
            ONE_PEN,
            serde_json::json!({"request_version": 2, "rack": {"first": 2}}),
        );
        let unchained = plan(&m, &unchained).unwrap();
        assert_eq!(unchained.view.rack_applied, Some(true));
        assert_eq!(spindle_asks(&unchained.items), 1);
        let chained = asked(
            ONE_PEN,
            serde_json::json!({"request_version": 2, "rack": {"first": 2, "carry_in": true}}),
        );
        let chained = plan(&m, &chained).unwrap();
        assert_eq!(chained.view.rack_applied, Some(true));
        assert!(chained.view.warnings.is_empty());
        assert_eq!(
            spindle_asks(&chained.items),
            0,
            "one change, no second ask for the spindle"
        );
        assert_eq!(chained.view.hole_ends.len(), 1);
    }

    #[test]
    fn the_places_given_to_the_tools_decide_where_the_chuck_goes() {
        let Some(m) = changer_machine() else { return };
        let ordered = plan(
            &m,
            &asked(
                TWO_TOOLS,
                serde_json::json!({"request_version": 2, "rack": {}}),
            ),
        )
        .unwrap();
        // the tools are not in the bar in the order the job uses them
        let scattered = asked(
            TWO_TOOLS,
            serde_json::json!({"request_version": 2, "rack": {"slots": [{"tool": 1, "place": 6}, {"tool": 2, "place": 3}]}}),
        );
        let scattered = plan(&m, &scattered).unwrap();
        assert_eq!(scattered.view.rack_applied, Some(true));
        assert!(scattered.view.warnings.is_empty());
        assert!(
            (scattered.view.estimate_s - ordered.view.estimate_s).abs() > 1e-9,
            "the chuck travels to other places"
        );
        assert_eq!(scattered.view.hole_ends.len(), 3);
    }

    #[test]
    fn a_rack_that_does_not_fit_is_refused_with_a_stable_message() {
        let m = machine();
        let with_rack = |rack: serde_json::Value| {
            asked(
                TWO_TOOLS,
                serde_json::json!({"request_version": 2, "rack": rack}),
            )
        };
        let bad = |rack: serde_json::Value| {
            let (code, detail) = refused(&m, &with_rack(rack));
            assert_eq!(code, "bad_rack");
            detail
        };
        assert_eq!(
            bad(serde_json::json!({"slots": [{"tool": 1, "place": 3}, {"tool": 2, "place": 3}]})),
            "rack.slots: place 3 is given to two tools"
        );
        assert_eq!(
            bad(serde_json::json!({"slots": [{"tool": 1, "place": 3}, {"tool": 1, "place": 4}]})),
            "rack.slots: tool 1 is given two places"
        );
        assert_eq!(
            bad(serde_json::json!({"slots": [{"tool": 1, "place": 0}]})),
            "rack.slots: place 0 is the test pin, not a place for tool 1"
        );
        assert_eq!(
            bad(serde_json::json!({"carry_slot": 4})),
            "rack.carry_slot needs rack.carry_in"
        );
        assert_eq!(
            bad(
                serde_json::json!({"carry_in": true, "first": 2, "carry_slot": 3, "slots": [{"tool": 1, "place": 3}]})
            ),
            "rack.carry_slot: place 3 is also the place of a tool of this job"
        );
        assert_eq!(
            bad(serde_json::json!({"first": 15})),
            "rack.first may be up to 14 places"
        );
        // a field of the wrong type is a request that does not read
        let wrong =
            serde_json::json!({"kind": "drill", "request_version": 2, "rack": {"first": "two"}});
        assert!(serde_json::from_value::<JobRequest>(wrong).is_err());
    }

    #[test]
    fn a_place_the_machine_file_does_not_have_is_refused() {
        let Some(m) = changer_machine() else { return };
        let (code, detail) = refused(
            &m,
            &asked(
                TWO_TOOLS,
                serde_json::json!({"request_version": 2, "rack": {"slots": [{"tool": 1, "place": 14}, {"tool": 2, "place": 19}]}}),
            ),
        );
        assert_eq!(code, "bad_rack");
        assert_eq!(
            detail,
            "place 19 of the rack is not defined in the configuration file of the machine"
        );
    }

    #[test]
    fn a_field_of_a_later_version_is_left_out_with_a_warning() {
        let Some(m) = changer_machine() else { return };
        let later = asked(
            TWO_TOOLS,
            serde_json::json!({"request_version": 3, "rack": {"first": 0, "carry_out": true, "spare_place": 9}}),
        );
        let planned = plan(&m, &later).unwrap();
        assert_eq!(planned.view.rack_applied, Some(true));
        let codes: Vec<(&str, &str)> = planned
            .view
            .warnings
            .iter()
            .map(|w| (w.code.as_str(), w.detail.as_str()))
            .collect();
        assert_eq!(
            codes,
            vec![
                ("request_version_newer", ""),
                ("rack_unknown_field", "spare_place")
            ]
        );
        assert_eq!(planned.view.warnings[0].values, vec![3.0]);
    }

    #[test]
    fn a_machine_that_changes_its_tools_by_hand_plans_the_old_job_and_says_so() {
        let plain = plan(&machine(), &serde_json::from_str(TWO_TOOLS).unwrap()).unwrap();
        let asked_for = asked(
            TWO_TOOLS,
            serde_json::json!({"request_version": 2, "rack": {"first": 2, "carry_in": true}}),
        );
        let planned = plan(&machine(), &asked_for).unwrap();
        assert_eq!(planned.view.rack_applied, Some(false));
        assert_eq!(
            planned
                .view
                .warnings
                .iter()
                .map(|w| w.code.as_str())
                .collect::<Vec<_>>(),
            vec!["rack_no_changer"]
        );
        assert_eq!(planned.items.len(), plain.items.len());
        assert_eq!(planned.view.hole_ends, plain.view.hole_ends);
    }
}
