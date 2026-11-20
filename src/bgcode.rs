//! Binary G-code (`.bgcode`), read and written from the public description of the format and from nothing else. A file is a header of 10 bytes (the magic `GCDE`, the version as
//! a 32-bit number, which is 1, and the kind of checksum as a 16-bit number: 0 none, 1 CRC-32) followed by blocks. A block is a header (its type, its compression, the size
//! of its data before compression and, only when it is compressed, the size after), the parameters of its type, the data and, when the file has a checksum, the CRC-32 of
//! the block in four bytes. All numbers are little-endian. The types are 0 file metadata, 1 G-code, 2 slicer metadata, 3 printer metadata, 4 print metadata and 5 thumbnail,
//! and they come in the order file metadata, printer metadata, thumbnails, print metadata, slicer metadata, G-code. The compressions are 0 none, 1 deflate, 2 heatshrink with a
//! window of 11 bits and a lookahead of 4 and 3 the same with a window of 12 bits. A metadata block has the parameter "encoding" (0, lines of `key=value`), a thumbnail has its
//! format (0 PNG, 1 JPG, 2 QOI), its width and its height, and a G-code block has the encoding of the text (0 plain, 1 and 2 packed in the MeatPack way).
//!
//! What is **not** done, and is refused and not guessed: the packed text of MeatPack (a block with that encoding is `bgcode_unsupported`; the packing of that text is not worked
//! out here until a real file can be compared with it) and anything the description does not name (another version, another type, another compression). The checksum is the
//! standard CRC-32 over the header, the parameters and the stored data of the block; no real `.bgcode` file has been read with it yet, only files this module wrote. The
//! compression "deflate" is written as stored blocks of a zlib stream (valid and not smaller); it is read whatever the blocks are.
//!
//! The module works on plain bytes and text only (it reads no file and runs nothing), so it is tried without the rest of the server.

use crate::heatshrink;
use crate::inflate;

/// The magic number at the start of a file.
pub const MAGIC: &[u8; 4] = b"GCDE";

/// The most bytes a block may give once it is read, unless the caller says another number.
pub const DEFAULT_MAX_BLOCK: usize = 64 * 1024 * 1024;

/// A refusal. `code` is one of the codes of the catalog of errors (`bad_bgcode` or `bgcode_unsupported`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BgError {
    pub code: &'static str,
    pub detail: String,
}

fn bad<T>(detail: impl Into<String>) -> Result<T, BgError> {
    Err(BgError {
        code: "bad_bgcode",
        detail: detail.into(),
    })
}

fn unsupported<T>(detail: impl Into<String>) -> Result<T, BgError> {
    Err(BgError {
        code: "bgcode_unsupported",
        detail: detail.into(),
    })
}

/// How the data of a block is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Deflate,
    /// Heatshrink with a window of 11 bits and a lookahead of 4.
    Heatshrink11,
    /// Heatshrink with a window of 12 bits and a lookahead of 4.
    Heatshrink12,
}

impl Compression {
    fn from_number(n: u16) -> Option<Compression> {
        match n {
            0 => Some(Compression::None),
            1 => Some(Compression::Deflate),
            2 => Some(Compression::Heatshrink11),
            3 => Some(Compression::Heatshrink12),
            _ => None,
        }
    }

    fn number(self) -> u16 {
        match self {
            Compression::None => 0,
            Compression::Deflate => 1,
            Compression::Heatshrink11 => 2,
            Compression::Heatshrink12 => 3,
        }
    }

    /// The word the answers of the server call it by.
    pub fn name(self) -> &'static str {
        match self {
            Compression::None => "none",
            Compression::Deflate => "deflate",
            Compression::Heatshrink11 => "heatshrink_11_4",
            Compression::Heatshrink12 => "heatshrink_12_4",
        }
    }
}

/// The kind of a block, in the order they come in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    FileMeta,
    PrinterMeta,
    Thumbnail,
    PrintMeta,
    SlicerMeta,
    Gcode,
}

impl Kind {
    fn from_number(n: u16) -> Option<Kind> {
        match n {
            0 => Some(Kind::FileMeta),
            1 => Some(Kind::Gcode),
            2 => Some(Kind::SlicerMeta),
            3 => Some(Kind::PrinterMeta),
            4 => Some(Kind::PrintMeta),
            5 => Some(Kind::Thumbnail),
            _ => None,
        }
    }

