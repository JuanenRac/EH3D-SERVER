//! A small slicer of the server: a closed mesh (STL, or 3MF, whose model is stored or deflated) cut into flat layers, each with the outline of the object as its wall and a
//! rectilinear infill, written as G-code that stays inside the limits of the profile it was asked for. It is the minimum that gives a program the printer plugin accepts; it has
//! no supports, no bridges, no seams, no variable width and the wall is the outline itself (it is not moved inwards), so it is for trying the chain of the server and for simple
//! objects, not for good prints. A refusal is always a refusal: an object that does not fit the bed, a temperature or a feed over the limit, too many layers or too much G-code
//! is an error, and nothing is cut down to fit.
//!
//! The module works on plain numbers and text only (it reads no file and runs nothing), so it is tried without the rest of the server. The route that uses it, and the option
//! of calling a program from the suite instead, are in `src/api_slice.rs`.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::inflate::inflate_raw;

/// The diameter of the filament the extrusion is worked out for, in mm.
const FILAMENT_MM: f64 = 1.75;
/// The length of a retraction, in mm.
const RETRACT_MM: f64 = 0.8;
/// A move longer than this is made with a retraction, in mm.
const RETRACT_OVER_MM: f64 = 2.0;
/// Two points of a cut that are closer than this are the same point, in mm.
const WELD_MM: f64 = 1.0e-3;

pub type Point = [f64; 2];
pub type Triangle = [[f64; 3]; 3];

/// What a slice was refused for. `code` is one of the codes of the catalog of errors (`bad_mesh` or `slice_limit`).
#[derive(Debug, Clone, PartialEq)]
pub struct SliceError {
    pub code: &'static str,
    pub detail: String,
}

fn bad_mesh<T>(detail: impl Into<String>) -> Result<T, SliceError> {
    Err(SliceError {
        code: "bad_mesh",
        detail: detail.into(),
    })
}

fn over_limit<T>(detail: impl Into<String>) -> Result<T, SliceError> {
    Err(SliceError {
        code: "slice_limit",
        detail: detail.into(),
    })
}

/// A mesh of triangles.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mesh {
    pub triangles: Vec<Triangle>,
}

impl Mesh {
    /// The lowest and the highest corner of the box around the mesh.
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for tri in &self.triangles {
            for v in tri {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
        }
        (!self.triangles.is_empty()).then_some((lo, hi))
    }
}

fn finite(v: [f64; 3]) -> bool {
    v.iter().all(|c| c.is_finite() && c.abs() < 1.0e6)
}

/// Reads an STL file, binary or text.
pub fn parse_stl(bytes: &[u8], max_triangles: usize) -> Result<Mesh, SliceError> {
    // a binary file is 80 bytes, a count and 50 bytes for each triangle; a text file may also start with "solid", so the size decides
    if bytes.len() >= 84 {
        let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        if count.checked_mul(50).and_then(|n| n.checked_add(84)) == Some(bytes.len()) {
            if count == 0 {
                return bad_mesh("the file has no triangle");
            }
            if count > max_triangles {
                return over_limit(format!(
                    "the mesh has {count} triangles, more than the {max_triangles} allowed"
                ));
            }
            let f = |at: usize| {
                f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as f64
            };
            let mut triangles = Vec::with_capacity(count);
            for i in 0..count {
                let base = 84 + i * 50 + 12;
                let mut tri = [[0.0; 3]; 3];
                for (v, vertex) in tri.iter_mut().enumerate() {
                    for (k, c) in vertex.iter_mut().enumerate() {
                        *c = f(base + v * 12 + k * 4);
                    }
                    if !finite(*vertex) {
                        return bad_mesh("a vertex is not a number the slicer can use");
                    }
                }
                triangles.push(tri);
            }
            return Ok(Mesh { triangles });
        }
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| SliceError {
            code: "bad_mesh",
            detail: "the file is neither a binary STL of the size it says nor a text STL".into(),
        })?
        .trim_start();
    if !text.starts_with("solid") {
        return bad_mesh("the file is neither a binary STL of the size it says nor a text STL");
    }
    let mut triangles = Vec::new();
    let mut current: Vec<[f64; 3]> = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        if words.next() == Some("vertex") {
            let v: Vec<f64> = words.filter_map(|w| w.parse().ok()).collect();
            if v.len() != 3 || !finite([v[0], v[1], v[2]]) {
                return bad_mesh("a vertex of the text STL has not three numbers");
            }
            current.push([v[0], v[1], v[2]]);
            if current.len() == 3 {
                if triangles.len() >= max_triangles {
                    return over_limit(format!(
                        "the mesh has more than the {max_triangles} triangles allowed"
                    ));
                }
                triangles.push([current[0], current[1], current[2]]);
                current.clear();
            }
        }
    }
    if !current.is_empty() {
        return bad_mesh("a triangle of the text STL has not three vertices");
    }
    if triangles.is_empty() {
        return bad_mesh("the file has no triangle");
    }
    Ok(Mesh { triangles })
}

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]) as usize)
}

fn u32_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]) as usize)
}

