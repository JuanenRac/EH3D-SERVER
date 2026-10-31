//! Free G-code on the server: the text of a program is cleaned into the lines that are sent to the controller, read for what it asks of the spindle,
//! and, when it goes on after a stop, the codes that were in force are rebuilt from the lines that were already sent. Nothing here touches a machine.
//!
//! The lines that are sent are the code without its comments; blank lines and `%` are dropped. A line that is not G-code (`$` settings of GRBL, its
//! real-time characters, control characters) is refused: a job must not change the controller's settings or unlock it.

/// The most lines a program may carry.
pub const MAX_LINES: usize = 1_000_000;
/// The longest line, in characters: GRBL takes 80 or so, others take more.
pub const MAX_LINE_LEN: usize = 200;
/// The speed of a rapid move assumed for the estimate when the machine says no spindle (mm/min).
pub const DEFAULT_RAPID_MM_MIN: f64 = 3_000.0;
/// The feed assumed for a cutting move that has none yet, for the estimate (mm/min).
const FEED_WHEN_NONE: f64 = 1_000.0;

/// One line that is sent: the line of the text it comes from (from 0) and the code.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub raw: usize,
    pub text: String,
}

/// What a program is when it is ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// The lines to send: first the ones that rebuild the state (see `preamble`), then the program from the line `skip` on.
    pub entries: Vec<Entry>,
    /// How many of the first entries only rebuild the state of the controller after a stop.
    pub preamble: usize,
    /// How many lines the text has (counting blank and comment lines).
    pub raw_total: usize,
    /// The job goes on after a stop: it waits for the operator before the first move.
    pub resume: bool,
}

