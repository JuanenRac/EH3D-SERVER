//! Behaviour of the pure parts of the server: ids, the two compressions, the multipart reader, the simulated clock, the G-code helpers and the catalog of errors.
//! Nothing here starts a server or touches a file.

use eh3d_server::errors;
use eh3d_server::gcode;
use eh3d_server::heatshrink::{self, HeatshrinkError};
use eh3d_server::ids::{self, Kind};
use eh3d_server::inflate::{self, InflateError};
use eh3d_server::multipart;
use eh3d_server::simclock::{finished_items, SimClock};

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

const KINDS: [Kind; 5] = [
    Kind::Task,
    Kind::Artifact,
    Kind::Upload,
    Kind::Project,
    Kind::Work,
];

// ---------------------------------------------------------------- ids

#[test]
fn uuid_v7_of_zero_has_the_fixed_version_and_variant_bits() {
    assert_eq!(
        ids::uuid_v7(0, [0; 10]),
        "00000000-0000-7000-8000-000000000000"
    );
}

#[test]
fn uuid_v7_puts_the_milliseconds_first_and_masks_the_random_bits() {
    assert_eq!(
        ids::uuid_v7(0x0123_4567_89AB, [0xFF; 10]),
        "01234567-89ab-7fff-bfff-ffffffffffff"
    );
}

#[test]
fn uuid_v7_keeps_only_forty_eight_bits_of_the_clock() {
    let text = ids::uuid_v7(u64::MAX, [0; 10]);
    assert!(text.starts_with("ffffffff-ffff-7"));
    assert_eq!(text, ids::uuid_v7(0xFFFF_FFFF_FFFF, [0; 10]));
}

#[test]
fn every_kind_has_its_own_prefix() {
    let prefixes: Vec<&str> = KINDS.iter().map(|k| k.prefix()).collect();
    assert_eq!(prefixes, ["t", "a", "up", "p", "w"]);
}

#[test]
fn made_ids_are_valid_for_their_kind_and_for_no_other() {
    for kind in KINDS {
        let id = ids::make(kind, 1_700_000_000_000, [7; 10]);
        assert!(ids::is_valid(kind, &id), "{id}");
        for other in KINDS {
            if other != kind {
                assert!(!ids::is_valid(other, &id), "{id} as {other:?}");
            }
        }
    }
}

#[test]
fn ids_sort_in_the_order_they_were_made() {
    let mut made: Vec<String> = (0..50u64)
        .map(|i| ids::make(Kind::Task, 1_700_000_000_000 + i * 3, [(i % 251) as u8; 10]))
        .collect();
    let sorted = made.clone();
    made.sort();
    assert_eq!(made, sorted);
}

#[test]
fn two_ids_of_the_same_millisecond_differ_by_their_random_part() {
    let a = ids::make(Kind::Artifact, 5, [1; 10]);
    let b = ids::make(Kind::Artifact, 5, [2; 10]);
    assert_ne!(a, b);
    assert!(ids::is_valid(Kind::Artifact, &a) && ids::is_valid(Kind::Artifact, &b));
}

#[test]
fn new_ids_are_valid_and_never_repeat_in_a_run() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        let id = ids::new(Kind::Work);
        assert!(ids::is_valid(Kind::Work, &id));
        assert!(seen.insert(id));
    }
}

#[test]
fn damaged_ids_are_not_valid() {
    let good = ids::make(Kind::Task, 99, [3; 10]);
    assert!(ids::is_valid(Kind::Task, &good));
    assert!(!ids::is_valid(Kind::Task, ""));
    assert!(!ids::is_valid(Kind::Task, "t-"));
    assert!(!ids::is_valid(Kind::Task, &good.to_uppercase()));
    assert!(!ids::is_valid(Kind::Task, &good.replacen('-', "_", 1)));
    assert!(!ids::is_valid(Kind::Task, &format!("{good}0")));
    assert!(!ids::is_valid(Kind::Task, &good[..good.len() - 1]));
    assert!(!ids::is_valid(Kind::Task, &good.replace("-7", "-4")));
    assert!(!ids::is_valid(Kind::Task, &format!(" {good}")));
}

