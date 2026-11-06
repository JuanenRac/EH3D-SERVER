//! The applications of EH3D that use this server, and the small rules every common service shares about them.
//!
//! The server is not only the server of the Bungard programs: the Studio, the mobile apps, the panel next to the machine, the PCB, CAM and laser tools keep their projects,
//! read the shared catalogs, queue their jobs and log in through the same four services. An application names itself with a short name (`studio`, `android`, `pcb`...):
//! it is the folder of its projects in the store and the `app` claim of its logins.

use crate::config::ServerConfig;

/// The applications of the family the server knows by name. `auth_extra_apps` in `server.cfg` adds more; with `auth_known_apps_only = false` any well-formed name is accepted.
pub const KNOWN_APPS: [&str; 13] = [
    "bungard-gui",
    "studio",
    "android",
    "ios",
    "watch",
    "hmi",
    "pcb",
    "cam",
    "laser",
    "pnp",
    "cad",
    "tool-manager",
    "cli",
];

/// Whether `name` is shaped like the name of an application: lower case letters, digits and hyphens, 1 to 32, not starting or ending with a hyphen.
pub fn valid_app(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Whether the server accepts `name` as an application: well formed and, unless the settings say otherwise, one of the known ones.
pub fn allowed(cfg: &ServerConfig, name: &str) -> bool {
    if !valid_app(name) {
        return false;
    }
    !cfg.auth_known_apps_only
        || KNOWN_APPS.contains(&name)
        || cfg.auth_extra_apps.iter().any(|a| a == name)
}

/// The applications the settings accept, for the clients that offer a choice.
pub fn list(cfg: &ServerConfig) -> Vec<String> {
    let mut out: Vec<String> = KNOWN_APPS.iter().map(|a| a.to_string()).collect();
    for extra in &cfg.auth_extra_apps {
        if !out.contains(extra) {
            out.push(extra.clone());
        }
    }
    out
}

/// A name or a label for a person to read: 1 to `max` characters, no control characters.
pub fn valid_label(text: &str, max: usize) -> bool {
    let count = text.chars().count();
    (1..=max).contains(&count) && !text.chars().any(char::is_control) && !text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_of_applications_are_short_and_plain() {
        for good in ["studio", "bungard-gui", "a", "pcb2"] {
            assert!(valid_app(good), "{good}");
        }
        let long = "x".repeat(33);
        for bad in [
            "",
            "Studio",
            "-a",
            "a-",
            "a b",
            "a/b",
            "../x",
            long.as_str(),
        ] {
            assert!(!valid_app(bad), "{bad}");
        }
    }

    #[test]
    fn only_the_known_applications_are_accepted_unless_the_settings_say_more() {
        let mut cfg = ServerConfig::default();
        assert!(allowed(&cfg, "android"));
        assert!(!allowed(&cfg, "my-tool"));
        cfg.auth_extra_apps = vec!["my-tool".into()];
        assert!(allowed(&cfg, "my-tool"));
        cfg.auth_extra_apps.clear();
        cfg.auth_known_apps_only = false;
        assert!(allowed(&cfg, "my-tool"));
        assert!(!allowed(&cfg, "My Tool"));
        assert_eq!(list(&cfg).len(), KNOWN_APPS.len());
    }

    #[test]
    fn a_label_has_no_control_characters() {
        assert!(valid_label("front panel", 80));
        assert!(!valid_label("", 80));
        assert!(!valid_label("  ", 80));
        assert!(!valid_label("a\nb", 80));
        assert!(!valid_label(&"x".repeat(81), 80));
    }
}
