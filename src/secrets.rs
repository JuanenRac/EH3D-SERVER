//! The keys that settings of a machine hold and that nobody reads back: the key of the API of a printer is kept in the settings of its machine (it is how the server reaches
//! the printer) and is shown as a row of stars wherever the settings are shown. A person who changes the settings and sends the stars back keeps the key that is there.

use serde_json::Value;

/// What stands for a key that is kept and never shown.
pub const MASK: &str = "********";

/// The settings with the key of the connection hidden.
pub fn mask_settings(settings: &Value) -> Value {
    let mut out = settings.clone();
    if let Some(key) = out.get_mut("connection").and_then(|c| c.get_mut("api_key")) {
        if key.as_str().is_some_and(|k| !k.is_empty()) {
            *key = Value::String(MASK.to_string());
        }
    }
    out
}

/// New settings that carry the stars in the place of the key get the key the machine has now.
pub fn keep_masked(old: &Value, new: &mut Value) {
    let sent_mask = new
        .get("connection")
        .and_then(|c| c.get("api_key"))
        .and_then(Value::as_str)
        .is_some_and(|k| k == MASK);
    if !sent_mask {
        return;
    }
    let kept = old
        .get("connection")
        .and_then(|c| c.get("api_key"))
        .cloned();
    if let Some(conn) = new.get_mut("connection").and_then(Value::as_object_mut) {
        match kept {
            Some(k) => {
                conn.insert("api_key".into(), k);
            }
            None => {
                conn.remove("api_key");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_key_of_a_connection_is_hidden_wherever_the_settings_are_shown() {
        let s = json!({"model": "generic", "connection": {"kind": "moonraker", "url": "http://x", "api_key": "secret-value"}});
        let shown = mask_settings(&s);
        assert_eq!(shown["connection"]["api_key"], MASK);
        assert_eq!(shown["connection"]["url"], "http://x");
        assert_eq!(
            s["connection"]["api_key"], "secret-value",
            "the settings themselves are not touched"
        );
        let none = json!({"connection": {"kind": "demo"}});
        assert_eq!(mask_settings(&none), none);
        let empty = json!({"connection": {"kind": "octoprint", "api_key": ""}});
        assert_eq!(mask_settings(&empty), empty);
        assert_eq!(mask_settings(&json!({"a": 1})), json!({"a": 1}));
    }

    #[test]
    fn stars_sent_back_keep_the_key_and_a_new_key_replaces_it() {
        let old =
            json!({"connection": {"kind": "moonraker", "url": "http://x", "api_key": "old-key"}});
        let mut same =
            json!({"connection": {"kind": "moonraker", "url": "http://y", "api_key": MASK}});
        keep_masked(&old, &mut same);
        assert_eq!(same["connection"]["api_key"], "old-key");
        assert_eq!(same["connection"]["url"], "http://y");
        let mut fresh =
            json!({"connection": {"kind": "moonraker", "url": "http://x", "api_key": "new-key"}});
        keep_masked(&old, &mut fresh);
        assert_eq!(fresh["connection"]["api_key"], "new-key");
        let mut gone =
            json!({"connection": {"kind": "moonraker", "url": "http://x", "api_key": MASK}});
        keep_masked(&json!({"connection": {"kind": "demo"}}), &mut gone);
        assert!(
            gone["connection"].get("api_key").is_none(),
            "there was no key to keep"
        );
        let mut other = json!({"model": "generic"});
        keep_masked(&old, &mut other);
        assert_eq!(other, json!({"model": "generic"}));
    }
}
