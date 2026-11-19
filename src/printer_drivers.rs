//! How the server talks to a 3D printer: the normalized state of a printer, the driver that every kind of printer has to be, and the drivers for the printers that answer
//! over HTTP, Klipper through Moonraker and OctoPrint. The machines of the plugin `printer3d` (`src/printer3d.rs`) work on a driver and do not know which printer is behind.
//!
//! A driver does one thing at a time and gives back what the printer said, changed into the state the server uses (`PrinterState`). Everything that reaches a printer goes through
//! the trait [`Http`], so the drivers are tried against servers that live in memory and a real network is only touched by [`TcpHttp`]: plain HTTP over a TCP connection with a
//! time limit, no TLS (a printer behind `https://` is refused with a clear message). The drivers have only been tried against those servers in memory and against a socket of
//! this computer: nothing has run against a real Klipper or OctoPrint.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{json, Value};

/// What a printer is doing, in the words the server uses for every kind of printer.
pub const STATES: &[&str] = &[
    "offline",
    "idle",
    "preparing",
    "printing",
    "pausing",
    "paused",
    "finishing",
    "finished",
    "failed",
    "cancelling",
    "cancelled",
    "error",
    "emergency",
];

/// One heater: the reading, the target and how hard it works.
#[derive(Debug, Clone, PartialEq)]
pub struct Heater {
    /// `hotend`, `bed` or `chamber`.
    pub name: &'static str,
    pub current_c: f64,
    pub target_c: f64,
    pub power_pct: f64,
}

/// The job the printer has, when it has one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrinterJob {
    pub name: String,
    pub progress_pct: f64,
    pub layer: Option<u64>,
    pub layers: Option<u64>,
    pub z_mm: Option<f64>,
    pub elapsed_s: f64,
}

/// What a printer is at the moment, whatever the printer.
#[derive(Debug, Clone, PartialEq)]
pub struct PrinterState {
    pub state: &'static str,
    /// The word the printer used, for the people who read it.
    pub raw: String,
    pub heaters: Vec<Heater>,
    pub job: PrinterJob,
    /// Seconds on the clock of the printer (the simulator's, or the time since the driver was made).
    pub t_s: f64,
}

impl PrinterState {
    pub fn heater(&self, name: &str) -> Option<&Heater> {
        self.heaters.iter().find(|h| h.name == name)
    }
}

/// Why a driver could not do what it was asked.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverError {
    /// The printer does not answer.
    Disconnected(String),
    /// The printer answered with a refusal.
    Refused(String),
}

impl DriverError {
    pub fn text(&self) -> &str {
        match self {
            DriverError::Disconnected(t) | DriverError::Refused(t) => t,
        }
    }
}

pub type DriverResult<T> = Result<T, DriverError>;

/// A kind of printer.
pub trait PrinterDriver: Send + Sync {
    fn state(&self) -> DriverResult<PrinterState>;
    /// Gives the printer the program under `name` and starts it.
    fn start_print(&self, name: &str, gcode: &str) -> DriverResult<()>;
    fn pause(&self) -> DriverResult<()>;
    fn resume(&self) -> DriverResult<()>;
    fn cancel(&self) -> DriverResult<()>;
    /// Sets the target of `heater` (`hotend`, `bed` or `chamber`); 0 switches it off.
    fn set_target(&self, heater: &str, target_c: f64) -> DriverResult<()>;
    /// Heaters off and motors released, at once. Must work when everything else does not.
    fn emergency_stop(&self) -> DriverResult<()>;
    /// Sends one line of G-code to the printer, outside any print.
    fn gcode(&self, line: &str) -> DriverResult<()>;
    /// Brings the printer back from an emergency stop (Klipper restarts its firmware); a printer that has nothing to do for it does nothing.
    fn reset(&self) -> DriverResult<()> {
        Ok(())
    }
    /// Makes a simulated printer fail the way `fault` says (`none`, `thermal_runaway`, `sensor_lost` or `disconnect`); a real printer ignores it.
    fn simulate(&self, _fault: &str) {}
}

// ---- HTTP ------------------------------------------------------------------------------------------------------------------------------

/// What a request carries.
#[derive(Debug, Clone, PartialEq)]
pub enum HttpBody {
    None,
    Json(Value),
    /// A file sent as `multipart/form-data`, with the fields that go beside it.
    Upload {
        field: &'static str,
        filename: String,
        bytes: Vec<u8>,
        fields: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: &'static str,
    /// The path with its query, as it goes on the request line.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: HttpBody,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Value,
}

/// The way to a printer: one request and its answer. `Err` is a printer that cannot be reached.
pub trait Http: Send + Sync {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String>;
}

/// The biggest answer a printer is read for, in bytes.
const MAX_ANSWER: usize = 2 * 1024 * 1024;

/// Plain HTTP over TCP, with a time limit on connecting, writing and reading.
#[derive(Debug)]
pub struct TcpHttp {
    host: String,
    port: u16,
    timeout: Duration,
}

impl TcpHttp {
    /// `http://host:port`, or `host:port` (the port is 80 when it is left out). `https://` is refused: there is no TLS here.
    pub fn new(url: &str, timeout: Duration) -> Result<TcpHttp, String> {
        let rest = if let Some(r) = url.strip_prefix("http://") {
            r
        } else if url.starts_with("https://") {
            return Err("https is not served yet: use the http address of the printer".into());
        } else if url.contains("://") {
            return Err("the address is http://host:port".into());
        } else {
            url
        };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() || rest.contains('/') || rest.contains(char::is_whitespace) {
            return Err("the address is http://host:port, with no path".into());
        }
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() => (
                h.to_string(),
                p.parse::<u16>()
                    .map_err(|_| "the port is a number from 1 to 65535".to_string())?,
            ),
            _ => (rest.to_string(), 80),
        };
        if port == 0 || host.len() > 253 {
            return Err("the port is a number from 1 to 65535".into());
        }
        Ok(TcpHttp {
            host,
            port,
            timeout,
        })
    }
}