#[test]
fn the_prefix_of_upload_is_not_taken_for_the_prefix_of_a_task() {
    let up = ids::make(Kind::Upload, 1, [0; 10]);
    assert!(up.starts_with("up-"));
    assert!(!ids::is_valid(Kind::Task, &up));
    assert!(!ids::is_valid(Kind::Project, &up));
}

// ---------------------------------------------------------------- heatshrink

#[test]
fn heatshrink_refuses_shapes_a_stream_cannot_have() {
    for (w, l) in [(3u8, 3u8), (16, 4), (11, 2), (11, 11), (11, 12), (0, 0)] {
        assert_eq!(
            heatshrink::encode(b"abc", w, l),
            Err(HeatshrinkError::BadShape),
            "{w},{l}"
        );
        assert_eq!(
            heatshrink::decode(b"abc", w, l, 1),
            Err(HeatshrinkError::BadShape),
            "{w},{l}"
        );
    }
}

#[test]
fn heatshrink_accepts_the_edges_of_the_valid_shapes() {
    for (w, l) in [(4u8, 3u8), (15, 14), (11, 4), (12, 4), (8, 3)] {
        let data = lcg(11, 300, 4);
        let packed = heatshrink::encode(&data, w, l).unwrap();
        assert_eq!(heatshrink::decode(&packed, w, l, data.len()).unwrap(), data);
    }
}

#[test]
fn heatshrink_of_nothing_is_nothing_and_back() {
    assert!(heatshrink::encode(b"", 11, 4).unwrap().is_empty());
    assert!(heatshrink::decode(b"", 11, 4, 0).unwrap().is_empty());
}

#[test]
fn heatshrink_writes_a_single_literal_as_nine_bits() {
    assert_eq!(heatshrink::encode(b"A", 11, 4).unwrap(), vec![0xA0, 0x80]);
    assert_eq!(heatshrink::decode(&[0xA0, 0x80], 11, 4, 1).unwrap(), b"A");
}

#[test]
fn heatshrink_goes_round_for_seeded_data_of_every_alphabet() {
    for (seed, alphabet) in [(1u32, 2u32), (2, 3), (3, 16), (4, 256)] {
        for size in [1usize, 2, 17, 255, 1000, 5000] {
            let data = lcg(seed, size, alphabet);
            for (w, l) in [(11u8, 4u8), (12, 4)] {
                let packed = heatshrink::encode(&data, w, l).unwrap();
                let back = heatshrink::decode(&packed, w, l, data.len()).unwrap();
                assert_eq!(back, data, "seed {seed} alphabet {alphabet} size {size}");
            }
        }
    }
}

#[test]
fn heatshrink_makes_repeated_data_shorter() {
    let data = vec![b'G'; 4000];
    let packed = heatshrink::encode(&data, 11, 4).unwrap();
    assert!(packed.len() < data.len() / 4);
    assert_eq!(
        heatshrink::decode(&packed, 11, 4, data.len()).unwrap(),
        data
    );
}

#[test]
fn heatshrink_encoding_is_deterministic() {
    let data = lcg(9, 800, 5);
    assert_eq!(
        heatshrink::encode(&data, 11, 4).unwrap(),
        heatshrink::encode(&data, 11, 4).unwrap()
    );
}

#[test]
fn heatshrink_truncated_data_is_told_and_does_not_panic() {
    assert_eq!(
        heatshrink::decode(&[], 11, 4, 1),
        Err(HeatshrinkError::Truncated)
    );
    let data = lcg(5, 600, 3);
    let packed = heatshrink::encode(&data, 11, 4).unwrap();
    for cut in [0usize, 1, packed.len() / 2, packed.len() - 1] {
        let r = heatshrink::decode(&packed[..cut], 11, 4, data.len());
        assert!(r.is_err(), "cut at {cut}");
    }
}

#[test]
fn heatshrink_a_copy_before_the_start_is_a_bad_distance() {
    assert_eq!(
        heatshrink::decode(&[0x00, 0x00], 11, 4, 4),
        Err(HeatshrinkError::BadDistance)
    );
}

#[test]
fn heatshrink_decoding_never_gives_more_than_the_size_asked() {
    let data = vec![b'x'; 500];
    let packed = heatshrink::encode(&data, 11, 4).unwrap();
    for size in [0usize, 1, 7, 100, 499] {
        let out = heatshrink::decode(&packed, 11, 4, size).unwrap();
        assert_eq!(out.len(), size);
        assert!(out.iter().all(|b| *b == b'x'));
    }
}

