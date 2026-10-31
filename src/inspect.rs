//! The check of the work through the camera of the drill head. The server turns the ring of LEDs on, takes a picture from the node, runs the analyser of the shop on it (the
//! library `eh3d-inspect`, the same the desktop program uses), makes the ring say how it went (green, or red blinking) and keeps the last report of the machine so the screen next
//! to the machine can show it. The profile of the shop (the tolerances and which checks are on) is kept by the server for each machine.

use std::path::{Path, PathBuf};

use eh3d_inspect::{inspect, suggest_light_level, ExpectedHole, Gray, Profile, Report, Verdict};
use serde_json::Value;

use crate::head::{self, HeadError};
use crate::store;

/// A picture of the camera: the biggest the node can send (an OV5640 at its full size is a few hundred kilobytes).
const MAX_PICTURE: usize = 6 * 1024 * 1024;
/// The sensor needs a moment to settle its exposure after the light changes.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(450);
/// How many times the light is corrected when the picture is too dark or too bright.
const LIGHT_TRIES: usize = 2;

pub fn profile_path(data_dir: &Path, machine: &str) -> PathBuf {
    data_dir.join("inspect").join(format!("{machine}.json"))
}

/// The profile of a machine: the one the shop saved, or the standard one.
pub fn load_profile(data_dir: &Path, machine: &str) -> Profile {
    store::read_json::<Value>(&profile_path(data_dir, machine))
        .ok()
        .flatten()
        .and_then(|v| Profile::from_json(&v.to_string()).ok())
        .map(|(p, _)| p)
        .unwrap_or_default()
}

pub fn save_profile(data_dir: &Path, machine: &str, p: &Profile) -> std::io::Result<()> {
    store::write_json(&profile_path(data_dir, machine), &p.to_json())
}

#[derive(Debug, Clone)]
pub struct Params {
    pub holes: Vec<ExpectedHole>,
    /// Turn the ring on at this level in the white of the camera before the picture (`None`: leave the light as it is).
    pub light_level: Option<u32>,
    /// Correct the light when the picture is too dark or too bright.
    pub auto_light: bool,
    /// Make the ring say the verdict (green, red blinking, amber).
    pub cue: bool,
    /// Put the ring back as it was.
    pub restore_light: bool,
}

