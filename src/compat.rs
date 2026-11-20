//! The keys of the compatibility routes: a machine of the plugin `printer3d` that programs written for Moonraker or OctoPrint may read has an API key of its own, and
//! the server keeps only the SHA-256 of it (in `compat-keys.json` of the data folder), never the key. The key is shown once, when an administrator makes it
//! (`POST /api/v1/work/machines/{id}/compat/key`), and a new one replaces the old. The routes themselves are in `src/api_compat.rs`; they answer only while the switch
//! `compat_api` of the security page is on (off by default) and only for the machine the key belongs to.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::store;
use crate::sync::Locked;

/// The header that carries the key (lower case, as the headers of the server are read).
pub const HEADER: &str = "x-api-key";

/// How a key starts, so that it is not mistaken for another secret.
pub const PREFIX: &str = "eh3d_";

/// The SHA-256 of a key, in hexadecimal: what the server keeps.
pub fn hash_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Whether two texts are the same, taking as long for any two of the same length.
pub fn same_text(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Whether a text has the shape of a key the server makes: the prefix and 64 digits of hexadecimal in lower case.
pub fn well_formed(key: &str) -> bool {
    key.strip_prefix(PREFIX).is_some_and(|rest| {
        rest.len() == 64
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The hashes of the keys, by the id of the machine each belongs to.
pub struct CompatKeys {
    path: PathBuf,
    keys: Mutex<BTreeMap<String, String>>,
}

impl CompatKeys {
    pub fn open(dir: &Path) -> io::Result<CompatKeys> {
        let path = dir.join("compat-keys.json");
        let keys: BTreeMap<String, String> = store::read_json(&path)?.unwrap_or_default();
        Ok(CompatKeys {
            path,
            keys: Mutex::new(keys),
        })
    }

    /// Makes a new key for the machine, keeps its hash (the old key stops working) and gives the key, which is not kept.
    pub fn issue(&self, machine: &str) -> io::Result<String> {
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        let key = format!("{PREFIX}{}", hex::encode(raw));
        let mut keys = self.keys.locked();
        let mut next = keys.clone();
        next.insert(machine.to_string(), hash_key(&key));
        store::write_json(&self.path, &next)?;
        *keys = next;
        Ok(key)
    }

    /// Takes the key of the machine away. Whether there was one.
    pub fn revoke(&self, machine: &str) -> io::Result<bool> {
        let mut keys = self.keys.locked();
        if !keys.contains_key(machine) {
            return Ok(false);
        }
        let mut next = keys.clone();
        next.remove(machine);
        store::write_json(&self.path, &next)?;
        *keys = next;
        Ok(true)
    }

    /// Whether the machine has a key.
    pub fn has(&self, machine: &str) -> bool {
        self.keys.locked().contains_key(machine)
    }

    /// The machine a key belongs to. A text that is not shaped like a key is not even looked up.
    pub fn find(&self, key: &str) -> Option<String> {
        if !well_formed(key) {
            return None;
        }
        let hash = hash_key(key);
        let keys = self.keys.locked();
        let mut found = None;
        for (machine, stored) in keys.iter() {
            if same_text(stored, &hash) {
                found = Some(machine.clone());
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eh3d-compat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_key_has_a_shape_and_two_texts_are_compared_whole() {
        let key = format!("{PREFIX}{}", "ab01".repeat(16));
        assert!(well_formed(&key));
        let upper = format!("{PREFIX}{}", "AB01".repeat(16));
        let long = format!("{PREFIX}{}", "ab01".repeat(17));
        let bare = "ab01".repeat(16);
        for bad in [
            "",
            "eh3d_",
            "eh3d_xyz",
            upper.as_str(),
            long.as_str(),
            bare.as_str(),
        ] {
            assert!(!well_formed(bad), "{bad}");
        }
        assert!(same_text("abc", "abc"));
        assert!(!same_text("abc", "abd"));
        assert!(!same_text("abc", "abcd"));
        assert_eq!(hash_key("a").len(), 64);
        assert_ne!(hash_key("a"), hash_key("b"));
    }

    #[test]
    fn a_key_is_shown_once_found_by_its_machine_replaced_and_revoked() {
        let d = dir("keys");
        let keys = CompatKeys::open(&d).unwrap();
        assert!(!keys.has("p1"));
        let first = keys.issue("p1").unwrap();
        assert!(well_formed(&first));
        assert!(keys.has("p1") && !keys.has("p2"));
        assert_eq!(keys.find(&first), Some("p1".to_string()));
        assert_eq!(keys.find("eh3d_nothing"), None);
        // the key itself is nowhere in the file, only its hash
        let file = std::fs::read_to_string(d.join("compat-keys.json")).unwrap();
        assert!(!file.contains(&first) && file.contains(&hash_key(&first)));
        // another machine, and a new key for the first one replaces the old
        let other = keys.issue("p2").unwrap();
        assert_ne!(first, other);
        let second = keys.issue("p1").unwrap();
        assert_eq!(keys.find(&first), None);
        assert_eq!(keys.find(&second), Some("p1".to_string()));
        assert_eq!(keys.find(&other), Some("p2".to_string()));
        // it survives a restart
        let again = CompatKeys::open(&d).unwrap();
        assert_eq!(again.find(&second), Some("p1".to_string()));
        // revoking
        assert!(again.revoke("p1").unwrap());
        assert!(!again.revoke("p1").unwrap());
        assert_eq!(again.find(&second), None);
        assert_eq!(again.find(&other), Some("p2".to_string()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
