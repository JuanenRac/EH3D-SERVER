//! A Marlin printer on a serial line, as far as the server needs it: the lines of the protocol (the checksum of a numbered line, the reading of the answers to `M105` and
//! `M27`) and a firmware in memory that answers the way Marlin does, so the driver (`src/printer_lan.rs`) is tried without a printer. Nothing here touches a serial port: the
//! simulated line is the only one the server serves, and nothing has run on a real Marlin printer.
//!
//! The firmware in memory knows the commands a host needs to run a print from the card of the printer: `M105` (temperatures), `M104`/`M109` and `M140`/`M190` (targets, with
//! the hottest the firmware allows), `M28`/`M29` (write a file to the card), `M23` (select it), `M24` (start or go on), `M25` (hold), `M524` (abort), `M27` (progress), `M112`
//! (emergency stop, after which the firmware answers an error to everything until `M999`) and `M110` (line number). Every other word is acknowledged and ignored.

use std::collections::HashMap;

/// The lowest temperature the room has in the simulation, in degrees.
const AMBIENT_C: f64 = 22.0;

/// The checksum of a line of the protocol: the exclusive or of its bytes.
pub fn checksum(text: &str) -> u8 {
    text.bytes().fold(0u8, |acc, b| acc ^ b)
}

/// A line the way it goes on the wire when the host numbers it: `N<n> <line>*<checksum>`.
pub fn frame(n: u32, line: &str) -> String {
    let body = format!("N{n} {line}");
    format!("{body}*{}", checksum(&body))
}

/// The number and the text of a line that came numbered (`N<n> <line>*<checksum>`), after the checksum was checked; a line with no number gives `None` for it. A wrong
/// checksum is an error, as it is for the firmware, which answers `Error:checksum mismatch` and asks for the line again.
pub fn unframe(line: &str) -> Result<(Option<u32>, String), String> {
    let line = line.trim();
    let Some(star) = line.rfind('*') else {
        return Ok((None, line.to_string()));
    };
    let (body, sum) = (&line[..star], &line[star + 1..]);
    let want: u8 = sum
        .trim()
        .parse()
        .map_err(|_| "the checksum is not a number".to_string())?;
    if checksum(body) != want {
        return Err("checksum mismatch".to_string());
    }
    let rest = body
        .strip_prefix('N')
        .ok_or("a checked line starts with N")?;
    let (digits, text) = rest.split_once(' ').unwrap_or((rest, ""));
    let n: u32 = digits
        .parse()
        .map_err(|_| "the line number is not a number".to_string())?;
    Ok((Some(n), text.to_string()))
}

/// What the firmware says the temperatures are.
#[derive(Debug, Clone, PartialEq)]
pub struct Temps {
    pub hotend_c: f64,
    pub hotend_target_c: f64,
    pub bed_c: f64,
    pub bed_target_c: f64,
}

fn pair(rest: &str) -> Option<(f64, f64)> {
    // "200.00 /210.00 ..." : the reading, then the target after a slash
    let mut parts = rest.split_whitespace();
    let current: f64 = parts.next()?.parse().ok()?;
    let target: f64 = parts.next()?.strip_prefix('/')?.parse().ok()?;
    // a reading that is not a number (`nan`, `inf`) is not a temperature: every comparison with it is false, so a limit would never trip
    (current.is_finite() && target.is_finite()).then_some((current, target))
}

/// Reads the answer to `M105`: `ok T:200.00 /200.00 B:60.00 /60.00 @:127 B@:0`. A line that has no hotend and no bed in it is not an answer.
pub fn parse_temps(line: &str) -> Option<Temps> {
    let text = line.trim();
    let text = text.strip_prefix("ok").map_or(text, str::trim_start);
    let mut hotend = None;
    let mut bed = None;
    // the fields are `T:`, `B:` and `@:`; each reading is followed by its target
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if let Some(r) = rest.strip_prefix("T:") {
            hotend = hotend.or_else(|| pair(r));
        } else if let Some(r) = rest.strip_prefix("B:") {
            bed = bed.or_else(|| pair(r));
        }
        match rest.find(' ') {
            Some(space) => at += space + 1,
            None => break,
        }
    }
    let (hotend, bed) = (hotend?, bed?);
    Some(Temps {
        hotend_c: hotend.0,
        hotend_target_c: hotend.1,
        bed_c: bed.0,
        bed_target_c: bed.1,
    })
}

