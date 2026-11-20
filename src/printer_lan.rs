//! The drivers of two more kinds of 3D printer, both **simulated only** for now: a Marlin printer on a serial line (`marlin`) and a Bambu Lab printer in LAN mode
//! (`bambu_lan`, an experiment). Each driver speaks the protocol of its printer through a small trait (`SerialLink`, `BambuLink`) and the link the server serves is a
//! printer in memory (`src/marlin.rs`, `src/bambu.rs`) and, for Bambu only and behind the switch `bambu_lan_enabled`, the TLS link of `src/bambu_mqtt.rs`; the drivers are tried against the printers in memory and nothing has run on a real Marlin, A1, P1S or other printer.
//!
//! The Bambu driver never takes a certificate it did not check: it asks the link for the fingerprint of the certificate the printer shows and compares it with the one pinned in
//! the settings of the machine (`connection.fingerprint`) before anything else is sent; with none pinned, or another, the connection is refused and the refusal says what the
//! printer showed (`src/bambu.rs` explains the chain of trust).

use std::sync::Mutex;
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::bambu::{trust_check, BambuCommand, BambuReport, BambuSim, TrustError};
use crate::marlin::{parse_sd_status, parse_temps, strip_comment, MarlinSim, SdStatus};
use crate::printer_drivers::{
    DriverError, DriverResult, Heater, PrinterDriver, PrinterJob, PrinterState,
};
use crate::sync::Locked;

/// The seconds the simulated clocks go on by for a call, when the rate is "no waiting".
const NO_WAITING_S: f64 = 1.0e6;

fn seconds(since: &mut Instant, rate: f64) -> f64 {
    let real = since.elapsed().as_secs_f64();
    *since = Instant::now();
    if rate <= 0.0 {
        NO_WAITING_S
    } else {
        real * rate
    }
}

/// A file name the card of a printer takes: letters, digits, `_`, `-` and `.`, up to 32 characters.
fn card_name(name: &str) -> Result<String, DriverError> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !name.starts_with('.');
    if ok {
        Ok(name.to_string())
    } else {
        Err(DriverError::Refused(
            "the name of the file is up to 32 letters, digits, _, - or . (and does not start with a dot)".into(),
        ))
    }
}

/// The layer marks of a program as the number of bytes of the lines that come before each one, counted the way the card counts them (a line without its comment and its
/// blanks, plus a line break; a line with nothing left is not kept), and the lines that are kept.
fn layer_marks(text: &str) -> (Vec<u64>, Vec<String>) {
    let (mut marks, mut lines, mut bytes) = (Vec::new(), Vec::new(), 0u64);
    for raw in text.lines() {
        let t = raw.trim();
        if t.starts_with(";LAYER_CHANGE") || t.starts_with(";LAYER:") {
            marks.push(bytes);
        }
        let kept = strip_comment(raw);
        if !kept.is_empty() {
            bytes += kept.len() as u64 + 1;
            lines.push(kept.to_string());
        }
    }
    (marks, lines)
}

// ---- Marlin on a serial line ----------------------------------------------------------------------------------------------------------

/// The way to a Marlin printer: one line of the protocol and the lines it answers.
pub trait SerialLink: Send + Sync {
    fn exchange(&self, line: &str) -> Result<Vec<String>, String>;
    /// Makes a simulated printer fail the way `fault` says; a real line ignores it.
    fn simulate(&self, _fault: &str) {}
}

/// The simulated serial line: a firmware in memory whose clock goes on by the time that passed, times the rate.
pub struct SimSerial {
    inner: Mutex<(MarlinSim, Instant, bool)>,
    rate: f64,
}

impl SimSerial {
    pub fn new(max_hotend_c: f64, max_bed_c: f64, line_s: f64, rate: f64) -> SimSerial {
        SimSerial {
            inner: Mutex::new((
                MarlinSim::new(max_hotend_c, max_bed_c, line_s),
                Instant::now(),
                false,
            )),
            rate,
        }
    }
}

