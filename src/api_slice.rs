//! Slicing on the server: `POST /api/v1/slice` takes a mesh (an STL, or a 3MF whose model is stored or deflated) and the profile of a printer and gives G-code that stays
//! inside the limits of that profile, as text or, with `format: "bgcode"`, as a binary G-code file (`src/bgcode.rs`); `POST /api/v1/slice/bgcode` reads such a file back (its
//! metadata, its pictures and, when asked, its G-code). It is **off** unless the switch `slice_enabled` of the security page is on (`404 slice_off`), and it is for trying the chain of the server
//! (a model, a program, a print that a person starts) with the small slicer of `src/slicer.rs`, or with the `phoenix-3dp` program of the suite when the server was told where it
//! is (`slice_phoenix_bin`): then the server runs that one file, with arguments of its own, in a folder of its own, with a time limit, no environment and no input, and reads
//! only the one file it writes. Whatever slices, the program that comes out is read again against the limits of the printer before it is given back, so a program over a limit
//! is a refusal and never a program cut down. The mesh and the program have a size limit, the work has a limit and the time has a limit (`slice_*` in `server.cfg`).

use std::path::Path as FsPath;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::artifacts::{InputRef, NewArtifact, Producer};
use crate::auth::{Identity, Role};
use crate::bgcode::{self, BgError, Bgcode, Limits, WriteOptions};
use crate::plugins::{work_machine_settings, PluginError};
use crate::printer3d::{scan_program, PrinterConfig};
use crate::simjobs::number_in;
use crate::slicer::{parse_3mf, parse_stl, slice, SliceError, SliceLimits, SliceProfile, Sliced};
use crate::sync::Locked;

/// The most triangles a mesh may have.
const MAX_TRIANGLES: usize = 1_000_000;
/// The most work (layers times triangles) the small slicer takes.
const MAX_WORK: u64 = 200_000_000;
/// The keys of a profile that are about slicing and not about the printer.
const SLICE_KEYS: &[&str] = &[
    "layer_mm",
    "line_mm",
    "infill_pct",
    "solid_layers",
    "hotend_c",
    "bed_c",
    "print_feed_mm_s",
    "travel_feed_mm_s",
    "phoenix_printer",
];
/// The size of the bed of the models the server knows, in mm.
const KNOWN_BEDS: &[(&str, [f64; 3])] = &[
    ("bambu_a1", [256.0, 256.0, 256.0]),
    ("bambu_a1_mini", [180.0, 180.0, 180.0]),
    ("bambu_p1s", [256.0, 256.0, 256.0]),
];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/slice", post(slice_mesh))
        .route("/api/v1/slice/bgcode", post(read_bgcode))
}

#[derive(Deserialize)]
struct SliceRequest {
    /// `{"artifact": "<id>"}` or `{"base64": "<bytes>"}`.
    mesh: Value,
    /// `{"machine": "<id of a 3D printer>"}`, or the settings of a printer (`model`, limits, `bed_x_mm`...) with the keys of slicing.
    profile: Value,
    /// `builtin` (the default) or `phoenix`.
    #[serde(default)]
    engine: Option<String>,
    /// `view` (the default) gives the program in the answer, `artifact` keeps it in the store of the caller.
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    artifact_name: Option<String>,
    /// `gcode` (the default) or `bgcode`: what the program is given as.
    #[serde(default)]
    format: Option<String>,
}

fn bad(why: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_plugin_request", why)
}

/// What a slice gave back to the handler.
struct Done {
    body: Value,
    engine: String,
    layers: usize,
}

async fn slice_mesh(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<SliceRequest>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:prepare")?;
    if !s.policy.get().slice_enabled {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "slice_off",
            "slicing on the server is off (the switch slice_enabled)",
        ));
    }
    let engine = b.engine.clone().unwrap_or_else(|| "builtin".into());
    if engine != "builtin" && engine != "phoenix" {
        return Err(bad("engine is builtin or phoenix"));
    }
    let output = b.output.clone().unwrap_or_else(|| "view".into());
    if output != "view" && output != "artifact" {
        return Err(bad("output is view or artifact"));
    }
    let format = b.format.clone().unwrap_or_else(|| "gcode".into());
    if format != "gcode" && format != "bgcode" {
        return Err(bad("format is gcode or bgcode"));
    }
    let (state, by) = (s.clone(), who.clone());
    let done =
        tokio::task::spawn_blocking(move || do_slice(&state, &by, &b, &engine, &output, &format))
            .await
            .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    let layers = done.layers.to_string();
    crate::api::note(
        &s,
        "slice",
        &who,
        &[
            ("engine", done.engine.as_str()),
            ("layers", layers.as_str()),
        ],
    );
    Ok(Json(done.body))
}

