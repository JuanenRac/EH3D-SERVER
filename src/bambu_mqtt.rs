//! The link to a real Bambu Lab printer in LAN mode: a small MQTT 3.1.1 client (the packets of connecting, subscribing, publishing and answering, nothing more) over the TLS of
//! `src/tls_pin.rs`, and the link (`BambuLink`) that the driver of `src/printer_lan.rs` uses with it. It is an **experiment** and everything here is tried against a printer in
//! memory behind a TLS server that the tests make; nothing has run on a real A1, P1S or other Bambu printer (`evidence` is `simulator`).
//!
//! The printer is reached on the port of its broker (8883) with the user `bblp` and its access code as the password. The server reads what it reports on `device/<serial>/report`
//! (a JSON document with a `print` object whose fields come a few at a time, so they are kept and merged) and writes its commands on `device/<serial>/request`. The commands
//! the link sends are hold, go on, cancel and one line of G-code; **starting a file is not served**: it needs the file to be given over FTPS, which is not written, so the
//! answer to that is a refusal that says so. The certificate of the printer is pinned (`tls_pin`): with none pinned, or another, nothing is sent.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::bambu::{BambuCommand, BambuReport};
use crate::printer_lan::{BambuLink, LinkError};
use crate::sync::Locked;
use crate::tls_pin::{self, PinError, PinnedStream};

/// The most bytes a packet of the printer may have.
const MAX_PACKET: usize = 4 * 1024 * 1024;
/// How often the whole state is asked for again, in seconds.
const REFRESH_S: u64 = 10;

/// Why the conversation with the broker went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MqttError {
    /// Nothing came in the time that was given.
    Timeout,
    /// The other side closed the connection.
    Closed,
    /// The line failed (the text says how).
    Io(String),
    /// A packet that is not one (the text says what is wrong with it).
    Malformed(&'static str),
}

/// A packet: its type (the high half of the first byte), the flags (the low half) and what follows its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub kind: u8,
    pub flags: u8,
    pub body: Vec<u8>,
}

fn put_length(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let mut byte = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            break;
        }
    }
}

fn put_text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

fn packet(first: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![first];
    put_length(&mut out, body.len());
    out.extend_from_slice(body);
    out
}

/// The packet that opens the conversation: the name of the client, the user and the password, with a clean session.
pub fn connect_packet(client: &str, user: &str, password: &str, keep_alive_s: u16) -> Vec<u8> {
    let mut body = Vec::new();
    put_text(&mut body, "MQTT");
    body.push(4);
    body.push(0xC2);
    body.extend_from_slice(&keep_alive_s.to_be_bytes());
    put_text(&mut body, client);
    put_text(&mut body, user);
    put_text(&mut body, password);
    packet(0x10, &body)
}

/// The packet that asks for a topic (quality of service 0).
pub fn subscribe_packet(id: u16, topic: &str) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    put_text(&mut body, topic);
    body.push(0);
    packet(0x82, &body)
}

/// The packet that sends a message on a topic (quality of service 0).
pub fn publish_packet(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    put_text(&mut body, topic);
    body.extend_from_slice(payload);
    packet(0x30, &body)
}

/// The packet that asks the broker to show it is there.
pub fn ping_packet() -> Vec<u8> {
    vec![0xC0, 0x00]
}

fn io_error(e: &io::Error) -> MqttError {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => MqttError::Timeout,
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => MqttError::Closed,
        _ => MqttError::Io(format!("{e}")),
    }
}