#[test]
fn heatshrink_errors_have_distinct_sentences() {
    let all = [
        HeatshrinkError::BadShape,
        HeatshrinkError::Truncated,
        HeatshrinkError::BadDistance,
    ];
    let texts: std::collections::HashSet<&str> = all.iter().map(|e| e.text()).collect();
    assert_eq!(texts.len(), 3);
    assert!(texts.iter().all(|t| !t.is_empty()));
}

// ---------------------------------------------------------------- inflate

#[test]
fn crc32_has_its_standard_check_values() {
    assert_eq!(inflate::crc32(b""), 0);
    assert_eq!(inflate::crc32(b"123456789"), 0xCBF4_3926);
}

#[test]
fn crc32_can_go_on_from_a_previous_value() {
    let data = lcg(21, 1000, 256);
    for cut in [0usize, 1, 333, 999, 1000] {
        let first = inflate::crc32_update(0, &data[..cut]);
        assert_eq!(
            inflate::crc32_update(first, &data[cut..]),
            inflate::crc32(&data)
        );
    }
}

#[test]
fn adler32_has_its_standard_check_values() {
    assert_eq!(inflate::adler32(b""), 1);
    assert_eq!(inflate::adler32(b"Wikipedia"), 0x11E6_0398);
}

#[test]
fn adler32_of_a_long_run_of_the_largest_byte_does_not_overflow() {
    let data = vec![0xFFu8; 200_000];
    let sum = inflate::adler32(&data);
    assert_eq!(sum, inflate::adler32(&data));
    assert!((sum & 0xFFFF) < 65_521 && (sum >> 16) < 65_521);
}

#[test]
fn stored_raw_deflate_goes_round_at_the_block_edges() {
    for size in [0usize, 1, 65_534, 65_535, 65_536, 131_070, 131_071] {
        let data = lcg(3, size, 256);
        let packed = inflate::deflate_stored_raw(&data);
        assert_eq!(
            inflate::inflate_raw(&packed, size + 1).unwrap(),
            data,
            "{size}"
        );
    }
}

#[test]
fn stored_zlib_goes_round_and_is_taken_by_inflate() {
    for size in [0usize, 1, 100, 70_000] {
        let data = lcg(8, size, 256);
        let packed = inflate::zlib_stored(&data);
        assert_eq!(&packed[..2], &[0x78, 0x01]);
        assert_eq!(inflate::inflate(&packed, size + 1).unwrap(), data);
    }
}

#[test]
fn inflate_also_reads_raw_data_with_no_wrapper() {
    let data = b"G1 X1 Y2\nG1 X3 Y4\n".to_vec();
    let raw = inflate::deflate_stored_raw(&data);
    assert_eq!(inflate::inflate(&raw, 1000).unwrap(), data);
}

#[test]
fn inflate_refuses_a_stream_that_gives_more_than_the_limit() {
    let data = vec![0u8; 100];
    let raw = inflate::deflate_stored_raw(&data);
    assert!(inflate::inflate_raw(&raw, 10).is_err());
    assert_eq!(inflate::inflate_raw(&raw, 100).unwrap().len(), 100);
}

#[test]
fn inflate_of_nothing_is_an_error_not_a_panic() {
    assert!(inflate::inflate(&[], 10).is_err());
    assert!(inflate::inflate_raw(&[], 10).is_err());
}

#[test]
fn inflate_notices_a_wrong_checksum_and_a_cut_stream() {
    let data = lcg(4, 500, 256);
    let mut bad = inflate::zlib_stored(&data);
    let last = bad.len() - 1;
    bad[last] ^= 0x01;
    assert!(matches!(
        inflate::inflate(&bad, 10_000),
        Err(InflateError::Damaged(_))
    ));
    let good = inflate::zlib_stored(&data);
    for cut in [2usize, 3, 10, good.len() / 2, good.len() - 5] {
        assert!(inflate::inflate(&good[..cut], 10_000).is_err(), "cut {cut}");
    }
}