fn encode(request: &HttpRequest, host: &str) -> Vec<u8> {
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\n",
        request.method, request.path, host
    );
    for (k, v) in &request.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    let (kind, body): (Option<String>, Vec<u8>) = match &request.body {
        HttpBody::None => (None, Vec::new()),
        HttpBody::Json(v) => (Some("application/json".into()), v.to_string().into_bytes()),
        HttpBody::Upload {
            field,
            filename,
            bytes,
            fields,
        } => {
            let boundary = "----eh3dserver7e5c1b0a";
            let mut out: Vec<u8> = Vec::new();
            for (k, v) in fields {
                out.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").into_bytes());
            }
            out.extend(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes(),
            );
            out.extend(bytes);
            out.extend(format!("\r\n--{boundary}--\r\n").into_bytes());
            (
                Some(format!("multipart/form-data; boundary={boundary}")),
                out,
            )
        }
    };
    if let Some(kind) = kind {
        head.push_str(&format!("Content-Type: {kind}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut all = head.into_bytes();
    all.extend(body);
    all
}

/// Takes the chunks of a `Transfer-Encoding: chunked` body together.
fn unchunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = data.windows(2).position(|w| w == b"\r\n")?;
        let size = usize::from_str_radix(
            std::str::from_utf8(&data[..end])
                .ok()?
                .split(';')
                .next()?
                .trim(),
            16,
        )
        .ok()?;
        data = &data[end + 2..];
        if size == 0 {
            return Some(out);
        }
        if data.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// The status and the JSON body of an answer read whole.
pub fn parse_answer(raw: &[u8]) -> Result<HttpResponse, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("the answer has no end of header")?;
    let head =
        std::str::from_utf8(&raw[..split]).map_err(|_| "the header of the answer is not text")?;
    let mut lines = head.lines();
    let status_line = lines.next().ok_or("the answer is empty")?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("the status line of the answer does not read")?;
    let chunked = lines.any(|l| {
        l.to_ascii_lowercase()
            .replace(' ', "")
            .starts_with("transfer-encoding:chunked")
    });
    let body = &raw[split + 4..];
    let body = if chunked {
        unchunk(body).ok_or("the chunks of the answer do not read")?
    } else {
        body.to_vec()
    };
    let text = String::from_utf8_lossy(&body);
    let value = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(text.trim()).unwrap_or_else(|_| json!({ "text": text.trim() }))
    };
    Ok(HttpResponse {
        status,
        body: value,
    })
}

impl Http for TcpHttp {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| format!("the address does not resolve: {e}"))?
            .next()
            .ok_or("the address does not resolve")?;
        let mut stream = TcpStream::connect_timeout(&addr, self.timeout)
            .map_err(|e| format!("cannot connect: {e}"))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(&encode(request, &format!("{}:{}", self.host, self.port)))
            .map_err(|e| format!("cannot send: {e}"))?;
        let mut raw = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&chunk[..n]);
                    if raw.len() > MAX_ANSWER {
                        return Err("the answer is too big".into());
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    if raw.is_empty() {
                        return Err("the printer did not answer in time".into());
                    }
                    break;
                }
                Err(e) => return Err(format!("cannot read: {e}")),
            }
        }
        parse_answer(&raw)
    }
}

fn heater_value(h: Option<&Value>) -> (f64, f64) {
    let n = |k: &str| {
        h.and_then(|v| v.get(k))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    (n("temperature"), n("target"))
}

fn ok_status(r: &HttpResponse) -> DriverResult<()> {
    if (200..300).contains(&r.status) {
        Ok(())
    } else {
        let why = r
            .body
            .get("error")
            .and_then(|e| {
                e.get("message")
                    .and_then(Value::as_str)
                    .or_else(|| e.as_str())
            })
            .map(str::to_string)
            .unwrap_or_else(|| format!("the printer answered {}", r.status));
        Err(DriverError::Refused(why))
    }
}

// ---- Moonraker -------------------------------------------------------------------------------------------------------------------------

/// A Klipper printer through Moonraker (port 7125 by default): the state is read from its objects, a program is uploaded and started with the print routes.
pub struct MoonrakerDriver<H: Http> {
    http: H,
    api_key: Option<String>,
    started: std::time::Instant,
}

impl<H: Http> MoonrakerDriver<H> {
    pub fn new(http: H, api_key: Option<String>) -> MoonrakerDriver<H> {
        MoonrakerDriver {
            http,
            api_key,
            started: std::time::Instant::now(),
        }
    }

    fn call(&self, method: &'static str, path: &str, body: HttpBody) -> DriverResult<HttpResponse> {
        let mut headers = Vec::new();
        if let Some(k) = &self.api_key {
            headers.push(("X-Api-Key".to_string(), k.clone()));
        }
        self.http
            .send(&HttpRequest {
                method,
                path: path.to_string(),
                headers,
                body,
            })
            .map_err(DriverError::Disconnected)
    }

    fn script(&self, gcode: &str) -> DriverResult<()> {
        let q = gcode.replace(' ', "%20");
        ok_status(&self.call(
            "POST",
            &format!("/printer/gcode/script?script={q}"),
            HttpBody::None,
        )?)
    }
}

/// The state of Moonraker's `print_stats` in the words of the server.
fn moonraker_state(word: &str) -> &'static str {
    match word {
        "standby" => "idle",
        "printing" => "printing",
        "paused" => "paused",
        "complete" => "finished",
        "cancelled" => "cancelled",
        "error" => "error",
        _ => "idle",
    }
}

