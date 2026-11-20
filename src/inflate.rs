//! The small pieces of compression the server needs and nothing else: the CRC-32 of the checksums of a file, the checksum of a zlib stream, a reader of deflate data (RFC 1951,
//! raw or wrapped as zlib, RFC 1950) and a writer that wraps data in deflate blocks that are stored (valid, and not smaller). They are written from the public descriptions of the
//! formats, work on plain bytes, read no file and run nothing, so they are tried without the rest of the server. A reader never gives more bytes than the limit it was told, so a
//! small input cannot grow without bound.

/// Why a stream was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateError {
    /// The data ends before the stream does.
    Truncated,
    /// The data is not a valid stream (a block type, a table, a distance or a checksum that cannot be).
    Damaged(&'static str),
    /// The stream would give more bytes than the limit.
    TooBig,
}

impl InflateError {
    /// A sentence for the person who reads a refusal.
    pub fn text(&self) -> String {
        match self {
            InflateError::Truncated => "the compressed data ends before the stream does".into(),
            InflateError::Damaged(why) => format!("the compressed data is damaged: {why}"),
            InflateError::TooBig => "the compressed data gives more bytes than the limit".into(),
        }
    }
}

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    table
}

/// The CRC-32 (the one of zip, PNG and gzip) of `data`, going on from `crc` (0 to start).
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let table = crc_table();
    let mut c = !crc;
    for b in data {
        c = table[((c ^ u32::from(*b)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// The CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

/// The Adler-32 checksum that ends a zlib stream.
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for byte in chunk {
            a += u32::from(*byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    bit: u32,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Bits<'a> {
        Bits {
            data,
            at: 0,
            bit: 0,
            count: 0,
        }
    }

    /// `n` bits (up to 16), the first one read being the lowest.
    fn take(&mut self, n: u32) -> Result<u32, InflateError> {
        while self.count < n {
            let byte = *self.data.get(self.at).ok_or(InflateError::Truncated)?;
            self.at += 1;
            self.bit |= u32::from(byte) << self.count;
            self.count += 8;
        }
        let value = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.count -= n;
        Ok(value)
    }

    /// Drops the bits that are left of the current byte.
    fn align(&mut self) {
        self.bit = 0;
        self.count = 0;
    }
}

/// A canonical Huffman code: how many codes there are of each length and the symbols in the order of their codes.
struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

fn build(lengths: &[u8]) -> Result<Huffman, InflateError> {
    let mut count = [0u16; 16];
    for l in lengths {
        count[usize::from(*l)] += 1;
    }
    let mut left: i32 = 1;
    for &c in &count[1..] {
        left <<= 1;
        left -= i32::from(c);
        if left < 0 {
            return Err(InflateError::Damaged("a table of codes is over-subscribed"));
        }
    }
    let mut offs = [0u16; 16];
    for len in 1..15 {
        offs[len + 1] = offs[len] + count[len];
    }
    let mut symbol = vec![0u16; lengths.len()];
    for (sym, l) in lengths.iter().enumerate() {
        if *l != 0 {
            symbol[usize::from(offs[usize::from(*l)])] = sym as u16;
            offs[usize::from(*l)] += 1;
        }
    }
    Ok(Huffman { count, symbol })
}

fn decode(bits: &mut Bits, h: &Huffman) -> Result<u16, InflateError> {
    let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
    for &c in &h.count[1..] {
        code |= bits.take(1)? as i32;
        let count = i32::from(c);
        if code - count < first {
            return h
                .symbol
                .get((index + (code - first)) as usize)
                .copied()
                .ok_or(InflateError::Damaged("a code that is not in its table"));
        }
        index += count;
        first += count;
        first <<= 1;
        code <<= 1;
    }
    Err(InflateError::Damaged("a code that is not in its table"))
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn fixed_tables() -> Result<(Huffman, Huffman), InflateError> {
    let mut lengths = [0u8; 288];
    for (i, l) in lengths.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((build(&lengths)?, build(&[5u8; 30])?))
}

fn dynamic_tables(bits: &mut Bits) -> Result<(Huffman, Huffman), InflateError> {
    let nlen = bits.take(5)? as usize + 257;
    let ndist = bits.take(5)? as usize + 1;
    let ncode = bits.take(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(InflateError::Damaged("too many codes in a table"));
    }
    let mut lengths = [0u8; 320];
    for &slot in CODE_LENGTH_ORDER.iter().take(ncode) {
        lengths[slot] = bits.take(3)? as u8;
    }
    let code_table = build(&lengths[..19])?;
    let mut lengths = [0u8; 320];
    let mut index = 0;
    while index < nlen + ndist {
        let sym = decode(bits, &code_table)?;
        if sym < 16 {
            lengths[index] = sym as u8;
            index += 1;
        } else {
            let (value, repeat) = match sym {
                16 => {
                    if index == 0 {
                        return Err(InflateError::Damaged("a repeat with nothing to repeat"));
                    }
                    (lengths[index - 1], 3 + bits.take(2)? as usize)
                }
                17 => (0, 3 + bits.take(3)? as usize),
                _ => (0, 11 + bits.take(7)? as usize),
            };
            if index + repeat > nlen + ndist {
                return Err(InflateError::Damaged("a repeat goes past the table"));
            }
            for slot in &mut lengths[index..index + repeat] {
                *slot = value;
            }
            index += repeat;
        }
    }
    if lengths[256] == 0 {
        return Err(InflateError::Damaged("a table without the end of block"));
    }
    Ok((
        build(&lengths[..nlen])?,
        build(&lengths[nlen..nlen + ndist])?,
    ))
}

fn codes(
    bits: &mut Bits,
    out: &mut Vec<u8>,
    max_out: usize,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), InflateError> {
    loop {
        let sym = usize::from(decode(bits, lit)?);
        if sym < 256 {
            if out.len() >= max_out {
                return Err(InflateError::TooBig);
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let s = sym - 257;
            if s >= 29 {
                return Err(InflateError::Damaged("a length that does not exist"));
            }
            let len = usize::from(LENGTH_BASE[s]) + bits.take(u32::from(LENGTH_EXTRA[s]))? as usize;
            let d = usize::from(decode(bits, dist)?);
            if d >= 30 {
                return Err(InflateError::Damaged("a distance that does not exist"));
            }
            let back = usize::from(DIST_BASE[d]) + bits.take(u32::from(DIST_EXTRA[d]))? as usize;
            if back > out.len() {
                return Err(InflateError::Damaged("a distance before the start"));
            }
            if out.len() + len > max_out {
                return Err(InflateError::TooBig);
            }
            for _ in 0..len {
                let byte = out[out.len() - back];
                out.push(byte);
            }
        }
    }
}

/// Reads raw deflate data (what a zip entry holds) up to `max_out` bytes.
pub fn inflate_raw(data: &[u8], max_out: usize) -> Result<Vec<u8>, InflateError> {
    let mut bits = Bits::new(data);
    let mut out = Vec::new();
    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            0 => {
                bits.align();
                let at = bits.at;
                let head = data.get(at..at + 4).ok_or(InflateError::Truncated)?;
                let len = usize::from(u16::from_le_bytes([head[0], head[1]]));
                let inv = u16::from_le_bytes([head[2], head[3]]);
                if inv != !(len as u16) {
                    return Err(InflateError::Damaged("a stored block with a wrong length"));
                }
                let body = data
                    .get(at + 4..at + 4 + len)
                    .ok_or(InflateError::Truncated)?;
                if out.len() + len > max_out {
                    return Err(InflateError::TooBig);
                }
                out.extend_from_slice(body);
                bits.at = at + 4 + len;
            }
            1 => {
                let (lit, dist) = fixed_tables()?;
                codes(&mut bits, &mut out, max_out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut bits)?;
                codes(&mut bits, &mut out, max_out, &lit, &dist)?;
            }
            _ => {
                return Err(InflateError::Damaged(
                    "a block of a type that does not exist",
                ))
            }
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

/// Whether the two first bytes are the header of a zlib stream (deflate, a window up to 32 KiB, no preset dictionary, and the check of the header).
fn is_zlib_header(data: &[u8]) -> bool {
    match data {
        [cmf, flg, ..] => {
            cmf & 0x0F == 8
                && cmf >> 4 <= 7
                && flg & 0x20 == 0
                && (u32::from(*cmf) * 256 + u32::from(*flg)) % 31 == 0
        }
        _ => false,
    }
}

/// Reads deflate data that may be wrapped as zlib: with the header of zlib the checksum at the end is checked, without it the data is raw.
pub fn inflate(data: &[u8], max_out: usize) -> Result<Vec<u8>, InflateError> {
    if !is_zlib_header(data) {
        return inflate_raw(data, max_out);
    }
    let out = inflate_raw(&data[2..], max_out)?;
    // the stream is followed by the Adler-32 of what it gives; the length of the deflate part is not known, so the checksum is the last four bytes
    let tail = data
        .get(data.len().saturating_sub(4)..)
        .filter(|t| t.len() == 4)
        .ok_or(InflateError::Truncated)?;
    let want = u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]);
    if want != adler32(&out) {
        return Err(InflateError::Damaged(
            "the checksum of the stream is not the one of its data",
        ));
    }
    Ok(out)
}

/// Deflate data that holds `data` in stored blocks (raw, with no wrapper).
pub fn deflate_stored_raw(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65_535 * 5 + 5);
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
        return out;
    }
    let mut chunks = data.chunks(65_535).peekable();
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// A zlib stream that holds `data` in stored blocks.
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&deflate_stored_raw(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seeded generator of bytes, so a test is always the same.
    fn bytes(seed: u64, n: usize, alphabet: u8) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((s >> 33) as u8) % alphabet
            })
            .collect()
    }

    #[test]
    fn the_checksums_are_the_known_ones() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32_update(crc32(b"1234"), b"56789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
    }

    #[test]
    fn a_stored_stream_goes_back_and_forth() {
        for n in [0usize, 1, 100, 65_535, 65_536, 140_000] {
            let data = bytes(n as u64 + 3, n, 255);
            assert_eq!(
                inflate_raw(&deflate_stored_raw(&data), usize::MAX).unwrap(),
                data
            );
            assert_eq!(inflate(&zlib_stored(&data), usize::MAX).unwrap(), data);
        }
    }

    #[test]
    fn a_stream_with_fixed_codes_of_another_program_is_read() {
        // "hello hello hello hello" packed by zlib at level 9 (fixed codes)
        let packed = [
            120, 218, 203, 72, 205, 201, 201, 87, 200, 64, 39, 1, 104, 3, 8, 177,
        ];
        assert_eq!(inflate(&packed, 100).unwrap(), b"hello hello hello hello");
        assert_eq!(
            inflate(&packed[2..packed.len() - 4], 100).unwrap(),
            b"hello hello hello hello"
        );
        assert_eq!(inflate(&packed, 22).unwrap_err(), InflateError::TooBig);
    }

    #[test]
    fn a_stream_with_dynamic_codes_of_another_program_is_read() {
        // 600 bytes made by the formula below, packed by zlib at level 9 (the block has its own tables of codes)
        let packed = [
            120, 218, 237, 203, 65, 13, 0, 48, 12, 195, 64, 172, 81, 211, 102, 26, 127, 0, 51, 130,
            33, 232, 239, 30, 182, 219, 138, 221, 170, 17, 188, 130, 83, 208, 129, 46, 24, 193, 35,
            120, 104, 21, 218, 242, 110, 187, 253, 182, 7, 153, 166, 233, 222,
        ];
        let want: Vec<u8> = (0u32..600)
            .map(|i| (97 + ((i * i + 3 * i) % 11) % 7 + if i % 5 == 0 { 3 } else { 0 }) as u8)
            .collect();
        assert_eq!(inflate(&packed, 1000).unwrap(), want);
        let mut damaged = packed;
        damaged[20] ^= 0x55;
        assert!(inflate(&damaged, 1000).is_err());
    }

    #[test]
    fn a_stream_that_is_wrong_is_refused() {
        let data = bytes(5, 500, 200);
        let mut z = zlib_stored(&data);
        assert_eq!(
            inflate(&z[..z.len() - 10], usize::MAX).unwrap_err(),
            InflateError::Truncated
        );
        // a wrong checksum
        let last = z.len() - 1;
        z[last] ^= 1;
        assert!(matches!(
            inflate(&z, usize::MAX),
            Err(InflateError::Damaged(_))
        ));
        // a stored block with a length that does not match its complement
        let mut raw = deflate_stored_raw(&data);
        raw[3] ^= 0xFF;
        assert!(matches!(
            inflate_raw(&raw, usize::MAX),
            Err(InflateError::Damaged(_))
        ));
        // the type 3 does not exist
        assert!(matches!(
            inflate_raw(&[0b0000_0111], 10),
            Err(InflateError::Damaged(_))
        ));
        // nothing at all
        assert_eq!(inflate_raw(&[], 10).unwrap_err(), InflateError::Truncated);
    }

    #[test]
    fn the_limit_holds() {
        let data = vec![7u8; 1000];
        assert_eq!(
            inflate_raw(&deflate_stored_raw(&data), 999).unwrap_err(),
            InflateError::TooBig
        );
        assert_eq!(
            inflate_raw(&deflate_stored_raw(&data), 1000).unwrap().len(),
            1000
        );
    }

    #[test]
    fn a_zlib_header_is_told_from_raw_data() {
        assert!(is_zlib_header(&[0x78, 0x01]));
        assert!(is_zlib_header(&[0x78, 0x9c]));
        assert!(is_zlib_header(&[0x78, 0xda]));
        assert!(!is_zlib_header(&[0x78, 0x02]));
        assert!(!is_zlib_header(&[0x01]));
        assert!(!is_zlib_header(&[]));
    }
}
