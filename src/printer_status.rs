//! The contract of what a 3D printer says, whatever its family (`eh3d.printers.status/1`), and the readers of what each family says: the `print` report of a Bambu Lab printer
//! in LAN mode and the objects of Klipper through Moonraker. Everything here is **read-only** and pure: it turns JSON that came from a machine on the network into the words
//! of the contract.
//!
//! The other end may be damaged or hostile, so nothing is taken on trust. A value that is missing, of another type or outside what a printer can mean is **unknown** (`null`),
//! never zero; texts are cut and cleaned of control characters; lists are bounded; and only the fields the contract names are kept.

use serde::Serialize;
use serde_json::{Map, Value};

/// The name and version of the contract of the status.
pub const CONTRACT: &str = "eh3d.printers.status/1";

/// The families of protocol the server reads. One adapter for each family, not for each maker.
pub const FAMILIES: &[&str] = &["bambu_lan", "moonraker"];

/// What a printer is doing, in the words of the contract.
pub const STATE_WORDS: &[&str] = &[
    "unknown",
    "idle",
    "preparing",
    "printing",
    "paused",
    "finished",
    "failed",
    "cancelled",
    "error",
    "emergency",
];

/// The longest text of a printer that is kept, in characters.
pub const MAX_TEXT: usize = 128;
/// The most alerts that are kept.
pub const MAX_ALERTS: usize = 32;
const MIN_C: f64 = -100.0;
const MAX_C: f64 = 1000.0;
const MAX_LAYERS: u64 = 1_000_000;
const MAX_REMAINING_S: u64 = 366 * 24 * 3600;

/// One temperature: the reading and the target, each unknown when the printer did not say it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Temperature {
    /// `nozzle`, `bed` or `chamber`.
    pub name: &'static str,
    pub current_c: Option<f64>,
    pub target_c: Option<f64>,
}

/// Something the printer says is wrong or worth knowing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Alert {
    /// `hms` (the health codes of a Bambu printer), `print` (the job failed) or `firmware` (the firmware stopped).
    pub source: &'static str,
    /// `error`, `warning` or `info`.
    pub severity: &'static str,
    /// The code as the maker writes it, or a word of the server for a printer that has no code.
    pub code: String,
    /// What the printer said in words, cut and cleaned; empty when it gave a code only.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Camera {
    /// Whether the printer says it has a camera. The server never gives its address nor its picture.
    pub present: bool,
}

/// What a printer is at the moment, as the contract says it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// One of [`STATE_WORDS`].
    pub state: &'static str,
    /// The word the printer used.
    pub raw: String,
    pub temperatures: Vec<Temperature>,
    pub progress_pct: Option<f64>,
    pub remaining_s: Option<u64>,
    /// Whether the time left was worked out by the server from the progress, because the printer does not say it.
    pub remaining_estimated: bool,
    pub layer: Option<u64>,
    pub layers: Option<u64>,
    pub job_name: String,
    pub alerts: Vec<Alert>,
    pub camera: Option<Camera>,
}

impl Status {
    /// What this printer is known to report. There is no capability of control: the contract is read-only.
    pub fn capabilities(&self) -> Vec<&'static str> {
        let mut out = vec!["state"];
        if !self.temperatures.is_empty() {
            out.push("temperatures");
        }
        if self.temperatures.iter().any(|t| t.name == "chamber") {
            out.push("chamber");
        }
        if self.progress_pct.is_some() {
            out.push("progress");
        }
        if self.remaining_s.is_some() {
            out.push("remaining_time");
        }
        if self.layers.is_some() {
            out.push("layers");
        }
        out.push("alerts");
        if self.camera.as_ref().is_some_and(|c| c.present) {
            out.push("camera");
        }
        out
    }
}

/// A text of a printer: no control characters nor characters that turn the reading order around, and not longer than [`MAX_TEXT`].
pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(MAX_TEXT)
        .collect()
}

/// A finite number, written as a number or as a text.
fn number(v: Option<&Value>) -> Option<f64> {
    let n = match v? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) if s.len() <= 32 => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    n.is_finite().then_some(n)
}