/// The most bytes the model of a 3MF file may give once it is read.
const MAX_MODEL_BYTES: usize = 256 * 1024 * 1024;

/// The bytes of a file in a zip, by name: stored, or deflated (the two ways a 3MF file keeps its model).
fn zip_entry(zip: &[u8], wanted: &str) -> Result<Vec<u8>, SliceError> {
    let eocd = (0..zip.len().saturating_sub(21))
        .rev()
        .take(70_000)
        .find(|&i| zip[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or(SliceError {
            code: "bad_mesh",
            detail: "the 3MF file is not a zip".into(),
        })?;
    let entries = u16_at(zip, eocd + 10).unwrap_or(0);
    let mut at = u32_at(zip, eocd + 16).unwrap_or(usize::MAX);
    for _ in 0..entries {
        let broken = || SliceError {
            code: "bad_mesh",
            detail: "the directory of the 3MF zip is damaged".into(),
        };
        if zip.get(at..at + 4) != Some(&[0x50, 0x4b, 0x01, 0x02]) {
            return Err(broken());
        }
        let method = u16_at(zip, at + 10).ok_or_else(broken)?;
        let size = u32_at(zip, at + 20).ok_or_else(broken)?;
        let full_size = u32_at(zip, at + 24).ok_or_else(broken)?;
        let name_len = u16_at(zip, at + 28).ok_or_else(broken)?;
        let extra_len = u16_at(zip, at + 30).ok_or_else(broken)?;
        let comment_len = u16_at(zip, at + 32).ok_or_else(broken)?;
        let local = u32_at(zip, at + 42).ok_or_else(broken)?;
        let name = zip.get(at + 46..at + 46 + name_len).ok_or_else(broken)?;
        if name.eq_ignore_ascii_case(wanted.as_bytes()) {
            if method != 0 && method != 8 {
                return bad_mesh(format!(
                    "the model of the 3MF file is packed with the method {method} of zip, and only stored and deflated files are read; save it as STL"
                ));
            }
            if full_size > MAX_MODEL_BYTES {
                return bad_mesh("the model of the 3MF file is bigger than the slicer reads");
            }
            if zip.get(local..local + 4) != Some(&[0x50, 0x4b, 0x03, 0x04]) {
                return Err(broken());
            }
            let skip = 30
                + u16_at(zip, local + 26).ok_or_else(broken)?
                + u16_at(zip, local + 28).ok_or_else(broken)?;
            let packed = zip
                .get(local + skip..local + skip + size)
                .ok_or_else(broken)?;
            if method == 0 {
                return Ok(packed.to_vec());
            }
            let model = inflate_raw(packed, MAX_MODEL_BYTES).map_err(|e| SliceError {
                code: "bad_mesh",
                detail: format!("the model of the 3MF file: {}", e.text()),
            })?;
            if model.len() != full_size {
                return bad_mesh("the model of the 3MF file does not have the size its zip says");
            }
            return Ok(model);
        }
        at += 46 + name_len + extra_len + comment_len;
    }
    bad_mesh("the 3MF file has no 3D/3dmodel.model")
}

/// The value of an attribute (`x="1.5"`) in a tag.
fn attribute(tag: &str, name: &str) -> Option<f64> {
    let mut from = 0;
    while let Some(i) = tag[from..].find(name) {
        let start = from + i;
        let before = tag[..start].chars().last().unwrap_or(' ');
        let rest = &tag[start + name.len()..];
        if before.is_whitespace() {
            if let Some(rest) = rest.strip_prefix("=\"") {
                return rest[..rest.find('"')?].parse().ok();
            }
        }
        from = start + name.len();
    }
    None
}

/// Reads a 3MF file whose model is stored or deflated (the objects with a mesh; the transforms of the build are not applied).
pub fn parse_3mf(bytes: &[u8], max_triangles: usize) -> Result<Mesh, SliceError> {
    let model = zip_entry(bytes, "3D/3dmodel.model")?;
    let xml = String::from_utf8_lossy(&model);
    let mut triangles = Vec::new();
    for object in xml.split("<object").skip(1) {
        let object = object.split("</object>").next().unwrap_or("");
        let mut vertices: Vec<[f64; 3]> = Vec::new();
        for tag in object.split("<vertex ").skip(1) {
            let tag = tag.split('>').next().unwrap_or("");
            let v = [
                attribute(tag, "x"),
                attribute(tag, "y"),
                attribute(tag, "z"),
            ];
            match v {
                [Some(x), Some(y), Some(z)] if finite([x, y, z]) => vertices.push([x, y, z]),
                _ => return bad_mesh("a vertex of the 3MF model has not three numbers"),
            }
        }
        for tag in object.split("<triangle ").skip(1) {
            let tag = tag.split('>').next().unwrap_or("");
            let corner = |name: &str| {
                attribute(tag, name)
                    .filter(|n| *n >= 0.0 && n.fract() == 0.0)
                    .and_then(|n| vertices.get(n as usize).copied())
            };
            let (Some(a), Some(b), Some(c)) = (corner("v1"), corner("v2"), corner("v3")) else {
                return bad_mesh("a triangle of the 3MF model names a vertex that is not there");
            };
            if triangles.len() >= max_triangles {
                return over_limit(format!(
                    "the mesh has more than the {max_triangles} triangles allowed"
                ));
            }
            triangles.push([a, b, c]);
        }
    }
    if triangles.is_empty() {
        return bad_mesh("the 3MF model has no triangle");
    }
    Ok(Mesh { triangles })
}

/// What the slice is made for: the machine (bed and limits) and the way to print.
#[derive(Debug, Clone, PartialEq)]
pub struct SliceProfile {
    pub bed_mm: [f64; 3],
    pub hotend_max_c: f64,
    pub bed_max_c: f64,
    pub max_feed_mm_s: f64,
    pub layer_mm: f64,
    pub line_mm: f64,
    pub infill_pct: f64,
    pub solid_layers: usize,
    pub hotend_c: f64,
    pub bed_c: f64,
    pub print_feed_mm_s: f64,
    pub travel_feed_mm_s: f64,
}

/// The least the server lets be asked for, whatever the profile.
pub const LAYER_RANGE_MM: (f64, f64) = (0.05, 0.6);
pub const LINE_RANGE_MM: (f64, f64) = (0.2, 1.2);

impl SliceProfile {
    /// Refuses a profile that asks for more than the machine it is for allows, or that has a value out of the ranges of the slicer.
    pub fn check(&self) -> Result<(), SliceError> {
        let range = |name: &str, v: f64, low: f64, high: f64| -> Result<(), SliceError> {
            if v.is_finite() && (low..=high).contains(&v) {
                Ok(())
            } else {
                over_limit(format!("{name} is from {low} to {high}"))
            }
        };
        for (i, axis) in ["bed_x_mm", "bed_y_mm", "bed_z_mm"].into_iter().enumerate() {
            range(axis, self.bed_mm[i], 10.0, 2000.0)?;
        }
        range(
            "layer_mm",
            self.layer_mm,
            LAYER_RANGE_MM.0,
            LAYER_RANGE_MM.1,
        )?;
        range("line_mm", self.line_mm, LINE_RANGE_MM.0, LINE_RANGE_MM.1)?;
        range("infill_pct", self.infill_pct, 0.0, 100.0)?;
        range("solid_layers", self.solid_layers as f64, 0.0, 20.0)?;
        if self.hotend_c > self.hotend_max_c {
            return over_limit(format!(
                "the hotend target {} C is over the limit of the profile, {} C",
                self.hotend_c, self.hotend_max_c
            ));
        }
        if self.bed_c > self.bed_max_c {
            return over_limit(format!(
                "the bed target {} C is over the limit of the profile, {} C",
                self.bed_c, self.bed_max_c
            ));
        }
        range("hotend_c", self.hotend_c, 0.0, self.hotend_max_c)?;
        range("bed_c", self.bed_c, 0.0, self.bed_max_c)?;
        for (name, feed) in [
            ("print_feed_mm_s", self.print_feed_mm_s),
            ("travel_feed_mm_s", self.travel_feed_mm_s),
        ] {
            if feed > self.max_feed_mm_s {
                return over_limit(format!(
                    "{name} {feed} is over the fastest feed of the profile, {} mm/s",
                    self.max_feed_mm_s
                ));
            }
            range(name, feed, 1.0, self.max_feed_mm_s)?;
        }
        Ok(())
    }
}

/// What the slice may take.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliceLimits {
    pub max_layers: usize,
    pub max_gcode_bytes: usize,
    /// The most work a slice may be: the layers times the triangles, which is how many times a triangle is looked at.
    pub max_work: u64,
}

/// What a slice gave.
#[derive(Debug, Clone, PartialEq)]
pub struct Sliced {
    pub gcode: String,
    pub layers: usize,
    pub estimate_s: f64,
    pub filament_mm: f64,
    /// The box around the object once it is on the bed: lowest and highest corner.
    pub bounds: ([f64; 3], [f64; 3]),
    /// The layers where the cut found an outline that does not close (a mesh with a hole); they are printed without it.
    pub open_layers: usize,
}

type Segment = (Point, Point);

fn cut(tri: &Triangle, z: f64) -> Option<Segment> {
    let above: Vec<bool> = tri.iter().map(|v| v[2] > z).collect();
    let mut points = Vec::new();
    for (a, b) in [(0usize, 1usize), (1, 2), (2, 0)] {
        if above[a] != above[b] {
            let (p, q) = (tri[a], tri[b]);
            let t = (z - p[2]) / (q[2] - p[2]);
            points.push([p[0] + t * (q[0] - p[0]), p[1] + t * (q[1] - p[1])]);
        }
    }
    (points.len() == 2).then(|| (points[0], points[1]))
}

fn key(p: Point) -> (i64, i64) {
    (
        (p[0] / WELD_MM).round() as i64,
        (p[1] / WELD_MM).round() as i64,
    )
}

/// Joins the segments of a cut into closed loops. The second number is how many chains did not close.
fn loops(segments: &[Segment]) -> (Vec<Vec<Point>>, usize) {
    let mut at: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, (a, b)) in segments.iter().enumerate() {
        at.entry(key(*a)).or_default().push(i);
        at.entry(key(*b)).or_default().push(i);
    }
    let mut used = vec![false; segments.len()];
    let (mut closed, mut open) = (Vec::new(), 0);
    for first in 0..segments.len() {
        if used[first] {
            continue;
        }
        used[first] = true;
        let start = segments[first].0;
        let mut chain = vec![start];
        let mut end = segments[first].1;
        loop {
            chain.push(end);
            if key(end) == key(start) {
                break;
            }
            let next = at
                .get(&key(end))
                .and_then(|list| list.iter().copied().find(|j| !used[*j]));
            let Some(j) = next else {
                break;
            };
            used[j] = true;
            let (a, b) = segments[j];
            end = if key(a) == key(end) { b } else { a };
        }
        if chain.len() >= 4 && key(*chain.last().unwrap_or(&start)) == key(start) {
            chain.pop();
            closed.push(chain);
        } else {
            open += 1;
        }
    }
    (closed, open)
}