impl SerialLink for SimSerial {
    fn exchange(&self, line: &str) -> Result<Vec<String>, String> {
        let mut g = self.inner.locked();
        let dt = seconds(&mut g.1, self.rate);
        if g.2 {
            return Err("the simulated serial line does not answer".into());
        }
        g.0.advance(dt);
        Ok(g.0.handle(line))
    }

    fn simulate(&self, fault: &str) {
        let mut g = self.inner.locked();
        g.2 = fault == "disconnect";
        g.0.set_fault(fault);
    }
}

#[derive(Default)]
struct MarlinTrack {
    active: bool,
    held: bool,
    finished: bool,
    name: String,
    started: Option<Instant>,
    marks: Vec<u64>,
}

/// A Marlin printer behind a serial line.
pub struct MarlinDriver<L: SerialLink> {
    link: L,
    since: Instant,
    track: Mutex<MarlinTrack>,
}

impl<L: SerialLink> MarlinDriver<L> {
    pub fn new(link: L) -> MarlinDriver<L> {
        MarlinDriver {
            link,
            since: Instant::now(),
            track: Mutex::new(MarlinTrack::default()),
        }
    }

    fn ask(&self, line: &str) -> DriverResult<Vec<String>> {
        let answer = self
            .link
            .exchange(line)
            .map_err(DriverError::Disconnected)?;
        if let Some(error) = answer.iter().find(|l| l.starts_with("Error:")) {
            return Err(DriverError::Refused(error.clone()));
        }
        Ok(answer)
    }
}

impl<L: SerialLink> PrinterDriver for MarlinDriver<L> {
    fn state(&self) -> DriverResult<PrinterState> {
        let t_s = self.since.elapsed().as_secs_f64();
        let temps = match self.ask("M105") {
            Ok(answer) => answer.iter().find_map(|l| parse_temps(l)).ok_or_else(|| {
                DriverError::Refused("the answer to M105 has no temperature".into())
            })?,
            Err(DriverError::Refused(text)) if text.starts_with("Error:Printer halted") => {
                return Ok(PrinterState {
                    state: "emergency",
                    raw: text,
                    heaters: Vec::new(),
                    job: PrinterJob::default(),
                    t_s,
                });
            }
            Err(e) => return Err(e),
        };
        let sd = self
            .ask("M27")?
            .iter()
            .find_map(|l| parse_sd_status(l))
            .unwrap_or(SdStatus::NotPrinting);
        let mut tr = self.track.locked();
        let mut job = PrinterJob::default();
        let mut state = "idle";
        let mut raw = "idle".to_string();
        if tr.active {
            match sd {
                SdStatus::Printing { done, total } => {
                    state = if tr.held { "paused" } else { "printing" };
                    raw = "SD printing".into();
                    let fraction = if total > 0 {
                        done as f64 / total as f64
                    } else {
                        0.0
                    };
                    job.progress_pct = (fraction * 1000.0).round() / 10.0;
                    if !tr.marks.is_empty() {
                        job.layers = Some(tr.marks.len() as u64);
                        job.layer = Some(tr.marks.iter().filter(|m| **m <= done).count() as u64);
                    }
                }
                SdStatus::NotPrinting => {
                    tr.active = false;
                    tr.held = false;
                    tr.finished = true;
                }
            }
        }
        if tr.finished && !tr.active {
            state = "finished";
            raw = "Done printing file".into();
            job.progress_pct = 100.0;
            if !tr.marks.is_empty() {
                job.layers = Some(tr.marks.len() as u64);
                job.layer = Some(tr.marks.len() as u64);
            }
        }
        if state != "idle" {
            job.name = tr.name.clone();
            job.elapsed_s = tr.started.map_or(0.0, |s| s.elapsed().as_secs_f64());
        }
        Ok(PrinterState {
            state,
            raw,
            heaters: vec![
                Heater {
                    name: "hotend",
                    current_c: temps.hotend_c,
                    target_c: temps.hotend_target_c,
                    power_pct: 0.0,
                },
                Heater {
                    name: "bed",
                    current_c: temps.bed_c,
                    target_c: temps.bed_target_c,
                    power_pct: 0.0,
                },
            ],
            job,
            t_s,
        })
    }

