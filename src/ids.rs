//! The identifiers the server makes for the things it keeps: tasks, artifacts, uploads and projects. Each is a prefix, a hyphen and a UUID version 7
//! (a time-ordered UUID: the first 48 bits are the milliseconds since 1970, so a list sorted by id is a list sorted by creation, and the rest is random).
//!
//! The generator takes the clock and the random bytes as parameters, so a test can fix them; `new` uses the real ones. Two ids made in the same millisecond
//! differ by their random part, and an id made later never sorts before one made earlier unless the clock went back.

use rand::RngCore;

/// The kinds of thing that have an id, with the prefix of their ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Task,
    Artifact,
    Upload,
    Project,
}

impl Kind {
    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Task => "t",
            Kind::Artifact => "a",
            Kind::Upload => "up",
            Kind::Project => "p",
        }
    }
}

/// A UUID version 7 in its text form (`xxxxxxxx-xxxx-7xxx-yxxx-xxxxxxxxxxxx`, `y` being 8, 9, a or b), from the milliseconds and ten random bytes.
pub fn uuid_v7(unix_ms: u64, random: [u8; 10]) -> String {
    let mut b = [0u8; 16];
    let ms = (unix_ms & 0xFFFF_FFFF_FFFF).to_be_bytes();
    b[..6].copy_from_slice(&ms[2..]);
    b[6..].copy_from_slice(&random);
    b[6] = (b[6] & 0x0F) | 0x70;
    b[8] = (b[8] & 0x3F) | 0x80;
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// An id of the given kind from the clock and the random bytes given.
pub fn make(kind: Kind, unix_ms: u64, random: [u8; 10]) -> String {
    format!("{}-{}", kind.prefix(), uuid_v7(unix_ms, random))
}

/// A new id of the given kind, from the real clock and the system's random numbers.
pub fn new(kind: Kind) -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut random = [0u8; 10];
    rand::thread_rng().fill_bytes(&mut random);
    make(kind, ms, random)
}

/// Whether `text` is an id of the given kind: the prefix, a hyphen and a well-formed UUID version 7 in lower case.
pub fn is_valid(kind: Kind, text: &str) -> bool {
    let Some(uuid) = text
        .strip_prefix(kind.prefix())
        .and_then(|r| r.strip_prefix('-'))
    else {
        return false;
    };
    let parts: Vec<&str> = uuid.split('-').collect();
    let lengths = [8usize, 4, 4, 4, 12];
    parts.len() == 5
        && parts.iter().zip(lengths).all(|(p, n)| {
            p.len() == n
                && p.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
        && parts[2].starts_with('7')
        && matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_the_one_of_version_7() {
        let id = uuid_v7(0x0123_4567_89AB, [0xFF; 10]);
        // 48 bits of time, then the version nibble 7, then the variant 10xx
        assert_eq!(id, "01234567-89ab-7fff-bfff-ffffffffffff");
        let id = uuid_v7(0, [0; 10]);
        assert_eq!(id, "00000000-0000-7000-8000-000000000000");
    }

    #[test]
    fn a_later_clock_sorts_later_and_the_random_part_tells_equal_clocks_apart() {
        let a = make(Kind::Task, 1_000, [1; 10]);
        let b = make(Kind::Task, 1_001, [0; 10]);
        let c = make(Kind::Task, 1_001, [2; 10]);
        assert!(a < b, "{a} {b}");
        assert_ne!(b, c);
        // a time beyond 48 bits is cut, never spilled into the version field
        let big = uuid_v7(u64::MAX, [0; 10]);
        assert_eq!(big.as_bytes()[14], b'7');
    }

    #[test]
    fn the_prefixes_and_the_checks() {
        let id = new(Kind::Artifact);
        assert!(id.starts_with("a-") && is_valid(Kind::Artifact, &id));
        assert!(
            !is_valid(Kind::Task, &id),
            "an artifact id is not a task id"
        );
        assert!(!is_valid(Kind::Task, "t-nothing"));
        assert!(!is_valid(
            Kind::Task,
            "t-00000000-0000-4000-8000-000000000000"
        ));
        assert!(!is_valid(
            Kind::Task,
            "t-00000000-0000-7000-c000-000000000000"
        ));
        assert!(!is_valid(
            Kind::Task,
            "t-0000000G-0000-7000-8000-000000000000"
        ));
        assert!(is_valid(Kind::Upload, &new(Kind::Upload)));
        assert!(is_valid(Kind::Project, &new(Kind::Project)));
        let (x, y) = (new(Kind::Task), new(Kind::Task));
        assert_ne!(x, y);
    }
}
