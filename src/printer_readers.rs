//! The readers of the printers, one for each family of protocol. A reader **only reads**: the trait [`StatusReader`] has no way to ask a printer to do anything, the reader of
//! Moonraker is built on [`GetOnly`], which refuses every request that is not a `GET` with no body, and the reader of a Bambu printer writes only what MQTT needs to be
//! told (the connection, the subscription to the reports, the acknowledgements and the pings) and the one request that asks for the whole state (`pushall`).
//!
//! The Bambu reader uses the pinned TLS of `src/tls_pin.rs` and the packets of `src/bambu_mqtt.rs`; it never sends a byte to a printer whose certificate is not the pinned
//! one. Both readers have only been tried against printers in memory: nothing has run on a real printer.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::bambu::{normalize_fingerprint, TrustError};
use crate::bambu_mqtt::{
    connect_packet, parse_publish, ping_packet, publish_packet, pushall_document, read_packet,
    subscribe_packet, MqttError, Packet,
};
use crate::printer_drivers::{Http, HttpBody, HttpRequest, HttpResponse};
use crate::printer_status::{
    bambu_status, merge_bambu, moonraker_camera, moonraker_status, Status,
};
use crate::tls_pin::{self, PinError, PinnedStream};

/// Why a printer could not be read. The words are fixed: nothing a printer sent and no key is ever part of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// It cannot be reached, or the line broke.
    Unreachable,
    /// The line is open and the printer has not said its state.
    NoReport,
    /// It refused the access code or the key.
    Refused,
    /// No fingerprint is pinned; `seen` is the certificate the printer shows.
    NotPinned { seen: String },
    /// The printer shows another certificate than the pinned one.
    Changed { seen: String },
    /// The pinned text is not a fingerprint.
    BadFingerprint,
    /// The printer answered something that cannot be read as its state.
    BadReport,
}

impl ReadError {
    pub fn word(&self) -> &'static str {
        match self {
            ReadError::Unreachable => "unreachable",
            ReadError::NoReport => "no_report",
            ReadError::Refused => "access_refused",
            ReadError::NotPinned { .. } => "certificate_not_pinned",
            ReadError::Changed { .. } => "certificate_changed",
            ReadError::BadFingerprint => "bad_fingerprint",
            ReadError::BadReport => "bad_report",
        }
    }

    /// The certificate the printer showed, when the refusal is about it.
    pub fn certificate_seen(&self) -> Option<&str> {
        match self {
            ReadError::NotPinned { seen } | ReadError::Changed { seen } => Some(seen),
            _ => None,
        }
    }
}

/// What a printer said and how old it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub status: Status,
    /// How long ago the printer last said something of its state (zero for a printer that is asked each time).
    pub report_age: Duration,
}

/// A way to know the state of a printer, and nothing else.
pub trait StatusReader: Send {
    fn read(&mut self) -> Result<Reading, ReadError>;
}

// ---- Klipper through Moonraker ---------------------------------------------------------------------------------------------------------

/// The way to Moonraker that can only look: every request that is not a `GET` with no body is refused before it reaches the network.
pub struct GetOnly<H: Http>(H);

impl<H: Http> GetOnly<H> {
    pub fn new(http: H) -> GetOnly<H> {
        GetOnly(http)
    }

    /// The transport underneath.
    pub fn inner(&self) -> &H {
        &self.0
    }
}

impl<H: Http> Http for GetOnly<H> {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
        if request.method != "GET" || request.body != HttpBody::None {
            return Err("the status reader only reads".into());
        }
        self.0.send(request)
    }
}

/// The objects of Klipper that are asked for.
pub const MOONRAKER_QUERY: &str = "/printer/objects/query?extruder&heater_bed&heater_generic%20chamber&temperature_sensor%20chamber&print_stats&virtual_sdcard&webhooks";
/// The list of cameras of Moonraker.
pub const MOONRAKER_WEBCAMS: &str = "/server/webcams/list";

pub struct MoonrakerReader<H: Http> {
    http: GetOnly<H>,
    api_key: Option<String>,
    camera: Option<bool>,
}