    fn start_print(&self, name: &str, gcode: &str) -> DriverResult<()> {
        let file = card_name(name)?;
        if self.track.locked().active {
            return Err(DriverError::Refused("the printer is busy".into()));
        }
        let (marks, lines) = layer_marks(gcode);
        self.ask(&format!("M28 {file}"))?;
        for line in &lines {
            self.ask(line)?;
        }
        self.ask("M29")?;
        self.ask(&format!("M23 {file}"))?;
        self.ask("M24")?;
        *self.track.locked() = MarlinTrack {
            active: true,
            name: file,
            started: Some(Instant::now()),
            marks,
            ..MarlinTrack::default()
        };
        Ok(())
    }

    fn pause(&self) -> DriverResult<()> {
        let (active, held) = {
            let tr = self.track.locked();
            (tr.active, tr.held)
        };
        if !active || held {
            return Err(DriverError::Refused("the printer is not printing".into()));
        }
        self.ask("M25")?;
        self.track.locked().held = true;
        Ok(())
    }

    fn resume(&self) -> DriverResult<()> {
        if !self.track.locked().held {
            return Err(DriverError::Refused("the printer is not held".into()));
        }
        self.ask("M24")?;
        self.track.locked().held = false;
        Ok(())
    }

    fn cancel(&self) -> DriverResult<()> {
        let result = self.ask("M524");
        *self.track.locked() = MarlinTrack::default();
        result.map(|_| ())
    }

    fn set_target(&self, heater: &str, target_c: f64) -> DriverResult<()> {
        let word = match heater {
            "hotend" => "M104",
            "bed" => "M140",
            _ => {
                return Err(DriverError::Refused(
                    "the printer has no such heater".into(),
                ))
            }
        };
        self.ask(&format!("{word} S{target_c}")).map(|_| ())
    }

    fn emergency_stop(&self) -> DriverResult<()> {
        // the firmware halts: the next M105 answers the error, which is how the state says `emergency`
        let result = self
            .link
            .exchange("M112")
            .map_err(DriverError::Disconnected);
        *self.track.locked() = MarlinTrack::default();
        result.map(|_| ())
    }

    fn gcode(&self, line: &str) -> DriverResult<()> {
        let line = strip_comment(line);
        if line.is_empty() {
            return Ok(());
        }
        self.ask(line).map(|_| ())
    }

    fn reset(&self) -> DriverResult<()> {
        *self.track.locked() = MarlinTrack::default();
        self.link
            .exchange("M999")
            .map(|_| ())
            .map_err(DriverError::Disconnected)
    }

    fn simulate(&self, fault: &str) {
        self.link.simulate(fault);
    }
}

// ---- Bambu Lab in LAN mode ------------------------------------------------------------------------------------------------------------

/// Why the link to a Bambu printer did not do what it was asked.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkError {
    /// The printer does not answer.
    Down(String),
    /// The printer answered with a refusal.
    Refused(String),
}

impl From<LinkError> for DriverError {
    fn from(e: LinkError) -> DriverError {
        match e {
            LinkError::Down(t) => DriverError::Disconnected(t),
            LinkError::Refused(t) => DriverError::Refused(t),
        }
    }
}

/// The way to a Bambu printer: the certificate it shows, its report, its commands and the files given to it.
pub trait BambuLink: Send + Sync {
    /// The SHA-256 fingerprint of the certificate the printer shows (hexadecimal).
    fn fingerprint(&self) -> Result<String, LinkError>;
    fn report(&self) -> Result<BambuReport, LinkError>;
    fn send(&self, command: &BambuCommand) -> Result<(), LinkError>;
    fn upload(&self, name: &str, bytes: &[u8]) -> Result<(), LinkError>;
    fn simulate(&self, _fault: &str) {}
}

