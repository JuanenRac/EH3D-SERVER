//! A Bambu Lab printer in LAN mode, as far as the server needs it: what the printer reports over MQTT (the temperatures, the layer, the progress), the commands the server
//! sends it (hold, go on, cancel, a line of G-code, start a file it was given over FTPS), the chain of trust of the connection and a printer in memory that does all of that so
//! the driver (`src/printer_lan.rs`) is tried without one. This is an EXPERIMENT: the connection to a real printer (MQTT over TLS on port 8883, with the access code
//! of the printer) is `src/bambu_mqtt.rs`, behind the switch `bambu_lan_enabled`, and the file transfer over FTPS is not served; the line the server opens by default is the simulated
//! one, and nothing has run on a real A1, P1S or other Bambu printer.
//!
//! **The chain of trust.** A Bambu printer shows a certificate it signed itself, so there is no authority to ask. The server never accepts a certificate it did not check:
//! the connection names the SHA-256 fingerprint of the certificate it expects (`connection.fingerprint`), and a connection whose printer shows another one, or one with no
//! fingerprint set, is refused. The refusal of a connection with no fingerprint says the fingerprint the printer showed, so the person who owns the printer can compare it
//! with what the screen of the printer says and pin it; the server does not pin it by itself.

use std::collections::HashMap;

/// The lowest temperature the room has in the simulation, in degrees.
const AMBIENT_C: f64 = 22.0;

/// Why a connection was not trusted.
#[derive(Debug, Clone, PartialEq)]
pub enum TrustError {
    /// No fingerprint was set: the printer showed `seen`, which the person has to check and pin.
    NotPinned { seen: String },
    /// The printer shows another certificate than the one that was pinned.
    Mismatch { pinned: String, seen: String },
    /// The fingerprint that was set is not 64 hexadecimal digits.
    Malformed,
}

impl TrustError {
    pub fn text(&self) -> String {
        match self {
            TrustError::NotPinned { seen } => format!(
                "no fingerprint is pinned for this printer, and a certificate is never accepted unchecked: the printer shows {seen}; compare it with the one the printer gives \
                 and set it as connection.fingerprint"
            ),
            TrustError::Mismatch { pinned, seen } => format!(
                "the certificate of the printer is not the pinned one (pinned {pinned}, shown {seen}); the connection is refused"
            ),
            TrustError::Malformed => {
                "connection.fingerprint is the SHA-256 of the certificate: 64 hexadecimal digits, with or without colons".into()
            }
        }
    }
}

/// A fingerprint as it is compared: 64 hexadecimal digits in lower case (colons and blanks between the pairs are taken out). `None` when it is not one.
pub fn normalize_fingerprint(text: &str) -> Option<String> {
    let clean: String = text
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    (clean.len() == 64 && clean.bytes().all(|b| b.is_ascii_hexdigit())).then_some(clean)
}