/// What `M27` says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SdStatus {
    NotPrinting,
    /// `SD printing byte <done>/<total>`
    Printing {
        done: u64,
        total: u64,
    },
}

/// Reads the answer to `M27`.
pub fn parse_sd_status(line: &str) -> Option<SdStatus> {
    let text = line.trim();
    if text.starts_with("Not SD printing") {
        return Some(SdStatus::NotPrinting);
    }
    let rest = text.strip_prefix("SD printing byte ")?;
    let (done, total) = rest.split_once('/')?;
    Some(SdStatus::Printing {
        done: done.trim().parse().ok()?,
        total: total.trim().parse().ok()?,
    })
}

/// The line without its comment (everything from the first `;`) and without the blanks at the ends.
pub fn strip_comment(line: &str) -> &str {
    line.split(';').next().unwrap_or("").trim()
}

/// The value after a letter in a line (`S` in `M104 S200`).
fn word(line: &str, letter: char) -> Option<f64> {
    line.split_whitespace().find_map(|w| {
        let mut chars = w.chars();
        if chars.next()?.eq_ignore_ascii_case(&letter) {
            chars.as_str().parse().ok()
        } else {
            None
        }
    })
}

/// The firmware of a printer, in memory.
#[derive(Debug)]
pub struct MarlinSim {
    hotend: f64,
    hotend_target: f64,
    bed: f64,
    bed_target: f64,
    /// The hottest target the firmware takes (its MAXTEMP minus the margin).
    max_hotend_c: f64,
    max_bed_c: f64,
    /// The files on the card: the size of each line, in bytes.
    files: HashMap<String, Vec<u64>>,
    writing: Option<(String, Vec<u64>)>,
    selected: Option<String>,
    /// The lines of the selected file that were printed.
    line_done: usize,
    printing: bool,
    held: bool,
    halted: bool,
    line_s: f64,
    /// The seconds the print has gone on for in the line being printed.
    carry_s: f64,
    last_n: u32,
    /// `none`, `thermal_runaway` (the hotend heats whatever its target is) or `sensor_lost` (the hotend reads -1).
    fault: &'static str,
}

impl MarlinSim {
    pub fn new(max_hotend_c: f64, max_bed_c: f64, line_s: f64) -> MarlinSim {
        MarlinSim {
            hotend: AMBIENT_C,
            hotend_target: 0.0,
            bed: AMBIENT_C,
            bed_target: 0.0,
            max_hotend_c,
            max_bed_c,
            files: HashMap::new(),
            writing: None,
            selected: None,
            line_done: 0,
            printing: false,
            held: false,
            halted: false,
            line_s,
            carry_s: 0.0,
            last_n: 0,
            fault: "none",
        }
    }

    /// Makes the simulated firmware fail the way `fault` says; a word it does not know clears the fault.
    pub fn set_fault(&mut self, fault: &str) {
        self.fault = match fault {
            "thermal_runaway" => "thermal_runaway",
            "sensor_lost" => "sensor_lost",
            _ => "none",
        };
    }

    pub fn halted(&self) -> bool {
        self.halted
    }

    /// Lets `dt` seconds go by: the heaters move to their targets and a print goes on by the lines it can do.
    pub fn advance(&mut self, dt: f64) {
        if self.halted || !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.hotend = approach(self.hotend, self.hotend_target, 4.0, 0.6, dt);
        self.bed = approach(self.bed, self.bed_target, 1.2, 0.2, dt);
        if self.fault == "thermal_runaway" {
            self.hotend = (self.hotend + 4.0 * dt).min(450.0);
        }
        if self.printing && !self.held {
            let total = self.selected_lines();
            if self.line_s <= 0.0 {
                self.line_done = total;
            } else {
                self.carry_s += dt;
                let lines = (self.carry_s / self.line_s) as usize;
                self.carry_s -= lines as f64 * self.line_s;
                self.line_done = self.line_done.saturating_add(lines).min(total);
            }
            if self.line_done >= total {
                self.printing = false;
                self.carry_s = 0.0;
            }
        }
    }