/// The fingerprint of the certificate a simulated printer shows: it depends on its serial, so two simulated printers show two certificates.
pub fn sim_fingerprint(serial: &str) -> String {
    hex::encode(Sha256::digest(
        format!("eh3d-simulated-bambu-certificate:{serial}").as_bytes(),
    ))
}

/// The simulated link: a printer in memory.
pub struct SimBambu {
    inner: Mutex<(BambuSim, Instant)>,
    rate: f64,
}

impl SimBambu {
    pub fn new(
        serial: &str,
        max_nozzle_c: f64,
        max_bed_c: f64,
        layer_s: f64,
        rate: f64,
    ) -> SimBambu {
        SimBambu {
            inner: Mutex::new((
                BambuSim::new(
                    serial,
                    &sim_fingerprint(serial),
                    max_nozzle_c,
                    max_bed_c,
                    layer_s,
                ),
                Instant::now(),
            )),
            rate,
        }
    }

    fn tick(&self) -> std::sync::MutexGuard<'_, (BambuSim, Instant)> {
        let mut g = self.inner.locked();
        let dt = seconds(&mut g.1, self.rate);
        g.0.advance(dt);
        g
    }
}

fn down() -> LinkError {
    LinkError::Down("the simulated printer does not answer".into())
}

impl BambuLink for SimBambu {
    fn fingerprint(&self) -> Result<String, LinkError> {
        let g = self.tick();
        if g.0.disconnected() {
            return Err(down());
        }
        Ok(g.0.certificate_fingerprint().to_string())
    }

    fn report(&self) -> Result<BambuReport, LinkError> {
        let g = self.tick();
        if g.0.disconnected() {
            return Err(down());
        }
        Ok(g.0.report())
    }

    fn send(&self, command: &BambuCommand) -> Result<(), LinkError> {
        let mut g = self.tick();
        if g.0.disconnected() {
            return Err(down());
        }
        g.0.handle(command).map_err(LinkError::Refused)
    }

    fn upload(&self, name: &str, bytes: &[u8]) -> Result<(), LinkError> {
        let mut g = self.tick();
        if g.0.disconnected() {
            return Err(down());
        }
        g.0.upload(name, bytes).map_err(LinkError::Refused)
    }

    fn simulate(&self, fault: &str) {
        self.inner
            .locked()
            .0
            .set_disconnected(fault == "disconnect");
    }
}

/// Why a Bambu driver could not be made.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectError {
    /// The certificate was not trusted.
    Untrusted(TrustError),
    /// The printer did not answer.
    Down(String),
}

#[derive(Default)]
struct BambuTrack {
    cancelled: bool,
    started: Option<Instant>,
    /// A fault of the simulation that the link cannot make (`thermal_runaway`, `sensor_lost`).
    fault: String,
}

/// A Bambu Lab printer in LAN mode.
pub struct BambuDriver<L: BambuLink> {
    link: L,
    since: Instant,
    track: Mutex<BambuTrack>,
}

impl<L: BambuLink> BambuDriver<L> {
    /// Opens the driver after checking the certificate of the printer against the fingerprint that was pinned. Nothing else is sent before the check passes.
    pub fn connect(link: L, pinned: Option<&str>) -> Result<BambuDriver<L>, ConnectError> {
        let shown = link.fingerprint().map_err(|e| match e {
            LinkError::Down(t) | LinkError::Refused(t) => ConnectError::Down(t),
        })?;
        trust_check(pinned, &shown).map_err(ConnectError::Untrusted)?;
        Ok(BambuDriver {
            link,
            since: Instant::now(),
            track: Mutex::new(BambuTrack::default()),
        })
    }

    fn gcode_line(&self, text: String) -> DriverResult<()> {
        self.link.send(&BambuCommand::Gcode(text))?;
        Ok(())
    }
}

