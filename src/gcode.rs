//! Free G-code on the server: the text of a program is cleaned into the lines that are sent to the controller, read for what it asks of the spindle,
//! and, when it goes on after a stop, the codes that were in force and the place the program had reached are rebuilt from the lines before it, with
//! the lines that take the machine back there. Nothing here touches a machine.
//!
//! The lines that are sent are the code without its comments; blank lines and `%` are dropped. A line that is not G-code (`$` settings of GRBL, its
//! real-time characters, control characters, characters outside ASCII) is refused wherever in the line it stands: a job must not change the
//! controller's settings or unlock it. A machine whose administrator switched `allow_dollar` on takes the `$` lines of GRBL (`$H`, `$X`, `$G`, `$J=...`,
//! settings), except `$RST`, which wipes the controller's memory; the real-time characters are refused always.
//!
//! What the controller would read in another way than the server did is refused too, with the line and the reason: a line the reader of the suite
//! cannot read whole (its diagnostic is part of the reason), a comment that is not closed, a carriage return inside a line, code after `M2`/`M30`.
//!
//! A program for a 3D printer is not for GRBL: [`parse_printer`] splits it, and `printer3d` judges what it asks.

use eh3d_parser_gcode::{codes, Dialect, Severity};

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
    /// The entry after which the spindle is given its time to reach its speed.
    pub spindle_wait_after: Option<usize>,
    /// The entry of the sequence of a job that goes on that switches the spindle or the laser on again (`M3`, `M4`).
    pub power_on_at: Option<usize>,
    /// The most the spindle of the machine may be asked to turn (a machine whose spindle switch is on `warn`).
    pub speed_limit: Option<f64>,
}

/// Where a program was at the line it goes on from, read from the lines before it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResumePoint {
    /// X, Y and Z in the work coordinates in force, in mm; `None` for an axis the program had not moved.
    pub at_mm: [Option<f64>; 3],
    /// The last of `M3`, `M4`, `M5` and the last `S`.
    pub spindle: Option<i32>,
    pub speed: Option<f64>,
    /// The last of `M7`, `M8`, `M9`.
    pub coolant: Option<i32>,
    /// `G17`, `G18`, `G19` and `G54` to `G59`, as numbers.
    pub plane: Option<i32>,
    pub wcs: Option<i32>,
    /// The program works in inches (`G20`).
    pub inches: bool,
    /// `G94` or `G95` (the feed for each turn), as a number.
    pub feed_mode: Option<i32>,
    /// The last `F`, as the program wrote it.
    pub feed: Option<f64>,
}

/// What the server adds of its own to the sequence of a job that goes on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResumeMoves {
    /// The height, in the work coordinates and in mm, the tool goes up to before the spindle starts and before it crosses.
    pub safe_z_mm: f64,
    /// The most the tool goes down at, in mm/min, back to where the program had it.
    pub plunge_feed_mm_min: f64,
    /// The machine carries a laser: it is switched on once the head is back over the point, and there is no speed to wait for.
    pub laser: bool,
}

impl Default for ResumeMoves {
    fn default() -> Self {
        ResumeMoves {
            safe_z_mm: 5.0,
            plunge_feed_mm_min: 100.0,
            laser: false,
        }
    }
}

