//! The node of the drill head (EH3D-NODE-BUNGARD-AUX) seen through the server: the server asks the node for its status, its camera and its measure of the table with
//! the address and the token that are in the machine's configuration (`head_address`, `head_token`), so a program or a browser that cannot reach the node (another
//! network) reaches it through the server, with the same login as everything else.
//!
//! The node speaks plain HTTP/1.0 on a wired port, so this is a small client over a TCP stream, not a library: a request, a header read until the empty line, and the
//! body either read whole (the status, the measure) or handed on as it comes (the camera, which never ends).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// How long the node has to answer: it is on the same bench or in the same room.
const CONNECT_AND_HEADER: Duration = Duration::from_secs(4);
/// A status or a measure is a line of JSON; more than this is not the node.
const MAX_BODY: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum HeadError {
    /// The address has no `host:port` (a port 80 is assumed when it has none) or is empty.
    NoAddress,
    /// The node did not answer in time, refused the connection or said something that is not HTTP.
    Unreachable,
}

/// The first part of an answer: its status, its content type and the bytes already read after the header.
pub struct Answer {
    pub status: u16,
    pub content_type: String,
    pub rest: Vec<u8>,
    pub stream: TcpStream,
}

/// The address as `host:port`: a node on port 80 may be written without it.
pub fn host_port(address: &str) -> Result<String, HeadError> {
    let a = address.trim();
    if a.is_empty() || a.contains(' ') || a.contains('/') {
        return Err(HeadError::NoAddress);
    }
    Ok(if a.contains(':') {
        a.to_string()
    } else {
        format!("{a}:80")
    })
}

/// The bytes of a request: HTTP/1.0 so that the node closes the connection when it has said everything.
pub fn request_bytes(method: &str, path: &str, host: &str, token: &str) -> Vec<u8> {
    let mut r = format!("{method} {path} HTTP/1.0\r\nHost: {host}\r\nAccept: */*\r\n");
    if !token.is_empty() {
        r.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if method == "POST" {
        r.push_str("Content-Length: 0\r\n");
    }
    r.push_str("\r\n");
    r.into_bytes()
}

/// The status, the content type and where the body starts, from the bytes of an answer read so far. `None` while the header is not complete yet.
pub fn parse_head(buf: &[u8]) -> Option<(u16, String, usize)> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = String::from_utf8_lossy(&buf[..end]);
    let mut lines = head.split("\r\n");
    let first = lines.next()?;
    let status = first.split_whitespace().nth(1)?.parse::<u16>().ok()?;
    let content_type = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default();
    Some((status, content_type, end))
}

/// Sends the request and reads the header; the body is left to the caller (`rest` has what came with the header, `stream` the rest of it).
pub async fn open(
    address: &str,
    token: &str,
    method: &str,
    path: &str,
) -> Result<Answer, HeadError> {
    let hp = host_port(address)?;
    let mut stream = timeout(CONNECT_AND_HEADER, TcpStream::connect(&hp))
        .await
        .map_err(|_| HeadError::Unreachable)?
        .map_err(|_| HeadError::Unreachable)?;
    stream
        .write_all(&request_bytes(method, path, &hp, token))
        .await
        .map_err(|_| HeadError::Unreachable)?;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        if let Some((status, content_type, start)) = parse_head(&buf) {
            return Ok(Answer {
                status,
                content_type,
                rest: buf[start..].to_vec(),
                stream,
            });
        }
        if buf.len() > 16 * 1024 {
            return Err(HeadError::Unreachable);
        }
        let n = timeout(CONNECT_AND_HEADER, stream.read(&mut chunk))
            .await
            .map_err(|_| HeadError::Unreachable)?
            .map_err(|_| HeadError::Unreachable)?;
        if n == 0 {
            return Err(HeadError::Unreachable);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// A whole answer (the status and the measure): the status, the content type and the body.
pub async fn fetch(
    address: &str,
    token: &str,
    method: &str,
    path: &str,
) -> Result<(u16, String, Vec<u8>), HeadError> {
    let mut a = open(address, token, method, path).await?;
    let mut body = std::mem::take(&mut a.rest);
    let mut chunk = [0u8; 2048];
    loop {
        if body.len() > MAX_BODY {
            return Err(HeadError::Unreachable);
        }
        let n = timeout(CONNECT_AND_HEADER, a.stream.read(&mut chunk))
            .await
            .map_err(|_| HeadError::Unreachable)?
            .map_err(|_| HeadError::Unreachable)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok((a.status, a.content_type, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_gets_a_port() {
        assert_eq!(host_port("192.168.0.60:8080").unwrap(), "192.168.0.60:8080");
        assert_eq!(host_port(" head.local ").unwrap(), "head.local:80");
        assert_eq!(host_port("").unwrap_err(), HeadError::NoAddress);
        assert_eq!(host_port("a b").unwrap_err(), HeadError::NoAddress);
        assert_eq!(host_port("http://x/").unwrap_err(), HeadError::NoAddress);
    }

    #[test]
    fn the_request_carries_the_token_only_when_there_is_one() {
        let r = String::from_utf8(request_bytes("GET", "/api/status", "h:80", "")).unwrap();
        assert!(r.starts_with("GET /api/status HTTP/1.0\r\n") && !r.contains("Authorization"));
        let r = String::from_utf8(request_bytes(
            "POST",
            "/api/measure?samples=5",
            "h:80",
            "abcdefgh12",
        ))
        .unwrap();
        assert!(
            r.contains("Authorization: Bearer abcdefgh12\r\n")
                && r.contains("Content-Length: 0")
                && r.ends_with("\r\n\r\n")
        );
    }

    #[test]
    fn the_header_is_read_when_it_is_complete() {
        let part = b"HTTP/1.0 200 OK\r\nContent-Type: appl";
        assert!(parse_head(part).is_none());
        let whole =
            b"HTTP/1.0 401 Unauthorized\r\ncontent-type: application/json\r\n\r\n{\"error\":1}";
        let (status, ct, start) = parse_head(whole).unwrap();
        assert_eq!((status, ct.as_str()), (401, "application/json"));
        assert_eq!(&whole[start..], b"{\"error\":1}");
        assert!(parse_head(b"garbage\r\n\r\n").is_none());
    }
}
