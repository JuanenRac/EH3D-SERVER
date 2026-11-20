//! The CAM worker: jobs that make or check G-code and move no machine. The plugin `cam` (`src/plugin_cam.rs`) hands the workers of the server to this engine. A job here is
//! a computed result: the G-code to drill the holes of an Excellon file, to cut the outline of a board, to isolate the tracks of a copper layer, or the report of a program
//! that somebody else made (`verify`). The G-code and the report are in the `view` of the job.
//!
//! Everything a program may ask is a setting of the worker (the depth, the feed, the turns, the safe height, the travel), and a request that asks for more is refused
//! (`bad_params`), never cut down. The isolation works on a grid of the copper: a layer with copper pours or with pads made by a macro is refused, because the grid would
//! leave them out and the tool would cut into copper. Nothing here has run on a machine.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::cnc::{analyze, CncConfig};
use crate::gcode;
use crate::gerber::{files_of, read_files, Layer, Shape};
use crate::plugins::{PluginError, PluginResult, Prepared};
use crate::simjobs::{evenly, known_keys, num, number_in, round2, SimBook};

/// The settings a CAM worker may have: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("max_depth_mm", "the deepest cut a job may ask, 0.01 to 20 (3 by default)"),
    ("max_feed_mm_min", "the fastest feed a job may ask, 1 to 30000 (3000 by default)"),
    ("max_spindle_rpm", "the most turns a job may ask, 0 to 65535 (30000 by default)"),
    ("max_safe_z_mm", "the highest safe height a job may ask, 0.1 to 100 (50 by default)"),
    ("travel_x_mm", "the travel of X, 0 to 5000: the work has to fit 0 to this (not set: not checked)"),
    ("travel_y_mm", "the travel of Y, 0 to 5000 (not set: not checked)"),
    ("max_gcode_lines", "the most lines of G-code a job may make or check, 100 to 1000000 (200000 by default)"),
    ("max_grid_cells", "the most cells of the grid of an isolation, 10000 to 20000000 (4000000 by default)"),
    ("max_input_mib", "the most text a job may bring, 1 to 64 MiB (64 by default)"),
    ("item_ms", "the milliseconds one step takes in the simulator, 0 to 3600000 (0 by default: the job ends as soon as it runs)"),
];

/// What the worker has built in: kind of job and what makes it.
pub const TOOLS: &[(&str, &str)] = &[
    (
        "drill",
        "G-code to drill the holes of an Excellon file, one tool after another",
    ),
    (
        "outline",
        "G-code to cut the outline of a rectangular board",
    ),
    (
        "isolate",
        "G-code to isolate the tracks of a copper layer (a grid; no pours)",
    ),
    (
        "stencil",
        "G-code to cut the openings of a stencil from a paste layer",
    ),
    (
        "autolevel",
        "G-code that probes a grid over the board, or the account of a probe map",
    ),
    (
        "inspect",
        "the account of a toolpath: lengths, depths and moves in the air",
    ),
    (
        "verify",
        "the report of a G-code program: bounds, limits, order of the moves",
    ),
];

/// The settings of a CAM worker, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct CamConfig {
    pub max_depth: f64,
    pub max_feed: f64,
    pub max_rpm: f64,
    pub max_safe_z: f64,
    pub travel: [Option<f64>; 2],
    pub max_lines: usize,
    pub max_cells: usize,
    pub max_input_mib: f64,
    pub item_ms: f64,
}

impl Default for CamConfig {
    fn default() -> Self {
        CamConfig {
            max_depth: 3.0,
            max_feed: 3000.0,
            max_rpm: 30000.0,
            max_safe_z: 50.0,
            travel: [None; 2],
            max_lines: 200_000,
            max_cells: 4_000_000,
            max_input_mib: 64.0,
            item_ms: 0.0,
        }
    }
}

impl CamConfig {
    pub fn from_settings(settings: &Value) -> Result<CamConfig, String> {
        let keys: Vec<&str> = SETTINGS_HELP.iter().map(|(k, _)| *k).collect();
        known_keys(settings, &keys, "a CAM worker")?;
        let mut c = CamConfig::default();
        if let Some(n) = number_in(settings, "max_depth_mm", 0.01, 20.0)? {
            c.max_depth = n;
        }
        if let Some(n) = number_in(settings, "max_feed_mm_min", 1.0, 30000.0)? {
            c.max_feed = n;
        }
        if let Some(n) = number_in(settings, "max_spindle_rpm", 0.0, 65535.0)? {
            c.max_rpm = n;
        }
        if let Some(n) = number_in(settings, "max_safe_z_mm", 0.1, 100.0)? {
            c.max_safe_z = n;
        }
        c.travel[0] = number_in(settings, "travel_x_mm", 0.0, 5000.0)?;
        c.travel[1] = number_in(settings, "travel_y_mm", 0.0, 5000.0)?;
        if let Some(n) = number_in(settings, "max_gcode_lines", 100.0, 1_000_000.0)? {
            c.max_lines = n as usize;
        }
        if let Some(n) = number_in(settings, "max_grid_cells", 10_000.0, 20_000_000.0)? {
            c.max_cells = n as usize;
        }
        if let Some(n) = number_in(settings, "max_input_mib", 1.0, 64.0)? {
            c.max_input_mib = n;
        }
        if let Some(n) = number_in(settings, "item_ms", 0.0, 3_600_000.0)? {
            c.item_ms = n;
        }
        Ok(c)
    }
}

fn bad(why: impl Into<String>) -> PluginError {
    PluginError::Coded("bad_params", why.into())
}

/// A number of `params` inside the limits; the default when it is not there.
fn param(params: &Value, key: &str, default: f64, low: f64, high: f64) -> PluginResult<f64> {
    match params.get(key) {
        None => Ok(default),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && n >= low && n <= high => Ok(n),
            _ => Err(bad(format!(
                "{key} is a number from {} to {}",
                num(low),
                num(high)
            ))),
        },
    }
}

/// The settings of the cutting that every kind of job takes, checked against the limits of the worker.
struct Cut {
    depth: f64,
    feed: f64,
    rpm: f64,
    safe_z: f64,
}

fn cut_params(params: &Value, cfg: &CamConfig, depth: f64) -> PluginResult<Cut> {
    Ok(Cut {
        depth: param(params, "depth_mm", depth, 0.01, cfg.max_depth)?,
        feed: param(
            params,
            "feed_mm_min",
            100.0_f64.min(cfg.max_feed),
            1.0,
            cfg.max_feed,
        )?,
        rpm: param(
            params,
            "spindle_rpm",
            12000.0_f64.min(cfg.max_rpm),
            0.0,
            cfg.max_rpm,
        )?,
        safe_z: param(
            params,
            "safe_z_mm",
            2.0_f64.min(cfg.max_safe_z),
            0.1,
            cfg.max_safe_z,
        )?,
    })
}

