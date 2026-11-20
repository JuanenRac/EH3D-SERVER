//! The 3D printers of the server: a queue of prints with the hands of a person on it, the limits of temperature of each model, the checks that go on while a printer heats and
//! prints, and an emergency stop that latches until an administrator releases it. The plugin `printer3d` (`src/plugin_printer3d.rs`) only hands the machines of the server to this
//! engine; the printer itself is behind a driver (`src/printer_drivers.rs`): the simulator that is here, Klipper through Moonraker, or OctoPrint.
//!
//! Nothing starts by itself unless the machine is set to (`auto_start_queue`, off by default). A program is read before it is accepted: a temperature over a limit refuses the
//! whole program, `M112` is refused (the emergency stop is a route, not a line of a file), a move past the bed is refused. The limits of temperature come from the profile of the
//! model (the numbers the maker gives) and a setting may only lower them or, up to a hard maximum, raise them. While a printer is polled the engine watches every heater: a reading
//! that is not a reading (a broken sensor), a heater that does not get to its target in time, one that wanders off its target or runs past its limit stops the printer and
//! latches the machine. The simulator heats and cools with a first-order model and can be made to fail in each of those ways.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::bambu::normalize_fingerprint;
use crate::gcode;
use crate::plugins::{PluginError, PluginResult, PluginStatus, Prepared, WorkState};
use crate::printer_drivers::{
    moonraker_shape, octoprint_shape, DriverError, DriverResult, Heater, MoonrakerDriver,
    OctoPrintDriver, PrinterDriver, PrinterJob, PrinterState, TcpHttp,
};
use crate::printer_lan::{BambuDriver, ConnectError, MarlinDriver, SimBambu, SimSerial};
use crate::simjobs::{flag_in, known_keys, num, number_in, round2};
use crate::sync::Locked;

/// The hard maximum of each limit of temperature: no setting goes over it.
pub const HARD_MAX: [(&str, f64); 3] = [("hotend", 450.0), ("bed", 150.0), ("chamber", 100.0)];

/// The limits of temperature the makers give, by model: `(model, hotend, bed, chamber, what it is)`. A chamber of 0 is a printer with no heater for it. These are the numbers of the
/// makers' own specifications as the project knows them; a person who sells or builds with another model sets the limits in the settings.
pub const PROFILES: &[(&str, f64, f64, f64, &str)] = &[
    (
        "generic",
        260.0,
        100.0,
        60.0,
        "any printer: the conservative limits of the server",
    ),
    ("bambu_a1", 300.0, 100.0, 0.0, "Bambu Lab A1"),
    ("bambu_a1_mini", 300.0, 80.0, 0.0, "Bambu Lab A1 mini"),
    (
        "bambu_p1s",
        300.0,
        100.0,
        0.0,
        "Bambu Lab P1S (the chamber has no heater)",
    ),
];

/// The settings a printer machine may have: key and what it takes.
pub const SETTINGS_HELP: &[(&str, &str)] = &[
    ("model", "the model: generic (the default), bambu_a1, bambu_a1_mini or bambu_p1s; its profile gives the limits of temperature"),
    ("connection", "how the printer is reached: {\"kind\": \"demo\"} (the simulator, the default), {\"kind\": \"moonraker\", \"url\": \"http://host:7125\", \"api_key\": \"...\"} or {\"kind\": \"octoprint\", \"url\": ..., \"api_key\": ...}; the key is never shown again; {\"kind\": \"marlin\", \"port\": \"sim\", \"baud\": 115200} is a Marlin printer on a serial line (only the simulated line is served); {\"kind\": \"bambu_lan\", \"host\": \"sim\", \"serial\": \"...\", \"fingerprint\": \"<sha256 of the certificate>\", \"api_key\": \"<access code>\"} is a Bambu Lab printer in LAN mode, an EXPERIMENT served only as a simulator: a certificate is never accepted unless it is the pinned one"),
    ("hotend_max_c", "the hottest the hotend may be asked, 0 to 450 (the profile of the model by default)"),
    ("bed_max_c", "the hottest the bed may be asked, 0 to 150 (the profile by default)"),
    ("chamber_max_c", "the hottest the chamber may be asked, 0 to 100 (the profile by default; 0 is a printer with no chamber heater)"),
    ("heat_timeout_s", "a heater that does not get to its target in this time stops the job, 30 to 3600 (900 by default)"),
    ("temp_deviation_c", "a heater this far from its target for deviation_s latches the machine, 1 to 50 (15 by default)"),
    ("deviation_s", "how long the deviation lasts before it latches, 1 to 120 (10 by default)"),
    ("min_temp_c", "a reading below this is a broken sensor, -50 to 50 (5 by default)"),
    ("max_feed_mm_s", "the fastest feed a program may ask, 1 to 500 (200 by default)"),
    ("bed_x_mm", "the bed along X, 10 to 2000: a program that goes past it is refused (not set: not checked)"),
    ("bed_y_mm", "the bed along Y, 10 to 2000 (not set: not checked)"),
    ("bed_z_mm", "the height of the printer, 10 to 2000 (not set: not checked)"),
    ("preheat_default_hotend_c", "the hotend target of a preheat that names none, 0 to 450 (200 by default)"),
    ("preheat_default_bed_c", "the bed target of a preheat that names none, 0 to 150 (60 by default)"),
    ("poll_ms", "how often the printer is asked, 200 to 10000 (1000 by default)"),
    ("auto_start_queue", "the next queued job starts by itself when the printer is free (false by default: a person starts every print)"),
    ("compat", "the answers shaped like another program's (GET .../compat/moonraker and .../compat/octoprint): off (the default), moonraker, octoprint or both"),
    ("compat_start", "a program that uses the write routes of Moonraker or OctoPrint may start a print that waits in the queue (false by default: a person starts it with the routes of the server; pausing and cancelling work for any key that may write)"),
    ("step_ms", "the milliseconds one line of a program takes in the simulator, 0 to 3600000 (20 by default)"),
    ("sim_rate", "how many times faster than real time the simulator works, 0 to 100000 (1 by default; 0 is no waiting)"),
    ("sim_fault", "makes the simulator fail: none (the default), thermal_runaway, sensor_lost or disconnect"),
];

/// The most lines of a manual command list.
pub const MAX_COMMANDS: usize = 50;
/// The slack, in degrees, within which a heater is at its target.
const AT_TARGET_C: f64 = 2.0;
/// The most samples of temperature kept.
const HISTORY: usize = 120;
/// The polls in a row that get no answer before a job is failed for it.
const MISSED_POLLS: u32 = 3;

/// How the printer is reached.
#[derive(Debug, Clone, PartialEq)]
pub enum Connection {
    Demo,
    Moonraker {
        url: String,
        api_key: Option<String>,
    },
    OctoPrint {
        url: String,
        api_key: Option<String>,
    },
    /// A Marlin printer on a serial line. Only the simulated line (`port` is `sim`) is served.
    Marlin {
        port: String,
        baud: u32,
    },
    /// A Bambu Lab printer in LAN mode (an experiment). Only the simulated printer (`host` is `sim`) is served. `access_code` is the access code of the printer (kept
    /// like the key of the API of the other printers), `fingerprint` the SHA-256 of the certificate the printer must show: with none, the connection is refused.
    BambuLan {
        host: String,
        serial: String,
        access_code: Option<String>,
        fingerprint: Option<String>,
    },
}

impl Connection {
    /// The word the settings and the detail of the machine call it by.
    pub fn name(&self) -> &'static str {
        match self {
            Connection::Demo => "demo",
            Connection::Moonraker { .. } => "moonraker",
            Connection::OctoPrint { .. } => "octoprint",
            Connection::Marlin { .. } => "marlin",
            Connection::BambuLan { .. } => "bambu_lan",
        }
    }
}

/// The settings of a printer machine, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct PrinterConfig {
    pub model: String,
    pub connection: Connection,
    pub limits: [f64; 3],
    pub heat_timeout_s: f64,
    pub temp_deviation_c: f64,
    pub deviation_s: f64,
    pub min_temp_c: f64,
    pub max_feed_mm_s: f64,
    pub bed: [Option<f64>; 3],
    pub preheat_hotend_c: f64,
    pub preheat_bed_c: f64,
    pub poll_ms: f64,
    pub auto_start_queue: bool,
    pub compat: String,
    pub compat_start: bool,
    pub step_ms: f64,
    pub sim_rate: f64,
    pub sim_fault: String,
}

impl Default for PrinterConfig {
    fn default() -> Self {
        PrinterConfig {
            model: "generic".into(),
            connection: Connection::Demo,
            limits: [260.0, 100.0, 60.0],
            heat_timeout_s: 900.0,
            temp_deviation_c: 15.0,
            deviation_s: 10.0,
            min_temp_c: 5.0,
            max_feed_mm_s: 200.0,
            bed: [None; 3],
            preheat_hotend_c: 200.0,
            preheat_bed_c: 60.0,
            poll_ms: 1000.0,
            auto_start_queue: false,
            compat: "off".into(),
            compat_start: false,
            step_ms: 20.0,
            sim_rate: 1.0,
            sim_fault: "none".into(),
        }
    }
}

fn heater_index(name: &str) -> Option<usize> {
    ["hotend", "bed", "chamber"].iter().position(|h| *h == name)
}

impl PrinterConfig {
    pub fn from_settings(settings: &Value) -> Result<PrinterConfig, String> {
        let keys: Vec<&str> = SETTINGS_HELP.iter().map(|(k, _)| *k).collect();
        known_keys(settings, &keys, "a 3D printer")?;
        let mut c = PrinterConfig::default();
        if let Some(m) = settings.get("model") {
            let name = m.as_str().ok_or("model is a text")?;
            let p = PROFILES.iter().find(|p| p.0 == name).ok_or_else(|| {
                format!(
                    "model is one of {}",
                    PROFILES.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
                )
            })?;
            c.model = name.to_string();
            c.limits = [p.1, p.2, p.3];
        }
        for (i, key) in ["hotend_max_c", "bed_max_c", "chamber_max_c"]
            .into_iter()
            .enumerate()
        {
            if let Some(n) = number_in(settings, key, 0.0, HARD_MAX[i].1)? {
                c.limits[i] = n;
            }
        }
        if let Some(n) = number_in(settings, "heat_timeout_s", 30.0, 3600.0)? {
            c.heat_timeout_s = n;
        }
        if let Some(n) = number_in(settings, "temp_deviation_c", 1.0, 50.0)? {
            c.temp_deviation_c = n;
        }
        if let Some(n) = number_in(settings, "deviation_s", 1.0, 120.0)? {
            c.deviation_s = n;
        }
        if let Some(n) = number_in(settings, "min_temp_c", -50.0, 50.0)? {
            c.min_temp_c = n;
        }
        if let Some(n) = number_in(settings, "max_feed_mm_s", 1.0, 500.0)? {
            c.max_feed_mm_s = n;
        }
        for (i, key) in ["bed_x_mm", "bed_y_mm", "bed_z_mm"].into_iter().enumerate() {
            c.bed[i] = number_in(settings, key, 10.0, 2000.0)?;
        }
        c.preheat_hotend_c = c.preheat_hotend_c.min(c.limits[0]);
        c.preheat_bed_c = c.preheat_bed_c.min(c.limits[1]);
        if let Some(n) = number_in(settings, "preheat_default_hotend_c", 0.0, 450.0)? {
            c.preheat_hotend_c = n;
        }
        if let Some(n) = number_in(settings, "preheat_default_bed_c", 0.0, 150.0)? {
            c.preheat_bed_c = n;
        }
        if c.preheat_hotend_c > c.limits[0] || c.preheat_bed_c > c.limits[1] {
            return Err("the default preheat is over a limit of temperature of the machine".into());
        }
        if let Some(n) = number_in(settings, "poll_ms", 200.0, 10_000.0)? {
            c.poll_ms = n;
        }
        if let Some(n) = number_in(settings, "step_ms", 0.0, 3_600_000.0)? {
            c.step_ms = n;
        }
        if let Some(n) = number_in(settings, "sim_rate", 0.0, 100_000.0)? {
            c.sim_rate = n;
        }
        if let Some(b) = flag_in(settings, "auto_start_queue")? {
            c.auto_start_queue = b;
        }
        if let Some(b) = flag_in(settings, "compat_start")? {
            c.compat_start = b;
        }
        if let Some(v) = settings.get("compat") {
            match v.as_str() {
                Some(w @ ("off" | "moonraker" | "octoprint" | "both")) => c.compat = w.to_string(),
                _ => return Err("compat is off, moonraker, octoprint or both".into()),
            }
        }
        if let Some(v) = settings.get("sim_fault") {
            match v.as_str() {
                Some(w @ ("none" | "thermal_runaway" | "sensor_lost" | "disconnect")) => {
                    c.sim_fault = w.to_string()
                }
                _ => {
                    return Err(
                        "sim_fault is none, thermal_runaway, sensor_lost or disconnect".into(),
                    )
                }
            }
        }
        if let Some(conn) = settings.get("connection") {
            let url = || -> Result<String, String> {
                let url = conn
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or("a connection to a printer has its `url`")?;
                TcpHttp::new(url, Duration::from_secs(1))?;
                Ok(url.to_string())
            };
            let key = || -> Result<Option<String>, String> {
                match conn.get("api_key") {
                    None | Some(Value::Null) => Ok(None),
                    Some(Value::String(k)) if k.len() <= 200 => Ok(Some(k.clone())),
                    Some(_) => Err("api_key is a text of up to 200 characters".into()),
                }
            };
            c.connection = match conn.get("kind").and_then(Value::as_str) {
                Some("demo") => Connection::Demo,
                Some("moonraker") => Connection::Moonraker {
                    url: url()?,
                    api_key: key()?,
                },
                Some("octoprint") => Connection::OctoPrint {
                    url: url()?,
                    api_key: key()?,
                },
                Some("marlin") => {
                    let port = conn
                        .get("port")
                        .and_then(Value::as_str)
                        .ok_or("a connection to a Marlin printer has its `port`")?;
                    if port != "sim" {
                        return Err("the serial line of a real Marlin printer is not served yet: port is \"sim\", the simulated line".into());
                    }
                    let baud = match conn.get("baud") {
                        None => 115_200,
                        Some(v) => v
                            .as_u64()
                            .filter(|b| (1200..=2_000_000).contains(b))
                            .ok_or("baud is a number from 1200 to 2000000")?
                            as u32,
                    };
                    Connection::Marlin {
                        port: port.to_string(),
                        baud,
                    }
                }
                Some("bambu_lan") => {
                    let host = conn
                        .get("host")
                        .and_then(Value::as_str)
                        .ok_or("a connection to a Bambu printer has its `host`")?;
                    if host != "sim" {
                        return Err("the LAN link to a real Bambu printer (MQTT over TLS and FTPS) is not served yet: host is \"sim\", the simulated printer; bambu_lan is an experiment".into());
                    }
                    let serial = conn
                        .get("serial")
                        .and_then(Value::as_str)
                        .filter(|s| {
                            (8..=32).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric())
                        })
                        .ok_or("serial is the serial number of the printer: 8 to 32 letters and digits")?;
                    let fingerprint = match conn.get("fingerprint") {
                        None | Some(Value::Null) => None,
                        Some(Value::String(f)) => Some(normalize_fingerprint(f).ok_or(
                            "fingerprint is the SHA-256 of the certificate: 64 hexadecimal digits",
                        )?),
                        Some(_) => return Err("fingerprint is a text".into()),
                    };
                    Connection::BambuLan {
                        host: host.to_string(),
                        serial: serial.to_string(),
                        access_code: key()?,
                        fingerprint,
                    }
                }
                _ => {
                    return Err(
                        "connection.kind is demo, moonraker, octoprint, marlin or bambu_lan".into(),
                    )
                }
            };
        }
        Ok(c)
    }

    pub fn limit(&self, heater: &str) -> f64 {
        heater_index(heater).map_or(0.0, |i| self.limits[i])
    }
}