impl<H: Http> MoonrakerReader<H> {
    pub fn new(http: H, api_key: Option<String>) -> MoonrakerReader<H> {
        MoonrakerReader {
            http: GetOnly::new(http),
            api_key: api_key.filter(|k| !k.is_empty()),
            camera: None,
        }
    }

    pub fn transport(&self) -> &H {
        self.http.inner()
    }

    fn get(&self, path: &str) -> Result<HttpResponse, ReadError> {
        let headers = self
            .api_key
            .iter()
            .map(|k| ("X-Api-Key".to_string(), k.clone()))
            .collect();
        let answer = self
            .http
            .send(&HttpRequest {
                method: "GET",
                path: path.to_string(),
                headers,
                body: HttpBody::None,
            })
            .map_err(|_| ReadError::Unreachable)?;
        match answer.status {
            200..=299 => Ok(answer),
            401 | 403 => Err(ReadError::Refused),
            _ => Err(ReadError::BadReport),
        }
    }
}

impl<H: Http> StatusReader for MoonrakerReader<H> {
    fn read(&mut self) -> Result<Reading, ReadError> {
        let answer = self.get(MOONRAKER_QUERY)?;
        let mut status = moonraker_status(&answer.body).ok_or(ReadError::BadReport)?;
        // the cameras are asked once; a Moonraker that does not answer that is only a printer whose camera is unknown
        if self.camera.is_none() {
            self.camera = self
                .get(MOONRAKER_WEBCAMS)
                .ok()
                .and_then(|a| moonraker_camera(&a.body));
        }
        status.camera = self
            .camera
            .map(|present| crate::printer_status::Camera { present });
        Ok(Reading {
            status,
            report_age: Duration::ZERO,
        })
    }
}

// ---- Bambu Lab on the LAN --------------------------------------------------------------------------------------------------------------

/// How long between two requests for the whole state while the printer talks. The reports in between carry what changed; asking more often loads the small printers.
const PUSHALL_EVERY: Duration = Duration::from_secs(300);
/// A printer that has said nothing for this long is asked for the whole state again.
const PUSHALL_QUIET: Duration = Duration::from_secs(60);
const PING_EVERY: Duration = Duration::from_secs(30);
/// The most packets one reading takes in, so that a printer that never stops talking does not keep the reading from ending.
const MAX_PACKETS: usize = 1000;
/// How long the reader waits for one more packet once it has the state.
const NO_MORE: Duration = Duration::from_millis(40);

/// A packet read with a short wait for its first byte and, once it has begun, with the time of the whole reading.
struct Whole<'a> {
    stream: &'a mut PinnedStream,
    full: Duration,
    deadline: Instant,
    begun: bool,
}

impl Read for Whole<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.begun && Instant::now() > self.deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "too slow"));
        }
        let n = self.stream.stream.read(buffer)?;
        if !self.begun && n > 0 {
            self.begun = true;
            let _ = self.stream.stream.sock.set_read_timeout(Some(self.full));
        }
        Ok(n)
    }
}

struct Session {
    stream: PinnedStream,
    known: Map<String, Value>,
    heard: Option<Instant>,
    asked: Instant,
    pinged: Instant,
    sequence: u32,
    unreadable: bool,
}

impl Session {
    fn send(&mut self, bytes: &[u8]) -> Result<(), ReadError> {
        self.stream
            .stream
            .write_all(bytes)
            .and_then(|()| self.stream.stream.flush())
            .map_err(|_| ReadError::Unreachable)
    }

    fn next(
        &mut self,
        wait: Duration,
        full: Duration,
        deadline: Instant,
    ) -> Result<Packet, MqttError> {
        let _ = self.stream.stream.sock.set_read_timeout(Some(wait));
        read_packet(&mut Whole {
            stream: &mut self.stream,
            full,
            deadline,
            begun: false,
        })
    }
}

fn line_error(e: MqttError) -> ReadError {
    match e {
        MqttError::Timeout | MqttError::Closed | MqttError::Io(_) => ReadError::Unreachable,
        MqttError::Malformed(_) => ReadError::BadReport,
    }
}