/// Reads one packet. A time limit before the first byte is `Timeout`; one in the middle of a packet is a failure of the line (the packet is lost).
pub fn read_packet<R: Read>(from: &mut R) -> Result<Packet, MqttError> {
    let mut first = [0u8; 1];
    from.read_exact(&mut first).map_err(|e| io_error(&e))?;
    let mid = |e: io::Error| match io_error(&e) {
        MqttError::Timeout => MqttError::Io("the packet stopped in the middle".into()),
        other => other,
    };
    let (mut length, mut shift) = (0usize, 0u32);
    loop {
        let mut byte = [0u8; 1];
        from.read_exact(&mut byte).map_err(mid)?;
        length |= usize::from(byte[0] & 0x7F) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 21 {
            return Err(MqttError::Malformed("a length of more than four bytes"));
        }
    }
    if length > MAX_PACKET {
        return Err(MqttError::Malformed("a packet bigger than the limit"));
    }
    let mut body = vec![0u8; length];
    from.read_exact(&mut body).map_err(mid)?;
    Ok(Packet {
        kind: first[0] >> 4,
        flags: first[0] & 0x0F,
        body,
    })
}

/// A message the broker gave: its topic, its text and, when the broker wants it acknowledged, the number to acknowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub topic: String,
    pub payload: Vec<u8>,
    pub ack: Option<u16>,
}

/// Reads a packet of type 3 (publish).
pub fn parse_publish(p: &Packet) -> Result<Message, MqttError> {
    if p.kind != 3 {
        return Err(MqttError::Malformed("not a publish"));
    }
    let short = MqttError::Malformed("a publish that is cut");
    let len = usize::from(u16::from_be_bytes([
        *p.body.first().ok_or(short.clone())?,
        *p.body.get(1).ok_or(short.clone())?,
    ]));
    let topic = p.body.get(2..2 + len).ok_or(short.clone())?;
    let topic = String::from_utf8(topic.to_vec())
        .map_err(|_| MqttError::Malformed("a topic that is not text"))?;
    let mut at = 2 + len;
    let ack = if (p.flags >> 1) & 3 > 0 {
        let id = u16::from_be_bytes([
            *p.body.get(at).ok_or(short.clone())?,
            *p.body.get(at + 1).ok_or(short.clone())?,
        ]);
        at += 2;
        Some(id)
    } else {
        None
    };
    Ok(Message {
        topic,
        payload: p.body[at..].to_vec(),
        ack,
    })
}

fn state_name(text: &str) -> &'static str {
    match text {
        "IDLE" => "IDLE",
        "PREPARE" => "PREPARE",
        "RUNNING" => "RUNNING",
        "PAUSE" => "PAUSE",
        "FINISH" => "FINISH",
        "FAILED" => "FAILED",
        _ => "UNKNOWN",
    }
}

/// What the printer has said so far, before it has said anything.
pub fn empty_report() -> BambuReport {
    BambuReport {
        gcode_state: "IDLE",
        nozzle_c: 0.0,
        nozzle_target_c: 0.0,
        bed_c: 0.0,
        bed_target_c: 0.0,
        percent: 0,
        layer: 0,
        layers: 0,
        remaining_min: 0,
        subtask: String::new(),
    }
}

/// Merges what a report says into what is known (the printer sends a few fields at a time). Gives whether the report had a `print` object.
pub fn merge_report(known: &mut BambuReport, document: &Value) -> bool {
    let Some(print) = document.get("print") else {
        return false;
    };
    let number = |key: &str| print.get(key).and_then(Value::as_f64);
    let whole = |key: &str| print.get(key).and_then(Value::as_u64);
    if let Some(state) = print.get("gcode_state").and_then(Value::as_str) {
        known.gcode_state = state_name(state);
    }
    if let Some(v) = number("nozzle_temper") {
        known.nozzle_c = v;
    }
    if let Some(v) = number("nozzle_target_temper") {
        known.nozzle_target_c = v;
    }
    if let Some(v) = number("bed_temper") {
        known.bed_c = v;
    }
    if let Some(v) = number("bed_target_temper") {
        known.bed_target_c = v;
    }
    if let Some(v) = whole("mc_percent") {
        known.percent = v.min(100) as u32;
    }
    if let Some(v) = whole("layer_num") {
        known.layer = v;
    }
    if let Some(v) = whole("total_layer_num") {
        known.layers = v;
    }
    if let Some(v) = whole("mc_remaining_time") {
        known.remaining_min = v;
    }
    if let Some(v) = print.get("subtask_name").and_then(Value::as_str) {
        known.subtask = v.chars().take(128).collect();
    }
    true
}

