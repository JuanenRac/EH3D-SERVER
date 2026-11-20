//! The compression the files of binary G-code use for their blocks when a printer with little memory reads them: an LZSS stream in which each item is a tag bit and then either
//! a literal byte (tag 1, 8 bits) or a copy of bytes that came before (tag 0, then the distance minus one in `window` bits and the length minus one in `lookahead` bits).
//! The bits are packed from the highest bit of each byte and the last byte is filled with zeros. The two shapes in use are a window of 11 bits with a lookahead of 4 and a
//! window of 12 bits with a lookahead of 4. This is written from that description of the stream and from nothing else; it works on plain bytes, reads no file and runs nothing.

/// Why a stream was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatshrinkError {
    /// The window or the lookahead is not one the stream can have.
    BadShape,
    /// The data ends before the bytes it was said to give.
    Truncated,
    /// A copy goes back further than the bytes that were given.
    BadDistance,
}

impl HeatshrinkError {
    /// A sentence for the person who reads a refusal.
    pub fn text(&self) -> &'static str {
        match self {
            HeatshrinkError::BadShape => {
                "the window or the lookahead of the compression is not valid"
            }
            HeatshrinkError::Truncated => {
                "the compressed data ends before the bytes it should give"
            }
            HeatshrinkError::BadDistance => {
                "a copy in the compressed data goes back before its start"
            }
        }
    }
}

fn check_shape(window: u8, lookahead: u8) -> Result<(), HeatshrinkError> {
    if (4..=15).contains(&window) && lookahead >= 3 && lookahead < window {
        Ok(())
    } else {
        Err(HeatshrinkError::BadShape)
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl BitReader<'_> {
    fn take(&mut self, n: u8) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.bit / 8)?;
            value = (value << 1) | u32::from((byte >> (7 - self.bit % 8)) & 1);
            self.bit += 1;
        }
        Some(value)
    }
}

/// Gives the `size` bytes that `data` holds, packed with a window of `window` bits and a lookahead of `lookahead` bits.
pub fn decode(
    data: &[u8],
    window: u8,
    lookahead: u8,
    size: usize,
) -> Result<Vec<u8>, HeatshrinkError> {
    check_shape(window, lookahead)?;
    let mut bits = BitReader { data, bit: 0 };
    let mut out: Vec<u8> = Vec::with_capacity(size.min(1 << 24));
    while out.len() < size {
        if bits.take(1).ok_or(HeatshrinkError::Truncated)? == 1 {
            let byte = bits.take(8).ok_or(HeatshrinkError::Truncated)?;
            out.push(byte as u8);
        } else {
            let back = bits.take(window).ok_or(HeatshrinkError::Truncated)? as usize + 1;
            let count = bits.take(lookahead).ok_or(HeatshrinkError::Truncated)? as usize + 1;
            if back > out.len() {
                return Err(HeatshrinkError::BadDistance);
            }
            for _ in 0..count.min(size - out.len()) {
                let byte = out[out.len() - back];
                out.push(byte);
            }
        }
    }
    Ok(out)
}

struct BitWriter {
    out: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    fn put(&mut self, value: u32, n: u8) {
        for i in (0..n).rev() {
            if self.bit.is_multiple_of(8) {
                self.out.push(0);
            }
            if (value >> i) & 1 == 1 {
                let last = self.out.len() - 1;
                self.out[last] |= 0x80 >> (self.bit % 8);
            }
            self.bit += 1;
        }
    }
}