/// Whether the certificate the printer showed (`presented`, already a fingerprint) is the one that was pinned. Nothing is accepted without a pin.
pub fn trust_check(pinned: Option<&str>, presented: &str) -> Result<(), TrustError> {
    let seen = normalize_fingerprint(presented).unwrap_or_else(|| presented.to_string());
    let Some(pinned) = pinned else {
        return Err(TrustError::NotPinned { seen });
    };
    let pinned = normalize_fingerprint(pinned).ok_or(TrustError::Malformed)?;
    // the same length is known (64), and the whole text is compared whatever the first difference
    let same = pinned.len() == seen.len()
        && pinned
            .bytes()
            .zip(seen.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if same {
        Ok(())
    } else {
        Err(TrustError::Mismatch { pinned, seen })
    }
}

/// What the printer reports (the part of its `print` report that the server reads).
#[derive(Debug, Clone, PartialEq)]
pub struct BambuReport {
    /// `IDLE`, `PREPARE`, `RUNNING`, `PAUSE`, `FINISH` or `FAILED`.
    pub gcode_state: &'static str,
    pub nozzle_c: f64,
    pub nozzle_target_c: f64,
    pub bed_c: f64,
    pub bed_target_c: f64,
    pub percent: u32,
    pub layer: u64,
    pub layers: u64,
    pub remaining_min: u64,
    pub subtask: String,
}

/// What the server sends the printer.
#[derive(Debug, Clone, PartialEq)]
pub enum BambuCommand {
    Pause,
    Resume,
    Stop,
    /// One line of G-code (`gcode_line`).
    Gcode(String),
    /// Starts a file that was uploaded (`project_file`).
    StartFile(String),
}

/// A Bambu printer in memory.
#[derive(Debug)]
pub struct BambuSim {
    serial: String,
    fingerprint: String,
    nozzle: f64,
    nozzle_target: f64,
    bed: f64,
    bed_target: f64,
    max_nozzle_c: f64,
    max_bed_c: f64,
    /// The files given over FTPS and the layers each one has.
    files: HashMap<String, u64>,
    state: &'static str,
    subtask: String,
    layers: u64,
    layer_s: f64,
    ran_s: f64,
    disconnected: bool,
    cancelled: bool,
}

impl BambuSim {
    pub fn new(
        serial: &str,
        fingerprint: &str,
        max_nozzle_c: f64,
        max_bed_c: f64,
        layer_s: f64,
    ) -> BambuSim {
        BambuSim {
            serial: serial.to_string(),
            fingerprint: fingerprint.to_string(),
            nozzle: AMBIENT_C,
            nozzle_target: 0.0,
            bed: AMBIENT_C,
            bed_target: 0.0,
            max_nozzle_c,
            max_bed_c,
            files: HashMap::new(),
            state: "IDLE",
            subtask: String::new(),
            layers: 0,
            layer_s,
            ran_s: 0.0,
            disconnected: false,
            cancelled: false,
        }
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// The fingerprint of the certificate the printer shows.
    pub fn certificate_fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn set_disconnected(&mut self, yes: bool) {
        self.disconnected = yes;
    }

    pub fn disconnected(&self) -> bool {
        self.disconnected
    }

    /// Whether the last end of a print was a cancel that the server asked for.
    pub fn was_cancelled(&self) -> bool {
        self.cancelled
    }

    /// The seconds go by: the heaters move to their targets and a print goes through its layers.
    pub fn advance(&mut self, dt: f64) {
        if self.disconnected || dt <= 0.0 {
            return;
        }
        self.nozzle = approach(self.nozzle, self.nozzle_target, 4.0, 0.6, dt);
        self.bed = approach(self.bed, self.bed_target, 1.2, 0.2, dt);
        match self.state {
            "PREPARE" => {
                // the printer heats to what the file asks, then it prints
                // a heater with no target is not waited for
                let ready = |now: f64, target: f64| target <= 0.0 || (now - target).abs() < 2.0;
                if ready(self.nozzle, self.nozzle_target) && ready(self.bed, self.bed_target) {
                    self.state = "RUNNING";
                    if self.layer_s <= 0.0 {
                        self.state = "FINISH";
                        self.nozzle_target = 0.0;
                        self.bed_target = 0.0;
                    }
                }
            }
            "RUNNING" => {
                self.ran_s += dt;
                if self.layer_s <= 0.0 || self.ran_s >= self.layers as f64 * self.layer_s {
                    self.ran_s = self.layers as f64 * self.layer_s;
                    self.state = "FINISH";
                    self.nozzle_target = 0.0;
                    self.bed_target = 0.0;
                }
            }
            _ => {}
        }
    }

    /// Takes a file over FTPS: the layers it has are counted from the `;LAYER` marks of a G-code file (a file with none is one layer for each 100 lines, at least one).
    pub fn upload(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if self.disconnected {
            return Err("the printer does not answer".into());
        }
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err("the name of the file has no folders".into());
        }
        let text = String::from_utf8_lossy(bytes);
        let marks = text
            .lines()
            .filter(|l| {
                let l = l.trim();
                l.starts_with(";LAYER_CHANGE") || l.starts_with(";LAYER:")
            })
            .count() as u64;
        let layers = if marks > 0 {
            marks
        } else {
            (text.lines().count() as u64 / 100).max(1)
        };
        self.files.insert(name.to_string(), layers);
        Ok(())
    }

    /// Does what the server asked.
    pub fn handle(&mut self, command: &BambuCommand) -> Result<(), String> {
        if self.disconnected {
            return Err("the printer does not answer".into());
        }
        match command {
            BambuCommand::Pause => {
                if self.state == "RUNNING" || self.state == "PREPARE" {
                    self.state = "PAUSE";
                    Ok(())
                } else {
                    Err("the printer is not printing".into())
                }
            }
            BambuCommand::Resume => {
                if self.state == "PAUSE" {
                    self.state = "RUNNING";
                    Ok(())
                } else {
                    Err("the printer is not held".into())
                }
            }
            BambuCommand::Stop => {
                if matches!(self.state, "RUNNING" | "PREPARE" | "PAUSE") {
                    self.state = "FAILED";
                    self.cancelled = true;
                }
                self.nozzle_target = 0.0;
                self.bed_target = 0.0;
                Ok(())
            }
            BambuCommand::Gcode(line) => self.gcode_line(line),
            BambuCommand::StartFile(name) => {
                if matches!(self.state, "RUNNING" | "PREPARE" | "PAUSE") {
                    return Err("the printer is busy".into());
                }
                let layers = *self
                    .files
                    .get(name)
                    .ok_or("the file was not given to the printer")?;
                self.layers = layers;
                self.subtask = name.clone();
                self.ran_s = 0.0;
                self.cancelled = false;
                self.state = "PREPARE";
                Ok(())
            }
        }
    }

    fn gcode_line(&mut self, line: &str) -> Result<(), String> {
        let text = line.split(';').next().unwrap_or("").trim();
        let mut words = text.split_whitespace();
        let Some(code) = words.next() else {
            return Ok(());
        };
        let s: Option<f64> = words
            .find_map(|w| w.strip_prefix('S').or_else(|| w.strip_prefix('s')))
            .and_then(|v| v.parse().ok());
        let (hotend, limit) = match code.to_ascii_uppercase().as_str() {
            "M104" | "M109" => (true, self.max_nozzle_c),
            "M140" | "M190" => (false, self.max_bed_c),
            _ => return Ok(()),
        };
        let target = s.unwrap_or(0.0);
        if !(0.0..=limit).contains(&target) {
            return Err(format!("{code} S{target} is out of 0 to {limit}"));
        }
        if hotend {
            self.nozzle_target = target;
        } else {
            self.bed_target = target;
        }
        Ok(())
    }

    /// What the printer reports now.
    pub fn report(&self) -> BambuReport {
        let (percent, layer, remaining_min) =
            if self.layers > 0 && matches!(self.state, "RUNNING" | "PAUSE" | "FINISH" | "FAILED") {
                let total = self.layers as f64 * self.layer_s;
                let fraction = if total > 0.0 {
                    (self.ran_s / total).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                (
                    (fraction * 100.0).floor() as u32,
                    ((fraction * self.layers as f64).ceil() as u64).min(self.layers),
                    (((total - self.ran_s).max(0.0)) / 60.0).ceil() as u64,
                )
            } else {
                (0, 0, 0)
            };
        BambuReport {
            gcode_state: self.state,
            nozzle_c: round1(self.nozzle),
            nozzle_target_c: self.nozzle_target,
            bed_c: round1(self.bed),
            bed_target_c: self.bed_target,
            percent,
            layer,
            layers: self.layers,
            remaining_min,
            subtask: self.subtask.clone(),
        }
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn approach(now: f64, target: f64, rate: f64, cool: f64, dt: f64) -> f64 {
    if target > 0.0 {
        if now < target {
            (now + rate * dt).min(target)
        } else {
            (now - cool * dt).max(target)
        }
    } else {
        (now - cool * dt).max(AMBIENT_C)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRINT: &str =
        "M104 S200\n;LAYER_CHANGE\nG1 X1\n;LAYER_CHANGE\nG1 X2\n;LAYER_CHANGE\nG1 X3\n";

    fn fp(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn sim() -> BambuSim {
        BambuSim::new("01P00A000000000", &fp('a'), 300.0, 100.0, 10.0)
    }

    #[test]
    fn a_fingerprint_is_read_with_or_without_colons_and_nothing_else() {
        let colons: Vec<String> = (0..32).map(|_| "AB".to_string()).collect();
        assert_eq!(
            normalize_fingerprint(&colons.join(":")),
            Some("ab".repeat(32))
        );
        assert_eq!(
            normalize_fingerprint(&"ab".repeat(32)),
            Some("ab".repeat(32))
        );
        assert_eq!(normalize_fingerprint("ab"), None);
        assert_eq!(normalize_fingerprint(&"zz".repeat(32)), None);
        assert_eq!(normalize_fingerprint(&"ab".repeat(33)), None);
    }

    #[test]
    fn a_certificate_is_trusted_only_when_it_is_the_pinned_one() {
        let shown = fp('a');
        assert_eq!(trust_check(Some(&shown), &shown), Ok(()));
        assert_eq!(trust_check(Some(&fp('A')), &shown), Ok(()));
        match trust_check(None, &shown) {
            Err(TrustError::NotPinned { seen }) => assert_eq!(seen, shown),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            trust_check(Some(&fp('b')), &shown),
            Err(TrustError::Mismatch { .. })
        ));
        assert_eq!(
            trust_check(Some("nonsense"), &shown),
            Err(TrustError::Malformed)
        );
        // the refusal without a pin says what to compare, and never that the connection went on
        let text = trust_check(None, &shown).unwrap_err().text();
        assert!(text.contains(&shown) && text.contains("never accepted"));
        assert!(trust_check(Some(&fp('b')), &shown)
            .unwrap_err()
            .text()
            .contains("refused"));
    }

    #[test]
    fn a_file_is_given_started_and_printed_layer_by_layer() {
        let mut p = sim();
        assert_eq!(p.report().gcode_state, "IDLE");
        assert!(p
            .handle(&BambuCommand::StartFile("a.gcode".into()))
            .is_err());
        p.upload("a.gcode", PRINT.as_bytes()).unwrap();
        assert!(p.upload("../x.gcode", b"").is_err());
        p.handle(&BambuCommand::Gcode("M104 S200".into())).unwrap();
        p.handle(&BambuCommand::Gcode("M140 S60".into())).unwrap();
        p.handle(&BambuCommand::StartFile("a.gcode".into()))
            .unwrap();
        assert_eq!(p.report().gcode_state, "PREPARE");
        p.advance(100.0);
        let r = p.report();
        assert_eq!(r.gcode_state, "RUNNING");
        assert_eq!((r.nozzle_c, r.bed_c), (200.0, 60.0));
        assert_eq!((r.layers, r.layer, r.percent), (3, 0, 0));
        p.advance(15.0);
        let r = p.report();
        assert_eq!((r.layer, r.percent, r.subtask.as_str()), (2, 50, "a.gcode"));
        p.advance(100.0);
        let r = p.report();
        assert_eq!((r.gcode_state, r.percent, r.layer), ("FINISH", 100, 3));
        assert_eq!(r.nozzle_target_c, 0.0);
    }

    #[test]
    fn a_print_is_held_gone_on_and_cancelled() {
        let mut p = sim();
        p.upload("a.gcode", PRINT.as_bytes()).unwrap();
        assert!(p.handle(&BambuCommand::Pause).is_err());
        assert!(p.handle(&BambuCommand::Resume).is_err());
        p.handle(&BambuCommand::StartFile("a.gcode".into()))
            .unwrap();
        assert!(p
            .handle(&BambuCommand::StartFile("a.gcode".into()))
            .is_err());
        p.advance(1.0);
        p.handle(&BambuCommand::Pause).unwrap();
        assert_eq!(p.report().gcode_state, "PAUSE");
        let held = p.report();
        p.advance(50.0);
        assert_eq!(p.report().percent, held.percent);
        p.handle(&BambuCommand::Resume).unwrap();
        assert_eq!(p.report().gcode_state, "RUNNING");
        p.handle(&BambuCommand::Stop).unwrap();
        assert_eq!(p.report().gcode_state, "FAILED");
        assert!(p.was_cancelled());
        assert_eq!(p.report().nozzle_target_c, 0.0);
    }

    #[test]
    fn a_target_over_what_the_printer_takes_is_refused_and_a_dead_line_answers_nothing() {
        let mut p = sim();
        assert!(p.handle(&BambuCommand::Gcode("M104 S320".into())).is_err());
        assert!(p.handle(&BambuCommand::Gcode("M140 S101".into())).is_err());
        assert!(p.handle(&BambuCommand::Gcode("G1 X10".into())).is_ok());
        assert_eq!(p.report().nozzle_target_c, 0.0);
        p.set_disconnected(true);
        assert!(p.disconnected());
        assert!(p.handle(&BambuCommand::Pause).is_err());
        assert!(p.upload("a.gcode", b"").is_err());
        p.set_disconnected(false);
        assert!(p.upload("a.gcode", b"G1 X1\n").is_ok());
        assert_eq!(p.serial(), "01P00A000000000");
        assert_eq!(p.certificate_fingerprint(), fp('a'));
    }
}