impl<H: Http> PrinterDriver for MoonrakerDriver<H> {
    fn state(&self) -> DriverResult<PrinterState> {
        let r = self.call(
            "GET",
            "/printer/objects/query?extruder&heater_bed&heater_generic%20chamber&print_stats&virtual_sdcard&webhooks&toolhead",
            HttpBody::None,
        )?;
        ok_status(&r)?;
        let status = r
            .body
            .get("result")
            .and_then(|r| r.get("status"))
            .ok_or_else(|| DriverError::Refused("the answer has no status".into()))?;
        let word = status
            .get("print_stats")
            .and_then(|p| p.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("standby");
        let down = status
            .get("webhooks")
            .and_then(|w| w.get("state"))
            .and_then(Value::as_str)
            .is_some_and(|s| s == "shutdown" || s == "error");
        let (state, raw) = if down {
            ("emergency", "shutdown".to_string())
        } else {
            (moonraker_state(word), word.to_string())
        };
        let mut heaters = Vec::new();
        for (name, key) in [
            ("hotend", "extruder"),
            ("bed", "heater_bed"),
            ("chamber", "heater_generic chamber"),
        ] {
            if let Some(h) = status.get(key) {
                let (cur, target) = heater_value(Some(h));
                heaters.push(Heater {
                    name,
                    current_c: cur,
                    target_c: target,
                    power_pct: h.get("power").and_then(Value::as_f64).unwrap_or(0.0) * 100.0,
                });
            }
        }
        let stats = status.get("print_stats");
        let info = stats.and_then(|s| s.get("info"));
        let job = PrinterJob {
            name: stats
                .and_then(|s| s.get("filename"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            progress_pct: status
                .get("virtual_sdcard")
                .and_then(|v| v.get("progress"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                * 100.0,
            layer: info
                .and_then(|i| i.get("current_layer"))
                .and_then(Value::as_u64),
            layers: info
                .and_then(|i| i.get("total_layer"))
                .and_then(Value::as_u64),
            z_mm: status
                .get("toolhead")
                .and_then(|t| t.get("position"))
                .and_then(|p| p.get(2))
                .and_then(Value::as_f64),
            elapsed_s: stats
                .and_then(|s| s.get("print_duration"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        };
        Ok(PrinterState {
            state,
            raw,
            heaters,
            job,
            t_s: self.started.elapsed().as_secs_f64(),
        })
    }

    fn start_print(&self, name: &str, gcode: &str) -> DriverResult<()> {
        let up = self.call(
            "POST",
            "/server/files/upload",
            HttpBody::Upload {
                field: "file",
                filename: name.to_string(),
                bytes: gcode.as_bytes().to_vec(),
                fields: vec![("root".into(), "gcodes".into())],
            },
        )?;
        ok_status(&up)?;
        let enc = name.replace(' ', "%20");
        ok_status(&self.call(
            "POST",
            &format!("/printer/print/start?filename={enc}"),
            HttpBody::None,
        )?)
    }

    fn pause(&self) -> DriverResult<()> {
        ok_status(&self.call("POST", "/printer/print/pause", HttpBody::None)?)
    }

    fn resume(&self) -> DriverResult<()> {
        ok_status(&self.call("POST", "/printer/print/resume", HttpBody::None)?)
    }

    fn cancel(&self) -> DriverResult<()> {
        ok_status(&self.call("POST", "/printer/print/cancel", HttpBody::None)?)
    }

    fn set_target(&self, heater: &str, target_c: f64) -> DriverResult<()> {
        let code = match heater {
            "hotend" => "M104",
            "bed" => "M140",
            "chamber" => "M141",
            _ => {
                return Err(DriverError::Refused(
                    "the heater is hotend, bed or chamber".into(),
                ))
            }
        };
        self.script(&format!("{code} S{}", crate::simjobs::num(target_c)))
    }

    fn emergency_stop(&self) -> DriverResult<()> {
        ok_status(&self.call("POST", "/printer/emergency_stop", HttpBody::None)?)
    }

    fn gcode(&self, line: &str) -> DriverResult<()> {
        self.script(line)
    }

    fn reset(&self) -> DriverResult<()> {
        ok_status(&self.call("POST", "/printer/firmware_restart", HttpBody::None)?)
    }
}

// ---- OctoPrint -------------------------------------------------------------------------------------------------------------------------

/// A printer behind OctoPrint (the API key goes in `X-Api-Key`).
pub struct OctoPrintDriver<H: Http> {
    http: H,
    api_key: Option<String>,
    started: std::time::Instant,
}

impl<H: Http> OctoPrintDriver<H> {
    pub fn new(http: H, api_key: Option<String>) -> OctoPrintDriver<H> {
        OctoPrintDriver {
            http,
            api_key,
            started: std::time::Instant::now(),
        }
    }

    fn call(&self, method: &'static str, path: &str, body: HttpBody) -> DriverResult<HttpResponse> {
        let mut headers = Vec::new();
        if let Some(k) = &self.api_key {
            headers.push(("X-Api-Key".to_string(), k.clone()));
        }
        self.http
            .send(&HttpRequest {
                method,
                path: path.to_string(),
                headers,
                body,
            })
            .map_err(DriverError::Disconnected)
    }

    fn command(&self, body: Value) -> DriverResult<()> {
        ok_status(&self.call("POST", "/api/job", HttpBody::Json(body))?)
    }
}

impl<H: Http> PrinterDriver for OctoPrintDriver<H> {
    fn state(&self) -> DriverResult<PrinterState> {
        let printer = self.call("GET", "/api/printer", HttpBody::None)?;
        // OctoPrint answers 409 while the printer is not connected to it
        if printer.status == 409 {
            return Ok(PrinterState {
                state: "offline",
                raw: "closed".into(),
                heaters: Vec::new(),
                job: PrinterJob::default(),
                t_s: self.started.elapsed().as_secs_f64(),
            });
        }
        ok_status(&printer)?;
        let job = self.call("GET", "/api/job", HttpBody::None)?;
        ok_status(&job)?;
        let flag = |k: &str| {
            printer
                .body
                .get("state")
                .and_then(|s| s.get("flags"))
                .and_then(|f| f.get(k))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        let completion = job
            .body
            .get("progress")
            .and_then(|p| p.get("completion"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let (state, raw) = if flag("cancelling") {
            ("cancelling", "cancelling")
        } else if flag("pausing") {
            ("pausing", "pausing")
        } else if flag("paused") {
            ("paused", "paused")
        } else if flag("printing") {
            ("printing", "printing")
        } else if flag("error") || flag("closedOrError") {
            ("error", "error")
        } else if flag("operational") && completion >= 100.0 {
            ("finished", "operational")
        } else if flag("operational") {
            ("idle", "operational")
        } else {
            ("offline", "offline")
        };
        let t = printer.body.get("temperature");
        let mut heaters = Vec::new();
        for (name, key) in [("hotend", "tool0"), ("bed", "bed"), ("chamber", "chamber")] {
            if let Some(h) = t.and_then(|t| t.get(key)) {
                heaters.push(Heater {
                    name,
                    current_c: h.get("actual").and_then(Value::as_f64).unwrap_or(0.0),
                    target_c: h.get("target").and_then(Value::as_f64).unwrap_or(0.0),
                    power_pct: 0.0,
                });
            }
        }
        let pj = PrinterJob {
            name: job
                .body
                .get("job")
                .and_then(|j| j.get("file"))
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            progress_pct: completion,
            layer: None,
            layers: None,
            z_mm: None,
            elapsed_s: job
                .body
                .get("progress")
                .and_then(|p| p.get("printTime"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        };
        Ok(PrinterState {
            state,
            raw: raw.into(),
            heaters,
            job: pj,
            t_s: self.started.elapsed().as_secs_f64(),
        })
    }

    fn start_print(&self, name: &str, gcode: &str) -> DriverResult<()> {
        let up = self.call(
            "POST",
            "/api/files/local",
            HttpBody::Upload {
                field: "file",
                filename: name.to_string(),
                bytes: gcode.as_bytes().to_vec(),
                fields: vec![
                    ("select".into(), "true".into()),
                    ("print".into(), "true".into()),
                ],
            },
        )?;
        ok_status(&up)
    }

    fn pause(&self) -> DriverResult<()> {
        self.command(json!({"command": "pause", "action": "pause"}))
    }

    fn resume(&self) -> DriverResult<()> {
        self.command(json!({"command": "pause", "action": "resume"}))
    }

    fn cancel(&self) -> DriverResult<()> {
        self.command(json!({"command": "cancel"}))
    }

    fn set_target(&self, heater: &str, target_c: f64) -> DriverResult<()> {
        let (path, body) = match heater {
            "hotend" => (
                "/api/printer/tool",
                json!({"command": "target", "targets": {"tool0": target_c}}),
            ),
            "bed" => (
                "/api/printer/bed",
                json!({"command": "target", "target": target_c}),
            ),
            "chamber" => (
                "/api/printer/chamber",
                json!({"command": "target", "target": target_c}),
            ),
            _ => {
                return Err(DriverError::Refused(
                    "the heater is hotend, bed or chamber".into(),
                ))
            }
        };
        ok_status(&self.call("POST", path, HttpBody::Json(body))?)
    }

    fn emergency_stop(&self) -> DriverResult<()> {
        // M112 is what stops the firmware; the job is cancelled after it so that OctoPrint does not go on sending
        let stop = self.call(
            "POST",
            "/api/printer/command",
            HttpBody::Json(json!({"commands": ["M112"]})),
        );
        let _ = self.cancel();
        ok_status(&stop?)
    }

    fn gcode(&self, line: &str) -> DriverResult<()> {
        ok_status(&self.call(
            "POST",
            "/api/printer/command",
            HttpBody::Json(json!({"commands": [line]})),
        )?)
    }
}

// ---- the compatibility layer -----------------------------------------------------------------------------------------------------------

/// The state of a printer in the shape that Moonraker answers `GET /printer/objects/query` with, so that a program that speaks Moonraker can read it.
pub fn moonraker_shape(s: &PrinterState) -> Value {
    let word = match s.state {
        "printing" | "preparing" | "pausing" | "finishing" => "printing",
        "paused" => "paused",
        "finished" => "complete",
        "cancelled" | "cancelling" => "cancelled",
        "failed" | "error" | "emergency" => "error",
        _ => "standby",
    };
    let heater = |name: &str| {
        s.heater(name).map(|h| json!({"temperature": h.current_c, "target": h.target_c, "power": h.power_pct / 100.0}))
    };
    json!({
        "result": {
            "eventtime": s.t_s,
            "status": {
                "extruder": heater("hotend"),
                "heater_bed": heater("bed"),
                "print_stats": {
                    "state": word, "filename": s.job.name, "print_duration": s.job.elapsed_s,
                    "info": { "current_layer": s.job.layer, "total_layer": s.job.layers },
                },
                "virtual_sdcard": { "progress": s.job.progress_pct / 100.0 },
                "webhooks": { "state": if s.state == "emergency" { "shutdown" } else { "ready" } },
            },
        }
    })
}

/// The state of a printer in the shape that OctoPrint answers `GET /api/printer` and `GET /api/job` with.
pub fn octoprint_shape(s: &PrinterState) -> Value {
    let printing = matches!(s.state, "printing" | "preparing" | "pausing" | "finishing");
    let heater = |name: &str| {
        s.heater(name)
            .map(|h| json!({"actual": h.current_c, "target": h.target_c, "offset": 0}))
    };
    json!({
        "printer": {
            "temperature": { "tool0": heater("hotend"), "bed": heater("bed") },
            "state": {
                "text": s.raw,
                "flags": {
                    "operational": s.state != "offline", "printing": printing, "paused": s.state == "paused",
                    "pausing": s.state == "pausing", "cancelling": s.state == "cancelling",
                    "error": matches!(s.state, "error" | "emergency" | "failed"), "ready": s.state == "idle",
                },
            },
        },
        "job": {
            "job": { "file": { "name": s.job.name } },
            "progress": { "completion": s.job.progress_pct, "printTime": s.job.elapsed_s },
            "state": s.raw,
        },
    })
}

#[cfg(test)]
pub mod fakes {
    //! Printers that live in memory and answer the way Moonraker and OctoPrint do, for the tests of the drivers and of the plugin.

    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    pub struct Fake {
        pub down: bool,
        pub kind: &'static str,
        pub state: String,
        pub hot: (f64, f64),
        pub bed: (f64, f64),
        pub file: String,
        pub progress: f64,
        pub uploaded: Vec<(String, usize)>,
        pub calls: Vec<String>,
        pub shutdown: bool,
        pub closed: bool,
        pub key_seen: Option<String>,
    }

    /// A printer in memory behind the trait [`Http`]; `kind` is `moonraker` or `octoprint`.
    pub struct FakePrinter {
        pub inner: Mutex<Fake>,
    }

    impl FakePrinter {
        pub fn new(kind: &'static str) -> FakePrinter {
            FakePrinter {
                inner: Mutex::new(Fake {
                    kind,
                    state: "standby".into(),
                    hot: (22.0, 0.0),
                    bed: (22.0, 0.0),
                    ..Fake::default()
                }),
            }
        }
    }

    fn ok(body: Value) -> Result<HttpResponse, String> {
        Ok(HttpResponse { status: 200, body })
    }

    impl Http for FakePrinter {
        fn send(&self, r: &HttpRequest) -> Result<HttpResponse, String> {
            let mut f = self.inner.lock().unwrap();
            if f.down {
                return Err("cannot connect: connection refused".into());
            }
            f.calls.push(format!("{} {}", r.method, r.path));
            f.key_seen = r
                .headers
                .iter()
                .find(|h| h.0 == "X-Api-Key")
                .map(|h| h.1.clone());
            let route = r.path.split('?').next().unwrap_or("");
            if f.kind == "moonraker" {
                match (r.method, route) {
                    ("GET", "/printer/objects/query") => ok(json!({"result": {"status": {
                        "extruder": {"temperature": f.hot.0, "target": f.hot.1, "power": 0.5},
                        "heater_bed": {"temperature": f.bed.0, "target": f.bed.1, "power": 0.0},
                        "print_stats": {"state": f.state, "filename": f.file, "print_duration": 12.0, "info": {"current_layer": 3, "total_layer": 10}},
                        "virtual_sdcard": {"progress": f.progress},
                        "webhooks": {"state": if f.shutdown { "shutdown" } else { "ready" }},
                        "toolhead": {"position": [1.0, 2.0, 0.6, 0.0]},
                    }}})),
                    ("POST", "/server/files/upload") => {
                        if let HttpBody::Upload {
                            filename, bytes, ..
                        } = &r.body
                        {
                            f.uploaded.push((filename.clone(), bytes.len()));
                            f.file = filename.clone();
                        }
                        Ok(HttpResponse {
                            status: 201,
                            body: json!({"item": {"path": f.file}}),
                        })
                    }
                    ("POST", "/printer/print/start") => {
                        f.state = "printing".into();
                        ok(json!({"result": "ok"}))
                    }
                    ("POST", "/printer/print/pause") => {
                        f.state = "paused".into();
                        ok(json!({"result": "ok"}))
                    }
                    ("POST", "/printer/print/resume") => {
                        f.state = "printing".into();
                        ok(json!({"result": "ok"}))
                    }
                    ("POST", "/printer/print/cancel") => {
                        f.state = "cancelled".into();
                        ok(json!({"result": "ok"}))
                    }
                    ("POST", "/printer/firmware_restart") => {
                        f.shutdown = false;
                        f.state = "standby".into();
                        ok(json!({"result": "ok"}))
                    }
                    ("POST", "/printer/emergency_stop") => {
                        f.shutdown = true;
                        f.hot.1 = 0.0;
                        f.bed.1 = 0.0;
                        ok(json!({"result": "ok"}))
                    }
                    ("POST", "/printer/gcode/script") => {
                        let q = r
                            .path
                            .split("script=")
                            .nth(1)
                            .unwrap_or("")
                            .replace("%20", " ");
                        let s: f64 = q
                            .split('S')
                            .nth(1)
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0.0);
                        if q.starts_with("M104") {
                            f.hot.1 = s;
                        } else if q.starts_with("M140") {
                            f.bed.1 = s;
                        }
                        ok(json!({"result": "ok"}))
                    }
                    _ => Ok(HttpResponse {
                        status: 404,
                        body: json!({"error": {"message": "not found"}}),
                    }),
                }
            } else {
                match (r.method, route) {
                    ("GET", "/api/printer") => {
                        if f.closed {
                            return Ok(HttpResponse {
                                status: 409,
                                body: json!("Printer is not operational"),
                            });
                        }
                        let printing = f.state == "printing";
                        ok(json!({
                            "temperature": {"tool0": {"actual": f.hot.0, "target": f.hot.1}, "bed": {"actual": f.bed.0, "target": f.bed.1}},
                            "state": {"text": f.state, "flags": {"operational": true, "printing": printing, "paused": f.state == "paused", "cancelling": false, "error": f.state == "error"}},
                        }))
                    }
                    ("GET", "/api/job") => ok(
                        json!({"job": {"file": {"name": f.file}}, "progress": {"completion": f.progress, "printTime": 12.0}, "state": f.state}),
                    ),
                    ("POST", "/api/files/local") => {
                        if let HttpBody::Upload {
                            filename, bytes, ..
                        } = &r.body
                        {
                            f.uploaded.push((filename.clone(), bytes.len()));
                            f.file = filename.clone();
                        }
                        f.state = "printing".into();
                        Ok(HttpResponse {
                            status: 201,
                            body: json!({"done": true}),
                        })
                    }
                    ("POST", "/api/job") => {
                        let cmd = if let HttpBody::Json(v) = &r.body {
                            v.clone()
                        } else {
                            Value::Null
                        };
                        match (cmd["command"].as_str(), cmd["action"].as_str()) {
                            (Some("pause"), Some("pause")) => f.state = "paused".into(),
                            (Some("pause"), Some("resume")) => f.state = "printing".into(),
                            (Some("cancel"), _) => f.state = "operational".into(),
                            _ => {}
                        }
                        Ok(HttpResponse {
                            status: 204,
                            body: Value::Null,
                        })
                    }
                    ("POST", "/api/printer/tool") => {
                        if let HttpBody::Json(v) = &r.body {
                            f.hot.1 = v["targets"]["tool0"].as_f64().unwrap_or(0.0);
                        }
                        Ok(HttpResponse {
                            status: 204,
                            body: Value::Null,
                        })
                    }
                    ("POST", "/api/printer/bed") => {
                        if let HttpBody::Json(v) = &r.body {
                            f.bed.1 = v["target"].as_f64().unwrap_or(0.0);
                        }
                        Ok(HttpResponse {
                            status: 204,
                            body: Value::Null,
                        })
                    }
                    ("POST", "/api/printer/command") => {
                        let text = if let HttpBody::Json(v) = &r.body {
                            v.to_string()
                        } else {
                            String::new()
                        };
                        if text.contains("M112") {
                            f.hot.1 = 0.0;
                            f.bed.1 = 0.0;
                            f.state = "error".into();
                        }
                        Ok(HttpResponse {
                            status: 204,
                            body: Value::Null,
                        })
                    }
                    _ => Ok(HttpResponse {
                        status: 404,
                        body: json!({"error": "not found"}),
                    }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;

    use super::fakes::FakePrinter;
    use super::*;

    /// The driver gets the fake through an `Arc` so that the test can look at it and change it.
    struct Shared(Arc<FakePrinter>);

    impl Http for Shared {
        fn send(&self, r: &HttpRequest) -> Result<HttpResponse, String> {
            self.0.send(r)
        }
    }

    fn moonraker() -> (MoonrakerDriver<Shared>, Arc<FakePrinter>) {
        let fake = Arc::new(FakePrinter::new("moonraker"));
        (
            MoonrakerDriver::new(Shared(fake.clone()), Some("secret-key".into())),
            fake,
        )
    }

    fn octoprint() -> (OctoPrintDriver<Shared>, Arc<FakePrinter>) {
        let fake = Arc::new(FakePrinter::new("octoprint"));
        (OctoPrintDriver::new(Shared(fake.clone()), None), fake)
    }

    #[test]
    fn moonraker_reads_the_state_of_the_printer_in_the_words_of_the_server() {
        let (d, fake) = moonraker();
        let s = d.state().unwrap();
        assert_eq!((s.state, s.raw.as_str()), ("idle", "standby"));
        assert_eq!(s.heater("hotend").unwrap().current_c, 22.0);
        assert_eq!(s.heater("hotend").unwrap().power_pct, 50.0);
        assert!(s.heater("chamber").is_none());
        assert_eq!(
            (s.job.layer, s.job.layers, s.job.z_mm),
            (Some(3), Some(10), Some(0.6))
        );
        for (word, want) in [
            ("printing", "printing"),
            ("paused", "paused"),
            ("complete", "finished"),
            ("cancelled", "cancelled"),
            ("error", "error"),
        ] {
            fake.inner.lock().unwrap().state = word.into();
            assert_eq!(d.state().unwrap().state, want, "{word}");
        }
        fake.inner.lock().unwrap().shutdown = true;
        assert_eq!(d.state().unwrap().state, "emergency");
        // the key goes in the header of every request
        assert_eq!(
            fake.inner.lock().unwrap().key_seen.as_deref(),
            Some("secret-key")
        );
    }

    #[test]
    fn moonraker_uploads_a_program_starts_it_and_holds_it() {
        let (d, fake) = moonraker();
        d.start_print("my part.gcode", "G28\nG1 X1\n").unwrap();
        {
            let f = fake.inner.lock().unwrap();
            assert_eq!(f.uploaded, vec![("my part.gcode".to_string(), 10)]);
            assert!(
                f.calls
                    .contains(&"POST /printer/print/start?filename=my%20part.gcode".to_string()),
                "{:?}",
                f.calls
            );
        }
        assert_eq!(d.state().unwrap().state, "printing");
        d.pause().unwrap();
        assert_eq!(d.state().unwrap().state, "paused");
        d.resume().unwrap();
        d.cancel().unwrap();
        assert_eq!(d.state().unwrap().state, "cancelled");
    }

    #[test]
    fn moonraker_sets_the_targets_and_stops_at_once() {
        let (d, fake) = moonraker();
        d.set_target("hotend", 200.0).unwrap();
        d.set_target("bed", 60.0).unwrap();
        let (hot, bed) = {
            let f = fake.inner.lock().unwrap();
            (f.hot.1, f.bed.1)
        };
        assert_eq!((hot, bed), (200.0, 60.0));
        assert!(matches!(
            d.set_target("nozzle", 1.0),
            Err(DriverError::Refused(_))
        ));
        d.emergency_stop().unwrap();
        {
            let f = fake.inner.lock().unwrap();
            assert!(f.shutdown && f.hot.1 == 0.0 && f.bed.1 == 0.0);
        }
        d.reset().unwrap();
        assert!(!fake.inner.lock().unwrap().shutdown);
        assert_eq!(d.state().unwrap().state, "idle");
    }

    #[test]
    fn a_printer_that_does_not_answer_is_disconnected_and_a_refusal_is_a_refusal() {
        let (d, fake) = moonraker();
        fake.inner.lock().unwrap().down = true;
        assert!(matches!(d.state(), Err(DriverError::Disconnected(_))));
        assert!(matches!(d.pause(), Err(DriverError::Disconnected(_))));
        assert!(matches!(
            d.emergency_stop(),
            Err(DriverError::Disconnected(_))
        ));
        fake.inner.lock().unwrap().down = false;
        let (o, ofake) = octoprint();
        ofake.inner.lock().unwrap().down = true;
        assert!(matches!(o.state(), Err(DriverError::Disconnected(_))));
        // a route the printer does not have is a refusal with its text
        struct Gone;
        impl Http for Gone {
            fn send(&self, _: &HttpRequest) -> Result<HttpResponse, String> {
                Ok(HttpResponse {
                    status: 404,
                    body: json!({"error": {"message": "not found"}}),
                })
            }
        }
        let g = MoonrakerDriver::new(Gone, None);
        assert_eq!(g.pause(), Err(DriverError::Refused("not found".into())));
        assert_eq!(DriverError::Refused("x".into()).text(), "x");
    }

    #[test]
    fn octoprint_reads_the_state_and_runs_a_print() {
        let (d, fake) = octoprint();
        let s = d.state().unwrap();
        assert_eq!(s.state, "idle", "operational and not printing");
        d.start_print("part.gcode", "G28\n").unwrap();
        assert_eq!(
            fake.inner.lock().unwrap().uploaded,
            vec![("part.gcode".to_string(), 4)]
        );
        let s = d.state().unwrap();
        assert_eq!(s.state, "printing");
        assert_eq!(s.job.name, "part.gcode");
        assert_eq!(s.heater("bed").unwrap().target_c, 0.0);
        d.pause().unwrap();
        assert_eq!(d.state().unwrap().state, "paused");
        d.resume().unwrap();
        d.cancel().unwrap();
        d.set_target("hotend", 210.0).unwrap();
        d.set_target("bed", 55.0).unwrap();
        let (hot, bed) = {
            let f = fake.inner.lock().unwrap();
            (f.hot.1, f.bed.1)
        };
        assert_eq!((hot, bed), (210.0, 55.0));
        assert!(matches!(
            d.set_target("nozzle", 1.0),
            Err(DriverError::Refused(_))
        ));
        d.emergency_stop().unwrap();
        let f = fake.inner.lock().unwrap();
        assert!(f.calls.iter().any(|c| c == "POST /api/printer/command"));
        assert_eq!((f.hot.1, f.bed.1), (0.0, 0.0));
    }

    #[test]
    fn a_line_of_gcode_reaches_the_printer_through_each_driver() {
        let (m, mfake) = moonraker();
        m.gcode("M104 S190").unwrap();
        assert_eq!(mfake.inner.lock().unwrap().hot.1, 190.0);
        let (o, ofake) = octoprint();
        o.gcode("G28").unwrap();
        assert!(ofake
            .inner
            .lock()
            .unwrap()
            .calls
            .contains(&"POST /api/printer/command".to_string()));
        assert_eq!(
            ofake.inner.lock().unwrap().state,
            "standby",
            "a line that is not M112 does not stop the fake"
        );
    }

    #[test]
    fn octoprint_says_offline_when_it_is_not_connected_and_finished_when_it_is_done() {
        let (d, fake) = octoprint();
        fake.inner.lock().unwrap().closed = true;
        let s = d.state().unwrap();
        assert_eq!((s.state, s.raw.as_str()), ("offline", "closed"));
        fake.inner.lock().unwrap().closed = false;
        fake.inner.lock().unwrap().progress = 100.0;
        assert_eq!(d.state().unwrap().state, "finished");
    }

    #[test]
    fn the_address_of_a_printer_is_read_and_a_secure_one_is_refused() {
        let t = Duration::from_secs(1);
        let a = TcpHttp::new("http://192.168.1.50:7125/", t).unwrap();
        assert_eq!((a.host.as_str(), a.port), ("192.168.1.50", 7125));
        let b = TcpHttp::new("printer.local", t).unwrap();
        assert_eq!((b.host.as_str(), b.port), ("printer.local", 80));
        assert!(TcpHttp::new("https://x:1", t)
            .unwrap_err()
            .contains("https"));
        for bad in [
            "ftp://x",
            "",
            "http://",
            "host:0",
            "host:99999",
            "host/path",
            "a b:80",
        ] {
            assert!(TcpHttp::new(bad, t).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_answers_are_read_whole_chunked_or_not() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"a\": true}";
        let r = parse_answer(plain).unwrap();
        assert_eq!((r.status, r.body["a"].clone()), (200, json!(true)));
        let chunked = b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"b\":\r\n3\r\n 1}\r\n0\r\n\r\n";
        let c = parse_answer(chunked).unwrap();
        assert_eq!((c.status, c.body["b"].clone()), (201, json!(1)));
        let empty = parse_answer(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert_eq!((empty.status, empty.body), (204, Value::Null));
        let text = parse_answer(b"HTTP/1.1 500 Oops\r\n\r\nplain words").unwrap();
        assert_eq!(text.body["text"], "plain words");
        for bad in [
            &b"nonsense"[..],
            b"HTTP/1.1 abc\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
        ] {
            assert!(parse_answer(bad).is_err());
        }
    }

    #[test]
    fn a_request_is_written_with_its_body_and_its_upload() {
        let json_req = HttpRequest {
            method: "POST",
            path: "/api/job".into(),
            headers: vec![("X-Api-Key".into(), "k".into())],
            body: HttpBody::Json(json!({"command": "cancel"})),
        };
        let text = String::from_utf8(encode(&json_req, "h:80")).unwrap();
        assert!(text.starts_with("POST /api/job HTTP/1.1\r\nHost: h:80\r\n"));
        assert!(
            text.contains("X-Api-Key: k\r\n")
                && text.contains("Content-Type: application/json\r\n")
        );
        assert!(
            text.ends_with("{\"command\":\"cancel\"}") && text.contains("Content-Length: 20\r\n")
        );
        let up = HttpRequest {
            method: "POST",
            path: "/x".into(),
            headers: Vec::new(),
            body: HttpBody::Upload {
                field: "file",
                filename: "a.gcode".into(),
                bytes: b"G28\n".to_vec(),
                fields: vec![("print".into(), "true".into())],
            },
        };
        let text = String::from_utf8(encode(&up, "h:80")).unwrap();
        assert!(text.contains("multipart/form-data; boundary="));
        assert!(text.contains("name=\"print\"\r\n\r\ntrue\r\n"));
        assert!(
            text.contains("name=\"file\"; filename=\"a.gcode\"") && text.contains("G28\n\r\n--")
        );
    }

    #[test]
    fn the_tcp_client_talks_to_a_server_on_a_socket_of_this_computer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            let seen = String::from_utf8_lossy(&buf[..n]).to_string();
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"ok\": true}").unwrap();
            seen
        });
        let http =
            TcpHttp::new(&format!("http://127.0.0.1:{port}"), Duration::from_secs(5)).unwrap();
        let r = http
            .send(&HttpRequest {
                method: "GET",
                path: "/printer/info".into(),
                headers: vec![("X-Api-Key".into(), "abc".into())],
                body: HttpBody::None,
            })
            .unwrap();
        assert_eq!((r.status, r.body["ok"].clone()), (200, json!(true)));
        let seen = server.join().unwrap();
        assert!(
            seen.starts_with("GET /printer/info HTTP/1.1\r\n")
                && seen.contains("X-Api-Key: abc\r\n"),
            "{seen}"
        );
        // nothing listens any more: the printer cannot be reached
        let gone = TcpHttp::new(
            &format!("http://127.0.0.1:{port}"),
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(gone
            .send(&HttpRequest {
                method: "GET",
                path: "/".into(),
                headers: Vec::new(),
                body: HttpBody::None
            })
            .is_err());
    }

    #[test]
    fn the_compatibility_shapes_carry_the_state_in_the_words_of_each_program() {
        let s = PrinterState {
            state: "printing",
            raw: "printing".into(),
            heaters: vec![
                Heater {
                    name: "hotend",
                    current_c: 200.0,
                    target_c: 210.0,
                    power_pct: 40.0,
                },
                Heater {
                    name: "bed",
                    current_c: 60.0,
                    target_c: 60.0,
                    power_pct: 0.0,
                },
            ],
            job: PrinterJob {
                name: "a.gcode".into(),
                progress_pct: 25.0,
                layer: Some(2),
                layers: Some(8),
                z_mm: None,
                elapsed_s: 30.0,
            },
            t_s: 5.0,
        };
        let m = moonraker_shape(&s);
        assert_eq!(m["result"]["status"]["extruder"]["target"], 210.0);
        assert_eq!(m["result"]["status"]["extruder"]["power"], 0.4);
        assert_eq!(m["result"]["status"]["print_stats"]["state"], "printing");
        assert_eq!(m["result"]["status"]["virtual_sdcard"]["progress"], 0.25);
        assert_eq!(m["result"]["status"]["webhooks"]["state"], "ready");
        let o = octoprint_shape(&s);
        assert_eq!(o["printer"]["temperature"]["tool0"]["actual"], 200.0);
        assert_eq!(o["printer"]["state"]["flags"]["printing"], true);
        assert_eq!(o["job"]["progress"]["completion"], 25.0);
        let stopped = PrinterState {
            state: "emergency",
            ..s.clone()
        };
        assert_eq!(
            moonraker_shape(&stopped)["result"]["status"]["webhooks"]["state"],
            "shutdown"
        );
        assert_eq!(
            moonraker_shape(&stopped)["result"]["status"]["print_stats"]["state"],
            "error"
        );
        assert_eq!(
            octoprint_shape(&stopped)["printer"]["state"]["flags"]["error"],
            true
        );
        for state in STATES {
            let any = PrinterState { state, ..s.clone() };
            assert!(moonraker_shape(&any)["result"]["status"]["print_stats"]["state"].is_string());
            assert!(octoprint_shape(&any)["printer"]["state"]["flags"].is_object());
        }
    }
}