#[test]
fn inflate_a_stored_block_with_a_wrong_length_is_damaged() {
    let mut raw = inflate::deflate_stored_raw(b"hello");
    raw[3] ^= 0xFF;
    assert!(matches!(
        inflate::inflate_raw(&raw, 100),
        Err(InflateError::Damaged(_))
    ));
}

#[test]
fn inflate_never_panics_on_seeded_garbage() {
    for seed in 0..60u32 {
        let junk = lcg(seed, (seed as usize % 40) + 1, 256);
        let _ = inflate::inflate(&junk, 4096);
        let _ = inflate::inflate_raw(&junk, 4096);
    }
}

#[test]
fn inflate_errors_say_what_happened() {
    assert!(!InflateError::Truncated.text().is_empty());
    assert!(InflateError::Damaged("a reason")
        .text()
        .contains("a reason"));
    assert_ne!(InflateError::TooBig.text(), InflateError::Truncated.text());
}

// ---------------------------------------------------------------- multipart

const TYPE: &str = "multipart/form-data; boundary=XyZ";

fn one_part(name: &str, file: Option<&str>, data: &str) -> Vec<u8> {
    let disposition = match file {
        Some(f) => format!("form-data; name=\"{name}\"; filename=\"{f}\""),
        None => format!("form-data; name=\"{name}\""),
    };
    format!("--XyZ\r\nContent-Disposition: {disposition}\r\n\r\n{data}\r\n--XyZ--\r\n").into_bytes()
}

#[test]
fn the_boundary_is_read_with_or_without_quotes_and_in_any_case() {
    assert_eq!(multipart::boundary(TYPE).as_deref(), Some("XyZ"));
    assert_eq!(
        multipart::boundary("multipart/form-data; boundary=\"a b\"").as_deref(),
        Some("a b")
    );
    assert_eq!(
        multipart::boundary("Multipart/Form-Data; Boundary=zz").as_deref(),
        Some("zz")
    );
    assert_eq!(
        multipart::boundary("multipart/form-data; charset=utf-8; boundary=q").as_deref(),
        Some("q")
    );
}

#[test]
fn no_boundary_is_given_for_other_types_or_for_an_empty_or_huge_one() {
    assert!(multipart::boundary("").is_none());
    assert!(multipart::boundary("application/json; boundary=x").is_none());
    assert!(multipart::boundary("multipart/form-data").is_none());
    assert!(multipart::boundary("multipart/form-data; boundary=").is_none());
    let huge = format!("multipart/form-data; boundary={}", "b".repeat(201));
    assert!(multipart::boundary(&huge).is_none());
    let edge = format!("multipart/form-data; boundary={}", "b".repeat(200));
    assert_eq!(multipart::boundary(&edge).map(|b| b.len()), Some(200));
}

#[test]
fn a_file_part_gives_its_name_filename_and_bytes() {
    let parts = multipart::parse(TYPE, &one_part("file", Some("a.gcode"), "G1 X1"), 4).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].name, "file");
    assert_eq!(parts[0].filename.as_deref(), Some("a.gcode"));
    assert_eq!(parts[0].data, b"G1 X1");
}

#[test]
fn a_field_part_has_no_filename() {
    let parts = multipart::parse(TYPE, &one_part("print", None, "true"), 4).unwrap();
    assert_eq!(parts[0].filename, None);
    assert_eq!(parts[0].data, b"true");
}

#[test]
fn an_empty_part_is_an_empty_data_not_an_error() {
    let parts = multipart::parse(TYPE, &one_part("file", Some("e.gcode"), ""), 4).unwrap();
    assert!(parts[0].data.is_empty());
}

#[test]
fn binary_data_with_line_breaks_inside_goes_through_whole() {
    let mut body =
        b"--XyZ\r\nContent-Disposition: form-data; name=\"f\"; filename=\"b.bin\"\r\n\r\n".to_vec();
    let payload: Vec<u8> = vec![0, 13, 10, 13, 10, 255, b'-', b'-', 1, 2];
    body.extend_from_slice(&payload);
    body.extend_from_slice(b"\r\n--XyZ--\r\n");
    let parts = multipart::parse(TYPE, &body, 2).unwrap();
    assert_eq!(parts[0].data, payload);
}