fn slice_error(e: SliceError) -> ApiError {
    if e.code == "bad_mesh" {
        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_mesh", e.detail)
    } else {
        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "slice_limit", e.detail)
    }
}

/// The bytes of the mesh and the artifact they came from, if they came from one.
fn read_mesh(
    s: &Shared,
    who: &Identity,
    mesh: &Value,
) -> Result<(Vec<u8>, Option<InputRef>), ApiError> {
    read_input(s, who, mesh, "mesh")
}

/// The bytes of an input (`{"artifact": "<id>"}` or `{"base64": "<bytes>"}`) and the artifact they came from, if they came from one; `what` is how the messages call it.
fn read_input(
    s: &Shared,
    who: &Identity,
    mesh: &Value,
    what: &str,
) -> Result<(Vec<u8>, Option<InputRef>), ApiError> {
    let limit = s.cfg.slice_max_mb * 1024 * 1024;
    let too_big = || {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "too_big",
            format!("the {what} is bigger than {} MiB", s.cfg.slice_max_mb),
        )
    };
    if let Some(id) = mesh.get("artifact").and_then(Value::as_str) {
        let store = s.artifacts.store.locked();
        let record = store
            .get(id)
            .filter(|r| s.artifact_visible(r, who))
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_artifact",
                    format!("the {what} is not an artifact you can see"),
                )
            })?;
        if record.size > limit {
            return Err(too_big());
        }
        let bytes = store.read_blob(&record.blob_sha256).map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "blob_damaged",
                format!("the bytes of the {what} cannot be read"),
            )
        })?;
        let input = InputRef {
            artifact_id: record.artifact_id.clone(),
            blob_sha256: record.blob_sha256.clone(),
        };
        return Ok((bytes, Some(input)));
    }
    if let Some(text) = mesh.get("base64").and_then(Value::as_str) {
        if text.len() as u64 > limit.saturating_mul(4) / 3 + 8 {
            return Err(too_big());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(|_| bad(format!("the {what} is not base64")))?;
        if bytes.len() as u64 > limit {
            return Err(too_big());
        }
        return Ok((bytes, None));
    }
    Err(bad(format!(
        "{what} is {{\"artifact\": \"<id>\"}} or {{\"base64\": \"<bytes>\"}}"
    )))
}

