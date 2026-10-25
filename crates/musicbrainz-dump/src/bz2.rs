// Vendored from wikidata-rs 0.3.1 (crates/wikidata-dump/src), same authors:
// the parallel bzip2 block decoder. Keep in step with fixes made there.

//! Parallel bzip2 decoding.
//!
//! A bzip2 file is one or more streams (`BZh1`…`BZh9`, byte-aligned), each a
//! run of blocks that start with the 48-bit magic `0x314159265359` and end
//! with `0x177245385090`, the stream's combined CRC and padding to a byte.
//! Blocks are bit-aligned and their lengths are not recorded anywhere, but
//! each block is self-contained: it carries its own Huffman tables and CRC.
//!
//! Wikimedia's `wikidata-*-all.json.bz2` is a single stream of ~900 kB
//! blocks (checked on the 2026-10-12 dump: one `BZh9` header at offset 0),
//! so streams cannot be split at byte boundaries the way `pbzip2` files can.
//! Instead [`Scanner`] finds the bit positions of every block and
//! end-of-stream magic, and cuts the input into [`Piece`]s, one per block,
//! copied to byte alignment. A worker wraps a block piece into a one-block
//! stream (`BZh9`, the block, an end-of-stream marker whose combined CRC is
//! the block's own CRC) and decodes it with libbzip2 (the `bzip2` crate,
//! here its pure-Rust port), which checks the block's CRC. Results come back
//! in file order through [`crate::parallel::ordered_map`]; [`Blocks`] then
//! checks each stream's combined CRC.
//!
//! The magic numbers can occur by chance inside compressed data (about once
//! in 2^48 bit positions; ~0.003 expected over the 103 GB dump). Such a
//! false start splits a block in two pieces that both fail to decode;
//! [`Blocks`] then joins failed pieces with their successors and decodes
//! them again, so a false positive costs time, never data.

use std::io::Read;

use crate::read_full;
use crate::{DumpError, Result};

/// Block start magic (π).
pub const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
/// End-of-stream magic (√π).
pub const EOS_MAGIC: u64 = 0x1772_4538_5090;
const MASK48: u64 = (1 << 48) - 1;

/// What a piece starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceKind {
    /// A block magic: the piece is one compressed block.
    Block,
    /// An end-of-stream marker: the piece holds the stream's combined CRC
    /// (and whatever lies before the next block: padding, the next
    /// stream's header).
    End,
}

/// The bits between one magic and the next, copied to byte alignment.
#[derive(Clone, Debug)]
pub struct Piece {
    /// What the piece starts with.
    pub kind: PieceKind,
    /// The bits, most significant first; the last byte is zero-padded.
    pub bits: Vec<u8>,
    /// How many bits of `bits` are used.
    pub nbits: u64,
    /// Bit offset in the file.
    pub bit_offset: u64,
}

impl Piece {
    /// The 32 bits after the magic: the block CRC, or the stream's
    /// combined CRC.
    pub fn crc(&self) -> Option<u32> {
        if self.nbits < 80 {
            return None;
        }
        Some(u32::from_be_bytes([
            self.bits[6],
            self.bits[7],
            self.bits[8],
            self.bits[9],
        ]))
    }

    /// `self` followed by `next` (to undo a false magic match).
    pub fn join(&self, next: &Piece) -> Piece {
        let mut w = BitWriter::with_capacity(self.bits.len() + next.bits.len() + 1);
        w.put_bits(&self.bits, self.nbits);
        w.put_bits(&next.bits, next.nbits);
        let (bits, nbits) = w.finish();
        Piece {
            kind: self.kind,
            bits,
            nbits,
            bit_offset: self.bit_offset,
        }
    }
}

/// Appends bit strings.
struct BitWriter {
    out: Vec<u8>,
    nbits: u64,
}

impl BitWriter {
    fn with_capacity(n: usize) -> Self {
        Self {
            out: Vec::with_capacity(n),
            nbits: 0,
        }
    }