#[test]
fn several_parts_keep_their_order() {
    let body = "--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n--XyZ\r\nContent-Disposition: form-data; name=\"b\"\r\n\r\n2\r\n--XyZ\r\nContent-Disposition: form-data; name=\"c\"\r\n\r\n3\r\n--XyZ--\r\n";
    let parts = multipart::parse(TYPE, body.as_bytes(), 3).unwrap();
    let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert!(multipart::parse(TYPE, body.as_bytes(), 2).is_err());
    assert!(multipart::parse(TYPE, body.as_bytes(), 0).is_err());
}

#[test]
fn name_is_not_taken_from_the_end_of_filename() {
    let body =
        "--XyZ\r\nContent-Disposition: form-data; filename=\"only.gcode\"\r\n\r\nx\r\n--XyZ--\r\n";
    assert!(multipart::parse(TYPE, body.as_bytes(), 4).is_err());
}

#[test]
fn damaged_bodies_are_errors_and_never_panics() {
    assert!(multipart::parse("text/plain", b"x", 4).is_err());
    assert!(multipart::parse(TYPE, b"", 4).is_err());
    assert!(multipart::parse(TYPE, b"no boundary here", 4).is_err());
    assert!(multipart::parse(TYPE, b"--XyZ", 4).is_err());
    assert!(multipart::parse(TYPE, b"--XyZxx", 4).is_err());
    assert!(multipart::parse(
        TYPE,
        b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\ndata",
        4
    )
    .is_err());
    assert!(multipart::parse(TYPE, b"--XyZ\r\nX-Other: 1\r\n\r\ndata\r\n--XyZ--", 4).is_err());
    let whole = one_part("file", Some("a.gcode"), "G1 X1");
    for cut in 0..whole.len() - 4 {
        assert!(
            multipart::parse(TYPE, &whole[..cut], 4).is_err(),
            "cut {cut}"
        );
    }
}

#[test]
fn a_body_with_only_the_closing_boundary_has_no_parts() {
    let parts = multipart::parse(TYPE, b"--XyZ--\r\n", 4).unwrap();
    assert!(parts.is_empty());
}

// ---------------------------------------------------------------- simulated clock

#[test]
fn a_new_clock_is_stopped_and_has_done_no_work() {
    let c = SimClock::new();
    assert!(!c.running());
    assert_eq!(c.real_s(), 0.0);
    assert_eq!(c.work_s(5.0), 0.0);
}

#[test]
fn running_twice_does_not_restart_the_clock() {
    let mut c = SimClock::new();
    c.run();
    c.run();
    assert!(c.running());
    c.hold();
    assert!(!c.running());
}

#[test]
fn setting_the_seconds_stops_the_clock_and_never_goes_below_zero() {
    let mut c = SimClock::new();
    c.run();
    c.set_ran_s(2.5);
    assert!(!c.running());
    assert_eq!(c.real_s(), 2.5);
    c.set_ran_s(-1.0);
    assert_eq!(c.real_s(), 0.0);
}

#[test]
fn work_scales_with_the_rate_and_a_negative_rate_means_all_at_once() {
    let mut c = SimClock::new();
    c.set_ran_s(4.0);
    assert_eq!(c.work_s(0.5), 2.0);
    assert_eq!(c.work_s(3.0), 12.0);
    assert!(c.work_s(-1.0).is_infinite());
    assert!(c.work_s(0.0).is_infinite());
}

#[test]
fn finished_items_follow_the_cumulative_times_exactly() {
    let t = [1.0, 2.0, 2.0, 5.0];
    let got: Vec<usize> = [0.0, 0.999, 1.0, 1.5, 2.0, 4.999, 5.0, 100.0]
        .iter()
        .map(|w| finished_items(&t, *w))
        .collect();
    assert_eq!(got, [0, 0, 1, 1, 3, 3, 4, 4]);
}

#[test]
fn finished_items_never_decrease_as_the_work_grows() {
    let mut cumulative = Vec::new();
    let mut total = 0.0;
    for step in lcg(5, 40, 7) {
        total += f64::from(step);
        cumulative.push(total);
    }
    let mut last = 0;
    for tenth in 0..400u32 {
        let n = finished_items(&cumulative, f64::from(tenth) / 10.0);
        assert!(n >= last);
        last = n;
    }
    assert_eq!(finished_items(&cumulative, f64::MAX), cumulative.len());
}