    fn number(self) -> u16 {
        match self {
            Kind::FileMeta => 0,
            Kind::Gcode => 1,
            Kind::SlicerMeta => 2,
            Kind::PrinterMeta => 3,
            Kind::PrintMeta => 4,
            Kind::Thumbnail => 5,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::FileMeta => "file metadata",
            Kind::PrinterMeta => "printer metadata",
            Kind::Thumbnail => "thumbnail",
            Kind::PrintMeta => "print metadata",
            Kind::SlicerMeta => "slicer metadata",
            Kind::Gcode => "G-code",
        }
    }

    /// The bytes of parameters the type has.
    fn params_len(self) -> usize {
        if self == Kind::Thumbnail {
            6
        } else {
            2
        }
    }
}

/// A picture of the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// 0 PNG, 1 JPG, 2 QOI.
    pub format: u16,
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

/// A list of `key=value` pairs, in the order the file has them.
pub type Meta = Vec<(String, String)>;

/// What a file holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bgcode {
    /// Whether the blocks end with a CRC-32.
    pub crc32: bool,
    pub file_meta: Meta,
    pub printer_meta: Meta,
    pub thumbnails: Vec<Thumbnail>,
    pub print_meta: Meta,
    pub slicer_meta: Meta,
    /// The G-code, all its blocks one after the other.
    pub gcode: String,
    /// The compression each block was read with, in file order (a summary for the answers).
    pub compressions: Vec<(String, Compression)>,
}

/// What a reader holds back.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The most bytes a block may give.
    pub max_block: usize,
    /// The most bytes of G-code in all.
    pub max_gcode: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_block: DEFAULT_MAX_BLOCK,
            max_gcode: DEFAULT_MAX_BLOCK,
        }
    }
}

fn le16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn parse_meta(raw: &[u8], what: &str) -> Result<Meta, BgError> {
    let text =
        std::str::from_utf8(raw).or_else(|_| bad(format!("the {what} block is not text")))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return bad(format!("a line of the {what} block has no ="));
        };
        out.push((k.trim().to_string(), v.trim().to_string()));
    }
    Ok(out)
}

fn decompress(
    kind: Kind,
    compression: Compression,
    stored: &[u8],
    size: usize,
) -> Result<Vec<u8>, BgError> {
    let name = kind.name();
    let out = match compression {
        Compression::None => stored.to_vec(),
        Compression::Deflate => inflate::inflate(stored, size)
            .or_else(|e| bad(format!("a {name} block: {}", e.text())))?,
        Compression::Heatshrink11 => heatshrink::decode(stored, 11, 4, size)
            .or_else(|e| bad(format!("a {name} block: {}", e.text())))?,
        Compression::Heatshrink12 => heatshrink::decode(stored, 12, 4, size)
            .or_else(|e| bad(format!("a {name} block: {}", e.text())))?,
    };
    if out.len() != size {
        return bad(format!(
            "a {name} block gives {} bytes and its header says {size}",
            out.len()
        ));
    }
    Ok(out)
}

