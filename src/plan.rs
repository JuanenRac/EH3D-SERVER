//! From what a client sends to the items the machine runs: a drilling job (holes and tools, or an Excellon file and a tool table) or a
//! routing job (paths and tools, or an HP-GL file and a tool table) is planned here with the libraries of EH3D-DRILLPRO and EH3D-ROUTEPRO,
//! using the constants and the tuning of the machine it is for. The same planning is what a preview shows, so what a client draws is what the
//! machine will do.

use eh3d_drillpro::config::{self, MachineSettings};
use eh3d_drillpro::job::{geometry_from, hole_end_indexes, job_machine, sort_holes, Sorting};
use eh3d_drillpro::ncd::{self, MIL_MM};
use eh3d_drillpro::protocol::job::{plan_drill_job, Item, JobHole, JobTool};
use eh3d_drillpro::tools as drill_tools;
use eh3d_routepro::hpgl::{self, Polyline, UNIT_MM};
use eh3d_routepro::job::plan_route_job;
use eh3d_routepro::tools as route_tools;
use serde::{Deserialize, Serialize};

use crate::machines::MachineRecord;

/// The most holes or points one job may carry.
pub const MAX_POINTS: usize = 200_000;

#[derive(Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum JobRequest {
    Drill(DrillRequest),
    Route(RouteRequest),
}

impl JobRequest {
    pub fn name(&self) -> Option<&str> {
        match self {
            JobRequest::Drill(d) => d.name.as_deref(),
            JobRequest::Route(r) => r.name.as_deref(),
        }
    }
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
}

#[derive(Deserialize, Debug)]
pub struct DrillToolIn {
    pub number: u8,
    #[serde(default)]
    pub extra_stroke_mm: f64,
    pub feed_mm_min: u32,
    pub spindle_rpm: u32,
}

#[derive(Deserialize, Debug)]
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

pub fn plan(machine: &MachineRecord, request: &JobRequest) -> Result<Planned, PlanError> {
    match request {
        JobRequest::Drill(d) => plan_drill(machine, d),
        JobRequest::Route(r) => plan_route(machine, r),
    }
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
    if holes.is_empty() {
        return fail("no_work", "the job has no holes");
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
        job_tools.push(JobTool {
            number,
            extra_stroke_mm: row.extra_stroke_mm,
            feed_mm_min: row.feed_mm_min,
            spindle_rpm: row.spindle_rpm,
        });
    }
    let job_holes: Vec<JobHole> = holes
        .iter()
        .map(|h| JobHole {
            tool: h.tool,
            x_mm: h.x_mm,
            y_mm: h.y_mm,
        })
        .collect();

    let jm = job_machine(settings.as_ref(), geometry, &connection, board_mm);
    let items = plan_drill_job(&jm, &job_tools, &job_holes);

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
        hole_ends: hole_end_indexes(&items),
        estimate_s: estimate_seconds(&items),
        travel_mm: travel,
    };
    Ok(Planned { items, view })
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
    if paths.is_empty() {
        return fail("no_work", "the job has no paths");
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
    let board = settings.as_ref().map_or(1.6, |m| m.board_mm);
    let jm = job_machine(settings.as_ref(), geometry, &connection, board);
    let items = plan_route_job(&paths, &tool_rows, &jm);

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
        hole_ends: Vec::new(),
        estimate_s: estimate_seconds(&items),
        travel_mm: length,
    };
    Ok(Planned { items, view })
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
    fn a_routing_job_from_an_hpgl_file_uses_the_plot_and_the_tool_table() {
        let plt = "IN;SP1;PU400,400;PD1600,400,1600,1200,400,1200,400,400;PU;";
        let rtd = format!("{}1 1.7 2 800 300 24000\n", "# comment\n".repeat(19));
        let request =
            serde_json::json!({"kind": "route", "files": {"plt": plt, "rtd": rtd}}).to_string();
        let planned = plan(&machine(), &serde_json::from_str(&request).unwrap()).unwrap();
        // 400 plotter units are 10 mm
        assert_eq!(planned.view.bounds_mm, Some([10.0, 10.0, 40.0, 30.0]));
    }
}
