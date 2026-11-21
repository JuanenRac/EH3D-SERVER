//! Edge cases of the pure parts of the server that talk to a printer, the drill head, the log and the binary G-code: the lines of the Marlin protocol and the firmware in
//! memory, tickets, hidden keys, the request to the node of the head, the audit, the limits of a spindle and the reader and writer of `.bgcode`.
//! Nothing here starts a server; the files it writes are in a folder of its own that is removed at the end.

use eh3d_server::audit::{self, Filter, LogFile};
use eh3d_server::bgcode::{self, Bgcode, Compression, Limits, Thumbnail, WriteOptions};
use eh3d_server::head::{self, HeadError};
use eh3d_server::inspect;
use eh3d_server::marlin::{
    checksum, frame, parse_sd_status, parse_temps, strip_comment, unframe, MarlinSim, SdStatus,
    Temps,
};
use eh3d_server::secrets::{self, MASK};
use eh3d_server::spindle::{self, Finding, Guard, Mode, Override};
use eh3d_server::tickets::{Tickets, TICKET_SECONDS};
use serde_json::json;

/// A small seeded generator, so every run sees the same bytes.
fn lcg(seed: u32, n: usize, alphabet: u32) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((s >> 16) % alphabet) as u8
        })
        .collect()
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("eh3d-r5-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------- the lines of the Marlin protocol

#[test]
fn the_checksum_of_nothing_is_zero_and_a_repeated_byte_cancels() {
    assert_eq!(checksum(""), 0);
    assert_eq!(checksum("aa"), 0);
    assert_eq!(checksum("a"), b'a');
}

#[test]
fn the_checksum_does_not_depend_on_the_order_of_the_bytes() {
    assert_eq!(checksum("M105"), checksum("501M"));
}

#[test]
fn a_framed_line_goes_back_to_its_number_and_text_for_any_number() {
    for n in [0u32, 1, 99, 65_535, u32::MAX] {
        let framed = frame(n, "G1 X10 Y-2.5");
        assert_eq!(
            unframe(&framed),
            Ok((Some(n), "G1 X10 Y-2.5".to_string())),
            "{framed}"
        );
    }
}

#[test]
fn a_framed_empty_line_keeps_its_number() {
    assert_eq!(unframe(&frame(7, "")), Ok((Some(7), String::new())));
}

#[test]
fn the_blanks_of_a_framed_line_are_kept_inside_the_checksum() {
    let framed = frame(3, " G1 ");
    assert_eq!(unframe(&framed), Ok((Some(3), " G1 ".to_string())));
}

#[test]
fn lines_made_from_a_seeded_alphabet_go_through_the_wire_and_back() {
    let alphabet = b"GMXYZEFST0123456789. -";
    for seed in 1..60u32 {
        let raw = lcg(seed, 1 + (seed as usize % 30), alphabet.len() as u32);
        let text: String = raw.iter().map(|b| alphabet[*b as usize] as char).collect();
        let framed = frame(seed, &text);
        assert_eq!(unframe(&framed), Ok((Some(seed), text.clone())), "{framed}");
    }
}

#[test]
fn one_changed_byte_of_a_framed_line_is_a_checksum_mismatch() {
    let framed = frame(12, "M104 S200");
    let broken = framed.replace("S200", "S201");
    assert_eq!(unframe(&broken), Err("checksum mismatch".to_string()));
}

#[test]
fn a_checksum_that_is_not_a_byte_is_refused() {
    let not_a_number = "the checksum is not a number".to_string();
    assert_eq!(unframe("N1 M105*abc"), Err(not_a_number.clone()));
    assert_eq!(unframe("N1 M105*256"), Err(not_a_number.clone()));
    assert_eq!(unframe("N1 M105*-1"), Err(not_a_number.clone()));
    assert_eq!(unframe("N1 M105*"), Err(not_a_number));
}

#[test]
fn a_checked_line_has_to_start_with_n_and_a_number() {
    let body = "G1 X1";
    let no_n = format!("{body}*{}", checksum(body));
    assert_eq!(
        unframe(&no_n),
        Err("a checked line starts with N".to_string())
    );
    let body = "Nx G1";
    let bad_number = format!("{body}*{}", checksum(body));
    assert_eq!(
        unframe(&bad_number),
        Err("the line number is not a number".to_string())
    );
    let body = "N4294967296 M105";
    let too_big = format!("{body}*{}", checksum(body));
    assert_eq!(
        unframe(&too_big),
        Err("the line number is not a number".to_string())
    );
}

#[test]
fn a_line_with_no_checksum_comes_back_trimmed_and_with_no_number() {
    assert_eq!(unframe("  M105 \r\n"), Ok((None, "M105".to_string())));
    assert_eq!(unframe(""), Ok((None, String::new())));
}

#[test]
fn the_temperature_report_is_read_with_or_without_the_ok() {
    let want = Temps {
        hotend_c: 200.0,
        hotend_target_c: 210.0,
        bed_c: 60.0,
        bed_target_c: 65.0,
    };
    assert_eq!(
        parse_temps("ok T:200.00 /210.00 B:60.00 /65.00 @:127 B@:0"),
        Some(want.clone())
    );
    assert_eq!(
        parse_temps("T:200.00 /210.00 B:60.00 /65.00"),
        Some(want.clone())
    );
    assert_eq!(parse_temps("  ok   T:200 /210 B:60 /65  "), Some(want));
}

#[test]
fn the_bed_may_come_before_the_hotend() {
    let t = parse_temps("ok B:20.0 /0.0 T:21.0 /0.0").unwrap();
    assert_eq!((t.hotend_c, t.bed_c), (21.0, 20.0));
}

#[test]
fn the_first_reading_of_a_kind_wins() {
    let t = parse_temps("ok T:1 /1 T:2 /2 B:3 /3 B:4 /4").unwrap();
    assert_eq!((t.hotend_c, t.bed_c), (1.0, 3.0));
}

#[test]
fn a_temperature_that_is_not_a_number_is_not_a_reading() {
    // `nan` and `inf` read as numbers, and every comparison with them is false, so a limit would never trip
    assert_eq!(parse_temps("ok T:nan /0.0 B:20.0 /0.0"), None);
    assert_eq!(parse_temps("ok T:20.0 /0.0 B:inf /0.0"), None);
    assert_eq!(parse_temps("ok T:20.0 /NaN B:20.0 /0.0"), None);
    assert_eq!(parse_temps("ok T:20.0 /0.0 B:20.0 /-inf"), None);
}

#[test]
fn a_report_that_lacks_a_target_or_a_kind_is_not_read() {
    assert_eq!(parse_temps("ok T:21.0 B:20.0"), None);
    assert_eq!(parse_temps("ok T:21.0 /0.0"), None);
    assert_eq!(parse_temps("ok B:21.0 /0.0"), None);
    assert_eq!(parse_temps(""), None);
    assert_eq!(parse_temps("T: /0 B: /0"), None);
}

#[test]
fn a_report_with_text_that_is_not_ascii_does_not_stop_the_reader() {
    assert_eq!(parse_temps("ok T:é /0 B:é /0"), None);
    let t = parse_temps("ok ñ T:21.5 /0 日本 B:20 /0").unwrap();
    assert_eq!(t.hotend_c, 21.5);
}

#[test]
fn a_negative_temperature_is_a_reading() {
    let t = parse_temps("ok T:-1.00 /0.00 B:20 /0").unwrap();
    assert_eq!(t.hotend_c, -1.0);
}

#[test]
fn the_card_status_has_two_forms_and_nothing_else() {
    assert_eq!(
        parse_sd_status("SD printing byte 0/0"),
        Some(SdStatus::Printing { done: 0, total: 0 })
    );
    assert_eq!(
        parse_sd_status("  Not SD printing  "),
        Some(SdStatus::NotPrinting)
    );
    assert_eq!(parse_sd_status("SD printing byte 5/-1"), None);
    assert_eq!(parse_sd_status("SD printing byte /5"), None);
    assert_eq!(parse_sd_status("SD printing byte 5"), None);
    assert_eq!(parse_sd_status("SD printing byte 1/2/3"), None);
    assert_eq!(parse_sd_status(""), None);
}

#[test]
fn the_card_status_holds_the_biggest_numbers_and_refuses_the_next() {
    let max = u64::MAX;
    assert_eq!(
        parse_sd_status(&format!("SD printing byte {max}/{max}")),
        Some(SdStatus::Printing {
            done: max,
            total: max
        })
    );
    assert_eq!(
        parse_sd_status("SD printing byte 18446744073709551616/1"),
        None
    );
}

#[test]
fn a_comment_ends_the_line_at_the_first_semicolon() {
    assert_eq!(strip_comment("G1 X1 ;a;b"), "G1 X1");
    assert_eq!(strip_comment("  M105  "), "M105");
    assert_eq!(strip_comment(";"), "");
    assert_eq!(strip_comment(""), "");
    assert_eq!(strip_comment("é ; ñ"), "é");
}

// ---------------------------------------------------------------- the firmware in memory

fn say(sim: &mut MarlinSim, line: &str) -> Vec<String> {
    sim.handle(line)
}

fn temps(sim: &mut MarlinSim) -> Temps {
    parse_temps(&sim.handle("M105")[0]).unwrap()
}

/// Writes a file of `lines` to the card of the simulated printer.
fn put_file(sim: &mut MarlinSim, name: &str, lines: &[&str]) {
    sim.handle(&format!("M28 {name}"));
    for line in lines {
        sim.handle(line);
    }
    sim.handle("M29");
}

#[test]
fn a_new_firmware_reads_the_room_temperature_everywhere() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    assert_eq!(
        say(&mut sim, "M105"),
        strings(&["ok T:22.00 /0.00 B:22.00 /0.00 @:0 B@:0"])
    );
    assert!(!sim.halted());
}

