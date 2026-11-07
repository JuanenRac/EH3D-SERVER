//! Updates of the installed EH3D services from the releases of GitHub. The server does not talk to GitHub and does not replace itself: a small helper
//! (`deploy/eh3d-update.py`), started by systemd with the rights it needs, does that. The two talk through files in one folder (`updates_dir`):
//!
//! - `request.json`: what the server asks for, `{"action": "check"}`, `{"action": "apply", "components": ["server", "studio"], "allow_unsigned": false}` (`true` when the switch `update_allow_unsigned` allows an update with no signature), `{"action": "undo", "components": [...]}` (puts back the version an update left behind) or `{"action": "clear_history"}`; the helper takes it and removes it.
//! - `components.json`: what the last check found, `[{"id", "latest", "url", "published"}]`.
//! - `previous.json`: what the helper keeps of the version that an update left behind, `{"<id>": {"available": true, "version": "0.0.7"}}` (the version may be null).
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

    /// What the helper keeps of the version left behind by the last update of a component: whether it can be put back, and which version it is when known.
    fn previous(&self, id: &str) -> (bool, Option<String>) {
        let found = read_json(&self.dir.join("previous.json"));
        let entry = found.as_ref().and_then(|f| f.get(id));
        let available = entry
            .and_then(|e| e.get("available"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let version = entry
            .and_then(|e| e.get("version"))
            .and_then(Value::as_str)
            .filter(|_| available)
            .map(str::to_string);
        (available, version)
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
                let (can_undo, previous) = self.previous(id);
                json!({
                    "id": id,
                    "name": name,
                    "installed": installed,
                    "previous": previous,
                    "can_undo": can_undo,
                    "latest": latest_v,
                    "newer": newer,
                    "url": latest.and_then(|c| c.get("url")).and_then(Value::as_str),
                    "published": latest.and_then(|c| c.get("published")).and_then(Value::as_str),
                    "notes": latest.and_then(|c| c.get("notes")).and_then(Value::as_str).unwrap_or(""),
                })
            })
            .collect();
        json!({
            "enabled": true,
            "pending": pending,
            "state": status.get("state").and_then(Value::as_str).unwrap_or("idle"),
            "message": status.get("message").and_then(Value::as_str).unwrap_or(""),
            "log": status.get("log").cloned().unwrap_or(Value::Array(vec![])),
            "percent": status.get("percent").and_then(Value::as_u64).unwrap_or(0),
            "steps": status.get("steps").cloned().unwrap_or(Value::Array(vec![])),
            "history": read_json(&self.dir.join("history.json")).unwrap_or(Value::Array(vec![])),
            "components": components,
        })
    }

    /// Asks the helper for a check, for the update of some components (only the ones that this server knows), for putting back the version an update left behind
    /// or for emptying its history. Only one request waits at a time.
    pub fn request(&self, action: &str, components: &[String]) -> Result<(), String> {
        self.request_with(action, components, false)
    }

    /// `request`, saying whether an update may be installed with no signature when its hash is right (`allow_unsigned`: the switch `update_allow_unsigned` of the security
    /// page). The helper asks for a signature unless this is true. It is written for an `apply` only.
    pub fn request_with(
        &self,
        action: &str,
        components: &[String],
        allow_unsigned: bool,
    ) -> Result<(), String> {
        if !["check", "apply", "undo", "clear_history"].contains(&action) {
            return Err("the action must be check, apply, undo or clear_history".into());
        }
        if action == "apply" || action == "undo" {
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
        if action == "undo" {
            if let Some(none) = components.iter().find(|c| !self.previous(c).0) {
                return Err(format!(
                    "there is no earlier version of '{none}' kept to put back"
                ));
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
        let mut body = json!({ "action": action, "components": components });
        if action == "apply" {
            body["allow_unsigned"] = json!(allow_unsigned);
        }
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
    fn the_steps_the_notes_and_the_history_of_the_helper_reach_the_studio() {
        let d = dir("steps");
        std::fs::write(
            d.join("components.json"),
            r#"[{"id":"studio","latest":"0.0.2","notes":"- one\n- two"}]"#,
        )
        .unwrap();
        std::fs::write(
            d.join("status.json"),
            r#"{"state":"updating","message":"m","log":[],"percent":40,"steps":[{"id":"studio.download","state":"done","percent":100},{"id":"studio.verify","state":"running","percent":50}]}"#,
        )
        .unwrap();
        std::fs::write(
            d.join("history.json"),
            r#"[{"id":"1","started":1,"kind":"apply","label":"studio","state":"done"}]"#,
        )
        .unwrap();
        let v = Updates::new(d.clone(), "0.0.1", None).view();
        assert_eq!(v["percent"], 40);
        assert_eq!(v["steps"].as_array().unwrap().len(), 2);
        assert_eq!(v["steps"][1]["state"], "running");
        assert_eq!(v["history"][0]["label"], "studio");
        let c = v["components"].as_array().unwrap();
        assert_eq!(c[1]["notes"], "- one\n- two");
        assert_eq!(c[0]["notes"], "");
        // a helper that does not write them yet (an older one) leaves them empty and nothing breaks
        let old = Updates::new(dir("old"), "0.0.1", None).view();
        assert_eq!(old["percent"], 0);
        assert!(old["steps"].as_array().unwrap().is_empty());
        assert!(old["history"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_apply_carries_the_switch_that_allows_the_hash_alone_and_the_other_requests_do_not() {
        let d = dir("unsigned");
        let u = Updates::new(d.clone(), "0.0.1", None);
        let read = |d: &Path| -> Value {
            serde_json::from_str(&std::fs::read_to_string(d.join("request.json")).unwrap()).unwrap()
        };
        u.request_with("apply", &["studio".to_string()], true)
            .unwrap();
        assert_eq!(read(&d)["allow_unsigned"], json!(true));
        std::fs::remove_file(d.join("request.json")).unwrap();
        u.request_with("check", &[], true).unwrap();
        assert!(read(&d).get("allow_unsigned").is_none());
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
        assert_eq!(written["allow_unsigned"], json!(false));
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

    #[test]
    fn the_version_left_behind_can_be_put_back_only_when_the_helper_kept_one() {
        let d = dir("undo");
        let u = Updates::new(d.clone(), "0.0.2", None);
        // nothing kept: no component offers it and the request is refused
        let v = u.view();
        assert!(v["components"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["can_undo"] == false && c["previous"].is_null()));
        assert!(u.request("undo", &["server".to_string()]).is_err());
        assert!(u.request("undo", &[]).is_err());
        std::fs::write(
            d.join("previous.json"),
            r#"{"server":{"available":true,"version":"0.0.1"},"studio":{"available":false,"version":"0.0.7"}}"#,
        )
        .unwrap();
        let v = u.view();
        let c = v["components"].as_array().unwrap();
        assert_eq!(
            (c[0]["can_undo"].clone(), c[0]["previous"].clone()),
            (json!(true), json!("0.0.1"))
        );
        assert_eq!(
            (c[1]["can_undo"].clone(), c[1]["previous"].clone()),
            (json!(false), Value::Null)
        );
        // only what has a kept version can be put back, and what is not a component never
        assert!(u.request("undo", &["studio".to_string()]).is_err());
        assert!(u.request("undo", &["kernel".to_string()]).is_err());
        u.request("undo", &["server".to_string()]).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("request.json")).unwrap())
                .unwrap();
        assert_eq!(written["action"], "undo");
        assert_eq!(written["components"], json!(["server"]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_history_is_emptied_by_a_request_that_names_no_component() {
        let d = dir("clear");
        let u = Updates::new(d.clone(), "0.0.1", None);
        u.request("clear_history", &[]).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("request.json")).unwrap())
                .unwrap();
        assert_eq!(written["action"], "clear_history");
        // one request at a time, as with the others
        assert!(u.request("clear_history", &[]).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
