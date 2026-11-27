//! What a G-code program must not get past: the server refuses, with the reason, whatever the controller or the printer would read differently from the way the server read it.
//! Everything here is the reading of text; no machine is involved.

use eh3d_server::cam::{CamConfig, CamEngine};
use eh3d_server::gcode::{self, Entry};
use eh3d_server::laser::{self, LaserConfig, Limits};
use eh3d_server::printer3d::{scan_program, PrinterConfig};
use serde_json::json;

/// A GRBL job of this text is refused as `bad_gcode`, and the reason holds `why`.
fn refused(text: &str, why: &str) {
    match gcode::parse(text, 0, false) {
        Ok(p) => panic!("{text:?} was accepted: {:?}", p.entries),
        Err(e) => {
            assert_eq!(e.code, "bad_gcode", "{text:?}: {}", e.detail);
            assert!(e.detail.contains(why), "{text:?}: {}", e.detail);
        }
    }
}

fn entry(text: &str) -> Entry {
    Entry {
        raw: 0,
        text: text.into(),
    }
}

/// The rules that `verify` of the CAM worker finds in a program, with limits of 1000 mm/min and 10000 turns.
fn verify_rules(text: &str) -> Vec<String> {
    let cfg = CamConfig {
        max_feed: 1_000.0,
        max_rpm: 10_000.0,
        ..CamConfig::default()
    };
    let made = CamEngine::default()
        .prepare("w1", &cfg, &json!({"kind": "verify", "text": text}))
        .unwrap();
    made.view["result"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn spaces_inside_a_number_do_not_hide_it() {
    assert_eq!(gcode::words("S3 000"), [('S', 3000.0)]);
    assert_eq!(gcode::words("F1 0 000"), [('F', 10000.0)]);
    let rules = verify_rules("G21 G90\nM3 S3 0000\nG1 X5 F5 000\nM5\nM2\n");
    for want in ["rpm_over_limit", "feed_over_limit"] {
        assert!(rules.iter().any(|r| r == want), "{want} in {rules:?}");
    }
    // what a job asks of the spindle of its machine is read the same way
    let read = gcode::read(&[entry("M3 S3 0000"), entry("G1 X5 F5 000")], 3_000.0);
    assert_eq!((read.max_rpm, read.max_feed_mm_min), (30_000.0, 5_000.0));
}

#[test]
fn the_unit_of_a_line_counts_for_all_of_it() {
    // F100 G20 is 100 in/min, wherever the G20 stands
    let read = gcode::read(&[entry("G1 X1 F100 G20")], 3_000.0);
    assert!((read.max_feed_mm_min - 2540.0).abs() < 1e-9, "{read:?}");
    assert!((read.points[0].0 - 25.4).abs() < 1e-9, "{read:?}");
    // the verification judges inches as millimetres
    let rules = verify_rules("G20 G90\nM3 S100\nG1 X1 F100\nM5\nM2\n");
    assert!(rules.iter().any(|r| r == "feed_over_limit"), "{rules:?}");
    // the feed of a jog is a feed too
    let read = gcode::read(&[entry("$J=G91 X1 F5 000")], 3_000.0);
    assert_eq!(read.max_feed_mm_min, 5_000.0);
}

#[test]
fn a_rapid_that_does_not_name_its_motion_is_still_seen_going_down() {
    let rules = verify_rules("G21 G90\nG0 Z2\nM3 S100\nZ-1\nM5\nM2\n");
    assert!(
        rules.iter().any(|r| r == "rapid_into_material"),
        "{rules:?}"
    );
    let rules = verify_rules("G21 G90\nG1 X5 F100\nX9\nM2\n");
    assert!(
        rules.iter().any(|r| r == "cut_with_spindle_off"),
        "{rules:?}"
    );
}

#[test]
fn a_command_of_the_controller_is_refused_wherever_it_stands() {
    for text in [
        "G0 X1 !",
        "G0 X1 ?",
        "G1 ~X1 F10",
        "G0 X1 $H",
        "N1 $RST=*",
        "G0 X1\u{18}",
        "G1 X1\u{e9}0 F10",
        "G0\u{85}X1",
        "/G0 X1",
    ] {
        refused(text, "line 1");
    }
    // what is only a comment is not sent, so it may say what it likes
    let p = gcode::parse(
        "\u{feff}(change the tool! 50% ~ $ ?)\nG21 ; ready? \u{e9}\nG0 X1\nM30\n%\n",
        0,
        false,
    )
    .unwrap();
    let sent: Vec<&str> = p.entries.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(sent, ["G21", "G0 X1", "M30"]);
}

#[test]
fn the_wipe_of_the_memory_is_refused_however_it_is_spelled() {
    for text in [
        "$ RST=$",
        "$R ST=#",
        "$ r s t = $",
        "$H $RST=$",
        "$J=G91 X1 $H",
    ] {
        let e = gcode::parse_with(text, 0, false, true);
        assert!(e.is_err(), "{text:?} was accepted: {e:?}");
    }
    assert!(gcode::parse_with("$$\n$H\n$J=G91 X1 F10\n", 0, false, true).is_ok());
    // a homing written with blanks still takes the position away
    assert!(gcode::resume_point("G0 X1 Y1 Z1\n$ H\n", 2).is_err());
}

#[test]
fn a_line_the_controller_would_not_read_the_same_way_is_refused() {
    for (text, why) in [
        ("G1 X1..5 F100", "word-number-invalid"),
        ("G1 X1 F100 #1", "character-ignored"),
        ("G1 X1) F100", "character-ignored"),
        ("M3 S1000 S30000", "word-repeated"),
        ("G0 G1 X1", "word-repeated"),
        ("G93\nG1 X1 F2", "feed-mode-unsupported"),
        ("G95", "feed-mode-unsupported"),
        ("M3 S-100", "negative-spindle"),
        ("G1 X1 F-5", "negative-feed"),
        ("G81 X1 Z-1 R1 F10", "canned-cycle-unsupported"),
        ("G41 D1", "cutter-compensation-unsupported"),
        ("M3.5", "unknown-m-code"),
        ("G1 X1 E5 F10", "unknown-word"),
        ("G0 X1 (a note M5", "not closed"),
        ("M3 S1000 ; on\rM5", "carriage return"),
        ("G0 X1\nM30\nM3 S1000", "after the end"),
    ] {
        refused(text, why);
    }
    // lines ending the way Windows ends them are lines like any other
    assert!(gcode::parse("G0 X1\r\nM30\r\n", 0, false).is_ok());
}

#[test]
fn the_turns_are_cut_however_the_number_is_written() {
    assert_eq!(gcode::cut_speed("M3 S 30000", 24_000.0), "M3 S24000");
    assert_eq!(gcode::cut_speed("M3 S3 0000", 24_000.0), "M3 S24000");
    assert_eq!(gcode::cut_speed("S+30000 M3", 24_000.0), "S24000 M3");
    assert_eq!(gcode::cut_speed("M3 S 12000 X1", 24_000.0), "M3 S 12000 X1");
}

#[test]
fn a_printer_program_is_judged_as_the_firmware_reads_it() {
    let cfg = PrinterConfig::default();
    for text in [
        "F999999\nG1 X1\n",
        "G2 X1 Y1 I1 F999999\n",
        "M109 R999\n",
        "M190 R999\n",
        "M303 E0 S999 C8\n",
        "M104 S100 S999\n",
        "M104 (S999) S100\n",
        "M104 S100 (S999)\n",
        "M 104 S999\n",
        "M104 S1..0\n",
        "M117 ready ; x\rM104 S999\n",
        "; a note\rM104 S999\n",
        "M28 other.gco\n",
        "M29\n",
    ] {
        assert!(scan_program(text, &cfg).is_err(), "{text:?} was accepted");
    }
    let small = PrinterConfig {
        bed: [Some(200.0); 3],
        ..PrinterConfig::default()
    };
    assert!(scan_program("G2 X5000 Y1 I1 F600\n", &small).is_err());
    // what slicers write goes on being taken
    let usual = "M117 Hello! (50%) M104 S999 ?\nG28 X Y\nG1 X1 Y1 E0.5 F600 ; to the start (outer)!\nSET_PRESSURE_ADVANCE ADVANCE=0.04\nM862.6 P\"Input shaper\"\nEXCLUDE_OBJECT_START NAME=part_1\nM104 S200\nM109 R200\nM84\n";
    scan_program(usual, &cfg).unwrap();
}

#[test]
fn a_laser_program_is_not_cut_short_or_read_twice_in_silence() {
    let limits = Limits::of(&LaserConfig::default());
    for text in [
        "M4 S100\nG1 X10 F600\nM2\nG1 X20\n",
        "G1 X1 F99999999 F100\n",
        "M3 M5\n",
        "G0 G1 X1 F100\n",
        "G0 X1 (a note M5\n",
        "M3 S10 ; on\rM5\n",
    ] {
        let read = laser::analyze(text, &limits, (0.0, 0.0), 100.0);
        assert!(read.is_err(), "{text:?} was accepted: {read:?}");
    }
}
