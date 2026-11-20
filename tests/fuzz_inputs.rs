//! Readers of input fed with damaged and made-up data from a fixed seed (the same run every time): binary G-code, 3MF files (zip), the deflate and heatshrink readers, and the
//! JSON bodies of the routes that take files and jobs. Nothing may panic and no route may answer with a 5xx: damaged input is a refusal, never a crash and never a half answer.

mod common;

use axum::http::Method;
use common::{admin, call, send};
use eh3d_server::bgcode::{self, Bgcode, Compression, Limits, WriteOptions};
use eh3d_server::{heatshrink, inflate, slicer};
use serde_json::{json, Value};

/// A small generator with a fixed seed (xorshift), so a failure can be repeated.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Changes a copy of `good` in one of the ways a file is damaged: flipped bits, a cut, bytes inserted or a stretch overwritten.
fn damage(rng: &mut Rng, good: &[u8]) -> Vec<u8> {
    let mut b = good.to_vec();
    if b.is_empty() {
        return rng.bytes(8);
    }
    match rng.below(4) {
        0 => {
            for _ in 0..1 + rng.below(3) {
                let at = rng.below(b.len());
                b[at] ^= 1 << rng.below(8);
            }
        }
        1 => b.truncate(rng.below(b.len())),
        2 => {
            let at = rng.below(b.len());
            let n = 1 + rng.below(16);
            let extra = rng.bytes(n);
            b.splice(at..at, extra);
        }
        _ => {
            let at = rng.below(b.len());
            let end = (at + 1 + rng.below(12)).min(b.len());
            let fill = rng.bytes(end - at);
            b[at..end].copy_from_slice(&fill);
        }
    }
    b
}

fn program(lines: usize) -> String {
    let mut s = String::new();
    for i in 0..lines {
        s.push_str(&format!("G1 X{} Y{} E{}.5 F1200\n", i % 90, i % 70, i));
    }
    s
}