/// Reads a file. A file that is cut, damaged, out of order or too big is a refusal; a block the module does not read (the packed text of MeatPack) is a refusal too, and no
/// part of the file is given back in that case.
pub fn read(bytes: &[u8], limits: &Limits) -> Result<Bgcode, BgError> {
    if bytes.len() < 10 {
        return bad("the file is shorter than its header");
    }
    if &bytes[..4] != MAGIC {
        return bad("the file does not start with GCDE");
    }
    let version = le32(bytes, 4).unwrap_or(0);
    if version != 1 {
        return unsupported(format!(
            "the version {version} of the format is not read (only 1)"
        ));
    }
    let crc32 = match le16(bytes, 8).unwrap_or(u16::MAX) {
        0 => false,
        1 => true,
        n => return bad(format!("the kind of checksum {n} does not exist")),
    };
    let mut file = Bgcode {
        crc32,
        ..Bgcode::default()
    };
    let (mut at, mut rank, mut seen) = (10usize, 0usize, 0u8);
    while at < bytes.len() {
        let start = at;
        let cut = || BgError {
            code: "bad_bgcode",
            detail: format!("the file is cut in the block that starts at byte {start}"),
        };
        let kind_number = le16(bytes, at).ok_or_else(cut)?;
        let kind = Kind::from_number(kind_number).ok_or_else(|| BgError {
            code: "bad_bgcode",
            detail: format!(
                "the block at byte {start} has a type ({kind_number}) that does not exist"
            ),
        })?;
        let compression_number = le16(bytes, at + 2).ok_or_else(cut)?;
        let compression = Compression::from_number(compression_number).ok_or_else(|| BgError {
            code: "bad_bgcode",
            detail: format!("the block at byte {start} has a compression ({compression_number}) that does not exist"),
        })?;
        let size = le32(bytes, at + 4).ok_or_else(cut)? as usize;
        let (stored_len, header_len) = if compression == Compression::None {
            (size, 8)
        } else {
            (le32(bytes, at + 8).ok_or_else(cut)? as usize, 12)
        };
        if size > limits.max_block {
            return bad(format!(
                "the block at byte {start} gives more than the {} bytes allowed",
                limits.max_block
            ));
        }
        let params_at = at + header_len;
        let data_at = params_at + kind.params_len();
        let data_end = data_at.checked_add(stored_len).ok_or_else(cut)?;
        let end = data_end + if crc32 { 4 } else { 0 };
        if end > bytes.len() {
            return Err(cut());
        }
        if crc32 {
            let want = le32(bytes, data_end).ok_or_else(cut)?;
            if inflate::crc32(&bytes[start..data_end]) != want {
                return bad(format!(
                    "the checksum of the block at byte {start} is not the one of its bytes"
                ));
            }
        }
        // the order: a type does not come after one that follows it, and the metadata come once
        let this = kind as usize;
        if this < rank {
            return bad(format!(
                "the {} block at byte {start} is out of order",
                kind.name()
            ));
        }
        if matches!(
            kind,
            Kind::FileMeta | Kind::PrinterMeta | Kind::PrintMeta | Kind::SlicerMeta
        ) {
            let bit = 1u8 << this;
            if seen & bit != 0 {
                return bad(format!("there are two {} blocks", kind.name()));
            }
            seen |= bit;
        }
        rank = this;
        let params = &bytes[params_at..data_at];
        let raw_params = le16(params, 0).ok_or_else(cut)?;
        match kind {
            Kind::Gcode => match raw_params {
                0 => {}
                1 | 2 => {
                    return unsupported(
                        "a G-code block is packed with MeatPack, which this server does not read yet; slice again without it",
                    )
                }
                n => return bad(format!("the G-code block at byte {start} has an encoding ({n}) that does not exist")),
            },
            Kind::Thumbnail => {
                if raw_params > 2 {
                    return bad(format!("the thumbnail at byte {start} has a format ({raw_params}) that does not exist"));
                }
            }
            _ => {
                if raw_params != 0 {
                    return bad(format!(
                        "the {} block at byte {start} has an encoding ({raw_params}) that does not exist",
                        kind.name()
                    ));
                }
            }
        }
        let raw = decompress(kind, compression, &bytes[data_at..data_end], size)?;
        file.compressions
            .push((kind.name().to_string(), compression));
        match kind {
            Kind::FileMeta => file.file_meta = parse_meta(&raw, kind.name())?,
            Kind::PrinterMeta => file.printer_meta = parse_meta(&raw, kind.name())?,
            Kind::PrintMeta => file.print_meta = parse_meta(&raw, kind.name())?,
            Kind::SlicerMeta => file.slicer_meta = parse_meta(&raw, kind.name())?,
            Kind::Thumbnail => file.thumbnails.push(Thumbnail {
                format: raw_params,
                width: le16(params, 2).ok_or_else(cut)?,
                height: le16(params, 4).ok_or_else(cut)?,
                data: raw,
            }),
            Kind::Gcode => {
                if file.gcode.len() + raw.len() > limits.max_gcode {
                    return bad(format!(
                        "the G-code is more than the {} bytes allowed",
                        limits.max_gcode
                    ));
                }
                let text = String::from_utf8(raw)
                    .or_else(|_| bad(format!("the G-code block at byte {start} is not text")))?;
                file.gcode.push_str(&text);
            }
        }
        at = end;
    }
    Ok(file)
}