fn celsius(v: Option<&Value>) -> Option<f64> {
    number(v)
        .filter(|c| (MIN_C..=MAX_C).contains(c))
        .map(|c| (c * 10.0).round() / 10.0)
}

fn percent(v: Option<f64>) -> Option<f64> {
    v.filter(|p| (0.0..=100.0).contains(p))
        .map(|p| (p * 10.0).round() / 10.0)
}

fn whole(v: Option<&Value>, max: u64) -> Option<u64> {
    let n = number(v)?;
    (n >= 0.0 && n.fract() == 0.0 && n <= max as f64).then_some(n as u64)
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(clean_text)
        .unwrap_or_default()
}

fn temperature(
    name: &'static str,
    current: Option<f64>,
    target: Option<f64>,
) -> Option<Temperature> {
    (current.is_some() || target.is_some()).then_some(Temperature {
        name,
        current_c: current,
        target_c: target,
    })
}

// ---- Bambu Lab -------------------------------------------------------------------------------------------------------------------------

/// The fields of the `print` report of a Bambu printer that are kept. Everything else is thrown away as it arrives.
const BAMBU_KEYS: &[&str] = &[
    "gcode_state",
    "nozzle_temper",
    "nozzle_target_temper",
    "bed_temper",
    "bed_target_temper",
    "chamber_temper",
    "mc_percent",
    "mc_remaining_time",
    "layer_num",
    "total_layer_num",
    "subtask_name",
    "print_error",
    "hms",
    "ipcam",
];

/// Takes into `known` what a message of the printer says: the printer sends a few fields at a time, so they are kept and merged. Only the fields of the contract are kept,
/// each bounded, so what is held cannot grow with what the printer sends. A value of the wrong type is ignored and what was known stays. Gives whether the message had at
/// least one field of the contract in its `print` object.
pub fn merge_bambu(known: &mut Map<String, Value>, document: &Value) -> bool {
    let Some(print) = document.get("print").and_then(Value::as_object) else {
        return false;
    };
    let mut got = false;
    for key in BAMBU_KEYS {
        let Some(value) = print.get(*key) else {
            continue;
        };
        let kept = match *key {
            "gcode_state" | "subtask_name" => value.as_str().map(|s| Value::from(clean_text(s))),
            "hms" => value.as_array().map(|list| {
                Value::Array(
                    list.iter()
                        .filter_map(|e| {
                            let attr = whole(e.get("attr"), u64::from(u32::MAX))?;
                            let code = whole(e.get("code"), u64::from(u32::MAX))?;
                            Some(serde_json::json!({ "attr": attr, "code": code }))
                        })
                        .take(MAX_ALERTS)
                        .collect(),
                )
            }),
            // only whether there is a camera is kept, never what the printer says about it
            "ipcam" => value.as_object().map(|o| Value::Bool(!o.is_empty())),
            _ => number(Some(value)).map(Value::from),
        };
        if let Some(kept) = kept {
            known.insert((*key).to_string(), kept);
            got = true;
        }
    }
    got
}

fn bambu_state(word: &str) -> &'static str {
    match word {
        "IDLE" => "idle",
        "PREPARE" | "SLICING" => "preparing",
        "RUNNING" => "printing",
        "PAUSE" => "paused",
        "FINISH" => "finished",
        "FAILED" => "failed",
        _ => "unknown",
    }
}

/// The health codes of a Bambu printer as its maker writes them, with the severity the code carries (1 and 2 are errors, 3 a warning, 4 information).
fn bambu_alerts(known: &Map<String, Value>) -> Vec<Alert> {
    let mut out = Vec::new();
    if let Some(error) = whole(known.get("print_error"), u64::from(u32::MAX)).filter(|e| *e != 0) {
        out.push(Alert {
            source: "print",
            severity: "error",
            code: format!("{:04X}_{:04X}", error >> 16, error & 0xFFFF),
            message: String::new(),
        });
    }
    for entry in known
        .get("hms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(attr), Some(code)) = (
            whole(entry.get("attr"), u64::from(u32::MAX)),
            whole(entry.get("code"), u64::from(u32::MAX)),
        ) else {
            continue;
        };
        let severity = match code >> 16 {
            1 | 2 => "error",
            4 => "info",
            _ => "warning",
        };
        out.push(Alert {
            source: "hms",
            severity,
            code: format!(
                "HMS_{:04X}_{:04X}_{:04X}_{:04X}",
                attr >> 16,
                attr & 0xFFFF,
                code >> 16,
                code & 0xFFFF
            ),
            message: String::new(),
        });
    }
    out.truncate(MAX_ALERTS);
    out
}