fn a_file(compression: Compression) -> Vec<u8> {
    let file = Bgcode {
        crc32: true,
        file_meta: vec![("Producer".into(), "a test".into())],
        printer_meta: vec![("printer_model".into(), "A1".into())],
        thumbnails: Vec::new(),
        print_meta: vec![("layers".into(), "12".into())],
        slicer_meta: vec![("layer_height".into(), "0.2".into())],
        gcode: program(300),
        compressions: Vec::new(),
    };
    bgcode::write(
        &file,
        &WriteOptions {
            metadata: compression,
            gcode: compression,
            ..WriteOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn damaged_binary_gcode_is_a_refusal_and_never_a_panic() {
    let mut rng = Rng(0x5eed_0001);
    let limits = Limits {
        max_block: 1 << 20,
        max_gcode: 1 << 20,
    };
    for compression in [
        Compression::None,
        Compression::Deflate,
        Compression::Heatshrink11,
        Compression::Heatshrink12,
    ] {
        let good = a_file(compression);
        assert!(bgcode::read(&good, &limits).is_ok(), "{compression:?}");
        for _ in 0..400 {
            let bad = damage(&mut rng, &good);
            // either a refusal or a file that reads: if it reads, it can be written again
            if let Ok(file) = bgcode::read(&bad, &limits) {
                let _ = bgcode::write(&file, &WriteOptions::default());
            }
        }
    }
    // made-up bytes behind a good magic number and header
    for _ in 0..400 {
        let mut b = b"GCDE\x01\x00\x00\x00\x01\x00".to_vec();
        let n = rng.below(200);
        b.extend(rng.bytes(n));
        let _ = bgcode::read(&b, &limits);
    }
    // and without one
    for _ in 0..200 {
        let n = rng.below(64);
        let b = rng.bytes(n);
        assert!(bgcode::read(&b, &limits).is_err());
    }
}

#[test]
fn made_up_deflate_and_heatshrink_data_is_a_refusal_and_never_a_panic() {
    let mut rng = Rng(0x5eed_0002);
    for _ in 0..600 {
        let n = rng.below(300);
        let data = rng.bytes(n);
        let _ = inflate::inflate_raw(&data, 4096);
        let _ = inflate::inflate(&data, 4096);
        let _ = heatshrink::decode(&data, 11, 4, 4096);
        let _ = heatshrink::decode(&data, 12, 4, 4096);
    }
    // a good stream, damaged
    let good = inflate::zlib_stored(
        b"The same bytes, stored, many times over. "
            .repeat(20)
            .as_slice(),
    );
    for _ in 0..400 {
        let bad = damage(&mut rng, &good);
        let _ = inflate::inflate(&bad, 1 << 16);
    }
}

/// A zip with one stored file, as the 3MF readers see it.
fn zip_of(name: &str, data: &[u8]) -> Vec<u8> {
    let mut z = Vec::new();
    z.extend([
        0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((name.len() as u16).to_le_bytes());
    z.extend([0, 0]);
    z.extend(name.as_bytes());
    z.extend(data);
    let directory = z.len();
    z.extend([
        0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend((name.len() as u16).to_le_bytes());
    z.extend([0u8; 16]);
    z.extend(name.as_bytes());
    let size = z.len() - directory;
    z.extend([0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend((size as u32).to_le_bytes());
    z.extend((directory as u32).to_le_bytes());
    z.extend([0, 0]);
    z
}

const MODEL: &str = r#"<model><resources><object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/><vertex x="0" y="0" z="10"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/><triangle v1="0" v2="1" v3="3"/></triangles></mesh></object></resources></model>"#;

#[test]
fn damaged_3mf_files_are_a_refusal_and_never_a_panic() {
    let mut rng = Rng(0x5eed_0003);
    let good = zip_of("3D/3dmodel.model", MODEL.as_bytes());
    assert!(slicer::parse_3mf(&good, 1000).is_ok());
    for _ in 0..800 {
        let bad = damage(&mut rng, &good);
        let _ = slicer::parse_3mf(&bad, 1000);
    }
    // a model whose text is damaged inside a good zip
    for _ in 0..400 {
        let bad = damage(&mut rng, MODEL.as_bytes());
        let _ = slicer::parse_3mf(&zip_of("3D/3dmodel.model", &bad), 1000);
    }
    for _ in 0..200 {
        let n = rng.below(120);
        let b = rng.bytes(n);
        let _ = slicer::parse_3mf(&b, 1000);
    }
}

/// A JSON value of a random shape, deep at most `depth`.
fn any_json(rng: &mut Rng, depth: usize) -> Value {
    match rng.below(if depth == 0 { 6 } else { 8 }) {
        0 => Value::Null,
        1 => json!(rng.below(2) == 0),
        2 => json!(rng.next() as i64),
        3 => json!(rng.next() as f64 / 7.0),
        4 => {
            let n = rng.below(12);
            Value::String(String::from_utf8_lossy(&rng.bytes(n)).to_string())
        }
        5 => {
            let words = [
                "kind", "gcode", "print", "text", "bgcode", "format", "machine", "copies",
            ];
            Value::String(words[rng.below(8)].to_string())
        }
        6 => Value::Array(
            (0..rng.below(4))
                .map(|_| any_json(rng, depth - 1))
                .collect(),
        ),
        _ => {
            let keys = [
                "machine",
                "request",
                "kind",
                "text",
                "profile",
                "mesh",
                "artifact",
                "bgcode",
                "format",
                "output",
                "copies",
                "printers",
                "include_gcode",
                "name",
                "app",
                "engine",
                "settings",
            ];
            let mut m = serde_json::Map::new();
            for _ in 0..rng.below(6) {
                m.insert(
                    keys[rng.below(keys.len())].to_string(),
                    any_json(rng, depth - 1),
                );
            }
            Value::Object(m)
        }
    }
}

#[tokio::test]
async fn made_up_json_bodies_of_the_routes_that_take_files_and_jobs_never_get_a_5xx() {
    let s = common::server_with("fuzz-json-bodies", |c| c.slice_enabled = true);
    let boss = admin(&s).await;
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(json!({"id": "f1", "name": "f1", "plugin": "printer3d", "settings": {}})),
    )
    .await;
    assert_eq!(status, 201);
    let mut rng = Rng(0x5eed_0004);
    let routes = [
        (Method::POST, "/api/v1/slice"),
        (Method::POST, "/api/v1/slice/bgcode"),
        (Method::POST, "/api/v1/work/jobs"),
        (Method::POST, "/api/v1/work/farm/jobs"),
        (Method::POST, "/api/v1/work/machines"),
        (Method::PUT, "/api/v1/work/machines/f1"),
        (Method::PUT, "/api/v1/auth/policy"),
    ];
    for (method, path) in routes {
        for round in 0..120 {
            let body = any_json(&mut rng, 3);
            let (status, answer) =
                call(&s, method.clone(), path, Some(&boss), Some(body.clone())).await;
            assert!(
                status.as_u16() < 500,
                "{method} {path} round {round}: {status} for {body}: {answer}"
            );
        }
        // bytes that are not JSON at all
        for _ in 0..40 {
            let n = rng.below(80);
            let raw = rng.bytes(n);
            let reply = send(
                &s,
                method.clone(),
                path,
                Some(&boss),
                &[],
                Some(raw),
                Some("application/json"),
            )
            .await;
            assert!(
                reply.status.as_u16() < 500,
                "{method} {path}: {}",
                reply.status
            );
        }
    }
}