    fn selected_lines(&self) -> usize {
        self.selected
            .as_ref()
            .and_then(|n| self.files.get(n))
            .map_or(0, Vec::len)
    }

    fn bytes(&self) -> (u64, u64) {
        let Some(sizes) = self.selected.as_ref().and_then(|n| self.files.get(n)) else {
            return (0, 0);
        };
        let done: u64 = sizes.iter().take(self.line_done).sum();
        (done, sizes.iter().sum())
    }

    /// Answers one line the host sends: the lines the firmware sends back, the last of which is `ok` unless the line was refused.
    pub fn handle(&mut self, raw: &str) -> Vec<String> {
        let (n, text) = match unframe(raw) {
            Ok(v) => v,
            Err(why) => {
                return vec![
                    format!("Error:{why}"),
                    format!("Resend: {}", self.last_n.saturating_add(1)),
                    "ok".into(),
                ]
            }
        };
        if let Some(n) = n {
            self.last_n = n;
        }
        let line = strip_comment(&text).to_string();
        if line.is_empty() {
            return vec!["ok".into()];
        }
        // while a file is being written to the card, every line but M29 is the file
        if let Some((_, sizes)) = self.writing.as_mut() {
            if !line.eq_ignore_ascii_case("M29") {
                sizes.push(line.len() as u64 + 1);
                return vec!["ok".into()];
            }
        }
        let code = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if self.halted && code != "M999" {
            return vec!["Error:Printer halted. kill() called!".into()];
        }
        match code.as_str() {
            "M105" => vec![format!(
                "ok T:{:.2} /{:.2} B:{:.2} /{:.2} @:0 B@:0",
                if self.fault == "sensor_lost" {
                    -1.0
                } else {
                    self.hotend
                },
                self.hotend_target,
                self.bed,
                self.bed_target
            )],
            "M104" | "M109" => self.set_target(&line, true),
            "M140" | "M190" => self.set_target(&line, false),
            "M110" => {
                self.last_n = word(&line, 'N').unwrap_or(0.0) as u32;
                vec!["ok".into()]
            }
            "M28" => {
                let name = line.split_whitespace().nth(1).unwrap_or("").to_string();
                if name.is_empty() {
                    return vec!["Error:M28 needs a file name".into(), "ok".into()];
                }
                self.writing = Some((name.clone(), Vec::new()));
                vec![format!("Writing to file: {name}"), "ok".into()]
            }
            "M29" => match self.writing.take() {
                Some((name, sizes)) => {
                    self.files.insert(name, sizes);
                    vec!["Done saving file.".into(), "ok".into()]
                }
                None => vec!["ok".into()],
            },
            "M23" => {
                let name = line.split_whitespace().nth(1).unwrap_or("").to_string();
                match self.files.get(&name) {
                    Some(sizes) => {
                        let size: u64 = sizes.iter().sum();
                        self.selected = Some(name.clone());
                        self.line_done = 0;
                        self.printing = false;
                        self.held = false;
                        vec![
                            format!("File opened: {name} Size: {size}"),
                            "File selected".into(),
                            "ok".into(),
                        ]
                    }
                    None => vec![format!("open failed, File: {name}."), "ok".into()],
                }
            }
            "M24" => {
                if self.selected.is_some() {
                    self.printing = true;
                    self.held = false;
                }
                vec!["ok".into()]
            }
            "M25" => {
                if self.printing {
                    self.held = true;
                }
                vec!["ok".into()]
            }
            "M524" => {
                self.printing = false;
                self.held = false;
                self.line_done = 0;
                self.carry_s = 0.0;
                vec!["ok".into()]
            }
            "M27" => {
                if self.selected.is_some() && (self.printing || self.held) {
                    let (done, total) = self.bytes();
                    vec![format!("SD printing byte {done}/{total}"), "ok".into()]
                } else {
                    vec!["Not SD printing".into(), "ok".into()]
                }
            }
            "M112" => {
                self.halted = true;
                self.printing = false;
                self.held = false;
                self.hotend_target = 0.0;
                self.bed_target = 0.0;
                vec!["ok".into()]
            }
            "M999" => {
                self.halted = false;
                vec!["ok".into()]
            }
            _ => vec!["ok".into()],
        }
    }