/// The status a Bambu printer gave, from what [`merge_bambu`] kept. `None` when nothing was kept: there is no report to speak of.
pub fn bambu_status(known: &Map<String, Value>) -> Option<Status> {
    if known.is_empty() {
        return None;
    }
    let raw = text(known.get("gcode_state"));
    let state = bambu_state(&raw);
    let temperatures = [
        temperature(
            "nozzle",
            celsius(known.get("nozzle_temper")),
            celsius(known.get("nozzle_target_temper")),
        ),
        temperature(
            "bed",
            celsius(known.get("bed_temper")),
            celsius(known.get("bed_target_temper")),
        ),
        temperature("chamber", celsius(known.get("chamber_temper")), None),
    ]
    .into_iter()
    .flatten()
    .collect();
    // the numbers of a print are those of the job in hand: an idle printer has none
    let job = state != "idle";
    let layers = whole(known.get("total_layer_num"), MAX_LAYERS).filter(|n| job && *n > 0);
    Some(Status {
        state,
        raw,
        temperatures,
        progress_pct: percent(number(known.get("mc_percent"))).filter(|_| job),
        // the printer says the time left in minutes
        remaining_s: whole(known.get("mc_remaining_time"), MAX_REMAINING_S / 60)
            .filter(|_| job)
            .map(|m| m * 60),
        remaining_estimated: false,
        layer: layers.and(whole(known.get("layer_num"), MAX_LAYERS)),
        layers,
        job_name: if job {
            text(known.get("subtask_name"))
        } else {
            String::new()
        },
        alerts: bambu_alerts(known),
        camera: known
            .get("ipcam")
            .and_then(Value::as_bool)
            .map(|present| Camera { present }),
    })
}

// ---- Klipper through Moonraker ---------------------------------------------------------------------------------------------------------

fn klipper_state(word: &str) -> &'static str {
    match word {
        "standby" => "idle",
        "printing" => "printing",
        "paused" => "paused",
        "complete" => "finished",
        "cancelled" => "cancelled",
        "error" => "error",
        _ => "unknown",
    }
}