fn check_travel(cfg: &CamConfig, points: &[(f64, f64)]) -> PluginResult<()> {
    for &(x, y) in points {
        for (v, t) in [(x, cfg.travel[0]), (y, cfg.travel[1])] {
            if let Some(t) = t {
                if v < -1e-6 || v > t + 1e-6 {
                    return Err(PluginError::Coded(
                        "out_of_travel",
                        format!(
                            "the work reaches {} mm, outside the travel 0 to {} mm",
                            num(v),
                            num(t)
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The finished program and what a client reads of it.
struct Made {
    lines: Vec<String>,
    steps: usize,
    note: Value,
}

fn header(c: &Cut, what: &str) -> Vec<String> {
    vec![
        format!("({what})"),
        "G21".into(),
        "G90".into(),
        "G17".into(),
        format!("G0 Z{}", num(c.safe_z)),
        format!("M3 S{}", num(c.rpm)),
    ]
}

fn footer(c: &Cut) -> Vec<String> {
    vec![format!("G0 Z{}", num(c.safe_z)), "M5".into(), "M2".into()]
}

// ---- drill ---------------------------------------------------------------------------------------------------------------------------

fn make_drill(layers: &[Layer], params: &Value, cfg: &CamConfig) -> PluginResult<Made> {
    let cut = cut_params(params, cfg, 1.8)?;
    let peck = param(params, "peck_mm", 0.0, 0.0, cfg.max_depth)?;
    let (dx, dy) = (
        param(params, "offset_x_mm", 0.0, -5000.0, 5000.0)?,
        param(params, "offset_y_mm", 0.0, -5000.0, 5000.0)?,
    );
    let pause = match params.get("tool_change").and_then(Value::as_str) {
        None | Some("pause") => true,
        Some("none") => false,
        Some(_) => return Err(bad("tool_change is `pause` or `none`")),
    };
    let mut holes: Vec<(f64, (f64, f64))> = layers
        .iter()
        .filter(|l| l.format == "excellon")
        .flat_map(|l| {
            l.holes
                .iter()
                .map(|h| (h.diameter, (h.at.0 + dx, h.at.1 + dy)))
        })
        .collect();
    if holes.is_empty() {
        return Err(PluginError::Coded(
            "missing_layer",
            "the job needs a drill file with holes in it".into(),
        ));
    }
    check_travel(cfg, &holes.iter().map(|h| h.1).collect::<Vec<_>>())?;
    holes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut lines = header(&cut, "drill");
    let (mut here, mut sizes, mut count) = ((0.0, 0.0), Vec::new(), 0usize);
    let mut i = 0;
    while i < holes.len() {
        let d = holes[i].0;
        let mut group: Vec<(f64, f64)> = Vec::new();
        while i < holes.len() && holes[i].0 == d {
            group.push(holes[i].1);
            i += 1;
        }
        sizes.push(json!({"diameter_mm": d, "holes": group.len()}));
        if pause {
            lines.push("M5".into());
            lines.push(format!("M0 (change to a {} mm drill)", num(d)));
            lines.push(format!("M3 S{}", num(cut.rpm)));
        } else {
            lines.push(format!("(drill {} mm)", num(d)));
        }
        while !group.is_empty() {
            let k = group
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    let da = (a.1 .0 - here.0).hypot(a.1 .1 - here.1);
                    let db = (b.1 .0 - here.0).hypot(b.1 .1 - here.1);
                    da.total_cmp(&db)
                })
                .map_or(0, |(k, _)| k);
            let at = group.swap_remove(k);
            here = at;
            lines.push(format!("G0 X{} Y{}", num(at.0), num(at.1)));
            if peck > 0.0 {
                let mut z = 0.0;
                while z < cut.depth - 1e-9 {
                    z = (z + peck).min(cut.depth);
                    lines.push(format!("G1 Z{} F{}", num(-z), num(cut.feed)));
                    lines.push(format!("G0 Z{}", num(cut.safe_z)));
                    if z < cut.depth - 1e-9 {
                        lines.push(format!("G0 Z{}", num(-(z - 0.2).max(0.0))));
                    }
                }
            } else {
                lines.push(format!("G1 Z{} F{}", num(-cut.depth), num(cut.feed)));
                lines.push(format!("G0 Z{}", num(cut.safe_z)));
            }
            count += 1;
        }
    }
    lines.extend(footer(&cut));
    Ok(Made {
        lines,
        steps: count,
        note: json!({"holes": count, "tools": sizes, "depth_mm": cut.depth, "tool_change": if pause { "pause" } else { "none" }}),
    })
}

// ---- outline -------------------------------------------------------------------------------------------------------------------------

/// The box of a board whose outline is a rectangle: every segment is along X or Y and the length of them all is the perimeter of the box.
fn rectangle_of(layers: &[Layer]) -> PluginResult<[f64; 4]> {
    let outline: Vec<&Layer> = layers.iter().filter(|l| l.function == "outline").collect();
    let layer = outline.first().ok_or_else(|| {
        PluginError::Coded(
            "missing_layer",
            "the job needs a layer that is the outline of the board".into(),
        )
    })?;
    if layer.tracks.is_empty() || !layer.flashes.is_empty() || layer.regions > 0 {
        return Err(bad("the outline layer has to be lines only"));
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let mut length = 0.0;
    for t in &layer.tracks {
        if (t.from.0 - t.to.0).abs() > 1e-6 && (t.from.1 - t.to.1).abs() > 1e-6 {
            return Err(bad(
                "the outline is not a rectangle: a line is neither along X nor along Y",
            ));
        }
        for p in [t.from, t.to] {
            x0 = x0.min(p.0);
            x1 = x1.max(p.0);
            y0 = y0.min(p.1);
            y1 = y1.max(p.1);
        }
        length += (t.from.0 - t.to.0).abs() + (t.from.1 - t.to.1).abs();
    }
    let on_edge = |v: f64, a: f64, b: f64| (v - a).abs() < 1e-6 || (v - b).abs() < 1e-6;
    for t in &layer.tracks {
        let horizontal = (t.from.1 - t.to.1).abs() <= 1e-6;
        let fits = if horizontal {
            on_edge(t.from.1, y0, y1)
        } else {
            on_edge(t.from.0, x0, x1)
        };
        if !fits {
            return Err(bad(
                "the outline is not a rectangle: a line is not on the edge of its box",
            ));
        }
    }
    let perimeter = 2.0 * ((x1 - x0) + (y1 - y0));
    if (x1 - x0) < 1e-6 || (y1 - y0) < 1e-6 || (length - perimeter).abs() > 0.01 {
        return Err(bad(
            "the outline is not a rectangle: its lines do not make the perimeter of their box",
        ));
    }
    Ok([x0, y0, x1, y1])
}

fn make_outline(layers: &[Layer], params: &Value, cfg: &CamConfig) -> PluginResult<Made> {
    let cut = cut_params(params, cfg, 1.8)?;
    let tool = param(params, "tool_dia_mm", 2.0, 0.1, 10.0)?;
    let pass = param(params, "pass_depth_mm", cut.depth.min(0.6), 0.01, cut.depth)?;
    let [x0, y0, x1, y1] = rectangle_of(layers)?;
    let r = tool / 2.0;
    let (ax, ay, bx, by) = (x0 - r, y0 - r, x1 + r, y1 + r);
    check_travel(cfg, &[(ax, ay), (bx, by)])?;
    let mut lines = header(&cut, "outline");
    let mut z = 0.0;
    let mut passes = 0usize;
    lines.push(format!("G0 X{} Y{}", num(ax), num(ay)));
    while z < cut.depth - 1e-9 {
        z = (z + pass).min(cut.depth);
        lines.push(format!("G1 Z{} F{}", num(-z), num(cut.feed)));
        lines.push(format!("G1 X{} Y{} F{}", num(bx), num(ay), num(cut.feed)));
        lines.push(format!("G1 X{} Y{}", num(bx), num(by)));
        lines.push(format!("G1 X{} Y{}", num(ax), num(by)));
        lines.push(format!("G1 X{} Y{}", num(ax), num(ay)));
        passes += 1;
    }
    lines.extend(footer(&cut));
    Ok(Made {
        lines,
        steps: passes,
        note: json!({
            "board_mm": {"width": round2(x1 - x0), "height": round2(y1 - y0)},
            "tool_dia_mm": tool, "passes": passes, "depth_mm": cut.depth,
            "cut_box_mm": {"min_x": round2(ax), "min_y": round2(ay), "max_x": round2(bx), "max_y": round2(by)},
        }),
    })
}

// ---- isolation ------------------------------------------------------------------------------------------------------------------------

struct Grid {
    x0: f64,
    y0: f64,
    res: f64,
    w: usize,
    h: usize,
}

impl Grid {
    fn at(&self, i: usize, j: usize) -> usize {
        j * self.w + i
    }

    fn center(&self, i: usize, j: usize) -> (f64, f64) {
        (
            self.x0 + (i as f64 + 0.5) * self.res,
            self.y0 + (j as f64 + 0.5) * self.res,
        )
    }
}

fn dist_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 < 1e-18 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// The copper of a layer on a grid. Fails on what the grid would leave out.
fn rasterize(
    layer: &Layer,
    tool: f64,
    reach: f64,
    res: f64,
    max_cells: usize,
) -> PluginResult<(Grid, Vec<bool>)> {
    if layer.regions > 0 {
        return Err(bad(
            "the layer has copper pours (regions): the isolation does not follow them yet",
        ));
    }
    if layer.flashes.iter().any(|f| f.shape == Shape::Unknown) {
        return Err(bad(
            "the layer has pads made by a macro: the isolation does not know their shape",
        ));
    }
    let Some([bx0, by0, bx1, by1]) = layer.bbox else {
        return Err(bad("the layer has no copper"));
    };
    let margin = reach + tool + 2.0 * res;
    let (x0, y0) = (bx0 - margin, by0 - margin);
    let w = ((bx1 + margin - x0) / res).ceil() as usize + 1;
    let h = ((by1 + margin - y0) / res).ceil() as usize + 1;
    if w.saturating_mul(h) > max_cells {
        return Err(PluginError::Coded(
            "too_big",
            format!(
                "the grid of the isolation would have {} cells, the most is {} (max_grid_cells)",
                w.saturating_mul(h),
                max_cells
            ),
        ));
    }
    let grid = Grid { x0, y0, res, w, h };
    let mut copper = vec![false; w * h];
    // a cell is marked when its square touches copper: half of the diagonal of a cell is the most a mark can lie out
    let half = res * std::f64::consts::FRAC_1_SQRT_2;
    let cell_range = |lo: f64, hi: f64, origin: f64, n: usize| -> (usize, usize) {
        let a = ((lo - origin) / res).floor().max(0.0) as usize;
        let b = (((hi - origin) / res).ceil().max(0.0) as usize).min(n - 1);
        (a.min(n - 1), b)
    };
    for t in &layer.tracks {
        let r = t.width / 2.0;
        let (i0, i1) = cell_range(
            t.from.0.min(t.to.0) - r - res,
            t.from.0.max(t.to.0) + r + res,
            x0,
            w,
        );
        let (j0, j1) = cell_range(
            t.from.1.min(t.to.1) - r - res,
            t.from.1.max(t.to.1) + r + res,
            y0,
            h,
        );
        for j in j0..=j1 {
            for i in i0..=i1 {
                if dist_to_segment(grid.center(i, j), t.from, t.to) <= r + half {
                    copper[grid.at(i, j)] = true;
                }
            }
        }
    }
    for f in &layer.flashes {
        let (sw, sh) = f.shape.size().unwrap_or((0.0, 0.0));
        let (i0, i1) = cell_range(f.at.0 - sw / 2.0 - res, f.at.0 + sw / 2.0 + res, x0, w);
        let (j0, j1) = cell_range(f.at.1 - sh / 2.0 - res, f.at.1 + sh / 2.0 + res, y0, h);
        for j in j0..=j1 {
            for i in i0..=i1 {
                let (cx, cy) = grid.center(i, j);
                let inside = match f.shape {
                    Shape::Circle(d) => (cx - f.at.0).hypot(cy - f.at.1) <= d / 2.0 + half,
                    _ => {
                        (cx - f.at.0).abs() <= sw / 2.0 + res / 2.0
                            && (cy - f.at.1).abs() <= sh / 2.0 + res / 2.0
                    }
                };
                if inside {
                    copper[grid.at(i, j)] = true;
                }
            }
        }
    }
    Ok((grid, copper))
}

/// The cells within `radius` of a copper cell.
fn dilate(grid: &Grid, copper: &[bool], radius: f64) -> Vec<bool> {
    let rc = (radius / grid.res).ceil() as i64;
    let mut disc: Vec<(i64, i64)> = Vec::new();
    for dj in -rc..=rc {
        for di in -rc..=rc {
            if ((di * di + dj * dj) as f64).sqrt() * grid.res <= radius {
                disc.push((di, dj));
            }
        }
    }
    let mut out = vec![false; copper.len()];
    for j in 0..grid.h {
        for i in 0..grid.w {
            if !copper[grid.at(i, j)] {
                continue;
            }
            for &(di, dj) in &disc {
                let (ni, nj) = (i as i64 + di, j as i64 + dj);
                if ni >= 0 && nj >= 0 && (ni as usize) < grid.w && (nj as usize) < grid.h {
                    out[grid.at(ni as usize, nj as usize)] = true;
                }
            }
        }
    }
    out
}

/// The loops that run along the edge of the cells of `mask`, the inside on the left.
fn loops_of(grid: &Grid, mask: &[bool]) -> Vec<Vec<(i64, i64)>> {
    let get = |i: i64, j: i64| -> bool {
        i >= 0
            && j >= 0
            && (i as usize) < grid.w
            && (j as usize) < grid.h
            && mask[grid.at(i as usize, j as usize)]
    };
    let mut next: HashMap<(i64, i64), Vec<(i64, i64)>> = HashMap::new();
    for j in 0..grid.h as i64 {
        for i in 0..grid.w as i64 {
            if !get(i, j) {
                continue;
            }
            if !get(i, j - 1) {
                next.entry((i, j)).or_default().push((i + 1, j));
            }
            if !get(i + 1, j) {
                next.entry((i + 1, j)).or_default().push((i + 1, j + 1));
            }
            if !get(i, j + 1) {
                next.entry((i + 1, j + 1)).or_default().push((i, j + 1));
            }
            if !get(i - 1, j) {
                next.entry((i, j + 1)).or_default().push((i, j));
            }
        }
    }
    let mut starts: Vec<(i64, i64)> = next.keys().copied().collect();
    starts.sort_unstable();
    let mut out = Vec::new();
    for s in starts {
        while next.get(&s).is_some_and(|v| !v.is_empty()) {
            let mut path = vec![s];
            let mut cur = s;
            while let Some(n) = next.get_mut(&cur).and_then(Vec::pop) {
                if n == s {
                    break;
                }
                path.push(n);
                cur = n;
            }
            if path.len() >= 4 {
                out.push(path);
            }
        }
    }
    out
}

/// Takes out the points of a closed loop that a straight line through their neighbours stays within `tol` of.
fn simplify(points: &[(f64, f64)], tol: f64) -> Vec<(f64, f64)> {
    fn rec(p: &[(f64, f64)], lo: usize, hi: usize, tol: f64, keep: &mut Vec<bool>) {
        if hi <= lo + 1 {
            return;
        }
        let (mut far, mut at) = (0.0, lo);
        for k in lo + 1..hi {
            let d = dist_to_segment(p[k], p[lo], p[hi]);
            if d > far {
                far = d;
                at = k;
            }
        }
        if far > tol {
            keep[at] = true;
            rec(p, lo, at, tol, keep);
            rec(p, at, hi, tol, keep);
        }
    }
    let mut closed: Vec<(f64, f64)> = points.to_vec();
    closed.push(points[0]);
    let mut keep = vec![false; closed.len()];
    keep[0] = true;
    let last = closed.len() - 1;
    keep[last] = true;
    // the loop is cut at the point farthest from the first so that the two halves are not a line back on itself
    let mid = (1..last)
        .max_by(|a, b| {
            let da = (closed[*a].0 - closed[0].0).hypot(closed[*a].1 - closed[0].1);
            let db = (closed[*b].0 - closed[0].0).hypot(closed[*b].1 - closed[0].1);
            da.total_cmp(&db)
        })
        .unwrap_or(last / 2);
    keep[mid] = true;
    rec(&closed, 0, mid, tol, &mut keep);
    rec(&closed, mid, last, tol, &mut keep);
    let mut out: Vec<(f64, f64)> = closed
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(p, _)| *p)
        .collect();
    out.pop();
    out
}

fn make_isolate(
    layers: &[Layer],
    request: &Value,
    params: &Value,
    cfg: &CamConfig,
) -> PluginResult<Made> {
    let cut = cut_params(params, cfg, 0.05)?;
    let tool = param(params, "tool_dia_mm", 0.2, 0.05, 3.0)?;
    let passes = param(params, "passes", 1.0, 1.0, 5.0)?.round() as usize;
    let overlap = param(params, "overlap_pct", 50.0, 0.0, 90.0)?;
    let res = param(
        params,
        "resolution_mm",
        (tool / 8.0).clamp(0.01, 0.1),
        0.005,
        0.5,
    )?;
    let step = tool * (1.0 - overlap / 100.0);
    let reach = tool / 2.0 + step * (passes - 1) as f64;
    let wanted = request.get("layer").and_then(Value::as_str);
    let layer = layers
        .iter()
        .find(|l| l.is_copper() && wanted.is_none_or(|w| l.name == w))
        .ok_or_else(|| {
            PluginError::Coded(
                "missing_layer",
                "the job needs a copper layer (name it in `layer` if there are several)".into(),
            )
        })?;
    let (grid, copper) = rasterize(layer, tool, reach, res, cfg.max_cells)?;
    let mut lines = header(&cut, "isolate");
    let (mut length, mut loops_total) = (0.0, 0usize);
    let mut here = (0.0, 0.0);
    let mut all: Vec<Vec<(f64, f64)>> = Vec::new();
    for k in 0..passes {
        let radius = tool / 2.0 + step * k as f64;
        // the path has to stay `radius` from the copper: the marks lie out by half a cell diagonal, the corners of the loop are half a diagonal from the cells they
        // bound and the simplification cuts at most a quarter of a cell, so the cells are grown by the radius and those three
        let grown = dilate(
            &grid,
            &copper,
            radius + grid.res * (2.0 * std::f64::consts::FRAC_1_SQRT_2 + 0.25),
        );
        for l in loops_of(&grid, &grown) {
            let pts: Vec<(f64, f64)> = l
                .iter()
                .map(|&(i, j)| (grid.x0 + i as f64 * grid.res, grid.y0 + j as f64 * grid.res))
                .collect();
            all.push(simplify(&pts, grid.res / 4.0));
        }
    }
    check_travel(cfg, &all.iter().flatten().copied().collect::<Vec<_>>())?;
    while !all.is_empty() {
        let k = all
            .iter()
            .enumerate()
            .min_by(|a, b| {
                let d = |p: &(f64, f64)| (p.0 - here.0).hypot(p.1 - here.1);
                d(&a.1[0]).total_cmp(&d(&b.1[0]))
            })
            .map_or(0, |(k, _)| k);
        let path = all.swap_remove(k);
        lines.push(format!("G0 X{} Y{}", num(path[0].0), num(path[0].1)));
        lines.push(format!("G1 Z{} F{}", num(-cut.depth), num(cut.feed)));
        for w in path.iter().skip(1).chain(std::iter::once(&path[0])) {
            lines.push(format!("G1 X{} Y{}", num(w.0), num(w.1)));
        }
        for pair in path.windows(2) {
            length += (pair[0].0 - pair[1].0).hypot(pair[0].1 - pair[1].1);
        }
        length += (path[path.len() - 1].0 - path[0].0).hypot(path[path.len() - 1].1 - path[0].1);
        lines.push(format!("G0 Z{}", num(cut.safe_z)));
        here = path[0];
        loops_total += 1;
    }
    if loops_total == 0 {
        return Err(bad("the layer has no copper to isolate"));
    }
    lines.extend(footer(&cut));
    Ok(Made {
        lines,
        steps: loops_total,
        note: json!({
            "layer": layer.name, "loops": loops_total, "passes": passes, "tool_dia_mm": tool,
            "resolution_mm": res, "cut_length_mm": round2(length), "grid_cells": grid.w * grid.h,
        }),
    })
}

// ---- verify --------------------------------------------------------------------------------------------------------------------------

fn make_verify(text: &str, params: &Value, cfg: &CamConfig) -> PluginResult<Made> {
    let program = gcode::parse(text, 0, false).map_err(|e| PluginError::Coded(e.code, e.detail))?;
    if program.entries.len() > cfg.max_lines {
        return Err(PluginError::Coded(
            "too_big",
            format!(
                "a program may have {} lines at the most (max_gcode_lines)",
                cfg.max_lines
            ),
        ));
    }
    let limit_z = param(params, "min_z_mm", -cfg.max_depth, -100.0, 0.0)?;
    let cnc = CncConfig {
        travel: [cfg.travel[0], cfg.travel[1], None],
        reject_out_of_travel: false,
        spindle_max_rpm: cfg.max_rpm.max(65535.0),
        ..CncConfig::default()
    };
    let a = analyze(&program.entries, [0.0; 3], &cnc)
        .map_err(|e| PluginError::Coded(e.code, e.detail))?;
    let mut findings: Vec<Value> = Vec::new();
    let mut note = |severity: &str, rule: &str, line: usize, message: String| {
        findings
            .push(json!({"severity": severity, "rule": rule, "line": line, "message": message}));
    };
    let (mut spindle_on, mut tool_changes, mut ended) = (false, 0usize, false);
    let mut z = f64::NAN;
    for (n, e) in program.entries.iter().enumerate() {
        let line = e.raw + 1;
        let words = gcode::words(&e.text);
        let g0 = words.iter().any(|&(l, v)| l == 'G' && v == 0.0);
        let cutting = words
            .iter()
            .any(|&(l, v)| l == 'G' && (1.0..=3.0).contains(&v));
        for &(l, v) in &words {
            match (l, v as i64) {
                ('M', 3 | 4) => spindle_on = true,
                ('M', 5) => spindle_on = false,
                ('M', 6) => tool_changes += 1,
                ('M', 0) => tool_changes += 1,
                ('M', 2 | 30) => ended = true,
                _ => {}
            }
        }
        if words.iter().any(|&(l, _)| l == 'F') {
            let f = words
                .iter()
                .filter(|w| w.0 == 'F')
                .map(|w| w.1)
                .fold(0.0, f64::max);
            if f > cfg.max_feed {
                note(
                    "error",
                    "feed_over_limit",
                    line,
                    format!(
                        "F{} is over the {} mm/min the worker allows",
                        num(f),
                        num(cfg.max_feed)
                    ),
                );
            }
        }
        if words.iter().any(|&(l, v)| l == 'S' && v > cfg.max_rpm) {
            note(
                "error",
                "rpm_over_limit",
                line,
                format!("S is over the {} turns the worker allows", num(cfg.max_rpm)),
            );
        }
        let new_z = a.after[n].0[2];
        if new_z < limit_z - 1e-9 && (z.is_nan() || new_z < z - 1e-9) {
            note(
                "error",
                "too_deep",
                line,
                format!(
                    "the cut reaches Z{} below the limit Z{}",
                    num(new_z),
                    num(limit_z)
                ),
            );
        }
        if g0 && new_z < -1e-9 && !z.is_nan() && new_z < z - 1e-9 {
            note(
                "error",
                "rapid_into_material",
                line,
                "a rapid move goes down below the surface".into(),
            );
        }
        if cutting && !spindle_on {
            note(
                "warning",
                "cut_with_spindle_off",
                line,
                "a cutting move comes before the spindle is switched on".into(),
            );
        }
        z = new_z;
    }
    if !ended {
        note(
            "warning",
            "no_program_end",
            program.entries.len(),
            "the program does not end with M2 or M30".into(),
        );
    }
    if spindle_on {
        note(
            "warning",
            "spindle_left_on",
            program.entries.len(),
            "the program ends with the spindle on".into(),
        );
    }
    if !a.out_of_travel.is_empty() {
        for l in &a.out_of_travel {
            note(
                "error",
                "out_of_travel",
                *l,
                "the move goes outside the travel of the worker".into(),
            );
        }
    }
    let errors = findings.iter().filter(|f| f["severity"] == "error").count();
    let b = a.bounds.map(|(lo, hi)| json!({"min": {"x": lo[0], "y": lo[1], "z": lo[2]}, "max": {"x": hi[0], "y": hi[1], "z": hi[2]}}));
    let steps = program.entries.len();
    Ok(Made {
        lines: program.entries.iter().map(|e| e.text.clone()).collect(),
        steps,
        note: json!({
            "verdict": if errors == 0 { "pass" } else { "fail" }, "errors": errors, "findings": findings,
            "lines": steps, "rapid_moves": a.rapid_moves, "cut_moves": a.cut_moves, "arcs": a.arcs,
            "travel_mm": round2(a.travel_mm), "estimate_s": round2(a.estimate_s),
            "max_feed_mm_min": a.max_feed, "max_rpm": a.max_rpm, "tool_changes": tool_changes, "bounds": b,
        }),
    })
}

// ---- stencil -------------------------------------------------------------------------------------------------------------------------

/// Whether a `params` flag is there and true; a value that is not a boolean is refused.
fn flag_param(params: &Value, key: &str) -> PluginResult<bool> {
    match params.get(key) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(format!("{key} is true or false"))),
    }
}

/// One aperture of a stencil: where it is and how it is cut.
enum Aperture {
    /// The path of the center of the tool: a rectangle of this width and height.
    Rect(f64, f64),
    /// The path of the center of the tool: a circle of this radius.
    Round(f64),
}

/// The G-code that cuts the apertures of a paste layer in a foil: the path of the center of the tool goes inside the edge of every pad by the radius of the tool. A pad that is
/// smaller than the tool, or made by a macro, is refused (with `skip_small` the pads that are too small are left out and counted, which makes a stencil with fewer openings).
fn make_stencil(
    layers: &[Layer],
    request: &Value,
    params: &Value,
    cfg: &CamConfig,
) -> PluginResult<Made> {
    let cut = cut_params(params, cfg, 0.12)?;
    let tool = param(params, "tool_dia_mm", 0.4, 0.05, 3.0)?;
    let shrink = param(params, "shrink_pct", 0.0, 0.0, 30.0)?;
    let mirror = flag_param(params, "mirror")?;
    let skip_small = flag_param(params, "skip_small")?;
    let wanted = request.get("layer").and_then(Value::as_str);
    let layer = layers
        .iter()
        .find(|l| l.function.starts_with("paste") && wanted.is_none_or(|w| l.name == w))
        .ok_or_else(|| {
            PluginError::Coded(
                "missing_layer",
                "the job needs a paste layer (name it in `layer` if there are several)".into(),
            )
        })?;
    if layer.flashes.is_empty() {
        return Err(PluginError::Coded(
            "missing_layer",
            "the paste layer has no pads".into(),
        ));
    }
    let scale = 1.0 - shrink / 100.0;
    let (lo, hi) = layer
        .flashes
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), f| {
            (lo.min(f.at.0), hi.max(f.at.0))
        });
    let mut pads: Vec<((f64, f64), Aperture)> = Vec::new();
    let mut skipped = 0usize;
    for f in &layer.flashes {
        let Some((w, h)) = f.shape.size() else {
            return Err(bad(
                "a pad made by a macro has no size known here: the stencil cannot be cut",
            ));
        };
        let at = (if mirror { lo + hi - f.at.0 } else { f.at.0 }, f.at.1);
        let (w, h) = (w * scale, h * scale);
        let aperture = match f.shape {
            Shape::Circle(_) => (w - tool > 1e-6).then(|| Aperture::Round((w - tool) / 2.0)),
            _ => (w - tool > 1e-6 && h - tool > 1e-6).then_some(Aperture::Rect(w - tool, h - tool)),
        };
        match aperture {
            Some(a) => pads.push((at, a)),
            None if skip_small => skipped += 1,
            None => {
                return Err(bad(format!(
                    "a pad of {} x {} mm is too small for a {} mm tool (skip_small: true leaves such pads out)",
                    num(w),
                    num(h),
                    num(tool)
                )))
            }
        }
    }
    if pads.is_empty() {
        return Err(bad(
            "every pad is too small for the tool: nothing would be cut",
        ));
    }
    let mut reach: Vec<(f64, f64)> = Vec::new();
    for (at, a) in &pads {
        let (rx, ry) = match a {
            Aperture::Rect(w, h) => (w / 2.0, h / 2.0),
            Aperture::Round(r) => (*r, *r),
        };
        reach.push((at.0 - rx, at.1 - ry));
        reach.push((at.0 + rx, at.1 + ry));
    }
    check_travel(cfg, &reach)?;
    let mut lines = header(&cut, "stencil");
    let (mut here, mut length) = ((0.0, 0.0), 0.0);
    let total = pads.len();
    while !pads.is_empty() {
        let k = pads
            .iter()
            .enumerate()
            .min_by(|a, b| {
                let d = |p: &(f64, f64)| (p.0 - here.0).hypot(p.1 - here.1);
                d(&a.1 .0).total_cmp(&d(&b.1 .0))
            })
            .map_or(0, |(k, _)| k);
        let (at, a) = pads.swap_remove(k);
        here = at;
        match a {
            Aperture::Rect(w, h) => {
                let (x0, y0, x1, y1) = (
                    at.0 - w / 2.0,
                    at.1 - h / 2.0,
                    at.0 + w / 2.0,
                    at.1 + h / 2.0,
                );
                lines.push(format!("G0 X{} Y{}", num(x0), num(y0)));
                lines.push(format!("G1 Z{} F{}", num(-cut.depth), num(cut.feed)));
                lines.push(format!("G1 X{} Y{}", num(x1), num(y0)));
                lines.push(format!("G1 X{} Y{}", num(x1), num(y1)));
                lines.push(format!("G1 X{} Y{}", num(x0), num(y1)));
                lines.push(format!("G1 X{} Y{}", num(x0), num(y0)));
                length += 2.0 * (w + h);
            }
            Aperture::Round(r) => {
                lines.push(format!("G0 X{} Y{}", num(at.0 + r), num(at.1)));
                lines.push(format!("G1 Z{} F{}", num(-cut.depth), num(cut.feed)));
                lines.push(format!(
                    "G2 X{} Y{} I{} J0",
                    num(at.0 + r),
                    num(at.1),
                    num(-r)
                ));
                length += std::f64::consts::TAU * r;
            }
        }
        lines.push(format!("G0 Z{}", num(cut.safe_z)));
    }
    lines.extend(footer(&cut));
    Ok(Made {
        lines,
        steps: total,
        note: json!({
            "layer": layer.name, "apertures": total, "skipped_too_small": skipped, "tool_dia_mm": tool,
            "shrink_pct": shrink, "mirror": mirror, "depth_mm": cut.depth, "cut_length_mm": round2(length),
        }),
    })
}

// ---- autolevel -----------------------------------------------------------------------------------------------------------------------

/// The height of a surface at `(u, v)`, both from 0 to 1 over the grid of probe points `heights` (rows from the bottom, columns from the left): the linear interpolation between
/// the four points around it. A grid with fewer than two rows or columns, or rows of different lengths, gives `None`.
pub fn bilinear(heights: &[Vec<f64>], u: f64, v: f64) -> Option<f64> {
    let rows = heights.len();
    let cols = heights.first()?.len();
    if rows < 2 || cols < 2 || heights.iter().any(|r| r.len() != cols) {
        return None;
    }
    let (fx, fy) = (
        u.clamp(0.0, 1.0) * (cols - 1) as f64,
        v.clamp(0.0, 1.0) * (rows - 1) as f64,
    );
    let (i, j) = (
        (fx.floor() as usize).min(cols - 2),
        (fy.floor() as usize).min(rows - 2),
    );
    let (tx, ty) = (fx - i as f64, fy - j as f64);
    let low = heights[j][i] * (1.0 - tx) + heights[j][i + 1] * tx;
    let high = heights[j + 1][i] * (1.0 - tx) + heights[j + 1][i + 1] * tx;
    Some(low * (1.0 - ty) + high * ty)
}

/// The probe grid of an `autolevel` request.
struct ProbeGrid {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    columns: usize,
    rows: usize,
}

impl ProbeGrid {
    fn from_params(params: &Value) -> PluginResult<ProbeGrid> {
        Ok(ProbeGrid {
            x: param(params, "x_mm", 0.0, -5000.0, 5000.0)?,
            y: param(params, "y_mm", 0.0, -5000.0, 5000.0)?,
            width: param(params, "width_mm", 50.0, 1.0, 5000.0)?,
            height: param(params, "height_mm", 50.0, 1.0, 5000.0)?,
            columns: param(params, "columns", 5.0, 2.0, 50.0)?.round() as usize,
            rows: param(params, "rows", 5.0, 2.0, 50.0)?.round() as usize,
        })
    }

    fn point(&self, column: usize, row: usize) -> (f64, f64) {
        (
            self.x + self.width * column as f64 / (self.columns - 1) as f64,
            self.y + self.height * row as f64 / (self.rows - 1) as f64,
        )
    }

    fn view(&self) -> Value {
        json!({
            "x_mm": self.x, "y_mm": self.y, "width_mm": self.width, "height_mm": self.height,
            "columns": self.columns, "rows": self.rows,
        })
    }
}

/// The heights of a probe map in a request: `heights` is a list of rows, each a list of numbers, as many columns in every row as the grid has.
fn heights_of(request: &Value, grid: &ProbeGrid) -> PluginResult<Vec<Vec<f64>>> {
    let rows = request
        .get("heights")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            bad("a probe map brings `heights`: a list of rows, each a list of numbers")
        })?;
    if rows.len() != grid.rows {
        return Err(bad(format!(
            "heights has {} rows and the grid has {}",
            rows.len(),
            grid.rows
        )));
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let cells = row
            .as_array()
            .filter(|c| c.len() == grid.columns)
            .ok_or_else(|| bad(format!("every row of heights has {} numbers", grid.columns)))?;
        let mut r = Vec::with_capacity(cells.len());
        for c in cells {
            match c.as_f64() {
                Some(z) if z.is_finite() && z.abs() <= 100.0 => r.push(z),
                _ => return Err(bad("a height is a number from -100 to 100 mm")),
            }
        }
        out.push(r);
    }
    Ok(out)
}

