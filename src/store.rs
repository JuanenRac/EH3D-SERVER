//! The files the server keeps (users, tokens, machines, jobs): JSON, written through a temporary file so that a power cut never leaves a
//! half-written one, and read with a clear difference between "there is no file yet" and "the file is damaged" (the second is an error:
//! the server never starts over silently on top of data it could not read).

use serde::{de::DeserializeOwned, Serialize};
use std::io;
use std::path::Path;

pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(&tmp, bytes)?;
    // a file that is replaced is replaced whole; on Windows the target must go first
    if cfg!(windows) && path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)
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