    fn put_u64(&mut self, v: u64, n: u32) {
        for i in (0..n).rev() {
            self.put_bit((v >> i) & 1 == 1);
        }
    }

    fn put_bit(&mut self, b: bool) {
        let r = (self.nbits % 8) as u32;
        if r == 0 {
            self.out.push(0);
        }
        if b {
            *self.out.last_mut().unwrap() |= 0x80 >> r;
        }
        self.nbits += 1;
    }

    /// Append the first `n` bits of `src`.
    fn put_bits(&mut self, src: &[u8], n: u64) {
        let r = (self.nbits % 8) as u32;
        let whole = (n / 8) as usize;
        if r == 0 {
            self.out.extend_from_slice(&src[..whole]);
            self.nbits += whole as u64 * 8;
        } else {
            for &b in &src[..whole] {
                *self.out.last_mut().unwrap() |= b >> r;
                self.out.push(b << (8 - r));
                self.nbits += 8;
            }
        }
        let rest = n % 8;
        if rest > 0 {
            let b = src[whole];
            for i in 0..rest {
                self.put_bit(b & (0x80 >> i) != 0);
            }
        }
    }

    fn finish(self) -> (Vec<u8>, u64) {
        (self.out, self.nbits)
    }
}

/// For each value of the second byte of an 8-byte window, which bit
/// shifts of the two magics it allows (bits 0–7: block, 8–15: end).
const fn shift_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut s = 0;
    while s < 8 {
        // The magic occupies window bits s..s+48 (from the top); byte 1 of
        // the window (bits 8..16) lies inside it for every s in 0..8.
        let b = ((BLOCK_MAGIC >> (32 + s)) & 0xff) as usize;
        t[b] |= 1 << s;
        let e = ((EOS_MAGIC >> (32 + s)) & 0xff) as usize;
        t[e] |= 1 << (8 + s);
        s += 1;
    }
    t
}

static SHIFTS: [u16; 256] = shift_table();

/// A magic found by [`find_magics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Magic {
    /// Bit position relative to the start of the scanned buffer.
    pub bit: u64,
    /// Which magic.
    pub kind: PieceKind,
}

/// Find every magic whose 48 bits lie in `buf`, starting at byte `from`
/// (positions at or after bit `from * 8`). Only windows that fit in `buf`
/// are tested: the caller keeps the last 7 bytes for the next call.
pub fn find_magics(buf: &[u8], from: usize, out: &mut Vec<Magic>) -> usize {
    // Window i covers bytes i..i+8; a magic starting at bit s of byte i
    // ends before bit s + 48 <= 55 of the window.
    if buf.len() < 8 {
        return from;
    }
    let end = buf.len() - 7;
    let mut i = from;
    while i < end {
        let m = SHIFTS[buf[i + 1] as usize];
        if m != 0 {
            let w = u64::from_be_bytes(buf[i..i + 8].try_into().unwrap());
            let mut bits = m;
            while bits != 0 {
                let k = bits.trailing_zeros();
                bits &= bits - 1;
                let s = k % 8;
                let v = (w >> (16 - s)) & MASK48;
                let (want, kind) = if k < 8 {
                    (BLOCK_MAGIC, PieceKind::Block)
                } else {
                    (EOS_MAGIC, PieceKind::End)
                };
                if v == want {
                    out.push(Magic {
                        bit: i as u64 * 8 + s as u64,
                        kind,
                    });
                }
            }
        }
        i += 1;
    }
    end
}

/// Copy bits `[from, to)` of `buf` into a new byte-aligned buffer.
fn copy_bits(buf: &[u8], from: u64, to: u64) -> Vec<u8> {
    let n = to - from;
    let nbytes = n.div_ceil(8) as usize;
    let mut out = Vec::with_capacity(nbytes + 16);
    let start = (from / 8) as usize;
    let r = (from % 8) as u32;
    if r == 0 {
        out.extend_from_slice(&buf[start..start + nbytes]);
    } else {
        let src = &buf[start..];
        for k in 0..nbytes {
            let hi = src[k] << r;
            let lo = src.get(k + 1).map_or(0, |b| b >> (8 - r));
            out.push(hi | lo);
        }
    }
    let rest = n % 8;
    if rest > 0 {
        let last = out.last_mut().unwrap();
        *last &= 0xffu8 << (8 - rest);
    }
    out
}