#[test]
fn a_target_in_the_limits_is_taken_and_the_report_shows_it() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    assert_eq!(say(&mut sim, "M104 S200"), strings(&["ok"]));
    assert_eq!(say(&mut sim, "M140 S60"), strings(&["ok"]));
    let t = temps(&mut sim);
    assert_eq!((t.hotend_target_c, t.bed_target_c), (200.0, 60.0));
}

#[test]
fn the_limit_itself_is_a_target_and_zero_too() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    assert_eq!(say(&mut sim, "M104 S275"), strings(&["ok"]));
    assert_eq!(say(&mut sim, "M140 S120"), strings(&["ok"]));
    assert_eq!(say(&mut sim, "M104 S0"), strings(&["ok"]));
    assert_eq!(temps(&mut sim).hotend_target_c, 0.0);
}

#[test]
fn a_target_past_the_limit_or_below_zero_is_an_error_and_changes_nothing() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    assert_eq!(
        say(&mut sim, "M104 S276"),
        strings(&["Error:M104 S276 is out of 0 to 275", "ok"])
    );
    assert_eq!(
        say(&mut sim, "M104 S-5"),
        strings(&["Error:M104 S-5 is out of 0 to 275", "ok"])
    );
    assert_eq!(
        say(&mut sim, "M140 S121"),
        strings(&["Error:M140 S121 is out of 0 to 120", "ok"])
    );
    let t = temps(&mut sim);
    assert_eq!((t.hotend_target_c, t.bed_target_c), (0.0, 0.0));
}

#[test]
fn a_target_that_is_not_a_number_is_an_error() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    let answer = say(&mut sim, "M104 Snan");
    assert!(answer[0].starts_with("Error:M104"), "{answer:?}");
    assert_eq!(temps(&mut sim).hotend_target_c, 0.0);
    let answer = say(&mut sim, "M104 Sinf");
    assert!(answer[0].starts_with("Error:M104"), "{answer:?}");
}

#[test]
fn the_words_are_read_in_any_letter_case() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    assert_eq!(say(&mut sim, "m104 s150"), strings(&["ok"]));
    assert_eq!(temps(&mut sim).hotend_target_c, 150.0);
    assert_eq!(say(&mut sim, "m109 S160"), strings(&["ok"]));
    assert_eq!(temps(&mut sim).hotend_target_c, 160.0);
}

#[test]
fn waiting_for_a_temperature_takes_the_same_target_as_setting_it() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    assert_eq!(say(&mut sim, "M109 S210"), strings(&["ok"]));
    assert_eq!(say(&mut sim, "M190 S70"), strings(&["ok"]));
    let t = temps(&mut sim);
    assert_eq!((t.hotend_target_c, t.bed_target_c), (210.0, 70.0));
}

#[test]
fn a_target_with_no_s_is_zero() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    say(&mut sim, "M104 S200");
    assert_eq!(say(&mut sim, "M104"), strings(&["ok"]));
    assert_eq!(temps(&mut sim).hotend_target_c, 0.0);
}

#[test]
fn the_heaters_rise_at_their_own_rate_and_fall_slower() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    say(&mut sim, "M104 S200");
    sim.advance(10.0);
    assert_eq!(temps(&mut sim).hotend_c, 62.0);
    say(&mut sim, "M104 S0");
    sim.advance(10.0);
    assert_eq!(temps(&mut sim).hotend_c, 56.0);
}

#[test]
fn a_heater_never_cools_below_the_room_and_never_passes_its_target() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    say(&mut sim, "M104 S30");
    sim.advance(1000.0);
    assert_eq!(temps(&mut sim).hotend_c, 30.0);
    say(&mut sim, "M104 S0");
    sim.advance(1000.0);
    assert_eq!(temps(&mut sim).hotend_c, 22.0);
}

#[test]
fn time_that_does_not_go_forward_changes_nothing() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    say(&mut sim, "M104 S200");
    for dt in [0.0, -5.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        sim.advance(dt);
        assert_eq!(temps(&mut sim).hotend_c, 22.0, "dt {dt}");
    }
}

#[test]
fn a_numbered_line_with_a_wrong_checksum_asks_for_the_line_again() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    assert_eq!(
        say(&mut sim, "N1 M105*255"),
        strings(&["Error:checksum mismatch", "Resend: 1", "ok"])
    );
    say(&mut sim, &frame(7, "M105"));
    assert_eq!(
        say(&mut sim, "N8 M105*255"),
        strings(&["Error:checksum mismatch", "Resend: 8", "ok"])
    );
}

#[test]
fn the_last_possible_line_number_does_not_stop_the_firmware() {
    // the number to ask for again is the next one, and there is none after the biggest
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    say(&mut sim, &frame(u32::MAX, "M105"));
    let answer = say(&mut sim, "N1 M105*255");
    assert_eq!(answer[1], format!("Resend: {}", u32::MAX));
}

#[test]
fn m110_sets_the_number_the_next_line_is_asked_for_by() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    assert_eq!(say(&mut sim, "M110 N5"), strings(&["ok"]));
    let answer = say(&mut sim, "N9 M105*255");
    assert_eq!(answer[1], "Resend: 6");
    say(&mut sim, "M110 N-3");
    let answer = say(&mut sim, "N9 M105*255");
    assert_eq!(answer[1], "Resend: 1");
}

#[test]
fn an_empty_line_a_comment_and_a_word_nobody_knows_are_acknowledged() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    for line in [
        "",
        "   ",
        "; only a comment",
        "G28",
        "G1 X10 F3000",
        "M999999",
    ] {
        assert_eq!(say(&mut sim, line), strings(&["ok"]), "{line:?}");
    }
}

#[test]
fn a_file_is_written_selected_and_printed_line_by_line() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    assert_eq!(
        say(&mut sim, "M28 job.gco"),
        strings(&["Writing to file: job.gco", "ok"])
    );
    assert_eq!(say(&mut sim, "G1 X1"), strings(&["ok"]));
    assert_eq!(say(&mut sim, "G1 X2 ; with a comment"), strings(&["ok"]));
    assert_eq!(say(&mut sim, "M29"), strings(&["Done saving file.", "ok"]));
    assert_eq!(
        say(&mut sim, "M23 job.gco"),
        strings(&["File opened: job.gco Size: 12", "File selected", "ok"])
    );
    assert_eq!(say(&mut sim, "M27"), strings(&["Not SD printing", "ok"]));
    say(&mut sim, "M24");
    assert_eq!(
        say(&mut sim, "M27"),
        strings(&["SD printing byte 0/12", "ok"])
    );
    sim.advance(1.5);
    assert_eq!(
        say(&mut sim, "M27"),
        strings(&["SD printing byte 6/12", "ok"])
    );
    sim.advance(1.0);
    assert_eq!(say(&mut sim, "M27"), strings(&["Not SD printing", "ok"]));
}

