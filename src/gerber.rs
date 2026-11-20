//! The Gerber and drill files read and checked, with no machine and no movement at all: the plugin `gerber` (`src/plugin_gerber.rs`) hands the workers of the server to this
//! engine. A job here is a report: `load` reads the files and lists the layers (function, units, box, apertures), `drc` goes over a basic set of rules (the width of the
//! tracks, the gap between pads, the size of the holes, the ring left around a hole, the distance to the edge of the board) and `report` gives both as text or as data.
//!
//! The readers are small on purpose: RS-274X with the format, the units, the apertures `C`, `R`, `O` and `P`, the draws, the flashes, the regions and the file function
//! (`%TF.FileFunction`), and Excellon with its tools and its hits. An arc is taken as the straight line between its ends, and an aperture made by a macro has no size, so the
//! checks that need a size do not look at it. Nothing here has been compared with another tool: the findings are a first filter, never the last word on a board.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::plugins::{PluginError, PluginResult, Prepared};
use crate::simjobs::{evenly, known_keys, num, number_in, round2, SimBook};

/// The most files a job may bring.
pub const MAX_FILES: usize = 64;
/// The most draws, flashes and holes a file may have.
pub const MAX_FEATURES: usize = 2_000_000;
/// The most pads whose gaps are compared two by two.
const CLEARANCE_PADS: usize = 3000;
/// The most tracks of one layer whose gaps to each other are checked (the check looks at every pair).
const CLEARANCE_TRACKS: usize = 2500;
/// The most findings one page of `findings_page` gives.
pub const MAX_PAGE: usize = 1000;

/// The settings a gerber worker may have: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("min_track_mm", "the narrowest track, 0.01 to 5 (0.15 by default)"),
    ("min_clearance_mm", "the least gap between two pads of one layer, 0.01 to 5 (0.15 by default)"),
    ("min_hole_mm", "the smallest hole, 0.1 to 10 (0.3 by default)"),
    ("min_annular_ring_mm", "the least ring of copper around a hole, 0.01 to 2 (0.15 by default)"),
    ("edge_distance_mm", "the least distance from a hole or a piece of copper to the edge of the board, 0 to 10 (0.3 by default)"),
    ("max_findings", "the most findings a report lists, 1 to 10000 (500 by default)"),
    ("max_input_mib", "the most text a job may bring, 1 to 64 MiB (64 by default)"),
    ("item_ms", "the milliseconds one check takes in the simulator, 0 to 3600000 (0 by default: the job ends as soon as it runs)"),
];

/// The rules a board is checked against.
#[derive(Debug, Clone, PartialEq)]
pub struct Rules {
    pub min_track_mm: f64,
    pub min_clearance_mm: f64,
    pub min_hole_mm: f64,
    pub min_annular_ring_mm: f64,
    pub edge_distance_mm: f64,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            min_track_mm: 0.15,
            min_clearance_mm: 0.15,
            min_hole_mm: 0.3,
            min_annular_ring_mm: 0.15,
            edge_distance_mm: 0.3,
        }
    }
}

/// The settings of a gerber worker, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct GerberConfig {
    pub rules: Rules,
    pub max_findings: usize,
    pub max_input_mib: f64,
    pub item_ms: f64,
}

impl Default for GerberConfig {
    fn default() -> Self {
        GerberConfig {
            rules: Rules::default(),
            max_findings: 500,
            max_input_mib: 64.0,
            item_ms: 0.0,
        }
    }
}

impl GerberConfig {
    pub fn from_settings(settings: &Value) -> Result<GerberConfig, String> {
        let keys: Vec<&str> = SETTINGS_HELP.iter().map(|(k, _)| *k).collect();
        known_keys(settings, &keys, "a gerber worker")?;
        let mut cfg = GerberConfig::default();
        if let Some(n) = number_in(settings, "min_track_mm", 0.01, 5.0)? {
            cfg.rules.min_track_mm = n;
        }
        if let Some(n) = number_in(settings, "min_clearance_mm", 0.01, 5.0)? {
            cfg.rules.min_clearance_mm = n;
        }
        if let Some(n) = number_in(settings, "min_hole_mm", 0.1, 10.0)? {
            cfg.rules.min_hole_mm = n;
        }
        if let Some(n) = number_in(settings, "min_annular_ring_mm", 0.01, 2.0)? {
            cfg.rules.min_annular_ring_mm = n;
        }
        if let Some(n) = number_in(settings, "edge_distance_mm", 0.0, 10.0)? {
            cfg.rules.edge_distance_mm = n;
        }
        if let Some(n) = number_in(settings, "max_findings", 1.0, 10000.0)? {
            cfg.max_findings = n as usize;
        }
        if let Some(n) = number_in(settings, "max_input_mib", 1.0, 64.0)? {
            cfg.max_input_mib = n;
        }
        if let Some(n) = number_in(settings, "item_ms", 0.0, 3_600_000.0)? {
            cfg.item_ms = n;
        }
        Ok(cfg)
    }
}

/// A refusal of the reading of a job: one of the codes of the catalog of errors and what is wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct GerberError {
    pub code: &'static str,
    pub detail: String,
}

impl From<GerberError> for PluginError {
    fn from(e: GerberError) -> PluginError {
        PluginError::Coded(e.code, e.detail)
    }
}

// ---- the readers ---------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Circle(f64),
    Rect(f64, f64),
    /// A macro or a shape with no size known here.
    Unknown,
}