/// A refusal or a fault: one of the codes of the catalog of errors and what is wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct Fault {
    pub code: &'static str,
    pub detail: String,
}

impl From<Fault> for PluginError {
    fn from(e: Fault) -> PluginError {
        PluginError::Coded(e.code, e.detail)
    }
}

fn fail<T>(code: &'static str, detail: String) -> Result<T, Fault> {
    Err(Fault { code, detail })
}

// ---- the program -----------------------------------------------------------------------------------------------------------------------

/// What reading a program found.
#[derive(Debug, Clone, PartialEq)]
pub struct Scan {
    pub lines: usize,
    pub layers: u64,
    pub max_hotend_c: f64,
    pub max_bed_c: f64,
    pub max_feed_mm_s: f64,
    pub travel_mm: f64,
    pub estimate_s: f64,
    /// The first line (counted from 1) of each layer.
    pub layer_starts: Vec<usize>,
}

/// Reads a program against the limits of the machine. A temperature over a limit, `M112`, a feed over the limit or a move past the bed refuses the whole program.
pub fn scan_program(text: &str, cfg: &PrinterConfig) -> Result<Scan, Fault> {
    let program = gcode::parse(text, 0, false).map_err(|e| Fault {
        code: e.code,
        detail: e.detail,
    })?;
    let layers_marked: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim();
            t.starts_with(";LAYER_CHANGE") || t.starts_with(";LAYER:")
        })
        .map(|(n, _)| n + 1)
        .collect();
    let mut scan = Scan {
        lines: program.entries.len(),
        layers: layers_marked.len() as u64,
        max_hotend_c: 0.0,
        max_bed_c: 0.0,
        max_feed_mm_s: 0.0,
        travel_mm: 0.0,
        estimate_s: 0.0,
        layer_starts: Vec::new(),
    };
    let (mut pos, mut absolute, mut scale, mut feed) = ([0.0f64; 3], true, 1.0f64, 0.0f64);
    let mut next_layer = 0usize;
    for (index, e) in program.entries.iter().enumerate() {
        while next_layer < layers_marked.len() && layers_marked[next_layer] <= e.raw + 1 {
            scan.layer_starts.push(index);
            next_layer += 1;
        }
        let words = gcode::words(&e.text);
        let line = e.raw + 1;
        let code = |letter: char| words.iter().find(|w| w.0 == letter).map(|w| w.1);
        let g = code('G').map(|v| (v * 10.0).round() as i32);
        let m = code('M').map(|v| v as i32);
        if m == Some(112) {
            return fail(
                "bad_gcode",
                format!("line {line}: M112 is the emergency stop of the printer and is not accepted in a program: the stop is a route"),
            );
        }
        if let (Some(m), Some(s)) = (m, code('S')) {
            let (heater, name) = match m {
                104 | 109 => (0, "hotend"),
                140 | 190 => (1, "bed"),
                141 | 191 => (2, "chamber"),
                _ => (9, ""),
            };
            if heater < 3 {
                if s > cfg.limits[heater] {
                    return fail(
                        "program_over_limit",
                        format!(
                            "line {line}: M{m} S{} asks the {name} for more than its limit of {} C",
                            num(s),
                            num(cfg.limits[heater])
                        ),
                    );
                }
                if heater == 0 {
                    scan.max_hotend_c = scan.max_hotend_c.max(s);
                } else if heater == 1 {
                    scan.max_bed_c = scan.max_bed_c.max(s);
                }
            }
        }
        match g {
            Some(200) => scale = 25.4,
            Some(210) => scale = 1.0,
            Some(900) => absolute = true,
            Some(910) => absolute = false,
            Some(280) => {
                // homing: the axes named (or all of them) go to zero
                let named: Vec<char> = words
                    .iter()
                    .map(|w| w.0)
                    .filter(|c| matches!(c, 'X' | 'Y' | 'Z'))
                    .collect();
                for (i, axis) in ['X', 'Y', 'Z'].into_iter().enumerate() {
                    if named.is_empty() || named.contains(&axis) {
                        pos[i] = 0.0;
                    }
                }
                continue;
            }
            _ => {}
        }
        if let Some(f) = code('F') {
            feed = f * scale / 60.0;
            if matches!(g, Some(0 | 10)) && feed > cfg.max_feed_mm_s {
                return fail(
                    "program_over_limit",
                    format!(
                        "line {line}: the feed of {} mm/s is over the {} mm/s the machine allows",
                        num(feed),
                        num(cfg.max_feed_mm_s)
                    ),
                );
            }
            scan.max_feed_mm_s = scan.max_feed_mm_s.max(feed);
        }
        if !matches!(g, Some(0 | 10)) {
            continue;
        }
        let mut to = pos;
        for (i, axis) in ['X', 'Y', 'Z'].into_iter().enumerate() {
            if let Some(v) = code(axis) {
                to[i] = if absolute {
                    v * scale
                } else {
                    pos[i] + v * scale
                };
                if let Some(limit) = cfg.bed[i] {
                    if to[i] < -1e-6 || to[i] > limit + 1e-6 {
                        return fail(
                            "out_of_travel",
                            format!(
                                "line {line}: the move goes to {axis}{} mm, outside 0 to {} mm",
                                num(to[i]),
                                num(limit)
                            ),
                        );
                    }
                }
            }
        }
        let d =
            ((to[0] - pos[0]).powi(2) + (to[1] - pos[1]).powi(2) + (to[2] - pos[2]).powi(2)).sqrt();
        scan.travel_mm += d;
        let speed = if g == Some(0) {
            200.0
        } else if feed > 0.0 {
            feed
        } else {
            50.0
        };
        scan.estimate_s += d / speed;
        pos = to;
    }
    Ok(scan)
}

// ---- the simulator ---------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Therm {
    current: f64,
    target: f64,
    power: f64,
    max_rate: f64,
    tau: f64,
}

impl Therm {
    fn new(ambient: f64, max_rate: f64, tau: f64) -> Therm {
        Therm {
            current: ambient,
            target: 0.0,
            power: 0.0,
            max_rate,
            tau,
        }
    }

    fn step(&mut self, dt: f64, ambient: f64, runaway: bool) {
        if runaway {
            self.current += 5.0 * dt;
            self.power = 100.0;
            return;
        }
        let goal = if self.target > 0.0 {
            self.target
        } else {
            ambient
        };
        let want = (goal - self.current) * (1.0 - (-dt / self.tau).exp());
        let delta = want.clamp(-self.max_rate * dt, self.max_rate * dt);
        self.current += delta;
        self.power = if self.target > 0.0 && dt > 0.0 {
            (100.0 * (delta.max(0.0) / (self.max_rate * dt))).clamp(0.0, 100.0)
        } else {
            0.0
        };
    }
}

#[derive(Debug, Clone)]
struct SimPrint {
    name: String,
    lines: usize,
    step_s: f64,
    done_s: f64,
    layer_starts: Vec<usize>,
}

/// A printer that lives in memory: three heaters with a first-order model and a print that goes on as the clock does.
#[derive(Debug, Clone)]
pub struct SimPrinter {
    t: f64,
    ambient: f64,
    heaters: [Therm; 3],
    has_chamber: bool,
    state: &'static str,
    print: Option<SimPrint>,
    fault: &'static str,
}

impl SimPrinter {
    pub fn new(has_chamber: bool) -> SimPrinter {
        let ambient = 22.0;
        SimPrinter {
            t: 0.0,
            ambient,
            heaters: [
                Therm::new(ambient, 6.0, 15.0),
                Therm::new(ambient, 1.5, 90.0),
                Therm::new(ambient, 0.3, 200.0),
            ],
            has_chamber,
            state: "idle",
            print: None,
            fault: "none",
        }
    }

    /// The clock goes on by `dt` seconds: the heaters move and the print gets on.
    pub fn advance(&mut self, dt: f64) {
        let mut left = dt.max(0.0);
        while left > 0.0 {
            let step = if left > 100.0 { 10.0 } else { left.min(1.0) };
            for (i, h) in self.heaters.iter_mut().enumerate() {
                h.step(
                    step,
                    self.ambient,
                    i == 0 && self.fault == "thermal_runaway",
                );
            }
            if self.state == "printing" {
                if let Some(p) = &mut self.print {
                    p.done_s += step;
                    if p.done_s >= p.lines as f64 * p.step_s {
                        self.state = "finished";
                        for h in &mut self.heaters {
                            h.target = 0.0;
                        }
                    }
                }
            }
            self.t += step;
            left -= step;
        }
    }

    pub fn set_fault(&mut self, fault: &str) {
        self.fault = match fault {
            "thermal_runaway" => "thermal_runaway",
            "sensor_lost" => "sensor_lost",
            "disconnect" => "disconnect",
            _ => "none",
        };
    }

    pub fn disconnected(&self) -> bool {
        self.fault == "disconnect"
    }

    pub fn start_print(&mut self, name: &str, lines: usize, step_s: f64, layer_starts: Vec<usize>) {
        self.print = Some(SimPrint {
            name: name.to_string(),
            lines,
            step_s,
            done_s: 0.0,
            layer_starts,
        });
        self.state = "printing";
    }

    pub fn pause(&mut self) -> bool {
        if self.state == "printing" {
            self.state = "paused";
            return true;
        }
        false
    }

    pub fn resume(&mut self) -> bool {
        if self.state == "paused" {
            self.state = "printing";
            return true;
        }
        false
    }

    pub fn cancel(&mut self) {
        if matches!(self.state, "printing" | "paused") {
            self.state = "cancelled";
        }
        for h in &mut self.heaters {
            h.target = 0.0;
        }
    }

