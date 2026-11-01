//! The files the server keeps (users, tokens, machines, jobs): JSON, written through a temporary file so that a power cut never leaves a
//! half-written one, and read with a clear difference between "there is no file yet" and "the file is damaged" (the second is an error:
//! the server never starts over silently on top of data it could not read).
//!
//! A write is durable: the new text is flushed to the disk before it replaces the old file, the folder is flushed after the replacement,
//! and the file that is replaced is kept once as `<name>.bak`. If a file is found damaged and the copy is good, the error says so and
//! how to recover (copy it over the file); the copy is never loaded by itself.

use serde::{de::DeserializeOwned, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Where the copy of the previous version of a file is kept: the same name with `.bak` added.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            let backup = backup_path(path);
            let good_copy = std::fs::read(&backup)
                .ok()
                .is_some_and(|b| serde_json::from_slice::<T>(&b).is_ok());
            let hint = if good_copy {
                format!(
                    "; {} holds the previous version and is good: copy it over {} to recover (it is not loaded by itself)",
                    backup.display(),
                    path.display()
                )
            } else {
                String::new()
            };
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}{hint}", path.display()),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    write_durable(path, &bytes, true)
}

/// Replaces a file with `bytes` so that a power cut leaves either the old file or the new one, whole: written to a temporary file and flushed, the old one kept as
/// `<name>.bak` when `keep_backup`, then renamed, then the folder flushed.
pub fn write_durable(path: &Path, bytes: &[u8], keep_backup: bool) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    // the new text reaches the disk before it takes the place of the old file: a power cut right after the rename must not leave an empty or short file
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    // the file that is replaced is kept once, to recover from if the new one is ever found damaged
    if keep_backup && path.exists() {
        let _ = std::fs::copy(path, backup_path(path));
    }
    // a file that is replaced is replaced whole; on Windows the target must go first
    if cfg!(windows) && path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    // the rename itself is flushed with the folder (not possible on Windows)
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Doc {
        name: String,
        n: u32,
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("eh3d-server-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_missing_file_is_none_and_a_written_one_comes_back() {
        let d = dir("store-a");
        let p = d.join("doc.json");
        assert_eq!(read_json::<Doc>(&p).unwrap(), None);
        let doc = Doc {
            name: "x".into(),
            n: 7,
        };
        write_json(&p, &doc).unwrap();
        assert_eq!(read_json::<Doc>(&p).unwrap(), Some(doc));
        write_json(
            &p,
            &Doc {
                name: "y".into(),
                n: 8,
            },
        )
        .unwrap();
        assert_eq!(read_json::<Doc>(&p).unwrap().unwrap().n, 8);
        assert!(!p.with_extension("tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_previous_version_is_kept_once_and_a_good_copy_is_pointed_at_when_the_file_is_damaged() {
        let d = dir("store-c");
        let p = d.join("doc.json");
        let doc = |n| Doc {
            name: "z".into(),
            n,
        };
        write_json(&p, &doc(1)).unwrap();
        assert!(
            !backup_path(&p).exists(),
            "the first write has nothing to keep"
        );
        write_json(&p, &doc(2)).unwrap();
        write_json(&p, &doc(3)).unwrap();
        // the copy is the version before the last one, only one generation
        let kept: Doc = serde_json::from_slice(&std::fs::read(backup_path(&p)).unwrap()).unwrap();
        assert_eq!(kept.n, 2);
        assert_eq!(read_json::<Doc>(&p).unwrap().unwrap().n, 3);
        // a power cut leaves the file empty: an error that names the good copy and how to recover, and the copy is not loaded
        std::fs::write(&p, b"").unwrap();
        let e = read_json::<Doc>(&p).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let text = e.to_string();
        assert!(
            text.contains(".bak") && text.contains("copy it over") && text.contains("doc.json"),
            "{text}"
        );
        // no good copy: no promise of one
        std::fs::write(backup_path(&p), b"{ not json").unwrap();
        let e = read_json::<Doc>(&p).unwrap_err().to_string();
        assert!(!e.contains("copy it over"), "{e}");
        // a missing main file is still "no file yet", whatever the copy holds
        std::fs::remove_file(&p).unwrap();
        assert_eq!(read_json::<Doc>(&p).unwrap(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_damaged_file_is_an_error_not_an_empty_start() {
        let d = dir("store-b");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("doc.json");
        std::fs::write(&p, b"{ not json").unwrap();
        let e = read_json::<Doc>(&p).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&d);
    }
}