impl<L: BambuLink> PrinterDriver for BambuDriver<L> {
    fn state(&self) -> DriverResult<PrinterState> {
        let r = self.link.report()?;
        let tr = self.track.locked();
        let state = match r.gcode_state {
            "IDLE" => "idle",
            "PREPARE" => "preparing",
            "RUNNING" => "printing",
            "PAUSE" => "paused",
            "FINISH" => "finished",
            "FAILED" if tr.cancelled => "cancelled",
            "FAILED" => "failed",
            _ => "error",
        };
        let hotend = match tr.fault.as_str() {
            "sensor_lost" => -1.0,
            "thermal_runaway" => r.nozzle_target_c + 500.0,
            _ => r.nozzle_c,
        };
        let mut job = PrinterJob::default();
        if !matches!(state, "idle") {
            job.name = r.subtask.clone();
            job.progress_pct = f64::from(r.percent);
            job.elapsed_s = tr.started.map_or(0.0, |s| s.elapsed().as_secs_f64());
            if r.layers > 0 {
                job.layers = Some(r.layers);
                job.layer = Some(r.layer);
            }
        }
        Ok(PrinterState {
            state,
            raw: r.gcode_state.to_string(),
            heaters: vec![
                Heater {
                    name: "hotend",
                    current_c: hotend,
                    target_c: r.nozzle_target_c,
                    power_pct: 0.0,
                },
                Heater {
                    name: "bed",
                    current_c: r.bed_c,
                    target_c: r.bed_target_c,
                    power_pct: 0.0,
                },
            ],
            job,
            t_s: self.since.elapsed().as_secs_f64(),
        })
    }

    fn start_print(&self, name: &str, gcode: &str) -> DriverResult<()> {
        let file = format!("{}.gcode", card_name(name)?);
        let report = self.link.report()?;
        if matches!(report.gcode_state, "RUNNING" | "PREPARE" | "PAUSE") {
            return Err(DriverError::Refused("the printer is busy".into()));
        }
        self.link.upload(&file, gcode.as_bytes())?;
        self.link.send(&BambuCommand::StartFile(file))?;
        *self.track.locked() = BambuTrack {
            started: Some(Instant::now()),
            ..BambuTrack::default()
        };
        Ok(())
    }

    fn pause(&self) -> DriverResult<()> {
        self.link.send(&BambuCommand::Pause)?;
        Ok(())
    }

    fn resume(&self) -> DriverResult<()> {
        self.link.send(&BambuCommand::Resume)?;
        Ok(())
    }

    fn cancel(&self) -> DriverResult<()> {
        self.track.locked().cancelled = true;
        self.link.send(&BambuCommand::Stop)?;
        Ok(())
    }

    fn set_target(&self, heater: &str, target_c: f64) -> DriverResult<()> {
        let word = match heater {
            "hotend" => "M104",
            "bed" => "M140",
            _ => {
                return Err(DriverError::Refused(
                    "the printer has no such heater".into(),
                ))
            }
        };
        self.gcode_line(format!("{word} S{target_c}"))
    }

    fn emergency_stop(&self) -> DriverResult<()> {
        // the LAN interface has no emergency stop of its own: the print is stopped and the heaters are switched off
        self.track.locked().cancelled = true;
        let stopped = self.link.send(&BambuCommand::Stop);
        let hotend = self.gcode_line("M104 S0".into());
        let bed = self.gcode_line("M140 S0".into());
        stopped.map_err(DriverError::from)?;
        hotend?;
        bed
    }

    fn gcode(&self, line: &str) -> DriverResult<()> {
        let line = strip_comment(line);
        if line.is_empty() {
            return Ok(());
        }
        self.gcode_line(line.to_string())
    }

    fn reset(&self) -> DriverResult<()> {
        self.track.locked().cancelled = false;
        Ok(())
    }