#[derive(Debug)]
pub enum InspectError {
    Head(HeadError),
    /// The node answered something that is not what was asked: its status and a word for it.
    Refused(u16, &'static str),
    /// The picture could not be read.
    Picture(String),
}

impl From<HeadError> for InspectError {
    fn from(e: HeadError) -> Self {
        InspectError::Head(e)
    }
}

async fn call(
    address: &str,
    token: &str,
    method: &str,
    path: &str,
) -> Result<(u16, Vec<u8>), InspectError> {
    let (status, _, body) = head::fetch_limit(address, token, method, path, MAX_PICTURE).await?;
    Ok((status, body))
}

fn level_and_ceiling(body: &[u8]) -> Option<(bool, u32, u32)> {
    let v: Value = serde_json::from_slice(body).ok()?;
    Some((
        v.get("on")?.as_bool()?,
        v.get("level")?.as_u64()? as u32,
        v.get("max_level").and_then(Value::as_u64).unwrap_or(80) as u32,
    ))
}

/// The whole check.
pub async fn run(
    address: &str,
    token: &str,
    profile: Profile,
    params: Params,
) -> Result<Report, InspectError> {
    // how the ring was, to put it back
    let mut before: Option<(bool, u32, u32)> = None;
    let mut ceiling = 80u32;
    if params.light_level.is_some() || params.cue {
        if let Ok((200, body)) = call(address, token, "GET", "/api/light").await {
            before = level_and_ceiling(&body);
            if let Some((_, _, c)) = before {
                ceiling = c;
            }
        }
    }
    let mut level = params.light_level.unwrap_or(0).min(100);
    if let Some(l) = params.light_level {
        let path = format!("/api/light?on=1&level={l}&tone=white&sectors=all");
        let (status, _) = call(address, token, "POST", &path).await?;
        if status != 200 {
            return Err(InspectError::Refused(status, "light"));
        }
        tokio::time::sleep(SETTLE).await;
    }
    let mut tries = 0;
    let report = loop {
        let (status, picture) = call(address, token, "GET", "/api/snapshot").await?;
        if status != 200 {
            return Err(InspectError::Refused(status, "snapshot"));
        }
        let p = profile.clone();
        let holes = params.holes.clone();
        let report = tokio::task::spawn_blocking(move || -> Result<Report, String> {
            let img = Gray::from_jpeg(&picture)?;
            Ok(inspect(&img, &holes, &p))
        })
        .await
        .map_err(|e| InspectError::Picture(e.to_string()))?
        .map_err(InspectError::Picture)?;
        // a picture that is too dark or too bright is worth another try with the light corrected, as long as the ring is ours to move
        let poor = report
            .findings
            .iter()
            .any(|f| f.code == "image_too_dark" || f.code == "image_too_bright");
        if poor && params.auto_light && params.light_level.is_some() && tries < LIGHT_TRIES {
            if let Some(q) = report.quality {
                if let Some(next) = suggest_light_level(level, &q, 125.0, ceiling) {
                    tries += 1;
                    level = next;
                    let path = format!("/api/light?on=1&level={next}&tone=white&sectors=all");
                    let _ = call(address, token, "POST", &path).await?;
                    tokio::time::sleep(SETTLE).await;
                    continue;
                }
            }
        }
        break report;
    };
    if params.cue {
        let cue = match report.verdict {
            Verdict::Ok => "ok",
            Verdict::NotOk => "nok",
            Verdict::Unknown => "warn",
        };
        let _ = call(address, token, "POST", &format!("/api/light?cue={cue}")).await;
    }
    if params.restore_light {
        if let Some((was_on, was_level, _)) = before {
            let path = if was_on {
                format!("/api/light?on=1&level={was_level}")
            } else {
                "/api/light?on=0".to_string()
            };
            let _ = call(address, token, "POST", &path).await;
        }
    }
    Ok(report)
}

/// A query for the ring that is safe to hand to the node: letters, digits and the few signs of a colour or a sector, and short.
pub fn light_query_ok(q: &str) -> bool {
    q.len() <= 200
        && q.chars()
            .all(|c| c.is_ascii_alphanumeric() || "=&_#.,-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_words_of_the_ring_go_through() {
        assert!(light_query_ok(
            "on=1&level=70&tone=warm&sectors=0x0f&color=ff8800&cue=nok&cue_ms=3000"
        ));
        assert!(light_query_ok(""));
        for bad in [
            "on=1 HTTP/1.1",
            "a=b\r\nX: y",
            "x=%00",
            "level=1;rm",
            "q=\"x\"",
        ] {
            assert!(!light_query_ok(bad), "{bad}");
        }
        assert!(!light_query_ok(&"a".repeat(201)));
    }

    #[test]
    fn the_profile_is_kept_for_each_machine() {
        let dir = std::env::temp_dir().join(format!("eh3d-inspect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_profile(&dir, "bench"), Profile::default());
        let mut p = Profile::preset("fine_pitch").unwrap();
        p.px_per_mm = 88.0;
        save_profile(&dir, "bench", &p).unwrap();
        assert_eq!(load_profile(&dir, "bench"), p);
        assert_eq!(load_profile(&dir, "other"), Profile::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_light_of_the_node_is_read() {
        assert_eq!(
            level_and_ceiling(br#"{"on":true,"level":40,"max_level":70}"#),
            Some((true, 40, 70))
        );
        assert_eq!(
            level_and_ceiling(br#"{"on":false,"level":0}"#),
            Some((false, 0, 80))
        );
        assert_eq!(level_and_ceiling(b"nope"), None);
    }
}
