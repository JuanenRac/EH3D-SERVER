//! Updates of the installed EH3D services from the releases of GitHub. The server does not talk to GitHub and does not replace itself: a small helper
//! (`deploy/eh3d-update.py`), started by systemd with the rights it needs, does that. The two talk through files in one folder (`updates_dir`):
//!
//! - `request.json`: what the server asks for, `{"action": "check"}` or `{"action": "apply", "components": ["server", "studio"]}`; the helper takes it and removes it.
//! - `components.json`: what the last check found, `[{"id", "latest", "url", "published"}]`.
//! - `status.json`: what the helper is doing, `{"state": "idle|checking|updating|done|error", "message": "...", "log": [...]}`.
//!
//! The server adds what it knows itself (the version it is, the version of the pages it serves) and says which components have a newer release.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The components that can be updated: the id, the name and the repository whose releases carry them.
pub const COMPONENTS: [(&str, &str, &str); 2] = [
    ("server", "EH3D-SERVER", "EH3D-SERVER"),
    ("studio", "EH3D-STUDIO", "EH3D-STUDIO"),
];

/// Whether version `latest` is newer than `installed` (numbers separated by points, a leading `v` is ignored; a part that is not a number counts as 0).
pub fn is_newer(installed: &str, latest: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    }
    let (a, b) = (parts(installed), parts(latest));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if y != x {
            return y > x;
        }
    }
    false
}

/// The folder where the server and the helper meet.
#[derive(Debug, Clone)]
pub struct Updates {
    dir: PathBuf,
    /// The version of the server itself and the folder of the pages of EH3D-STUDIO it serves.
    server_version: String,
    studio_dir: Option<PathBuf>,
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

impl Updates {
    pub fn new(dir: PathBuf, server_version: &str, studio_dir: Option<PathBuf>) -> Updates {
        Updates {
            dir,
            server_version: server_version.to_string(),
            studio_dir,
        }
    }

    /// The version of the pages of EH3D-STUDIO that are served: the `version` of the `version.json` that its build leaves with them.
    fn studio_version(&self) -> Option<String> {
        let v = read_json(&self.studio_dir.as_ref()?.join("version.json"))?;
        v.get("version")?.as_str().map(str::to_string)
    }

    fn installed(&self, id: &str) -> Option<String> {
        match id {
            "server" => Some(self.server_version.clone()),
            "studio" => self.studio_version(),
            _ => None,
        }
    }

    /// Everything the Studio shows about the updates.
    pub fn view(&self) -> Value {
        let found = read_json(&self.dir.join("components.json")).unwrap_or(Value::Array(vec![]));
        let status =
            read_json(&self.dir.join("status.json")).unwrap_or_else(|| json!({ "state": "idle" }));
        let pending = self.dir.join("request.json").exists();
        let components: Vec<Value> = COMPONENTS
            .iter()
            .map(|(id, name, _)| {
                let latest = found.as_array().and_then(|a| {
                    a.iter()
                        .find(|c| c.get("id").and_then(Value::as_str) == Some(*id))
                });
                let latest_v = latest.and_then(|c| c.get("latest")).and_then(Value::as_str);
                let installed = self.installed(id);
                let newer = match (&installed, latest_v) {
                    (Some(i), Some(l)) => is_newer(i, l),
                    (None, Some(_)) => true,
                    _ => false,
                };
                json!({
                    "id": id,
                    "name": name,
                    "installed": installed,
                    "latest": latest_v,
                    "newer": newer,
                    "url": latest.and_then(|c| c.get("url")).and_then(Value::as_str),
                    "published": latest.and_then(|c| c.get("published")).and_then(Value::as_str),
                })
            })
            .collect();
        json!({
            "enabled": true,
            "pending": pending,
            "state": status.get("state").and_then(Value::as_str).unwrap_or("idle"),
            "message": status.get("message").and_then(Value::as_str).unwrap_or(""),
            "log": status.get("log").cloned().unwrap_or(Value::Array(vec![])),
            "components": components,
        })
    }