#[test]
fn while_a_file_is_written_every_line_is_the_file_even_a_heater_command() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    say(&mut sim, "M28 a.gco");
    assert_eq!(say(&mut sim, "M104 S999"), strings(&["ok"]));
    assert_eq!(
        say(&mut sim, "m29 ; end"),
        strings(&["Done saving file.", "ok"])
    );
    assert_eq!(temps(&mut sim).hotend_target_c, 0.0);
}

#[test]
fn a_file_with_no_name_is_refused_and_a_file_that_is_not_there_does_not_open() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    assert_eq!(
        say(&mut sim, "M28"),
        strings(&["Error:M28 needs a file name", "ok"])
    );
    assert_eq!(
        say(&mut sim, "M23 nope.gco"),
        strings(&["open failed, File: nope.gco.", "ok"])
    );
    assert_eq!(
        say(&mut sim, "M23"),
        strings(&["open failed, File: .", "ok"])
    );
    assert_eq!(say(&mut sim, "M29"), strings(&["ok"]));
}

#[test]
fn a_print_can_be_held_and_taken_up_again() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    put_file(&mut sim, "a.gco", &["G1 X1", "G1 X2", "G1 X3"]);
    say(&mut sim, "M23 a.gco");
    say(&mut sim, "M24");
    sim.advance(1.0);
    say(&mut sim, "M25");
    sim.advance(100.0);
    assert_eq!(
        say(&mut sim, "M27"),
        strings(&["SD printing byte 6/18", "ok"])
    );
    say(&mut sim, "M24");
    sim.advance(1.0);
    assert_eq!(
        say(&mut sim, "M27"),
        strings(&["SD printing byte 12/18", "ok"])
    );
}

#[test]
fn an_aborted_print_goes_back_to_the_start() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    put_file(&mut sim, "a.gco", &["G1 X1", "G1 X2"]);
    say(&mut sim, "M23 a.gco");
    say(&mut sim, "M24");
    sim.advance(1.0);
    assert_eq!(say(&mut sim, "M524"), strings(&["ok"]));
    assert_eq!(say(&mut sim, "M27"), strings(&["Not SD printing", "ok"]));
    say(&mut sim, "M24");
    assert_eq!(
        say(&mut sim, "M27"),
        strings(&["SD printing byte 0/12", "ok"])
    );
}

#[test]
fn a_line_that_takes_no_time_finishes_the_print_at_once() {
    let mut sim = MarlinSim::new(275.0, 120.0, 0.0);
    put_file(&mut sim, "a.gco", &["G1 X1"]);
    say(&mut sim, "M23 a.gco");
    say(&mut sim, "M24");
    sim.advance(0.001);
    assert_eq!(say(&mut sim, "M27"), strings(&["Not SD printing", "ok"]));
}

#[test]
fn time_that_is_far_too_long_finishes_a_print_that_has_begun() {
    // the number of lines done is added to the lines done before: it must not overflow
    let mut sim = MarlinSim::new(275.0, 120.0, 0.01);
    let lines: Vec<String> = (0..8).map(|i| format!("G1 X{i}")).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    put_file(&mut sim, "long.gco", &refs);
    say(&mut sim, "M23 long.gco");
    say(&mut sim, "M24");
    sim.advance(0.03);
    sim.advance(1.0e300);
    assert_eq!(say(&mut sim, "M27"), strings(&["Not SD printing", "ok"]));
}

#[test]
fn an_emergency_stop_halts_everything_until_the_firmware_is_started_again() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    say(&mut sim, "M104 S200");
    assert_eq!(say(&mut sim, "M112"), strings(&["ok"]));
    assert!(sim.halted());
    assert_eq!(
        say(&mut sim, "M105"),
        strings(&["Error:Printer halted. kill() called!"])
    );
    sim.advance(100.0);
    assert_eq!(say(&mut sim, "M999"), strings(&["ok"]));
    assert!(!sim.halted());
    let t = temps(&mut sim);
    assert_eq!((t.hotend_c, t.hotend_target_c), (22.0, 0.0));
}

#[test]
fn an_emergency_stop_ends_a_print() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    put_file(&mut sim, "a.gco", &["G1 X1", "G1 X2"]);
    say(&mut sim, "M23 a.gco");
    say(&mut sim, "M24");
    say(&mut sim, "M112");
    say(&mut sim, "M999");
    assert_eq!(say(&mut sim, "M27"), strings(&["Not SD printing", "ok"]));
}

#[test]
fn a_sensor_that_is_lost_reads_minus_one_and_a_fault_that_is_not_a_word_clears_it() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    sim.set_fault("sensor_lost");
    assert_eq!(temps(&mut sim).hotend_c, -1.0);
    sim.set_fault("whatever");
    assert_eq!(temps(&mut sim).hotend_c, 22.0);
}

#[test]
fn a_thermal_runaway_heats_whatever_the_target_is_up_to_a_ceiling() {
    let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
    sim.set_fault("thermal_runaway");
    sim.advance(10.0);
    let t = temps(&mut sim);
    assert_eq!((t.hotend_c, t.hotend_target_c), (62.0, 0.0));
    sim.advance(1000.0);
    assert_eq!(temps(&mut sim).hotend_c, 450.0);
}

#[test]
fn what_the_firmware_says_about_the_temperatures_is_read_back_whatever_the_fault() {
    for fault in ["none", "sensor_lost", "thermal_runaway"] {
        let mut sim = MarlinSim::new(275.0, 120.0, 1.0);
        sim.set_fault(fault);
        say(&mut sim, "M140 S50");
        sim.advance(3.0);
        let report = say(&mut sim, "M105");
        assert_eq!(report.len(), 1, "{fault}");
        assert!(parse_temps(&report[0]).is_some(), "{fault}: {report:?}");
    }
}

// ---------------------------------------------------------------- tickets