/// The status in an answer of `GET /printer/objects/query` of Moonraker. `None` when the answer is not one: it has no `result.status` with at least the state of the print
/// or of the firmware.
pub fn moonraker_status(body: &Value) -> Option<Status> {
    let status = body.get("result")?.get("status")?.as_object()?;
    let part = |key: &str| status.get(key).and_then(Value::as_object);
    let stats = part("print_stats");
    let hooks = part("webhooks");
    let word = stats.and_then(|s| s.get("state")).and_then(Value::as_str);
    let firmware = hooks.and_then(|w| w.get("state")).and_then(Value::as_str);
    if word.is_none() && firmware.is_none() {
        return None;
    }
    let stopped = firmware.is_some_and(|f| f == "shutdown" || f == "error");
    let (state, raw) = if stopped {
        ("emergency", clean_text(firmware.unwrap_or_default()))
    } else if firmware == Some("startup") {
        ("preparing", "startup".to_string())
    } else {
        let word = word.unwrap_or_default();
        (klipper_state(word), clean_text(word))
    };
    let heater = |name: &'static str, key: &str| -> Option<Temperature> {
        let h = part(key)?;
        temperature(
            name,
            celsius(h.get("temperature")),
            celsius(h.get("target")),
        )
    };
    let temperatures = [
        heater("nozzle", "extruder"),
        heater("bed", "heater_bed"),
        heater("chamber", "heater_generic chamber")
            .or_else(|| heater("chamber", "temperature_sensor chamber")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let job = matches!(
        state,
        "printing" | "paused" | "finished" | "cancelled" | "error" | "emergency"
    );
    let fraction = part("virtual_sdcard")
        .and_then(|v| number(v.get("progress")))
        .filter(|f| (0.0..=1.0).contains(f))
        .filter(|_| job);
    let elapsed = stats
        .and_then(|s| number(s.get("print_duration")))
        .filter(|e| (0.0..=MAX_REMAINING_S as f64).contains(e));
    // Klipper does not say the time left: it is worked out from what is done, only once enough is done to mean something
    let remaining_s = match (state, fraction, elapsed) {
        ("printing" | "paused", Some(f), Some(e)) if f >= 0.02 && e > 0.0 => {
            let left = e * (1.0 - f) / f;
            (left <= MAX_REMAINING_S as f64).then_some(left.round() as u64)
        }
        _ => None,
    };
    let info = stats.and_then(|s| s.get("info"));
    let layers =
        whole(info.and_then(|i| i.get("total_layer")), MAX_LAYERS).filter(|n| job && *n > 0);
    let mut alerts = Vec::new();
    if stopped {
        alerts.push(Alert {
            source: "firmware",
            severity: "error",
            code: format!("klipper_{raw}"),
            message: text(hooks.and_then(|w| w.get("state_message"))),
        });
    }
    if word == Some("error") {
        alerts.push(Alert {
            source: "print",
            severity: "error",
            code: String::from("klipper_print_error"),
            message: text(stats.and_then(|s| s.get("message"))),
        });
    }
    Some(Status {
        state,
        raw,
        temperatures,
        progress_pct: percent(fraction.map(|f| f * 100.0)),
        remaining_estimated: remaining_s.is_some(),
        remaining_s,
        layer: layers.and(whole(info.and_then(|i| i.get("current_layer")), MAX_LAYERS)),
        layers,
        job_name: if job {
            text(stats.and_then(|s| s.get("filename")))
        } else {
            String::new()
        },
        alerts,
        camera: None,
    })
}

/// Whether the answer of `GET /server/webcams/list` of Moonraker names a camera. `None` when the answer is not that list.
pub fn moonraker_camera(body: &Value) -> Option<bool> {
    let list = body.get("result")?.get("webcams")?.as_array()?;
    Some(!list.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bambu(doc: Value) -> Option<Status> {
        let mut known = Map::new();
        merge_bambu(&mut known, &doc);
        bambu_status(&known)
    }

    fn full_bambu() -> Value {
        json!({"print": {
            "gcode_state": "RUNNING", "nozzle_temper": 219.96, "nozzle_target_temper": 220, "bed_temper": 59.9, "bed_target_temper": 60,
            "chamber_temper": 31, "mc_percent": 42, "mc_remaining_time": 33, "layer_num": 10, "total_layer_num": 100,
            "subtask_name": "cube", "print_error": 0, "hms": [], "ipcam": {"ipcam_dev": "1", "rtsp_url": "rtsps://user:pw@10.0.0.9/live"},
        }})
    }

    fn full_klipper() -> Value {
        json!({"result": {"status": {
            "extruder": {"temperature": 205.0, "target": 210.0}, "heater_bed": {"temperature": 60.1, "target": 60.0},
            "temperature_sensor chamber": {"temperature": 35.0},
            "print_stats": {"state": "printing", "filename": "part.gcode", "print_duration": 600.0, "info": {"current_layer": 3, "total_layer": 10}},
            "virtual_sdcard": {"progress": 0.25}, "webhooks": {"state": "ready"},
        }}})
    }

    #[test]
    fn a_whole_bambu_report_reads_in_the_words_of_the_contract() {
        let s = bambu(full_bambu()).unwrap();
        assert_eq!((s.state, s.raw.as_str()), ("printing", "RUNNING"));
        assert_eq!(s.temperatures.len(), 3);
        assert_eq!(s.temperatures[0].current_c, Some(220.0));
        assert_eq!(s.temperatures[2].target_c, None);
        assert_eq!(s.progress_pct, Some(42.0));
        assert_eq!(s.remaining_s, Some(1980));
        assert_eq!((s.layer, s.layers), (Some(10), Some(100)));
        assert_eq!(s.job_name, "cube");
        assert_eq!(s.camera, Some(Camera { present: true }));
        assert!(s.alerts.is_empty());
        let caps = s.capabilities();
        for c in [
            "state",
            "temperatures",
            "chamber",
            "progress",
            "remaining_time",
            "layers",
            "alerts",
            "camera",
        ] {
            assert!(caps.contains(&c), "{c}");
        }
        // what the printer said of its camera, with its address and password, is nowhere in what is kept or shown
        let mut known = Map::new();
        merge_bambu(&mut known, &full_bambu());
        let held = serde_json::to_string(&known).unwrap() + &serde_json::to_string(&s).unwrap();
        assert!(
            !held.contains("rtsp") && !held.contains("10.0.0.9"),
            "{held}"
        );
    }

    #[test]
    fn a_bambu_report_is_merged_and_a_wrong_value_changes_nothing() {
        let mut known = Map::new();
        assert!(merge_bambu(&mut known, &full_bambu()));
        assert!(merge_bambu(
            &mut known,
            &json!({"print": {"mc_percent": 43}})
        ));
        assert!(!merge_bambu(
            &mut known,
            &json!({"print": {"nozzle_temper": "hot", "gcode_state": 7, "hms": "none", "ipcam": 3}})
        ));
        let s = bambu_status(&known).unwrap();
        assert_eq!(s.progress_pct, Some(43.0));
        assert_eq!(s.state, "printing");
        assert_eq!(s.temperatures[0].current_c, Some(220.0));
        for nothing in [
            json!({}),
            json!([]),
            json!(null),
            json!(3),
            json!({"print": []}),
            json!({"info": {}}),
            json!({"print": {"command": "push_status"}}),
        ] {
            assert!(!merge_bambu(&mut known, &nothing), "{nothing}");
        }
        assert_eq!(bambu_status(&Map::new()), None);
    }

    #[test]
    fn what_a_bambu_printer_did_not_say_is_unknown_and_never_zero() {
        let s = bambu(json!({"print": {"bed_temper": 25}})).unwrap();
        assert_eq!(s.state, "unknown");
        assert_eq!(s.temperatures.len(), 1);
        assert_eq!(
            (s.temperatures[0].current_c, s.temperatures[0].target_c),
            (Some(25.0), None)
        );
        assert_eq!(
            (
                s.progress_pct,
                s.remaining_s,
                s.layer,
                s.layers,
                s.camera.clone()
            ),
            (None, None, None, None, None)
        );
        let idle = bambu(json!({"print": {"gcode_state": "IDLE", "mc_percent": 100, "subtask_name": "old", "total_layer_num": 9}})).unwrap();
        assert_eq!(
            (
                idle.state,
                idle.progress_pct,
                idle.layers,
                idle.job_name.as_str()
            ),
            ("idle", None, None, "")
        );
        let new = bambu(json!({"print": {"gcode_state": "SOMETHING NEW"}})).unwrap();
        assert_eq!((new.state, new.raw.as_str()), ("unknown", "SOMETHING NEW"));
    }

    #[test]
    fn numbers_a_printer_cannot_mean_are_unknown() {
        let s = bambu(json!({"print": {
            "gcode_state": "RUNNING", "nozzle_temper": 1e308, "nozzle_target_temper": -1e9, "bed_temper": "NaN", "bed_target_temper": "inf",
            "chamber_temper": "1e999", "mc_percent": 250, "mc_remaining_time": -3, "layer_num": 2.5, "total_layer_num": 18446744073709551615u64,
        }}))
        .unwrap();
        assert!(s.temperatures.is_empty(), "{:?}", s.temperatures);
        assert_eq!(
            (s.progress_pct, s.remaining_s, s.layer, s.layers),
            (None, None, None, None)
        );
        let text = bambu(json!({"print": {"gcode_state": "RUNNING", "mc_percent": "42", "nozzle_temper": " 200.5 "}})).unwrap();
        assert_eq!(
            (text.progress_pct, text.temperatures[0].current_c),
            (Some(42.0), Some(200.5))
        );
        // JSON itself has no way to write a number that is not one, and a number too big to hold is refused whole
        assert!(serde_json::from_str::<Value>(r#"{"print":{"nozzle_temper":NaN}}"#).is_err());
        assert!(serde_json::from_str::<Value>(r#"{"print":{"nozzle_temper":1e999}}"#).is_err());
    }

    #[test]
    fn the_codes_of_the_health_of_a_bambu_printer_are_written_with_their_severity() {
        let s = bambu(json!({"print": {
            "gcode_state": "PAUSE", "print_error": 0x0300_8001u32,
            "hms": [{"attr": 0x0300_0100u32, "code": 0x0001_0007u32}, {"attr": 1, "code": 0x0003_0000u32}, {"attr": 2, "code": 0x0004_0001u32},
                    {"attr": 3, "code": 9}, {"attr": "x", "code": 1}, {"code": 1}, 7, null, {"attr": -1, "code": 1}],
        }}))
        .unwrap();
        let seen: Vec<(&str, &str, &str)> = s
            .alerts
            .iter()
            .map(|a| (a.source, a.severity, a.code.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("print", "error", "0300_8001"),
                ("hms", "error", "HMS_0300_0100_0001_0007"),
                ("hms", "warning", "HMS_0000_0001_0003_0000"),
                ("hms", "info", "HMS_0000_0002_0004_0001"),
                ("hms", "warning", "HMS_0000_0003_0000_0009"),
            ]
        );
    }

    #[test]
    fn huge_texts_and_lists_are_bounded_and_cleaned() {
        let list: Vec<Value> = (0..10_000).map(|i| json!({"attr": i, "code": 1})).collect();
        let mut known = Map::new();
        merge_bambu(
            &mut known,
            &json!({"print": {"gcode_state": "RUNNING", "subtask_name": format!("a\u{7}\u{202E}b{}", "x".repeat(100_000)), "hms": list, "junk": "y".repeat(100_000)}}),
        );
        assert!(serde_json::to_string(&known).unwrap().len() < 4096);
        let s = bambu_status(&known).unwrap();
        assert_eq!(s.alerts.len(), MAX_ALERTS);
        assert_eq!(s.job_name.chars().count(), MAX_TEXT);
        assert!(s.job_name.starts_with("abx"));
        assert_eq!(clean_text("é\n\u{2066}ñ"), "éñ");
    }

    #[test]
    fn a_whole_moonraker_answer_reads_in_the_words_of_the_contract() {
        let s = moonraker_status(&full_klipper()).unwrap();
        assert_eq!((s.state, s.raw.as_str()), ("printing", "printing"));
        assert_eq!(
            s.temperatures.iter().map(|t| t.name).collect::<Vec<_>>(),
            vec!["nozzle", "bed", "chamber"]
        );
        assert_eq!(s.progress_pct, Some(25.0));
        assert_eq!((s.remaining_s, s.remaining_estimated), (Some(1800), true));
        assert_eq!((s.layer, s.layers), (Some(3), Some(10)));
        assert_eq!(s.job_name, "part.gcode");
        assert!(s.alerts.is_empty() && s.camera.is_none());
    }

    #[test]
    fn a_stopped_or_failed_klipper_says_so() {
        let mut body = full_klipper();
        body["result"]["status"]["webhooks"] =
            json!({"state": "shutdown", "state_message": "MCU 'mcu' shutdown:\nTimer too close"});
        let s = moonraker_status(&body).unwrap();
        assert_eq!(s.state, "emergency");
        assert_eq!(s.alerts[0].code, "klipper_shutdown");
        assert_eq!(s.alerts[0].message, "MCU 'mcu' shutdown:Timer too close");
        assert_eq!(s.remaining_s, None);
        let mut body = full_klipper();
        body["result"]["status"]["print_stats"]["state"] = json!("error");
        body["result"]["status"]["print_stats"]["message"] = json!("Extruder not heating");
        let s = moonraker_status(&body).unwrap();
        assert_eq!(
            (s.state, s.alerts[0].source, s.alerts[0].message.as_str()),
            ("error", "print", "Extruder not heating")
        );
        let s =
            moonraker_status(&json!({"result": {"status": {"webhooks": {"state": "startup"}}}}))
                .unwrap();
        assert_eq!(s.state, "preparing");
        assert!(s.temperatures.is_empty());
    }

    #[test]
    fn a_partial_or_wrong_moonraker_answer_is_unknown_or_refused() {
        for bad in [
            json!(null),
            json!([]),
            json!("x"),
            json!({}),
            json!({"result": null}),
            json!({"result": {"status": []}}),
            json!({"result": {"status": {}}}),
            json!({"result": {"status": {"extruder": {"temperature": 20}}}}),
            json!({"result": {"status": {"print_stats": {"state": 3}}}}),
            json!({"text": "{\"result\":{\"status\":"}),
        ] {
            assert_eq!(moonraker_status(&bad), None, "{bad}");
        }
        let s = moonraker_status(&json!({"result": {"status": {
            "print_stats": {"state": "printing", "print_duration": -4, "info": {"current_layer": 3}},
            "virtual_sdcard": {"progress": 7.0}, "extruder": {"temperature": null}, "heater_bed": 5,
        }}}))
        .unwrap();
        assert!(s.temperatures.is_empty());
        assert_eq!(
            (s.progress_pct, s.remaining_s, s.layer, s.layers),
            (None, None, None, None)
        );
        // almost nothing done: there is no time left to work out, and a huge one is not given
        for progress in [0.0, 0.001, 0.019] {
            let mut body = full_klipper();
            body["result"]["status"]["virtual_sdcard"]["progress"] = json!(progress);
            assert_eq!(
                moonraker_status(&body).unwrap().remaining_s,
                None,
                "{progress}"
            );
        }
        let mut body = full_klipper();
        body["result"]["status"]["print_stats"]["print_duration"] = json!(3.0e7);
        body["result"]["status"]["virtual_sdcard"]["progress"] = json!(0.02);
        assert_eq!(moonraker_status(&body).unwrap().remaining_s, None);
    }

    #[test]
    fn only_whether_there_is_a_camera_is_read() {
        assert_eq!(
            moonraker_camera(&json!({"result": {"webcams": []}})),
            Some(false)
        );
        assert_eq!(
            moonraker_camera(&json!({"result": {"webcams": [{"stream_url": "http://x"}]}})),
            Some(true)
        );
        for bad in [
            json!(null),
            json!({"result": {}}),
            json!({"result": {"webcams": 2}}),
        ] {
            assert_eq!(moonraker_camera(&bad), None);
        }
    }

    #[test]
    fn with_a_fixed_seed_no_report_breaks_the_readers() {
        let mut seed: u64 = 0x0DD0_5EED;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let pieces = [
            "null",
            "true",
            "-1",
            "0",
            "42",
            "1e308",
            "-1e308",
            "4294967296",
            "\"RUNNING\"",
            "\"printing\"",
            "\"NaN\"",
            "[]",
            "{}",
            "[{\"attr\":1,\"code\":2}]",
            "[[[[[1]]]]]",
            "\"\\u0000\\u202e\"",
        ];
        for _ in 0..2000 {
            let mut p = || pieces[next(pieces.len() as u64) as usize];
            let text = format!(
                r#"{{"print":{{"gcode_state":{},"nozzle_temper":{},"mc_percent":{},"mc_remaining_time":{},"layer_num":{},"total_layer_num":{},"print_error":{},"hms":{},"ipcam":{},"subtask_name":{}}}}}"#,
                p(),
                p(),
                p(),
                p(),
                p(),
                p(),
                p(),
                p(),
                p(),
                p()
            );
            let klipper = format!(
                r#"{{"result":{{"status":{{"print_stats":{{"state":{},"print_duration":{},"info":{}}},"virtual_sdcard":{{"progress":{}}},"webhooks":{{"state":{}}},"extruder":{}}}}}}}"#,
                p(),
                p(),
                p(),
                p(),
                p(),
                p()
            );
            let b = bambu(serde_json::from_str(&text).unwrap());
            let k = moonraker_status(&serde_json::from_str(&klipper).unwrap());
            for s in [b, k].into_iter().flatten() {
                assert!(STATE_WORDS.contains(&s.state));
                assert!(s.progress_pct.is_none_or(|v| (0.0..=100.0).contains(&v)));
                assert!(s.remaining_s.is_none_or(|v| v <= MAX_REMAINING_S));
                assert!(s.temperatures.iter().all(|t| t
                    .current_c
                    .into_iter()
                    .chain(t.target_c)
                    .all(|c| (MIN_C..=MAX_C).contains(&c))));
                assert!(serde_json::to_string(&s).is_ok());
            }
        }
    }
}