/// Splits a bzip2 byte stream into [`Piece`]s (see the module docs).
pub struct Scanner<R> {
    reader: R,
    buf: Vec<u8>,
    /// Bit offset of `buf[0]` in the file.
    base_bit: u64,
    /// Next byte of `buf` to scan.
    scan_from: usize,
    /// Magic positions found but not cut yet (absolute bits).
    found: std::collections::VecDeque<Magic>,
    /// The piece being collected: its start (absolute bit) and kind.
    current: Option<Magic>,
    eof: bool,
    done: bool,
    read_size: usize,
    /// Resuming: the bit the first block must start at.
    first: Option<u64>,
    /// Pieces cut so far.
    pub pieces: u64,
}

impl<R: Read> Scanner<R> {
    /// Start scanning `reader`, which must begin with a `BZh` header.
    pub fn new(mut reader: R) -> Result<Self> {
        let mut head = [0u8; 4];
        let n = read_full(&mut reader, &mut head).map_err(DumpError::from)?;
        if n < 4 || &head[..3] != b"BZh" || !(b'1'..=b'9').contains(&head[3]) {
            return Err(DumpError::Format("not a bzip2 file (no BZh header)".into()));
        }
        Ok(Self {
            reader,
            buf: Vec::new(),
            base_bit: 32,
            scan_from: 0,
            found: Default::default(),
            current: None,
            eof: false,
            done: false,
            read_size: 4 << 20,
            first: None,
            pieces: 0,
        })
    }

    /// Continue scanning a file at bit `bit`, where a block starts (a
    /// [`Blocks`] position); `reader` is positioned at byte `bit / 8`.
    pub fn resume(reader: R, bit: u64) -> Result<Self> {
        Ok(Self {
            reader,
            buf: Vec::new(),
            base_bit: bit / 8 * 8,
            scan_from: 0,
            found: Default::default(),
            current: None,
            eof: false,
            done: false,
            read_size: 4 << 20,
            first: Some(bit),
            pieces: 0,
        })
    }

    /// Use reads of `n` bytes (tests use small reads to exercise joins).
    pub fn with_read_size(mut self, n: usize) -> Self {
        self.read_size = n.max(16);
        self
    }

    fn fill(&mut self) -> Result<()> {
        // Drop bytes no piece needs any more.
        let keep_from_bit = self
            .current
            .map(|m| m.bit)
            .unwrap_or(self.base_bit + self.scan_from as u64 * 8);
        let drop = ((keep_from_bit - self.base_bit) / 8) as usize;
        let drop = drop.min(self.scan_from);
        if drop > 0 {
            self.buf.drain(..drop);
            self.base_bit += drop as u64 * 8;
            self.scan_from -= drop;
        }
        let old = self.buf.len();
        self.buf.resize(old + self.read_size, 0);
        let n = read_full(&mut self.reader, &mut self.buf[old..]).map_err(DumpError::from)?;
        self.buf.truncate(old + n);
        if n == 0 {
            self.eof = true;
        }
        Ok(())
    }

    fn cut(&mut self, start: Magic, end_bit: u64) -> Piece {
        let rel_from = start.bit - self.base_bit;
        let rel_to = end_bit - self.base_bit;
        self.pieces += 1;
        Piece {
            kind: start.kind,
            bits: copy_bits(&self.buf, rel_from, rel_to),
            nbits: rel_to - rel_from,
            bit_offset: start.bit,
        }
    }

