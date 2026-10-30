//! The server's own settings: one small text file of `key = value` lines, as the machine file of the programs. Every key is optional, an
//! unknown key or a value out of range is an error (nothing is silently replaced by a default).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    /// Where the server listens. The default port, 18200, is the EH3D server's own (ARMOR and HYDRA-UMC use others on the same machine).
    pub listen: SocketAddr,
    /// Where users, tokens, machines and jobs are kept.
    pub data_dir: PathBuf,
    /// How long a login lasts, in hours.
    pub session_hours: u32,
    /// The largest job (or file) a request may carry, in megabytes.
    pub max_body_mb: usize,
    /// How many finished jobs are kept in the list.
    pub keep_jobs: usize,
    /// A folder with the pages of EH3D-STUDIO: when it is set the server serves them at `/` (on the same address, so the pages need no
    /// cross-origin permission). Not set: the server answers only the API.
    pub studio_dir: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:18200".parse().expect("a valid default address"),
            data_dir: PathBuf::from("data"),
            session_hours: 12,
            max_body_mb: 8,
            keep_jobs: 200,
            studio_dir: None,
        }
    }
}

fn number(key: &str, value: &str, low: u64, high: u64) -> Result<u64, String> {
    let n: u64 = value
        .parse()
        .map_err(|_| format!("{key} must be a whole number, not '{value}'"))?;
    if n < low || n > high {
        return Err(format!("{key} must be from {low} to {high}"));
    }
    Ok(n)
}

pub fn parse(text: &str) -> Result<ServerConfig, String> {
    let mut c = ServerConfig::default();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {}: expected key = value", n + 1));
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        let at = |e: String| format!("line {}: {e}", n + 1);
        match key.as_str() {
            "listen" => {
                c.listen = value.parse().map_err(|_| {
                    at(format!(
                        "listen must be an address like 0.0.0.0:18200, not '{value}'"
                    ))
                })?
            }
            "data_dir" => {
                if value.is_empty() {
                    return Err(at("data_dir must not be empty".into()));
                }
                c.data_dir = PathBuf::from(value);
            }
            "session_hours" => c.session_hours = number(&key, value, 1, 720).map_err(at)? as u32,
            "max_body_mb" => c.max_body_mb = number(&key, value, 1, 256).map_err(at)? as usize,
            "keep_jobs" => c.keep_jobs = number(&key, value, 10, 100_000).map_err(at)? as usize,
            "studio_dir" => {
                c.studio_dir = if value.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(value))
                }
            }
            other => return Err(at(format!("unknown key '{other}'"))),
        }
    }
    Ok(c)
}

/// Reads the file at `path`; with no path, `server.cfg` in the working folder, and the defaults when it is not there.
pub fn load(path: Option<&Path>) -> Result<ServerConfig, String> {
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            parse(&text)
        }
        None => match std::fs::read_to_string("server.cfg") {
            Ok(text) => parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ServerConfig::default()),
            Err(e) => Err(format!("server.cfg: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_studio_folder_is_optional() {
        assert_eq!(parse("").unwrap().studio_dir, None);
        assert_eq!(
            parse(
                "studio_dir =
"
            )
            .unwrap()
            .studio_dir,
            None
        );
        assert_eq!(
            parse(
                "studio_dir = /opt/eh3d-studio
"
            )
            .unwrap()
            .studio_dir,
            Some(PathBuf::from("/opt/eh3d-studio"))
        );
    }

    #[test]
    fn the_defaults_are_the_servers_own_port_and_a_data_folder() {
        let c = parse("").unwrap();
        assert_eq!(c.listen.port(), 18200);
        assert_eq!(c.data_dir, PathBuf::from("data"));
        assert_eq!((c.session_hours, c.max_body_mb, c.keep_jobs), (12, 8, 200));
    }

    #[test]
    fn every_key_is_read_and_comments_are_ignored() {
        let c = parse("# a comment\nlisten = 127.0.0.1:18300\ndata_dir = /var/lib/eh3d\nsession_hours = 4\nmax_body_mb = 16 \n; another\nkeep_jobs = 50\n").unwrap();
        assert_eq!(c.listen, "127.0.0.1:18300".parse().unwrap());
        assert_eq!(c.data_dir, PathBuf::from("/var/lib/eh3d"));
        assert_eq!((c.session_hours, c.max_body_mb, c.keep_jobs), (4, 16, 50));
    }

    #[test]
    fn mistakes_are_errors_not_silent_defaults() {
        assert!(parse("lisen = 1.2.3.4:5\n")
            .unwrap_err()
            .contains("unknown key"));
        assert!(parse("listen = nowhere\n").is_err());
        assert!(parse("session_hours = 0\n").is_err());
        assert!(parse("max_body_mb = lots\n").is_err());
        assert!(parse("keep_jobs = 5\n").is_err());
        assert!(parse("just words\n").is_err());
        assert!(parse("data_dir =\n").is_err());
    }
}