impl Shape {
    /// The width and the height of the box around the shape, in mm.
    pub fn size(self) -> Option<(f64, f64)> {
        match self {
            Shape::Circle(d) => Some((d, d)),
            Shape::Rect(w, h) => Some((w, h)),
            Shape::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub from: (f64, f64),
    pub to: (f64, f64),
    pub width: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flash {
    pub at: (f64, f64),
    pub shape: Shape,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hole {
    pub at: (f64, f64),
    pub diameter: f64,
    pub plated: bool,
}

/// One file that was read.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub name: String,
    /// `gerber` or `excellon`.
    pub format: &'static str,
    pub function: String,
    pub units: &'static str,
    pub bbox: Option<[f64; 4]>,
    pub apertures: usize,
    pub tracks: Vec<Track>,
    pub flashes: Vec<Flash>,
    pub regions: usize,
    pub holes: Vec<Hole>,
    pub tools: Vec<f64>,
}

impl Layer {
    fn new(name: &str, format: &'static str) -> Layer {
        Layer {
            name: name.to_string(),
            format,
            function: "unknown".into(),
            units: "mm",
            bbox: None,
            apertures: 0,
            tracks: Vec::new(),
            flashes: Vec::new(),
            regions: 0,
            holes: Vec::new(),
            tools: Vec::new(),
        }
    }

    fn grow(&mut self, x0: f64, y0: f64, x1: f64, y1: f64) {
        self.bbox = Some(match self.bbox {
            None => [x0, y0, x1, y1],
            Some([a, b, c, d]) => [a.min(x0), b.min(y0), c.max(x1), d.max(y1)],
        });
    }

    pub fn is_copper(&self) -> bool {
        self.function.starts_with("copper")
    }
}

fn bad(name: &str, why: impl std::fmt::Display) -> GerberError {
    GerberError {
        code: "bad_gerber",
        detail: format!("{name}: {why}"),
    }
}

/// The function of a file from its name when the file does not say.
fn function_from_name(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    let ext = n.rsplit('.').next().unwrap_or("");
    let has = |parts: &[&str]| parts.iter().any(|p| n.contains(p));
    if has(&["edge_cuts", "edge.cuts", "outline", "profile"])
        || matches!(ext, "gko" | "gm1" | "gml")
    {
        "outline".into()
    } else if has(&["f_cu", "top_copper", "-top.", "_top."]) || ext == "gtl" {
        "copper_top".into()
    } else if has(&["b_cu", "bottom_copper", "-bottom.", "_bottom."]) || ext == "gbl" {
        "copper_bottom".into()
    } else if has(&["in1_cu", "in2_cu", "inner"])
        || (ext.len() == 2 && ext.starts_with('g') && ext[1..].parse::<u8>().is_ok())
    {
        "copper_inner".into()
    } else if has(&["mask"]) || matches!(ext, "gts" | "gbs") {
        "soldermask".into()
    } else if has(&["silk", "legend"]) || matches!(ext, "gto" | "gbo") {
        "silkscreen".into()
    } else if has(&["paste"]) || matches!(ext, "gtp" | "gbp") {
        "paste".into()
    } else {
        "unknown".into()
    }
}

/// The function from `%TF.FileFunction,Copper,L1,Top` (the fields after the name of the attribute).
fn function_from_attribute(fields: &[&str]) -> Option<String> {
    let first = fields.first()?.to_ascii_lowercase();
    let side = fields
        .iter()
        .map(|f| f.to_ascii_lowercase())
        .find(|f| matches!(f.as_str(), "top" | "bot" | "bottom" | "inr"));
    Some(match first.as_str() {
        "copper" => match side.as_deref() {
            Some("top") => "copper_top".into(),
            Some("bot" | "bottom") => "copper_bottom".into(),
            _ => "copper_inner".into(),
        },
        "profile" => "outline".into(),
        "soldermask" => "soldermask".into(),
        "legend" => "silkscreen".into(),
        "paste" => "paste".into(),
        _ => return None,
    })
}

/// The number of a coordinate: `digits` as written, `decimals` after the point, trailing zeros omitted when `trailing` is set (and then padded to `total` digits).
fn coordinate(text: &str, decimals: usize, total: usize, trailing: bool) -> Option<f64> {
    if text.contains('.') {
        return text.parse().ok();
    }
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut digits = digits.to_string();
    while trailing && digits.len() < total {
        digits.push('0');
    }
    let value: f64 = digits.parse().ok()?;
    let value = value / 10f64.powi(decimals as i32);
    Some(if negative { -value } else { value })
}

/// Splits `X123Y-45I6J7` into `(letter, text)` pairs.
fn letters(code: &str) -> Vec<(char, String)> {
    let mut out: Vec<(char, String)> = Vec::new();
    for c in code.chars() {
        if c.is_ascii_alphabetic() {
            out.push((c.to_ascii_uppercase(), String::new()));
        } else if let Some(last) = out.last_mut() {
            last.1.push(c);
        }
    }
    out
}

struct GerberState {
    int_digits: usize,
    decimals: usize,
    trailing: bool,
    scale: Option<f64>,
    apertures: BTreeMap<u32, Shape>,
    current: Option<u32>,
    at: (f64, f64),
    clear: bool,
    region: bool,
    region_points: usize,
}

/// Reads a Gerber file (RS-274X). `name` is the name of the file as the job calls it.
pub fn read_gerber(name: &str, text: &str) -> Result<Layer, GerberError> {
    let mut layer = Layer::new(name, "gerber");
    layer.function = function_from_name(name);
    let mut st = GerberState {
        int_digits: 2,
        decimals: 4,
        trailing: false,
        scale: None,
        apertures: BTreeMap::new(),
        current: None,
        at: (0.0, 0.0),
        clear: false,
        region: false,
        region_points: 0,
    };
    let mut ended = false;
    let mut coords_seen = false;
    let mut blocks: Vec<(bool, String)> = Vec::new();
    let (mut extended, mut buf) = (false, String::new());
    for c in text.chars() {
        match c {
            '%' => {
                if extended {
                    if !buf.trim().is_empty() {
                        blocks.push((true, std::mem::take(&mut buf)));
                    }
                    buf.clear();
                }
                extended = !extended;
            }
            '*' if !extended => blocks.push((false, std::mem::take(&mut buf))),
            '\n' | '\r' if !extended => {}
            c => buf.push(c),
        }
    }
    if extended {
        return Err(bad(name, "a % command is not closed"));
    }
    for (is_extended, block) in blocks {
        if is_extended {
            for cmd in block.split('*').map(str::trim).filter(|c| !c.is_empty()) {
                extended_command(name, cmd, &mut st, &mut layer)?;
            }
            continue;
        }
        let code: String = block.chars().filter(|c| !c.is_whitespace()).collect();
        if code.is_empty() || code.starts_with("G04") {
            continue;
        }
        if code == "M02" || code == "M00" {
            ended = true;
            continue;
        }
        let (mut draw, mut op) = (false, None);
        let (mut x, mut y) = (None, None);
        let mut mode_g = None;
        for (letter, value) in letters(&code) {
            match letter {
                'G' => {
                    let g: u32 = value.parse().unwrap_or(u32::MAX);
                    match g {
                        36 => {
                            st.region = true;
                            st.region_points = 0;
                        }
                        37 => {
                            if st.region && st.region_points > 0 {
                                layer.regions += 1;
                            }
                            st.region = false;
                        }
                        1..=3 => mode_g = Some(g),
                        _ => {}
                    }
                }
                'X' | 'Y' | 'I' | 'J' => {
                    coords_seen = true;
                    let v = coordinate(
                        &value,
                        st.decimals,
                        st.int_digits + st.decimals,
                        st.trailing,
                    )
                    .ok_or_else(|| bad(name, format!("`{letter}{value}` is not a coordinate")))?;
                    match letter {
                        'X' => x = Some(v),
                        'Y' => y = Some(v),
                        _ => {}
                    }
                }
                'D' => {
                    let d: u32 = value
                        .parse()
                        .map_err(|_| bad(name, format!("`D{value}` is not a D code")))?;
                    match d {
                        1 => {
                            draw = true;
                            op = Some(1);
                        }
                        2 => op = Some(2),
                        3 => op = Some(3),
                        n if n >= 10 => st.current = Some(n),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        let _ = mode_g;
        let _ = draw;
        let Some(op) = op else {
            // coordinates with no D code repeat the last operation of some old files; they are not followed
            continue;
        };
        let scale = match st.scale {
            Some(s) => s,
            None if coords_seen => {
                return Err(GerberError {
                    code: "unit_unknown",
                    detail: format!("{name}: the file uses coordinates before it says its units (%MOMM*% or %MOIN*%)"),
                })
            }
            None => 1.0,
        };
        let to = (
            x.map_or(st.at.0, |v| v * scale),
            y.map_or(st.at.1, |v| v * scale),
        );
        let width = st
            .current
            .and_then(|n| st.apertures.get(&n))
            .copied()
            .unwrap_or(Shape::Unknown);
        match op {
            1 => {
                if st.region {
                    st.region_points += 1;
                    layer.grow(
                        st.at.0.min(to.0),
                        st.at.1.min(to.1),
                        st.at.0.max(to.0),
                        st.at.1.max(to.1),
                    );
                } else if let Some((w, _)) = width.size() {
                    layer.grow(
                        st.at.0.min(to.0) - w / 2.0,
                        st.at.1.min(to.1) - w / 2.0,
                        st.at.0.max(to.0) + w / 2.0,
                        st.at.1.max(to.1) + w / 2.0,
                    );
                    if !st.clear {
                        layer.tracks.push(Track {
                            from: st.at,
                            to,
                            width: w,
                        });
                    }
                } else {
                    layer.grow(
                        st.at.0.min(to.0),
                        st.at.1.min(to.1),
                        st.at.0.max(to.0),
                        st.at.1.max(to.1),
                    );
                }
            }
            3 => {
                match width.size() {
                    Some((w, h)) => layer.grow(
                        to.0 - w / 2.0,
                        to.1 - h / 2.0,
                        to.0 + w / 2.0,
                        to.1 + h / 2.0,
                    ),
                    None => layer.grow(to.0, to.1, to.0, to.1),
                }
                if !st.clear {
                    layer.flashes.push(Flash {
                        at: to,
                        shape: width,
                    });
                }
            }
            _ => {
                if st.region {
                    st.region_points += 1;
                }
            }
        }
        st.at = to;
        if layer.tracks.len() + layer.flashes.len() > MAX_FEATURES {
            return Err(bad(name, "too many draws and flashes"));
        }
    }
    if !ended && layer.tracks.is_empty() && layer.flashes.is_empty() && layer.regions == 0 {
        return Err(bad(name, "there is no Gerber data in the file"));
    }
    layer.apertures = st.apertures.len();
    layer.units = if st.scale == Some(25.4) { "in" } else { "mm" };
    Ok(layer)
}

fn extended_command(
    name: &str,
    cmd: &str,
    st: &mut GerberState,
    layer: &mut Layer,
) -> Result<(), GerberError> {
    if let Some(rest) = cmd.strip_prefix("FS") {
        let letters = rest.to_ascii_uppercase();
        st.trailing = letters.contains('T');
        let x = letters
            .find('X')
            .ok_or_else(|| bad(name, "the format has no X"))?;
        let digits: Vec<char> = letters[x + 1..].chars().take(2).collect();
        if digits.len() == 2 && digits.iter().all(char::is_ascii_digit) {
            st.int_digits = digits[0].to_digit(10).unwrap_or(2) as usize;
            st.decimals = digits[1].to_digit(10).unwrap_or(4) as usize;
        } else {
            return Err(bad(name, "the coordinate format is not like FSLAX24Y24"));
        }
    } else if let Some(rest) = cmd.strip_prefix("MO") {
        st.scale = match rest {
            "MM" => Some(1.0),
            "IN" => Some(25.4),
            _ => return Err(bad(name, "the units are MM or IN")),
        };
    } else if let Some(rest) = cmd.strip_prefix("AD") {
        // ADD10C,0.2   ADD11R,0.5X0.6   ADD12O,0.5X0.6   ADD13P,1.0X6   ADD14NAME,...
        let rest = rest.strip_prefix('D').unwrap_or(rest);
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let number: u32 = digits
            .parse()
            .map_err(|_| bad(name, "an aperture has no number"))?;
        let body = &rest[digits.len()..];
        let (shape_name, args) = body.split_once(',').unwrap_or((body, ""));
        let scale = st.scale.unwrap_or(1.0);
        let values: Vec<f64> = args
            .split('X')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        let shape = match (shape_name, values.as_slice()) {
            ("C", [d, ..]) => Shape::Circle(d * scale),
            ("R" | "O", [w, h, ..]) => Shape::Rect(w * scale, h * scale),
            ("P", [d, ..]) => Shape::Circle(d * scale),
            _ => Shape::Unknown,
        };
        st.apertures.insert(number, shape);
    } else if let Some(rest) = cmd.strip_prefix("TF.") {
        let fields: Vec<&str> = rest.split(',').collect();
        if fields.first() == Some(&"FileFunction") {
            if let Some(f) = function_from_attribute(&fields[1..]) {
                layer.function = f;
            }
        }
    } else if cmd.starts_with("LPC") {
        st.clear = true;
    } else if cmd.starts_with("LPD") {
        st.clear = false;
    }
    Ok(())
}

/// Reads an Excellon drill file.
pub fn read_excellon(name: &str, text: &str) -> Result<Layer, GerberError> {
    let mut layer = Layer::new(name, "excellon");
    let lower = name.to_ascii_lowercase();
    layer.function = if lower.contains("npth") || lower.contains("nonplated") {
        "drill_nonplated".into()
    } else {
        "drill_plated".into()
    };
    let mut scale: Option<f64> = None;
    let (mut trailing, mut inch_default) = (false, false);
    let mut tools: BTreeMap<u32, f64> = BTreeMap::new();
    let mut current: Option<u32> = None;
    let mut in_header = false;
    let mut hits = 0usize;
    let mut ended = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix(';') {
            let c = comment.to_ascii_lowercase();
            if c.contains("file_function") || c.contains("filefunction") {
                if c.contains("nonplated") || c.contains("npth") {
                    layer.function = "drill_nonplated".into();
                } else if c.contains("plated") || c.contains("pth") {
                    layer.function = "drill_plated".into();
                }
            }
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper == "M48" {
            in_header = true;
            continue;
        }
        if upper == "%" || upper == "M95" {
            in_header = false;
            continue;
        }
        if upper == "M30" || upper == "M00" {
            ended = true;
            continue;
        }
        if upper.starts_with("METRIC") || upper.starts_with("INCH") {
            let inch = upper.starts_with("INCH");
            scale = Some(if inch { 25.4 } else { 1.0 });
            inch_default = inch;
            trailing = upper.contains(",TZ");
            continue;
        }
        if upper.starts_with('T') && upper[1..].starts_with(|c: char| c.is_ascii_digit()) {
            let digits: String = upper[1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            let number: u32 = digits.parse().unwrap_or(0);
            let rest = &upper[1 + digits.len()..];
            if let Some(c) = rest.strip_prefix('C') {
                let size: String = c
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                let size: f64 = size
                    .parse()
                    .map_err(|_| bad(name, format!("the tool T{number} has no diameter")))?;
                let s = scale.ok_or_else(|| GerberError {
                    code: "unit_unknown",
                    detail: format!("{name}: a tool is defined before the units (METRIC or INCH)"),
                })?;
                tools.insert(number, size * s);
            } else if !in_header || rest.is_empty() {
                current = if number == 0 { None } else { Some(number) };
            }
            continue;
        }
        if upper.starts_with('X') || upper.starts_with('Y') {
            if upper.contains("G85") {
                continue;
            }
            let Some(s) = scale else {
                return Err(GerberError {
                    code: "unit_unknown",
                    detail: format!(
                        "{name}: the file has holes before it says its units (METRIC or INCH)"
                    ),
                });
            };
            let Some(tool) = current.and_then(|n| tools.get(&n)).copied() else {
                return Err(bad(name, "a hole comes before any tool is selected"));
            };
            let (decimals, total) = if inch_default { (4, 6) } else { (3, 6) };
            let (mut x, mut y) = (None, None);
            for (letter, value) in letters(&upper) {
                let v = coordinate(&value, decimals, total, trailing)
                    .ok_or_else(|| bad(name, format!("`{letter}{value}` is not a coordinate")))?;
                match letter {
                    'X' => x = Some(v * s),
                    'Y' => y = Some(v * s),
                    _ => {}
                }
            }
            let (Some(x), Some(y)) = (x, y) else {
                return Err(bad(name, "a hole has no X or no Y"));
            };
            layer.grow(
                x - tool / 2.0,
                y - tool / 2.0,
                x + tool / 2.0,
                y + tool / 2.0,
            );
            layer.holes.push(Hole {
                at: (x, y),
                diameter: tool,
                plated: layer.function != "drill_nonplated",
            });
            hits += 1;
            if hits > MAX_FEATURES {
                return Err(bad(name, "too many holes"));
            }
        }
    }
    if hits == 0 && tools.is_empty() && !ended {
        return Err(bad(name, "there is no Excellon data in the file"));
    }
    layer.units = if inch_default { "in" } else { "mm" };
    layer.tools = tools.values().copied().collect();
    layer.apertures = tools.len();
    Ok(layer)
}

/// Whether the text is a drill file.
fn looks_like_excellon(name: &str, text: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".drl") || n.ends_with(".xln") || n.ends_with(".exc") || n.ends_with(".ncd") {
        return true;
    }
    let head: String = text.chars().take(600).collect();
    let h = head.to_ascii_uppercase();
    (h.contains("M48") || h.contains("METRIC") || h.contains("INCH,")) && !h.contains("%FS")
}

/// Reads every file of a job.
pub fn read_files(files: &[(String, String)]) -> Result<Vec<Layer>, GerberError> {
    if files.is_empty() {
        return Err(GerberError {
            code: "missing_layer",
            detail: "the job brings no file".into(),
        });
    }
    files
        .iter()
        .map(|(name, text)| {
            if looks_like_excellon(name, text) {
                read_excellon(name, text)
            } else {
                read_gerber(name, text)
            }
        })
        .collect()
}

// ---- the checks ----------------------------------------------------------------------------------------------------------------------

/// What the checks found.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: &'static str,
    pub layer: String,
    pub at: (f64, f64),
    pub value: f64,
    pub limit: f64,
    pub message: String,
}

impl Finding {
    fn view(&self) -> Value {
        json!({
            "rule": self.rule, "severity": self.severity, "layer": self.layer,
            "x": round2(self.at.0), "y": round2(self.at.1),
            "value": (self.value * 1000.0).round() / 1000.0, "limit": self.limit,
            "message": self.message,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drc {
    pub findings: Vec<Finding>,
    pub total: usize,
    pub truncated: bool,
}

impl Drc {
    pub fn errors(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == "error")
            .count()
    }

    pub fn verdict(&self) -> &'static str {
        if self.errors() == 0 {
            "pass"
        } else {
            "fail"
        }
    }
}

fn outline_box(layers: &[Layer]) -> Option<[f64; 4]> {
    layers
        .iter()
        .filter(|l| l.function == "outline")
        .filter_map(|l| l.bbox)
        .reduce(|a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        })
}

fn pad_gap(a: &Flash, b: &Flash) -> Option<f64> {
    let (wa, ha) = a.shape.size()?;
    let (wb, hb) = b.shape.size()?;
    if let (Shape::Circle(da), Shape::Circle(db)) = (a.shape, b.shape) {
        let d = (a.at.0 - b.at.0).hypot(a.at.1 - b.at.1);
        return Some(d - da / 2.0 - db / 2.0);
    }
    let dx = ((a.at.0 - b.at.0).abs() - (wa + wb) / 2.0).max(0.0);
    let dy = ((a.at.1 - b.at.1).abs() - (ha + hb) / 2.0).max(0.0);
    Some(if dx == 0.0 && dy == 0.0 {
        -1.0
    } else {
        dx.hypot(dy)
    })
}

fn orient(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

fn point_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f64::EPSILON {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// The distance between the center lines of two tracks (0 when they cross or touch).
fn center_distance(a: &Track, b: &Track) -> f64 {
    let crosses = orient(a.from, a.to, b.from) * orient(a.from, a.to, b.to) < 0.0
        && orient(b.from, b.to, a.from) * orient(b.from, b.to, a.to) < 0.0;
    if crosses {
        return 0.0;
    }
    [
        point_segment(a.from, b.from, b.to),
        point_segment(a.to, b.from, b.to),
        point_segment(b.from, a.from, a.to),
        point_segment(b.to, a.from, a.to),
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min)
}

/// The copper-to-copper gap between two tracks of one layer: the distance of the center lines less half of each width. Zero or less when they touch or cross (tracks that
/// meet are taken as parts of one net: the check has no nets, so it cannot tell a short from a join).
pub fn track_gap(a: &Track, b: &Track) -> f64 {
    center_distance(a, b) - a.width / 2.0 - b.width / 2.0
}

fn track_box(t: &Track) -> [f64; 4] {
    let r = t.width / 2.0;
    [
        t.from.0.min(t.to.0) - r,
        t.from.1.min(t.to.1) - r,
        t.from.0.max(t.to.0) + r,
        t.from.1.max(t.to.1) + r,
    ]
}

/// Goes over the layers against the rules. Findings past `max_findings` are counted and not kept.
pub fn run_drc(layers: &[Layer], rules: &Rules, max_findings: usize) -> Drc {
    let mut out = Drc {
        findings: Vec::new(),
        total: 0,
        truncated: false,
    };
    let push = |out: &mut Drc, f: Finding| {
        out.total += 1;
        if out.findings.len() < max_findings {
            out.findings.push(f);
        } else {
            out.truncated = true;
        }
    };
    let outline = outline_box(layers);
    if outline.is_none() {
        push(
            &mut out,
            Finding {
                rule: "no_outline",
                severity: "warning",
                layer: String::new(),
                at: (0.0, 0.0),
                value: 0.0,
                limit: 0.0,
                message:
                    "no layer is the outline of the board: the distance to the edge is not checked"
                        .into(),
            },
        );
    }
    for layer in layers.iter().filter(|l| l.is_copper()) {
        for t in &layer.tracks {
            if t.width < rules.min_track_mm - 1e-9 {
                push(
                    &mut out,
                    Finding {
                        rule: "track_width",
                        severity: "error",
                        layer: layer.name.clone(),
                        at: t.from,
                        value: t.width,
                        limit: rules.min_track_mm,
                        message: format!(
                            "a track is {} mm wide, the least is {} mm",
                            num(t.width),
                            num(rules.min_track_mm)
                        ),
                    },
                );
            }
        }
        if layer.flashes.len() <= CLEARANCE_PADS {
            for (i, a) in layer.flashes.iter().enumerate() {
                for b in &layer.flashes[i + 1..] {
                    if let Some(gap) = pad_gap(a, b) {
                        if gap > 1e-9 && gap < rules.min_clearance_mm - 1e-9 {
                            push(
                                &mut out,
                                Finding {
                                    rule: "clearance",
                                    severity: "error",
                                    layer: layer.name.clone(),
                                    at: a.at,
                                    value: gap,
                                    limit: rules.min_clearance_mm,
                                    message: format!(
                                        "two pads are {} mm apart, the least is {} mm",
                                        num(gap),
                                        num(rules.min_clearance_mm)
                                    ),
                                },
                            );
                        }
                    }
                }
            }
        } else {
            push(
                &mut out,
                Finding {
                    rule: "clearance_skipped",
                    severity: "warning",
                    layer: layer.name.clone(),
                    at: (0.0, 0.0),
                    value: layer.flashes.len() as f64,
                    limit: CLEARANCE_PADS as f64,
                    message: "the layer has too many pads for the gap between them to be checked"
                        .into(),
                },
            );
        }
        if layer.tracks.len() <= CLEARANCE_TRACKS {
            let gap = rules.min_clearance_mm;
            let boxes: Vec<[f64; 4]> = layer.tracks.iter().map(track_box).collect();
            for (i, a) in layer.tracks.iter().enumerate() {
                for (j, b) in layer.tracks.iter().enumerate().skip(i + 1) {
                    let (p, q) = (boxes[i], boxes[j]);
                    // tracks whose boxes are farther apart than the gap cannot be closer than it
                    if p[2] + gap < q[0]
                        || q[2] + gap < p[0]
                        || p[3] + gap < q[1]
                        || q[3] + gap < p[1]
                    {
                        continue;
                    }
                    let g = track_gap(a, b);
                    if g > 1e-9 && g < gap - 1e-9 {
                        push(
                            &mut out,
                            Finding {
                                rule: "track_clearance",
                                severity: "error",
                                layer: layer.name.clone(),
                                at: a.from,
                                value: g,
                                limit: gap,
                                message: format!(
                                    "two tracks are {} mm apart, the least is {} mm",
                                    num(g),
                                    num(gap)
                                ),
                            },
                        );
                    }
                }
            }
        } else {
            push(
                &mut out,
                Finding {
                    rule: "track_clearance_skipped",
                    severity: "warning",
                    layer: layer.name.clone(),
                    at: (0.0, 0.0),
                    value: layer.tracks.len() as f64,
                    limit: CLEARANCE_TRACKS as f64,
                    message: "the layer has too many tracks for the gap between them to be checked"
                        .into(),
                },
            );
        }
        if let (Some([x0, y0, x1, y1]), Some(b)) = (layer.bbox, outline) {
            let inside = [x0 - b[0], y0 - b[1], b[2] - x1, b[3] - y1];
            let least = inside.iter().copied().fold(f64::INFINITY, f64::min);
            if least < rules.edge_distance_mm - 1e-9 {
                push(
                    &mut out,
                    Finding {
                        rule: "copper_edge",
                        severity: "error",
                        layer: layer.name.clone(),
                        at: (x0, y0),
                        value: least,
                        limit: rules.edge_distance_mm,
                        message: format!(
                            "the copper comes within {} mm of the edge, the least is {} mm",
                            num(least),
                            num(rules.edge_distance_mm)
                        ),
                    },
                );
            }
        }
    }
    let copper_pads: Vec<&Flash> = layers
        .iter()
        .filter(|l| l.is_copper())
        .flat_map(|l| l.flashes.iter())
        .collect();
    for layer in layers.iter().filter(|l| l.format == "excellon") {
        for h in &layer.holes {
            if h.diameter < rules.min_hole_mm - 1e-9 {
                push(
                    &mut out,
                    Finding {
                        rule: "hole_small",
                        severity: "error",
                        layer: layer.name.clone(),
                        at: h.at,
                        value: h.diameter,
                        limit: rules.min_hole_mm,
                        message: format!(
                            "a hole is {} mm, the least is {} mm",
                            num(h.diameter),
                            num(rules.min_hole_mm)
                        ),
                    },
                );
            }
            if let Some(b) = outline {
                let r = h.diameter / 2.0;
                let inside = [
                    h.at.0 - r - b[0],
                    h.at.1 - r - b[1],
                    b[2] - h.at.0 - r,
                    b[3] - h.at.1 - r,
                ];
                let least = inside.iter().copied().fold(f64::INFINITY, f64::min);
                if least < 0.0 {
                    push(
                        &mut out,
                        Finding {
                            rule: "hole_outside",
                            severity: "error",
                            layer: layer.name.clone(),
                            at: h.at,
                            value: least,
                            limit: 0.0,
                            message: "a hole is not inside the outline of the board".into(),
                        },
                    );
                } else if least < rules.edge_distance_mm - 1e-9 {
                    push(
                        &mut out,
                        Finding {
                            rule: "hole_edge",
                            severity: "error",
                            layer: layer.name.clone(),
                            at: h.at,
                            value: least,
                            limit: rules.edge_distance_mm,
                            message: format!(
                                "a hole comes within {} mm of the edge, the least is {} mm",
                                num(least),
                                num(rules.edge_distance_mm)
                            ),
                        },
                    );
                }
            }
            if h.plated {
                let pad = copper_pads.iter().find(|p| {
                    (p.at.0 - h.at.0).abs() < 0.01
                        && (p.at.1 - h.at.1).abs() < 0.01
                        && p.shape.size().is_some()
                });
                if let Some((w, hh)) = pad.and_then(|p| p.shape.size()) {
                    let ring = w.min(hh) / 2.0 - h.diameter / 2.0;
                    if ring < rules.min_annular_ring_mm - 1e-9 {
                        push(
                            &mut out,
                            Finding {
                                rule: "annular_ring",
                                severity: "error",
                                layer: layer.name.clone(),
                                at: h.at,
                                value: ring,
                                limit: rules.min_annular_ring_mm,
                                message: format!(
                                    "the ring around a hole is {} mm, the least is {} mm",
                                    num(ring),
                                    num(rules.min_annular_ring_mm)
                                ),
                            },
                        );
                    }
                }
            }
        }
    }
    out
}

/// The rules of the request laid over the ones of the worker.
pub fn rules_of(request: &Value, base: &Rules) -> Result<Rules, GerberError> {
    let mut rules = base.clone();
    let Some(over) = request.get("rules") else {
        return Ok(rules);
    };
    let wrong = |why: String| GerberError {
        code: "bad_rules",
        detail: why,
    };
    let map = over
        .as_object()
        .ok_or_else(|| wrong("rules is an object".into()))?;
    for (key, value) in map {
        let (slot, low, high) = match key.as_str() {
            "min_track_mm" => (&mut rules.min_track_mm, 0.01, 5.0),
            "min_clearance_mm" => (&mut rules.min_clearance_mm, 0.01, 5.0),
            "min_hole_mm" => (&mut rules.min_hole_mm, 0.1, 10.0),
            "min_annular_ring_mm" => (&mut rules.min_annular_ring_mm, 0.01, 2.0),
            "edge_distance_mm" => (&mut rules.edge_distance_mm, 0.0, 10.0),
            _ => return Err(wrong(format!("{key} is not a rule"))),
        };
        match value.as_f64() {
            Some(n) if n.is_finite() && (low..=high).contains(&n) => *slot = n,
            _ => return Err(wrong(format!("{key} is a number from {low} to {high}"))),
        }
    }
    Ok(rules)
}

/// The files of a request: `files` (name and text of each) or the text of an artifact.
pub fn files_of(request: &Value, max_input_mib: f64) -> Result<Vec<(String, String)>, GerberError> {
    let mut files: Vec<(String, String)> = Vec::new();
    if let Some(list) = request.get("files") {
        let list = list.as_array().ok_or_else(|| GerberError {
            code: "bad_gerber",
            detail: "files is a list of {name, text}".into(),
        })?;
        if list.len() > MAX_FILES {
            return Err(GerberError {
                code: "bad_gerber",
                detail: format!("a job may bring {MAX_FILES} files at the most"),
            });
        }
        for f in list {
            let name = f.get("name").and_then(Value::as_str);
            let text = f.get("text").and_then(Value::as_str);
            match (name, text) {
                (Some(n), Some(t)) if !n.is_empty() && n.len() <= 120 => {
                    files.push((n.to_string(), t.to_string()))
                }
                _ => {
                    return Err(GerberError {
                        code: "bad_gerber",
                        detail: "every file has a `name` and a `text`".into(),
                    })
                }
            }
        }
    } else if let Some(text) = request.get("artifact_text").and_then(Value::as_str) {
        let name = request
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| request.get("artifact").and_then(Value::as_str))
            .unwrap_or("artifact.gbr");
        files.push((name.to_string(), text.to_string()));
    }
    let bytes: usize = files.iter().map(|(_, t)| t.len()).sum();
    if bytes as f64 > max_input_mib * 1024.0 * 1024.0 {
        return Err(GerberError {
            code: "input_too_large",
            detail: format!(
                "the files are over {} MiB (max_input_mib)",
                num(max_input_mib)
            ),
        });
    }
    Ok(files)
}

/// A page of the findings of a finished check (`view.findings` of the job): filtered by `severity`, `rule` and `layer` (a filter that is `None` lets everything pass),
/// from `offset`, at most `limit` (1 to `MAX_PAGE`). Gives the findings of the page, how many matched and whether there are more.
pub fn findings_page(
    findings: &Value,
    severity: Option<&str>,
    rule: Option<&str>,
    layer: Option<&str>,
    offset: usize,
    limit: usize,
) -> Value {
    let limit = limit.clamp(1, MAX_PAGE);
    let empty = Vec::new();
    let all = findings.as_array().unwrap_or(&empty);
    let matching: Vec<&Value> = all
        .iter()
        .filter(|f| severity.is_none_or(|s| f["severity"] == s))
        .filter(|f| rule.is_none_or(|r| f["rule"] == r))
        .filter(|f| layer.is_none_or(|l| f["layer"] == l))
        .collect();
    let page: Vec<&Value> = matching.iter().skip(offset).take(limit).copied().collect();
    json!({
        "findings": page,
        "matched": matching.len(),
        "offset": offset,
        "limit": limit,
        "more": offset + page.len() < matching.len(),
    })
}

fn layers_view(layers: &[Layer]) -> Vec<Value> {
    layers
        .iter()
        .map(|l| {
            json!({
                "name": l.name, "format": l.format, "function": l.function, "units": l.units,
                "bbox_mm": l.bbox.map(|[a, b, c, d]| json!({"min_x": round2(a), "min_y": round2(b), "max_x": round2(c), "max_y": round2(d)})),
                "apertures": l.apertures, "tracks": l.tracks.len(), "flashes": l.flashes.len(),
                "regions": l.regions, "holes": l.holes.len(), "tools_mm": l.tools,
            })
        })
        .collect()
}

fn board_box(layers: &[Layer]) -> Option<Value> {
    let b = outline_box(layers).or_else(|| {
        layers
            .iter()
            .filter(|l| l.is_copper())
            .filter_map(|l| l.bbox)
            .reduce(|a, b| {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            })
    })?;
    Some(json!({"width_mm": round2(b[2] - b[0]), "height_mm": round2(b[3] - b[1])}))
}

/// The text of a report.
fn report_text(layers: &[Layer], drc: Option<&Drc>, rules: &Rules) -> String {
    let mut t = String::new();
    t.push_str(&format!("Layers: {}\n", layers.len()));
    for l in layers {
        t.push_str(&format!(
            "  {} [{}, {}, {}]: {} tracks, {} pads, {} regions, {} holes\n",
            l.name,
            l.format,
            l.function,
            l.units,
            l.tracks.len(),
            l.flashes.len(),
            l.regions,
            l.holes.len()
        ));
    }
    if let Some(d) = drc {
        t.push_str(&format!(
            "Rules: track {} mm, gap {} mm, hole {} mm, ring {} mm, edge {} mm\n",
            num(rules.min_track_mm),
            num(rules.min_clearance_mm),
            num(rules.min_hole_mm),
            num(rules.min_annular_ring_mm),
            num(rules.edge_distance_mm)
        ));
        t.push_str(&format!(
            "Verdict: {} ({} findings{})\n",
            d.verdict(),
            d.total,
            if d.truncated { ", the list is cut" } else { "" }
        ));
        for f in &d.findings {
            t.push_str(&format!(
                "  {} {} {}: {}\n",
                f.severity, f.rule, f.layer, f.message
            ));
        }
    }
    t
}

/// The checks a kind of job goes through, in order: each takes the time of an item in the simulator.
fn checks_of(kind: &str) -> Vec<&'static str> {
    match kind {
        "load" => vec!["read"],
        _ => vec![
            "read",
            "track_width",
            "clearance",
            "holes",
            "annular_ring",
            "edge_distance",
        ],
    }
}

// ---- the engine ----------------------------------------------------------------------------------------------------------------------

/// The gerber workers of the server: their jobs.
#[derive(Default)]
pub struct GerberEngine {
    book: SimBook,
}

impl GerberEngine {
    pub fn book(&self) -> &SimBook {
        &self.book
    }

    pub fn prepare(
        &self,
        machine: &str,
        cfg: &GerberConfig,
        request: &Value,
    ) -> PluginResult<Prepared> {
        let kind = request.get("kind").and_then(Value::as_str).unwrap_or("");
        let kind: &'static str = match kind {
            "load" => "load",
            "drc" => "drc",
            "report" => "report",
            _ => {
                return Err(PluginError::Bad(
                    "the request has to be {\"kind\": \"load\" | \"drc\" | \"report\", ...}".into(),
                ))
            }
        };
        let files = files_of(request, cfg.max_input_mib)?;
        let layers = read_files(&files)?;
        let mut view = json!({
            "kind": kind,
            "layers": layers_view(&layers),
            "board": board_box(&layers),
        });
        if kind != "load" {
            let rules = rules_of(request, &cfg.rules)?;
            if !layers.iter().any(Layer::is_copper) {
                return Err(GerberError {
                    code: "missing_layer",
                    detail: "the checks need a copper layer: none of the files is one (the function of a file comes from its name or from %TF.FileFunction)".into(),
                }
                .into());
            }
            let drc = run_drc(&layers, &rules, cfg.max_findings);
            view["verdict"] = json!(drc.verdict());
            view["errors"] = json!(drc.errors());
            view["findings_total"] = json!(drc.total);
            view["truncated"] = json!(drc.truncated);
            view["findings"] = json!(drc.findings.iter().map(Finding::view).collect::<Vec<_>>());
            view["rules"] = json!({
                "min_track_mm": rules.min_track_mm, "min_clearance_mm": rules.min_clearance_mm,
                "min_hole_mm": rules.min_hole_mm, "min_annular_ring_mm": rules.min_annular_ring_mm,
                "edge_distance_mm": rules.edge_distance_mm,
            });
            if kind == "report" {
                let format = request
                    .get("format")
                    .and_then(Value::as_str)
                    .unwrap_or("text");
                match format {
                    "text" => view["report"] = json!(report_text(&layers, Some(&drc), &rules)),
                    "json" => {
                        view["report"] = json!({"layers": layers_view(&layers), "verdict": drc.verdict(), "findings": view["findings"].clone()})
                    }
                    _ => return Err(PluginError::Bad("format is `text` or `json`".into())),
                }
            }
        }
        let cumulative = evenly(checks_of(kind).len(), cfg.item_ms / 1000.0);
        let native_id = self.book.add(machine, kind, cumulative, 1.0);
        Ok(Prepared { native_id, view })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COPPER: &str = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.100*%\n%ADD11C,1.600*%\n%ADD12R,1.600X1.000*%\nD10*\nX100000Y100000D02*\nX400000Y100000D01*\nD11*\nX100000Y200000D03*\nX117000Y200000D03*\nD12*\nX300000Y300000D03*\nM02*\n";
    const OUTLINE: &str = "%FSLAX24Y24*%\n%MOMM*%\n%ADD10C,0.100*%\nD10*\nX0Y0D02*\nX500000Y0D01*\nX500000Y500000D01*\nX0Y500000D01*\nX0Y0D01*\nM02*\n";
    const DRILL: &str = "M48\n; plated holes\nMETRIC,TZ\nT1C0.800\nT2C0.200\n%\nT1\nX10.0Y20.0\nX50.0Y30.0\nT2\nX20.0Y20.0\nM30\n";

    fn files(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect()
    }

    fn track(from: (f64, f64), to: (f64, f64), width: f64) -> Track {
        Track { from, to, width }
    }

    #[test]
    fn the_gap_between_two_tracks_is_the_copper_to_copper_distance() {
        let a = track((0.0, 0.0), (10.0, 0.0), 0.2);
        // parallel, 0.5 mm between the center lines: 0.3 mm of gap
        let b = track((0.0, 0.5), (10.0, 0.5), 0.2);
        assert!((track_gap(&a, &b) - 0.3).abs() < 1e-9);
        // end to end, 1 mm apart on a line
        let c = track((11.0, 0.0), (20.0, 0.0), 0.2);
        assert!((track_gap(&a, &c) - 0.8).abs() < 1e-9);
        // touching and crossing tracks have no gap
        let d = track((10.0, 0.0), (10.0, 5.0), 0.2);
        assert!(track_gap(&a, &d) <= 0.0);
        let x = track((5.0, -3.0), (5.0, 3.0), 0.2);
        assert!(track_gap(&a, &x) <= 0.0);
        // a track that is a point
        let dot = track((5.0, 1.0), (5.0, 1.0), 0.2);
        assert!((track_gap(&a, &dot) - 0.8).abs() < 1e-9);
        // the order does not matter
        assert_eq!(track_gap(&a, &b), track_gap(&b, &a));
    }

    #[test]
    fn two_tracks_too_close_are_found_and_joined_or_far_ones_are_not() {
        let close = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.200*%\nD10*\nX0Y0D02*\nX100000Y0D01*\nX0Y3000D02*\nX100000Y3000D01*\nX0Y100000D02*\nX100000Y100000D01*\nM02*\n";
        let layers = read_files(&files(&[("a.gtl", close)])).unwrap();
        assert_eq!(layers[0].tracks.len(), 3);
        let drc = run_drc(&layers, &Rules::default(), 100);
        let found: Vec<&Finding> = drc
            .findings
            .iter()
            .filter(|f| f.rule == "track_clearance")
            .collect();
        // 0.3 mm between the center lines is 0.1 mm of copper gap, under the 0.15 of the default rules; the third track is far away
        assert_eq!(found.len(), 1, "{:?}", drc.findings);
        assert!((found[0].value - 0.1).abs() < 1e-6);
        assert_eq!(found[0].severity, "error");
        let lax = Rules {
            min_clearance_mm: 0.05,
            ..Rules::default()
        };
        assert!(run_drc(&layers, &lax, 100)
            .findings
            .iter()
            .all(|f| f.rule != "track_clearance"));
    }

    #[test]
    fn the_findings_come_in_pages_that_can_be_filtered() {
        let findings = json!([
            {"rule": "track_width", "severity": "error", "layer": "top"},
            {"rule": "no_outline", "severity": "warning", "layer": ""},
            {"rule": "track_clearance", "severity": "error", "layer": "top"},
            {"rule": "clearance", "severity": "error", "layer": "bottom"},
        ]);
        let all = findings_page(&findings, None, None, None, 0, 100);
        assert_eq!(
            (all["matched"].clone(), all["more"].clone()),
            (json!(4), json!(false))
        );
        let errors = findings_page(&findings, Some("error"), None, None, 0, 100);
        assert_eq!(errors["matched"], 3);
        let top = findings_page(&findings, Some("error"), None, Some("top"), 0, 100);
        assert_eq!(top["findings"].as_array().unwrap().len(), 2);
        let one = findings_page(&findings, None, Some("clearance"), None, 0, 100);
        assert_eq!(one["findings"][0]["layer"], "bottom");
        let first = findings_page(&findings, None, None, None, 0, 3);
        assert_eq!(
            (
                first["findings"].as_array().unwrap().len(),
                first["more"].clone()
            ),
            (3, json!(true))
        );
        let second = findings_page(&findings, None, None, None, 3, 3);
        assert_eq!(
            (
                second["findings"].as_array().unwrap().len(),
                second["more"].clone()
            ),
            (1, json!(false))
        );
        // a limit of 0 is one, one past the maximum is the maximum, and a value that is not a list is an empty page
        assert_eq!(findings_page(&findings, None, None, None, 0, 0)["limit"], 1);
        assert_eq!(
            findings_page(&findings, None, None, None, 0, 5000)["limit"],
            MAX_PAGE
        );
        assert_eq!(
            findings_page(&json!(null), None, None, None, 0, 10)["matched"],
            0
        );
        assert_eq!(
            findings_page(&findings, None, None, None, 99, 10)["findings"],
            json!([])
        );
    }

    #[test]
    fn a_gerber_file_gives_its_function_units_box_and_features() {
        let l = read_gerber("board-F_Cu.gbr", COPPER).unwrap();
        assert_eq!(
            (l.function.as_str(), l.units, l.format),
            ("copper_top", "mm", "gerber")
        );
        assert_eq!((l.tracks.len(), l.flashes.len(), l.apertures), (1, 3, 3));
        assert_eq!(l.tracks[0].width, 0.1);
        assert_eq!(l.tracks[0].from, (10.0, 10.0));
        assert_eq!(l.tracks[0].to, (40.0, 10.0));
        assert_eq!(l.flashes[2].shape, Shape::Rect(1.6, 1.0));
        let [x0, y0, x1, y1] = l.bbox.unwrap();
        assert!((x0 - 9.2).abs() < 1e-9 && (y0 - 9.95).abs() < 1e-9);
        assert!((x1 - 40.05).abs() < 1e-9 && (y1 - 30.5).abs() < 1e-9);
        let o = read_gerber("board.gko", OUTLINE).unwrap();
        assert_eq!(o.function, "outline");
        assert_eq!(o.tracks.len(), 4);
    }

    #[test]
    fn the_function_comes_from_the_attribute_before_the_name() {
        let t = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L2,Bot*%\n%ADD10C,0.2*%\nD10*\nX0Y0D03*\nM02*\n";
        assert_eq!(
            read_gerber("anything.gbr", t).unwrap().function,
            "copper_bottom"
        );
        for (name, f) in [
            ("a.gtl", "copper_top"),
            ("a.gbl", "copper_bottom"),
            ("a.gts", "soldermask"),
            ("a.gto", "silkscreen"),
            ("a-Edge_Cuts.gbr", "outline"),
            ("a-B_Cu.gbr", "copper_bottom"),
            ("a-F_Paste.gbr", "paste"),
            ("a.g2", "copper_inner"),
            ("a.xyz", "unknown"),
        ] {
            assert_eq!(function_from_name(name), f, "{name}");
        }
    }

    #[test]
    fn inches_and_trailing_zero_omission_are_read() {
        let t = "%FSTAX24Y24*%\n%MOIN*%\n%ADD10C,0.01*%\nD10*\nX1Y1D02*\nX2Y1D01*\nM02*\n";
        let l = read_gerber("a.gbr", t).unwrap();
        assert_eq!(l.units, "in");
        // X1 with the trailing zeros omitted of a 2.4 format is 10 inches, X2 is 20
        assert_eq!(l.tracks[0].from, (254.0, 254.0));
        assert_eq!(l.tracks[0].to, (508.0, 254.0));
        assert!((l.tracks[0].width - 0.254).abs() < 1e-9);
        assert_eq!(coordinate("123", 4, 6, true), Some(12.3));
        assert_eq!(coordinate("-5", 3, 6, false), Some(-0.005));
        assert_eq!(coordinate("1.5", 3, 6, false), Some(1.5));
        assert_eq!(coordinate("", 3, 6, false), None);
        assert_eq!(coordinate("12a", 3, 6, false), None);
    }

    #[test]
    fn a_file_with_coordinates_and_no_units_is_refused() {
        let t = "%FSLAX24Y24*%\n%ADD10C,0.1*%\nD10*\nX1Y1D02*\nX2Y1D01*\nM02*\n";
        let e = read_gerber("a.gbr", t).unwrap_err();
        assert_eq!(e.code, "unit_unknown");
        let d = read_excellon("a.drl", "M48\nT1C0.8\n%\nT1\nX1Y1\nM30\n").unwrap_err();
        assert_eq!(d.code, "unit_unknown");
    }

    #[test]
    fn a_file_that_is_not_gerber_is_refused_with_its_name() {
        for (text, why) in [
            ("hello world", "no data"),
            ("%FSLAX24Y24*%\n%MOMM*%\nX1Y1D02", "open"),
            ("%FSLAX9*%\n", "format"),
            ("%MOXX*%\n", "units"),
            ("%FSLAX24Y24*%\n%MOMM*%\nXabcD03*\nM02*\n", "coordinate"),
        ] {
            let e = read_gerber("bad.gbr", text).unwrap_err();
            assert_eq!(e.code, "bad_gerber", "{why}");
            assert!(e.detail.starts_with("bad.gbr:"));
        }
    }

    #[test]
    fn an_excellon_file_gives_its_tools_and_holes() {
        let l = read_excellon("a.drl", DRILL).unwrap();
        assert_eq!((l.holes.len(), l.tools.clone()), (3, vec![0.8, 0.2]));
        assert_eq!(l.holes[0].at, (10.0, 20.0));
        assert_eq!(l.holes[2].diameter, 0.2);
        assert!(l.holes[0].plated);
        assert_eq!(l.function, "drill_plated");
        let np = read_excellon("board-NPTH.drl", DRILL).unwrap();
        assert!(!np.holes[0].plated);
        assert_eq!(np.function, "drill_nonplated");
        let inch = read_excellon("a.drl", "M48\nINCH\nT1C0.0315\n%\nT1\nX1.0Y2.0\nM30\n").unwrap();
        assert!((inch.holes[0].diameter - 0.80010).abs() < 1e-4);
        assert_eq!(inch.holes[0].at, (25.4, 50.8));
        let no_tool = read_excellon("a.drl", "M48\nMETRIC\n%\nX1.0Y2.0\nM30\n").unwrap_err();
        assert_eq!(no_tool.code, "bad_gerber");
        assert!(read_excellon("a.drl", "nothing").is_err());
    }

    #[test]
    fn the_files_of_a_job_are_told_apart() {
        let layers = read_files(&files(&[
            ("a.gtl", COPPER),
            ("a.gko", OUTLINE),
            ("a.drl", DRILL),
        ]))
        .unwrap();
        assert_eq!(
            layers.iter().map(|l| l.format).collect::<Vec<_>>(),
            vec!["gerber", "gerber", "excellon"]
        );
        assert_eq!(read_files(&[]).unwrap_err().code, "missing_layer");
    }

    #[test]
    fn the_checks_find_a_narrow_track_close_pads_a_small_hole_and_a_thin_ring() {
        let layers = read_files(&files(&[
            ("a.gtl", COPPER),
            ("a.gko", OUTLINE),
            ("a.drl", DRILL),
        ]))
        .unwrap();
        let d = run_drc(&layers, &Rules::default(), 100);
        let rules: Vec<&str> = d.findings.iter().map(|f| f.rule).collect();
        assert!(rules.contains(&"track_width"), "{rules:?}");
        assert!(rules.contains(&"clearance"), "{rules:?}");
        assert!(rules.contains(&"hole_small"), "{rules:?}");
        assert!(rules.contains(&"hole_outside"), "{rules:?}");
        assert_eq!(d.verdict(), "fail");
        assert_eq!(d.total, d.findings.len());
        let tw = d.findings.iter().find(|f| f.rule == "track_width").unwrap();
        assert_eq!((tw.value, tw.limit), (0.1, 0.15));
        // the pads at 10,20 and 11.7,20 are 1.6 across: 0.1 mm between the edges; a hole at 10,20 of 0.8 inside a 1.6 pad leaves 0.4 mm
        assert!(!rules.contains(&"annular_ring"));
        let strict = Rules {
            min_annular_ring_mm: 0.5,
            ..Rules::default()
        };
        let d2 = run_drc(&layers, &strict, 100);
        let ring = d2
            .findings
            .iter()
            .find(|f| f.rule == "annular_ring")
            .unwrap();
        assert!((ring.value - 0.4).abs() < 1e-9);
    }

    #[test]
    fn two_pads_closer_than_the_gap_are_found_by_their_edges() {
        let t = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,1.0*%\nD10*\nX0Y0D03*\nX10800Y0D03*\nX30000Y0D03*\nM02*\n";
        let l = read_gerber("a.gbr", t).unwrap();
        let d = run_drc(&[l], &Rules::default(), 10);
        let gaps: Vec<&Finding> = d
            .findings
            .iter()
            .filter(|f| f.rule == "clearance")
            .collect();
        assert_eq!(gaps.len(), 1);
        assert!((gaps[0].value - 0.08).abs() < 1e-9);
    }

    #[test]
    fn a_hole_near_the_edge_or_outside_the_outline_is_found() {
        let layers = read_files(&files(&[
            ("a.gtl", COPPER),
            ("a.gko", OUTLINE),
            (
                "a.drl",
                "M48\nMETRIC\nT1C1.0\n%\nT1\nX0.5Y25.0\nX60.0Y25.0\nX25.0Y25.0\nM30\n",
            ),
        ]))
        .unwrap();
        let d = run_drc(&layers, &Rules::default(), 100);
        let edge: Vec<_> = d
            .findings
            .iter()
            .filter(|f| f.rule == "hole_edge")
            .collect();
        let out: Vec<_> = d
            .findings
            .iter()
            .filter(|f| f.rule == "hole_outside")
            .collect();
        // the hole at 0.5 with r 0.5 reaches the line of the outline (0.05 left, under 0.3); the one at 60 is outside
        assert_eq!((edge.len(), out.len()), (1, 1), "{:?}", d.findings);
    }

    #[test]
    fn a_board_without_an_outline_warns_and_the_findings_are_cut_at_the_limit() {
        let l = read_gerber("a.gtl", COPPER).unwrap();
        let d = run_drc(std::slice::from_ref(&l), &Rules::default(), 1);
        assert!(d.truncated && d.findings.len() == 1 && d.total >= 2);
        let all = run_drc(&[l], &Rules::default(), 100);
        assert!(all
            .findings
            .iter()
            .any(|f| f.rule == "no_outline" && f.severity == "warning"));
        let clean = read_gerber(
            "a.gtl",
            "%FSLAX24Y24*%\n%MOMM*%\n%ADD10C,0.3*%\nD10*\nX100000Y100000D02*\nX200000Y100000D01*\nM02*\n",
        )
        .unwrap();
        let ok = run_drc(&[clean], &Rules::default(), 100);
        assert_eq!(ok.verdict(), "pass");
        assert_eq!(ok.errors(), 0);
    }

    #[test]
    fn the_rules_of_a_request_lie_over_the_ones_of_the_worker() {
        let base = Rules::default();
        let r = rules_of(
            &json!({"rules": {"min_track_mm": 0.2, "edge_distance_mm": 0}}),
            &base,
        )
        .unwrap();
        assert_eq!(
            (r.min_track_mm, r.edge_distance_mm, r.min_hole_mm),
            (0.2, 0.0, 0.3)
        );
        assert_eq!(rules_of(&json!({}), &base).unwrap(), base);
        for bad in [
            json!({"rules": 5}),
            json!({"rules": {"nonsense": 1}}),
            json!({"rules": {"min_track_mm": 9}}),
            json!({"rules": {"min_hole_mm": "x"}}),
        ] {
            assert_eq!(
                rules_of(&bad, &base).unwrap_err().code,
                "bad_rules",
                "{bad}"
            );
        }
    }

    #[test]
    fn the_files_of_a_request_come_inline_or_from_an_artifact_and_have_limits() {
        let cfg = GerberConfig::default();
        let inline = json!({"files": [{"name": "a.gtl", "text": "x"}]});
        assert_eq!(
            files_of(&inline, cfg.max_input_mib).unwrap(),
            files(&[("a.gtl", "x")])
        );
        let art = json!({"artifact": "a-1", "artifact_text": "x"});
        assert_eq!(
            files_of(&art, cfg.max_input_mib).unwrap(),
            files(&[("a-1", "x")])
        );
        assert!(files_of(&json!({}), cfg.max_input_mib).unwrap().is_empty());
        for bad in [
            json!({"files": 5}),
            json!({"files": [{"name": "a"}]}),
            json!({"files": [{"name": "", "text": "x"}]}),
        ] {
            assert_eq!(
                files_of(&bad, cfg.max_input_mib).unwrap_err().code,
                "bad_gerber",
                "{bad}"
            );
        }
        let many: Vec<Value> = (0..65)
            .map(|i| json!({"name": format!("{i}.gbr"), "text": ""}))
            .collect();
        assert!(files_of(&json!({"files": many}), cfg.max_input_mib).is_err());
        let tiny = GerberConfig {
            max_input_mib: 1.0,
            ..GerberConfig::default()
        };
        let big = "x".repeat(1024 * 1024 + 1);
        assert_eq!(
            files_of(
                &json!({"files": [{"name": "a", "text": big}]}),
                tiny.max_input_mib
            )
            .unwrap_err()
            .code,
            "input_too_large"
        );
    }

    #[test]
    fn the_settings_have_ranges_and_a_key_that_is_not_one_is_refused() {
        assert_eq!(
            GerberConfig::from_settings(&json!({})).unwrap(),
            GerberConfig::default()
        );
        let c = GerberConfig::from_settings(
            &json!({"min_track_mm": 0.2, "max_findings": 10, "item_ms": 5}),
        )
        .unwrap();
        assert_eq!(
            (c.rules.min_track_mm, c.max_findings, c.item_ms),
            (0.2, 10, 5.0)
        );
        for bad in [
            json!({"min_track_mm": 0}),
            json!({"min_hole_mm": 11}),
            json!({"max_findings": 0}),
            json!({"max_input_mib": 65}),
            json!({"zzz": 1}),
        ] {
            assert!(GerberConfig::from_settings(&bad).is_err(), "{bad}");
        }
        assert_eq!(SETTINGS_HELP.len(), 8);
    }

    #[test]
    fn the_jobs_load_check_and_report() {
        let engine = GerberEngine::default();
        let cfg = GerberConfig::default();
        let req = |kind: &str| {
            json!({"kind": kind, "files": [
            {"name": "a.gtl", "text": COPPER}, {"name": "a.gko", "text": OUTLINE}, {"name": "a.drl", "text": DRILL}]})
        };
        let load = engine.prepare("w1", &cfg, &req("load")).unwrap();
        assert_eq!(load.view["layers"].as_array().unwrap().len(), 3);
        assert_eq!(load.view["board"]["width_mm"], 50.1);
        assert!(load.view.get("verdict").is_none());
        let drc = engine.prepare("w1", &cfg, &req("drc")).unwrap();
        assert_eq!(drc.view["verdict"], "fail");
        assert!(drc.view["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rule"] == "track_width"));
        let report = engine.prepare("w1", &cfg, &req("report")).unwrap();
        assert!(report.view["report"]
            .as_str()
            .unwrap()
            .contains("Verdict: fail"));
        let mut json_report = req("report");
        json_report["format"] = json!("json");
        let j = engine.prepare("w1", &cfg, &json_report).unwrap();
        assert_eq!(j.view["report"]["verdict"], "fail");
        let mut odd = req("report");
        odd["format"] = json!("pdf");
        assert!(engine.prepare("w1", &cfg, &odd).is_err());
        // a job is a short list of checks that the simulator goes through, one every item_ms
        assert_eq!(engine.book().peek(&drc.native_id).unwrap().total, 6);
        assert_eq!(engine.book().peek(&load.native_id).unwrap().total, 1);
        engine.book().start("w1", &load.native_id).unwrap();
        assert_eq!(
            engine.book().status(&load.native_id).unwrap().state,
            crate::plugins::WorkState::Done
        );
    }

    #[test]
    fn a_job_that_cannot_be_checked_is_refused_with_a_code() {
        let engine = GerberEngine::default();
        let cfg = GerberConfig::default();
        let only_outline = json!({"kind": "drc", "files": [{"name": "a.gko", "text": OUTLINE}]});
        assert_eq!(
            engine
                .prepare("w1", &cfg, &only_outline)
                .err()
                .unwrap()
                .code(),
            "missing_layer"
        );
        let none = json!({"kind": "load"});
        assert_eq!(
            engine.prepare("w1", &cfg, &none).err().unwrap().code(),
            "missing_layer"
        );
        let kind = json!({"kind": "paint", "files": []});
        assert_eq!(
            engine.prepare("w1", &cfg, &kind).err().unwrap().code(),
            "bad_plugin_request"
        );
        let rules = json!({"kind": "drc", "rules": {"min_hole_mm": 99}, "files": [{"name": "a.gtl", "text": COPPER}]});
        assert_eq!(
            engine.prepare("w1", &cfg, &rules).err().unwrap().code(),
            "bad_rules"
        );
        let junk = json!({"kind": "load", "files": [{"name": "a.gbr", "text": "junk"}]});
        assert_eq!(
            engine.prepare("w1", &cfg, &junk).err().unwrap().code(),
            "bad_gerber"
        );
    }
}