/// The stretches of a scan line inside the loops (even-odd): the scan goes along X at height `c`, or along Y at `c` when `along_y`.
fn scan(loops: &[Vec<Point>], c: f64, along_y: bool) -> Vec<(f64, f64)> {
    let (u, v) = if along_y { (1, 0) } else { (0, 1) };
    let mut crossings = Vec::new();
    for lp in loops {
        for i in 0..lp.len() {
            let (p, q) = (lp[i], lp[(i + 1) % lp.len()]);
            let (lo, hi) = if p[v] <= q[v] { (p, q) } else { (q, p) };
            if lo[v] <= c && c < hi[v] {
                let t = (c - lo[v]) / (hi[v] - lo[v]);
                crossings.push(lo[u] + t * (hi[u] - lo[u]));
            }
        }
    }
    crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (0..crossings.len() / 2)
        .map(|i| (crossings[2 * i], crossings[2 * i + 1]))
        .collect()
}

struct Writer {
    out: String,
    at: Point,
    time_s: f64,
    filament_mm: f64,
    per_mm: f64,
    line_mm: f64,
    layer_mm: f64,
    print_feed: f64,
    travel_feed: f64,
}

impl Writer {
    fn travel_to(&mut self, to: Point) {
        let d = dist(self.at, to);
        if d < 1.0e-6 {
            return;
        }
        let retract = d > RETRACT_OVER_MM;
        if retract {
            let _ = writeln!(self.out, "G1 E-{RETRACT_MM} F2400");
        }
        let _ = writeln!(
            self.out,
            "G0 X{:.3} Y{:.3} F{:.0}",
            to[0],
            to[1],
            self.travel_feed * 60.0
        );
        if retract {
            let _ = writeln!(self.out, "G1 E{RETRACT_MM} F2400");
        }
        self.time_s += d / self.travel_feed;
        self.at = to;
    }