/// The reader of a Bambu Lab printer in LAN mode. The connection is kept open between readings.
pub struct BambuReader {
    host: String,
    port: u16,
    serial: String,
    access_code: String,
    pinned: Option<String>,
    timeout: Duration,
    session: Option<Session>,
}

impl BambuReader {
    /// `serial` goes into the names of the topics, so it must be letters and digits only (a `#` or a `+` would subscribe to more than this printer).
    pub fn new(
        host: &str,
        port: u16,
        serial: &str,
        access_code: &str,
        pinned: Option<&str>,
        timeout: Duration,
    ) -> BambuReader {
        BambuReader {
            host: host.to_string(),
            port,
            serial: serial.chars().filter(char::is_ascii_alphanumeric).collect(),
            access_code: access_code.to_string(),
            pinned: pinned.filter(|p| !p.is_empty()).map(str::to_string),
            timeout,
            session: None,
        }
    }

    fn report_topic(&self) -> String {
        format!("device/{}/report", self.serial)
    }

    /// The only message the reader ever publishes: the request for the whole state.
    fn ask_everything(&self, session: &mut Session) -> Result<(), ReadError> {
        session.sequence = session.sequence.wrapping_add(1);
        let topic = format!("device/{}/request", self.serial);
        let document = pushall_document(session.sequence);
        session.send(&publish_packet(&topic, document.as_bytes()))?;
        session.asked = Instant::now();
        Ok(())
    }

    fn open(&self) -> Result<Session, ReadError> {
        if let Some(p) = &self.pinned {
            normalize_fingerprint(p).ok_or(ReadError::BadFingerprint)?;
        }
        let stream = tls_pin::connect(&self.host, self.port, self.pinned.as_deref(), self.timeout)
            .map_err(|e| match e {
                PinError::Untrusted(TrustError::NotPinned { seen }) => {
                    ReadError::NotPinned { seen }
                }
                PinError::Untrusted(TrustError::Mismatch { seen, .. }) => {
                    ReadError::Changed { seen }
                }
                PinError::Untrusted(TrustError::Malformed) => ReadError::BadFingerprint,
                PinError::Down(_) => ReadError::Unreachable,
            })?;
        let now = Instant::now();
        let deadline = now + self.timeout;
        let mut session = Session {
            stream,
            known: Map::new(),
            heard: None,
            asked: now,
            pinged: now,
            sequence: 0,
            unreadable: false,
        };
        let client = format!("eh3d-status-{:08x}", std::process::id());
        session.send(&connect_packet(&client, "bblp", &self.access_code, 60))?;
        let reply = session
            .next(self.timeout, self.timeout, deadline)
            .map_err(line_error)?;
        if reply.kind != 2 || reply.body.len() != 2 {
            return Err(ReadError::BadReport);
        }
        if reply.body[1] != 0 {
            return Err(ReadError::Refused);
        }
        session.send(&subscribe_packet(1, &self.report_topic()))?;
        for _ in 0..MAX_PACKETS {
            if Instant::now() > deadline {
                break;
            }
            let p = session
                .next(self.timeout, self.timeout, deadline)
                .map_err(line_error)?;
            if p.kind == 9 {
                if p.body.last().is_none_or(|rc| *rc == 0x80) {
                    return Err(ReadError::Refused);
                }
                self.ask_everything(&mut session)?;
                return Ok(session);
            }
        }
        Err(ReadError::Unreachable)
    }

    /// Takes in what the printer has sent.
    fn take_in(&self, session: &mut Session) -> Result<(), ReadError> {
        let deadline = Instant::now() + self.timeout;
        let topic = self.report_topic();
        let mut wait = if session.heard.is_none() {
            self.timeout
        } else {
            NO_MORE
        };
        for _ in 0..MAX_PACKETS {
            let p = match session.next(wait, self.timeout, deadline) {
                Ok(p) => p,
                Err(MqttError::Timeout) => return Ok(()),
                Err(e) => return Err(line_error(e)),
            };
            if p.kind == 3 {
                let message = parse_publish(&p).map_err(line_error)?;
                if let Some(id) = message.ack {
                    // PUBACK
                    let mut ack = vec![0x40, 0x02];
                    ack.extend_from_slice(&id.to_be_bytes());
                    session.send(&ack)?;
                }
                if message.topic == topic {
                    match serde_json::from_slice::<Value>(&message.payload) {
                        Ok(document) => {
                            if merge_bambu(&mut session.known, &document) {
                                session.heard = Some(Instant::now());
                                wait = NO_MORE;
                            }
                        }
                        Err(_) => session.unreadable = true,
                    }
                }
            }
            if Instant::now() > deadline {
                break;
            }
        }
        Ok(())
    }