    /// The next piece, `None` at the end of input.
    pub fn next_piece(&mut self) -> Result<Option<Piece>> {
        loop {
            if self.done {
                return Ok(None);
            }
            if let Some(m) = self.found.pop_front() {
                if let Some(first) = self.first {
                    if m.bit < first {
                        continue;
                    }
                    self.first = None;
                    if m.bit != first || m.kind != PieceKind::Block {
                        return Err(DumpError::Format(format!(
                            "no bzip2 block starts at bit {first} (a resume point of another file?)"
                        )));
                    }
                }
                let prev = self.current.replace(m);
                match prev {
                    Some(p) => return Ok(Some(self.cut(p, m.bit))),
                    None => {
                        if m.kind == PieceKind::End {
                            return Err(DumpError::Format(format!(
                                "bzip2: end-of-stream marker without a block at bit {}",
                                m.bit
                            )));
                        }
                        continue;
                    }
                }
            }
            if self.eof {
                self.done = true;
                let total_bits = self.base_bit + self.buf.len() as u64 * 8;
                return match self.current.take() {
                    Some(m) if m.kind == PieceKind::End && total_bits - m.bit >= 80 => {
                        Ok(Some(self.cut(m, total_bits)))
                    }
                    Some(m) => Err(DumpError::Truncated(format!(
                        "the bzip2 stream ends inside a block (at byte {} of the compressed input)",
                        m.bit / 8
                    ))),
                    None if self.first.is_some() => Err(DumpError::Format(
                        "no bzip2 block at the resume point (a resume point of another file?)".into(),
                    )),
                    None => Err(DumpError::Truncated("the bzip2 stream has no blocks".into())),
                };
            }
            self.fill()?;
            let mut found = Vec::new();
            if self.eof {
                // Test the last windows too, padded with zeros; keep only
                // matches that lie entirely in the data.
                let len = self.buf.len();
                let mut padded = self.buf[self.scan_from.saturating_sub(0)..].to_vec();
                padded.extend_from_slice(&[0u8; 8]);
                find_magics(&padded, 0, &mut found);
                let off = self.scan_from as u64 * 8;
                found.retain_mut(|m| {
                    m.bit += off;
                    m.bit + 48 <= len as u64 * 8
                });
                self.scan_from = len;
            } else {
                self.scan_from = find_magics(&self.buf, self.scan_from, &mut found);
            }
            let base = self.base_bit;
            self.found.extend(found.into_iter().map(|m| Magic {
                bit: m.bit + base,
                kind: m.kind,
            }));
        }
    }
}