// ---------------------------------------------------------------- G-code helpers

#[test]
fn strip_removes_comments_of_both_kinds() {
    assert_eq!(gcode::strip("G1 X1 ; move"), "G1 X1");
    assert_eq!(gcode::strip("; only a comment"), "");
    assert_eq!(gcode::strip("(note) G0 X2"), "G0 X2");
    assert_eq!(gcode::strip("G1 (a;b) X1"), "G1  X1");
    assert_eq!(gcode::strip("   "), "");
    assert_eq!(gcode::strip(""), "");
}

#[test]
fn strip_leaves_an_unclosed_parenthesis_as_a_comment_to_the_end() {
    assert_eq!(gcode::strip("G1 X1 (never closed"), "G1 X1");
    assert_eq!(gcode::strip("G1 X1 ) stray"), "G1 X1 ) stray");
}

#[test]
fn strip_is_idempotent() {
    for line in ["G1 X1 ; a", "(a) M3 (b) S10", "  G0 Z5  ", "$H", "%"] {
        let once = gcode::strip(line);
        assert_eq!(gcode::strip(&once), once, "{line}");
    }
}

#[test]
fn words_reads_letters_and_numbers() {
    let w = gcode::words("G1 X10 Y-2.5");
    assert!(w.iter().any(|x| x.0 == 'X' && (x.1 - 10.0).abs() < 1e-9));
    assert!(w.iter().any(|x| x.0 == 'Y' && (x.1 + 2.5).abs() < 1e-9));
}

#[test]
fn a_dollar_line_and_an_empty_line_have_no_words() {
    assert!(gcode::words("").is_empty());
    assert!(gcode::words("$H").is_empty());
}

#[test]
fn cut_speed_lowers_only_the_values_above_the_maximum() {
    assert_eq!(gcode::cut_speed("M3 S12000", 10000.0), "M3 S10000");
    assert_eq!(gcode::cut_speed("M3 S500", 10000.0), "M3 S500");
    assert_eq!(gcode::cut_speed("M3 S10000", 10000.0), "M3 S10000");
    assert_eq!(gcode::cut_speed("M3 s20000", 10000.0), "M3 S10000");
    assert_eq!(gcode::cut_speed("M3 S2.5", 1.5), "M3 S1.5");
}

#[test]
fn cut_speed_leaves_a_line_with_no_speed_untouched() {
    for line in ["G1 X1 Y2", "", "M5", "G0 Z5 F300"] {
        assert_eq!(gcode::cut_speed(line, 100.0), line);
    }
}

#[test]
fn cut_speed_cuts_every_speed_word_of_the_line() {
    assert_eq!(gcode::cut_speed("S9000 M3 S8000", 100.0), "S100 M3 S100");
}

#[test]
fn cut_speed_is_idempotent() {
    for line in ["M3 S12000", "S5 S999", "G1 X1 S0", "M3 S"] {
        let once = gcode::cut_speed(line, 800.0);
        assert_eq!(gcode::cut_speed(&once, 800.0), once, "{line}");
    }
}

// ---------------------------------------------------------------- catalog of errors

#[test]
fn the_error_catalog_json_has_the_three_keys_in_every_row() {
    let json = errors::json();
    let rows = json.as_array().unwrap();
    assert!(rows.len() > 20);
    for row in rows {
        let code = row["code"].as_str().unwrap();
        assert!(errors::known(code));
        assert!((400..600).contains(&row["status"].as_u64().unwrap()));
        assert!(!row["description"].as_str().unwrap().is_empty());
    }
}

#[test]
fn the_error_catalog_json_is_in_the_order_of_its_codes() {
    let json = errors::json();
    let codes: Vec<&str> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["code"].as_str().unwrap())
        .collect();
    let mut sorted = codes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(codes, sorted);
}

#[test]
fn unknown_codes_are_not_known_whatever_their_shape() {
    for code in [
        "",
        " ",
        "Unauthorized",
        "unauthorized ",
        "made_up",
        "weak_password\n",
    ] {
        assert!(!errors::known(code), "{code:?}");
    }
    assert!(errors::known("weak_password"));
    assert!(errors::known("wrong_machine"));
}