    pub fn set_target(&mut self, heater: &str, target: f64) -> bool {
        match heater_index(heater) {
            Some(2) if !self.has_chamber => false,
            Some(i) => {
                self.heaters[i].target = target;
                true
            }
            None => false,
        }
    }

    pub fn emergency_stop(&mut self) {
        self.state = "emergency";
        for h in &mut self.heaters {
            h.target = 0.0;
        }
    }

    /// The printer is brought back from an emergency stop.
    pub fn reset(&mut self) {
        if self.state == "emergency" {
            self.state = "idle";
            self.print = None;
        }
    }

    pub fn state(&self) -> PrinterState {
        let mut heaters = Vec::new();
        for (i, name) in ["hotend", "bed", "chamber"].into_iter().enumerate() {
            if i == 2 && !self.has_chamber {
                continue;
            }
            let h = &self.heaters[i];
            let lost = i == 0 && self.fault == "sensor_lost";
            heaters.push(Heater {
                name,
                current_c: if lost {
                    -1.0
                } else {
                    (h.current * 10.0).round() / 10.0
                },
                target_c: h.target,
                power_pct: (h.power * 10.0).round() / 10.0,
            });
        }
        let mut job = PrinterJob::default();
        if let Some(p) = &self.print {
            let total = p.lines as f64 * p.step_s;
            let fraction = if total > 0.0 {
                (p.done_s / total).min(1.0)
            } else {
                1.0
            };
            let line = (fraction * p.lines as f64) as usize;
            job.name = p.name.clone();
            job.progress_pct = (fraction * 1000.0).round() / 10.0;
            job.elapsed_s = p.done_s.min(total);
            if !p.layer_starts.is_empty() {
                job.layers = Some(p.layer_starts.len() as u64);
                job.layer = Some(p.layer_starts.iter().filter(|s| **s <= line).count() as u64);
            }
            job.z_mm = job.layer.map(|l| l as f64 * 0.2);
        }
        PrinterState {
            state: self.state,
            raw: self.state.to_string(),
            heaters,
            job,
            t_s: self.t,
        }
    }
}

/// The simulator as a driver: the clock goes on by the time that passed since the last call, times the rate (a rate of 0 is no waiting).
pub struct SimDriver {
    inner: Mutex<(SimPrinter, Instant)>,
    rate: f64,
    step_s: f64,
}

impl SimDriver {
    pub fn new(cfg: &PrinterConfig) -> SimDriver {
        SimDriver {
            inner: Mutex::new((SimPrinter::new(cfg.limits[2] > 0.0), Instant::now())),
            rate: cfg.sim_rate,
            step_s: cfg.step_ms / 1000.0,
        }
    }

    fn tick(&self) -> std::sync::MutexGuard<'_, (SimPrinter, Instant)> {
        let mut g = self.inner.locked();
        let real = g.1.elapsed().as_secs_f64();
        g.1 = Instant::now();
        let dt = if self.rate <= 0.0 {
            1.0e6
        } else {
            real * self.rate
        };
        g.0.advance(dt);
        g
    }
}

impl PrinterDriver for SimDriver {
    fn state(&self) -> DriverResult<PrinterState> {
        let g = self.tick();
        if g.0.disconnected() {
            return Err(DriverError::Disconnected(
                "the simulated printer does not answer".into(),
            ));
        }
        Ok(g.0.state())
    }

    fn start_print(&self, name: &str, text: &str) -> DriverResult<()> {
        let mut g = self.tick();
        if g.0.disconnected() {
            return Err(DriverError::Disconnected(
                "the simulated printer does not answer".into(),
            ));
        }
        if matches!(g.0.state, "printing" | "paused" | "emergency") {
            return Err(DriverError::Refused("the printer is busy".into()));
        }
        let program = gcode::parse(text, 0, false).map_err(|e| DriverError::Refused(e.detail))?;
        let starts: Vec<usize> = {
            let marks: Vec<usize> = text
                .lines()
                .enumerate()
                .filter(|(_, l)| {
                    l.trim().starts_with(";LAYER_CHANGE") || l.trim().starts_with(";LAYER:")
                })
                .map(|(n, _)| n + 1)
                .collect();
            marks
                .iter()
                .map(|m| {
                    program
                        .entries
                        .iter()
                        .position(|e| e.raw + 1 >= *m)
                        .unwrap_or(program.entries.len())
                })
                .collect()
        };
        g.0.start_print(name, program.entries.len(), self.step_s, starts);
        Ok(())
    }

    fn pause(&self) -> DriverResult<()> {
        let mut g = self.tick();
        g.0.pause()
            .then_some(())
            .ok_or_else(|| DriverError::Refused("the printer is not printing".into()))
    }

    fn resume(&self) -> DriverResult<()> {
        let mut g = self.tick();
        g.0.resume()
            .then_some(())
            .ok_or_else(|| DriverError::Refused("the printer is not held".into()))
    }

    fn cancel(&self) -> DriverResult<()> {
        self.tick().0.cancel();
        Ok(())
    }

    fn set_target(&self, heater: &str, target_c: f64) -> DriverResult<()> {
        let mut g = self.tick();
        g.0.set_target(heater, target_c)
            .then_some(())
            .ok_or_else(|| DriverError::Refused("the printer has no such heater".into()))
    }

    fn emergency_stop(&self) -> DriverResult<()> {
        self.tick().0.emergency_stop();
        Ok(())
    }

    fn gcode(&self, line: &str) -> DriverResult<()> {
        let words = gcode::words(&gcode::strip(line));
        let m = words.iter().find(|w| w.0 == 'M').map(|w| w.1 as i32);
        let s = words.iter().find(|w| w.0 == 'S').map(|w| w.1);
        if let (Some(m), Some(s)) = (m, s) {
            let heater = match m {
                104 | 109 => "hotend",
                140 | 190 => "bed",
                141 | 191 => "chamber",
                _ => return Ok(()),
            };
            return self.set_target(heater, s);
        }
        Ok(())
    }

    fn simulate(&self, fault: &str) {
        self.inner.locked().0.set_fault(fault);
    }

    fn reset(&self) -> DriverResult<()> {
        self.tick().0.reset();
        Ok(())
    }
}

// ---- the engine ------------------------------------------------------------------------------------------------------------------------

/// How a driver is made for a machine: the engine asks for one per machine and keeps it.
pub type DriverFactory =
    Box<dyn Fn(&PrinterConfig) -> PluginResult<Arc<dyn PrinterDriver>> + Send + Sync>;

fn real_driver(cfg: &PrinterConfig) -> PluginResult<Arc<dyn PrinterDriver>> {
    let timeout = Duration::from_millis((cfg.poll_ms as u64).clamp(500, 5000));
    Ok(match &cfg.connection {
        Connection::Demo => Arc::new(SimDriver::new(cfg)),
        Connection::Moonraker { url, api_key } => Arc::new(MoonrakerDriver::new(
            TcpHttp::new(url, timeout).map_err(PluginError::Bad)?,
            api_key.clone(),
        )),
        Connection::OctoPrint { url, api_key } => Arc::new(OctoPrintDriver::new(
            TcpHttp::new(url, timeout).map_err(PluginError::Bad)?,
            api_key.clone(),
        )),
        Connection::Marlin { .. } => Arc::new(MarlinDriver::new(SimSerial::new(
            (cfg.limits[0] + 15.0).min(450.0),
            (cfg.limits[1] + 10.0).min(150.0),
            cfg.step_ms / 1000.0,
            cfg.sim_rate,
        ))),
        Connection::BambuLan {
            serial,
            fingerprint,
            ..
        } => {
            // a layer takes a hundred steps in the simulation
            let link = SimBambu::new(
                serial,
                cfg.limits[0],
                cfg.limits[1],
                cfg.step_ms / 10.0,
                cfg.sim_rate,
            );
            let driver =
                BambuDriver::connect(link, fingerprint.as_deref()).map_err(|e| match e {
                    ConnectError::Untrusted(why) => {
                        PluginError::Coded("printer_untrusted", why.text())
                    }
                    ConnectError::Down(text) => disconnected(&text),
                })?;
            Arc::new(driver)
        }
    })
}

/// What the engine remembers of one heater to tell a heater that works from one that does not.
#[derive(Debug, Clone, Default)]
struct Watch {
    reached: bool,
    out_since: Option<f64>,
    heating_since: Option<f64>,
    last_target: f64,
}

#[derive(Default)]
struct MachineRt {
    driver: Option<(String, Arc<dyn PrinterDriver>)>,
    /// Why the machine is latched, when it is.
    latched: Option<String>,
    watch: HashMap<&'static str, Watch>,
    history: VecDeque<(f64, f64, f64)>,
    /// What the last poll found: the state, or nothing when the printer did not answer.
    last: Option<PrinterState>,
    /// When the printer was last asked.
    polled: Option<Instant>,
}

struct PJob {
    machine: String,
    kind: &'static str,
    name: String,
    program: Option<String>,
    commands: Vec<String>,
    preheat: Vec<(&'static str, f64)>,
    state: WorkState,
    message: String,
    started: Option<Instant>,
    seen_printing: bool,
    missed: u32,
    done: u64,
    total: u64,
}

/// The 3D printer machines of the server.
pub struct Printer3dEngine {
    jobs: Mutex<HashMap<String, PJob>>,
    rt: Mutex<HashMap<String, MachineRt>>,
    factory: DriverFactory,
}

impl Default for Printer3dEngine {
    fn default() -> Self {
        Printer3dEngine::with_factory(Box::new(real_driver))
    }
}

fn disconnected(text: &str) -> PluginError {
    Fault {
        code: "printer_disconnected",
        detail: format!("the printer does not answer: {text}"),
    }
    .into()
}

fn latched_error(why: &str) -> PluginError {
    Fault {
        code: "printer_latched",
        detail: format!("the machine is latched ({why}): an administrator has to release it"),
    }
    .into()
}

fn from_driver(e: DriverError) -> PluginError {
    match e {
        DriverError::Disconnected(t) => disconnected(&t),
        DriverError::Refused(t) => PluginError::Failed(format!("the printer refused: {t}")),
    }
}

impl Printer3dEngine {
    pub fn with_factory(factory: DriverFactory) -> Printer3dEngine {
        Printer3dEngine {
            jobs: Mutex::new(HashMap::new()),
            rt: Mutex::new(HashMap::new()),
            factory,
        }
    }

    fn connection_key(cfg: &PrinterConfig) -> String {
        format!(
            "{:?}|{}|{}|{}",
            cfg.connection,
            cfg.step_ms,
            cfg.sim_rate,
            cfg.limits[2] > 0.0
        )
    }

    fn driver(&self, machine: &str, cfg: &PrinterConfig) -> PluginResult<Arc<dyn PrinterDriver>> {
        let key = Self::connection_key(cfg);
        {
            let rt = self.rt.locked();
            if let Some((k, d)) = rt.get(machine).and_then(|m| m.driver.as_ref()) {
                if *k == key {
                    return Ok(d.clone());
                }
            }
        }
        let d = (self.factory)(cfg)?;
        self.rt
            .locked()
            .entry(machine.to_string())
            .or_default()
            .driver = Some((key, d.clone()));
        Ok(d)
    }

    pub fn busy(&self, machine: &str) -> bool {
        self.jobs
            .locked()
            .values()
            .any(|j| j.machine == machine && j.state.active())
    }

    pub fn latched(&self, machine: &str) -> Option<String> {
        self.rt
            .locked()
            .get(machine)
            .and_then(|m| m.latched.clone())
    }

    /// Latches the machine, stops the printer and fails the job that was open on it.
    fn latch(&self, machine: &str, driver: &Arc<dyn PrinterDriver>, fault: &Fault) {
        let _ = driver.emergency_stop();
        self.rt
            .locked()
            .entry(machine.to_string())
            .or_default()
            .latched = Some(format!("{}: {}", fault.code, fault.detail));
        for j in self
            .jobs
            .locked()
            .values_mut()
            .filter(|j| j.machine == machine && j.state.active())
        {
            j.state = WorkState::Failed;
            j.message = format!("{}: {}", fault.code, fault.detail);
        }
    }