    fn turn(&self, session: &mut Session) -> Result<Reading, ReadError> {
        let quiet = session.heard.is_none_or(|h| h.elapsed() >= PUSHALL_QUIET);
        if session.asked.elapsed() >= PUSHALL_EVERY
            || (quiet && session.asked.elapsed() >= PUSHALL_QUIET)
        {
            self.ask_everything(session)?;
        }
        if session.pinged.elapsed() >= PING_EVERY {
            session.send(&ping_packet())?;
            session.pinged = Instant::now();
        }
        self.take_in(session)?;
        let (Some(heard), Some(status)) = (session.heard, bambu_status(&session.known)) else {
            return Err(if session.unreadable {
                ReadError::BadReport
            } else {
                ReadError::NoReport
            });
        };
        Ok(Reading {
            status,
            report_age: heard.elapsed(),
        })
    }
}

impl StatusReader for BambuReader {
    fn read(&mut self) -> Result<Reading, ReadError> {
        let mut session = match self.session.take() {
            Some(s) => s,
            None => self.open()?,
        };
        let result = self.turn(&mut session);
        // a line that failed or confused the reader is not used again, nor one that has been silent for too long; one that has only said nothing yet is kept
        let keep = match &result {
            Ok(reading) => reading.report_age < PUSHALL_QUIET * 3,
            Err(e) => *e == ReadError::NoReport,
        };
        if keep {
            self.session = Some(session);
        }
        result
    }
}

// ---- the simulated printers ------------------------------------------------------------------------------------------------------------

/// The length of the print a simulated printer repeats, in seconds.
const SIM_PRINT_S: u64 = 1200;
const SIM_LAYERS: u64 = 120;

/// The words a simulated printer of a family says `elapsed` seconds after it began: it prints the same part over and over. The words go through the same readers as the
/// ones of a real printer.
pub fn simulated_document(family: &str, elapsed: Duration) -> Value {
    let at = elapsed.as_secs() % SIM_PRINT_S;
    let done = at as f64 / SIM_PRINT_S as f64;
    let layer = at * SIM_LAYERS / SIM_PRINT_S + 1;
    if family == "moonraker" {
        serde_json::json!({"result": {"status": {
            "extruder": {"temperature": 210.0, "target": 210.0}, "heater_bed": {"temperature": 60.0, "target": 60.0},
            "print_stats": {"state": "printing", "filename": "simulated.gcode", "print_duration": at, "info": {"current_layer": layer, "total_layer": SIM_LAYERS}},
            "virtual_sdcard": {"progress": done}, "webhooks": {"state": "ready"},
        }}})
    } else {
        serde_json::json!({"print": {
            "gcode_state": "RUNNING", "nozzle_temper": 220.0, "nozzle_target_temper": 220.0, "bed_temper": 60.0, "bed_target_temper": 60.0,
            "mc_percent": (done * 100.0).floor(), "mc_remaining_time": (SIM_PRINT_S - at).div_ceil(60), "layer_num": layer, "total_layer_num": SIM_LAYERS,
            "subtask_name": "simulated", "print_error": 0, "hms": [],
        }})
    }
}

/// A printer that lives in memory.
pub struct SimReader {
    family: String,
    began: Instant,
}

impl SimReader {
    pub fn new(family: &str) -> SimReader {
        SimReader {
            family: family.to_string(),
            began: Instant::now(),
        }
    }
}