    /// Asks the helper for a check, or for the update of some components (only the ones that this server knows). Only one request waits at a time.
    pub fn request(&self, action: &str, components: &[String]) -> Result<(), String> {
        if action != "check" && action != "apply" {
            return Err("the action must be check or apply".into());
        }
        if action == "apply" {
            if components.is_empty() {
                return Err("say which components to update".into());
            }
            if let Some(bad) = components
                .iter()
                .find(|c| !COMPONENTS.iter().any(|k| k.0 == c.as_str()))
            {
                return Err(format!("'{bad}' is not a component that can be updated"));
            }
        }
        let busy = read_json(&self.dir.join("status.json"))
            .and_then(|s| s.get("state").and_then(Value::as_str).map(str::to_string))
            .is_some_and(|s| s == "checking" || s == "updating");
        if busy || self.dir.join("request.json").exists() {
            return Err("an update is already waiting or running".into());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let tmp = self.dir.join("request.json.tmp");
        let body = json!({ "action": action, "components": components });
        std::fs::write(&tmp, body.to_string()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.dir.join("request.json")).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eh3d-updates-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn versions_are_compared_part_by_part() {
        assert!(
            is_newer("0.0.1", "0.0.2")
                && is_newer("0.9.9", "0.10.0")
                && is_newer("0.8.1.9", "0.8.2.0")
                && is_newer("0.0.1", "v0.1.0")
        );
        assert!(
            !is_newer("0.0.2", "0.0.2")
                && !is_newer("0.1.0", "0.0.9")
                && !is_newer("1.0", "1.0.0")
                && !is_newer("0.0.3", "garbage")
        );
        assert!(is_newer("0.0.1", "0.0.1.1"));
    }

    #[test]
    fn what_was_found_and_what_is_installed_say_which_components_are_newer() {
        let d = dir("view");
        let studio = d.join("studio");
        std::fs::create_dir_all(&studio).unwrap();
        std::fs::write(studio.join("version.json"), r#"{"version":"0.0.1"}"#).unwrap();
        std::fs::write(
            d.join("components.json"),
            r#"[{"id":"server","latest":"0.0.1","url":"https://example.org/s"},{"id":"studio","latest":"0.0.2","url":"https://example.org/t","published":"x"}]"#,
        )
        .unwrap();
        std::fs::write(
            d.join("status.json"),
            r#"{"state":"done","message":"all is up to date","log":["a","b"]}"#,
        )
        .unwrap();
        let u = Updates::new(d.clone(), "0.0.1", Some(studio));
        let v = u.view();
        assert_eq!(v["state"], "done");
        assert_eq!(v["log"].as_array().unwrap().len(), 2);
        let c = v["components"].as_array().unwrap();
        assert_eq!(
            (
                c[0]["id"].as_str(),
                c[0]["installed"].as_str(),
                c[0]["newer"].as_bool()
            ),
            (Some("server"), Some("0.0.1"), Some(false))
        );
        assert_eq!(
            (
                c[1]["id"].as_str(),
                c[1]["latest"].as_str(),
                c[1]["newer"].as_bool()
            ),
            (Some("studio"), Some("0.0.2"), Some(true))
        );
        // nothing found yet: nothing is newer
        let empty = Updates::new(dir("empty"), "0.0.1", None);
        let v = empty.view();
        assert_eq!(v["state"], "idle");
        assert!(v["components"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["newer"] == false));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_request_is_written_once_and_only_for_what_can_be_updated() {
        let d = dir("request");
        let u = Updates::new(d.clone(), "0.0.1", None);
        assert!(u.request("sing", &[]).is_err());
        assert!(u.request("apply", &[]).is_err());
        assert!(u.request("apply", &["kernel".to_string()]).is_err());
        u.request("apply", &["server".to_string(), "studio".to_string()])
            .unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("request.json")).unwrap())
                .unwrap();
        assert_eq!(written["action"], "apply");
        assert_eq!(written["components"], json!(["server", "studio"]));
        assert_eq!(u.view()["pending"], true);
        // one request waits at a time
        assert!(u.request("check", &[]).is_err());
        std::fs::remove_file(d.join("request.json")).unwrap();
        // and not while the helper is working
        std::fs::write(d.join("status.json"), r#"{"state":"updating"}"#).unwrap();
        assert!(u.request("check", &[]).is_err());
        std::fs::write(d.join("status.json"), r#"{"state":"done"}"#).unwrap();
        u.request("check", &[]).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }
}