impl Program {
    /// The lines of the program itself that are sent (those after the preamble).
    pub fn work(&self) -> usize {
        self.entries.len() - self.preamble
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GcodeError {
    pub code: &'static str,
    pub detail: String,
}

fn fail<T>(code: &'static str, detail: impl Into<String>) -> Result<T, GcodeError> {
    Err(GcodeError {
        code,
        detail: detail.into(),
    })
}

/// The code of a line: comments in parentheses and after `;` removed.
pub fn strip(line: &str) -> String {
    let mut out = String::new();
    let mut in_comment = false;
    for c in line.chars() {
        match c {
            ';' if !in_comment => break,
            '(' if !in_comment => in_comment = true,
            ')' if in_comment => in_comment = false,
            _ if in_comment => {}
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

/// The words of a line of code: a letter and its number.
pub fn words(code: &str) -> Vec<(char, f64)> {
    let mut out = Vec::new();
    let mut cur: Option<(char, String)> = None;
    let mut flush = |cur: &mut Option<(char, String)>| {
        if let Some((l, n)) = cur.take() {
            if let Ok(v) = n.parse::<f64>() {
                out.push((l, v));
            }
        }
    };
    for c in code.chars() {
        if c.is_whitespace() {
            continue;
        }
        if c.is_ascii_alphabetic() {
            flush(&mut cur);
            cur = Some((c.to_ascii_uppercase(), String::new()));
        } else if let Some((_, n)) = cur.as_mut() {
            n.push(c);
        }
    }
    flush(&mut cur);
    out
}

/// A line that can be sent: refuses what is not G-code.
fn check_line(raw: usize, code: &str) -> Result<(), GcodeError> {
    if code.chars().any(|c| c.is_control() && c != '\t') {
        return fail(
            "bad_gcode",
            format!("line {}: control characters are not G-code", raw + 1),
        );
    }
    if code.starts_with(['$', '!', '~', '?']) {
        return fail(
            "bad_gcode",
            format!(
                "line {}: a settings or real-time command of the controller is not part of a job",
                raw + 1
            ),
        );
    }
    if code.chars().count() > MAX_LINE_LEN {
        return fail(
            "bad_gcode",
            format!("line {}: longer than {MAX_LINE_LEN} characters", raw + 1),
        );
    }
    Ok(())
}

/// The codes that stay on until another of their group changes them, as far as they can be given again after a stop.
#[derive(Debug, Default, Clone, PartialEq)]
struct Modal {
    units: Option<i32>,
    distance: Option<i32>,
    plane: Option<i32>,
    wcs: Option<i32>,
    feed_mode: Option<i32>,
    feed: Option<f64>,
    spindle: Option<i32>,
    speed: Option<f64>,
    coolant: Option<i32>,
}

impl Modal {
    fn apply(&mut self, code: &str) {
        for (letter, v) in words(code) {
            match letter {
                'G' => {
                    let g = (v * 10.0).round() as i32;
                    match g {
                        200 | 210 => self.units = Some(g),
                        900 | 910 => self.distance = Some(g),
                        170 | 180 | 190 => self.plane = Some(g),
                        540 | 550 | 560 | 570 | 580 | 590 => self.wcs = Some(g),
                        940 | 950 => self.feed_mode = Some(g),
                        _ => {}
                    }
                }
                'M' => match v.round() as i32 {
                    m @ (3..=5) => self.spindle = Some(m),
                    m @ (7..=9) => self.coolant = Some(m),
                    _ => {}
                },
                'F' => self.feed = Some(v),
                'S' => self.speed = Some(v),
                _ => {}
            }
        }
    }

    /// The lines that give the state back. The spindle and the coolant only when `outputs` says so: starting a spindle is not something to do by itself.
    fn lines(&self, outputs: bool) -> Vec<String> {
        let mut out = Vec::new();
        let groups: Vec<String> = [
            self.units,
            self.distance,
            self.plane,
            self.wcs,
            self.feed_mode,
        ]
        .iter()
        .flatten()
        .map(|g| {
            if g % 10 == 0 {
                format!("G{}", g / 10)
            } else {
                format!("G{}.{}", g / 10, g % 10)
            }
        })
        .collect();
        if !groups.is_empty() {
            out.push(groups.join(" "));
        }
        if let Some(f) = self.feed {
            out.push(format!("F{}", number(f)));
        }
        if outputs {
            match (self.spindle, self.speed) {
                (Some(m @ (3 | 4)), Some(s)) => out.push(format!("M{m} S{}", number(s))),
                (Some(m @ (3 | 4)), None) => out.push(format!("M{m}")),
                _ => {}
            }
            if let Some(m @ (7 | 8)) = self.coolant {
                out.push(format!("M{m}"));
            }
        }
        out
    }
}

/// A number as short as it can be written.
fn number(v: f64) -> String {
    let text = format!("{v:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Cleans the text into a program. `skip` is how many lines of the text (counting the blank and comment ones) were already sent: they are not sent
/// again; the codes they put in force are given again instead and the job waits for the operator before it moves.
pub fn parse(text: &str, skip: usize, restore_outputs: bool) -> Result<Program, GcodeError> {
    let raw: Vec<&str> = text.lines().collect();
    if raw.len() > MAX_LINES {
        return fail(
            "too_big",
            format!("a program may have {MAX_LINES} lines at the most"),
        );
    }
    if skip > raw.len() {
        return fail("no_work", "skip is beyond the end of the program");
    }
    let mut modal = Modal::default();
    let mut entries: Vec<Entry> = Vec::new();
    for (n, line) in raw.iter().enumerate() {
        let code = strip(line);
        if code.is_empty() || code == "%" {
            continue;
        }
        check_line(n, &code)?;
        if n < skip {
            modal.apply(&code);
        } else {
            entries.push(Entry { raw: n, text: code });
        }
    }
    if entries.is_empty() {
        return fail("no_work", "the program has no line of code to send");
    }
    let mut lead: Vec<Entry> = Vec::new();
    if skip > 0 {
        lead = modal
            .lines(restore_outputs)
            .into_iter()
            .map(|text| Entry { raw: skip, text })
            .collect();
    }
    let preamble = lead.len();
    lead.extend(entries);
    Ok(Program {
        entries: lead,
        preamble,
        raw_total: raw.len(),
        resume: skip > 0,
    })
}

/// What the program asks, read from the whole text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reading {
    /// The fastest the spindle is asked to turn (`S`).
    pub max_rpm: f64,
    /// The fastest cutting feed, in mm/min (`F`, converted when the program works in inches).
    pub max_feed_mm_min: f64,
    /// X and Y of the places the moves go to.
    pub points: Vec<(f64, f64)>,
    /// The length of all the moves and the time they take, in mm and seconds.
    pub travel_mm: f64,
    pub estimate_s: f64,
}

/// Reads a program: the straight moves (`G0`, `G1`; an arc counts as the straight line to its end), absolute or relative, in mm or inches.
pub fn read(lines: &[Entry], rapid_mm_min: f64) -> Reading {
    let mut r = Reading::default();
    let (mut pos, mut absolute, mut scale, mut motion) = ([0.0f64; 3], true, 1.0, 0i32);
    let mut feed = 0.0f64;
    for e in lines {
        let ws = words(&e.text);
        let mut target = [None; 3];
        let mut machine_coords = false;
        for &(l, v) in &ws {
            match l {
                'G' => match (v * 10.0).round() as i32 {
                    0 => motion = 0,
                    10 | 20 | 30 => motion = 1,
                    200 => scale = 25.4,
                    210 => scale = 1.0,
                    900 => absolute = true,
                    910 => absolute = false,
                    530 => machine_coords = true,
                    _ => {}
                },
                'F' => {
                    feed = v * scale;
                    r.max_feed_mm_min = r.max_feed_mm_min.max(feed);
                }
                'S' => r.max_rpm = r.max_rpm.max(v),
                'X' => target[0] = Some(v * scale),
                'Y' => target[1] = Some(v * scale),
                'Z' => target[2] = Some(v * scale),
                _ => {}
            }
        }
        if target.iter().all(Option::is_none) {
            continue;
        }
        let to: Vec<f64> = (0..3)
            .map(|i| match (target[i], absolute || machine_coords) {
                (Some(v), true) => v,
                (Some(v), false) => pos[i] + v,
                (None, _) => pos[i],
            })
            .collect();
        let d =
            ((to[0] - pos[0]).powi(2) + (to[1] - pos[1]).powi(2) + (to[2] - pos[2]).powi(2)).sqrt();
        r.travel_mm += d;
        let speed = if motion == 0 {
            rapid_mm_min
        } else if feed > 0.0 {
            feed
        } else {
            FEED_WHEN_NONE
        };
        if speed > 0.0 {
            r.estimate_s += d / speed * 60.0;
        }
        r.points.push((to[0], to[1]));
        pos = [to[0], to[1], to[2]];
    }
    r
}

/// The turns of every `S` word cut to `max` (for a machine whose spindle switch is on `warn`).
pub fn cut_speed(code: &str, max: f64) -> String {
    let mut out = String::new();
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.eq_ignore_ascii_case(&'S') {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.') {
                j += 1;
            }
            let digits: String = chars[i + 1..j].iter().collect();
            match digits.parse::<f64>() {
                Ok(v) if v > max => {
                    out.push('S');
                    out.push_str(&number(max));
                }
                _ => out.extend(&chars[i..j]),
            }
            i = j;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_blank_lines_and_the_percent_sign_are_not_sent() {
        let p = parse(
            "%\n(a program)\nG21 G90 ; units\n\nG1 X10 (here) Y5 F300\n%\nM30\n",
            0,
            false,
        )
        .unwrap();
        let t: Vec<&str> = p.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(t, ["G21 G90", "G1 X10  Y5 F300", "M30"]);
        // each line remembers the line of the text it came from
        assert_eq!(
            p.entries.iter().map(|e| e.raw).collect::<Vec<_>>(),
            [2, 4, 6]
        );
        assert_eq!((p.preamble, p.raw_total, p.resume), (0, 7, false));
        assert_eq!(p.work(), 3);
    }

    #[test]
    fn what_is_not_gcode_is_refused_with_its_line() {
        for bad in ["$100=250", "$X", "G0 X1\n!", "G0\u{7}X1", "~"] {
            let e = parse(bad, 0, false).unwrap_err();
            assert_eq!(e.code, "bad_gcode", "{bad}");
        }
        let e = parse("G0 X1\nG0 X2\n$H\n", 0, false).unwrap_err();
        assert!(e.detail.contains("line 3"), "{}", e.detail);
        let long = format!("G1 X1 {}", "(c)".repeat(100) + &"Y".repeat(MAX_LINE_LEN));
        assert_eq!(parse(&long, 0, false).unwrap_err().code, "bad_gcode");
        assert_eq!(
            parse("(only a comment)\n\n", 0, false).unwrap_err().code,
            "no_work"
        );
        assert_eq!(parse("G0 X1", 5, false).unwrap_err().code, "no_work");
    }

    #[test]
    fn a_program_that_goes_on_gets_its_state_back_and_waits() {
        let text = "G20 G91\nG17\nG55\nF30\nM3 S9000\nM8\nG1 X1\nG1 X2\nG1 X3\n";
        // the first six lines were sent
        let p = parse(text, 6, false).unwrap();
        assert!(p.resume);
        assert_eq!(p.preamble, 2);
        assert_eq!(p.entries[0].text, "G20 G91 G17 G55");
        assert_eq!(p.entries[1].text, "F30");
        assert_eq!(p.work(), 3);
        assert_eq!(p.entries[2].raw, 6);
        // the spindle and the coolant come back only when asked
        let q = parse(text, 6, true).unwrap();
        let t: Vec<&str> = q.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(&t[..4], ["G20 G91 G17 G55", "F30", "M3 S9000", "M8"]);
        assert_eq!(q.preamble, 4);
        // a program that ends in the middle of nothing has no preamble to give
        assert_eq!(parse("G1 X1\nG1 X2", 0, true).unwrap().preamble, 0);
    }

    #[test]
    fn a_later_code_of_a_group_replaces_the_earlier_one() {
        let p = parse("G90\nG91\nM3 S5000\nM5\nG1 X1", 4, true).unwrap();
        assert_eq!(p.entries[0].text, "G91");
        // the spindle was switched off again: nothing to start
        assert_eq!(p.preamble, 1);
    }

    #[test]
    fn the_spindle_and_the_feeds_are_read_in_millimetres() {
        let p = parse(
            "G21 G90\nM3 S12000\nG1 X10 Y0 F300\nG20\nG1 X1 Y1 F20\nG0 X0 Y0",
            0,
            false,
        )
        .unwrap();
        let r = read(&p.entries, 3_000.0);
        assert_eq!(r.max_rpm, 12_000.0);
        assert!(
            (r.max_feed_mm_min - 508.0).abs() < 1e-9,
            "{}",
            r.max_feed_mm_min
        );
        assert_eq!(r.points.len(), 3);
        assert_eq!(r.points[0], (10.0, 0.0));
        assert!(r.travel_mm > 10.0 && r.estimate_s > 0.0);
    }

    #[test]
    fn relative_moves_add_up() {
        let p = parse("G91\nG1 X5 F600\nG1 X5\nG1 Y5", 0, false).unwrap();
        let r = read(&p.entries, 3_000.0);
        assert_eq!(r.points, vec![(5.0, 0.0), (10.0, 0.0), (10.0, 5.0)]);
        assert!((r.travel_mm - 15.0).abs() < 1e-9);
        // 15 mm at 600 mm/min
        assert!((r.estimate_s - 1.5).abs() < 1e-9);
    }

    #[test]
    fn the_turns_above_the_top_are_cut_and_nothing_else_is_touched() {
        assert_eq!(cut_speed("M3 S30000", 24_000.0), "M3 S24000");
        assert_eq!(cut_speed("M3 S12000", 24_000.0), "M3 S12000");
        assert_eq!(cut_speed("G1 X5 F300", 24_000.0), "G1 X5 F300");
        assert_eq!(cut_speed("S99999.5 M3", 24_000.0), "S24000 M3");
    }
}