    fn line_to(&mut self, to: Point) {
        let d = dist(self.at, to);
        if d < 1.0e-6 {
            return;
        }
        let e = d * self.per_mm;
        let _ = writeln!(
            self.out,
            "G1 X{:.3} Y{:.3} E{:.5} F{:.0}",
            to[0],
            to[1],
            e,
            self.print_feed * 60.0
        );
        self.time_s += d / self.print_feed;
        self.filament_mm += e;
        self.at = to;
    }
}

fn dist(a: Point, b: Point) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Slices the mesh for the profile.
pub fn slice(mesh: &Mesh, p: &SliceProfile, limits: &SliceLimits) -> Result<Sliced, SliceError> {
    p.check()?;
    let Some((lo, hi)) = mesh.bounds() else {
        return bad_mesh("the mesh has no triangle");
    };
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    for (i, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
        if size[i] > p.bed_mm[i] {
            return over_limit(format!(
                "the object is {:.1} mm along {axis} and the printer has {} mm",
                size[i], p.bed_mm[i]
            ));
        }
    }
    if size[2] < p.layer_mm {
        return bad_mesh("the object is lower than one layer");
    }
    let layers = (size[2] / p.layer_mm).ceil() as usize;
    let work = (layers as u64).saturating_mul(mesh.triangles.len() as u64);
    if work > limits.max_work {
        return over_limit(format!(
            "the object is too much work to slice here ({layers} layers of {} triangles)",
            mesh.triangles.len()
        ));
    }
    if layers > limits.max_layers {
        return over_limit(format!(
            "the object needs {layers} layers, more than the {} allowed",
            limits.max_layers
        ));
    }
    // the object goes to the middle of the bed and down to it
    let shift = [
        p.bed_mm[0] / 2.0 - (lo[0] + hi[0]) / 2.0,
        p.bed_mm[1] / 2.0 - (lo[1] + hi[1]) / 2.0,
        -lo[2],
    ];
    let triangles: Vec<Triangle> = mesh
        .triangles
        .iter()
        .map(|t| {
            let mut t = *t;
            for v in &mut t {
                for k in 0..3 {
                    v[k] += shift[k];
                }
            }
            t
        })
        .collect();

    let per_mm = p.line_mm * p.layer_mm / (std::f64::consts::PI * (FILAMENT_MM / 2.0).powi(2));
    let mut w = Writer {
        out: String::new(),
        at: [0.0, 0.0],
        time_s: 0.0,
        filament_mm: 0.0,
        per_mm,
        line_mm: p.line_mm,
        layer_mm: p.layer_mm,
        print_feed: p.print_feed_mm_s,
        travel_feed: p.travel_feed_mm_s,
    };
    let _ = writeln!(
        w.out,
        "; sliced by the small slicer of the server: outline and rectilinear infill, no supports"
    );
    let _ = writeln!(
        w.out,
        "; layer {} mm, line {} mm, infill {} %, {} solid layers at the bottom and at the top",
        p.layer_mm, p.line_mm, p.infill_pct, p.solid_layers
    );
    let _ = writeln!(
        w.out,
        "M140 S{}\nM104 S{}\nG28\nM190 S{}\nM109 S{}\nG90\nM83",
        p.bed_c, p.hotend_c, p.bed_c, p.hotend_c
    );
    let spacing = |solid: bool| {
        if solid || p.infill_pct >= 100.0 {
            p.line_mm
        } else if p.infill_pct <= 0.0 {
            f64::INFINITY
        } else {
            p.line_mm * 100.0 / p.infill_pct
        }
    };
    let mut open_layers = 0;
    let mut printed = 0;
    for layer in 0..layers {
        let z = (layer as f64 + 0.5) * p.layer_mm;
        let segments: Vec<Segment> = triangles.iter().filter_map(|t| cut(t, z)).collect();
        let (outlines, open) = loops(&segments);
        if open > 0 {
            open_layers += 1;
        }
        if outlines.is_empty() {
            continue;
        }
        printed += 1;
        let top = ((layer + 1) as f64 * p.layer_mm).min(size[2]);
        let _ = writeln!(
            w.out,
            ";LAYER_CHANGE\n;Z:{:.3}\nG1 Z{:.3} F{:.0}",
            top,
            top,
            p.travel_feed_mm_s * 60.0
        );
        for lp in &outlines {
            w.travel_to(lp[0]);
            for q in lp.iter().skip(1).chain(std::iter::once(&lp[0])) {
                w.line_to(*q);
            }
        }
        let solid = layer < p.solid_layers || layer + p.solid_layers >= layers;
        let step = spacing(solid);
        if step.is_finite() {
            let along_y = layer % 2 == 1;
            let (min, max) = {
                let axis = usize::from(!along_y);
                let all = outlines.iter().flatten();
                (
                    all.clone().map(|q| q[axis]).fold(f64::INFINITY, f64::min),
                    all.map(|q| q[axis]).fold(f64::NEG_INFINITY, f64::max),
                )
            };
            let mut c = min + step / 2.0;
            let mut forward = true;
            while c < max {
                let mut runs = scan(&outlines, c, along_y);
                if !forward {
                    runs.reverse();
                }
                for (a, b) in runs {
                    let (from, to) = if forward { (a, b) } else { (b, a) };
                    let at = |u: f64| if along_y { [c, u] } else { [u, c] };
                    w.travel_to(at(from));
                    w.line_to(at(to));
                }
                forward = !forward;
                c += step;
            }
        }
        if w.out.len() > limits.max_gcode_bytes {
            return over_limit(format!(
                "the program is bigger than the {} bytes allowed",
                limits.max_gcode_bytes
            ));
        }
    }
    if printed == 0 {
        return bad_mesh("no layer of the mesh has a closed outline: the mesh is not closed");
    }
    // the head goes up at the end, but never past the top of the printer
    let lift = (p.bed_mm[2] - size[2]).clamp(0.0, 5.0);
    let _ = writeln!(w.out, "M104 S0\nM140 S0");
    if lift >= 0.1 {
        let _ = writeln!(w.out, "G91\nG1 Z{lift:.1} F600\nG90");
    }
    let _ = writeln!(w.out, "M84");
    // the width and height of a line are only used for the extrusion; keep the reading of them honest
    let _ = (w.line_mm, w.layer_mm);
    if w.out.len() > limits.max_gcode_bytes {
        return over_limit(format!(
            "the program is bigger than the {} bytes allowed",
            limits.max_gcode_bytes
        ));
    }
    Ok(Sliced {
        gcode: w.out,
        layers: printed,
        estimate_s: w.time_s.round(),
        filament_mm: (w.filament_mm * 10.0).round() / 10.0,
        bounds: (
            [lo[0] + shift[0], lo[1] + shift[1], 0.0],
            [hi[0] + shift[0], hi[1] + shift[1], size[2]],
        ),
        open_layers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed box as twelve triangles.
    fn cube(size: f64) -> Mesh {
        let v = |x: usize, y: usize, z: usize| [x as f64 * size, y as f64 * size, z as f64 * size];
        let quads = [
            [v(0, 0, 0), v(0, 1, 0), v(1, 1, 0), v(1, 0, 0)],
            [v(0, 0, 1), v(1, 0, 1), v(1, 1, 1), v(0, 1, 1)],
            [v(0, 0, 0), v(1, 0, 0), v(1, 0, 1), v(0, 0, 1)],
            [v(1, 0, 0), v(1, 1, 0), v(1, 1, 1), v(1, 0, 1)],
            [v(1, 1, 0), v(0, 1, 0), v(0, 1, 1), v(1, 1, 1)],
            [v(0, 1, 0), v(0, 0, 0), v(0, 0, 1), v(0, 1, 1)],
        ];
        let mut triangles = Vec::new();
        for q in quads {
            triangles.push([q[0], q[1], q[2]]);
            triangles.push([q[0], q[2], q[3]]);
        }
        Mesh { triangles }
    }

    fn profile() -> SliceProfile {
        SliceProfile {
            bed_mm: [256.0, 256.0, 256.0],
            hotend_max_c: 300.0,
            bed_max_c: 100.0,
            max_feed_mm_s: 200.0,
            layer_mm: 0.2,
            line_mm: 0.45,
            infill_pct: 20.0,
            solid_layers: 2,
            hotend_c: 215.0,
            bed_c: 60.0,
            print_feed_mm_s: 60.0,
            travel_feed_mm_s: 150.0,
        }
    }

    const LIMITS: SliceLimits = SliceLimits {
        max_layers: 1000,
        max_gcode_bytes: 8_000_000,
        max_work: 1_000_000,
    };

    fn ascii_stl(mesh: &Mesh) -> String {
        let mut text = String::from("solid test\n");
        for t in &mesh.triangles {
            text.push_str("facet normal 0 0 0\nouter loop\n");
            for v in t {
                text.push_str(&format!("vertex {} {} {}\n", v[0], v[1], v[2]));
            }
            text.push_str("endloop\nendfacet\n");
        }
        text.push_str("endsolid test\n");
        text
    }

    fn binary_stl(mesh: &Mesh) -> Vec<u8> {
        let mut out = vec![0u8; 80];
        out.extend((mesh.triangles.len() as u32).to_le_bytes());
        for t in &mesh.triangles {
            out.extend([0u8; 12]);
            for v in t {
                for c in v {
                    out.extend((*c as f32).to_le_bytes());
                }
            }
            out.extend([0u8; 2]);
        }
        out
    }

    #[test]
    fn a_text_and_a_binary_stl_give_the_same_mesh() {
        let cube = cube(10.0);
        let text = parse_stl(ascii_stl(&cube).as_bytes(), 100).unwrap();
        let binary = parse_stl(&binary_stl(&cube), 100).unwrap();
        assert_eq!(text, cube);
        assert_eq!(binary, cube);
        assert_eq!(text.bounds(), Some(([0.0; 3], [10.0; 3])));
    }

    #[test]
    fn a_file_that_is_not_a_mesh_is_refused() {
        for bad in [
            &b""[..],
            &b"hello"[..],
            &b"solid x\nvertex 1 2\nendsolid"[..],
        ] {
            assert_eq!(parse_stl(bad, 100).unwrap_err().code, "bad_mesh");
        }
        let mut broken = binary_stl(&cube(1.0));
        broken.truncate(broken.len() - 10);
        assert_eq!(parse_stl(&broken, 100).unwrap_err().code, "bad_mesh");
        let big = parse_stl(&binary_stl(&cube(1.0)), 5).unwrap_err();
        assert_eq!(big.code, "slice_limit");
        let nan = ascii_stl(&cube(1.0)).replacen("vertex 0 0 0", "vertex nan 0 0", 1);
        assert_eq!(parse_stl(nan.as_bytes(), 100).unwrap_err().code, "bad_mesh");
    }

    /// A zip with one stored file.
    fn zip_of(name: &str, data: &[u8]) -> Vec<u8> {
        let mut z = Vec::new();
        z.extend([
            0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        z.extend((data.len() as u32).to_le_bytes());
        z.extend((data.len() as u32).to_le_bytes());
        z.extend((name.len() as u16).to_le_bytes());
        z.extend([0, 0]);
        z.extend(name.as_bytes());
        z.extend(data);
        let directory = z.len();
        z.extend([
            0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        z.extend((data.len() as u32).to_le_bytes());
        z.extend((data.len() as u32).to_le_bytes());
        z.extend((name.len() as u16).to_le_bytes());
        z.extend([0u8; 16]);
        z.extend(name.as_bytes());
        let size = z.len() - directory;
        z.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend((size as u32).to_le_bytes());
        z.extend((directory as u32).to_le_bytes());
        z.extend([0, 0]);
        z
    }

    /// A zip with one file kept with `method` (0 stored, 8 deflated): `packed` is what the file holds and `full` its size once read.
    fn zip_with(name: &str, method: u8, packed: &[u8], full: usize) -> Vec<u8> {
        let mut z = zip_of(name, packed);
        let local = 8;
        z[local] = method;
        let at = z
            .windows(4)
            .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        z[at + 10] = method;
        z[at + 24..at + 28].copy_from_slice(&(full as u32).to_le_bytes());
        z
    }

    /// The model below packed by zlib at level 9 as raw deflate (a block with its own tables of codes), made by an independent program.
    const PACKED_MODEL: [u8; 127] = [
        125, 144, 75, 14, 195, 32, 12, 68, 175, 130, 124, 1, 215, 201, 214, 112, 151, 54, 177, 90,
        170, 164, 72, 64, 81, 218, 211, 167, 224, 69, 89, 177, 25, 141, 244, 236, 241, 135, 247,
        176, 202, 230, 56, 74, 10, 239, 184, 72, 114, 28, 110, 79, 89, 178, 241, 171, 5, 2, 199,
        187, 164, 135, 227, 34, 49, 251, 134, 171, 147, 195, 28, 22, 46, 96, 62, 77, 191, 85, 177,
        71, 52, 96, 138, 104, 192, 20, 81, 99, 248, 159, 156, 163, 191, 190, 238, 91, 111, 77, 161,
        86, 94, 166, 186, 172, 41, 179, 133, 169, 118, 13, 248, 220, 82, 187, 44, 212, 11, 81, 239,
        254, 153, 238, 23, 168, 239, 57, 1,
    ];
    const PACKED_MODEL_XML: &str = r#"<model><resources><object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/><vertex x="0" y="0" z="10"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/><triangle v1="0" v2="1" v3="3"/></triangles></mesh></object></resources></model>"#;

    #[test]
    fn a_deflated_3mf_is_read() {
        // packed by another program
        let zip = zip_with("3D/3dmodel.model", 8, &PACKED_MODEL, PACKED_MODEL_XML.len());
        let mesh = parse_3mf(&zip, 10).unwrap();
        assert_eq!(mesh.triangles.len(), 2);
        assert_eq!(mesh.triangles[1][2], [0.0, 0.0, 10.0]);
        // packed by the module (stored blocks of deflate)
        let own = crate::inflate::deflate_stored_raw(PACKED_MODEL_XML.as_bytes());
        let zip = zip_with("3D/3dmodel.model", 8, &own, PACKED_MODEL_XML.len());
        assert_eq!(parse_3mf(&zip, 10).unwrap().triangles.len(), 2);
        // a size that the zip says wrong, damaged data and a cut stream are refused
        let lying = zip_with(
            "3D/3dmodel.model",
            8,
            &PACKED_MODEL,
            PACKED_MODEL_XML.len() + 1,
        );
        assert!(parse_3mf(&lying, 10).unwrap_err().detail.contains("size"));
        let mut damaged = PACKED_MODEL;
        damaged[40] ^= 0xFF;
        let zip = zip_with("3D/3dmodel.model", 8, &damaged, PACKED_MODEL_XML.len());
        assert_eq!(parse_3mf(&zip, 10).unwrap_err().code, "bad_mesh");
        let zip = zip_with(
            "3D/3dmodel.model",
            8,
            &PACKED_MODEL[..60],
            PACKED_MODEL_XML.len(),
        );
        assert_eq!(parse_3mf(&zip, 10).unwrap_err().code, "bad_mesh");
        // a model that says it is far bigger than the slicer reads
        let huge = zip_with("3D/3dmodel.model", 8, &PACKED_MODEL, MAX_MODEL_BYTES + 1);
        assert!(parse_3mf(&huge, 10).unwrap_err().detail.contains("bigger"));
    }

    #[test]
    fn a_stored_3mf_is_read_and_one_with_another_method_is_refused() {
        let xml = r#"<model><resources><object id="1"><mesh><vertices>
            <vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/><vertex x="0" y="0" z="10"/>
            </vertices><triangles><triangle v1="0" v2="1" v3="2"/><triangle v1="0" v2="1" v3="3"/></triangles></mesh></object></resources></model>"#;
        let zip = zip_of("3D/3dmodel.model", xml.as_bytes());
        let mesh = parse_3mf(&zip, 10).unwrap();
        assert_eq!(mesh.triangles.len(), 2);
        assert_eq!(mesh.triangles[1][2], [0.0, 0.0, 10.0]);
        assert_eq!(parse_3mf(&zip, 1).unwrap_err().code, "slice_limit");
        let other = zip_of("other.txt", b"x");
        assert!(parse_3mf(&other, 10)
            .unwrap_err()
            .detail
            .contains("no 3D/3dmodel.model"));
        assert_eq!(
            parse_3mf(b"not a zip at all, padded to be long enough", 10)
                .unwrap_err()
                .code,
            "bad_mesh"
        );
        let mut odd = zip.clone();
        // the method of the entry in the directory: 12 (bzip2) is not read
        let at = odd
            .windows(4)
            .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
            .unwrap()
            + 10;
        odd[at] = 12;
        assert!(parse_3mf(&odd, 10)
            .unwrap_err()
            .detail
            .contains("method 12"));
        let bad_index = xml
            .replace(r#"v3="3""#, r#"v3="9""#)
            .replace(r#"v3="2""#, r#"v3="9""#);
        let zip = zip_of("3D/3dmodel.model", bad_index.as_bytes());
        assert_eq!(parse_3mf(&zip, 10).unwrap_err().code, "bad_mesh");
    }

    #[test]
    fn a_cube_gives_layers_with_walls_infill_and_the_limits_of_the_profile() {
        let out = slice(&cube(20.0), &profile(), &LIMITS).unwrap();
        assert_eq!(out.layers, 100);
        assert_eq!(out.open_layers, 0);
        // centred on the bed and on it
        assert_eq!(out.bounds, ([118.0, 118.0, 0.0], [138.0, 138.0, 20.0]));
        assert!(out.gcode.contains("M104 S215") && out.gcode.contains("M140 S60"));
        assert_eq!(out.gcode.matches(";LAYER_CHANGE").count(), 100);
        assert!(out.gcode.trim_end().ends_with("M84"));
        assert!(out.filament_mm > 100.0 && out.estimate_s > 60.0, "{out:?}");
        // never past the bed, never a feed over the limit, never a temperature over the limit
        for line in out
            .gcode
            .lines()
            .filter(|l| l.starts_with("G0 ") || l.starts_with("G1 X"))
        {
            for word in line.split_whitespace() {
                let value: f64 = word[1..].parse().unwrap_or(0.0);
                match &word[..1] {
                    "X" | "Y" => assert!((0.0..=256.0).contains(&value), "{line}"),
                    "F" => assert!(value <= 200.0 * 60.0, "{line}"),
                    _ => {}
                }
            }
        }
        // more infill, more plastic
        let dense = SliceProfile {
            infill_pct: 80.0,
            ..profile()
        };
        assert!(slice(&cube(20.0), &dense, &LIMITS).unwrap().filament_mm > out.filament_mm);
        let empty = SliceProfile {
            infill_pct: 0.0,
            solid_layers: 0,
            ..profile()
        };
        assert!(slice(&cube(20.0), &empty, &LIMITS).unwrap().filament_mm < out.filament_mm);
    }

    #[test]
    fn what_does_not_fit_or_goes_past_a_limit_is_refused_and_never_cut_down() {
        let refused = |mesh: Mesh, p: SliceProfile, limits: SliceLimits| {
            slice(&mesh, &p, &limits).unwrap_err()
        };
        let e = refused(cube(300.0), profile(), LIMITS);
        assert_eq!(e.code, "slice_limit");
        assert!(e.detail.contains("along X"), "{}", e.detail);
        let tall = Mesh {
            triangles: cube(10.0)
                .triangles
                .iter()
                .map(|t| t.map(|v| [v[0], v[1], v[2] * 30.0]))
                .collect(),
        };
        assert!(refused(tall, profile(), LIMITS).detail.contains("along Z"));
        let hot = SliceProfile {
            hotend_c: 310.0,
            ..profile()
        };
        assert!(refused(cube(10.0), hot, LIMITS)
            .detail
            .contains("over the limit"));
        let warm = SliceProfile {
            bed_c: 101.0,
            ..profile()
        };
        assert!(refused(cube(10.0), warm, LIMITS)
            .detail
            .contains("bed target"));
        let fast = SliceProfile {
            print_feed_mm_s: 250.0,
            ..profile()
        };
        assert!(refused(cube(10.0), fast, LIMITS)
            .detail
            .contains("fastest feed"));
        let thin = SliceProfile {
            layer_mm: 0.01,
            ..profile()
        };
        assert_eq!(refused(cube(10.0), thin, LIMITS).code, "slice_limit");
        let few = SliceLimits {
            max_layers: 10,
            ..LIMITS
        };
        assert!(refused(cube(10.0), profile(), few)
            .detail
            .contains("layers"));
        let heavy = SliceLimits {
            max_work: 100,
            ..LIMITS
        };
        assert!(refused(cube(10.0), profile(), heavy)
            .detail
            .contains("too much work"));
        let small = SliceLimits {
            max_gcode_bytes: 1000,
            ..LIMITS
        };
        assert!(refused(cube(20.0), profile(), small)
            .detail
            .contains("bytes"));
        assert_eq!(refused(Mesh::default(), profile(), LIMITS).code, "bad_mesh");
    }

    #[test]
    fn the_head_never_goes_up_past_the_top_of_the_printer() {
        let low = slice(&cube(20.0), &profile(), &LIMITS).unwrap();
        assert!(low.gcode.contains("G91\nG1 Z5.0 F600\nG90"));
        let tight = SliceProfile {
            bed_mm: [256.0, 256.0, 22.0],
            ..profile()
        };
        let near = slice(&cube(20.0), &tight, &LIMITS).unwrap();
        assert!(near.gcode.contains("G1 Z2.0 F600"));
        let full = SliceProfile {
            bed_mm: [256.0, 256.0, 20.0],
            ..profile()
        };
        let top = slice(&cube(20.0), &full, &LIMITS).unwrap();
        assert!(!top.gcode.contains("G91") && top.gcode.trim_end().ends_with("M84"));
    }

    #[test]
    fn a_mesh_with_no_closed_outline_is_not_sliced() {
        // one open triangle standing up: every cut is one segment that does not close
        let tri = Mesh {
            triangles: vec![[[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 0.0, 10.0]]],
        };
        let e = slice(&tri, &profile(), &LIMITS).unwrap_err();
        assert_eq!(e.code, "bad_mesh");
        assert!(e.detail.contains("not closed"));
    }
}