    /// Asks the printer and looks at what it says. A printer that cannot be reached is an `Err`; a heater that is not as it should be stops the printer and latches the machine.
    fn poll(&self, machine: &str, cfg: &PrinterConfig) -> PluginResult<PrinterState> {
        let driver = self.driver(machine, cfg)?;
        driver.simulate(&cfg.sim_fault);
        let state = match driver.state() {
            Ok(state) => state,
            Err(e) => {
                let mut rt = self.rt.locked();
                let m = rt.entry(machine.to_string()).or_default();
                m.last = None;
                m.polled = Some(Instant::now());
                return Err(from_driver(e));
            }
        };
        let found = self.watch(machine, cfg, &state, &driver);
        {
            let mut rt = self.rt.locked();
            let m = rt.entry(machine.to_string()).or_default();
            let at = |n: &str| state.heater(n).map_or(0.0, |h| h.current_c);
            if m.history
                .back()
                .is_none_or(|(t, _, _)| *t + 0.5 <= state.t_s)
            {
                m.history.push_back((state.t_s, at("hotend"), at("bed")));
                while m.history.len() > HISTORY {
                    m.history.pop_front();
                }
            }
            m.last = Some(state.clone());
            m.polled = Some(Instant::now());
        }
        if let Some((fault, latch)) = found {
            if latch {
                self.latch(machine, &driver, &fault);
            } else {
                for h in &state.heaters {
                    let _ = driver.set_target(h.name, 0.0);
                }
                for j in self
                    .jobs
                    .locked()
                    .values_mut()
                    .filter(|j| j.machine == machine && j.state.active())
                {
                    j.state = WorkState::Failed;
                    j.message = format!("{}: {}", fault.code, fault.detail);
                }
            }
        }
        Ok(state)
    }

    /// The checks of the heaters. Gives the fault and whether it latches the machine.
    fn watch(
        &self,
        machine: &str,
        cfg: &PrinterConfig,
        st: &PrinterState,
        driver: &Arc<dyn PrinterDriver>,
    ) -> Option<(Fault, bool)> {
        let mut rt = self.rt.locked();
        let m = rt.entry(machine.to_string()).or_default();
        for h in &st.heaters {
            let limit = cfg.limit(h.name);
            let w = m.watch.entry(h.name).or_default();
            if h.current_c < cfg.min_temp_c {
                return Some((
                    Fault {
                        code: "sensor_fault",
                        detail: format!(
                            "the {} reads {} C: the sensor is broken or unplugged",
                            h.name,
                            num(h.current_c)
                        ),
                    },
                    true,
                ));
            }
            if h.target_c > limit {
                let _ = driver.set_target(h.name, 0.0);
                return Some((
                    Fault { code: "temperature_over_limit", detail: format!("the {} was set to {} C past its limit of {} C by something outside the server", h.name, num(h.target_c), num(limit)) },
                    true,
                ));
            }
            if limit > 0.0 && h.current_c > limit + 10.0 {
                return Some((
                    Fault {
                        code: "thermal_runaway",
                        detail: format!(
                            "the {} reads {} C, past its limit of {} C",
                            h.name,
                            num(h.current_c),
                            num(limit)
                        ),
                    },
                    true,
                ));
            }
            if h.target_c > 0.0 {
                if (h.target_c - w.last_target).abs() > 1e-9 {
                    *w = Watch {
                        last_target: h.target_c,
                        heating_since: Some(st.t_s),
                        ..Watch::default()
                    };
                }
                let off = (h.current_c - h.target_c).abs();
                if off <= AT_TARGET_C {
                    w.reached = true;
                    w.out_since = None;
                    w.heating_since = None;
                } else if w.reached {
                    if off > cfg.temp_deviation_c {
                        let since = *w.out_since.get_or_insert(st.t_s);
                        if st.t_s - since >= cfg.deviation_s {
                            return Some((
                                Fault {
                                    code: "thermal_runaway",
                                    detail: format!(
                                        "the {} is {} C from its target of {} C for {} s",
                                        h.name,
                                        num(off),
                                        num(h.target_c),
                                        num(cfg.deviation_s)
                                    ),
                                },
                                true,
                            ));
                        }
                    } else {
                        w.out_since = None;
                    }
                } else if let Some(since) = w.heating_since {
                    if st.t_s - since > cfg.heat_timeout_s {
                        return Some((
                            Fault {
                                code: "heater_timeout",
                                detail: format!(
                                    "the {} did not get to {} C in {} s",
                                    h.name,
                                    num(h.target_c),
                                    num(cfg.heat_timeout_s)
                                ),
                            },
                            false,
                        ));
                    }
                }
            } else {
                *w = Watch::default();
            }
        }
        None
    }

    /// What a client sees of the machine besides its state.
    pub fn detail(&self, machine: &str, cfg: &PrinterConfig) -> Value {
        // the list of machines asks for this often: a printer that was asked less than `poll_ms` ago is not asked again
        let cached = {
            let rt = self.rt.locked();
            rt.get(machine).and_then(|m| match m.polled {
                Some(at) if at.elapsed() < Duration::from_millis(cfg.poll_ms as u64) => {
                    Some(m.last.clone())
                }
                _ => None,
            })
        };
        let state = match cached {
            Some(Some(s)) => Ok(s),
            Some(None) => Err(disconnected("no answer at the last poll")),
            None => self.poll(machine, cfg),
        };
        let reachable = state.is_ok();
        let latched = self.latched(machine);
        let mut v = json!({
            "model": cfg.model,
            "connection": cfg.connection.name(),
            "online": reachable,
            "latched": latched.is_some(),
            "latched_by": latched,
            "limits_c": { "hotend": cfg.limits[0], "bed": cfg.limits[1], "chamber": cfg.limits[2] },
            "auto_start_queue": cfg.auto_start_queue,
            "compat": cfg.compat,
            "compat_start": cfg.compat_start,
        });
        if let Connection::BambuLan { fingerprint, .. } = &cfg.connection {
            v["bambu_lan"] =
                json!({"experimental": true, "fingerprint_pinned": fingerprint.is_some()});
        }
        if let Ok(s) = state {
            v["state"] = json!(s.state);
            v["temperatures"] = json!(s
                .heaters
                .iter()
                .map(|h| json!({"name": h.name, "current_c": h.current_c, "target_c": h.target_c}))
                .collect::<Vec<_>>());
            v["job"] = json!({"name": s.job.name, "progress_pct": s.job.progress_pct, "layer": s.job.layer, "layers": s.job.layers});
        }
        v
    }