/// How a file is written.
#[derive(Debug, Clone, Copy)]
pub struct WriteOptions {
    pub crc32: bool,
    pub metadata: Compression,
    pub gcode: Compression,
    /// The most bytes of G-code in a block (a block ends at a line break; a line longer than this is a block of its own).
    pub gcode_block: usize,
}

impl Default for WriteOptions {
    fn default() -> WriteOptions {
        WriteOptions {
            crc32: true,
            metadata: Compression::None,
            gcode: Compression::Heatshrink12,
            gcode_block: 64 * 1024,
        }
    }
}

fn pack(compression: Compression, raw: &[u8]) -> Result<Vec<u8>, BgError> {
    match compression {
        Compression::None => Ok(raw.to_vec()),
        Compression::Deflate => Ok(inflate::zlib_stored(raw)),
        Compression::Heatshrink11 => heatshrink::encode(raw, 11, 4).or_else(|e| bad(e.text())),
        Compression::Heatshrink12 => heatshrink::encode(raw, 12, 4).or_else(|e| bad(e.text())),
    }
}

fn put_block(
    out: &mut Vec<u8>,
    kind: Kind,
    compression: Compression,
    params: &[u8],
    raw: &[u8],
    crc32: bool,
) -> Result<(), BgError> {
    let size = u32::try_from(raw.len()).or_else(|_| bad("a block is bigger than 4 GiB"))?;
    let start = out.len();
    out.extend_from_slice(&kind.number().to_le_bytes());
    out.extend_from_slice(&compression.number().to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    let stored = if compression == Compression::None {
        raw.to_vec()
    } else {
        let packed = pack(compression, raw)?;
        let packed_len =
            u32::try_from(packed.len()).or_else(|_| bad("a block is bigger than 4 GiB"))?;
        out.extend_from_slice(&packed_len.to_le_bytes());
        packed
    };
    out.extend_from_slice(params);
    out.extend_from_slice(&stored);
    if crc32 {
        let sum = inflate::crc32(&out[start..]);
        out.extend_from_slice(&sum.to_le_bytes());
    }
    Ok(())
}

fn meta_text(meta: &Meta, what: &str) -> Result<Vec<u8>, BgError> {
    let mut text = String::new();
    for (k, v) in meta {
        if k.is_empty() || k.contains(['=', '\n', '\r']) || v.contains(['\n', '\r']) {
            return bad(format!(
                "a key or a value of the {what} has a line break (or the key an =, or is empty)"
            ));
        }
        text.push_str(k);
        text.push('=');
        text.push_str(v);
        text.push('\n');
    }
    Ok(text.into_bytes())
}

/// Writes a file. The blocks come in the order of the format; a block of metadata with nothing in it is left out.
pub fn write(file: &Bgcode, options: &WriteOptions) -> Result<Vec<u8>, BgError> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&u16::from(options.crc32).to_le_bytes());
    let metas: [(Kind, &Meta); 2] = [
        (Kind::FileMeta, &file.file_meta),
        (Kind::PrinterMeta, &file.printer_meta),
    ];
    for (kind, meta) in metas {
        if !meta.is_empty() {
            let raw = meta_text(meta, kind.name())?;
            put_block(
                &mut out,
                kind,
                options.metadata,
                &0u16.to_le_bytes(),
                &raw,
                options.crc32,
            )?;
        }
    }
    for t in &file.thumbnails {
        if t.format > 2 {
            return bad("the format of a thumbnail is 0 (PNG), 1 (JPG) or 2 (QOI)");
        }
        let mut params = Vec::with_capacity(6);
        params.extend_from_slice(&t.format.to_le_bytes());
        params.extend_from_slice(&t.width.to_le_bytes());
        params.extend_from_slice(&t.height.to_le_bytes());
        put_block(
            &mut out,
            Kind::Thumbnail,
            Compression::None,
            &params,
            &t.data,
            options.crc32,
        )?;
    }
    let metas: [(Kind, &Meta); 2] = [
        (Kind::PrintMeta, &file.print_meta),
        (Kind::SlicerMeta, &file.slicer_meta),
    ];
    for (kind, meta) in metas {
        if !meta.is_empty() {
            let raw = meta_text(meta, kind.name())?;
            put_block(
                &mut out,
                kind,
                options.metadata,
                &0u16.to_le_bytes(),
                &raw,
                options.crc32,
            )?;
        }
    }
    let block = options.gcode_block.max(1);
    let mut rest = file.gcode.as_str();
    while !rest.is_empty() {
        let mut cut = rest.len().min(block);
        if cut < rest.len() {
            // the block ends after the last line break inside the limit (or after the first one past it when a line is longer)
            cut = match rest[..cut].rfind('\n') {
                Some(i) => i + 1,
                None => rest.find('\n').map_or(rest.len(), |i| i + 1),
            };
        }
        let (head, tail) = rest.split_at(cut);
        put_block(
            &mut out,
            Kind::Gcode,
            options.gcode,
            &0u16.to_le_bytes(),
            head.as_bytes(),
            options.crc32,
        )?;
        rest = tail;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pairs: &[(&str, &str)]) -> Meta {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn program(lines: usize) -> String {
        let mut s = String::new();
        for i in 0..lines {
            s.push_str(&format!(
                "G1 X{}.{} Y{}.5 E0.{}\n",
                i % 200,
                i % 10,
                (i * 7) % 180,
                i % 90
            ));
            if i % 50 == 0 {
                s.push_str(";LAYER_CHANGE\n");
            }
        }
        s
    }

    fn sample() -> Bgcode {
        Bgcode {
            crc32: true,
            file_meta: meta(&[("Producer", "a test")]),
            printer_meta: meta(&[("printer_model", "A1"), ("filament_type", "PLA")]),
            thumbnails: vec![
                Thumbnail {
                    format: 0,
                    width: 16,
                    height: 8,
                    data: vec![1, 2, 3, 4, 5],
                },
                Thumbnail {
                    format: 2,
                    width: 32,
                    height: 32,
                    data: (0..=255).collect(),
                },
            ],
            print_meta: meta(&[("layers", "12"), ("estimated printing time", "1h 2m")]),
            slicer_meta: meta(&[("layer_height", "0.2")]),
            gcode: program(700),
            compressions: Vec::new(),
        }
    }

    fn same(a: &Bgcode, b: &Bgcode) {
        assert_eq!(a.file_meta, b.file_meta);
        assert_eq!(a.printer_meta, b.printer_meta);
        assert_eq!(a.thumbnails, b.thumbnails);
        assert_eq!(a.print_meta, b.print_meta);
        assert_eq!(a.slicer_meta, b.slicer_meta);
        assert_eq!(a.gcode, b.gcode);
    }

    #[test]
    fn a_file_goes_back_and_forth_with_every_compression_and_checksum() {
        let file = sample();
        for crc32 in [true, false] {
            for metadata in [
                Compression::None,
                Compression::Deflate,
                Compression::Heatshrink11,
                Compression::Heatshrink12,
            ] {
                for gcode in [
                    Compression::None,
                    Compression::Deflate,
                    Compression::Heatshrink11,
                    Compression::Heatshrink12,
                ] {
                    let options = WriteOptions {
                        crc32,
                        metadata,
                        gcode,
                        gcode_block: 4096,
                    };
                    let bytes = write(&file, &options).unwrap();
                    let back = read(&bytes, &Limits::default()).unwrap();
                    same(&file, &back);
                    assert_eq!(back.crc32, crc32);
                }
            }
        }
    }

    #[test]
    fn the_header_and_the_blocks_are_laid_out_as_the_format_says() {
        let file = Bgcode {
            crc32: true,
            file_meta: meta(&[("a", "b")]),
            gcode: "G28\n".into(),
            ..Bgcode::default()
        };
        let options = WriteOptions {
            gcode: Compression::None,
            ..WriteOptions::default()
        };
        let bytes = write(&file, &options).unwrap();
        assert_eq!(&bytes[..10], &[b'G', b'C', b'D', b'E', 1, 0, 0, 0, 1, 0]);
        // file metadata block: type 0, no compression, 4 bytes ("a=b\n"), encoding 0, the data and the CRC
        assert_eq!(&bytes[10..18], &[0, 0, 0, 0, 4, 0, 0, 0]);
        assert_eq!(&bytes[18..20], &[0, 0]);
        assert_eq!(&bytes[20..24], b"a=b\n");
        let crc = inflate::crc32(&bytes[10..24]);
        assert_eq!(&bytes[24..28], &crc.to_le_bytes());
        // the G-code block: type 1
        assert_eq!(&bytes[28..30], &[1, 0]);
        // a compressed block has the two sizes
        let packed = write(
            &file,
            &WriteOptions {
                gcode: Compression::Heatshrink12,
                ..options
            },
        )
        .unwrap();
        assert_eq!(&packed[28..32], &[1, 0, 3, 0]);
        assert_eq!(&packed[32..36], &[4, 0, 0, 0]);
    }

    #[test]
    fn blocks_are_cut_at_line_breaks() {
        let file = Bgcode {
            gcode: program(500),
            ..Bgcode::default()
        };
        let options = WriteOptions {
            gcode: Compression::None,
            gcode_block: 1000,
            ..WriteOptions::default()
        };
        let bytes = write(&file, &options).unwrap();
        let back = read(&bytes, &Limits::default()).unwrap();
        assert_eq!(back.gcode, file.gcode);
        let blocks = back
            .compressions
            .iter()
            .filter(|(k, _)| k == "G-code")
            .count();
        assert!(blocks > 5, "{blocks}");
        // one line longer than the block is a block of its own and nothing is lost
        let long = Bgcode {
            gcode: format!("{}\nG28\n", "; ".repeat(3000)),
            ..Bgcode::default()
        };
        let back = read(&write(&long, &options).unwrap(), &Limits::default()).unwrap();
        assert_eq!(back.gcode, long.gcode);
    }

    #[test]
    fn a_flipped_bit_is_caught_by_the_checksum() {
        let file = sample();
        let options = WriteOptions {
            gcode_block: 4096,
            ..WriteOptions::default()
        };
        let bytes = write(&file, &options).unwrap();
        for at in (10..bytes.len()).step_by(97) {
            let mut damaged = bytes.clone();
            damaged[at] ^= 0x10;
            assert!(read(&damaged, &Limits::default()).is_err(), "byte {at}");
        }
    }

    #[test]
    fn a_file_that_is_cut_anywhere_is_refused_and_never_panics() {
        let file = Bgcode {
            gcode: program(40),
            thumbnails: sample().thumbnails,
            ..sample()
        };
        for crc32 in [true, false] {
            let options = WriteOptions {
                crc32,
                gcode_block: 300,
                ..WriteOptions::default()
            };
            let bytes = write(&file, &options).unwrap();
            // a cut that reads is a cut at the end of a block, and what it gives is the start of the whole
            for n in 0..bytes.len() {
                if let Ok(part) = read(&bytes[..n], &Limits::default()) {
                    assert!(file.gcode.starts_with(&part.gcode), "cut at {n}");
                }
            }
            for n in (0..10).chain(11..14) {
                assert_eq!(
                    read(&bytes[..n], &Limits::default()).unwrap_err().code,
                    "bad_bgcode"
                );
            }
            assert!(read(&bytes[..bytes.len() - 1], &Limits::default()).is_err());
        }
    }

    #[test]
    fn what_the_format_does_not_name_is_refused() {
        let good = write(
            &sample(),
            &WriteOptions {
                crc32: false,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        let first = 10;
        // a type that does not exist
        let mut a = good.clone();
        a[first] = 9;
        assert!(read(&a, &Limits::default())
            .unwrap_err()
            .detail
            .contains("type"));
        // a compression that does not exist
        let mut b = good.clone();
        b[first + 2] = 7;
        assert!(read(&b, &Limits::default())
            .unwrap_err()
            .detail
            .contains("compression"));
        // a version and a magic and a kind of checksum that do not exist
        let mut c = good.clone();
        c[4] = 2;
        assert_eq!(
            read(&c, &Limits::default()).unwrap_err().code,
            "bgcode_unsupported"
        );
        let mut d = good.clone();
        d[0] = b'X';
        assert!(read(&d, &Limits::default())
            .unwrap_err()
            .detail
            .contains("GCDE"));
        let mut e = good.clone();
        e[8] = 5;
        assert_eq!(read(&e, &Limits::default()).unwrap_err().code, "bad_bgcode");
        // an encoding of metadata that does not exist
        let mut f = good.clone();
        f[first + 8] = 3;
        assert_eq!(read(&f, &Limits::default()).unwrap_err().code, "bad_bgcode");
    }

    #[test]
    fn meatpack_is_refused_with_its_own_code() {
        let file = Bgcode {
            gcode: "G28\n".into(),
            ..Bgcode::default()
        };
        let mut bytes = write(
            &file,
            &WriteOptions {
                crc32: false,
                gcode: Compression::None,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        // the G-code block is the first one: type, compression, size, then the encoding
        assert_eq!(&bytes[10..12], &[1, 0]);
        for encoding in [1u8, 2] {
            bytes[18] = encoding;
            assert_eq!(
                read(&bytes, &Limits::default()).unwrap_err().code,
                "bgcode_unsupported"
            );
        }
        bytes[18] = 3;
        assert_eq!(
            read(&bytes, &Limits::default()).unwrap_err().code,
            "bad_bgcode"
        );
    }

    #[test]
    fn the_order_and_the_limits_are_held() {
        let options = WriteOptions {
            crc32: false,
            gcode: Compression::None,
            ..WriteOptions::default()
        };
        let one = write(
            &Bgcode {
                gcode: "G28\n".into(),
                ..Bgcode::default()
            },
            &options,
        )
        .unwrap();
        let two = write(
            &Bgcode {
                file_meta: meta(&[("a", "b")]),
                ..Bgcode::default()
            },
            &options,
        )
        .unwrap();
        // G-code and then file metadata
        let mut out = one.clone();
        out.extend_from_slice(&two[10..]);
        assert!(read(&out, &Limits::default())
            .unwrap_err()
            .detail
            .contains("out of order"));
        // two blocks of file metadata
        let mut twice = two.clone();
        twice.extend_from_slice(&two[10..]);
        assert!(read(&twice, &Limits::default())
            .unwrap_err()
            .detail
            .contains("two"));
        // limits
        let big = write(
            &Bgcode {
                gcode: program(1000),
                ..Bgcode::default()
            },
            &WriteOptions::default(),
        )
        .unwrap();
        assert!(read(
            &big,
            &Limits {
                max_block: 100,
                max_gcode: 1 << 20
            }
        )
        .is_err());
        assert!(read(
            &big,
            &Limits {
                max_block: 1 << 20,
                max_gcode: 100
            }
        )
        .is_err());
        assert!(read(&big, &Limits::default()).is_ok());
        // a size said in the header that the data does not give
        let mut lying = write(
            &Bgcode {
                gcode: "G28\nG28\n".into(),
                ..Bgcode::default()
            },
            &WriteOptions {
                crc32: false,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        lying[14] = 30;
        assert!(read(&lying, &Limits::default()).is_err());
    }

    #[test]
    fn what_cannot_be_written_is_refused() {
        let o = WriteOptions::default();
        let bad_key = Bgcode {
            file_meta: meta(&[("a=b", "c")]),
            ..Bgcode::default()
        };
        assert_eq!(write(&bad_key, &o).unwrap_err().code, "bad_bgcode");
        let bad_value = Bgcode {
            file_meta: meta(&[("a", "b\nc")]),
            ..Bgcode::default()
        };
        assert!(write(&bad_value, &o).is_err());
        let bad_format = Bgcode {
            thumbnails: vec![Thumbnail {
                format: 3,
                width: 1,
                height: 1,
                data: vec![],
            }],
            ..Bgcode::default()
        };
        assert!(write(&bad_format, &o).is_err());
        // an empty file is a header and nothing else, and reads back as empty
        let empty = write(&Bgcode::default(), &o).unwrap();
        assert_eq!(empty.len(), 10);
        assert_eq!(read(&empty, &Limits::default()).unwrap().gcode, "");
    }
}