/// Where the program was at line `skip`, or why the server does not know it: then the job is not resumed, because the tool could not be taken back
/// to its place.
pub fn resume_point(text: &str, skip: usize) -> Result<ResumePoint, String> {
    const AXIS: [char; 3] = ['X', 'Y', 'Z'];
    let mut point = ResumePoint::default();
    let (mut relative, mut scale, mut wcs) = (false, 1.0, 540);
    // for each axis: the line that left it where the program does not say
    let mut lost: [Option<usize>; 3] = [None; 3];
    let mut shifted: Option<usize> = None;
    let (mut tool_length, mut cycle): (Option<usize>, Option<usize>) = (None, None);
    for (n, line) in text.lines().take(skip).enumerate() {
        let code = strip(line.trim_start_matches('\u{feff}'));
        let number_of_line = n + 1;
        if code.starts_with('$') {
            // a homing cycle or a jog moves the machine in a way the words of the program do not follow
            let rest = &dollar_command(&code)[1..];
            for (axis, letter) in AXIS.iter().enumerate() {
                if rest.starts_with('H') || (rest.starts_with("J=") && rest.contains(*letter)) {
                    lost[axis] = Some(number_of_line);
                }
            }
            continue;
        }
        let ws = words(&code);
        let named = AXIS.map(|a| ws.iter().find(|(l, _)| *l == a).map(|&(_, v)| v));
        let (mut home, mut elsewhere, mut offset) = (false, false, false);
        for g in g_codes(&ws) {
            match g {
                200 => {
                    scale = 25.4;
                    point.inches = true;
                }
                210 => {
                    scale = 1.0;
                    point.inches = false;
                }
                900 => relative = false,
                910 => relative = true,
                940 | 950 => point.feed_mode = Some(g / 10),
                170 | 180 | 190 => point.plane = Some(g / 10),
                540 | 550 | 560 | 570 | 580 | 590 => {
                    point.wcs = Some(g / 10);
                    if g != wcs {
                        // what was read so far is in the other coordinates
                        for (axis, here) in point.at_mm.iter_mut().enumerate() {
                            if here.take().is_some() {
                                lost[axis] = Some(number_of_line);
                            }
                        }
                        wcs = g;
                    }
                }
                100 => {
                    offset = true;
                    for (axis, here) in point.at_mm.iter_mut().enumerate() {
                        if here.take().is_some() {
                            lost[axis] = Some(number_of_line);
                        }
                    }
                }
                920 => {
                    offset = true;
                    shifted = Some(number_of_line);
                }
                921 | 922 => shifted = None,
                431 => {
                    offset = true;
                    tool_length = Some(number_of_line);
                }
                490 => tool_length = None,
                280 | 300 => home = true,
                530 => elsewhere = true,
                382..=385 => elsewhere = true,
                800 => cycle = None,
                810..=890 => cycle = Some(number_of_line),
                _ => {}
            }
        }
        for &(l, v) in &ws {
            match l {
                'M' => match v.round() as i32 {
                    m @ 3..=5 => point.spindle = Some(m),
                    m @ 7..=9 => point.coolant = Some(m),
                    // the end of the program switches both off
                    2 | 30 => {
                        point.spindle = Some(5);
                        point.coolant = Some(9);
                    }
                    _ => {}
                },
                'S' => point.speed = Some(v),
                'F' => point.feed = Some(v),
                _ => {}
            }
        }
        // `G28` and `G30` with no axis take every axis away
        let every = home && named.iter().all(Option::is_none);
        for axis in 0..3 {
            if every || ((home || elsewhere) && named[axis].is_some()) {
                point.at_mm[axis] = None;
                lost[axis] = Some(number_of_line);
                continue;
            }
            let Some(value) = named[axis] else { continue };
            if offset {
                continue;
            }
            if !relative {
                point.at_mm[axis] = Some(value * scale);
                lost[axis] = None;
            } else {
                match point.at_mm[axis].as_mut() {
                    Some(here) => *here += value * scale,
                    None => lost[axis] = Some(number_of_line),
                }
            }
        }
    }
    let refuse = |why: String| Err(format!("the job is not resumed: {why}"));
    if relative {
        return refuse(
            "the program is in relative distances (G91) there, so where it was is not certain"
                .into(),
        );
    }
    if let Some(n) = cycle {
        return refuse(format!("the canned cycle of line {n} is still on there"));
    }
    if let Some(n) = shifted {
        return refuse(format!(
            "line {n} shifts the zero (G92), which the reset of the controller takes away"
        ));
    }
    if let Some(n) = tool_length {
        return refuse(format!(
            "line {n} sets a tool length offset (G43.1), which the reset of the controller takes away"
        ));
    }
    if let Some((axis, Some(n))) = lost.iter().enumerate().find(|(_, l)| l.is_some()) {
        return refuse(format!(
            "after line {n} the position of the axis {} in the work coordinates is not known",
            AXIS[axis]
        ));
    }
    if point.feed.is_some_and(|f| !f.is_finite()) || point.speed.is_some_and(|s| !s.is_finite()) {
        return refuse("a feed or a speed of the program is not a number".into());
    }
    if point.at_mm.iter().flatten().any(|v| !v.is_finite()) {
        return refuse("a position of the program is not a number".into());
    }
    Ok(point)
}