    pub fn prepare(
        &self,
        machine: &str,
        cfg: &PrinterConfig,
        request: &Value,
    ) -> PluginResult<Prepared> {
        if let Some(why) = self.latched(machine) {
            return Err(latched_error(&why));
        }
        let kind = request.get("kind").and_then(Value::as_str).unwrap_or("");
        let mut job = PJob {
            machine: machine.to_string(),
            kind: "print",
            name: request
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty() && n.len() <= 80)
                .unwrap_or("part")
                .to_string(),
            program: None,
            commands: Vec::new(),
            preheat: Vec::new(),
            state: WorkState::Queued,
            message: String::new(),
            started: None,
            seen_printing: false,
            missed: 0,
            done: 0,
            total: 100,
        };
        let view = match kind {
            "print" => {
                let text = request
                    .get("artifact_text")
                    .and_then(Value::as_str)
                    .or_else(|| request.get("text").and_then(Value::as_str))
                    .ok_or_else(|| PluginError::Bad("a print request brings the program in `text` or names a G-code artifact in `artifact`".into()))?;
                let scan = scan_program(text, cfg)?;
                if let Some(a) = request.get("artifact").and_then(Value::as_str) {
                    job.name = format!("{a}.gcode");
                } else if !job.name.ends_with(".gcode") {
                    job.name.push_str(".gcode");
                }
                job.program = Some(text.to_string());
                if request
                    .get("preheat")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    job.preheat = vec![
                        (
                            "hotend",
                            scan.max_hotend_c
                                .max(cfg.preheat_hotend_c)
                                .min(cfg.limits[0]),
                        ),
                        (
                            "bed",
                            scan.max_bed_c.max(cfg.preheat_bed_c).min(cfg.limits[1]),
                        ),
                    ];
                }
                json!({
                    "kind": "print", "name": job.name, "lines": scan.lines, "layers": scan.layers, "max_hotend_c": scan.max_hotend_c,
                    "max_bed_c": scan.max_bed_c, "max_feed_mm_s": scan.max_feed_mm_s, "travel_mm": round2(scan.travel_mm),
                    "estimate_s": round2(scan.estimate_s), "preheat": request.get("preheat").and_then(Value::as_bool).unwrap_or(false),
                })
            }
            "preheat" => {
                job.kind = "preheat";
                for (name, key, default) in [
                    ("hotend", "hotend_c", cfg.preheat_hotend_c),
                    ("bed", "bed_c", cfg.preheat_bed_c),
                ] {
                    let asked = request.get(key);
                    let value = match asked {
                        None | Some(Value::Null) => {
                            if request.get("hotend_c").is_none() && request.get("bed_c").is_none() {
                                default
                            } else {
                                continue;
                            }
                        }
                        Some(v) => v
                            .as_f64()
                            .filter(|n| n.is_finite() && *n >= 0.0)
                            .ok_or_else(|| {
                                PluginError::Bad(format!("{key} is a number of degrees"))
                            })?,
                    };
                    if value > cfg.limit(name) {
                        return Err(Fault {
                            code: "temperature_over_limit",
                            detail: format!(
                                "{key} {} is over the limit of the machine ({} C)",
                                num(value),
                                num(cfg.limit(name))
                            ),
                        }
                        .into());
                    }
                    job.preheat.push((name, value));
                }
                if job.preheat.is_empty() {
                    return Err(PluginError::Bad(
                        "a preheat has hotend_c, bed_c or both".into(),
                    ));
                }
                json!({"kind": "preheat", "targets_c": job.preheat.iter().map(|(n, v)| json!({"heater": n, "target_c": v})).collect::<Vec<_>>()})
            }
            "gcode" => {
                job.kind = "gcode";
                let text = request.get("text").and_then(Value::as_str).ok_or_else(|| {
                    PluginError::Bad("a gcode request has the commands in `text`".into())
                })?;
                let lines: Vec<String> = text
                    .lines()
                    .map(gcode::strip)
                    .filter(|l| !l.is_empty())
                    .collect();
                if lines.is_empty() || lines.len() > MAX_COMMANDS {
                    return Err(PluginError::Bad(format!(
                        "a manual list has 1 to {MAX_COMMANDS} commands"
                    )));
                }
                scan_program(&lines.join("\n"), cfg)?;
                job.commands = lines;
                json!({"kind": "gcode", "commands": job.commands.len()})
            }
            _ => {
                return Err(PluginError::Bad(
                    "the request has to be {\"kind\": \"print\" | \"preheat\" | \"gcode\", ...}"
                        .into(),
                ))
            }
        };
        let native_id = crate::ids::new(crate::ids::Kind::Work);
        self.jobs.locked().insert(native_id.clone(), job);
        Ok(Prepared { native_id, view })
    }

    pub fn start(&self, machine: &str, cfg: &PrinterConfig, native_id: &str) -> PluginResult<()> {
        if let Some(why) = self.latched(machine) {
            return Err(latched_error(&why));
        }
        let (kind, name, program, commands, preheat) = {
            let jobs = self.jobs.locked();
            if jobs
                .values()
                .any(|j| j.machine == machine && j.state.active())
            {
                return Err(PluginError::Busy);
            }
            let job = jobs
                .get(native_id)
                .ok_or_else(|| PluginError::Bad("the job is not there".into()))?;
            if job.state != WorkState::Queued {
                return Err(PluginError::NotInState);
            }
            (
                job.kind,
                job.name.clone(),
                job.program.clone(),
                job.commands.clone(),
                job.preheat.clone(),
            )
        };
        let driver = self.driver(machine, cfg)?;
        let current = driver.state().map_err(from_driver)?;
        if matches!(
            current.state,
            "printing" | "paused" | "preparing" | "emergency"
        ) {
            return Err(PluginError::Busy);
        }
        for (heater, target) in &preheat {
            driver.set_target(heater, *target).map_err(from_driver)?;
        }
        match kind {
            "print" => driver
                .start_print(&name, program.as_deref().unwrap_or(""))
                .map_err(from_driver)?,
            "gcode" => {
                for line in &commands {
                    driver.gcode(line).map_err(from_driver)?;
                }
            }
            _ => {}
        }
        let mut jobs = self.jobs.locked();
        if let Some(job) = jobs.get_mut(native_id) {
            job.state = WorkState::Running;
            job.started = Some(Instant::now());
            job.program = None;
            if kind == "gcode" {
                job.state = WorkState::Done;
                job.done = job.total;
            }
        }
        Ok(())
    }

    pub fn pause(&self, machine: &str, cfg: &PrinterConfig, native_id: &str) -> PluginResult<()> {
        self.hold(machine, cfg, native_id, true)
    }

    pub fn resume(&self, machine: &str, cfg: &PrinterConfig, native_id: &str) -> PluginResult<()> {
        self.hold(machine, cfg, native_id, false)
    }

    fn hold(
        &self,
        machine: &str,
        cfg: &PrinterConfig,
        native_id: &str,
        pause: bool,
    ) -> PluginResult<()> {
        let (kind, state) = {
            let jobs = self.jobs.locked();
            let j = jobs
                .get(native_id)
                .ok_or_else(|| PluginError::Bad("the job is not there".into()))?;
            (j.kind, j.state)
        };
        if kind != "print" {
            return Err(PluginError::NotSupported("only a print can be held"));
        }
        if state
            != if pause {
                WorkState::Running
            } else {
                WorkState::Paused
            }
        {
            return Err(PluginError::NotInState);
        }
        let driver = self.driver(machine, cfg)?;
        if pause {
            driver.pause().map_err(from_driver)?;
        } else {
            driver.resume().map_err(from_driver)?;
        }
        if let Some(j) = self.jobs.locked().get_mut(native_id) {
            j.state = if pause {
                WorkState::Paused
            } else {
                WorkState::Running
            };
        }
        Ok(())
    }

    /// Stops a running or held job: the print is cancelled and the heaters go off. Not a latch (that is the emergency stop).
    pub fn stop(&self, machine: &str, cfg: &PrinterConfig, native_id: &str) -> PluginResult<()> {
        {
            let jobs = self.jobs.locked();
            let j = jobs
                .get(native_id)
                .ok_or_else(|| PluginError::Bad("the job is not there".into()))?;
            if !j.state.active() {
                return Err(PluginError::NotInState);
            }
        }
        let driver = self.driver(machine, cfg)?;
        let result = driver.cancel();
        for h in ["hotend", "bed", "chamber"] {
            let _ = driver.set_target(h, 0.0);
        }
        if let Some(j) = self.jobs.locked().get_mut(native_id) {
            j.state = WorkState::Cancelled;
            j.message = "stopped".into();
        }
        result.map_err(from_driver)
    }

    pub fn discard(&self, native_id: &str) {
        self.jobs.locked().remove(native_id);
    }

    pub fn status(
        &self,
        machine: &str,
        cfg: &PrinterConfig,
        native_id: &str,
    ) -> PluginResult<PluginStatus> {
        let (state, kind, preheat, started) = {
            let jobs = self.jobs.locked();
            let j = jobs.get(native_id).ok_or_else(|| {
                PluginError::Failed("the printer lost the job (the server restarted)".into())
            })?;
            (j.state, j.kind, j.preheat.clone(), j.started)
        };
        if state.active() {
            match self.poll(machine, cfg) {
                Err(_) => {
                    if let Some(j) = self.jobs.locked().get_mut(native_id) {
                        j.missed += 1;
                        j.message = "no answer from the printer".into();
                        if j.missed >= MISSED_POLLS && j.state.active() {
                            j.state = WorkState::Failed;
                            j.message =
                                "printer_disconnected: the printer stopped answering".into();
                        }
                    }
                }
                Ok(st) => {
                    let mut jobs = self.jobs.locked();
                    if let Some(j) = jobs.get_mut(native_id) {
                        j.missed = 0;
                        if j.state.active() {
                            apply_state(j, kind, &preheat, &st, started);
                        }
                    }
                }
            }
        }
        let jobs = self.jobs.locked();
        let j = jobs
            .get(native_id)
            .ok_or_else(|| PluginError::Failed("the printer lost the job".into()))?;
        Ok(PluginStatus {
            state: j.state,
            done: j.done,
            total: j.total,
            message: j.message.clone(),
        })
    }

    // ---- the operations that are not a job ----

    pub fn temperatures(&self, machine: &str, cfg: &PrinterConfig) -> PluginResult<Value> {
        let st = self.poll(machine, cfg)?;
        let heaters: Vec<Value> = st
            .heaters
            .iter()
            .map(|h| json!({"name": h.name, "current_c": h.current_c, "target_c": h.target_c, "power_pct": h.power_pct, "limit_c": cfg.limit(h.name)}))
            .collect();
        let history: Vec<Value> = self
            .rt
            .locked()
            .get(machine)
            .map(|m| {
                m.history
                    .iter()
                    .map(|(t, hot, bed)| json!({"t_s": round2(*t), "hotend_c": hot, "bed_c": bed}))
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({"heaters": heaters, "history": history, "t_s": st.t_s}))
    }

    /// Sets the target of a heater: refused above the limit of the machine, whoever asks.
    pub fn set_temperature(
        &self,
        machine: &str,
        cfg: &PrinterConfig,
        body: &Value,
    ) -> PluginResult<Value> {
        if let Some(why) = self.latched(machine) {
            return Err(latched_error(&why));
        }
        let heater = body
            .get("heater")
            .and_then(Value::as_str)
            .filter(|h| heater_index(h).is_some())
            .ok_or_else(|| {
                PluginError::from(Fault {
                    code: "bad_heater",
                    detail: "heater is hotend, bed or chamber".into(),
                })
            })?;
        let target = body
            .get("target_c")
            .and_then(Value::as_f64)
            .filter(|t| t.is_finite() && *t >= 0.0)
            .ok_or_else(|| PluginError::Bad("target_c is a number of degrees, 0 or more".into()))?;
        let limit = cfg.limit(heater);
        if target > limit {
            return Err(Fault {
                code: "temperature_over_limit",
                detail: format!(
                    "{} C is over the limit of the {} ({} C)",
                    num(target),
                    heater,
                    num(limit)
                ),
            }
            .into());
        }
        let driver = self.driver(machine, cfg)?;
        driver.set_target(heater, target).map_err(from_driver)?;
        Ok(json!({"heater": heater, "target_c": target, "limit_c": limit}))
    }

    pub fn layer(&self, machine: &str, cfg: &PrinterConfig) -> PluginResult<Value> {
        let st = self.poll(machine, cfg)?;
        let eta = if st.job.progress_pct > 0.5 && st.job.progress_pct < 100.0 {
            Some(round2(
                st.job.elapsed_s * (100.0 - st.job.progress_pct) / st.job.progress_pct,
            ))
        } else {
            None
        };
        Ok(
            json!({"layer": st.job.layer, "layers": st.job.layers, "z_mm": st.job.z_mm, "progress_pct": st.job.progress_pct, "eta_s": eta}),
        )
    }

    /// The emergency stop: heaters off and motors released at once, the job that was open ends, the machine is latched until an administrator releases it.
    pub fn emergency_stop(&self, machine: &str, cfg: &PrinterConfig) -> PluginResult<Value> {
        let driver = self.driver(machine, cfg)?;
        let reached = driver.emergency_stop();
        let mut stopped = false;
        for j in self
            .jobs
            .locked()
            .values_mut()
            .filter(|j| j.machine == machine && j.state.active())
        {
            j.state = WorkState::Cancelled;
            j.message =
                "emergency stop: the machine is latched until an administrator releases it".into();
            stopped = true;
        }
        self.rt
            .locked()
            .entry(machine.to_string())
            .or_default()
            .latched = Some("emergency stop".into());
        match reached {
            Ok(()) => Ok(json!({"stopped": stopped, "latched": true, "printer_reached": true})),
            Err(e) => Ok(
                json!({"stopped": stopped, "latched": true, "printer_reached": false, "why": e.text()}),
            ),
        }
    }

    /// An administrator looked at the machine: the latch is released and the printer is told to start again from the emergency stop. A printer that cannot be reached is
    /// released all the same, and the answer says that it was not reset.
    pub fn release(&self, machine: &str, cfg: &PrinterConfig) -> Value {
        let was = self
            .rt
            .locked()
            .entry(machine.to_string())
            .or_default()
            .latched
            .take();
        let reset = self
            .driver(machine, cfg)
            .map_err(|e| e.detail())
            .and_then(|d| d.reset().map_err(|e| e.text().to_string()));
        match reset {
            Ok(()) => json!({"released": was.is_some(), "latched": false, "printer_reset": true}),
            Err(why) => {
                json!({"released": was.is_some(), "latched": false, "printer_reset": false, "why": why})
            }
        }
    }

    /// The state of the printer in the shape of another program (`moonraker` or `octoprint`), when the machine is set to give it.
    pub fn compat(&self, machine: &str, cfg: &PrinterConfig, flavor: &str) -> PluginResult<Value> {
        let allowed = cfg.compat == flavor || cfg.compat == "both";
        if !allowed {
            return Err(PluginError::NotSupported(
                "the compatibility answer of this kind is off for the machine (setting compat)",
            ));
        }
        let st = self.poll(machine, cfg)?;
        Ok(if flavor == "moonraker" {
            moonraker_shape(&st)
        } else {
            octoprint_shape(&st)
        })
    }
}