    fn set_target(&mut self, line: &str, hotend: bool) -> Vec<String> {
        let target = word(line, 'S').unwrap_or(0.0);
        let limit = if hotend {
            self.max_hotend_c
        } else {
            self.max_bed_c
        };
        if !(0.0..=limit).contains(&target) {
            return vec![
                format!("Error:{} S{target} is out of 0 to {limit}", &line[..4]),
                "ok".into(),
            ];
        }
        if hotend {
            self.hotend_target = target;
        } else {
            self.bed_target = target;
        }
        vec!["ok".into()]
    }
}

/// A temperature that goes to its target at `rate` degrees a second, or cools to the room at `cool` a second.
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

    fn send(sim: &mut MarlinSim, line: &str) -> Vec<String> {
        sim.handle(line)
    }

    #[test]
    fn a_numbered_line_carries_a_checksum_that_is_checked() {
        // the example of the documentation of the protocol: N123 M110 N123*... is the xor of the bytes
        let framed = frame(5, "G1 X10");
        assert!(framed.starts_with("N5 G1 X10*"));
        assert_eq!(unframe(&framed), Ok((Some(5), "G1 X10".to_string())));
        assert_eq!(unframe("M105"), Ok((None, "M105".to_string())));
        let bad = framed.replace("X10", "X11");
        assert_eq!(unframe(&bad), Err("checksum mismatch".to_string()));
        assert!(unframe("N5 G1*abc").is_err());
        assert_eq!(checksum(""), 0);
        assert_eq!(checksum("ab"), b'a' ^ b'b');
    }

    #[test]
    fn the_temperature_report_and_the_card_status_are_read() {
        let t = parse_temps("ok T:200.00 /210.00 B:60.00 /65.00 @:127 B@:0").unwrap();
        assert_eq!(
            t,
            Temps {
                hotend_c: 200.0,
                hotend_target_c: 210.0,
                bed_c: 60.0,
                bed_target_c: 65.0
            }
        );
        assert!(parse_temps("T:21.5 /0.0 B:22.0 /0.0").is_some());
        assert!(parse_temps("ok").is_none());
        assert!(parse_temps("ok T:20.0 /0.0").is_none());
        assert_eq!(
            parse_sd_status("SD printing byte 120/480"),
            Some(SdStatus::Printing {
                done: 120,
                total: 480
            })
        );
        assert_eq!(
            parse_sd_status("Not SD printing"),
            Some(SdStatus::NotPrinting)
        );
        assert_eq!(parse_sd_status("ok"), None);
        assert_eq!(strip_comment("G1 X1 ; move"), "G1 X1");
        assert_eq!(strip_comment("; only a comment"), "");
    }

    #[test]
    fn the_heaters_go_to_their_targets_inside_the_limits_of_the_firmware() {
        let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
        assert_eq!(send(&mut sim, "M104 S200"), vec!["ok".to_string()]);
        assert_eq!(send(&mut sim, "M140 S60"), vec!["ok".to_string()]);
        let too_hot = send(&mut sim, "M104 S300");
        assert!(too_hot[0].starts_with("Error:M104"), "{too_hot:?}");
        sim.advance(100.0);
        let t = parse_temps(&send(&mut sim, "M105")[0]).unwrap();
        assert_eq!((t.hotend_c, t.hotend_target_c), (200.0, 200.0));
        assert_eq!((t.bed_c, t.bed_target_c), (60.0, 60.0));
        send(&mut sim, "M104 S0");
        sim.advance(1000.0);
        let t = parse_temps(&send(&mut sim, "M105")[0]).unwrap();
        assert_eq!(t.hotend_c, AMBIENT_C);
    }

    #[test]
    fn a_file_written_to_the_card_prints_holds_and_aborts() {
        let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
        send(&mut sim, "M28 part.gco");
        for line in ["G28", "G1 X10 F600", "G1 X20", "M104 S0"] {
            assert_eq!(send(&mut sim, line), vec!["ok".to_string()]);
        }
        assert_eq!(send(&mut sim, "M29")[0], "Done saving file.");
        assert_eq!(send(&mut sim, "M27")[0], "Not SD printing");
        assert!(send(&mut sim, "M23 nothing.gco")[0].starts_with("open failed"));
        assert_eq!(send(&mut sim, "M23 part.gco")[1], "File selected");
        send(&mut sim, "M24");
        sim.advance(2.0);
        let status = parse_sd_status(&send(&mut sim, "M27")[0]).unwrap();
        assert_eq!(
            status,
            SdStatus::Printing {
                done: 4 + 12,
                total: 4 + 12 + 7 + 8
            }
        );
        send(&mut sim, "M25");
        sim.advance(50.0);
        assert!(matches!(
            parse_sd_status(&send(&mut sim, "M27")[0]),
            Some(SdStatus::Printing { .. })
        ));
        send(&mut sim, "M24");
        sim.advance(50.0);
        assert_eq!(send(&mut sim, "M27")[0], "Not SD printing");
        // an abort goes back to the start
        send(&mut sim, "M23 part.gco");
        send(&mut sim, "M24");
        sim.advance(1.0);
        send(&mut sim, "M524");
        assert_eq!(send(&mut sim, "M27")[0], "Not SD printing");
    }

    #[test]
    fn the_emergency_stop_halts_the_firmware_until_it_is_reset() {
        let mut sim = MarlinSim::new(275.0, 120.0, 0.0);
        send(&mut sim, "M104 S200");
        assert_eq!(send(&mut sim, "M112"), vec!["ok".to_string()]);
        assert!(sim.halted());
        assert!(send(&mut sim, "M105")[0].starts_with("Error:Printer halted"));
        assert!(send(&mut sim, "M104 S100")[0].starts_with("Error:Printer halted"));
        assert_eq!(send(&mut sim, "M999"), vec!["ok".to_string()]);
        assert!(!sim.halted());
        let t = parse_temps(&send(&mut sim, "M105")[0]).unwrap();
        assert_eq!(t.hotend_target_c, 0.0);
    }

    #[test]
    fn the_simulated_faults_show_in_the_readings() {
        let mut sim = MarlinSim::new(275.0, 120.0, 0.0);
        sim.set_fault("sensor_lost");
        let t = parse_temps(&send(&mut sim, "M105")[0]).unwrap();
        assert_eq!(t.hotend_c, -1.0);
        sim.set_fault("thermal_runaway");
        sim.advance(30.0);
        let t = parse_temps(&send(&mut sim, "M105")[0]).unwrap();
        assert!(t.hotend_c > 100.0 && t.hotend_target_c == 0.0, "{t:?}");
        sim.set_fault("none");
        sim.advance(1000.0);
        let t = parse_temps(&send(&mut sim, "M105")[0]).unwrap();
        assert_eq!(t.hotend_c, AMBIENT_C);
    }

    #[test]
    fn a_line_with_a_wrong_checksum_is_asked_again() {
        let mut sim = MarlinSim::new(275.0, 120.0, 0.0);
        send(&mut sim, &frame(1, "M110 N0"));
        let bad = frame(1, "M105").replace("M105", "M106");
        let answer = send(&mut sim, &bad);
        assert_eq!(answer[0], "Error:checksum mismatch");
        assert_eq!(answer[1], "Resend: 1");
        let good = send(&mut sim, &frame(1, "M105"));
        assert!(good[0].starts_with("ok T:"));
    }
}