impl StatusReader for SimReader {
    fn read(&mut self) -> Result<Reading, ReadError> {
        let document = simulated_document(&self.family, self.began.elapsed());
        let status = if self.family == "moonraker" {
            moonraker_status(&document)
        } else {
            let mut known = Map::new();
            merge_bambu(&mut known, &document);
            bambu_status(&known)
        };
        Ok(Reading {
            status: status.ok_or(ReadError::BadReport)?,
            report_age: Duration::ZERO,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::sync::Locked;
    use crate::tls_pin::tests::TestServer;

    const SERIAL: &str = "01P00A111111111";
    const CODE: &str = "12345678";
    const SECOND: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(600);
    const REPORT: &str = r#"{"print":{"gcode_state":"RUNNING","nozzle_temper":205.5,"nozzle_target_temper":210,"bed_temper":59.9,"bed_target_temper":60,"mc_percent":42,"layer_num":10,"total_layer_num":100,"mc_remaining_time":33,"subtask_name":"cube"}}"#;

    /// What a Moonraker in memory was asked, and what it answers.
    struct Recorder {
        asked: Mutex<Vec<HttpRequest>>,
        status: Mutex<u16>,
        body: Mutex<Value>,
        down: Mutex<bool>,
    }

    impl Recorder {
        fn new() -> Recorder {
            Recorder {
                asked: Mutex::new(Vec::new()),
                status: Mutex::new(200),
                body: Mutex::new(simulated_document("moonraker", Duration::from_secs(600))),
                down: Mutex::new(false),
            }
        }
    }

    impl Http for Recorder {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
            self.asked.locked().push(request.clone());
            if *self.down.locked() {
                return Err("cannot connect".into());
            }
            let body = if request.path == MOONRAKER_WEBCAMS {
                json!({"result": {"webcams": [{"name": "cam", "stream_url": "http://user:pw@10.0.0.9/stream"}]}})
            } else {
                self.body.locked().clone()
            };
            Ok(HttpResponse {
                status: *self.status.locked(),
                body,
            })
        }
    }

    #[test]
    fn the_status_reader_of_moonraker_cannot_send_anything_but_a_look() {
        let only = GetOnly::new(Recorder::new());
        for (method, body) in [
            ("POST", HttpBody::None),
            ("PUT", HttpBody::None),
            ("DELETE", HttpBody::None),
            ("get", HttpBody::None),
            ("GET", HttpBody::Json(json!({"script": "M112"}))),
        ] {
            let request = HttpRequest {
                method,
                path: "/printer/emergency_stop".into(),
                headers: Vec::new(),
                body,
            };
            assert!(only.send(&request).is_err(), "{method}");
        }
        assert!(
            only.inner().asked.locked().is_empty(),
            "nothing reached the printer"
        );
    }

    #[test]
    fn many_readings_of_moonraker_only_ever_get_the_status_and_the_cameras() {
        let mut reader = MoonrakerReader::new(Recorder::new(), Some("a-key".into()));
        for _ in 0..50 {
            let r = reader.read().unwrap();
            assert_eq!(r.status.state, "printing");
            assert_eq!(r.status.camera.as_ref().map(|c| c.present), Some(true));
            assert!(!serde_json::to_string(&r.status)
                .unwrap()
                .contains("10.0.0.9"));
        }
        let asked = reader.transport().asked.locked();
        assert_eq!(asked.len(), 51, "the cameras are asked once");
        for request in asked.iter() {
            assert_eq!(request.method, "GET");
            assert_eq!(request.body, HttpBody::None);
            assert!(
                request.path == MOONRAKER_QUERY || request.path == MOONRAKER_WEBCAMS,
                "{}",
                request.path
            );
            assert_eq!(
                request.headers,
                vec![("X-Api-Key".to_string(), "a-key".to_string())]
            );
        }
    }

    #[test]
    fn a_moonraker_that_fails_gives_a_fixed_word_with_no_key_in_it() {
        let mut reader = MoonrakerReader::new(Recorder::new(), Some("key-not-to-show".into()));
        let mut words = Vec::new();
        for (status, body, down) in [
            (401, json!({}), false),
            (403, json!({}), false),
            (500, json!({"error": "key-not-to-show"}), false),
            (200, json!({"result": {}}), false),
            (200, json!({"text": "<html>"}), false),
            (200, Value::Null, false),
            (200, json!({}), true),
        ] {
            *reader.transport().status.locked() = status;
            *reader.transport().body.locked() = body;
            *reader.transport().down.locked() = down;
            let error = reader.read().unwrap_err();
            assert!(!format!("{error:?}").contains("key-not-to-show"));
            words.push(error.word());
        }
        assert_eq!(
            words,
            vec![
                "access_refused",
                "access_refused",
                "bad_report",
                "bad_report",
                "bad_report",
                "bad_report",
                "unreachable"
            ]
        );
        let none = MoonrakerReader::new(Recorder::new(), Some(String::new()));
        assert!(none.api_key.is_none());
    }

    /// Everything a broker in the test received: the kind of each packet and its body.
    type Seen = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

    /// What the broker does once the reader has subscribed.
    #[derive(Clone, Copy, PartialEq)]
    enum Mood {
        Reports,
        Silent,
        Rubbish,
        Closes,
        Chatters,
        Huge,
    }

    fn broker(
        mood: Mood,
        seen: Seen,
    ) -> impl Fn(&mut rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream>) + Send + 'static
    {
        let report = format!("device/{SERIAL}/report");
        move |tls| {
            let Ok(connect) = read_packet(tls) else {
                return;
            };
            let ok = String::from_utf8_lossy(&connect.body).contains(CODE);
            seen.locked().push((connect.kind, connect.body));
            let _ = tls.write_all(&[0x20, 0x02, 0x00, if ok { 0 } else { 5 }]);
            let _ = tls.flush();
            if !ok {
                return;
            }
            let Ok(sub) = read_packet(tls) else { return };
            let _ = tls.write_all(&[0x90, 0x03, sub.body[0], sub.body[1], 0x00]);
            let _ = tls.flush();
            seen.locked().push((sub.kind, sub.body));
            while let Ok(p) = read_packet(tls) {
                let asks = p.kind == 3 && String::from_utf8_lossy(&p.body).contains("pushall");
                seen.locked().push((p.kind, p.body));
                if !asks {
                    continue;
                }
                match mood {
                    Mood::Reports => {
                        let _ = tls.write_all(&publish_packet(&report, REPORT.as_bytes()));
                        // a message that wants to be acknowledged (quality of service 1)
                        let mut body = vec![0, report.len() as u8];
                        body.extend_from_slice(report.as_bytes());
                        body.extend_from_slice(&[0, 7]);
                        body.extend_from_slice(br#"{"print":{"mc_percent":43}}"#);
                        let mut packet = vec![0x32, body.len() as u8];
                        packet.extend(body);
                        let _ = tls.write_all(&packet);
                    }
                    Mood::Silent => {}
                    Mood::Rubbish => {
                        let cut = br#"{"print":{"nozzle_temper":"#;
                        let _ = tls.write_all(&publish_packet(&report, cut));
                    }
                    Mood::Closes => return,
                    Mood::Chatters => {
                        let other = publish_packet("device/other/report", REPORT.as_bytes());
                        while tls.write_all(&other).and_then(|()| tls.flush()).is_ok() {}
                        return;
                    }
                    Mood::Huge => {
                        let _ = tls.write_all(&[0x30, 0xFF, 0xFF, 0xFF, 0x7F]);
                    }
                }
                let _ = tls.flush();
            }
        }
    }

    fn reader(
        server: &TestServer,
        code: &str,
        pin: Option<&str>,
        timeout: Duration,
    ) -> BambuReader {
        BambuReader::new("127.0.0.1", server.port, SERIAL, code, pin, timeout)
    }

    #[test]
    fn a_bambu_printer_is_read_and_the_only_thing_ever_published_asks_for_the_state() {
        let seen: Seen = Arc::default();
        let server = TestServer::start_with(1, broker(Mood::Reports, seen.clone()));
        let mut r = reader(&server, CODE, Some(&server.fingerprint), SECOND);
        let first = r.read().unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            (first.status.state, first.status.layers),
            ("printing", Some(100))
        );
        for _ in 0..20 {
            let again = r.read().unwrap();
            assert_eq!(again.status.job_name, "cube");
        }
        assert_eq!(r.read().unwrap().status.progress_pct, Some(43.0));
        drop(r);
        std::thread::sleep(Duration::from_millis(300));
        let seen = seen.locked();
        let request = format!("device/{SERIAL}/request");
        let mut published = 0;
        for (kind, body) in seen.iter() {
            // CONNECT, PUBLISH, PUBACK, SUBSCRIBE, PINGREQ and DISCONNECT are the only kinds of packet a reader has a use for
            assert!(
                matches!(kind, 1 | 3 | 4 | 8 | 12 | 14),
                "a packet of kind {kind}"
            );
            if *kind == 3 {
                let packet = Packet {
                    kind: 3,
                    flags: 0,
                    body: body.clone(),
                };
                let p = parse_publish(&packet).unwrap();
                assert_eq!(p.topic, request);
                let document: Value = serde_json::from_slice(&p.payload).unwrap();
                assert_eq!(document["pushing"]["command"], "pushall");
                assert_eq!(document.as_object().unwrap().len(), 1, "{document}");
                published += 1;
            }
        }
        assert_eq!(published, 1, "the whole state is asked for once");
        assert!(
            seen.iter().any(|(k, b)| *k == 4 && b[..] == [0, 7]),
            "the acknowledgement"
        );
    }

    #[test]
    fn a_printer_whose_certificate_is_not_the_pinned_one_gets_no_byte() {
        let seen: Seen = Arc::default();
        let server = TestServer::start_with(8, broker(Mood::Reports, seen.clone()));
        let shown = Some(server.fingerprint.as_str());
        let none = reader(&server, CODE, None, SECOND).read().unwrap_err();
        assert_eq!(
            (none.word(), none.certificate_seen()),
            ("certificate_not_pinned", shown)
        );
        let other = "b".repeat(64);
        let changed = reader(&server, CODE, Some(&other), SECOND)
            .read()
            .unwrap_err();
        assert_eq!(
            (changed.word(), changed.certificate_seen()),
            ("certificate_changed", shown)
        );
        let bad = reader(&server, CODE, Some("xyz"), SECOND)
            .read()
            .unwrap_err();
        assert_eq!(bad, ReadError::BadFingerprint);
        assert!(
            seen.locked().is_empty(),
            "nothing was said to a printer that is not trusted"
        );
        let refused = reader(&server, "00000000", Some(&server.fingerprint), SECOND)
            .read()
            .unwrap_err();
        assert_eq!(refused.word(), "access_refused");
        assert!(!format!("{refused:?}").contains("00000000"));
    }

    #[test]
    fn a_silent_confused_closing_or_chattering_printer_ends_each_reading_in_time() {
        for (mood, word) in [
            (Mood::Silent, "no_report"),
            (Mood::Rubbish, "bad_report"),
            (Mood::Closes, "unreachable"),
            (Mood::Chatters, "no_report"),
            (Mood::Huge, "bad_report"),
        ] {
            let server = TestServer::start_with(2, broker(mood, Arc::default()));
            let mut r = reader(&server, CODE, Some(&server.fingerprint), SHORT);
            let began = Instant::now();
            let error = r.read().unwrap_err();
            assert_eq!(error.word(), word);
            assert!(
                began.elapsed() < Duration::from_secs(20),
                "{word} took {:?}",
                began.elapsed()
            );
        }
    }

    #[test]
    fn a_printer_that_is_not_there_is_unreachable_and_a_serial_cannot_widen_the_topic() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let pin = "a".repeat(64);
        let mut r = BambuReader::new("127.0.0.1", port, "#/+ \r\nAB1", CODE, Some(&pin), SHORT);
        assert_eq!(r.report_topic(), "device/AB1/report");
        assert_eq!(r.read().unwrap_err(), ReadError::Unreachable);
    }

    #[test]
    fn the_simulated_printers_speak_through_the_same_readers() {
        for family in crate::printer_status::FAMILIES {
            let r = SimReader::new(family).read().unwrap();
            assert_eq!(r.status.state, "printing", "{family}");
            assert!(r.status.layers.is_some() && r.status.progress_pct.is_some());
            for at in [0, 1, 599, 1199, 1200, 86_400] {
                let document = simulated_document(family, Duration::from_secs(at));
                assert!(document.is_object(), "{family} {at}");
            }
        }
    }
}