/// The program that probes a grid of points over the board (`mode: "probe"`, the default), or the account of a probe map that was made with it (`mode: "map"`).
fn make_autolevel(request: &Value, params: &Value, cfg: &CamConfig) -> PluginResult<Made> {
    let grid = ProbeGrid::from_params(params)?;
    let mode = request
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("probe");
    let corners = [
        grid.point(0, 0),
        grid.point(grid.columns - 1, grid.rows - 1),
    ];
    check_travel(cfg, &corners)?;
    match mode {
        "probe" => {
            let cut = cut_params(params, cfg, 1.0)?;
            let feed = param(
                params,
                "probe_feed_mm_min",
                25.0_f64.min(cfg.max_feed),
                1.0,
                cfg.max_feed,
            )?;
            let mut lines = vec![
                "(autolevel probe)".to_string(),
                "G21".into(),
                "G90".into(),
                "G17".into(),
                format!("G0 Z{}", num(cut.safe_z)),
            ];
            let mut positions: Vec<Value> = Vec::new();
            for row in 0..grid.rows {
                // the rows are probed one way and then the other, so the head does not run back across the board
                let columns: Vec<usize> = if row.is_multiple_of(2) {
                    (0..grid.columns).collect()
                } else {
                    (0..grid.columns).rev().collect()
                };
                for column in columns {
                    let (x, y) = grid.point(column, row);
                    lines.push(format!("(probe row {row} column {column})"));
                    lines.push(format!("G0 X{} Y{}", num(x), num(y)));
                    lines.push(format!("G38.2 Z{} F{}", num(-cut.depth), num(feed)));
                    lines.push(format!("G0 Z{}", num(cut.safe_z)));
                    positions.push(
                        json!({"row": row, "column": column, "x_mm": round2(x), "y_mm": round2(y)}),
                    );
                }
            }
            lines.push(format!("G0 Z{}", num(cut.safe_z)));
            lines.push("M2".into());
            Ok(Made {
                lines,
                steps: positions.len(),
                note: json!({
                    "mode": "probe", "points": positions.len(), "grid": grid.view(), "probe_depth_mm": cut.depth,
                    "probe_feed_mm_min": feed, "positions": positions,
                }),
            })
        }
        "map" => {
            let tolerance = param(params, "tolerance_mm", 0.1, 0.001, 5.0)?;
            let heights = heights_of(request, &grid)?;
            let all: Vec<f64> = heights.iter().flatten().copied().collect();
            let (min, max) = all
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &z| {
                    (a.min(z), b.max(z))
                });
            let mean = all.iter().sum::<f64>() / all.len() as f64;
            let worst = heights
                .iter()
                .enumerate()
                .flat_map(|(r, row)| row.iter().enumerate().map(move |(c, &z)| (r, c, z)))
                .max_by(|a, b| (a.2 - mean).abs().total_cmp(&(b.2 - mean).abs()))
                .unwrap_or((0, 0, 0.0));
            let column_mean =
                |c: usize| heights.iter().map(|r| r[c]).sum::<f64>() / grid.rows as f64;
            let row_mean = |r: usize| heights[r].iter().sum::<f64>() / grid.columns as f64;
            let tilt_x = (column_mean(grid.columns - 1) - column_mean(0)) / grid.width;
            let tilt_y = (row_mean(grid.rows - 1) - row_mean(0)) / grid.height;
            let range = max - min;
            let (wx, wy) = grid.point(worst.1, worst.0);
            Ok(Made {
                lines: Vec::new(),
                steps: grid.rows,
                note: json!({
                    "mode": "map", "grid": grid.view(), "points": all.len(), "min_mm": round3(min), "max_mm": round3(max),
                    "mean_mm": round3(mean), "range_mm": round3(range), "tolerance_mm": tolerance,
                    "verdict": if range <= tolerance { "flat" } else { "uneven" },
                    "tilt_x_mm_per_mm": round6(tilt_x), "tilt_y_mm_per_mm": round6(tilt_y),
                    "center_mm": bilinear(&heights, 0.5, 0.5).map(round3),
                    "farthest_from_the_mean": {"row": worst.0, "column": worst.1, "x_mm": round2(wx), "y_mm": round2(wy), "height_mm": round3(worst.2)},
                }),
            })
        }
        _ => Err(bad("mode is `probe` or `map`")),
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn round6(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

// ---- inspect -------------------------------------------------------------------------------------------------------------------------

/// The account of a toolpath: how far it cuts and rapids, at which depths it cuts and how much of the cutting is in the air. It judges nothing (`verify` does that). Pictures
/// are not read here.
fn make_inspect(text: &str, cfg: &CamConfig) -> PluginResult<Made> {
    let program = gcode::parse(text, 0, false).map_err(|e| PluginError::Coded(e.code, e.detail))?;
    if program.entries.len() > cfg.max_lines {
        return Err(PluginError::Coded(
            "too_big",
            format!(
                "a program may have {} lines at the most (max_gcode_lines)",
                cfg.max_lines
            ),
        ));
    }
    let cnc = CncConfig {
        travel: [None, None, None],
        reject_out_of_travel: false,
        spindle_max_rpm: 65535.0,
        ..CncConfig::default()
    };
    let a = analyze(&program.entries, [0.0; 3], &cnc)
        .map_err(|e| PluginError::Coded(e.code, e.detail))?;
    let mut levels: std::collections::BTreeMap<i64, (usize, f64)> =
        std::collections::BTreeMap::new();
    let (mut cut_length, mut rapid_length, mut air_moves, mut plunges) = (0.0, 0.0, 0usize, 0usize);
    let mut before = [0.0; 3];
    for (n, e) in program.entries.iter().enumerate() {
        let after = a.after[n].0;
        let words = gcode::words(&e.text);
        let rapid = words.iter().any(|&(l, v)| l == 'G' && v == 0.0);
        let cutting = words
            .iter()
            .any(|&(l, v)| l == 'G' && (1.0..=3.0).contains(&v));
        let flat = (after[0] - before[0]).hypot(after[1] - before[1]);
        let long = flat.hypot(after[2] - before[2]);
        if cutting {
            if flat < 1e-9 && after[2] < before[2] - 1e-9 {
                plunges += 1;
            } else {
                let at = levels
                    .entry((after[2] * 1000.0).round() as i64)
                    .or_default();
                at.0 += 1;
                at.1 += long;
            }
            cut_length += long;
            if after[2] > 1e-9 {
                air_moves += 1;
            }
        } else if rapid {
            rapid_length += long;
        }
        before = after;
    }
    let total_levels = levels.len();
    let shown: Vec<Value> = levels
        .iter()
        .take(50)
        .map(|(z, (moves, length))| json!({"z_mm": *z as f64 / 1000.0, "moves": moves, "length_mm": round2(*length)}))
        .collect();
    let b = a.bounds.map(|(lo, hi)| json!({"min": {"x": lo[0], "y": lo[1], "z": lo[2]}, "max": {"x": hi[0], "y": hi[1], "z": hi[2]}}));
    let steps = program.entries.len();
    Ok(Made {
        lines: Vec::new(),
        steps,
        note: json!({
            "lines": steps, "rapid_moves": a.rapid_moves, "cut_moves": a.cut_moves, "arcs": a.arcs, "plunges": plunges,
            "cut_length_mm": round2(cut_length), "rapid_length_mm": round2(rapid_length), "air_moves": air_moves,
            "levels": shown, "levels_total": total_levels, "estimate_s": round2(a.estimate_s),
            "max_feed_mm_min": a.max_feed, "max_rpm": a.max_rpm, "bounds": b,
        }),
    })
}

// ---- the engine ----------------------------------------------------------------------------------------------------------------------

/// The CAM workers of the server: their jobs.
#[derive(Default)]
pub struct CamEngine {
    book: SimBook,
}

impl CamEngine {
    pub fn book(&self) -> &SimBook {
        &self.book
    }

    pub fn prepare(
        &self,
        machine: &str,
        cfg: &CamConfig,
        request: &Value,
    ) -> PluginResult<Prepared> {
        let kind = request.get("kind").and_then(Value::as_str).unwrap_or("");
        let params = match request.get("params") {
            None => json!({}),
            Some(p) if p.is_object() => p.clone(),
            Some(_) => return Err(bad("params is an object")),
        };
        let (kind, made): (&'static str, Made) = match kind {
            "drill" | "outline" | "isolate" => {
                let files = files_of(request, cfg.max_input_mib)?;
                let layers = read_files(&files)?;
                match kind {
                    "drill" => ("drill", make_drill(&layers, &params, cfg)?),
                    "outline" => ("outline", make_outline(&layers, &params, cfg)?),
                    _ => ("isolate", make_isolate(&layers, request, &params, cfg)?),
                }
            }
            "stencil" => {
                let files = files_of(request, cfg.max_input_mib)?;
                let layers = read_files(&files)?;
                ("stencil", make_stencil(&layers, request, &params, cfg)?)
            }
            "autolevel" => ("autolevel", make_autolevel(request, &params, cfg)?),
            "inspect" => {
                let text = request
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| request.get("artifact_text").and_then(Value::as_str))
                    .ok_or_else(|| PluginError::Bad("an inspect request brings the program in `text` or names a G-code artifact".into()))?;
                if text.len() as f64 > cfg.max_input_mib * 1024.0 * 1024.0 {
                    return Err(PluginError::Coded("input_too_large", format!("the program is over {} MiB (max_input_mib)", num(cfg.max_input_mib))));
                }
                ("inspect", make_inspect(text, cfg)?)
            }
            "verify" => {
                let text = request
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| request.get("artifact_text").and_then(Value::as_str))
                    .ok_or_else(|| PluginError::Bad("a verify request brings the program in `text` or names a G-code artifact".into()))?;
                if text.len() as f64 > cfg.max_input_mib * 1024.0 * 1024.0 {
                    return Err(PluginError::Coded("input_too_large", format!("the program is over {} MiB (max_input_mib)", num(cfg.max_input_mib))));
                }
                ("verify", make_verify(text, &params, cfg)?)
            }
            _ => {
                return Err(PluginError::Bad(
                    "the request has to be {\"kind\": \"drill\" | \"outline\" | \"isolate\" | \"stencil\" | \"autolevel\" | \"inspect\" | \"verify\", ...}".into(),
                ))
            }
        };
        if made.lines.len() > cfg.max_lines {
            return Err(PluginError::Coded(
                "too_big",
                format!(
                    "the program would have {} lines, the most is {} (max_gcode_lines)",
                    made.lines.len(),
                    cfg.max_lines
                ),
            ));
        }
        let mut view = json!({"kind": kind, "result": made.note, "lines": made.lines.len()});
        // a program is in the view when the job made one (`verify`, `inspect` and a probe map only report)
        if !matches!(kind, "verify" | "inspect") && !made.lines.is_empty() {
            view["gcode"] = json!(made.lines.join("\n") + "\n");
        }
        let native_id = self.book.add(
            machine,
            kind,
            evenly(made.steps.max(1), cfg.item_ms / 1000.0),
            1.0,
        );
        Ok(Prepared { native_id, view })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::WorkState;

    const DRILL: &str =
        "M48\nMETRIC\nT1C0.800\nT2C1.000\n%\nT1\nX10.0Y10.0\nX20.0Y10.0\nT2\nX15.0Y30.0\nM30\n";
    const OUTLINE: &str = "%FSLAX24Y24*%\n%MOMM*%\n%ADD10C,0.100*%\nD10*\nX0Y0D02*\nX500000Y0D01*\nX500000Y400000D01*\nX0Y400000D01*\nX0Y0D01*\nM02*\n";
    const TRACK: &str = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.200*%\nD10*\nX100000Y100000D02*\nX200000Y100000D01*\nM02*\n";

    fn run(request: Value) -> PluginResult<Prepared> {
        CamEngine::default().prepare("w1", &CamConfig::default(), &request)
    }

    fn gcode_of(p: &Prepared) -> String {
        p.view["gcode"].as_str().unwrap().to_string()
    }

    #[test]
    fn the_settings_have_ranges_and_a_key_that_is_not_one_is_refused() {
        assert_eq!(
            CamConfig::from_settings(&json!({})).unwrap(),
            CamConfig::default()
        );
        let c =
            CamConfig::from_settings(&json!({"max_depth_mm": 2, "travel_x_mm": 200, "item_ms": 3}))
                .unwrap();
        assert_eq!(
            (c.max_depth, c.travel, c.item_ms),
            (2.0, [Some(200.0), None], 3.0)
        );
        for bad in [
            json!({"max_depth_mm": 0}),
            json!({"max_feed_mm_min": 40000}),
            json!({"max_grid_cells": 5}),
            json!({"zzz": 1}),
            json!({"max_input_mib": 0}),
        ] {
            assert!(CamConfig::from_settings(&bad).is_err(), "{bad}");
        }
        assert_eq!(SETTINGS_HELP.len(), 10);
        assert_eq!(TOOLS.len(), 7);
    }

    const PASTE: &str = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Paste,Top*%\n%ADD10R,1.200X0.600*%\n%ADD11C,1.000*%\nD10*\nX100000Y100000D03*\nX200000Y100000D03*\nD11*\nX150000Y200000D03*\nM02*\n";

    fn stencil(extra: Value) -> PluginResult<Prepared> {
        let mut request =
            json!({"kind": "stencil", "files": [{"name": "board.gtp", "text": PASTE}]});
        if let (Some(r), Some(e)) = (request.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                r.insert(k.clone(), v.clone());
            }
        }
        run(request)
    }

    #[test]
    fn a_stencil_cuts_inside_every_pad_by_the_radius_of_the_tool() {
        let p = stencil(json!({"params": {"tool_dia_mm": 0.4}})).unwrap();
        let g = gcode_of(&p);
        assert!(g.starts_with("(stencil)\nG21\n"));
        assert_eq!(p.view["result"]["apertures"], 3);
        assert_eq!(p.view["result"]["skipped_too_small"], 0);
        // the first rectangle is 1.2 x 0.6 mm at (10, 10): the path of the center of a 0.4 mm tool is 0.8 x 0.2 mm
        assert!(g.contains("G0 X9.6 Y9.9"), "{g}");
        assert!(
            g.contains("G1 X10.4 Y9.9") && g.contains("G1 X10.4 Y10.1"),
            "{g}"
        );
        // the round pad is 1 mm: a circle of 0.3 mm radius around (15, 20)
        assert!(
            g.contains("G0 X15.3 Y20") && g.contains("G2 X15.3 Y20 I-0.3 J0"),
            "{g}"
        );
        assert!(g.contains("G1 Z-0.12 F100"));
        assert!(g.trim_end().ends_with("M5\nM2"));
        // the pads are cut nearest first
        assert!(g.find("X9.6 Y9.9").unwrap() < g.find("X19.6 Y9.9").unwrap());
    }

    #[test]
    fn a_stencil_can_shrink_the_openings_and_mirror_the_foil() {
        let plain = stencil(json!({"params": {"tool_dia_mm": 0.2}})).unwrap();
        let shrunk = stencil(json!({"params": {"tool_dia_mm": 0.2, "shrink_pct": 10}})).unwrap();
        assert!(
            plain.view["result"]["cut_length_mm"].as_f64()
                > shrunk.view["result"]["cut_length_mm"].as_f64()
        );
        let mirrored = stencil(json!({"params": {"tool_dia_mm": 0.4, "mirror": true}})).unwrap();
        // the pads at x = 10 and 20 swap places around the middle of the layer, 15
        assert!(gcode_of(&mirrored).contains("G0 X19.6 Y9.9"));
        assert!(gcode_of(&mirrored).contains("G0 X9.6 Y9.9"));
        assert_eq!(mirrored.view["result"]["mirror"], true);
    }

    #[test]
    fn a_stencil_refuses_what_it_cannot_cut_unless_told_to_skip_it() {
        // a 0.6 mm pad and a 1 mm tool
        let small = stencil(json!({"params": {"tool_dia_mm": 0.7}}));
        assert_eq!(small.err().map(|e| e.code()), Some("bad_params"));
        let skipped = stencil(json!({"params": {"tool_dia_mm": 0.7, "skip_small": true}})).unwrap();
        assert_eq!(skipped.view["result"]["apertures"], 1);
        assert_eq!(skipped.view["result"]["skipped_too_small"], 2);
        let nothing = stencil(json!({"params": {"tool_dia_mm": 3.0, "skip_small": true}}));
        assert_eq!(nothing.err().map(|e| e.code()), Some("bad_params"));
        let flag = stencil(json!({"params": {"mirror": "yes"}}));
        assert_eq!(flag.err().map(|e| e.code()), Some("bad_params"));
        // no paste layer, and a macro pad
        let no_paste = run(json!({"kind": "stencil", "files": [{"name": "t.gtl", "text": TRACK}]}));
        assert_eq!(no_paste.err().map(|e| e.code()), Some("missing_layer"));
        let macro_pad = PASTE.replace("%ADD11C,1.000*%", "%AMOVAL*1,1,0.5,0,0*%\n%ADD11OVAL*%");
        let macro_job =
            run(json!({"kind": "stencil", "files": [{"name": "b.gtp", "text": macro_pad}]}));
        assert_eq!(macro_job.err().map(|e| e.code()), Some("bad_params"));
        // the work has to fit the travel
        let cfg = CamConfig {
            travel: [Some(12.0), Some(12.0)],
            ..CamConfig::default()
        };
        let out = CamEngine::default().prepare(
            "w1",
            &cfg,
            &json!({"kind": "stencil", "files": [{"name": "b.gtp", "text": PASTE}]}),
        );
        assert_eq!(out.err().map(|e| e.code()), Some("out_of_travel"));
    }

    #[test]
    fn the_autolevel_program_probes_a_grid_row_by_row_back_and_forth() {
        let p = run(json!({"kind": "autolevel", "params": {"x_mm": 5, "y_mm": 10, "width_mm": 40, "height_mm": 20, "columns": 3, "rows": 2, "depth_mm": 1.5, "probe_feed_mm_min": 30}})).unwrap();
        let g = gcode_of(&p);
        assert!(
            g.starts_with("(autolevel probe)\nG21\n") && !g.contains("M3"),
            "{g}"
        );
        assert_eq!(g.matches("G38.2 Z-1.5 F30").count(), 6);
        assert_eq!(p.view["result"]["points"], 6);
        // the first row goes left to right and the second right to left
        let first = g.find("G0 X5 Y10").unwrap();
        let last_of_first = g.find("G0 X45 Y10").unwrap();
        let second_row_start = g.find("G0 X45 Y30").unwrap();
        let second_row_end = g.find("G0 X5 Y30").unwrap();
        assert!(
            first < last_of_first
                && last_of_first < second_row_start
                && second_row_start < second_row_end
        );
        assert_eq!(p.view["result"]["positions"][3]["column"], 2);
        assert_eq!(p.view["result"]["positions"][3]["row"], 1);
        assert!(g.trim_end().ends_with("M2"));
    }

    #[test]
    fn the_autolevel_grid_is_held_to_its_limits_and_to_the_travel() {
        for params in [
            json!({"columns": 1}),
            json!({"rows": 51}),
            json!({"width_mm": 0}),
            json!({"depth_mm": 99}),
            json!({"probe_feed_mm_min": 90000}),
        ] {
            let r = run(json!({"kind": "autolevel", "params": params.clone()}));
            assert_eq!(r.err().map(|e| e.code()), Some("bad_params"), "{params}");
        }
        let unknown = run(json!({"kind": "autolevel", "mode": "fly"}));
        assert_eq!(unknown.err().map(|e| e.code()), Some("bad_params"));
        let cfg = CamConfig {
            travel: [Some(30.0), None],
            ..CamConfig::default()
        };
        let out = CamEngine::default().prepare(
            "w1",
            &cfg,
            &json!({"kind": "autolevel", "params": {"width_mm": 60}}),
        );
        assert_eq!(out.err().map(|e| e.code()), Some("out_of_travel"));
    }

    #[test]
    fn a_probe_map_is_judged_flat_or_uneven_and_its_tilt_is_told() {
        let request = |heights: Value, tolerance: f64| {
            run(
                json!({"kind": "autolevel", "mode": "map", "heights": heights,
                "params": {"width_mm": 40, "height_mm": 20, "columns": 3, "rows": 2, "tolerance_mm": tolerance}}),
            )
        };
        let tilted = request(json!([[0.0, 0.1, 0.2], [0.0, 0.1, 0.2]]), 0.05).unwrap();
        let r = &tilted.view["result"];
        assert_eq!(r["verdict"], "uneven");
        assert_eq!(r["range_mm"], 0.2);
        assert_eq!(r["mean_mm"], 0.1);
        assert_eq!(r["tilt_x_mm_per_mm"], 0.005);
        assert_eq!(r["tilt_y_mm_per_mm"], 0.0);
        assert_eq!(r["center_mm"], 0.1);
        assert!(tilted.view.get("gcode").is_none(), "a map makes no program");
        let flat = request(json!([[0.0, 0.01, 0.02], [0.01, 0.0, 0.02]]), 0.05).unwrap();
        assert_eq!(flat.view["result"]["verdict"], "flat");
        for bad_map in [
            json!([[0.0, 0.1, 0.2]]),
            json!([[0.0, 0.1], [0.0, 0.1]]),
            json!([[0.0, 0.1, "x"], [0.0, 0.1, 0.2]]),
            json!([[0.0, 0.1, 500.0], [0.0, 0.1, 0.2]]),
            json!("nothing"),
        ] {
            assert_eq!(
                request(bad_map.clone(), 0.1).err().map(|e| e.code()),
                Some("bad_params"),
                "{bad_map}"
            );
        }
    }

    #[test]
    fn the_interpolation_of_a_probe_map_is_linear_between_the_points() {
        let h = vec![vec![0.0, 1.0], vec![2.0, 3.0]];
        assert_eq!(bilinear(&h, 0.0, 0.0), Some(0.0));
        assert_eq!(bilinear(&h, 1.0, 0.0), Some(1.0));
        assert_eq!(bilinear(&h, 0.0, 1.0), Some(2.0));
        assert_eq!(bilinear(&h, 1.0, 1.0), Some(3.0));
        let near = |got: Option<f64>, want: f64| got.is_some_and(|v| (v - want).abs() < 1e-9);
        assert!(near(bilinear(&h, 0.5, 0.5), 1.5));
        // outside the grid the nearest edge holds
        assert_eq!(bilinear(&h, -3.0, 9.0), Some(2.0));
        // three columns: the middle cell
        let h3 = vec![vec![0.0, 1.0, 5.0], vec![0.0, 1.0, 5.0]];
        assert!(near(bilinear(&h3, 0.5, 0.0), 1.0));
        assert!(near(bilinear(&h3, 0.75, 0.3), 3.0));
        assert_eq!(bilinear(&[vec![1.0]], 0.5, 0.5), None);
        assert_eq!(bilinear(&[vec![1.0, 2.0], vec![1.0]], 0.5, 0.5), None);
        assert_eq!(bilinear(&[], 0.5, 0.5), None);
    }

    #[test]
    fn an_inspection_tells_how_far_the_program_cuts_at_each_depth() {
        let program = "G21\nG90\nM3 S1000\nG0 X0 Y0 Z2\nG1 Z-0.5 F100\nG1 X10 Y0\nG1 X10 Y10\nG1 Z-1\nG1 X0 Y10\nG0 Z2\nG0 X20 Y0\nG1 X25 Y0\nM5\nM2\n";
        let p = run(json!({"kind": "inspect", "text": program})).unwrap();
        let r = &p.view["result"];
        assert_eq!(r["plunges"], 2);
        assert_eq!(r["levels_total"], 3);
        assert_eq!(r["levels"][0]["z_mm"], -1.0);
        assert_eq!(r["levels"][0]["length_mm"], 10.0);
        assert_eq!(r["levels"][1]["z_mm"], -0.5);
        assert_eq!(r["levels"][1]["moves"], 2);
        assert_eq!(r["levels"][1]["length_mm"], 20.0);
        // the last move cuts at Z2, in the air, and is a level of its own
        assert_eq!(r["levels"][2]["z_mm"], 2.0);
        assert_eq!(r["levels"][2]["length_mm"], 5.0);
        assert_eq!(r["air_moves"], 1);
        assert!(p.view.get("gcode").is_none());
        assert_eq!(r["bounds"]["min"]["z"], -1.0);
        assert_eq!(
            run(json!({"kind": "inspect"})).err().map(|e| e.code()),
            Some("bad_plugin_request")
        );
        let too_big = CamConfig {
            max_input_mib: 1.0,
            ..CamConfig::default()
        };
        let long = "G1 X1\n".repeat(300_000);
        let out =
            CamEngine::default().prepare("w1", &too_big, &json!({"kind": "inspect", "text": long}));
        assert_eq!(out.err().map(|e| e.code()), Some("input_too_large"));
    }

    #[test]
    fn a_drill_program_goes_tool_by_tool_with_a_pause_to_change_the_drill() {
        let p = run(json!({"kind": "drill", "files": [{"name": "a.drl", "text": DRILL}]})).unwrap();
        let g = gcode_of(&p);
        assert!(g.starts_with("(drill)\nG21\nG90\nG17\n"));
        assert!(g.contains("M0 (change to a 0.8 mm drill)"));
        assert!(g.contains("M0 (change to a 1 mm drill)"));
        assert!(g.find("0.8 mm").unwrap() < g.find("1 mm drill").unwrap());
        assert!(g.contains("G1 Z-1.8 F100"));
        assert!(g.trim_end().ends_with("M5\nM2"));
        assert_eq!(p.view["result"]["holes"], 3);
        assert_eq!(p.view["result"]["tools"][0]["holes"], 2);
        let none = run(json!({"kind": "drill", "files": [{"name": "a.drl", "text": DRILL}], "params": {"tool_change": "none"}})).unwrap();
        assert!(!gcode_of(&none).contains("M0"));
    }

    #[test]
    fn the_holes_of_a_tool_are_taken_nearest_first_and_the_offset_moves_them() {
        let text = "M48\nMETRIC\nT1C0.8\n%\nT1\nX30.0Y0.0\nX10.0Y0.0\nX20.0Y0.0\nM30\n";
        let p = run(json!({"kind": "drill", "files": [{"name": "a.drl", "text": text}], "params": {"offset_x_mm": 1}})).unwrap();
        let g = gcode_of(&p);
        let a = g.find("X11 Y0").unwrap();
        let b = g.find("X21 Y0").unwrap();
        let c = g.find("X31 Y0").unwrap();
        assert!(a < b && b < c, "{g}");
    }

    #[test]
    fn a_pecking_drill_goes_down_in_steps() {
        let p = run(json!({"kind": "drill", "files": [{"name": "a.drl", "text": DRILL}], "params": {"peck_mm": 0.9, "depth_mm": 1.8}})).unwrap();
        let g = gcode_of(&p);
        assert!(g.contains("G1 Z-0.9 F100") && g.contains("G1 Z-1.8 F100"));
    }

    #[test]
    fn the_limits_of_the_worker_refuse_a_request_and_never_cut_it_down() {
        let files = json!([{"name": "a.drl", "text": DRILL}]);
        for (params, why) in [
            (json!({"depth_mm": 5}), "depth"),
            (json!({"feed_mm_min": 9000}), "feed"),
            (json!({"spindle_rpm": 70000}), "rpm"),
            (json!({"safe_z_mm": 80}), "safe z"),
            (json!({"tool_change": "magic"}), "tool change"),
            (json!({"depth_mm": "deep"}), "type"),
            (json!({"peck_mm": 99}), "peck"),
        ] {
            let e = run(json!({"kind": "drill", "files": files, "params": params}))
                .err()
                .unwrap();
            assert_eq!(e.code(), "bad_params", "{why}");
            assert_eq!(e.status(), 422);
        }
        assert_eq!(
            run(json!({"kind": "drill", "files": files, "params": 5}))
                .err()
                .unwrap()
                .code(),
            "bad_params"
        );
        assert_eq!(
            run(json!({"kind": "drill", "files": [{"name": "a.gtl", "text": TRACK}]}))
                .err()
                .unwrap()
                .code(),
            "missing_layer"
        );
    }

    #[test]
    fn the_work_has_to_fit_the_travel_of_the_worker() {
        let cfg = CamConfig {
            travel: [Some(15.0), Some(100.0)],
            ..CamConfig::default()
        };
        let e = CamEngine::default()
            .prepare(
                "w1",
                &cfg,
                &json!({"kind": "drill", "files": [{"name": "a.drl", "text": DRILL}]}),
            )
            .err()
            .unwrap();
        assert_eq!(e.code(), "out_of_travel");
    }

    #[test]
    fn the_outline_of_a_rectangular_board_is_cut_in_passes_around_the_outside() {
        let p = run(json!({"kind": "outline", "files": [{"name": "a.gko", "text": OUTLINE}], "params": {"tool_dia_mm": 2, "depth_mm": 1.8, "pass_depth_mm": 0.6}})).unwrap();
        let g = gcode_of(&p);
        assert_eq!(p.view["result"]["passes"], 3);
        assert_eq!(p.view["result"]["board_mm"]["width"], 50.0);
        assert!(g.contains("G0 X-1 Y-1"), "{g}");
        assert!(g.contains("G1 Z-0.6 F100") && g.contains("G1 Z-1.2") && g.contains("G1 Z-1.8"));
        assert!(g.contains("G1 X51 Y-1"));
        // an outline that is not a rectangle, or no outline at all
        let l_shape = "%FSLAX24Y24*%\n%MOMM*%\n%ADD10C,0.1*%\nD10*\nX0Y0D02*\nX500000Y0D01*\nX500000Y200000D01*\nX250000Y200000D01*\nX250000Y400000D01*\nX0Y400000D01*\nX0Y0D01*\nM02*\n";
        assert_eq!(
            run(json!({"kind": "outline", "files": [{"name": "a.gko", "text": l_shape}]}))
                .err()
                .unwrap()
                .code(),
            "bad_params"
        );
        let diagonal =
            "%FSLAX24Y24*%\n%MOMM*%\n%ADD10C,0.1*%\nD10*\nX0Y0D02*\nX500000Y400000D01*\nM02*\n";
        assert_eq!(
            run(json!({"kind": "outline", "files": [{"name": "a.gko", "text": diagonal}]}))
                .err()
                .unwrap()
                .code(),
            "bad_params"
        );
        assert_eq!(
            run(json!({"kind": "outline", "files": [{"name": "a.gtl", "text": TRACK}]}))
                .err()
                .unwrap()
                .code(),
            "missing_layer"
        );
    }

    #[test]
    fn the_isolation_runs_around_a_track_at_the_distance_of_the_tool() {
        let p = run(json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": TRACK}], "params": {"tool_dia_mm": 0.2}})).unwrap();
        let g = gcode_of(&p);
        assert_eq!(p.view["result"]["loops"], 1);
        // a track 10 mm long and 0.2 wide: the path is a rounded rectangle about 0.2 mm out from its axis (0.1 of copper and 0.1 of tool radius)
        let length = p.view["result"]["cut_length_mm"].as_f64().unwrap();
        assert!((21.0..=26.0).contains(&length), "{length}");
        // no point of the path is closer to the axis of the track than the half width of its copper plus the radius of the tool
        let res = p.view["result"]["resolution_mm"].as_f64().unwrap();
        let (a, b) = ((10.0, 10.0), (20.0, 10.0));
        let mut points = 0;
        let _ = res;
        for line in g
            .lines()
            .filter(|l| l.starts_with("G1 X") || l.starts_with("G0 X"))
        {
            let x: f64 = line
                .split('X')
                .nth(1)
                .unwrap()
                .split(' ')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            let y: f64 = line
                .split('Y')
                .nth(1)
                .unwrap()
                .split(' ')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(dist_to_segment((x, y), a, b) >= 0.2 - 1e-6, "{line}");
            points += 1;
        }
        assert!(points >= 8, "{points}");
        assert!(g.contains("G1 Z-0.05 F100") && g.contains("G0 Z2"));
    }

    #[test]
    fn two_tracks_close_to_each_other_share_one_loop_and_pads_make_their_own() {
        let two = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.200*%\nD10*\nX100000Y100000D02*\nX200000Y100000D01*\nX100000Y103000D02*\nX200000Y103000D01*\nM02*\n";
        let p = run(json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": two}], "params": {"tool_dia_mm": 0.2}})).unwrap();
        assert_eq!(
            p.view["result"]["loops"], 1,
            "the two tracks are 0.3 mm apart: their loops touch and the tool goes round both"
        );
        let far = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.200*%\nD10*\nX100000Y100000D02*\nX200000Y100000D01*\nX100000Y200000D02*\nX200000Y200000D01*\nM02*\n";
        let q = run(json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": far}], "params": {"tool_dia_mm": 0.2}})).unwrap();
        assert_eq!(q.view["result"]["loops"], 2);
        let pad = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,1.0*%\nD10*\nX100000Y100000D03*\nM02*\n";
        let r = run(json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": pad}], "params": {"tool_dia_mm": 0.2, "passes": 2}})).unwrap();
        assert_eq!(r.view["result"]["loops"], 2, "one loop for each pass");
    }

    #[test]
    fn the_isolation_refuses_what_the_grid_would_leave_out() {
        let region = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.2*%\nD10*\nG36*\nX0Y0D02*\nX100000Y0D01*\nX100000Y100000D01*\nX0Y0D01*\nG37*\nM02*\n";
        assert_eq!(
            run(json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": region}]}))
                .err()
                .unwrap()
                .code(),
            "bad_params"
        );
        let macro_pad = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%AMOC8*5,1,8,0,0,1.08,22.5*%\n%ADD10OC8,0.5*%\nD10*\nX100000Y100000D03*\nM02*\n";
        assert_eq!(
            run(json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": macro_pad}]}))
                .err()
                .unwrap()
                .code(),
            "bad_params"
        );
        let tiny = CamConfig {
            max_cells: 10_000,
            ..CamConfig::default()
        };
        let big = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.2*%\nD10*\nX0Y0D02*\nX1000000Y1000000D01*\nM02*\n";
        let e = CamEngine::default()
            .prepare(
                "w1",
                &tiny,
                &json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": big}]}),
            )
            .err()
            .unwrap();
        assert_eq!(e.code(), "too_big");
        assert_eq!(run(json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": TRACK}], "layer": "nope"})).err().unwrap().code(), "missing_layer");
        assert_eq!(run(json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": TRACK}], "params": {"passes": 9}})).err().unwrap().code(), "bad_params");
    }

    #[test]
    fn a_program_is_verified_against_the_limits_of_the_worker() {
        let good = "G21 G90\nG0 Z2\nM3 S12000\nG0 X5 Y5\nG1 Z-1 F100\nG1 X10 F200\nG0 Z2\nM5\nM2\n";
        let p = run(json!({"kind": "verify", "text": good})).unwrap();
        assert_eq!(p.view["result"]["verdict"], "pass");
        assert_eq!(p.view["result"]["errors"], 0);
        assert!(p.view.get("gcode").is_none());
        assert_eq!(p.view["result"]["bounds"]["min"]["z"], -1.0);
        let bad = "G21 G90\nG1 X5 F9000\nG0 Z-1\nG1 Z-5 F100\nM3 S60000\n";
        let b = run(json!({"kind": "verify", "text": bad})).unwrap();
        assert_eq!(b.view["result"]["verdict"], "fail");
        let rules: Vec<String> = b.view["result"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["rule"].as_str().unwrap().to_string())
            .collect();
        for want in [
            "feed_over_limit",
            "rapid_into_material",
            "too_deep",
            "rpm_over_limit",
            "cut_with_spindle_off",
            "no_program_end",
            "spindle_left_on",
        ] {
            assert!(rules.iter().any(|r| r == want), "{want} in {rules:?}");
        }
        let cfg = CamConfig {
            travel: [Some(4.0), Some(4.0)],
            ..CamConfig::default()
        };
        let t = CamEngine::default()
            .prepare(
                "w1",
                &cfg,
                &json!({"kind": "verify", "text": "G1 X5 F100\nM2\n"}),
            )
            .unwrap();
        assert_eq!(t.view["result"]["verdict"], "fail");
        assert_eq!(
            run(json!({"kind": "verify"})).err().unwrap().code(),
            "bad_plugin_request"
        );
        assert_eq!(
            run(json!({"kind": "verify", "text": "$H"}))
                .err()
                .unwrap()
                .code(),
            "bad_gcode"
        );
        let big = "x".repeat(2 * 1024 * 1024);
        let small = CamConfig {
            max_input_mib: 1.0,
            ..CamConfig::default()
        };
        let e = CamEngine::default()
            .prepare("w1", &small, &json!({"kind": "verify", "text": big}))
            .err()
            .unwrap();
        assert_eq!(e.code(), "input_too_large");
    }

    #[test]
    fn a_job_that_made_too_many_lines_is_refused_and_an_unknown_kind_too() {
        let cfg = CamConfig {
            max_lines: 100,
            ..CamConfig::default()
        };
        let many = "G1 X1 F100\n".repeat(150);
        let e = CamEngine::default()
            .prepare("w1", &cfg, &json!({"kind": "verify", "text": many}))
            .err()
            .unwrap();
        assert_eq!(e.code(), "too_big");
        assert_eq!(
            run(json!({"kind": "weld"})).err().unwrap().code(),
            "bad_plugin_request"
        );
    }

    #[test]
    fn a_job_is_a_list_of_steps_the_simulator_goes_through() {
        let engine = CamEngine::default();
        let cfg = CamConfig::default();
        let p = engine
            .prepare(
                "w1",
                &cfg,
                &json!({"kind": "drill", "files": [{"name": "a.drl", "text": DRILL}]}),
            )
            .unwrap();
        assert_eq!(engine.book().peek(&p.native_id).unwrap().total, 3);
        engine.book().start("w1", &p.native_id).unwrap();
        assert_eq!(
            engine.book().status(&p.native_id).unwrap().state,
            WorkState::Done
        );
    }

    #[test]
    fn the_simplified_loop_keeps_its_corners() {
        let square = vec![
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (3.0, 0.0),
            (3.0, 1.0),
            (3.0, 2.0),
            (3.0, 3.0),
            (2.0, 3.0),
            (1.0, 3.0),
            (0.0, 3.0),
            (0.0, 2.0),
            (0.0, 1.0),
        ];
        let s = simplify(&square, 0.1);
        assert_eq!(s.len(), 4, "{s:?}");
        for corner in [(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)] {
            assert!(s.contains(&corner), "{corner:?}");
        }
    }
}