impl<R: Read> Iterator for Scanner<R> {
    type Item = Result<Piece>;
    fn next(&mut self) -> Option<Result<Piece>> {
        match self.next_piece() {
            Ok(Some(p)) => Some(Ok(p)),
            Ok(None) => None,
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

/// Decode one block piece (see the module docs) with
/// [`crate::bzdecode`], or libbzip2 for the rare randomised blocks. The
/// output is the block's uncompressed bytes; an error means the piece is
/// not a valid block on its own (corrupt, or cut at a false magic).
pub fn decode_block(piece: &Piece, scratch_hint: usize) -> std::result::Result<Vec<u8>, String> {
    if use_libbzip2() {
        return decode_block_libbzip2(piece, scratch_hint);
    }
    let mut out = Vec::with_capacity(scratch_hint.max(1 << 20));
    match crate::bzdecode::decode_block(&piece.bits, piece.nbits, &mut out) {
        Ok(_) => Ok(out),
        Err(crate::bzdecode::DecodeError::Randomised) => decode_block_libbzip2(piece, scratch_hint),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether `WIKIDATA_BZ2_DECODER=libbzip2` asks for libbzip2 throughout
/// (for comparisons).
fn use_libbzip2() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("WIKIDATA_BZ2_DECODER").is_ok_and(|v| v == "libbzip2"))
}

/// Decode one block piece with libbzip2, as a one-block stream.
pub fn decode_block_libbzip2(piece: &Piece, scratch_hint: usize) -> std::result::Result<Vec<u8>, String> {
    let crc = piece.crc().ok_or("block too short")?;
    let mut w = BitWriter::with_capacity(piece.bits.len() + 16);
    w.put_bits(b"BZh9", 32);
    w.put_bits(&piece.bits, piece.nbits);
    w.put_u64(EOS_MAGIC, 48);
    w.put_u64(u64::from(crc), 32);
    let (stream, _) = w.finish();

    let mut d = bzip2::Decompress::new(false);
    let mut out: Vec<u8> = Vec::with_capacity(scratch_hint.max(1 << 20));
    loop {
        let consumed = d.total_in() as usize;
        if out.capacity() - out.len() < 64 << 10 {
            out.reserve(out.capacity().max(1 << 20));
        }
        match d.decompress_vec(&stream[consumed..], &mut out) {
            Ok(bzip2::Status::StreamEnd) => return Ok(out),
            Ok(_) => {
                if d.total_in() as usize >= stream.len() && out.capacity() > out.len() {
                    return Err("block ended early".into());
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// A decoded piece, as it comes back from a worker.
pub enum Decoded {
    /// A block's bytes and CRC; the piece too, in case it must be joined.
    Block {
        /// Uncompressed bytes.
        data: Vec<u8>,
        /// The block CRC.
        crc: u32,
        /// The piece, kept for joins.
        piece: Piece,
    },
    /// A block piece that did not decode on its own.
    Failed {
        /// The piece.
        piece: Piece,
        /// Why.
        error: String,
    },
    /// An end-of-stream piece.
    End(Piece),
}

/// Decode on a worker.
pub fn decode_piece(piece: Piece) -> Decoded {
    match piece.kind {
        PieceKind::End => Decoded::End(piece),
        PieceKind::Block => match decode_block(&piece, 0) {
            Ok(data) => Decoded::Block {
                crc: piece.crc().unwrap_or(0),
                data,
                piece,
            },
            Err(error) => Decoded::Failed { piece, error },
        },
    }
}

/// Counters from bzip2 decoding.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bz2Stats {
    /// Streams seen.
    pub streams: u64,
    /// Blocks decoded.
    pub blocks: u64,
    /// False magic matches undone by joining pieces.
    pub false_magics: u64,
}

/// How many following pieces a failed piece may be joined with.
const MAX_JOIN: usize = 8;

/// Checks decoded pieces in file order and yields block bytes (see the
/// module docs): verifies each stream's combined CRC and undoes false
/// magic matches.
pub struct Blocks<I> {
    inner: I,
    combined: u32,
    pending: std::collections::VecDeque<Result<Decoded>>,
    /// Counters.
    pub stats: Bz2Stats,
    /// Where the block last returned starts, and the counts before it:
    /// reading can resume there ([`Scanner::resume`], [`Blocks::resumed`]).
    pub last_start: Option<crate::BlockStart>,
    failed: bool,
}

impl<I: Iterator<Item = Result<Decoded>>> Blocks<I> {
    /// Wrap decoded pieces in file order.
    pub fn new(inner: I) -> Self {
        Self {
            inner,
            combined: 0,
            pending: Default::default(),
            stats: Bz2Stats::default(),
            last_start: None,
            failed: false,
        }
    }

    /// Continue a stream whose blocks before this point combine to `crc`,
    /// after `blocks` blocks and `streams` streams.
    pub fn resumed(mut self, crc: u32, blocks: u64, streams: u64) -> Self {
        self.combined = crc;
        self.stats.blocks = blocks;
        self.stats.streams = streams;
        self
    }

    fn pull(&mut self) -> Option<Result<Decoded>> {
        self.pending.pop_front().or_else(|| self.inner.next())
    }

    fn block(&mut self, data: Vec<u8>, crc: u32, bit: u64) -> Vec<u8> {
        self.last_start = Some(crate::BlockStart {
            plain: false,
            bit,
            crc: self.combined,
            blocks: self.stats.blocks,
            streams: self.stats.streams,
        });
        self.combined = self.combined.rotate_left(1) ^ crc;
        self.stats.blocks += 1;
        data
    }

    fn fail(&mut self, e: DumpError) -> Option<Result<Vec<u8>>> {
        self.failed = true;
        Some(Err(e))
    }

    fn piece_of(d: &Decoded) -> &Piece {
        match d {
            Decoded::Block { piece, .. } | Decoded::Failed { piece, .. } | Decoded::End(piece) => piece,
        }
    }
}

impl<I: Iterator<Item = Result<Decoded>>> Iterator for Blocks<I> {
    type Item = Result<Vec<u8>>;

    fn next(&mut self) -> Option<Result<Vec<u8>>> {
        if self.failed {
            return None;
        }
        loop {
            let d = match self.pull()? {
                Ok(d) => d,
                Err(e) => return self.fail(e),
            };
            match d {
                Decoded::Block { data, crc, piece } => return Some(Ok(self.block(data, crc, piece.bit_offset))),
                Decoded::End(piece) => {
                    let crc = piece.crc().unwrap_or(0);
                    if crc != self.combined {
                        return self.fail(DumpError::Format(format!(
                            "bzip2: stream CRC mismatch at byte {} (stream {:#010x}, blocks {:#010x})",
                            piece.bit_offset / 8,
                            crc,
                            self.combined
                        )));
                    }
                    self.stats.streams += 1;
                    self.combined = 0;
                }
                Decoded::Failed { piece, error } => {
                    // Join with the following pieces until it decodes.
                    let mut joined = piece;
                    let mut taken = Vec::new();
                    let mut ok = None;
                    for _ in 0..MAX_JOIN {
                        let next = match self.pull() {
                            Some(Ok(n)) => n,
                            Some(Err(e)) => return self.fail(e),
                            None => break,
                        };
                        joined = joined.join(Self::piece_of(&next));
                        taken.push(next);
                        if let Ok(data) = decode_block(&joined, 0) {
                            ok = Some(data);
                            break;
                        }
                    }
                    match ok {
                        Some(data) => {
                            self.stats.false_magics += taken.len() as u64;
                            let crc = joined.crc().unwrap_or(0);
                            return Some(Ok(self.block(data, crc, joined.bit_offset)));
                        }
                        None => {
                            let at = joined.bit_offset / 8;
                            let truncated = taken.is_empty();
                            for t in taken.into_iter().rev() {
                                self.pending.push_front(Ok(t));
                            }
                            return self.fail(if truncated {
                                DumpError::Truncated(format!(
                                    "bzip2: the last block (at byte {at}) does not decode: {error}"
                                ))
                            } else {
                                DumpError::Format(format!("bzip2: corrupt block at byte {at}: {error}"))
                            });
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    pub(crate) fn compress(data: &[u8], level: u32) -> Vec<u8> {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn sample(n: usize) -> Vec<u8> {
        // Varied, poorly compressible text so blocks fill up.
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let w = format!("{{\"id\":\"Q{}\",\"v\":{}}},\n", x % 100_000_000, x >> 40);
            out.extend_from_slice(w.as_bytes());
        }
        out.truncate(n);
        out
    }

    fn decode_all(file: &[u8], read_size: usize) -> Result<(Vec<u8>, Bz2Stats)> {
        let scanner = Scanner::new(file).unwrap().with_read_size(read_size);
        let decoded = scanner.map(|p| p.map(decode_piece));
        let mut blocks = Blocks::new(decoded);
        let mut out = Vec::new();
        for b in blocks.by_ref() {
            out.extend(b?);
        }
        Ok((out, blocks.stats))
    }

    #[test]
    fn table_matches_magics() {
        // Every shift of each magic is found.
        for s in 0..8u64 {
            for (magic, kind) in [(BLOCK_MAGIC, PieceKind::Block), (EOS_MAGIC, PieceKind::End)] {
                let mut buf = vec![0u8; 24];
                let v = (magic << (16 - s)) as u128;
                buf[4..12].copy_from_slice(&(v as u64).to_be_bytes());
                let mut found = Vec::new();
                find_magics(&buf, 0, &mut found);
                assert_eq!(found, vec![Magic { bit: 32 + s, kind }], "shift {s}");
            }
        }
    }

    #[test]
    fn copy_and_join_bits() {
        let buf = [0b1011_0110u8, 0b1100_1010, 0b0101_0101];
        let a = copy_bits(&buf, 3, 13);
        assert_eq!(a, vec![0b1011_0110, 0b0100_0000]);
        let p = Piece {
            kind: PieceKind::Block,
            bits: a,
            nbits: 10,
            bit_offset: 3,
        };
        let q = Piece {
            kind: PieceKind::Block,
            bits: copy_bits(&buf, 13, 24),
            nbits: 11,
            bit_offset: 13,
        };
        let j = p.join(&q);
        assert_eq!(j.nbits, 21);
        assert_eq!(j.bits, copy_bits(&buf, 3, 24));
    }

    #[test]
    fn single_stream_many_blocks() {
        let data = sample(3_000_000);
        let file = compress(&data, 1); // 100 kB blocks: ~30 of them
        for read_size in [4 << 20, 4096, 17] {
            let (out, stats) = decode_all(&file, read_size).unwrap();
            assert_eq!(out.len(), data.len());
            assert!(out == data);
            assert!(stats.blocks >= 20, "{stats:?}");
            assert_eq!(stats.streams, 1);
        }
    }

    #[test]
    fn multi_stream() {
        let a = sample(500_000);
        let b = sample(300_000);
        let mut file = compress(&a, 1);
        file.extend(compress(&b, 9));
        file.extend(compress(b"", 9));
        let (out, stats) = decode_all(&file, 1 << 16).unwrap();
        let mut want = a.clone();
        want.extend(&b);
        assert!(out == want);
        assert_eq!(stats.streams, 3);
    }

    #[test]
    fn truncated_is_an_error() {
        let file = compress(&sample(400_000), 1);
        for cut in [file.len() - 3, file.len() / 2, 100] {
            let r = decode_all(&file[..cut], 1 << 16);
            assert!(matches!(r, Err(DumpError::Truncated(_))), "cut at {cut}: {:?}", r.err());
        }
    }

    #[test]
    fn corrupt_is_an_error() {
        let mut file = compress(&sample(400_000), 1);
        let i = file.len() / 3;
        file[i] ^= 0x55;
        assert!(decode_all(&file, 1 << 16).is_err());
    }

    #[test]
    fn false_magic_is_undone() {
        // Split a real block piece at an arbitrary bit, as a false magic
        // match would: both halves fail, the join decodes.
        let data = sample(300_000);
        let file = compress(&data, 1);
        let pieces: Vec<Piece> = Scanner::new(&file[..]).unwrap().map(|p| p.unwrap()).collect();
        let mut split = Vec::new();
        for (i, p) in pieces.iter().enumerate() {
            if i == 1 {
                let cut = p.nbits / 2;
                let a = Piece {
                    kind: PieceKind::Block,
                    bits: copy_bits(&p.bits, 0, cut),
                    nbits: cut,
                    bit_offset: p.bit_offset,
                };
                let b = Piece {
                    kind: PieceKind::Block,
                    bits: copy_bits(&p.bits, cut, p.nbits),
                    nbits: p.nbits - cut,
                    bit_offset: p.bit_offset + cut,
                };
                split.push(a);
                split.push(b);
            } else {
                split.push(p.clone());
            }
        }
        let mut blocks = Blocks::new(split.into_iter().map(|p| Ok(decode_piece(p))));
        let mut out = Vec::new();
        for b in blocks.by_ref() {
            out.extend(b.unwrap());
        }
        assert!(out == data);
        assert_eq!(blocks.stats.false_magics, 1);
    }

    #[test]
    fn not_bzip2() {
        assert!(Scanner::new(&b"{\"a\":1}"[..]).is_err());
    }
}