/// The profile of the slice and the settings of the printer it is for, with the bed filled in.
fn build_profile(s: &Shared, profile: &Value) -> Result<(SliceProfile, PrinterConfig), ApiError> {
    let map = profile
        .as_object()
        .ok_or_else(|| bad("profile is an object"))?;
    let mut cfg = if let Some(id) = map.get("machine") {
        let id = id
            .as_str()
            .ok_or_else(|| bad("machine is the id of a 3D printer"))?;
        let settings =
            work_machine_settings(s, id, "printer3d").ok_or(PluginError::UnknownMachine)?;
        PrinterConfig::from_settings(&settings).map_err(bad)?
    } else {
        if map.contains_key("connection") {
            return Err(bad("a profile has no connection: it is not a machine"));
        }
        let rest: Map<String, Value> = map
            .iter()
            .filter(|(k, _)| !SLICE_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        PrinterConfig::from_settings(&Value::Object(rest)).map_err(bad)?
    };
    let known = KNOWN_BEDS
        .iter()
        .find(|(model, _)| *model == cfg.model)
        .map(|(_, bed)| *bed);
    let mut bed = [0.0; 3];
    for (i, axis) in ["bed_x_mm", "bed_y_mm", "bed_z_mm"].into_iter().enumerate() {
        bed[i] = cfg.bed[i].or(known.map(|k| k[i])).ok_or_else(|| {
            bad(format!(
                "{axis} is needed: the model is not one with a known bed"
            ))
        })?;
        cfg.bed[i] = Some(bed[i]);
    }
    let num = |key: &str, low: f64, high: f64, default: f64| -> Result<f64, ApiError> {
        Ok(number_in(profile, key, low, high)
            .map_err(bad)?
            .unwrap_or(default))
    };
    let profile = SliceProfile {
        bed_mm: bed,
        hotend_max_c: cfg.limits[0],
        bed_max_c: cfg.limits[1],
        max_feed_mm_s: cfg.max_feed_mm_s,
        layer_mm: num("layer_mm", 0.01, 2.0, 0.2)?,
        line_mm: num("line_mm", 0.1, 3.0, 0.45)?,
        infill_pct: num("infill_pct", 0.0, 100.0, 15.0)?,
        solid_layers: num("solid_layers", 0.0, 50.0, 3.0)? as usize,
        hotend_c: num("hotend_c", 0.0, 450.0, cfg.preheat_hotend_c)?,
        bed_c: num("bed_c", 0.0, 150.0, cfg.preheat_bed_c)?,
        print_feed_mm_s: num(
            "print_feed_mm_s",
            1.0,
            500.0,
            60.0_f64.min(cfg.max_feed_mm_s),
        )?,
        travel_feed_mm_s: num(
            "travel_feed_mm_s",
            1.0,
            500.0,
            150.0_f64.min(cfg.max_feed_mm_s),
        )?,
    };
    profile.check().map_err(slice_error)?;
    Ok((profile, cfg))
}

fn do_slice(
    s: &Shared,
    who: &Identity,
    b: &SliceRequest,
    engine: &str,
    output: &str,
    format: &str,
) -> Result<Done, ApiError> {
    let (bytes, input) = read_mesh(s, who, &b.mesh)?;
    let (profile, cfg) = build_profile(s, &b.profile)?;
    let limits = SliceLimits {
        max_layers: s.cfg.slice_max_layers as usize,
        max_gcode_bytes: (s.cfg.slice_max_gcode_mb * 1024 * 1024) as usize,
        max_work: MAX_WORK,
    };
    let three_mf = bytes.starts_with(b"PK");
    let sliced = if engine == "builtin" {
        let mesh = if three_mf {
            parse_3mf(&bytes, MAX_TRIANGLES)
        } else {
            parse_stl(&bytes, MAX_TRIANGLES)
        }
        .map_err(slice_error)?;
        slice(&mesh, &profile, &limits).map_err(slice_error)?
    } else {
        let name = b
            .profile
            .get("phoenix_printer")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("the engine phoenix needs phoenix_printer in the profile: the id of the printer in its catalog"))?;
        run_phoenix(s, &bytes, three_mf, name, &limits)?
    };
    // whatever made the program, it is read again against the limits of the printer: a program over a limit is a refusal
    let scan =
        scan_program(&sliced.gcode, &cfg).map_err(|f| ApiError::from(PluginError::from(f)))?;
    let mut body = json!({
        "engine": engine,
        "format": format,
        "layers": sliced.layers,
        "estimate_s": sliced.estimate_s,
        "filament_mm": sliced.filament_mm,
        "bounds_mm": {"min": sliced.bounds.0, "max": sliced.bounds.1},
        "open_layers": sliced.open_layers,
        "limits": {
            "hotend_max_c": profile.hotend_max_c, "bed_max_c": profile.bed_max_c, "max_feed_mm_s": profile.max_feed_mm_s,
            "bed_mm": profile.bed_mm,
        },
        "read": {"lines": scan.lines, "max_hotend_c": scan.max_hotend_c, "max_bed_c": scan.max_bed_c, "max_feed_mm_s": scan.max_feed_mm_s},
    });
    // what is given: the text of the program, or a binary G-code file made from it
    let (payload, name, mime): (Vec<u8>, &str, &str) = if format == "bgcode" {
        let file = slice_file(&sliced, &cfg, &profile, engine);
        let bytes = bgcode::write(&file, &WriteOptions::default()).map_err(bg_error)?;
        (bytes, "slice.bgcode", "application/x-bgcode")
    } else {
        (
            sliced.gcode.clone().into_bytes(),
            "slice.gcode",
            "text/x-gcode",
        )
    };
    if output == "artifact" {
        let meta = NewArtifact {
            name: b.artifact_name.clone().unwrap_or_else(|| name.into()),
            format: mime.into(),
            producer: Some(Producer {
                tool: format!("eh3d-server slice ({engine})"),
                version: env!("CARGO_PKG_VERSION").into(),
                config_sha256: None,
            }),
            inputs: input.into_iter().collect(),
            ..Default::default()
        };
        let record = s.add_artifact(who, meta, &payload).map_err(|e| match e {
            crate::artifacts::ArtifactError::Bad(why) => bad(why),
            other => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage",
                format!(
                    "the program could not be kept as an artifact: {}",
                    other.code()
                ),
            ),
        })?;
        body["artifact"] = json!({
            "artifact_id": record.artifact_id, "blob_sha256": record.blob_sha256, "size": record.size,
            "name": record.name, "format": record.format,
        });
    } else if format == "bgcode" {
        body["bgcode_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(&payload));
        body["bgcode_bytes"] = json!(payload.len());
    } else {
        body["gcode"] = json!(sliced.gcode);
    }
    Ok(Done {
        body,
        engine: engine.to_string(),
        layers: sliced.layers,
    })
}