/// Brings a job up to what the printer says.
fn apply_state(
    j: &mut PJob,
    kind: &str,
    preheat: &[(&'static str, f64)],
    st: &PrinterState,
    started: Option<Instant>,
) {
    match kind {
        "preheat" => {
            let mut fraction = 1.0f64;
            let mut all = true;
            for (heater, target) in preheat {
                let Some(h) = st.heater(heater) else { continue };
                if (h.current_c - target).abs() > AT_TARGET_C && *target > 0.0 {
                    all = false;
                    let span = (target - 22.0).max(1.0);
                    fraction = fraction.min(((h.current_c - 22.0) / span).clamp(0.0, 1.0));
                }
            }
            j.done = if all { 100 } else { (fraction * 100.0) as u64 };
            if all {
                j.state = WorkState::Done;
            }
        }
        "print" => {
            j.done = st.job.progress_pct.clamp(0.0, 100.0) as u64;
            match st.state {
                "printing" | "preparing" | "pausing" | "finishing" => {
                    j.seen_printing = true;
                    j.state = WorkState::Running;
                }
                "paused" => {
                    j.seen_printing = true;
                    j.state = WorkState::Paused;
                }
                "finished" => {
                    j.state = WorkState::Done;
                    j.done = 100;
                }
                "cancelled" | "cancelling" => {
                    j.state = WorkState::Cancelled;
                    j.message = "the printer cancelled the print".into();
                }
                "failed" | "error" => {
                    j.state = WorkState::Failed;
                    j.message = format!("the printer reports an error ({})", st.raw);
                }
                "emergency" => {
                    j.state = WorkState::Cancelled;
                    j.message = "the printer is in emergency".into();
                }
                "idle" | "offline" => {
                    if j.seen_printing {
                        j.state = WorkState::Done;
                        j.done = 100;
                    } else if started.is_some_and(|s| s.elapsed() > Duration::from_secs(30)) {
                        j.state = WorkState::Failed;
                        j.message = "the printer did not start the print".into();
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer_drivers::fakes::FakePrinter;
    use crate::printer_drivers::{Http, HttpRequest, HttpResponse};

    const PART: &str = "G28\nM104 S200\nM140 S60\n;LAYER_CHANGE\nG1 X10 Y10 Z0.2 F3000\nG1 X20 Y10\n;LAYER_CHANGE\nG1 X20 Y20 Z0.4\nG1 X10 Y20\n;LAYER_CHANGE\nG1 X10 Y10 Z0.6\nM104 S0\nM140 S0\n";

    fn quick() -> PrinterConfig {
        PrinterConfig {
            sim_rate: 0.0,
            step_ms: 20.0,
            ..PrinterConfig::default()
        }
    }

    fn engine() -> Printer3dEngine {
        Printer3dEngine::default()
    }

    fn print(text: &str) -> Value {
        json!({"kind": "print", "text": text, "name": "part"})
    }

    #[test]
    fn the_profile_of_the_model_gives_the_limits_and_a_setting_changes_them_inside_the_hard_maximum(
    ) {
        let g = PrinterConfig::from_settings(&json!({})).unwrap();
        assert_eq!(
            (g.model.as_str(), g.limits),
            ("generic", [260.0, 100.0, 60.0])
        );
        assert_eq!(g, PrinterConfig::default());
        assert!(!g.auto_start_queue && g.compat == "off" && g.connection == Connection::Demo);
        let a1 = PrinterConfig::from_settings(&json!({"model": "bambu_a1"})).unwrap();
        assert_eq!(a1.limits, [300.0, 100.0, 0.0]);
        let p1s =
            PrinterConfig::from_settings(&json!({"model": "bambu_p1s", "bed_max_c": 90})).unwrap();
        assert_eq!(p1s.limits, [300.0, 90.0, 0.0]);
        assert_eq!(
            PrinterConfig::from_settings(&json!({"model": "bambu_a1_mini"}))
                .unwrap()
                .limits[1],
            80.0
        );
        assert_eq!(p1s.limit("bed"), 90.0);
        assert_eq!(p1s.limit("nothing"), 0.0);
        for bad in [
            json!({"model": "mystery"}),
            json!({"model": 5}),
            json!({"hotend_max_c": 451}),
            json!({"bed_max_c": 151}),
            json!({"chamber_max_c": 101}),
            json!({"heat_timeout_s": 10}),
            json!({"temp_deviation_c": 0}),
            json!({"deviation_s": 500}),
            json!({"min_temp_c": 80}),
            json!({"max_feed_mm_s": 0}),
            json!({"bed_x_mm": 5}),
            json!({"poll_ms": 10}),
            json!({"auto_start_queue": "yes"}),
            json!({"compat": "all"}),
            json!({"sim_fault": "fire"}),
            json!({"preheat_default_hotend_c": 300}),
            json!({"zzz": 1}),
            json!({"connection": {"kind": "bambu_lan", "url": "http://x"}}),
            json!({"connection": {"kind": "moonraker"}}),
            json!({"connection": {"kind": "moonraker", "url": "https://x:7125"}}),
            json!({"connection": {"kind": "octoprint", "url": "http://x", "api_key": 5}}),
            json!({"connection": {"kind": "usb"}}),
            json!({"connection": 5}),
        ] {
            assert!(PrinterConfig::from_settings(&bad).is_err(), "{bad}");
        }
        let moon = PrinterConfig::from_settings(&json!({"connection": {"kind": "moonraker", "url": "http://192.168.1.50:7125", "api_key": "k"}, "compat": "both"})).unwrap();
        assert_eq!(
            moon.connection,
            Connection::Moonraker {
                url: "http://192.168.1.50:7125".into(),
                api_key: Some("k".into())
            }
        );
        assert_eq!(SETTINGS_HELP.len(), 22);
    }

    #[test]
    fn a_program_is_read_before_it_is_accepted() {
        let cfg = PrinterConfig {
            bed: [Some(220.0), Some(220.0), Some(250.0)],
            ..PrinterConfig::default()
        };
        let scan = scan_program(PART, &cfg).unwrap();
        assert_eq!(
            (scan.layers, scan.max_hotend_c, scan.max_bed_c),
            (3, 200.0, 60.0)
        );
        assert_eq!(scan.layer_starts.len(), 3);
        assert!(scan.travel_mm > 20.0 && scan.estimate_s > 0.0);
        assert_eq!(scan.max_feed_mm_s, 50.0);
        for (text, code) in [
            ("M104 S270\n", "program_over_limit"),
            ("M109 S300\n", "program_over_limit"),
            ("M140 S101\n", "program_over_limit"),
            ("M190 S120\n", "program_over_limit"),
            ("M141 S61\n", "program_over_limit"),
            ("M191 S99\n", "program_over_limit"),
            ("G1 X10 F20000\n", "program_over_limit"),
            ("M112\n", "bad_gcode"),
            ("G1 X300 F600\n", "out_of_travel"),
            ("G1 Z-1 F600\n", "out_of_travel"),
            ("$H\n", "bad_gcode"),
            ("(nothing)\n", "no_work"),
        ] {
            let e = scan_program(text, &cfg).unwrap_err();
            assert_eq!(e.code, code, "{text}");
        }
        assert!(
            scan_program("M104 S260\nM140 S100\nM141 S60\n", &cfg).is_ok(),
            "the limits themselves are allowed"
        );
        // homing puts the axes back to zero and a relative move adds up
        assert!(scan_program("G1 X200 F600\nG28 X\nG91\nG1 X200 F600\n", &cfg).is_ok());
        assert_eq!(
            scan_program("G1 X200 F600\nG91\nG1 X30 F600\n", &cfg)
                .unwrap_err()
                .code,
            "out_of_travel"
        );
        let inches = scan_program("G20\nG1 X1 F60\n", &cfg).unwrap();
        assert!((inches.travel_mm - 25.4).abs() < 1e-9);
    }

    #[test]
    fn the_simulated_heater_follows_a_first_order_curve_and_cools_down() {
        let mut p = SimPrinter::new(true);
        assert_eq!(p.state().heater("hotend").unwrap().current_c, 22.0);
        assert!(p.set_target("hotend", 200.0));
        p.advance(5.0);
        let a = p.state().heater("hotend").unwrap().current_c;
        assert!(a > 22.0 && a < 200.0, "{a}");
        p.advance(300.0);
        let b = p.state().heater("hotend").unwrap().current_c;
        assert!((b - 200.0).abs() < 1.0, "{b}");
        assert!(
            p.state().heater("hotend").unwrap().power_pct < 5.0,
            "at its target the heater barely works"
        );
        p.set_target("hotend", 0.0);
        p.advance(30.0);
        let c = p.state().heater("hotend").unwrap().current_c;
        assert!(c < b - 30.0, "it cools: {c}");
        p.advance(2000.0);
        assert!((p.state().heater("hotend").unwrap().current_c - 22.0).abs() < 1.0);
        // the bed is slower than the hotend
        p.set_target("bed", 60.0);
        p.advance(20.0);
        assert!(p.state().heater("bed").unwrap().current_c < 40.0);
        p.advance(1000.0);
        assert!((p.state().heater("bed").unwrap().current_c - 60.0).abs() < 1.0);
        assert!(!p.set_target("nozzle", 1.0));
        let none = SimPrinter::new(false);
        assert!(none.state().heater("chamber").is_none());
        let mut no_chamber = none.clone();
        assert!(!no_chamber.set_target("chamber", 30.0));
    }

    #[test]
    fn the_simulated_faults_show_in_the_readings() {
        let mut p = SimPrinter::new(true);
        p.set_target("hotend", 200.0);
        p.set_fault("thermal_runaway");
        p.advance(100.0);
        assert!(
            p.state().heater("hotend").unwrap().current_c > 300.0,
            "the heater ignores its target"
        );
        let mut q = SimPrinter::new(true);
        q.set_fault("sensor_lost");
        assert_eq!(q.state().heater("hotend").unwrap().current_c, -1.0);
        q.set_fault("disconnect");
        assert!(q.disconnected());
        q.set_fault("whatever");
        assert!(!q.disconnected());
    }

    #[test]
    fn the_simulated_print_counts_layers_and_ends_when_its_lines_are_done() {
        let mut p = SimPrinter::new(true);
        p.set_target("hotend", 200.0);
        p.start_print("part.gcode", 100, 1.0, vec![0, 40, 80]);
        assert_eq!(p.state().state, "printing");
        p.advance(50.0);
        let s = p.state();
        assert_eq!(
            (s.job.progress_pct, s.job.layer, s.job.layers),
            (50.0, Some(2), Some(3))
        );
        assert!(p.pause());
        assert!(!p.pause());
        p.advance(500.0);
        assert_eq!(
            p.state().job.progress_pct,
            50.0,
            "a held print does not go on"
        );
        assert!(p.resume());
        assert!(!p.resume());
        p.advance(60.0);
        let done = p.state();
        assert_eq!((done.state, done.job.progress_pct), ("finished", 100.0));
        assert_eq!(
            done.heater("hotend").unwrap().target_c,
            0.0,
            "the heaters go off when the print ends"
        );
        let mut c = SimPrinter::new(true);
        c.start_print("x", 10, 1.0, Vec::new());
        c.cancel();
        assert_eq!(c.state().state, "cancelled");
        let mut e = SimPrinter::new(true);
        e.start_print("x", 10, 1.0, Vec::new());
        e.set_target("bed", 60.0);
        e.emergency_stop();
        assert_eq!(e.state().state, "emergency");
        assert_eq!(e.state().heater("bed").unwrap().target_c, 0.0);
    }

    #[test]
    fn a_print_waits_for_a_person_and_runs_to_the_end_on_the_simulator() {
        let e = engine();
        let cfg = quick();
        let p = e.prepare("p1", &cfg, &print(PART)).unwrap();
        assert_eq!(p.view["layers"], 3);
        assert_eq!(p.view["max_hotend_c"], 200.0);
        assert_eq!(
            e.status("p1", &cfg, &p.native_id).unwrap().state,
            WorkState::Queued
        );
        e.start("p1", &cfg, &p.native_id).unwrap();
        assert!(e.busy("p1"));
        let st = e.status("p1", &cfg, &p.native_id).unwrap();
        assert_eq!((st.state, st.done, st.total), (WorkState::Done, 100, 100));
        assert!(!e.busy("p1"));
        assert!(e.detail("p1", &cfg)["temperatures"].is_array());
        e.discard(&p.native_id);
        assert!(e.status("p1", &cfg, &p.native_id).is_err());
    }

    #[test]
    fn one_print_at_a_time_and_a_print_can_be_held_and_stopped() {
        let e = engine();
        let slow = PrinterConfig {
            sim_rate: 1.0,
            step_ms: 3_600_000.0,
            ..PrinterConfig::default()
        };
        let a = e.prepare("p1", &slow, &print(PART)).unwrap();
        let b = e.prepare("p1", &slow, &print(PART)).unwrap();
        e.start("p1", &slow, &a.native_id).unwrap();
        assert_eq!(
            e.start("p1", &slow, &b.native_id).unwrap_err(),
            PluginError::Busy
        );
        assert_eq!(
            e.start("p1", &slow, &a.native_id).unwrap_err(),
            PluginError::Busy
        );
        assert_eq!(
            e.status("p1", &slow, &a.native_id).unwrap().state,
            WorkState::Running
        );
        e.pause("p1", &slow, &a.native_id).unwrap();
        assert_eq!(
            e.status("p1", &slow, &a.native_id).unwrap().state,
            WorkState::Paused
        );
        assert_eq!(
            e.pause("p1", &slow, &a.native_id).unwrap_err(),
            PluginError::NotInState
        );
        e.resume("p1", &slow, &a.native_id).unwrap();
        assert_eq!(
            e.resume("p1", &slow, &a.native_id).unwrap_err(),
            PluginError::NotInState
        );
        e.stop("p1", &slow, &a.native_id).unwrap();
        let st = e.status("p1", &slow, &a.native_id).unwrap();
        assert_eq!(st.state, WorkState::Cancelled);
        assert!(e.latched("p1").is_none(), "a stop is not an emergency stop");
        assert_eq!(
            e.stop("p1", &slow, &a.native_id).unwrap_err(),
            PluginError::NotInState
        );
        e.start("p1", &slow, &b.native_id).unwrap();
    }

    #[test]
    fn the_emergency_stop_ends_the_job_latches_the_machine_and_only_a_release_frees_it() {
        let e = engine();
        let slow = PrinterConfig {
            sim_rate: 1.0,
            step_ms: 3_600_000.0,
            ..PrinterConfig::default()
        };
        let a = e.prepare("p1", &slow, &print(PART)).unwrap();
        let queued = e.prepare("p1", &slow, &print(PART)).unwrap();
        e.start("p1", &slow, &a.native_id).unwrap();
        e.set_temperature("p1", &slow, &json!({"heater": "bed", "target_c": 60}))
            .unwrap();
        let out = e.emergency_stop("p1", &slow).unwrap();
        assert_eq!(
            out,
            json!({"stopped": true, "latched": true, "printer_reached": true})
        );
        assert_eq!(
            e.status("p1", &slow, &a.native_id).unwrap().state,
            WorkState::Cancelled
        );
        assert!(e.latched("p1").is_some());
        for refused in [
            e.prepare("p1", &slow, &print(PART)).err().unwrap(),
            e.start("p1", &slow, &queued.native_id).unwrap_err(),
            e.set_temperature("p1", &slow, &json!({"heater": "bed", "target_c": 10}))
                .unwrap_err(),
        ] {
            assert_eq!(refused.code(), "printer_latched");
            assert_eq!(refused.status(), 409);
        }
        assert_eq!(e.detail("p1", &slow)["latched"], true);
        assert_eq!(
            e.release("p1", &slow),
            json!({"released": true, "latched": false, "printer_reset": true})
        );
        assert_eq!(
            e.release("p1", &slow),
            json!({"released": false, "latched": false, "printer_reset": true})
        );
        let again = e.prepare("p1", &slow, &print(PART)).unwrap();
        e.start("p1", &slow, &again.native_id).unwrap();
        // another machine was never latched
        assert!(e.prepare("p2", &slow, &print(PART)).is_ok());
    }

    #[test]
    fn a_target_over_the_limit_of_the_model_is_refused_whoever_asks() {
        let e = engine();
        let cfg = PrinterConfig::from_settings(&json!({"model": "bambu_a1"})).unwrap();
        let ok = e
            .set_temperature("p1", &cfg, &json!({"heater": "hotend", "target_c": 300}))
            .unwrap();
        assert_eq!(
            (ok["target_c"].clone(), ok["limit_c"].clone()),
            (json!(300.0), json!(300.0))
        );
        for (body, code) in [
            (
                json!({"heater": "hotend", "target_c": 301}),
                "temperature_over_limit",
            ),
            (
                json!({"heater": "bed", "target_c": 101}),
                "temperature_over_limit",
            ),
            (
                json!({"heater": "chamber", "target_c": 30}),
                "temperature_over_limit",
            ),
            (json!({"heater": "nozzle", "target_c": 30}), "bad_heater"),
            (json!({"target_c": 30}), "bad_heater"),
            (json!({"heater": "bed"}), "bad_plugin_request"),
            (
                json!({"heater": "bed", "target_c": -5}),
                "bad_plugin_request",
            ),
            (
                json!({"heater": "bed", "target_c": "hot"}),
                "bad_plugin_request",
            ),
        ] {
            assert_eq!(
                e.set_temperature("p1", &cfg, &body).unwrap_err().code(),
                code,
                "{body}"
            );
        }
        let t = e.temperatures("p1", &cfg).unwrap();
        assert_eq!(t["heaters"][0]["limit_c"], 300.0);
        assert!(t["history"].as_array().is_some());
    }

    #[test]
    fn a_preheat_job_heats_to_its_targets_and_ends_there() {
        let e = engine();
        let cfg = quick();
        let p = e
            .prepare(
                "p1",
                &cfg,
                &json!({"kind": "preheat", "hotend_c": 180, "bed_c": 50}),
            )
            .unwrap();
        assert_eq!(p.view["targets_c"].as_array().unwrap().len(), 2);
        e.start("p1", &cfg, &p.native_id).unwrap();
        let st = e.status("p1", &cfg, &p.native_id).unwrap();
        assert_eq!(
            st.state,
            WorkState::Done,
            "with no waiting the heaters are at their targets at once"
        );
        let defaults = e.prepare("p1", &cfg, &json!({"kind": "preheat"})).unwrap();
        assert_eq!(defaults.view["targets_c"][0]["target_c"], 200.0);
        assert_eq!(defaults.view["targets_c"][1]["target_c"], 60.0);
        let only = e
            .prepare("p1", &cfg, &json!({"kind": "preheat", "bed_c": 40}))
            .unwrap();
        assert_eq!(only.view["targets_c"].as_array().unwrap().len(), 1);
        let hot = e
            .prepare("p1", &cfg, &json!({"kind": "preheat", "hotend_c": 300}))
            .err()
            .unwrap();
        assert_eq!(hot.code(), "temperature_over_limit");
        assert_eq!(
            e.prepare("p1", &cfg, &json!({"kind": "preheat", "hotend_c": "x"}))
                .err()
                .unwrap()
                .code(),
            "bad_plugin_request"
        );
        let slow = PrinterConfig {
            sim_rate: 1.0,
            ..PrinterConfig::default()
        };
        let q = e
            .prepare("p3", &slow, &json!({"kind": "preheat", "hotend_c": 200}))
            .unwrap();
        e.start("p3", &slow, &q.native_id).unwrap();
        let st = e.status("p3", &slow, &q.native_id).unwrap();
        assert_eq!(
            st.state,
            WorkState::Running,
            "a real second is not enough to heat a hotend"
        );
        assert!(st.done < 100);
    }

    #[test]
    fn a_manual_command_list_sets_temperatures_within_the_limits() {
        let e = engine();
        let cfg = quick();
        let p = e
            .prepare(
                "p1",
                &cfg,
                &json!({"kind": "gcode", "text": "M104 S150\nM140 S50 ; bed\n"}),
            )
            .unwrap();
        assert_eq!(p.view["commands"], 2);
        e.start("p1", &cfg, &p.native_id).unwrap();
        assert_eq!(
            e.status("p1", &cfg, &p.native_id).unwrap().state,
            WorkState::Done
        );
        let t = e.temperatures("p1", &cfg).unwrap();
        assert_eq!(t["heaters"][0]["target_c"], 150.0);
        for (text, code) in [("M104 S999", "program_over_limit"), ("M112", "bad_gcode")] {
            assert_eq!(
                e.prepare("p1", &cfg, &json!({"kind": "gcode", "text": text}))
                    .err()
                    .unwrap()
                    .code(),
                code
            );
        }
        let many = "M105\n".repeat(51);
        assert_eq!(
            e.prepare("p1", &cfg, &json!({"kind": "gcode", "text": many}))
                .err()
                .unwrap()
                .code(),
            "bad_plugin_request"
        );
        assert_eq!(
            e.prepare("p1", &cfg, &json!({"kind": "gcode", "text": ""}))
                .err()
                .unwrap()
                .code(),
            "bad_plugin_request"
        );
        assert_eq!(
            e.prepare("p1", &cfg, &json!({"kind": "weld"}))
                .err()
                .unwrap()
                .code(),
            "bad_plugin_request"
        );
        assert_eq!(
            e.prepare("p1", &cfg, &json!({"kind": "print"}))
                .err()
                .unwrap()
                .code(),
            "bad_plugin_request"
        );
        let held = e
            .prepare("p1", &cfg, &json!({"kind": "gcode", "text": "M104 S10"}))
            .unwrap();
        assert_eq!(
            e.pause("p1", &cfg, &held.native_id).unwrap_err().code(),
            "not_supported"
        );
    }

    #[test]
    fn a_program_over_a_limit_is_refused_when_it_is_prepared() {
        let e = engine();
        let cfg = quick();
        let r = e
            .prepare("p1", &cfg, &print("M104 S280\nG1 X1 F600\n"))
            .err()
            .unwrap();
        assert_eq!((r.code(), r.status()), ("program_over_limit", 422));
        let artifact = e
            .prepare(
                "p1",
                &cfg,
                &json!({"kind": "print", "artifact": "a-1", "artifact_text": PART}),
            )
            .unwrap();
        assert_eq!(artifact.view["name"], "a-1.gcode");
    }

    #[test]
    fn a_broken_sensor_latches_the_machine_and_fails_the_job() {
        let e = engine();
        let ok = PrinterConfig {
            sim_rate: 1.0,
            step_ms: 3_600_000.0,
            ..PrinterConfig::default()
        };
        let p = e.prepare("p1", &ok, &print(PART)).unwrap();
        e.start("p1", &ok, &p.native_id).unwrap();
        let broken = PrinterConfig {
            sim_fault: "sensor_lost".into(),
            ..ok.clone()
        };
        let st = e.status("p1", &broken, &p.native_id).unwrap();
        assert_eq!(st.state, WorkState::Failed);
        assert!(st.message.starts_with("sensor_fault:"), "{}", st.message);
        assert!(e.latched("p1").unwrap().starts_with("sensor_fault"));
        assert_eq!(
            e.prepare("p1", &broken, &print(PART)).err().unwrap().code(),
            "printer_latched"
        );
    }

    #[test]
    fn a_heater_that_runs_away_past_its_limit_latches_the_machine() {
        let e = engine();
        let cfg = PrinterConfig {
            sim_rate: 100_000.0,
            sim_fault: "thermal_runaway".into(),
            ..PrinterConfig::default()
        };
        e.set_temperature("p2", &cfg, &json!({"heater": "hotend", "target_c": 200}))
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(e.temperatures("p2", &cfg).is_ok());
        let why = e
            .latched("p2")
            .expect("past its limit by more than 10 degrees: latched");
        assert!(why.starts_with("thermal_runaway"), "{why}");
        assert_eq!(
            e.prepare("p2", &cfg, &print(PART)).err().unwrap().code(),
            "printer_latched"
        );
    }

    type Readings = (f64, f64, f64, bool);

    /// A printer whose readings the test moves by hand: `(current, target, clock, printing)`.
    struct Scripted(Arc<Mutex<Readings>>);

    impl PrinterDriver for Scripted {
        fn state(&self) -> DriverResult<PrinterState> {
            let g = self.0.lock().unwrap();
            Ok(PrinterState {
                state: if g.3 { "printing" } else { "idle" },
                raw: "scripted".into(),
                heaters: vec![Heater {
                    name: "hotend",
                    current_c: g.0,
                    target_c: g.1,
                    power_pct: 0.0,
                }],
                job: PrinterJob::default(),
                t_s: g.2,
            })
        }

        fn start_print(&self, _: &str, _: &str) -> DriverResult<()> {
            self.0.lock().unwrap().3 = true;
            Ok(())
        }

        fn pause(&self) -> DriverResult<()> {
            Ok(())
        }

        fn resume(&self) -> DriverResult<()> {
            Ok(())
        }

        fn cancel(&self) -> DriverResult<()> {
            Ok(())
        }

        fn set_target(&self, _: &str, t: f64) -> DriverResult<()> {
            self.0.lock().unwrap().1 = t;
            Ok(())
        }

        fn emergency_stop(&self) -> DriverResult<()> {
            Ok(())
        }

        fn gcode(&self, _: &str) -> DriverResult<()> {
            Ok(())
        }
    }

    fn scripted(state: Readings) -> (Printer3dEngine, Arc<Mutex<Readings>>) {
        let shared = Arc::new(Mutex::new(state));
        let inner = shared.clone();
        (
            Printer3dEngine::with_factory(Box::new(move |_| {
                Ok(Arc::new(Scripted(inner.clone())) as Arc<dyn PrinterDriver>)
            })),
            shared,
        )
    }

    #[test]
    fn a_heater_that_drifts_off_its_target_latches_after_the_time_of_the_deviation() {
        let (eng, hand) = scripted((200.0, 200.0, 0.0, false));
        let cfg = PrinterConfig::default();
        let p = eng.prepare("h1", &cfg, &print(PART)).unwrap();
        eng.start("h1", &cfg, &p.native_id).unwrap();
        eng.status("h1", &cfg, &p.native_id).unwrap();
        let at = |cur: f64, t: f64| {
            let mut g = hand.lock().unwrap();
            g.0 = cur;
            g.2 = t;
        };
        // 20 degrees off for less than 10 seconds: nothing
        at(180.0, 5.0);
        eng.status("h1", &cfg, &p.native_id).unwrap();
        at(180.0, 9.0);
        assert_eq!(
            eng.status("h1", &cfg, &p.native_id).unwrap().state,
            WorkState::Running
        );
        // back inside for a moment: the count starts again
        at(199.0, 10.0);
        eng.status("h1", &cfg, &p.native_id).unwrap();
        at(150.0, 12.0);
        eng.status("h1", &cfg, &p.native_id).unwrap();
        assert!(eng.latched("h1").is_none());
        at(150.0, 23.0);
        let st = eng.status("h1", &cfg, &p.native_id).unwrap();
        assert_eq!(st.state, WorkState::Failed);
        assert!(st.message.starts_with("thermal_runaway:"), "{}", st.message);
        assert!(eng.latched("h1").is_some());
    }

    #[test]
    fn a_heater_that_does_not_get_to_its_target_stops_the_job_without_latching() {
        let (eng, cold) = scripted((22.0, 200.0, 0.0, false));
        let cfg = PrinterConfig {
            heat_timeout_s: 60.0,
            ..PrinterConfig::default()
        };
        let p = eng.prepare("c1", &cfg, &print(PART)).unwrap();
        eng.start("c1", &cfg, &p.native_id).unwrap();
        eng.status("c1", &cfg, &p.native_id).unwrap();
        cold.lock().unwrap().2 = 59.0;
        assert_eq!(
            eng.status("c1", &cfg, &p.native_id).unwrap().state,
            WorkState::Running
        );
        cold.lock().unwrap().2 = 61.0;
        let st = eng.status("c1", &cfg, &p.native_id).unwrap();
        assert_eq!(st.state, WorkState::Failed);
        assert!(st.message.starts_with("heater_timeout:"), "{}", st.message);
        assert!(
            eng.latched("c1").is_none(),
            "a heater that is slow is not a fire"
        );
        assert_eq!(cold.lock().unwrap().1, 0.0, "the heater was switched off");
    }

    #[test]
    fn a_target_set_past_the_limit_from_outside_is_switched_off_and_latches() {
        let (eng, outside) = scripted((200.0, 290.0, 0.0, false));
        let cfg = PrinterConfig::default();
        assert!(eng.temperatures("o1", &cfg).is_ok());
        assert_eq!(
            outside.lock().unwrap().1,
            0.0,
            "the target was put back to zero"
        );
        assert!(eng
            .latched("o1")
            .unwrap()
            .starts_with("temperature_over_limit"));
    }

    #[test]
    fn a_printer_that_stops_answering_fails_the_job_after_three_polls() {
        let e = engine();
        let ok = PrinterConfig {
            sim_rate: 1.0,
            step_ms: 3_600_000.0,
            ..PrinterConfig::default()
        };
        let p = e.prepare("p1", &ok, &print(PART)).unwrap();
        e.start("p1", &ok, &p.native_id).unwrap();
        let gone = PrinterConfig {
            sim_fault: "disconnect".into(),
            ..ok.clone()
        };
        for n in 1..=2 {
            let st = e.status("p1", &gone, &p.native_id).unwrap();
            assert_eq!(st.state, WorkState::Running, "poll {n}");
            assert_eq!(st.message, "no answer from the printer");
        }
        let st = e.status("p1", &gone, &p.native_id).unwrap();
        assert_eq!(st.state, WorkState::Failed);
        assert!(st.message.starts_with("printer_disconnected:"));
        assert_eq!(
            e.temperatures("p1", &gone).unwrap_err().code(),
            "printer_disconnected"
        );
        assert_eq!(e.detail("p1", &gone)["online"], false);
        assert!(e.latched("p1").is_none());
    }

    #[test]
    fn the_compatibility_answers_are_off_until_the_machine_asks_for_them() {
        let e = engine();
        let off = quick();
        assert_eq!(
            e.compat("p1", &off, "moonraker").unwrap_err().code(),
            "not_supported"
        );
        let moon = PrinterConfig {
            compat: "moonraker".into(),
            ..quick()
        };
        let m = e.compat("p1", &moon, "moonraker").unwrap();
        assert_eq!(m["result"]["status"]["print_stats"]["state"], "standby");
        assert_eq!(
            e.compat("p1", &moon, "octoprint").unwrap_err().code(),
            "not_supported"
        );
        let both = PrinterConfig {
            compat: "both".into(),
            ..quick()
        };
        assert_eq!(
            e.compat("p1", &both, "octoprint").unwrap()["printer"]["state"]["flags"]["operational"],
            true
        );
    }

    #[test]
    fn the_layer_and_the_estimate_come_from_the_printer() {
        let e = engine();
        let slow = PrinterConfig {
            sim_rate: 1.0,
            step_ms: 1000.0,
            ..PrinterConfig::default()
        };
        let idle = e.layer("p1", &slow).unwrap();
        assert!(idle["layer"].is_null() && idle["eta_s"].is_null());
        let p = e.prepare("p1", &slow, &print(PART)).unwrap();
        e.start("p1", &slow, &p.native_id).unwrap();
        let l = e.layer("p1", &slow).unwrap();
        assert_eq!(l["layers"], 3);
    }

    #[test]
    fn a_moonraker_machine_runs_a_print_against_a_server_in_memory() {
        struct Shared(Arc<FakePrinter>);
        impl Http for Shared {
            fn send(&self, r: &HttpRequest) -> Result<HttpResponse, String> {
                self.0.send(r)
            }
        }
        let fake = Arc::new(FakePrinter::new("moonraker"));
        let f2 = fake.clone();
        let eng = Printer3dEngine::with_factory(Box::new(move |_| {
            Ok(
                Arc::new(MoonrakerDriver::new(Shared(f2.clone()), Some("k".into())))
                    as Arc<dyn PrinterDriver>,
            )
        }));
        let cfg = PrinterConfig::from_settings(
            &json!({"connection": {"kind": "moonraker", "url": "http://printer:7125"}}),
        )
        .unwrap();
        let p = eng.prepare("m1", &cfg, &print(PART)).unwrap();
        eng.start("m1", &cfg, &p.native_id).unwrap();
        assert_eq!(fake.inner.lock().unwrap().uploaded.len(), 1);
        let st = eng.status("m1", &cfg, &p.native_id).unwrap();
        assert_eq!(st.state, WorkState::Running);
        eng.pause("m1", &cfg, &p.native_id).unwrap();
        assert_eq!(fake.inner.lock().unwrap().state, "paused");
        eng.resume("m1", &cfg, &p.native_id).unwrap();
        fake.inner.lock().unwrap().state = "complete".into();
        assert_eq!(
            eng.status("m1", &cfg, &p.native_id).unwrap().state,
            WorkState::Done
        );
        // the emergency stop reaches the printer
        let q = eng.prepare("m1", &cfg, &print(PART)).unwrap();
        fake.inner.lock().unwrap().state = "standby".into();
        eng.start("m1", &cfg, &q.native_id).unwrap();
        let out = eng.emergency_stop("m1", &cfg).unwrap();
        assert_eq!(out["printer_reached"], true);
        assert!(fake.inner.lock().unwrap().shutdown);
        // a printer that is down: the answer says it was not reached, and the machine is latched all the same
        fake.inner.lock().unwrap().down = true;
        let out = eng.emergency_stop("m1", &cfg).unwrap();
        assert_eq!(
            (out["printer_reached"].clone(), out["latched"].clone()),
            (json!(false), json!(true))
        );
        assert!(eng.latched("m1").is_some());
    }

    #[test]
    fn an_octoprint_machine_runs_a_print_against_a_server_in_memory() {
        struct Shared(Arc<FakePrinter>);
        impl Http for Shared {
            fn send(&self, r: &HttpRequest) -> Result<HttpResponse, String> {
                self.0.send(r)
            }
        }
        let fake = Arc::new(FakePrinter::new("octoprint"));
        let f2 = fake.clone();
        let eng = Printer3dEngine::with_factory(Box::new(move |_| {
            Ok(Arc::new(OctoPrintDriver::new(Shared(f2.clone()), None)) as Arc<dyn PrinterDriver>)
        }));
        let cfg = PrinterConfig::from_settings(&json!({"connection": {"kind": "octoprint", "url": "http://octopi.local", "api_key": "abc"}})).unwrap();
        let p = eng.prepare("o1", &cfg, &print(PART)).unwrap();
        eng.start("o1", &cfg, &p.native_id).unwrap();
        assert_eq!(
            eng.status("o1", &cfg, &p.native_id).unwrap().state,
            WorkState::Running
        );
        eng.pause("o1", &cfg, &p.native_id).unwrap();
        assert_eq!(
            eng.status("o1", &cfg, &p.native_id).unwrap().state,
            WorkState::Paused
        );
        eng.resume("o1", &cfg, &p.native_id).unwrap();
        fake.inner.lock().unwrap().progress = 100.0;
        fake.inner.lock().unwrap().state = "operational".into();
        let done = eng.status("o1", &cfg, &p.native_id).unwrap();
        assert_eq!(done.state, WorkState::Done);
        let t = eng
            .set_temperature("o1", &cfg, &json!({"heater": "bed", "target_c": 55}))
            .unwrap();
        assert_eq!(t["target_c"], 55.0);
        assert_eq!(fake.inner.lock().unwrap().bed.1, 55.0);
    }

    #[test]
    fn a_printer_that_goes_idle_before_it_started_is_given_time_and_the_driver_is_kept_per_machine()
    {
        let e = engine();
        let cfg = quick();
        let a = e.driver("p1", &cfg).unwrap();
        let b = e.driver("p1", &cfg).unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "the same driver while the connection is the same"
        );
        let other = PrinterConfig {
            step_ms: 5.0,
            ..quick()
        };
        let c = e.driver("p1", &other).unwrap();
        assert!(
            !Arc::ptr_eq(&a, &c),
            "a new driver when the connection changed"
        );
        let mut job = PJob {
            machine: "p1".into(),
            kind: "print",
            name: "x".into(),
            program: None,
            commands: Vec::new(),
            preheat: Vec::new(),
            state: WorkState::Running,
            message: String::new(),
            started: Some(Instant::now()),
            seen_printing: false,
            missed: 0,
            done: 0,
            total: 100,
        };
        let idle = PrinterState {
            state: "idle",
            raw: "idle".into(),
            heaters: Vec::new(),
            job: PrinterJob::default(),
            t_s: 0.0,
        };
        let started = job.started;
        apply_state(&mut job, "print", &[], &idle, started);
        assert_eq!(
            job.state,
            WorkState::Running,
            "just started: the printer may not have woken yet"
        );
        apply_state(
            &mut job,
            "print",
            &[],
            &idle,
            Some(Instant::now() - Duration::from_secs(60)),
        );
        assert_eq!(job.state, WorkState::Failed);
        let mut seen = PJob {
            seen_printing: true,
            state: WorkState::Running,
            ..job
        };
        apply_state(&mut seen, "print", &[], &idle, None);
        assert_eq!(seen.state, WorkState::Done);
    }

    #[test]
    fn the_marlin_and_bambu_connections_are_checked_and_only_the_simulated_ones_are_served() {
        let marlin = PrinterConfig::from_settings(
            &json!({"connection": {"kind": "marlin", "port": "sim", "baud": 250000}}),
        )
        .unwrap();
        assert_eq!(
            marlin.connection,
            Connection::Marlin {
                port: "sim".into(),
                baud: 250_000
            }
        );
        assert_eq!(marlin.connection.name(), "marlin");
        let default_baud =
            PrinterConfig::from_settings(&json!({"connection": {"kind": "marlin", "port": "sim"}}))
                .unwrap();
        assert!(matches!(
            default_baud.connection,
            Connection::Marlin { baud: 115_200, .. }
        ));
        let colons = vec!["AB"; 32].join(":");
        let bambu = PrinterConfig::from_settings(&json!({"model": "bambu_p1s", "connection": {
            "kind": "bambu_lan", "host": "sim", "serial": "01P00A111111111", "api_key": "12345678", "fingerprint": colons}}))
        .unwrap();
        assert_eq!(
            bambu.connection,
            Connection::BambuLan {
                host: "sim".into(),
                serial: "01P00A111111111".into(),
                access_code: Some("12345678".into()),
                fingerprint: Some("ab".repeat(32))
            }
        );
        assert_eq!(
            (bambu.model.as_str(), bambu.limits),
            ("bambu_p1s", [300.0, 100.0, 0.0])
        );
        for bad in [
            json!({"connection": {"kind": "marlin"}}),
            json!({"connection": {"kind": "marlin", "port": "COM3"}}),
            json!({"connection": {"kind": "marlin", "port": "sim", "baud": 5}}),
            json!({"connection": {"kind": "bambu_lan", "serial": "01P00A111111111"}}),
            json!({"connection": {"kind": "bambu_lan", "host": "192.168.1.9", "serial": "01P00A111111111"}}),
            json!({"connection": {"kind": "bambu_lan", "host": "sim", "serial": "short"}}),
            json!({"connection": {"kind": "bambu_lan", "host": "sim", "serial": "01P00A111111111", "fingerprint": "abc"}}),
            json!({"connection": {"kind": "bambu_lan", "host": "sim", "serial": "01P00A111111111", "fingerprint": 5}}),
        ] {
            assert!(PrinterConfig::from_settings(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_marlin_machine_runs_a_print_on_its_simulated_line_inside_its_limits() {
        let e = engine();
        let cfg = PrinterConfig {
            connection: Connection::Marlin {
                port: "sim".into(),
                baud: 115_200,
            },
            ..quick()
        };
        let p = e.prepare("m1", &cfg, &print(PART)).unwrap();
        e.start("m1", &cfg, &p.native_id).unwrap();
        let st = e.status("m1", &cfg, &p.native_id).unwrap();
        assert_eq!((st.state, st.done), (WorkState::Done, 100));
        assert_eq!(e.detail("m1", &cfg)["connection"], "marlin");
        // a target over the limit of the model is refused before the line is touched
        let over = json!({"heater": "hotend", "target_c": 270});
        assert!(e.set_temperature("m1", &cfg, &over).is_err());
        let ok = json!({"heater": "hotend", "target_c": 200});
        assert!(e.set_temperature("m1", &cfg, &ok).is_ok());
    }

    #[test]
    fn a_bambu_machine_is_refused_until_the_certificate_is_pinned_and_then_prints_on_the_simulator()
    {
        let e = engine();
        let serial = "01P00A111111111";
        let conn = |fingerprint: Option<String>| Connection::BambuLan {
            host: "sim".into(),
            serial: serial.into(),
            access_code: Some("12345678".into()),
            fingerprint,
        };
        let model = |connection| PrinterConfig {
            model: "bambu_a1".into(),
            limits: [300.0, 100.0, 0.0],
            connection,
            ..quick()
        };
        // no fingerprint pinned: nothing is sent, and the refusal says what the printer showed
        let open = model(conn(None));
        let p = e.prepare("b1", &open, &print(PART)).unwrap();
        let refused = e.start("b1", &open, &p.native_id).unwrap_err();
        assert_eq!(refused.code(), "printer_untrusted");
        assert!(
            refused
                .detail()
                .contains(&crate::printer_lan::sim_fingerprint(serial)),
            "{}",
            refused.detail()
        );
        // another certificate than the pinned one
        let wrong = model(conn(Some(crate::printer_lan::sim_fingerprint("other"))));
        let refused = e.start("b1", &wrong, &p.native_id).unwrap_err();
        assert_eq!(refused.code(), "printer_untrusted");
        assert!(refused.detail().contains("not the pinned one"));
        // the pinned one
        let pinned = model(conn(Some(crate::printer_lan::sim_fingerprint(serial))));
        e.start("b1", &pinned, &p.native_id).unwrap();
        let st = e.status("b1", &pinned, &p.native_id).unwrap();
        assert_eq!((st.state, st.done), (WorkState::Done, 100));
        let detail = e.detail("b1", &pinned);
        assert_eq!(detail["connection"], "bambu_lan");
        assert_eq!(detail["bambu_lan"]["experimental"], true);
        assert_eq!(detail["bambu_lan"]["fingerprint_pinned"], true);
    }
}