/// A text as a JSON string (with its quotes).
fn json_text(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The document of a command of the print, as the printer reads it.
pub fn request_document(sequence: u32, command: &BambuCommand) -> Result<String, String> {
    let (name, param) = match command {
        BambuCommand::Pause => ("pause", None),
        BambuCommand::Resume => ("resume", None),
        BambuCommand::Stop => ("stop", None),
        BambuCommand::Gcode(line) => {
            let line = line.trim_end_matches('\n');
            if line.is_empty() || line.len() > 200 || line.chars().any(char::is_control) {
                return Err("a line of G-code for the printer is up to 200 characters, with no control characters".into());
            }
            ("gcode_line", Some(format!("{line}\n")))
        }
        BambuCommand::StartFile(_) => {
            return Err("starting a file on a real Bambu printer needs the file to be given over FTPS, which the server does not do yet".into())
        }
    };
    let param = param.map_or(String::new(), |p| format!(",\"param\":{}", json_text(&p)));
    Ok(format!(
        "{{\"print\":{{\"sequence_id\":\"{sequence}\",\"command\":\"{name}\"{param}}}}}"
    ))
}

/// The document that asks the printer for its whole state.
pub fn pushall_document(sequence: u32) -> String {
    format!("{{\"pushing\":{{\"sequence_id\":\"{sequence}\",\"command\":\"pushall\"}}}}")
}

struct Session {
    stream: PinnedStream,
    report: BambuReport,
    have_report: bool,
    asked: Instant,
}

impl Session {
    fn send(&mut self, bytes: &[u8]) -> Result<(), MqttError> {
        self.stream
            .stream
            .write_all(bytes)
            .map_err(|e| io_error(&e))?;
        self.stream.stream.flush().map_err(|e| io_error(&e))
    }

    fn wait(&mut self, limit: Duration) {
        let _ = self.stream.stream.sock.set_read_timeout(Some(limit));
    }
}

/// The link to a Bambu Lab printer on the LAN.
pub struct BambuTlsLink {
    host: String,
    port: u16,
    serial: String,
    access_code: String,
    pinned: Option<String>,
    timeout: Duration,
    session: Mutex<Option<Session>>,
    sequence: AtomicU32,
}

impl BambuTlsLink {
    pub fn new(
        host: &str,
        port: u16,
        serial: &str,
        access_code: Option<&str>,
        pinned: Option<&str>,
        timeout: Duration,
    ) -> BambuTlsLink {
        BambuTlsLink {
            host: host.to_string(),
            port,
            serial: serial.to_string(),
            access_code: access_code.unwrap_or_default().to_string(),
            pinned: pinned.map(str::to_string),
            timeout,
            session: Mutex::new(None),
            sequence: AtomicU32::new(1),
        }
    }

    fn next_sequence(&self) -> u32 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    fn report_topic(&self) -> String {
        format!("device/{}/report", self.serial)
    }

    fn request_topic(&self) -> String {
        format!("device/{}/request", self.serial)
    }

    /// Opens the line: the pinned TLS connection, the broker's welcome with the access code, and the subscription to the reports.
    fn open(&self) -> Result<Session, LinkError> {
        let stream = tls_pin::connect(&self.host, self.port, self.pinned.as_deref(), self.timeout)
            .map_err(|e| match e {
                PinError::Untrusted(why) => LinkError::Down(why.text()),
                PinError::Down(text) => LinkError::Down(text),
            })?;
        let mut session = Session {
            stream,
            report: empty_report(),
            have_report: false,
            asked: Instant::now(),
        };
        let down = |e: MqttError| LinkError::Down(format!("the printer: {e:?}"));
        let client = format!("eh3d-server-{:08x}", std::process::id());
        session
            .send(&connect_packet(&client, "bblp", &self.access_code, 60))
            .map_err(down)?;
        let reply = read_packet(&mut session.stream.stream).map_err(down)?;
        if reply.kind != 2 || reply.body.len() != 2 {
            return Err(LinkError::Down(
                "the printer did not answer the connection like a broker".into(),
            ));
        }
        if reply.body[1] != 0 {
            return Err(LinkError::Refused(
                "the printer refused the access code (or the user)".into(),
            ));
        }
        session
            .send(&subscribe_packet(1, &self.report_topic()))
            .map_err(down)?;
        loop {
            let p = read_packet(&mut session.stream.stream).map_err(down)?;
            if p.kind == 9 {
                if p.body.last().is_none_or(|rc| *rc == 0x80) {
                    return Err(LinkError::Refused(
                        "the printer refused the subscription to its reports".into(),
                    ));
                }
                break;
            }
        }
        Ok(session)
    }

    fn with_session<T>(
        &self,
        work: impl FnOnce(&mut Session) -> Result<T, MqttError>,
    ) -> Result<T, LinkError> {
        let mut guard = self.session.locked();
        if guard.is_none() {
            *guard = Some(self.open()?);
        }
        let session = guard.as_mut().expect("the session was just made");
        match work(session) {
            Ok(value) => Ok(value),
            Err(e) => {
                // a line that failed is not used again: the next question opens a new one
                *guard = None;
                Err(LinkError::Down(format!(
                    "the line to the printer failed: {e:?}"
                )))
            }
        }
    }

    /// Takes in what the printer has sent, up to `limit` of waiting for the first packet; gives whether a report came.
    fn take_in(&self, session: &mut Session, limit: Duration) -> Result<bool, MqttError> {
        let mut got = false;
        session.wait(limit);
        loop {
            match read_packet(&mut session.stream.stream) {
                Ok(p) if p.kind == 3 => {
                    let m = parse_publish(&p)?;
                    if let Some(id) = m.ack {
                        session.send(&packet(0x40, &id.to_be_bytes()))?;
                    }
                    if m.topic == self.report_topic() {
                        if let Ok(document) = serde_json::from_slice::<Value>(&m.payload) {
                            if merge_report(&mut session.report, &document) {
                                got = true;
                                session.have_report = true;
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(MqttError::Timeout) => return Ok(got),
                Err(e) => return Err(e),
            }
            // after the first packet only what is already waiting is taken
            session.wait(Duration::from_millis(30));
        }
    }
}

impl BambuLink for BambuTlsLink {
    fn fingerprint(&self) -> Result<String, LinkError> {
        if let Some(session) = self.session.locked().as_ref() {
            return Ok(session.stream.fingerprint.clone());
        }
        match tls_pin::connect(&self.host, self.port, self.pinned.as_deref(), self.timeout) {
            Ok(connection) => {
                // the certificate was compared with the pin; the conversation opens its own connection
                let fingerprint = connection.fingerprint.clone();
                drop(connection);
                Ok(fingerprint)
            }
            Err(PinError::Untrusted(crate::bambu::TrustError::NotPinned { seen }))
            | Err(PinError::Untrusted(crate::bambu::TrustError::Mismatch { seen, .. })) => Ok(seen),
            Err(PinError::Untrusted(why)) => Err(LinkError::Refused(why.text())),
            Err(PinError::Down(text)) => Err(LinkError::Down(text)),
        }
    }

    fn report(&self) -> Result<BambuReport, LinkError> {
        let wait = self.timeout;
        self.with_session(|session| {
            let due = session.asked.elapsed() >= Duration::from_secs(REFRESH_S);
            if !session.have_report || due {
                let topic = self.request_topic();
                let document = pushall_document(self.next_sequence());
                session.send(&publish_packet(&topic, document.as_bytes()))?;
                session.asked = Instant::now();
            }
            let limit = if session.have_report {
                Duration::from_millis(30)
            } else {
                wait
            };
            self.take_in(session, limit)?;
            if session.have_report {
                Ok(session.report.clone())
            } else {
                Err(MqttError::Timeout)
            }
        })
    }

    fn send(&self, command: &BambuCommand) -> Result<(), LinkError> {
        let document =
            request_document(self.next_sequence(), command).map_err(LinkError::Refused)?;
        let topic = self.request_topic();
        self.with_session(|session| session.send(&publish_packet(&topic, document.as_bytes())))
    }

    fn upload(&self, _name: &str, _bytes: &[u8]) -> Result<(), LinkError> {
        Err(LinkError::Refused(
            "giving a file to a real Bambu printer needs FTPS, which the server does not do yet"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls_pin::tests::TestServer;
    use std::sync::Arc;

    #[test]
    fn the_packets_are_laid_out_as_mqtt_says() {
        assert_eq!(ping_packet(), vec![0xC0, 0x00]);
        let c = connect_packet("id", "bblp", "12345678", 60);
        assert_eq!(c[0], 0x10);
        assert_eq!(&c[2..11], &[0, 4, b'M', b'Q', b'T', b'T', 4, 0xC2, 0]);
        assert_eq!(c[11], 60);
        assert_eq!(c[1] as usize, c.len() - 2);
        let s = subscribe_packet(7, "a/b");
        assert_eq!(s, vec![0x82, 8, 0, 7, 0, 3, b'a', b'/', b'b', 0]);
        let p = publish_packet("t", b"xy");
        assert_eq!(p, vec![0x30, 5, 0, 1, b't', b'x', b'y']);
        // a length of more than 127 takes two bytes
        let big = publish_packet("t", &[b'x'; 200]);
        assert_eq!(&big[..3], &[0x30, 0xCB, 0x01]);
        let back = read_packet(&mut &big[..]).unwrap();
        assert_eq!((back.kind, back.body.len()), (3, 203));
        let m = parse_publish(&back).unwrap();
        assert_eq!((m.topic.as_str(), m.payload.len(), m.ack), ("t", 200, None));
    }

    #[test]
    fn a_packet_that_is_wrong_or_cut_is_an_error_and_not_a_guess() {
        assert_eq!(read_packet(&mut &[][..]).unwrap_err(), MqttError::Closed);
        assert_eq!(
            read_packet(&mut &[0x30u8, 0xFF, 0xFF, 0xFF, 0xFF, 0x01][..]).unwrap_err(),
            MqttError::Malformed("a length of more than four bytes")
        );
        assert!(read_packet(&mut &[0x30u8, 5, 0, 1][..]).is_err());
        let too_big = [0x30u8, 0xFF, 0xFF, 0xFF, 0x7F];
        assert_eq!(
            read_packet(&mut &too_big[..]).unwrap_err(),
            MqttError::Malformed("a packet bigger than the limit")
        );
        let cut = Packet {
            kind: 3,
            flags: 0,
            body: vec![0, 9, b'a'],
        };
        assert!(parse_publish(&cut).is_err());
        assert!(parse_publish(&Packet {
            kind: 2,
            flags: 0,
            body: vec![]
        })
        .is_err());
        // a message with a quality of service 1 carries the number to acknowledge
        let with_id = Packet {
            kind: 3,
            flags: 0b0010,
            body: vec![0, 1, b't', 0, 9, b'z'],
        };
        let m = parse_publish(&with_id).unwrap();
        assert_eq!((m.ack, m.payload), (Some(9), vec![b'z']));
    }

    #[test]
    fn a_report_is_merged_a_few_fields_at_a_time() {
        let mut known = empty_report();
        let first: Value = serde_json::from_str(
            r#"{"print":{"gcode_state":"RUNNING","nozzle_temper":205.5,"nozzle_target_temper":210,"bed_temper":59.9,"bed_target_temper":60,"mc_percent":42,"layer_num":10,"total_layer_num":100,"mc_remaining_time":33,"subtask_name":"cube"}}"#,
        )
        .unwrap();
        assert!(merge_report(&mut known, &first));
        assert_eq!(
            (known.gcode_state, known.percent, known.layer, known.layers),
            ("RUNNING", 42, 10, 100)
        );
        assert_eq!(
            (known.nozzle_c, known.bed_target_c, known.remaining_min),
            (205.5, 60.0, 33)
        );
        assert_eq!(known.subtask, "cube");
        let second: Value =
            serde_json::from_str(r#"{"print":{"mc_percent":43,"gcode_state":"SOMETHING NEW"}}"#)
                .unwrap();
        assert!(merge_report(&mut known, &second));
        assert_eq!(
            (known.percent, known.gcode_state, known.layers),
            (43, "UNKNOWN", 100)
        );
        // a document with no print object changes nothing
        let other: Value = serde_json::from_str(r#"{"info":{"command":"get_version"}}"#).unwrap();
        assert!(!merge_report(&mut known, &other));
        assert_eq!(known.percent, 43);
        // a percentage over 100 is held at 100
        let over: Value = serde_json::from_str(r#"{"print":{"mc_percent":900}}"#).unwrap();
        merge_report(&mut known, &over);
        assert_eq!(known.percent, 100);
    }

    #[test]
    fn the_commands_are_written_as_the_printer_reads_them() {
        assert_eq!(
            request_document(3, &BambuCommand::Pause).unwrap(),
            r#"{"print":{"sequence_id":"3","command":"pause"}}"#
        );
        assert!(request_document(4, &BambuCommand::Resume)
            .unwrap()
            .contains(r#""command":"resume""#));
        assert!(request_document(5, &BambuCommand::Stop)
            .unwrap()
            .contains(r#""command":"stop""#));
        assert_eq!(
            request_document(6, &BambuCommand::Gcode("M104 S200".into())).unwrap(),
            r#"{"print":{"sequence_id":"6","command":"gcode_line","param":"M104 S200\n"}}"#
        );
        for bad in ["", "G1 X1\nG1 X2", "G1\u{7}", &"G".repeat(201)] {
            assert!(
                request_document(1, &BambuCommand::Gcode(bad.into())).is_err(),
                "{bad:?}"
            );
        }
        assert!(
            request_document(1, &BambuCommand::StartFile("a.gcode".into()))
                .unwrap_err()
                .contains("FTPS")
        );
        assert_eq!(json_text("a\"b\\c\n\u{1}"), r#""a\"b\\c\n\u0001""#);
        assert_eq!(
            pushall_document(2),
            r#"{"pushing":{"sequence_id":"2","command":"pushall"}}"#
        );
    }

    /// What a broker in the test saw: the messages published to the request topic.
    type Seen = Arc<Mutex<Vec<String>>>;

    /// A broker of the test: it welcomes the access code `12345678`, takes one subscription and answers `pushall` with a report; it keeps the other messages it is given.
    fn broker(
        serial: &str,
        seen: Seen,
    ) -> impl Fn(&mut rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream>) + Send + 'static
    {
        let request = format!("device/{serial}/request");
        let report = format!("device/{serial}/report");
        move |tls| {
            let Ok(connect) = read_packet(tls) else {
                return;
            };
            let text = String::from_utf8_lossy(&connect.body).to_string();
            let ok = text.contains("bblp") && text.contains("12345678");
            let _ = tls.write_all(&[0x20, 0x02, 0x00, if ok { 0 } else { 5 }]);
            let _ = tls.flush();
            if !ok {
                return;
            }
            let Ok(sub) = read_packet(tls) else { return };
            let _ = tls.write_all(&[0x90, 0x03, sub.body[0], sub.body[1], 0x00]);
            let _ = tls.flush();
            while let Ok(p) = read_packet(tls) {
                if p.kind != 3 {
                    continue;
                }
                let Ok(m) = parse_publish(&p) else { return };
                if m.topic != request {
                    continue;
                }
                let doc = String::from_utf8_lossy(&m.payload).to_string();
                seen.locked().push(doc.clone());
                if doc.contains("pushall") {
                    let json = r#"{"print":{"gcode_state":"RUNNING","nozzle_temper":205.5,"nozzle_target_temper":210,"bed_temper":59.9,"bed_target_temper":60,"mc_percent":42,"layer_num":10,"total_layer_num":100,"mc_remaining_time":33,"subtask_name":"cube"}}"#;
                    let _ = tls.write_all(&publish_packet(&report, json.as_bytes()));
                    let _ = tls.flush();
                }
            }
        }
    }

    const SECOND: Duration = Duration::from_secs(5);

    #[test]
    fn a_pinned_printer_is_read_and_commanded_through_tls() {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let server = TestServer::start_with(1, broker("01P00A111111111", seen.clone()));
        let link = BambuTlsLink::new(
            "127.0.0.1",
            server.port,
            "01P00A111111111",
            Some("12345678"),
            Some(&server.fingerprint),
            SECOND,
        );
        let r = link.report().unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            (r.gcode_state, r.percent, r.layer, r.layers),
            ("RUNNING", 42, 10, 100)
        );
        assert_eq!(
            (r.nozzle_c, r.bed_c, r.subtask.as_str()),
            (205.5, 59.9, "cube")
        );
        link.send(&BambuCommand::Pause).unwrap();
        link.send(&BambuCommand::Gcode("M104 S0".into())).unwrap();
        // the second question does not ask for the whole state again, and the same line is used
        assert_eq!(link.report().unwrap().percent, 42);
        assert!(link.upload("a.gcode", b"G28").is_err());
        assert!(matches!(
            link.send(&BambuCommand::StartFile("a".into())),
            Err(LinkError::Refused(_))
        ));
        // the broker sees the messages in order
        let mut log = seen.locked().clone();
        for _ in 0..60 {
            if log.iter().any(|d| d.contains("M104 S0")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            log = seen.locked().clone();
        }
        assert!(log[0].contains("pushall"), "{log:?}");
        assert!(
            log.iter().any(|d| d.contains(r#""command":"pause""#)),
            "{log:?}"
        );
        assert!(log.iter().any(|d| d.contains("M104 S0")), "{log:?}");
        assert_eq!(
            log.iter().filter(|d| d.contains("pushall")).count(),
            1,
            "{log:?}"
        );
    }

    #[test]
    fn a_wrong_access_code_is_refused_and_a_certificate_that_is_not_pinned_gets_nothing() {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let server = TestServer::start_with(8, broker("01P00A111111111", seen.clone()));
        let wrong = BambuTlsLink::new(
            "127.0.0.1",
            server.port,
            "01P00A111111111",
            Some("00000000"),
            Some(&server.fingerprint),
            SECOND,
        );
        assert!(
            matches!(wrong.report(), Err(LinkError::Refused(_))),
            "the access code"
        );
        // with no pin the link only says what the printer showed, and nothing is asked
        let open = BambuTlsLink::new(
            "127.0.0.1",
            server.port,
            "01P00A111111111",
            Some("12345678"),
            None,
            SECOND,
        );
        assert_eq!(open.fingerprint().unwrap(), server.fingerprint);
        assert!(open.report().is_err());
        // another pin: the shown fingerprint is given back to be compared, and a report is refused
        let other = "b".repeat(64);
        let mismatched = BambuTlsLink::new(
            "127.0.0.1",
            server.port,
            "01P00A111111111",
            Some("12345678"),
            Some(&other),
            SECOND,
        );
        assert_eq!(mismatched.fingerprint().unwrap(), server.fingerprint);
        assert!(mismatched.send(&BambuCommand::Pause).is_err());
        assert!(seen.locked().is_empty());
        // a pin that is malformed is a refusal of trust
        let malformed = BambuTlsLink::new(
            "127.0.0.1",
            server.port,
            "01P00A111111111",
            None,
            Some("xyz"),
            SECOND,
        );
        assert!(matches!(
            malformed.fingerprint(),
            Err(LinkError::Refused(_))
        ));
    }

    #[test]
    fn a_printer_that_is_not_there_is_down() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let pin = "a".repeat(64);
        let link = BambuTlsLink::new(
            "127.0.0.1",
            port,
            "01P00A111111111",
            Some("12345678"),
            Some(&pin),
            Duration::from_millis(500),
        );
        assert!(matches!(link.report(), Err(LinkError::Down(_))));
        assert!(matches!(link.fingerprint(), Err(LinkError::Down(_))));
    }
}