/// The refusal of the module of binary G-code as an answer.
fn bg_error(e: BgError) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e.code, e.detail)
}

/// A binary G-code file for a program that was sliced: who made it, the printer, what the print is and how it was cut.
fn slice_file(
    sliced: &Sliced,
    cfg: &PrinterConfig,
    profile: &SliceProfile,
    engine: &str,
) -> Bgcode {
    let pair = |k: &str, v: String| (k.to_string(), v);
    Bgcode {
        crc32: true,
        file_meta: vec![pair(
            "Producer",
            format!("eh3d-server {}", env!("CARGO_PKG_VERSION")),
        )],
        printer_meta: vec![pair("printer_model", cfg.model.clone())],
        thumbnails: Vec::new(),
        print_meta: vec![
            pair("layers", sliced.layers.to_string()),
            pair(
                "estimated printing time (s)",
                format!("{:.0}", sliced.estimate_s),
            ),
            pair("filament used [mm]", format!("{:.1}", sliced.filament_mm)),
        ],
        slicer_meta: vec![
            pair("engine", engine.to_string()),
            pair("layer_height", format!("{}", profile.layer_mm)),
            pair("infill_pct", format!("{}", profile.infill_pct)),
        ],
        gcode: sliced.gcode.clone(),
        compressions: Vec::new(),
    }
}

#[derive(Deserialize)]
struct ReadBgcodeRequest {
    /// `{"artifact": "<id>"}` or `{"base64": "<bytes>"}`.
    bgcode: Value,
    /// Whether the answer carries the G-code (it can be big).
    #[serde(default)]
    include_gcode: bool,
}

fn meta_object(meta: &[(String, String)]) -> Value {
    Value::Object(
        meta.iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

/// `POST /api/v1/slice/bgcode`: reads a binary G-code file and says what is in it. Off with the switch of slicing.
async fn read_bgcode(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<ReadBgcodeRequest>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:prepare")?;
    if !s.policy.get().slice_enabled {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "slice_off",
            "slicing on the server is off (the switch slice_enabled)",
        ));
    }
    let state = s.clone();
    let by = who.clone();
    let body = tokio::task::spawn_blocking(move || describe_bgcode(&state, &by, &b))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    crate::api::note(&s, "slice_bgcode", &who, &[]);
    Ok(Json(body))
}

fn describe_bgcode(s: &Shared, who: &Identity, b: &ReadBgcodeRequest) -> Result<Value, ApiError> {
    let (bytes, _) = read_input(s, who, &b.bgcode, "bgcode")?;
    let gcode_limit = (s.cfg.slice_max_gcode_mb * 1024 * 1024) as usize;
    let limits = Limits {
        max_block: gcode_limit,
        max_gcode: gcode_limit,
    };
    let file = bgcode::read(&bytes, &limits).map_err(bg_error)?;
    let pictures: Vec<Value> = file
        .thumbnails
        .iter()
        .map(|t| {
            let format = ["png", "jpg", "qoi"].get(usize::from(t.format)).copied().unwrap_or("unknown");
            json!({
                "format": format,
                "width": t.width, "height": t.height, "bytes": t.data.len(),
            })
        })
        .collect();
    let blocks: Vec<Value> = file
        .compressions
        .iter()
        .map(|(kind, c)| json!({"type": kind, "compression": c.name()}))
        .collect();
    let layers = file
        .gcode
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with(";LAYER_CHANGE") || t.starts_with(";LAYER:")
        })
        .count();
    let mut body = json!({
        "version": 1,
        "checksum": if file.crc32 { "crc32" } else { "none" },
        "file": meta_object(&file.file_meta),
        "printer": meta_object(&file.printer_meta),
        "print": meta_object(&file.print_meta),
        "slicer": meta_object(&file.slicer_meta),
        "thumbnails": pictures,
        "blocks": blocks,
        "gcode_bytes": file.gcode.len(),
        "layers": layers,
        "evidence": "synthetic",
    });
    if b.include_gcode {
        body["gcode"] = json!(file.gcode);
    }
    Ok(body)
}