/// Packs `data` with a window of `window` bits and a lookahead of `lookahead` bits: at each place the longest copy inside the window is taken when it is shorter to write than
/// the literals it replaces.
pub fn encode(data: &[u8], window: u8, lookahead: u8) -> Result<Vec<u8>, HeatshrinkError> {
    check_shape(window, lookahead)?;
    let max_back = 1usize << window;
    let max_len = 1usize << lookahead;
    // a copy takes 1 + window + lookahead bits and a literal takes 9
    let break_even = (1 + usize::from(window) + usize::from(lookahead)) / 9;
    let mut w = BitWriter {
        out: Vec::new(),
        bit: 0,
    };
    let mut at = 0;
    while at < data.len() {
        let (mut best_len, mut best_back) = (0usize, 0usize);
        let room = max_len.min(data.len() - at);
        for back in 1..=max_back.min(at) {
            let start = at - back;
            let mut len = 0;
            while len < room && data[start + len] == data[at + len] {
                len += 1;
            }
            if len > best_len {
                (best_len, best_back) = (len, back);
                if len == room {
                    break;
                }
            }
        }
        if best_len > break_even {
            w.put(0, 1);
            w.put((best_back - 1) as u32, window);
            w.put((best_len - 1) as u32, lookahead);
            at += best_len;
        } else {
            w.put(1, 1);
            w.put(u32::from(data[at]), 8);
            at += 1;
        }
    }
    Ok(w.out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_stream_is_the_one_the_description_gives() {
        // 'a' as a literal: tag 1 and the byte 0x61 -> bits 1 01100001 -> 0xB0 0x80
        assert_eq!(encode(b"a", 8, 4).unwrap(), vec![0xB0, 0x80]);
        // "aaaa": a literal, then a copy of three bytes one back (tag 0, distance-1 = 0 in 8 bits, length-1 = 2 in 4 bits)
        let packed = encode(b"aaaa", 8, 4).unwrap();
        assert_eq!(packed, vec![0xB0, 0x80, 0x08]);
        assert_eq!(decode(&packed, 8, 4, 4).unwrap(), b"aaaa");
    }

    #[test]
    fn what_is_packed_comes_back_for_both_shapes_and_for_all_kinds_of_data() {
        let mut inputs = vec![Vec::new(), b"x".to_vec(), vec![0u8; 5000], vec![255u8; 3]];
        inputs.push(bytes(1, 4000, 255));
        inputs.push(bytes(2, 9000, 4));
        inputs.push(b"G1 X10.5 Y20.25 E0.5 F1800\n".repeat(300));
        for (window, lookahead) in [(11u8, 4u8), (12, 4), (8, 3), (6, 5)] {
            for data in &inputs {
                let packed = encode(data, window, lookahead).unwrap();
                assert_eq!(
                    &decode(&packed, window, lookahead, data.len()).unwrap(),
                    data
                );
            }
        }
    }

    #[test]
    fn text_with_repeats_gets_smaller() {
        let text = b"G1 X10.5 Y20.25 E0.5 F1800\n".repeat(300);
        assert!(encode(&text, 12, 4).unwrap().len() < text.len() / 3);
    }

    #[test]
    fn bad_streams_are_refused_and_not_guessed() {
        assert_eq!(
            decode(&[], 11, 4, 1).unwrap_err(),
            HeatshrinkError::Truncated
        );
        // a copy at the very start has nothing to copy: tag 0, distance 1, length 1
        assert_eq!(
            decode(&[0x00, 0x00, 0x00], 11, 4, 5).unwrap_err(),
            HeatshrinkError::BadDistance
        );
        let packed = encode(&bytes(3, 100, 255), 11, 4).unwrap();
        assert_eq!(
            decode(&packed[..packed.len() - 5], 11, 4, 100).unwrap_err(),
            HeatshrinkError::Truncated
        );
        assert_eq!(
            decode(&packed, 3, 4, 1).unwrap_err(),
            HeatshrinkError::BadShape
        );
        assert_eq!(
            decode(&packed, 11, 11, 1).unwrap_err(),
            HeatshrinkError::BadShape
        );
        assert_eq!(encode(b"a", 16, 4).unwrap_err(), HeatshrinkError::BadShape);
        // asking for zero bytes asks for nothing
        assert!(decode(&[], 11, 4, 0).unwrap().is_empty());
    }
}
