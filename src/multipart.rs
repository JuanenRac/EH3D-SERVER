//! The reading of a `multipart/form-data` body, for the compatibility routes that take a file the way Moonraker and OctoPrint do (`POST /server/files/upload`,
//! `POST /api/files/local`). It reads what the programs send (a part with a `name`, perhaps a `filename`, and its bytes) and refuses everything it cannot read whole; it does
//! not stream, so the caller has already limited the size of the body.

/// One part of the body.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub data: Vec<u8>,
}

/// The boundary a `Content-Type: multipart/form-data; boundary=...` says; `None` when it is not that type or says none.
pub fn boundary(content_type: &str) -> Option<String> {
    let mut pieces = content_type.split(';');
    let kind = pieces.next()?.trim();
    if !kind.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    pieces.find_map(|p| {
        let (key, value) = p.trim().split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("boundary") {
            return None;
        }
        let value = value.trim().trim_matches('"');
        (!value.is_empty() && value.len() <= 200).then(|| value.to_string())
    })
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from > haystack.len() - needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// The value of `key="..."` in a header line such as `Content-Disposition: form-data; name="file"; filename="a.gcode"`.
fn quoted(header: &str, key: &str) -> Option<String> {
    let mut at = 0;
    while let Some(i) = header[at..].find(key) {
        let start = at + i;
        let before = header[..start].chars().last();
        let after = &header[start + key.len()..];
        // `name=` must not be the end of `filename=`
        if before.is_none_or(|c| c == ' ' || c == ';') {
            if let Some(rest) = after.strip_prefix("=\"") {
                let end = rest.find('"')?;
                return Some(rest[..end].to_string());
            }
        }
        at = start + key.len();
    }
    None
}

/// The parts of a body. At most `max_parts` of them; a body that does not start with its boundary, that has a part without a name or that is not closed is an error.
pub fn parse(content_type: &str, body: &[u8], max_parts: usize) -> Result<Vec<Part>, String> {
    let boundary =
        boundary(content_type).ok_or("the body is not multipart/form-data with a boundary")?;
    let delimiter = format!("--{boundary}").into_bytes();
    let mut at = find(body, &delimiter, 0).ok_or("the body does not start with its boundary")?;
    let mut parts = Vec::new();
    loop {
        at += delimiter.len();
        let rest = &body[at..];
        if rest.starts_with(b"--") {
            return Ok(parts);
        }
        let rest = rest
            .strip_prefix(b"\r\n")
            .ok_or("a boundary is followed by a line break")?;
        at += 2;
        let head_end = find(rest, b"\r\n\r\n", 0).ok_or("a part has no end of headers")?;
        let headers = String::from_utf8_lossy(&rest[..head_end]).to_string();
        let data_start = at + head_end + 4;
        let next = find(body, &[b"\r\n".as_slice(), &delimiter].concat(), data_start)
            .ok_or("the body is not closed by its boundary")?;
        let disposition = headers
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-disposition:"))
            .ok_or("a part has no Content-Disposition")?;
        let name = quoted(disposition, "name").ok_or("a part has no name")?;
        if parts.len() >= max_parts {
            return Err(format!("the body has more than {max_parts} parts"));
        }
        parts.push(Part {
            name,
            filename: quoted(disposition, "filename"),
            data: body[data_start..next].to_vec(),
        });
        at = next + 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPE: &str = "multipart/form-data; boundary=XyZ";

    fn body(parts: &[(&str, Option<&str>, &str)]) -> Vec<u8> {
        let mut out = String::new();
        for (name, file, data) in parts {
            out.push_str("--XyZ\r\n");
            match file {
                Some(f) => out.push_str(&format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: text/plain\r\n\r\n"
                )),
                None => out.push_str(&format!(
                    "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"
                )),
            }
            out.push_str(data);
            out.push_str("\r\n");
        }
        out.push_str("--XyZ--\r\n");
        out.into_bytes()
    }

    #[test]
    fn the_boundary_is_read_from_the_type() {
        assert_eq!(boundary(TYPE), Some("XyZ".to_string()));
        assert_eq!(
            boundary("Multipart/Form-Data; charset=x; boundary=\"a b\""),
            Some("a b".to_string())
        );
        assert_eq!(boundary("application/json"), None);
        assert_eq!(boundary("multipart/form-data"), None);
        assert_eq!(boundary("multipart/form-data; boundary="), None);
    }

    #[test]
    fn a_file_and_its_fields_are_read_whole() {
        let b = body(&[
            ("print", None, "false"),
            (
                "file",
                Some("part.gcode"),
                "G28\r\nG1 X1\r\n\r\n--not a boundary\r\n",
            ),
        ]);
        let parts = parse(TYPE, &b, 8).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "print");
        assert_eq!(parts[0].filename, None);
        assert_eq!(parts[0].data, b"false");
        assert_eq!(parts[1].name, "file");
        assert_eq!(parts[1].filename.as_deref(), Some("part.gcode"));
        assert_eq!(parts[1].data, b"G28\r\nG1 X1\r\n\r\n--not a boundary\r\n");
    }

    #[test]
    fn a_body_that_cannot_be_read_whole_is_refused() {
        assert!(parse("text/plain", b"x", 8).is_err());
        assert!(parse(TYPE, b"nothing here", 8).is_err());
        let open = b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nno end";
        assert!(parse(TYPE, open, 8).is_err());
        let nameless = b"--XyZ\r\nContent-Type: text/plain\r\n\r\nx\r\n--XyZ--\r\n";
        assert!(parse(TYPE, nameless, 8).is_err());
        let too_many = body(&[("a", None, "1"), ("b", None, "2")]);
        assert!(parse(TYPE, &too_many, 1).is_err());
        assert_eq!(parse(TYPE, &body(&[]), 1).unwrap(), Vec::new());
    }

    #[test]
    fn name_is_not_read_out_of_filename() {
        let b = b"--XyZ\r\nContent-Disposition: form-data; filename=\"f.gcode\"; name=\"file\"\r\n\r\nx\r\n--XyZ--\r\n";
        let parts = parse(TYPE, b, 4).unwrap();
        assert_eq!(parts[0].name, "file");
        assert_eq!(parts[0].filename.as_deref(), Some("f.gcode"));
    }
}