fn failed(why: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "slice_failed", why)
}

/// Runs the `phoenix-3dp` program of the suite on the mesh: the one file `slice_phoenix_bin` names, with the arguments below and nothing else, in a folder that is made for this
/// slice and removed after, with no environment, no input and a time limit. The program writes one G-code file, which is read up to the limit of its size.
fn run_phoenix(
    s: &Shared,
    mesh: &[u8],
    three_mf: bool,
    printer: &str,
    limits: &SliceLimits,
) -> Result<Sliced, ApiError> {
    let bin = s.cfg.slice_phoenix_bin.clone().ok_or_else(|| {
        ApiError::from(PluginError::NotSupported(
            "the server was not told where the slicer of the suite is (slice_phoenix_bin)",
        ))
    })?;
    if printer.is_empty()
        || printer.len() > 64
        || !printer
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(bad(
            "phoenix_printer is lowercase letters, digits and hyphens (up to 64)",
        ));
    }
    if !bin.is_file() {
        return Err(failed(
            "the program that slices is not where slice_phoenix_bin says",
        ));
    }
    let dir = s
        .cfg
        .data_dir
        .join("slice-tmp")
        .join(crate::ids::new(crate::ids::Kind::Work));
    std::fs::create_dir_all(&dir)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage", ""))?;
    let result = phoenix_in(s, &bin, &dir, mesh, three_mf, printer, limits);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The run of the program in the folder made for it.
fn phoenix_in(
    s: &Shared,
    bin: &FsPath,
    dir: &FsPath,
    mesh: &[u8],
    three_mf: bool,
    printer: &str,
    limits: &SliceLimits,
) -> Result<Sliced, ApiError> {
    let model = dir.join(if three_mf { "model.3mf" } else { "model.stl" });
    let out = dir.join("out.gcode");
    std::fs::write(&model, mesh)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage", ""))?;
    let mut command = Command::new(bin);
    command
        .arg("slice")
        .arg(&model)
        .arg("--printer")
        .arg(printer)
        .arg("--out")
        .arg(&out)
        .current_dir(dir)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|_| failed("the program that slices could not be started"))?;
    let started = Instant::now();
    let limit = Duration::from_secs(s.cfg.slice_timeout_s);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failed(format!(
                    "the program that slices took more than {} seconds and was stopped",
                    s.cfg.slice_timeout_s
                )));
            }
            Err(_) => return Err(failed("the program that slices cannot be waited for")),
        }
    };
    if !status.success() {
        return Err(failed(
            "the program that slices refused the model or the printer (it does not cut a model down to fit)",
        ));
    }
    read_output(&out, limits)
}

/// The G-code the program wrote, if it is there and is not bigger than the limit.
fn read_output(out: &FsPath, limits: &SliceLimits) -> Result<Sliced, ApiError> {
    let size = std::fs::metadata(out)
        .map_err(|_| failed("the program that slices wrote no file"))?
        .len();
    if size as usize > limits.max_gcode_bytes {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "slice_limit",
            format!(
                "the program is bigger than the {} bytes allowed",
                limits.max_gcode_bytes
            ),
        ));
    }
    let gcode = std::fs::read_to_string(out)
        .map_err(|_| failed("the file the program wrote is not text"))?;
    let layers = gcode
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with(";LAYER_CHANGE") || t.starts_with(";LAYER:")
        })
        .count();
    Ok(Sliced {
        gcode,
        layers,
        estimate_s: 0.0,
        filament_mm: 0.0,
        bounds: ([0.0; 3], [0.0; 3]),
        open_layers: 0,
    })
}