    fn simulate(&self, fault: &str) {
        self.track.locked().fault = match fault {
            "thermal_runaway" | "sensor_lost" => fault.to_string(),
            _ => String::new(),
        };
        self.link.simulate(fault);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAM: &str = "; header\nM104 S200\n;LAYER_CHANGE\nG1 X1 E1\n\n;LAYER_CHANGE\nG1 X2 E1\n;LAYER_CHANGE\nG1 X3 E1\n";

    fn marlin(rate: f64, line_s: f64) -> MarlinDriver<SimSerial> {
        MarlinDriver::new(SimSerial::new(275.0, 120.0, line_s, rate))
    }

    fn bambu(pinned: Option<&str>) -> Result<BambuDriver<SimBambu>, ConnectError> {
        BambuDriver::connect(
            SimBambu::new("01P00A111111111", 300.0, 100.0, 0.0, 0.0),
            pinned,
        )
    }

    #[test]
    fn the_layers_are_counted_in_the_bytes_the_card_counts() {
        let (marks, lines) = layer_marks(PROGRAM);
        assert_eq!(lines, vec!["M104 S200", "G1 X1 E1", "G1 X2 E1", "G1 X3 E1"]);
        let first = "M104 S200".len() as u64 + 1;
        let second = first + "G1 X1 E1".len() as u64 + 1;
        assert_eq!(
            marks,
            vec![first, second, second + "G1 X2 E1".len() as u64 + 1]
        );
        assert!(card_name("part_1.gco").is_ok());
        for bad in ["", ".hidden", "a b", "a/b", &"x".repeat(33)] {
            assert!(card_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_marlin_printer_prints_a_program_from_its_card_to_the_end() {
        let d = marlin(0.0, 0.01);
        let s = d.state().unwrap();
        assert_eq!((s.state, s.heaters.len()), ("idle", 2));
        d.set_target("hotend", 200.0).unwrap();
        assert!(
            d.set_target("hotend", 300.0).is_err(),
            "the firmware refuses more than it allows"
        );
        assert!(d.set_target("chamber", 40.0).is_err());
        d.start_print("part.gco", PROGRAM).unwrap();
        assert!(d.start_print("again.gco", PROGRAM).is_err());
        // the print ran at once on the clock that does not wait
        let s = d.state().unwrap();
        assert_eq!(s.state, "finished");
        assert_eq!(
            (s.job.progress_pct, s.job.layer, s.job.layers),
            (100.0, Some(3), Some(3))
        );
        assert_eq!(s.job.name, "part.gco");
        assert!(d.start_print("bad name", PROGRAM).is_err());
    }

    #[test]
    fn a_marlin_print_is_held_gone_on_and_cancelled() {
        // a real clock: the lines take time, so the print is in the middle when it is asked
        let d = marlin(1.0, 3600.0);
        d.start_print("p.gco", PROGRAM).unwrap();
        let s = d.state().unwrap();
        assert_eq!(s.state, "printing");
        assert_eq!(s.job.layers, Some(3));
        d.pause().unwrap();
        assert_eq!(d.state().unwrap().state, "paused");
        assert!(d.pause().is_err());
        d.resume().unwrap();
        assert_eq!(d.state().unwrap().state, "printing");
        assert!(d.resume().is_err());
        d.cancel().unwrap();
        assert_eq!(d.state().unwrap().state, "idle");
        assert!(d.pause().is_err());
    }

    #[test]
    fn a_marlin_emergency_halts_the_firmware_until_a_reset_and_a_dead_line_is_a_disconnect() {
        let d = marlin(0.0, 0.0);
        d.emergency_stop().unwrap();
        let s = d.state().unwrap();
        assert_eq!(s.state, "emergency");
        assert!(s.raw.starts_with("Error:Printer halted"));
        d.reset().unwrap();
        assert_eq!(d.state().unwrap().state, "idle");
        d.simulate("sensor_lost");
        assert_eq!(d.state().unwrap().heater("hotend").unwrap().current_c, -1.0);
        d.simulate("disconnect");
        assert!(matches!(d.state(), Err(DriverError::Disconnected(_))));
        d.simulate("none");
        assert_eq!(d.state().unwrap().state, "idle");
        d.gcode("M104 S150 ; warm").unwrap();
        assert!(d.gcode("M104 S500").is_err());
        d.gcode("; nothing").unwrap();
    }

    #[test]
    fn a_bambu_printer_is_never_trusted_without_its_pinned_certificate() {
        let shown = sim_fingerprint("01P00A111111111");
        assert_ne!(shown, sim_fingerprint("01P00A222222222"));
        assert!(bambu(Some(&shown)).is_ok());
        assert!(bambu(Some(&shown.to_uppercase())).is_ok());
        match bambu(None) {
            Err(ConnectError::Untrusted(TrustError::NotPinned { seen })) => assert_eq!(seen, shown),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("a printer with no pinned certificate was trusted"),
        }
        let other = sim_fingerprint("other");
        assert!(matches!(
            bambu(Some(&other)),
            Err(ConnectError::Untrusted(TrustError::Mismatch { .. }))
        ));
        assert!(matches!(
            bambu(Some("short")),
            Err(ConnectError::Untrusted(TrustError::Malformed))
        ));
    }

    #[test]
    fn a_bambu_print_runs_pauses_and_is_cancelled_through_the_normalized_state() {
        let shown = sim_fingerprint("01P00A111111111");
        let d = bambu(Some(&shown)).ok().unwrap();
        let s = d.state().unwrap();
        assert_eq!(s.state, "idle");
        d.set_target("hotend", 215.0).unwrap();
        assert!(d.set_target("hotend", 320.0).is_err());
        assert!(d.set_target("chamber", 40.0).is_err());
        d.start_print("part", PROGRAM).unwrap();
        // the clock that does not wait: heated and printed at once
        let s = d.state().unwrap();
        assert_eq!(s.state, "finished");
        assert_eq!(
            (s.job.progress_pct, s.job.layer, s.job.layers),
            (100.0, Some(3), Some(3))
        );
        assert_eq!(s.job.name, "part.gcode");
        assert!(s.heater("hotend").is_some() && s.heater("bed").is_some());
        // a real clock: held, gone on, cancelled
        let slow = BambuDriver::connect(
            SimBambu::new("01P00A111111111", 300.0, 100.0, 3600.0, 1.0),
            Some(&shown),
        )
        .ok()
        .unwrap();
        slow.set_target("hotend", 0.0).unwrap();
        slow.start_print("part", PROGRAM).unwrap();
        assert_eq!(slow.state().unwrap().state, "printing");
        assert!(slow.start_print("again", PROGRAM).is_err());
        slow.pause().unwrap();
        assert_eq!(slow.state().unwrap().state, "paused");
        slow.resume().unwrap();
        assert!(slow.resume().is_err());
        slow.cancel().unwrap();
        assert_eq!(slow.state().unwrap().state, "cancelled");
        slow.reset().unwrap();
        assert_eq!(slow.state().unwrap().state, "failed");
    }

    #[test]
    fn a_bambu_emergency_stops_and_cools_and_the_faults_of_the_simulation_show() {
        let shown = sim_fingerprint("01P00A111111111");
        let d = bambu(Some(&shown)).ok().unwrap();
        d.set_target("bed", 60.0).unwrap();
        d.emergency_stop().unwrap();
        assert_eq!(d.state().unwrap().heater("bed").unwrap().target_c, 0.0);
        d.simulate("sensor_lost");
        assert_eq!(d.state().unwrap().heater("hotend").unwrap().current_c, -1.0);
        d.simulate("thermal_runaway");
        assert!(d.state().unwrap().heater("hotend").unwrap().current_c > 300.0);
        d.simulate("disconnect");
        assert!(matches!(d.state(), Err(DriverError::Disconnected(_))));
        assert!(matches!(d.pause(), Err(DriverError::Disconnected(_))));
        d.simulate("none");
        assert!(d.state().is_ok());
        d.gcode("; only a comment").unwrap();
    }
}