#[test]
fn a_ticket_is_a_prefix_and_thirty_two_hexadecimal_digits() {
    let mut t = Tickets::default();
    let ticket = t.issue("session", 100).unwrap();
    assert_eq!(ticket.len(), 34);
    assert!(ticket.starts_with("t_"));
    assert!(ticket[2..].bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn two_tickets_are_not_the_same() {
    let mut t = Tickets::default();
    let a = t.issue("s", 100).unwrap();
    let b = t.issue("s", 100).unwrap();
    assert_ne!(a, b);
}

#[test]
fn a_ticket_that_was_never_issued_gives_nothing() {
    let mut t = Tickets::default();
    assert_eq!(t.redeem("t_00000000000000000000000000000000", 100), None);
    assert_eq!(t.redeem("", 100), None);
}

#[test]
fn a_ticket_is_good_until_the_second_before_it_expires() {
    let mut t = Tickets::default();
    let a = t.issue("tok", 1_000).unwrap();
    assert_eq!(
        t.redeem(&a, 1_000 + TICKET_SECONDS - 1).as_deref(),
        Some("tok")
    );
    let b = t.issue("tok", 1_000).unwrap();
    assert_eq!(t.redeem(&b, 1_000 + TICKET_SECONDS), None);
}

#[test]
fn an_expired_ticket_is_used_up_all_the_same() {
    let mut t = Tickets::default();
    let a = t.issue("tok", 1_000).unwrap();
    assert_eq!(t.redeem(&a, 5_000), None);
    assert_eq!(t.redeem(&a, 1_001), None);
}

#[test]
fn each_ticket_gives_back_the_session_it_was_made_for() {
    let mut t = Tickets::default();
    let a = t.issue("one", 10).unwrap();
    let b = t.issue("two", 10).unwrap();
    assert_eq!(t.redeem(&b, 11).as_deref(), Some("two"));
    assert_eq!(t.redeem(&a, 11).as_deref(), Some("one"));
}

// ---------------------------------------------------------------- the keys that are never shown

#[test]
fn a_key_is_hidden_with_the_same_number_of_stars_whatever_it_is() {
    assert_eq!(MASK, "********");
    for key in ["k", "a very long key with spaces ", "ключ", " "] {
        let shown = secrets::mask_settings(&json!({"connection": {"api_key": key}}));
        assert_eq!(shown["connection"]["api_key"], MASK, "{key}");
    }
}

#[test]
fn hiding_a_hidden_key_changes_nothing() {
    let once = secrets::mask_settings(&json!({"connection": {"api_key": "k", "url": "u"}}));
    assert_eq!(secrets::mask_settings(&once), once);
}

#[test]
fn settings_that_are_not_shaped_like_a_connection_are_left_alone() {
    for s in [
        json!({"connection": "text"}),
        json!({"connection": [1, 2]}),
        json!({"connection": {"api_key": 5}}),
        json!({"connection": {"api_key": null}}),
        json!(null),
        json!([1]),
        json!("x"),
    ] {
        assert_eq!(secrets::mask_settings(&s), s);
    }
}

#[test]
fn the_other_fields_of_the_settings_survive_the_hiding() {
    let s = json!({"model": "m", "limits": {"hotend": 275}, "connection": {"kind": "k", "api_key": "x"}});
    let shown = secrets::mask_settings(&s);
    assert_eq!(shown["model"], "m");
    assert_eq!(shown["limits"]["hotend"], 275);
    assert_eq!(shown["connection"]["kind"], "k");
}

#[test]
fn stars_sent_back_with_nothing_to_keep_remove_the_key() {
    let mut new = json!({"connection": {"kind": "k", "api_key": MASK}});
    secrets::keep_masked(&json!({"model": "m"}), &mut new);
    assert!(new["connection"].get("api_key").is_none());
    assert_eq!(new["connection"]["kind"], "k");
}

#[test]
fn a_key_that_is_not_the_stars_is_never_replaced() {
    let old = json!({"connection": {"api_key": "old"}});
    for sent in ["", "new", "*******", "*********"] {
        let mut new = json!({"connection": {"api_key": sent}});
        secrets::keep_masked(&old, &mut new);
        assert_eq!(new["connection"]["api_key"], sent);
    }
}

#[test]
fn stars_in_a_shape_that_is_not_a_connection_do_not_stop_the_merge() {
    let old = json!({"connection": {"api_key": "old"}});
    for mut new in [
        json!({"connection": "text"}),
        json!({"connection": MASK}),
        json!(5),
        json!({}),
    ] {
        let before = new.clone();
        secrets::keep_masked(&old, &mut new);
        assert_eq!(new, before);
    }
}

#[test]
fn the_key_that_was_kept_goes_back_into_the_new_settings_untouched() {
    let old = json!({"connection": {"api_key": "pa ss\"word\u{e9}"}});
    let mut new = json!({"connection": {"api_key": MASK, "url": "http://z"}});
    secrets::keep_masked(&old, &mut new);
    assert_eq!(new["connection"]["api_key"], "pa ss\"word\u{e9}");
    assert_eq!(new["connection"]["url"], "http://z");
}

// ---------------------------------------------------------------- the node of the head

#[test]
fn an_address_gets_the_port_of_the_web_unless_it_has_one() {
    assert_eq!(head::host_port("head.local").unwrap(), "head.local:80");
    assert_eq!(
        head::host_port("head.local:8080").unwrap(),
        "head.local:8080"
    );
    assert_eq!(head::host_port("  10.0.0.5  ").unwrap(), "10.0.0.5:80");
    assert_eq!(head::host_port("head\n").unwrap(), "head:80");
}

#[test]
fn an_address_that_is_not_a_host_is_refused() {
    for bad in ["", "   ", "a b", "http://x", "x/y", "x/"] {
        assert_eq!(head::host_port(bad), Err(HeadError::NoAddress), "{bad:?}");
    }
}

#[test]
fn an_address_with_a_line_break_or_another_control_character_is_refused() {
    for bad in ["head\r\nX-Evil: 1", "a\tb", "a\u{0}b", "a\u{7f}b", "a\nb"] {
        assert_eq!(head::host_port(bad), Err(HeadError::NoAddress), "{bad:?}");
    }
}

fn request(method: &str, path: &str, host: &str, token: &str) -> String {
    String::from_utf8(head::request_bytes(method, path, host, token)).unwrap()
}

#[test]
fn a_get_has_a_host_and_no_body_and_ends_with_the_empty_line() {
    let r = request("GET", "/status", "h:80", "");
    assert_eq!(
        r,
        "GET /status HTTP/1.0\r\nHost: h:80\r\nAccept: */*\r\n\r\n"
    );
}

#[test]
fn only_a_post_says_it_has_no_body() {
    assert!(request("POST", "/m", "h:80", "").contains("Content-Length: 0\r\n"));
    for method in ["GET", "PUT", "DELETE", "HEAD"] {
        assert!(!request(method, "/m", "h:80", "").contains("Content-Length"));
    }
}

#[test]
fn a_path_cannot_begin_a_new_header() {
    let r = request("GET", "/a\r\nX-Evil: 1", "h:80", "");
    assert!(!r.contains("\r\nX-Evil"), "{r:?}");
    assert_eq!(r.matches("\r\n\r\n").count(), 1);
    assert!(r.ends_with("\r\n\r\n"));
}

#[test]
fn a_token_cannot_begin_a_new_header() {
    let r = request("GET", "/a", "h:80", "tok\r\nX-Evil: 1");
    assert!(!r.contains("\r\nX-Evil"), "{r:?}");
    assert!(r.contains("Authorization: Bearer tokX-Evil: 1\r\n"));
    assert_eq!(r.matches("\r\n\r\n").count(), 1);
}

#[test]
fn a_host_and_a_method_cannot_begin_a_new_header_either() {
    let r = request("GET\r\nX-Evil: 1", "/a", "h:80\r\nX-Evil: 2", "");
    assert!(!r.contains("\r\nX-Evil"), "{r:?}");
    assert_eq!(r.matches("\r\n\r\n").count(), 1);
}

#[test]
fn a_header_is_not_complete_until_the_empty_line() {
    assert!(head::parse_head(b"").is_none());
    assert!(head::parse_head(b"HTTP/1.0 200 OK\r\n").is_none());
    assert!(head::parse_head(b"HTTP/1.0 200 OK\r\n\r").is_none());
    assert!(head::parse_head(b"\r\n\r\n").is_none());
}

#[test]
fn the_status_and_the_start_of_the_body_are_found() {
    let buf = b"HTTP/1.1 204 No Content\r\nX: y\r\n\r\nrest";
    let (status, content_type, start) = head::parse_head(buf).unwrap();
    assert_eq!((status, content_type.as_str()), (204, ""));
    assert_eq!(&buf[start..], b"rest");
}

#[test]
fn the_content_type_is_found_in_any_letter_case_and_trimmed() {
    let buf = b"HTTP/1.0 200 OK\r\nno colon here\r\nCONTENT-type:   text/plain  \r\n\r\n";
    let (_, content_type, start) = head::parse_head(buf).unwrap();
    assert_eq!(content_type, "text/plain");
    assert_eq!(start, buf.len());
}

#[test]
fn the_first_content_type_counts() {
    let buf = b"HTTP/1.0 200 OK\r\nContent-Type: a/b\r\nContent-Type: c/d\r\n\r\n";
    assert_eq!(head::parse_head(buf).unwrap().1, "a/b");
}

#[test]
fn a_status_that_is_not_a_number_of_the_range_is_not_an_answer() {
    for line in [
        "HTTP/1.0 abc",
        "HTTP/1.0 99999",
        "HTTP/1.0 -1",
        "HTTP/1.0",
        "",
    ] {
        let buf = format!("{line}\r\n\r\n");
        assert!(head::parse_head(buf.as_bytes()).is_none(), "{line:?}");
    }
}

#[test]
fn bytes_that_are_not_text_in_the_header_do_not_stop_the_reader() {
    let buf = b"HTTP/1.0 200 \xff\xfe\r\nX-\xff: \xfe\r\n\r\n";
    let (status, _, start) = head::parse_head(buf).unwrap();
    assert_eq!(status, 200);
    assert_eq!(start, buf.len());
}

#[test]
fn any_bytes_at_all_are_read_without_a_panic() {
    for seed in 1..300u32 {
        let mut buf = lcg(seed, (seed as usize * 7) % 120, 256);
        if seed.is_multiple_of(3) {
            buf.extend_from_slice(b"\r\n\r\n");
        }
        if seed.is_multiple_of(5) {
            let mut with_status = b"HTTP/1.0 200 OK\r\n".to_vec();
            with_status.extend_from_slice(&buf);
            buf = with_status;
        }
        let _ = head::parse_head(&buf);
    }
}

// ---------------------------------------------------------------- the ring of lights

#[test]
fn a_query_for_the_ring_is_short_and_made_of_plain_signs() {
    assert!(inspect::light_query_ok(
        "on=1&level=70&color=ff8800&cue_ms=3000"
    ));
    assert!(inspect::light_query_ok("a=b,c.d-e_f#1"));
    assert!(inspect::light_query_ok(&"a".repeat(200)));
    assert!(!inspect::light_query_ok(&"a".repeat(201)));
}

#[test]
fn a_query_with_a_sign_that_can_do_harm_is_refused() {
    for bad in [
        "a b", "a%20b", "a+b", "a/b", "a?b", "a;b", "a\"b", "a'b", "a\\b", "a\nb", "a\u{0}b",
        "a:b", "é", "日本",
    ] {
        assert!(!inspect::light_query_ok(bad), "{bad:?}");
    }
}

// ---------------------------------------------------------------- the audit

#[test]
fn a_plain_value_goes_as_it_is_and_the_others_are_quoted() {
    assert_eq!(audit::value_text("abc"), "abc");
    assert_eq!(audit::value_text("a.b_c:d/e@f-g"), "a.b_c:d/e@f-g");
    assert_eq!(audit::value_text(""), "\"\"");
    assert_eq!(audit::value_text("a b"), "\"a b\"");
    assert_eq!(audit::value_text("a=b"), "\"a=b\"");
    assert_eq!(audit::value_text("a+b"), "\"a+b\"");
}

#[test]
fn a_value_with_a_line_break_or_a_quote_cannot_split_the_line() {
    assert_eq!(audit::value_text("a\nb"), "\"a\\nb\"");
    assert_eq!(audit::value_text("a\r\nevent=x"), "\"a\\r\\nevent=x\"");
    assert_eq!(audit::value_text("a\"b"), "\"a\\\"b\"");
    assert!(!audit::value_text("x\u{2028}y").contains('\n'));
}

#[test]
fn a_value_that_is_not_ascii_is_quoted_and_kept_as_it_is() {
    assert_eq!(audit::value_text("é"), "\"é\"");
    assert_eq!(audit::value_text("日本"), "\"日本\"");
}

#[test]
fn the_fields_of_a_line_are_joined_by_one_space() {
    assert_eq!(audit::pairs(&[]), "");
    assert_eq!(audit::pairs(&[("a", "1")]), "a=1");
    assert_eq!(
        audit::pairs(&[("a", "1"), ("b", "x y"), ("c", "")]),
        "a=1 b=\"x y\" c=\"\""
    );
}

#[test]
fn a_line_begins_with_its_event() {
    assert_eq!(audit::line("login", &[]), "event=login");
    assert_eq!(
        audit::line("login", &[("who", "ann"), ("ip", "10.0.0.1")]),
        "event=login who=ann ip=10.0.0.1"
    );
    assert_eq!(audit::line("two words", &[]), "event=\"two words\"");
}

#[test]
fn the_date_of_a_count_of_seconds_is_the_one_of_the_calendar() {
    let cases: [(u64, &str); 8] = [
        (0, "1970-01-01T00:00:00Z"),
        (86_399, "1970-01-01T23:59:59Z"),
        (86_400, "1970-01-02T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (1_000_000_000, "2001-09-09T01:46:40Z"),
        (1_700_000_000, "2023-11-14T22:13:20Z"),
        (1_709_164_800, "2024-02-29T00:00:00Z"),
        (4_102_444_800, "2100-01-01T00:00:00Z"),
    ];
    for (secs, want) in cases {
        assert_eq!(audit::iso(secs), want, "{secs}");
    }
}

#[test]
fn the_year_2100_has_no_leap_day_and_the_year_2000_has() {
    assert_eq!(audit::iso(4_107_542_400), "2100-03-01T00:00:00Z");
    assert_eq!(audit::iso(951_868_800), "2000-03-01T00:00:00Z");
}

#[test]
fn the_last_second_a_number_can_hold_gives_a_date_and_not_a_panic() {
    let text = audit::iso(u64::MAX);
    assert!(text.ends_with('Z') && text.contains('T'), "{text}");
}

#[test]
fn the_days_of_a_century_follow_each_other_without_a_gap() {
    let mut last = audit::iso(0);
    for day in 1..400u64 {
        let now = audit::iso(day * 86_400);
        assert!(now > last, "{last} then {now}");
        assert!(now.ends_with("T00:00:00Z"));
        last = now;
    }
}

fn write_lines(path: &std::path::Path, lines: &[&str]) {
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn everything() -> Filter {
    Filter {
        since: None,
        until: None,
        who: None,
        event: None,
        limit: 100,
    }
}

fn ats(lines: &[serde_json::Value]) -> Vec<u64> {
    lines.iter().map(|l| l["at"].as_u64().unwrap()).collect()
}

const AUDIT_LINES: [&str; 5] = [
    r#"{"t":10,"event":"login","who":"a"}"#,
    r#"{"t":20,"event":"logout","who":"b"}"#,
    "this is not json",
    "[1,2,3]",
    r#"{"t":30,"event":"login","who":"b"}"#,
];

#[test]
fn the_audit_comes_newest_first_and_skips_what_does_not_read() {
    let dir = scratch("audit-order");
    let path = dir.join("audit.jsonl");
    write_lines(&path, &AUDIT_LINES);
    let found = audit::read_lines(&path, &everything());
    assert_eq!(ats(&found), [30, 20, 10]);
    assert!(found[0].get("t").is_none());
    assert_eq!(found[0]["event"], "login");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_audit_is_filtered_by_time_person_and_event() {
    let dir = scratch("audit-filter");
    let path = dir.join("audit.jsonl");
    write_lines(&path, &AUDIT_LINES);
    let f = |change: &dyn Fn(&mut Filter)| {
        let mut filter = everything();
        change(&mut filter);
        ats(&audit::read_lines(&path, &filter))
    };
    assert_eq!(f(&|x| x.since = Some(15)), [30, 20]);
    assert_eq!(f(&|x| x.since = Some(20)), [30, 20]);
    assert_eq!(f(&|x| x.until = Some(20)), [20, 10]);
    assert_eq!(f(&|x| x.until = Some(9)), Vec::<u64>::new());
    assert_eq!(f(&|x| x.who = Some("b".into())), [30, 20]);
    assert_eq!(f(&|x| x.event = Some("login".into())), [30, 10]);
    assert_eq!(
        f(&|x| {
            x.who = Some("b".into());
            x.event = Some("login".into())
        }),
        [30]
    );
    assert_eq!(f(&|x| x.who = Some("nobody".into())), Vec::<u64>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_audit_stops_at_the_limit_and_a_limit_of_zero_gives_nothing() {
    let dir = scratch("audit-limit");
    let path = dir.join("audit.jsonl");
    write_lines(&path, &AUDIT_LINES);
    let mut filter = everything();
    filter.limit = 1;
    assert_eq!(ats(&audit::read_lines(&path, &filter)), [30]);
    filter.limit = 0;
    assert!(audit::read_lines(&path, &filter).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_older_copy_of_the_audit_is_read_first_and_so_comes_last() {
    let dir = scratch("audit-older");
    let path = dir.join("audit.jsonl");
    write_lines(&path, &AUDIT_LINES);
    write_lines(
        &dir.join("audit.jsonl.1"),
        &[r#"{"t":5,"event":"boot","who":"s"}"#],
    );
    assert_eq!(
        ats(&audit::read_lines(&path, &everything())),
        [30, 20, 10, 5]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_line_with_no_time_is_at_zero_and_a_missing_file_is_empty() {
    let dir = scratch("audit-notime");
    let path = dir.join("audit.jsonl");
    assert!(audit::read_lines(&path, &everything()).is_empty());
    write_lines(&path, &[r#"{"event":"x"}"#]);
    assert_eq!(ats(&audit::read_lines(&path, &everything())), [0]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_log_that_is_off_writes_nothing_and_one_that_is_on_adds_a_line_each_time() {
    let dir = scratch("logfile");
    let off = LogFile::new(dir.join("off.log"), false, 1);
    off.append("lost");
    assert!(!off.path().exists());
    let on = LogFile::new(dir.join("sub").join("on.log"), true, 64);
    on.append("one");
    on.append("two");
    let text = std::fs::read_to_string(on.path()).unwrap();
    assert_eq!(text, "one\ntwo\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_log_that_is_full_is_set_aside_and_goes_on_in_a_new_file() {
    let dir = scratch("logfile-full");
    let log = LogFile::new(dir.join("x.log"), true, 1);
    let line = "x".repeat(400);
    for _ in 0..3 {
        log.append(&line);
    }
    let older = std::fs::read_to_string(dir.join("x.log.1")).unwrap();
    let now = std::fs::read_to_string(log.path()).unwrap();
    assert_eq!(older.lines().count(), 2);
    assert_eq!(now.lines().count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the spindle

fn limits() -> spindle::Limits {
    spindle::Limits {
        min_rpm: 5_000,
        max_rpm: 24_000,
        power_w: 2_200,
        max_xy_mm_min: 3_000,
        max_z_mm_min: 1_200,
        max_bit_mm: 6.0,
    }
}

fn guard(mode: Mode) -> Guard {
    Guard {
        limits: limits(),
        mode,
    }
}

fn codes(found: &[Finding]) -> Vec<&str> {
    found.iter().map(|f| f.code.as_str()).collect()
}

#[test]
fn the_word_of_a_mode_is_read_back_and_an_empty_word_is_the_careful_one() {
    assert_eq!(Mode::parse(""), Some(Mode::Reject));
    assert_eq!(Mode::parse("   "), Some(Mode::Reject));
    assert_eq!(Mode::parse("reject"), Some(Mode::Reject));
    assert_eq!(Mode::parse(" warn "), Some(Mode::Warn));
    for mode in [Mode::Reject, Mode::Warn] {
        assert_eq!(Mode::parse(mode.word()), Some(mode));
    }
}

#[test]
fn a_word_that_is_not_a_mode_is_not_one() {
    for bad in ["WARN", "Reject", "off", "warn!", "re ject", "ignore"] {
        assert_eq!(Mode::parse(bad), None, "{bad}");
    }
}

#[test]
fn no_change_keeps_the_starting_limits_and_an_unknown_spindle_has_none() {
    for id in spindle::IDS {
        assert_eq!(Override::default().over(id), spindle::defaults(id));
    }
    assert_eq!(Override::default().over("steam"), None);
    assert_eq!(Override::default().over(""), None);
}

#[test]
fn each_member_of_a_change_replaces_only_its_own_limit() {
    let base = spindle::defaults("original").unwrap();
    let change = Override {
        min_rpm: Some(1),
        max_rpm: Some(2),
        power_w: Some(3),
        max_xy_mm_min: Some(4),
        max_z_mm_min: Some(5),
        max_bit_mm: Some(6.5),
    };
    let l = change.over("original").unwrap();
    assert_eq!(
        (
            l.min_rpm,
            l.max_rpm,
            l.power_w,
            l.max_xy_mm_min,
            l.max_z_mm_min
        ),
        (1, 2, 3, 4, 5)
    );
    assert_eq!(l.max_bit_mm, 6.5);
    let only_max = Override {
        max_rpm: Some(10),
        ..Override::default()
    };
    let l = only_max.over("original").unwrap();
    assert_eq!(l.max_rpm, 10);
    assert_eq!((l.min_rpm, l.power_w), (base.min_rpm, base.power_w));
}

#[test]
fn a_change_with_nothing_in_it_is_an_empty_document() {
    assert_eq!(serde_json::to_string(&Override::default()).unwrap(), "{}");
    let back: Override = serde_json::from_str("{}").unwrap();
    assert_eq!(back, Override::default());
    let some: Override = serde_json::from_str(r#"{"max_bit_mm": 2.5}"#).unwrap();
    assert_eq!(some.max_bit_mm, Some(2.5));
}

#[test]
fn a_change_with_a_limit_that_is_not_a_whole_positive_number_does_not_read() {
    for bad in [
        r#"{"max_rpm": -1}"#,
        r#"{"max_rpm": 1.5}"#,
        r#"{"max_rpm": "fast"}"#,
        r#"{"max_rpm": 4294967296}"#,
        r#"{"max_bit_mm": "3"}"#,
    ] {
        assert!(serde_json::from_str::<Override>(bad).is_err(), "{bad}");
    }
}

#[test]
fn turns_inside_the_limits_are_no_excess_and_zero_turns_are_not_judged() {
    let g = guard(Mode::Reject);
    for rpm in [0, 5_000, 12_000, 24_000] {
        assert!(g.check(1, rpm, None, None).is_empty(), "{rpm}");
    }
}

#[test]
fn turns_one_past_a_limit_are_an_excess() {
    let g = guard(Mode::Reject);
    let above = g.check(2, 24_001, None, None);
    assert_eq!(codes(&above), ["rpm_above_max"]);
    assert_eq!(
        (above[0].tool, above[0].value, above[0].limit),
        (2, 24_001, 24_000)
    );
    let below = g.check(3, 4_999, None, None);
    assert_eq!(codes(&below), ["rpm_below_min"]);
    assert_eq!(
        (below[0].tool, below[0].value, below[0].limit),
        (3, 4_999, 5_000)
    );
    assert_eq!(codes(&g.check(1, 1, None, None)), ["rpm_below_min"]);
}

#[test]
fn the_biggest_number_of_turns_is_an_excess_and_not_a_panic() {
    let g = guard(Mode::Reject);
    assert_eq!(
        codes(&g.check(1, u32::MAX, Some(u32::MAX), Some(u32::MAX))).len(),
        3
    );
}

#[test]
fn a_feed_at_the_limit_is_fine_and_one_past_it_is_an_excess() {
    let g = guard(Mode::Reject);
    assert!(g.check(1, 0, Some(3_000), Some(1_200)).is_empty());
    assert_eq!(
        codes(&g.check(1, 0, Some(3_001), None)),
        ["feed_xy_above_max"]
    );
    assert_eq!(
        codes(&g.check(1, 0, None, Some(1_201))),
        ["feed_z_above_max"]
    );
    assert!(g.check(1, 0, Some(0), Some(0)).is_empty());
}

#[test]
fn every_excess_of_a_tool_is_listed_turns_first_then_the_feeds() {
    let g = guard(Mode::Reject);
    let found = g.check(9, 30_000, Some(5_000), Some(2_000));
    assert_eq!(
        codes(&found),
        ["rpm_above_max", "feed_xy_above_max", "feed_z_above_max"]
    );
    assert!(found.iter().all(|f| f.tool == 9));
}

#[test]
fn in_the_careful_mode_the_turns_go_as_asked_and_in_the_other_they_stop_at_the_top() {
    assert_eq!(guard(Mode::Reject).turns(30_000), 30_000);
    assert_eq!(guard(Mode::Warn).turns(30_000), 24_000);
    assert_eq!(guard(Mode::Warn).turns(24_000), 24_000);
    assert_eq!(guard(Mode::Warn).turns(100), 100);
    assert_eq!(guard(Mode::Warn).turns(0), 0);
    assert_eq!(guard(Mode::Warn).turns(u32::MAX), 24_000);
}

#[test]
fn the_sentence_of_a_refusal_names_the_tool_the_value_and_the_limit() {
    let found = [Finding {
        code: "rpm_above_max".into(),
        tool: 1,
        value: 70_000,
        limit: 63_000,
    }];
    assert_eq!(
        spindle::describe(&found),
        "tool 1: turns above the top of the spindle (70000 against 63000)"
    );
}

#[test]
fn the_sentences_of_several_excesses_are_joined_and_none_gives_nothing() {
    assert_eq!(spindle::describe(&[]), "");
    let g = guard(Mode::Reject);
    let text = spindle::describe(&g.check(4, 30_000, Some(9_000), Some(9_000)));
    assert_eq!(text.matches("; ").count(), 2);
    assert!(text.contains("feed along the cut"));
    assert!(text.contains("plunge feed"));
}

#[test]
fn a_finding_goes_through_json_and_back() {
    let f = Finding {
        code: "feed_z_above_max".into(),
        tool: 3,
        value: 1_300,
        limit: 1_200,
    };
    let back: Finding = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
    assert_eq!(back, f);
}

// ---------------------------------------------------------------- binary G-code

fn program(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("G1 X{i}.5 Y{} E0.{i}\n", i * 2))
        .collect()
}

fn meta(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn plain(crc32: bool) -> WriteOptions {
    WriteOptions {
        crc32,
        metadata: Compression::None,
        gcode: Compression::None,
        gcode_block: 64 * 1024,
    }
}

fn header(version: u32, checksum_kind: u16) -> Vec<u8> {
    let mut h = bgcode::MAGIC.to_vec();
    h.extend_from_slice(&version.to_le_bytes());
    h.extend_from_slice(&checksum_kind.to_le_bytes());
    h
}

fn read(bytes: &[u8]) -> Result<Bgcode, bgcode::BgError> {
    bgcode::read(bytes, &Limits::default())
}

#[test]
fn the_constants_of_the_format_are_the_ones_of_the_specification() {
    assert_eq!(bgcode::MAGIC, b"GCDE");
    assert_eq!(bgcode::DEFAULT_MAX_BLOCK, 64 * 1024 * 1024);
    let d = Limits::default();
    assert_eq!(
        (d.max_block, d.max_gcode),
        (64 * 1024 * 1024, 64 * 1024 * 1024)
    );
}

#[test]
fn the_words_of_the_compressions_are_the_ones_the_answers_use() {
    assert_eq!(Compression::None.name(), "none");
    assert_eq!(Compression::Deflate.name(), "deflate");
    assert_eq!(Compression::Heatshrink11.name(), "heatshrink_11_4");
    assert_eq!(Compression::Heatshrink12.name(), "heatshrink_12_4");
}

#[test]
fn a_file_with_nothing_in_it_is_a_header_and_reads_back_empty() {
    let bytes = bgcode::write(&Bgcode::default(), &WriteOptions::default()).unwrap();
    assert_eq!(bytes, [b'G', b'C', b'D', b'E', 1, 0, 0, 0, 1, 0]);
    let back = read(&bytes).unwrap();
    assert!(back.crc32 && back.gcode.is_empty() && back.compressions.is_empty());
    let bytes = bgcode::write(&Bgcode::default(), &plain(false)).unwrap();
    assert_eq!(&bytes[8..], [0, 0]);
    assert!(!read(&bytes).unwrap().crc32);
}

#[test]
fn the_program_comes_back_the_same_with_every_compression_and_with_or_without_the_checksum() {
    let text = program(200);
    for compression in [
        Compression::None,
        Compression::Deflate,
        Compression::Heatshrink11,
        Compression::Heatshrink12,
    ] {
        for crc32 in [false, true] {
            let file = Bgcode {
                gcode: text.clone(),
                ..Bgcode::default()
            };
            let options = WriteOptions {
                crc32,
                metadata: Compression::None,
                gcode: compression,
                gcode_block: 64 * 1024,
            };
            let bytes = bgcode::write(&file, &options).unwrap();
            let back = read(&bytes).unwrap();
            assert_eq!(back.gcode, text, "{compression:?} {crc32}");
            assert_eq!(back.crc32, crc32);
            assert_eq!(back.compressions, [("G-code".to_string(), compression)]);
        }
    }
}

#[test]
fn every_part_of_a_file_comes_back_in_the_order_it_had() {
    let file = Bgcode {
        crc32: true,
        file_meta: meta(&[("Producer", "tests"), ("b", "2")]),
        printer_meta: meta(&[("printer_model", "MK4"), ("a", "1")]),
        thumbnails: vec![
            Thumbnail {
                format: 0,
                width: 16,
                height: 8,
                data: vec![1, 2, 3],
            },
            Thumbnail {
                format: 2,
                width: u16::MAX,
                height: 1,
                data: vec![],
            },
        ],
        print_meta: meta(&[("time", "10")]),
        slicer_meta: meta(&[("layer_height", "0.2")]),
        gcode: program(5),
        compressions: Vec::new(),
    };
    let back = read(&bgcode::write(&file, &WriteOptions::default()).unwrap()).unwrap();
    assert_eq!(back.file_meta, file.file_meta);
    assert_eq!(back.printer_meta, file.printer_meta);
    assert_eq!(back.thumbnails, file.thumbnails);
    assert_eq!(back.print_meta, file.print_meta);
    assert_eq!(back.slicer_meta, file.slicer_meta);
    assert_eq!(back.gcode, file.gcode);
    let names: Vec<&str> = back.compressions.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "file metadata",
            "printer metadata",
            "thumbnail",
            "thumbnail",
            "print metadata",
            "slicer metadata",
            "G-code"
        ]
    );
}

#[test]
fn a_long_program_is_written_in_blocks_that_end_at_a_line_break() {
    let text = program(400);
    let file = Bgcode {
        gcode: text.clone(),
        ..Bgcode::default()
    };
    let options = WriteOptions {
        gcode_block: 100,
        ..plain(true)
    };
    let back = read(&bgcode::write(&file, &options).unwrap()).unwrap();
    assert_eq!(back.gcode, text);
    assert!(back.compressions.len() > 10);
}

#[test]
fn a_line_longer_than_the_block_is_a_block_of_its_own() {
    let text = format!("{}\nG1\n", "X".repeat(500));
    let file = Bgcode {
        gcode: text.clone(),
        ..Bgcode::default()
    };
    let options = WriteOptions {
        gcode_block: 50,
        ..plain(false)
    };
    let back = read(&bgcode::write(&file, &options).unwrap()).unwrap();
    assert_eq!(back.gcode, text);
    assert_eq!(back.compressions.len(), 2);
}

#[test]
fn a_limit_of_a_block_that_falls_inside_a_letter_does_not_cut_the_letter() {
    // the comments of a program may have letters of two or three bytes
    for text in ["é\n".repeat(50), "é;é;é".to_string(), "日本語\n".repeat(20)] {
        for block in [0usize, 1, 2, 3, 4, 5, 7] {
            let file = Bgcode {
                gcode: text.clone(),
                ..Bgcode::default()
            };
            let options = WriteOptions {
                gcode_block: block,
                ..plain(true)
            };
            let bytes = bgcode::write(&file, &options).unwrap();
            assert_eq!(read(&bytes).unwrap().gcode, text, "block {block}");
        }
    }
}

#[test]
fn a_program_with_no_text_has_no_block() {
    let bytes = bgcode::write(&Bgcode::default(), &plain(true)).unwrap();
    assert_eq!(bytes.len(), 10);
}

#[test]
fn the_checksum_adds_four_bytes_to_each_block() {
    let file = Bgcode {
        gcode: program(3),
        file_meta: meta(&[("a", "b")]),
        ..Bgcode::default()
    };
    let without = bgcode::write(&file, &plain(false)).unwrap();
    let with = bgcode::write(&file, &plain(true)).unwrap();
    assert_eq!(with.len(), without.len() + 8);
}

#[test]
fn a_value_that_would_break_the_metadata_is_refused_when_writing() {
    for pairs in [
        meta(&[("a", "x\ny")]),
        meta(&[("a", "x\ry")]),
        meta(&[("a\nb", "x")]),
        meta(&[("a=b", "x")]),
        meta(&[("", "x")]),
    ] {
        for which in 0..4 {
            let mut file = Bgcode::default();
            match which {
                0 => file.file_meta = pairs.clone(),
                1 => file.printer_meta = pairs.clone(),
                2 => file.print_meta = pairs.clone(),
                _ => file.slicer_meta = pairs.clone(),
            }
            let err = bgcode::write(&file, &WriteOptions::default()).unwrap_err();
            assert_eq!(err.code, "bad_bgcode", "{pairs:?}");
        }
    }
}

#[test]
fn a_picture_of_a_format_that_does_not_exist_is_refused_when_writing() {
    let file = Bgcode {
        thumbnails: vec![Thumbnail {
            format: 3,
            width: 1,
            height: 1,
            data: vec![0],
        }],
        ..Bgcode::default()
    };
    assert_eq!(
        bgcode::write(&file, &WriteOptions::default())
            .unwrap_err()
            .code,
        "bad_bgcode"
    );
}

#[test]
fn a_value_with_blanks_at_its_ends_is_read_without_them() {
    let mut bytes = header(1, 0);
    let text = b"k = v\n\nz=\n";
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(text);
    let back = read(&bytes).unwrap();
    assert_eq!(back.file_meta, meta(&[("k", "v"), ("z", "")]));
}

#[test]
fn a_header_that_is_short_or_wrong_is_refused() {
    for len in 0..10 {
        let bytes = header(1, 0);
        assert_eq!(read(&bytes[..len]).unwrap_err().code, "bad_bgcode", "{len}");
    }
    let mut wrong = header(1, 0);
    wrong[0] = b'X';
    assert_eq!(read(&wrong).unwrap_err().code, "bad_bgcode");
    assert_eq!(read(&header(1, 2)).unwrap_err().code, "bad_bgcode");
    assert_eq!(read(&header(1, u16::MAX)).unwrap_err().code, "bad_bgcode");
}

#[test]
fn a_version_that_is_not_the_first_is_not_supported() {
    for version in [0u32, 2, 3, u32::MAX] {
        assert_eq!(
            read(&header(version, 0)).unwrap_err().code,
            "bgcode_unsupported",
            "{version}"
        );
    }
}

#[test]
fn a_block_of_a_type_that_does_not_exist_is_refused_by_its_number() {
    let mut bytes = header(1, 0);
    bytes.extend_from_slice(&[9, 0]);
    let err = read(&bytes).unwrap_err();
    assert_eq!(err.code, "bad_bgcode");
    assert!(err.detail.contains("(9)"), "{}", err.detail);
}

#[test]
fn a_block_that_is_cut_in_its_header_is_refused() {
    for len in 1..8 {
        let mut bytes = header(1, 0);
        bytes.extend_from_slice(&[1, 0, 0, 0, 5, 0, 0, 0][..len]);
        assert_eq!(read(&bytes).unwrap_err().code, "bad_bgcode", "{len}");
    }
}

#[test]
fn the_packed_text_of_meatpack_is_not_supported_and_another_encoding_is_not_valid() {
    let mut bytes = header(1, 0);
    bytes.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
    assert_eq!(read(&bytes).unwrap_err().code, "bgcode_unsupported");
    let mut bytes = header(1, 0);
    bytes.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 3, 0]);
    assert_eq!(read(&bytes).unwrap_err().code, "bad_bgcode");
}

#[test]
fn text_that_is_not_text_is_refused_in_the_program_and_in_the_metadata() {
    let mut gcode = header(1, 0);
    gcode.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0xFF]);
    assert!(read(&gcode).unwrap_err().detail.contains("not text"));
    let mut meta_block = header(1, 0);
    meta_block.extend_from_slice(&[0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0xFF, 0xFE]);
    assert!(read(&meta_block).unwrap_err().detail.contains("not text"));
}

#[test]
fn a_line_of_metadata_with_no_equal_sign_is_refused() {
    let mut bytes = header(1, 0);
    bytes.extend_from_slice(&[0, 0, 0, 0, 7, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(b"novalue");
    assert!(read(&bytes).unwrap_err().detail.contains("no ="));
}

#[test]
fn blocks_out_of_order_or_repeated_are_refused() {
    let a = bgcode::write(
        &Bgcode {
            gcode: "G1\n".into(),
            ..Bgcode::default()
        },
        &plain(false),
    )
    .unwrap();
    let b = bgcode::write(
        &Bgcode {
            file_meta: meta(&[("a", "b")]),
            ..Bgcode::default()
        },
        &plain(false),
    )
    .unwrap();
    let mut late = a.clone();
    late.extend_from_slice(&b[10..]);
    assert!(read(&late).unwrap_err().detail.contains("out of order"));
    let mut twice = b.clone();
    twice.extend_from_slice(&b[10..]);
    assert!(read(&twice)
        .unwrap_err()
        .detail
        .contains("two file metadata blocks"));
}

#[test]
fn a_byte_changed_inside_a_block_is_caught_by_its_checksum() {
    let file = Bgcode {
        gcode: program(10),
        ..Bgcode::default()
    };
    let mut bytes = bgcode::write(&file, &plain(true)).unwrap();
    let at = bytes.len() - 5;
    bytes[at] ^= 0x01;
    let err = read(&bytes).unwrap_err();
    assert_eq!(err.code, "bad_bgcode");
    assert!(err.detail.contains("checksum"), "{}", err.detail);
}

#[test]
fn a_file_cut_anywhere_is_never_read_as_a_whole_and_never_stops_the_reader() {
    let file = Bgcode {
        file_meta: meta(&[("a", "b")]),
        gcode: program(30),
        ..Bgcode::default()
    };
    let bytes = bgcode::write(&file, &WriteOptions::default()).unwrap();
    for len in 0..bytes.len() {
        let _ = read(&bytes[..len]);
    }
    assert!(read(&bytes[..bytes.len() - 1]).is_err());
    assert!(read(&bytes).is_ok());
}

#[test]
fn a_block_bigger_than_the_limit_is_refused_and_so_is_too_much_program() {
    let file = Bgcode {
        gcode: program(100),
        ..Bgcode::default()
    };
    let bytes = bgcode::write(&file, &plain(true)).unwrap();
    let small_block = Limits {
        max_block: 10,
        max_gcode: 1 << 20,
    };
    let err = bgcode::read(&bytes, &small_block).unwrap_err();
    assert!(
        err.detail.contains("more than the 10 bytes"),
        "{}",
        err.detail
    );
    let small_gcode = Limits {
        max_block: 1 << 20,
        max_gcode: 10,
    };
    let err = bgcode::read(&bytes, &small_gcode).unwrap_err();
    assert!(err.detail.contains("G-code is more than"), "{}", err.detail);
    let exact = Limits {
        max_block: 1 << 20,
        max_gcode: file.gcode.len(),
    };
    assert!(bgcode::read(&bytes, &exact).is_ok());
}

#[test]
fn bytes_that_look_like_the_start_of_a_file_never_stop_the_reader() {
    let limits = Limits {
        max_block: 4096,
        max_gcode: 4096,
    };
    for seed in 1..300u32 {
        let mut bytes = header(1, (seed % 2) as u16);
        bytes.extend_from_slice(&lcg(seed, (seed as usize * 3) % 160, 8));
        let _ = bgcode::read(&bytes, &limits);
        let mut noisy = header(1, (seed % 2) as u16);
        noisy.extend_from_slice(&lcg(seed + 1000, (seed as usize * 5) % 160, 256));
        let _ = bgcode::read(&noisy, &limits);
    }
}