/// Puts in front of a program that goes on the lines that take the machine back to where the program was, in place of the codes alone: up to the safe
/// height, the spindle (and its time to reach its speed), over the point, down to it, and the modes of the program. The spindle is started when the
/// program had it on, whatever `restore_outputs` says: a stopped tool must not be taken back into the work. The coolant follows `restore_outputs`.
/// A program that does not go on is left as it is.
pub fn with_resume_sequence(
    program: &mut Program,
    point: &ResumePoint,
    moves: ResumeMoves,
    restore_outputs: bool,
) {
    if !program.resume {
        return;
    }
    let [x, y, z] = point.at_mm;
    let mut lines = vec!["G21 G90".to_string()];
    // after the reset the controller is in G17, G54 and G94: only what differs is given again
    if let Some(plane) = point.plane.filter(|p| *p != 17) {
        lines.push(format!("G{plane}"));
    }
    if let Some(wcs) = point.wcs.filter(|w| *w != 54) {
        lines.push(format!("G{wcs}"));
    }
    // the feeds that follow are in mm/min
    if point.feed_mode.is_some() {
        lines.push("G94".to_string());
    }
    let lift = z.map_or(moves.safe_z_mm, |z| z.max(moves.safe_z_mm));
    lines.push(format!("G0 Z{}", number(lift)));
    let power_on = match point.spindle {
        Some(m @ (3 | 4)) => {
            let limit = program.speed_limit.unwrap_or(f64::INFINITY);
            let speed = point
                .speed
                .map(|s| format!(" S{}", number(s.min(limit))))
                .unwrap_or_default();
            Some(format!("M{m}{speed}"))
        }
        _ => None,
    };
    // a spindle turns before the tool goes back into the work; a laser is switched on only once the head is there
    let mut spindle_wait_after = None;
    let mut power_on_at = None;
    if let Some(line) = power_on.as_ref().filter(|_| !moves.laser) {
        spindle_wait_after = Some(lines.len());
        power_on_at = Some(lines.len());
        lines.push(line.clone());
    }
    if restore_outputs {
        if let Some(m @ (7 | 8)) = point.coolant {
            lines.push(format!("M{m}"));
        }
    }
    let across: String = [('X', x), ('Y', y)]
        .iter()
        .filter_map(|(a, v)| v.map(|v| format!(" {a}{}", number(v))))
        .collect();
    if !across.is_empty() {
        lines.push(format!("G0{across}"));
    }
    if let Some(z) = z.filter(|z| *z < lift) {
        let scale = if point.inches { 25.4 } else { 1.0 };
        // a program in G95 is refused before it gets here: F is in mm/min or in/min
        let own = point.feed.filter(|f| *f > 0.0).map(|f| f * scale);
        let feed = own.map_or(moves.plunge_feed_mm_min, |f| {
            f.min(moves.plunge_feed_mm_min)
        });
        lines.push(format!("G1 Z{} F{}", number(z), number(feed)));
    }
    if let Some(line) = power_on.filter(|_| moves.laser) {
        power_on_at = Some(lines.len());
        lines.push(line);
    }
    // the first line of the program that moves is given its motion again: until then the controller is in rapid, as after a reset
    lines.push("G0".to_string());
    if point.inches {
        lines.push("G20".to_string());
    }
    if let Some(f) = point.feed {
        lines.push(format!("F{}", number(f)));
    }
    // the lines of the sequence belong to the line the job goes on from
    let raw = program
        .entries
        .first()
        .filter(|_| program.preamble > 0)
        .or_else(|| program.entries.get(program.preamble))
        .map_or(program.raw_total, |e| e.raw);
    let first = program.entries.split_off(program.preamble);
    program.entries = lines.into_iter().map(|text| Entry { raw, text }).collect();
    program.preamble = program.entries.len();
    program.entries.extend(first);
    program.spindle_wait_after = spindle_wait_after;
    program.power_on_at = power_on_at;
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

/// The words of a line of code: a letter and its number, as the reader of G-code of the suite (EH3D-CORE-LIBS) reads them for GRBL, where blanks
/// count for nothing (`S1 000` is `S1000`). A line it cannot make a block of (a `$` line) has none. The reading says nothing of what was left out:
/// a line that is sent has been through [`parse`] first.
pub fn words(code: &str) -> Vec<(char, f64)> {
    block_words(code, Dialect::Grbl)
}

fn block_words(code: &str, dialect: Dialect) -> Vec<(char, f64)> {
    eh3d_parser_gcode::parse(code, dialect)
        .blocks
        .into_iter()
        .next()
        .map(|b| b.words.into_iter().map(|w| (w.letter, w.value)).collect())
        .unwrap_or_default()
}

/// A `$` line as GRBL reads it: without blanks and in capitals.
fn dollar_command(code: &str) -> String {
    code.chars()
        .filter(|c| !matches!(c, ' ' | '\t'))
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// A `$` line of GRBL that a machine with the switch on may take: `$` and then only letters, digits and the signs of a command or a setting. A second
/// `$` is taken only in `$$`, and the wipe of the memory never, however it is spelled.
fn dollar_ok(code: &str) -> bool {
    let command = dollar_command(code);
    let rest = &command[1..];
    !rest.starts_with("RST")
        && (rest == "$"
            || rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "=.-+#".contains(c)))
}

/// The diagnostics of the walk through a line that say the controller refuses it, or reads it in a way the server cannot judge.
const NOT_JUDGED: [&str; 10] = [
    codes::WORD_REPEATED,
    codes::FEED_MODE_UNSUPPORTED,
    codes::UNKNOWN_G_CODE,
    codes::UNKNOWN_M_CODE,
    codes::UNKNOWN_WORD,
    codes::NEGATIVE_FEED,
    codes::NEGATIVE_SPINDLE,
    codes::TOOL_NUMBER_INVALID,
    codes::CANNED_CYCLE_UNSUPPORTED,
    codes::CUTTER_COMPENSATION_UNSUPPORTED,
];

/// The first thing that the reader of the suite could not read or judge in a line: its code and what it says.
fn unreadable(code: &str) -> Option<(&'static str, String)> {
    let program = eh3d_parser_gcode::parse(code, Dialect::Grbl);
    if let Some(d) = program
        .diagnostics
        .iter()
        .find(|d| d.severity >= Severity::Error)
    {
        return Some((d.code, d.message.clone()));
    }
    eh3d_parser_gcode::interpret(&program)
        .diagnostics
        .into_iter()
        .find(|d| d.severity >= Severity::Error && NOT_JUDGED.contains(&d.code))
        .map(|d| (d.code, d.message))
}

fn refuse_line<T>(raw: usize, why: &str) -> Result<T, GcodeError> {
    fail("bad_gcode", format!("line {}: {why}", raw + 1))
}

/// A line that can be sent: refuses what is not G-code, and what the controller would read in another way than the server.
fn check_line(raw: usize, code: &str, allow_dollar: bool) -> Result<(), GcodeError> {
    if code.chars().any(|c| c.is_control() && c != '\t') {
        return refuse_line(raw, "control characters are not G-code");
    }
    // GRBL takes the bytes outside ASCII as commands of their own
    if !code.is_ascii() {
        return refuse_line(raw, "characters outside ASCII are not G-code");
    }
    if code.len() > MAX_LINE_LEN {
        return refuse_line(raw, &format!("longer than {MAX_LINE_LEN} characters"));
    }
    if allow_dollar && code.starts_with('$') {
        if !dollar_ok(code) {
            return refuse_line(
                raw,
                "this `$` command is not taken even by a machine that allows them",
            );
        }
        let command = dollar_command(code);
        // the words of a jog are read like any others
        return match command.strip_prefix("$J=").and_then(unreadable) {
            Some((what, message)) => refuse_line(raw, &format!("{what}: {message}")),
            None => Ok(()),
        };
    }
    if code.contains(['$', '!', '~', '?']) {
        return refuse_line(
            raw,
            "a settings or real-time command of the controller is not part of a job",
        );
    }
    if code.starts_with('/') {
        return refuse_line(
            raw,
            "a block-delete line (`/`) is not taken by the controller",
        );
    }
    if let Some((what, message)) = unreadable(code) {
        return refuse_line(raw, &format!("{what}: {message}"));
    }
    Ok(())
}

/// Whether a comment in parentheses of the line is left open, which would take the rest of the line with it.
pub(crate) fn comment_left_open(line: &str) -> bool {
    let mut open = false;
    for c in line.chars() {
        match c {
            ';' if !open => return false,
            '(' if !open => open = true,
            ')' if open => open = false,
            _ => {}
        }
    }
    open
}

/// The line as written, without the byte order mark of the file and without the carriage return of its end. A carriage return left inside ends the
/// line for a controller or a printer, also inside a comment: what follows it would not be what was read.
pub(crate) fn whole_line(n: usize, line: &str) -> Result<&str, GcodeError> {
    let line = if n == 0 {
        line.trim_start_matches('\u{feff}')
    } else {
        line
    };
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.contains('\r') {
        return refuse_line(n, "a carriage return inside a line would end it there");
    }
    Ok(line)
}

/// Whether the spindle (`M3`, `M4`) and the coolant (`M7`, `M8`) are on after the words of a line.
fn outputs_after(words: &[(char, f64)], outputs: &mut (bool, bool)) {
    for &(_, v) in words.iter().filter(|w| w.0 == 'M') {
        match v.round() as i32 {
            3 | 4 => outputs.0 = true,
            5 => outputs.0 = false,
            7 | 8 => outputs.1 = true,
            9 => outputs.1 = false,
            2 | 30 => *outputs = (false, false),
            _ => {}
        }
    }
}

/// Whether the lines leave the spindle and the coolant on at their end.
pub fn left_on(entries: &[Entry]) -> (bool, bool) {
    let mut outputs = (false, false);
    for e in entries {
        outputs_after(&words(&e.text), &mut outputs);
    }
    outputs
}

/// The first line (counted from 0 in the text) where the program waits for the operator (`M0`, `M1`) with the spindle on.
pub fn pause_with_spindle_on(entries: &[Entry]) -> Option<usize> {
    let mut outputs = (false, false);
    for e in entries {
        let ws = words(&e.text);
        outputs_after(&ws, &mut outputs);
        let waits = ws.iter().any(|&(l, v)| l == 'M' && (v == 0.0 || v == 1.0));
        if waits && outputs.0 {
            return Some(e.raw);
        }
    }
    None
}

/// The text of a program for a 3D printer cut into its lines of code. The printer is given the text as it is, so no line may hold anything that
/// ends it early, also in a comment; what the lines ask of the printer is judged by `printer3d`.
pub fn parse_printer(text: &str) -> Result<Vec<Entry>, GcodeError> {
    let raw: Vec<&str> = text.lines().collect();
    if raw.len() > MAX_LINES {
        return fail(
            "too_big",
            format!("a program may have {MAX_LINES} lines at the most"),
        );
    }
    let mut entries = Vec::new();
    for (n, line) in raw.iter().enumerate() {
        let line = whole_line(n, line)?;
        if line.chars().any(|c| c.is_control() && c != '\t') {
            return refuse_line(n, "control characters are not G-code");
        }
        let code = strip(line);
        if code.is_empty() || code == "%" {
            continue;
        }
        if code.starts_with(['$', '!', '~', '?']) {
            return refuse_line(
                n,
                "a settings or real-time command of the controller is not part of a job",
            );
        }
        if code.chars().count() > MAX_LINE_LEN {
            return refuse_line(n, &format!("longer than {MAX_LINE_LEN} characters"));
        }
        entries.push(Entry { raw: n, text: code });
    }
    if entries.is_empty() {
        return fail("no_work", "the program has no line of code to send");
    }
    Ok(entries)
}

/// A number as short as it can be written.
fn number(v: f64) -> String {
    let text = format!("{v:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The codes of the `G` words of a line, as ten times the number (`G92.1` is 921).
fn g_codes(ws: &[(char, f64)]) -> impl Iterator<Item = i32> + '_ {
    ws.iter()
        .filter(|(l, _)| *l == 'G')
        .map(|(_, v)| (v * 10.0).round() as i32)
}

/// The motion in force after a line that names one: `Some(0..=3)` for `G0` to `G3`; `None` inside for a code that ends it or that must never be given again
/// by itself (`G80`, a probe).
fn motion_of(ws: &[(char, f64)]) -> Option<Option<u8>> {
    g_codes(ws).fold(None, |found, g| match g {
        0 => Some(Some(0)),
        10 => Some(Some(1)),
        20 => Some(Some(2)),
        30 => Some(Some(3)),
        800 | 382..=385 => Some(None),
        _ => found,
    })
}

/// Whether a line has a word of an axis (or of the centre of an arc).
fn has_axis(ws: &[(char, f64)]) -> bool {
    ws.iter()
        .any(|(l, _)| matches!(l, 'X' | 'Y' | 'Z' | 'A' | 'B' | 'C' | 'I' | 'J' | 'K'))
}

/// A controller that was reset by the stop is in `G0` again: the first line that moves without naming how gets the motion that was in force, so that a cut is
/// never made at the speed of a rapid move.
fn restore_motion(motion: Option<u8>, entries: &mut [Entry]) {
    let Some(motion) = motion else { return };
    for e in entries {
        if e.text.starts_with('$') {
            continue;
        }
        let ws = words(&e.text);
        if motion_of(&ws).is_some() {
            return;
        }
        // these codes take the words of the axes for themselves
        let taken = g_codes(&ws).any(|g| matches!(g, 100 | 280 | 300 | 431 | 920));
        if has_axis(&ws) && !taken {
            e.text = format!("G{motion} {}", e.text);
            return;
        }
    }
}

/// The line a stopped job goes on from by itself: `margin` lines that move before the line `reached` (the controller takes lines ahead of the movement, and the
/// stop empties what it had taken), and never before `before`, where the stopped job itself had started. Refused, with the reason, when sending those lines again
/// or giving the codes in force back would not put the machine where the program expects it.
pub fn resume_line(
    text: &str,
    reached: usize,
    margin: usize,
    before: usize,
) -> Result<usize, &'static str> {
    const RELATIVE: &str = "the program moves in relative distances (G91) where it would go on: the moves would be counted again from where the head now stands";
    const LOST: &str = "an offset the program set (G92 or G43.1) is lost when the controller is stopped and cannot be given back";
    const SET_AGAIN: &str =
        "the lines that would be sent again set an offset (G92 or G10) from where the head stands";
    let raw: Vec<&str> = text.lines().collect();
    let end = reached.min(raw.len());
    // nothing was taken: the job goes on where it had started
    if end == 0 {
        return Ok(before);
    }
    let words_of = |n: usize| words(&strip(raw[n]));
    let (mut skip, mut back) = (end, 0);
    while skip > 0 && back < margin {
        skip -= 1;
        let ws = words_of(skip);
        // a line that only sets an offset moves nothing
        if has_axis(&ws) && !g_codes(&ws).any(|g| matches!(g, 100 | 431 | 920)) {
            back += 1;
        }
    }
    let skip = skip.max(before).min(raw.len());
    let (mut relative, mut shifted, mut tool_length) = (false, false, false);
    for n in 0..skip {
        for g in g_codes(&words_of(n)) {
            match g {
                900 => relative = false,
                910 => relative = true,
                920 => shifted = true,
                921 | 922 => shifted = false,
                431 => tool_length = true,
                490 => tool_length = false,
                _ => {}
            }
        }
    }
    if relative {
        return Err(RELATIVE);
    }
    if shifted || tool_length {
        return Err(LOST);
    }
    for n in skip..end {
        for g in g_codes(&words_of(n)) {
            match g {
                910 => return Err(RELATIVE),
                100 | 920 => return Err(SET_AGAIN),
                _ => {}
            }
        }
    }
    Ok(skip)
}

/// Cleans the text into a program. `skip` is how many lines of the text (counting the blank and comment ones) were already sent: they are not sent
/// again; the codes they put in force are given again instead and the job waits for the operator before it moves.
pub fn parse(text: &str, skip: usize, restore_outputs: bool) -> Result<Program, GcodeError> {
    parse_with(text, skip, restore_outputs, false)
}

/// Like [`parse`], and takes the `$` lines of GRBL when `allow_dollar` is set (the switch of the machine).
pub fn parse_with(
    text: &str,
    skip: usize,
    restore_outputs: bool,
    allow_dollar: bool,
) -> Result<Program, GcodeError> {
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
    let mut modal = eh3d_parser_gcode::ModalState::default();
    let mut motion: Option<u8> = None;
    let mut entries: Vec<Entry> = Vec::new();
    // the line where the program ends (M2, M30): a controller runs whatever is sent after it
    let mut ended: Option<usize> = None;
    for (n, line) in raw.iter().enumerate() {
        let line = whole_line(n, line)?;
        if comment_left_open(line) {
            return refuse_line(n, "a comment in parentheses is not closed");
        }
        let code = strip(line);
        if code.is_empty() || code == "%" {
            continue;
        }
        check_line(n, &code, allow_dollar)?;
        if let Some(end) = ended {
            return refuse_line(
                n,
                &format!("code after the end of the program (line {end})"),
            );
        }
        if words(&code)
            .iter()
            .any(|&(l, v)| l == 'M' && (v == 2.0 || v == 30.0))
        {
            ended = Some(n + 1);
        }
        if n < skip {
            let ws = words(&code);
            if let Some(named) = motion_of(&ws) {
                motion = named;
            }
            modal.apply_words(ws);
        } else {
            entries.push(Entry { raw: n, text: code });
        }
    }
    if entries.is_empty() {
        return fail("no_work", "the program has no line of code to send");
    }
    restore_motion(motion, &mut entries);
    let mut lead: Vec<Entry> = Vec::new();
    if skip > 0 {
        lead = modal
            .restore_lines(restore_outputs)
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
        spindle_wait_after: None,
        power_on_at: None,
        speed_limit: None,
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
        // a jog moves in its own distances: only its feed is taken, as it is the same thing the machine is asked
        if e.text.starts_with('$') {
            if let Some(jog) = dollar_command(&e.text).strip_prefix("$J=") {
                let ws = words(jog);
                let unit = unit_of(&ws).unwrap_or(scale);
                for &(_, v) in ws.iter().filter(|w| w.0 == 'F') {
                    r.max_feed_mm_min = r.max_feed_mm_min.max(v * unit);
                }
            }
            continue;
        }
        let ws = words(&e.text);
        let mut target = [None; 3];
        let mut machine_coords = false;
        // the unit of a line counts for all its words, wherever the G20 or the G21 stands
        scale = unit_of(&ws).unwrap_or(scale);
        for &(l, v) in &ws {
            match l {
                'G' => match (v * 10.0).round() as i32 {
                    0 => motion = 0,
                    10 | 20 | 30 => motion = 1,
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

/// The unit a line sets, as the millimetres of one of its distances (`G20`, `G21`).
pub fn unit_of(words: &[(char, f64)]) -> Option<f64> {
    g_codes(words)
        .filter_map(|g| match g {
            200 => Some(25.4),
            210 => Some(1.0),
            _ => None,
        })
        .last()
}

/// The turns of every `S` word cut to `max` (for a machine whose spindle switch is on `warn`). The number is taken as GRBL takes it, blanks and all
/// (`S 3 0000` is `S30000`).
pub fn cut_speed(code: &str, max: f64) -> String {
    if code.starts_with('$') {
        return code.to_string();
    }
    let mut out = String::new();
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.eq_ignore_ascii_case(&'S') {
            // `end` is after the last character of the number
            let (mut j, mut end) = (i + 1, i + 1);
            let mut digits = String::new();
            while j < chars.len() && matches!(chars[j], '0'..='9' | '.' | '+' | '-' | ' ' | '\t') {
                if !matches!(chars[j], ' ' | '\t') {
                    digits.push(chars[j]);
                    end = j + 1;
                }
                j += 1;
            }
            match digits.parse::<f64>() {
                Ok(v) if v > max => {
                    out.push('S');
                    out.push_str(&number(max));
                }
                _ => out.extend(&chars[i..end]),
            }
            i = end;
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
    fn a_machine_that_allows_dollar_lines_takes_them_but_never_the_wipe_or_the_realtime_characters()
    {
        let text = "$H\nG21\n$X\n$G\n$J=G91 X1 F100\n$100=250.000\nG0 X1\n";
        let ok = parse_with(text, 0, false, true).unwrap();
        assert_eq!(ok.entries.len(), 7);
        assert_eq!(ok.entries[0].text, "$H");
        assert_eq!(parse(text, 0, false).unwrap_err().code, "bad_gcode");
        for bad in ["$RST=*", "$rst=$", "!", "~", "?", "$\"x\"", "$H\u{1}"] {
            assert_eq!(
                parse_with(bad, 0, false, true).unwrap_err().code,
                "bad_gcode",
                "{bad}"
            );
        }
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

    fn texts(p: &Program) -> Vec<&str> {
        p.entries.iter().map(|e| e.text.as_str()).collect()
    }

    #[test]
    fn a_job_that_goes_on_gives_the_motion_in_force_to_the_first_line_that_moves_without_one() {
        let text = "G21 G90\nM3 S9000\nG1 X1 F300\nX2\nY3\nX4\nM5\n";
        let p = parse(text, 4, false).unwrap();
        // a controller that was reset is in G0: without the G1 the cut would go at the speed of a rapid move
        assert_eq!(texts(&p), ["G21 G90", "F300", "G1 Y3", "X4", "M5"]);
        assert_eq!((p.preamble, p.entries[2].raw), (2, 4));
        // the lines that move nothing by this motion are passed over
        let p = parse("G0 X1\nM8\nG92 X0\nG4 P1\nZ5\n", 1, false).unwrap();
        assert_eq!(texts(&p), ["M8", "G92 X0", "G4 P1", "G0 Z5"]);
        // a line that names its own motion needs nothing, and neither does the start of a program
        let p = parse("G1 X1 F300\nG0 Z5\nX2\n", 1, false).unwrap();
        assert_eq!(texts(&p), ["F300", "G0 Z5", "X2"]);
        assert_eq!(texts(&parse("G1 F300\nX2\n", 0, false).unwrap())[1], "X2");
        // an arc keeps its direction; a probe is never given again by itself
        let p = parse("G2 X1 I1 F100\nX2 I1\n", 1, false).unwrap();
        assert_eq!(texts(&p), ["F100", "G2 X2 I1"]);
        let p = parse("G38.2 Z-5 F50\nZ-6\n", 1, false).unwrap();
        assert_eq!(texts(&p), ["F50", "Z-6"]);
    }

    #[test]
    fn the_line_to_go_on_from_goes_back_by_lines_that_move_and_not_by_lines_of_text() {
        let text = "G21 G90\nG0 Z5\nG0 X1\n(a note)\n\nG0 X2\n; another\nM8\nG0 X3\nG0 X4\n";
        // two moves before the line 9 are those of the lines 8 and 5: the notes, the blank line and the M8 do not count
        assert_eq!(resume_line(text, 9, 2, 0), Ok(5));
        assert_eq!(resume_line(text, 10, 3, 0), Ok(5));
        // no margin: the line reached; never before where the job had started; the whole program when there are fewer moves
        assert_eq!(resume_line(text, 9, 0, 0), Ok(9));
        assert_eq!(resume_line(text, 9, 2, 8), Ok(8));
        assert_eq!(resume_line(text, 9, 16, 0), Ok(0));
        // a line beyond the text counts as its end, and nothing taken changes nothing
        assert_eq!(resume_line(text, 99, 1, 0), Ok(9));
        assert_eq!(resume_line(text, 0, 16, 3), Ok(3));
    }

    #[test]
    fn a_job_does_not_go_on_by_itself_where_the_codes_cannot_be_given_back() {
        let why = |text: &str, reached| resume_line(text, reached, 1, 0).unwrap_err();
        // relative distances at the line to go on from, or among the lines sent again
        assert!(why("G91\nG1 X1 F100\nG1 X1\nG1 X1\n", 3).contains("G91"));
        assert!(why("G90\nG0 X1\nG91\nG0 X1\nG90\n", 5).contains("G91"));
        // relative lines that are over and done with long before do not matter
        assert_eq!(
            resume_line("G91\nG0 X1\nG90\nG0 X2\nG0 X3\n", 5, 1, 0),
            Ok(4)
        );
        // an offset in force that the reset takes away
        assert!(why("G92 X0\nG0 X1\nG0 X2\n", 3).contains("G43.1"));
        assert!(why("G43.1 Z1.5\nG0 X1\nG0 X2\n", 3).contains("G43.1"));
        assert_eq!(resume_line("G92 X0\nG92.1\nG0 X1\nG0 X2\n", 4, 1, 0), Ok(3));
        // an offset set again from wherever the head is now
        assert!(why("G0 X1\nG0 X2\nG10 L20 P1 X0\n", 3).contains("G10"));
        assert!(why("G0 X1\nG92 Y0\n", 2).contains("G92"));
    }

    fn sequence(text: &str, skip: usize, outputs: bool) -> (Vec<String>, Option<usize>) {
        let mut p = parse(text, skip, outputs).unwrap();
        let point = resume_point(text, skip).unwrap();
        with_resume_sequence(&mut p, &point, ResumeMoves::default(), outputs);
        assert_eq!(p.entries[p.preamble].raw, skip);
        assert!(p.entries[..p.preamble].iter().all(|e| e.raw == skip));
        let lines = p.entries[..p.preamble]
            .iter()
            .map(|e| e.text.clone())
            .collect();
        (lines, p.spindle_wait_after)
    }

    #[test]
    fn a_resumed_job_goes_up_starts_the_spindle_crosses_and_goes_down_to_where_it_was() {
        let text = "G21 G90\nG55\nM3 S9000\nM8\nG0 X10 Y20\nG1 Z-1.5 F300\nX30\nX40\n";
        let (lines, wait) = sequence(text, 7, true);
        assert_eq!(
            lines,
            [
                "G21 G90",
                "G55",
                "G0 Z5",
                "M3 S9000",
                "M8",
                "G0 X30 Y20",
                "G1 Z-1.5 F100",
                "G0",
                "F300"
            ]
        );
        assert_eq!(
            wait,
            Some(3),
            "the wait comes after the line of the spindle"
        );
        // the spindle starts also without the outputs; the coolant does not
        let (lines, wait) = sequence(text, 7, false);
        assert!(lines.contains(&"M3 S9000".to_string()) && !lines.contains(&"M8".to_string()));
        assert_eq!(wait, Some(3));
    }

    #[test]
    fn the_sequence_keeps_the_units_a_slower_feed_and_a_height_above_the_safe_one() {
        // inches: the places are given in mm and the unit comes back at the end; a feed slower than the plunge one is kept
        let (lines, wait) = sequence("G20 G90\nF2\nG0 X1 Y2\nG1 Z-0.1\nX2\n", 4, false);
        assert_eq!(
            lines,
            [
                "G21 G90",
                "G0 Z5",
                "G0 X25.4 Y50.8",
                "G1 Z-2.54 F50.8",
                "G0",
                "G20",
                "F2"
            ]
        );
        assert_eq!(wait, None, "no spindle in the program, nothing to wait for");
        // the program was above the safe height: it stays there, and the spindle that was switched off stays off
        let (lines, wait) = sequence("M3 S1000\nM5\nG0 Z30\nG0 X1\nG0 X2\n", 4, true);
        assert_eq!(lines, ["G21 G90", "G0 Z30", "G0 X1", "G0"]);
        assert_eq!(wait, None);
        // a plane and relative lines that are over and done with
        let (lines, _) = sequence(
            "G18\nG0 X1 Z1\nG91\nG0 X2 Z-3\nG90\nG0 Y1\nG0 Y2\n",
            6,
            false,
        );
        assert_eq!(
            lines,
            ["G21 G90", "G18", "G0 Z5", "G0 X3 Y1", "G1 Z-2 F100", "G0"]
        );
        // a feed for each turn cannot be judged against the limits: such a program is not taken at all
        let e = parse("G95\nF0.1\nG0 X1 Z-1\nG1 X2\n", 3, false).unwrap_err();
        assert!(e.detail.contains("feed-mode-unsupported"), "{}", e.detail);
        // the mode of the feed the program had set is given again
        let (lines, _) = sequence("G94\nF100\nG0 X1 Z-1\nG1 X2\n", 3, false);
        assert_eq!(
            lines,
            [
                "G21 G90",
                "G94",
                "G0 Z5",
                "G0 X1",
                "G1 Z-1 F100",
                "G0",
                "F100"
            ]
        );
        // a machine whose spindle gives less starts it at what it gives
        let text = "M3 S30000\nG0 X1\nG0 X2\n";
        let mut p = parse(text, 2, false).unwrap();
        p.speed_limit = Some(24_000.0);
        let point = resume_point(text, 2).unwrap();
        with_resume_sequence(&mut p, &point, ResumeMoves::default(), false);
        assert!(texts(&p).contains(&"M3 S24000"), "{:?}", texts(&p));
        // a program that does not go on is left as it is
        let mut p = parse("G0 X1", 0, false).unwrap();
        with_resume_sequence(
            &mut p,
            &ResumePoint::default(),
            ResumeMoves::default(),
            true,
        );
        assert_eq!((p.preamble, p.entries.len()), (0, 1));
    }

    #[test]
    fn a_job_is_not_resumed_where_the_place_of_the_tool_is_not_certain() {
        let why = |text: &str| {
            let skip = text.lines().count();
            resume_point(text, skip).unwrap_err()
        };
        assert!(why("G91\nG1 X1 F100\n").contains("G91"));
        assert!(why("G91\nG1 X1 F100\nG90\n").contains("axis X"));
        assert!(why("G0 X1\nG92 X0\n").contains("G92"));
        assert!(why("G43.1 Z2\nG0 X1\n").contains("G43.1"));
        assert!(why("G81 Z-1 R1 X1\n").contains("canned cycle"));
        assert!(why("G0 Z5\nG53 G0 Z0\n").contains("axis Z"));
        assert!(why("G0 X1 Y1\nG28\n").contains("axis X"));
        assert!(why("$H\nG0 X1\n").contains("axis Y"));
        assert!(why("G0 X1\nG55\n").contains("axis X"));
        assert!(why("G0 Y1\nG10 L20 P1 X0\n").contains("axis Y"));
        assert!(why("G38.2 Z-5 F50\n").contains("axis Z"));
        // an absolute move after it says again where the axis is
        let p = resume_point("G53 G0 Z0\nG0 X1 Y2 Z3\n", 2).unwrap();
        assert_eq!(p.at_mm, [Some(1.0), Some(2.0), Some(3.0)]);
        // G54 is the one in force from the start, and the end of a program switches the outputs off
        let p = resume_point("G0 X1\nG54\nM3 S10\nM8\nM30\n", 5).unwrap();
        assert_eq!(
            (p.at_mm[0], p.spindle, p.coolant),
            (Some(1.0), Some(5), Some(9))
        );
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

    /// The lines and the words that the reader of EH3D-CORE-LIBS also gives for them (its test `shared_words` holds the same table): while the two readers are not one,
    /// a change that makes them disagree fails in the repository that made it.
    const SHARED_WORDS: &[(&str, &[(char, f64)])] = &[
        (
            "G1 X10 Y-5.5 F300",
            &[('G', 1.0), ('X', 10.0), ('Y', -5.5), ('F', 300.0)],
        ),
        ("g0x1y2", &[('G', 0.0), ('X', 1.0), ('Y', 2.0)]),
        (
            "G1 X1 (a note) Y2 ; the rest",
            &[('G', 1.0), ('X', 1.0), ('Y', 2.0)],
        ),
        ("M3 S12000", &[('M', 3.0), ('S', 12000.0)]),
        ("G21G90", &[('G', 21.0), ('G', 90.0)]),
        ("G1 X-0.25 Z-1", &[('G', 1.0), ('X', -0.25), ('Z', -1.0)]),
    ];

    #[test]
    fn the_lines_of_the_shared_table_give_the_words_the_core_reader_gives() {
        for (line, expected) in SHARED_WORDS {
            assert_eq!(&words(&strip(line)), expected, "{line:?}");
        }
    }

    /// A small generator with a fixed seed: the same programs on every run.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }

    const PIECES: &[&str] = &[
        "G0",
        "G1",
        "G2",
        "G20",
        "G21",
        "G90",
        "G91",
        "G54",
        "G92",
        "M3",
        "M5",
        "M8",
        "S12000",
        "F300",
        "X10",
        "Y-5.5",
        "Z.5",
        "X",
        "(note)",
        "; tail",
        "%",
        "$H",
        "$$",
        "!",
        "\t",
        " ",
        "é",
        "9999999999999999999999",
        "G1X1Y1",
        "\u{1}",
    ];

    /// Lines a controller takes, so that the made-up programs are not all refused.
    const TAKEN: &[&str] = &[
        "G21 G90",
        "G0 X10",
        "G1 Y-5.5 F300",
        "M3 S12000",
        "M5",
        "M8",
        "(note)",
        "; tail ! ?",
    ];

    fn program(rng: &mut Rng) -> String {
        let lines = 1 + rng.below(12);
        (0..lines)
            .map(|_| {
                if rng.below(8) != 0 {
                    return TAKEN[rng.below(TAKEN.len())].to_string();
                }
                (0..1 + rng.below(6))
                    .map(|_| PIECES[rng.below(PIECES.len())])
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn made_up_programs_never_panic_and_a_resumed_one_keeps_its_lines() {
        let mut rng = Rng(0x5E57_ED00);
        let (mut ok, mut refused) = (0, 0);
        for _ in 0..3_000 {
            let text = program(&mut rng);
            let skip = rng.below(14);
            let _ = words(&strip(&text));
            match parse_with(&text, skip, rng.below(2) == 0, rng.below(2) == 0) {
                Ok(p) => {
                    ok += 1;
                    // the lines of the program come after the preamble that gives the state back, and none is empty
                    assert!(p.preamble <= p.entries.len(), "{text:?}");
                    assert!(p.entries.iter().all(|e| !e.text.is_empty()), "{text:?}");
                    assert_eq!(p.resume, skip > 0);
                    // a program that is not resumed has no preamble
                    if skip == 0 {
                        assert_eq!(p.preamble, 0, "{text:?}");
                    }
                }
                Err(_) => refused += 1,
            }
        }
        assert!(
            ok > 100 && refused > 100,
            "{ok} {refused}: both answers must be reached"
        );
    }
}
