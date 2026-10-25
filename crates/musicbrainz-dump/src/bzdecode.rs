// Vendored from wikidata-rs 0.3.1 (crates/wikidata-dump/src), same authors:
// the parallel bzip2 block decoder. Keep in step with fixes made there.

//! A bzip2 block decoder tuned for throughput.
//!
//! It decodes one block (see [`crate::bz2`] for how blocks are found)
//! straight from its bits, about twice as fast as libbzip2 on the
//! Wikidata dump (one core: 148 vs 76 MB/s of output; ten cores: the
//! same ratio, with the same 3.6 MB table per block). Where the time
//! goes, and what changed:
//!
//! - **The inverse BWT walk** (libbzip2: half the time) follows the
//!   T-vector, where each step is a load that depends on the previous one
//!   and misses the cache. Here eight chains are followed at once, from
//!   the text's start and from seven evenly spaced indices; each chain
//!   stops where another started, and the segments are joined (see
//!   `walk`): eight loads in flight instead of one, ~3× faster.
//! - **Building the T-vector** places runs of a byte together, so the
//!   per-byte counter is not read and written back for every byte, and
//!   writes it with plain stores (`T[j] = i << 8 | F[j]`, since
//!   `F[j] = L[i]`), ~1.7× faster.
//! - **Huffman codes** are read through a 10-bit lookup table, from a bit
//!   buffer kept in a register.
//! - **The CRC** uses the CPU's CRC-32 instructions (through crc32fast,
//!   on bit-reversed bytes), and **the initial run-length decoding**
//!   looks for runs eight positions at a time: together ~4× faster.
//! - Scratch tables are reused per thread.
//!
//! Every block's CRC is checked, so a decoding error cannot go unnoticed.
//! Randomised blocks (written only by bzip2 versions before 0.9.5) are
//! left to libbzip2.

use std::cell::RefCell;

use crate::bz2::BLOCK_MAGIC;

/// Longest Huffman code.
const MAX_CODE_LEN: usize = 20;
/// Bits resolved by one table lookup.
const TABLE_BITS: u32 = 10;
/// Largest block (level 9).
const MAX_BLOCK: usize = 900_000;
/// Symbols per Huffman group.
const GROUP_SIZE: u32 = 50;
/// libbzip2's limit; extra selectors are ignored (as bzip2 1.0.8 does).
const MAX_SELECTORS: usize = 18_002;

/// Why a block did not decode.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Randomised blocks are not handled here.
    Randomised,
    /// Not a valid block.
    Invalid(&'static str),
    /// The data does not match the block's CRC.
    Crc,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Randomised => f.write_str("randomised block"),
            DecodeError::Invalid(m) => write!(f, "invalid block: {m}"),
            DecodeError::Crc => f.write_str("block CRC mismatch"),
        }
    }
}

type R<T> = Result<T, DecodeError>;

/// MSB-first bit reader over a byte slice.
struct Bits<'a> {
    data: &'a [u8],
    pos: u64,
    end: u64,
}

impl<'a> Bits<'a> {
    #[inline(always)]
    fn window(&self) -> u64 {
        // 64 bits starting at `pos`, zero-padded past the end.
        let byte = (self.pos >> 3) as usize;
        let w = match self.data.get(byte..byte + 8) {
            Some(b) => u64::from_be_bytes(b.try_into().unwrap()),
            None => {
                let mut b = [0u8; 8];
                let rest = self.data.get(byte..).unwrap_or(&[]);
                b[..rest.len()].copy_from_slice(rest);
                u64::from_be_bytes(b)
            }
        };
        w << (self.pos & 7)
    }

    /// The next `n` (1..=32) bits, without consuming them.
    #[inline(always)]
    fn peek(&self, n: u32) -> u32 {
        (self.window() >> (64 - n)) as u32
    }

    #[inline(always)]
    fn skip(&mut self, n: u32) {
        self.pos += u64::from(n);
    }

    #[inline(always)]
    fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n);
        v
    }

    #[inline(always)]
    fn bit(&mut self) -> bool {
        self.get(1) == 1
    }

    fn overrun(&self) -> bool {
        self.pos > self.end
    }
}

/// A bit reader for the hot loop: up to 64 bits in a register, refilled
/// eight bytes at a time, so reading a code does not wait for a load.
struct FastBits<'a> {
    data: &'a [u8],
    /// Next byte to load.
    byte: usize,
    /// Bits, most significant first.
    buf: u64,
    /// How many of them are valid.
    cnt: u32,
}

impl<'a> FastBits<'a> {
    fn new(data: &'a [u8], bit: u64) -> Self {
        let mut f = FastBits {
            data,
            byte: (bit >> 3) as usize,
            buf: 0,
            cnt: 0,
        };
        f.refill();
        f.skip((bit & 7) as u32);
        f
    }

    #[inline(always)]
    fn load(&self) -> u64 {
        match self.data.get(self.byte..self.byte + 8) {
            Some(b) => u64::from_be_bytes(b.try_into().unwrap()),
            None => {
                let mut b = [0u8; 8];
                let rest = self.data.get(self.byte..).unwrap_or(&[]);
                b[..rest.len()].copy_from_slice(rest);
                u64::from_be_bytes(b)
            }
        }
    }

    /// Top up to at least 56 valid bits. Bits of a partly loaded byte are
    /// loaded again, at the same place, by the next refill.
    #[inline(always)]
    fn refill(&mut self) {
        self.buf |= self.load() >> self.cnt;
        let bytes = (63 - self.cnt) >> 3;
        self.byte += bytes as usize;
        self.cnt += bytes * 8;
    }

    #[inline(always)]
    fn skip(&mut self, n: u32) {
        self.buf <<= n;
        self.cnt -= n;
    }

    /// The bit position reached.
    fn position(&self) -> u64 {
        self.byte as u64 * 8 - u64::from(self.cnt)
    }
}

/// One Huffman group's decoding tables.
struct Table {
    /// `(symbol << 8) | length` for codes up to `TABLE_BITS` long, by
    /// their first bits; 0 when the code is longer.
    fast: Vec<u32>,
    /// libbzip2's limit/base/perm tables, for longer codes.
    limit: [i32; MAX_CODE_LEN + 2],
    base: [i32; MAX_CODE_LEN + 2],
    perm: [u16; 258],
    min_len: u32,
    max_len: u32,
}

impl Table {
    fn new() -> Table {
        Table {
            fast: vec![0; 1 << TABLE_BITS],
            limit: [0; MAX_CODE_LEN + 2],
            base: [0; MAX_CODE_LEN + 2],
            perm: [0; 258],
            min_len: 0,
            max_len: 0,
        }
    }

    /// Build from code lengths (as `BZ2_hbCreateDecodeTables` does, plus
    /// the lookup table).
    fn build(&mut self, lens: &[u8]) -> R<()> {
        let min_len = u32::from(*lens.iter().min().unwrap());
        let max_len = u32::from(*lens.iter().max().unwrap());
        self.min_len = min_len;
        self.max_len = max_len;
        let mut pp = 0;
        for l in min_len..=max_len {
            for (s, &len) in lens.iter().enumerate() {
                if u32::from(len) == l {
                    self.perm[pp] = s as u16;
                    pp += 1;
                }
            }
        }
        let mut base = [0i32; MAX_CODE_LEN + 2];
        for &l in lens {
            base[l as usize + 1] += 1;
        }
        for i in 1..base.len() {
            base[i] += base[i - 1];
        }
        let mut limit = [0i32; MAX_CODE_LEN + 2];
        let mut vec = 0i32;
        for i in min_len as usize..=max_len as usize {
            vec += base[i + 1] - base[i];
            limit[i] = vec - 1;
            vec <<= 1;
        }
        for i in min_len as usize + 1..=max_len as usize {
            base[i] = ((limit[i - 1] + 1) << 1) - base[i];
        }
        self.limit = limit;
        self.base = base;
        // Canonical codes, shortest first, by symbol within a length.
        self.fast.iter_mut().for_each(|e| *e = 0);
        let mut code = 0u32;
        let mut k = 0usize;
        for l in min_len..=max_len {
            while k < pp && u32::from(lens[self.perm[k] as usize]) == l {
                if code >= 1 << l {
                    return Err(DecodeError::Invalid("over-subscribed Huffman code"));
                }
                if l <= TABLE_BITS {
                    let shift = TABLE_BITS - l;
                    let first = (code << shift) as usize;
                    let entry = (u32::from(self.perm[k]) << 8) | l;
                    for e in &mut self.fast[first..first + (1 << shift)] {
                        *e = entry;
                    }
                }
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        Ok(())
    }

    /// The next symbol.
    #[inline(always)]
    fn decode(&self, r: &mut FastBits) -> R<u16> {
        if r.cnt < MAX_CODE_LEN as u32 {
            r.refill();
        }
        let v = (r.buf >> (64 - MAX_CODE_LEN as u32)) as u32;
        let e = self.fast[(v >> (MAX_CODE_LEN as u32 - TABLE_BITS)) as usize];
        if e != 0 {
            r.skip(e & 0xff);
            return Ok((e >> 8) as u16);
        }
        for zn in self.min_len.max(TABLE_BITS + 1)..=self.max_len {
            let zvec = (v >> (MAX_CODE_LEN as u32 - zn)) as i32;
            if zvec <= self.limit[zn as usize] {
                r.skip(zn);
                let i = zvec - self.base[zn as usize];
                if !(0..258).contains(&i) {
                    return Err(DecodeError::Invalid("bad Huffman code"));
                }
                return Ok(self.perm[i as usize]);
            }
        }
        Err(DecodeError::Invalid("bad Huffman code"))
    }
}

/// Per-thread scratch space.
struct Scratch {
    /// The L column (the MTF-decoded block).
    ll: Vec<u8>,
    tt: Vec<u32>,
    bwt: Vec<u8>,
    chains: Vec<u8>,
    tables: Vec<Table>,
    selectors: Vec<u8>,
}

thread_local! {
    static SCRATCH: RefCell<Option<Scratch>> = const { RefCell::new(None) };
}

/// The bzip2 CRC (CRC-32, MSB first, polynomial 0x04c11db7), 8 bytes at
/// a time: the reference for [`block_crc`].
#[cfg(test)]
struct Crc {
    tables: [[u32; 256]; 8],
}

#[cfg(test)]
const fn crc_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04c1_1db7
            } else {
                c << 1
            };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut s = 1;
    while s < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[s - 1][i];
            t[s][i] = (prev << 8) ^ t[0][(prev >> 24) as usize];
            i += 1;
        }
        s += 1;
    }
    t
}

#[cfg(test)]
static CRC: Crc = Crc { tables: crc_tables() };

#[cfg(test)]
impl Crc {
    fn update(&self, mut crc: u32, data: &[u8]) -> u32 {
        let t = &self.tables;
        let mut chunks = data.chunks_exact(8);
        for c in &mut chunks {
            let hi = crc ^ u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
            crc = t[7][(hi >> 24) as usize]
                ^ t[6][((hi >> 16) & 0xff) as usize]
                ^ t[5][((hi >> 8) & 0xff) as usize]
                ^ t[4][(hi & 0xff) as usize]
                ^ t[3][c[4] as usize]
                ^ t[2][c[5] as usize]
                ^ t[1][c[6] as usize]
                ^ t[0][c[7] as usize];
        }
        for &b in chunks.remainder() {
            crc = (crc << 8) ^ t[0][((crc >> 24) ^ u32::from(b)) as usize];
        }
        crc
    }
}

/// Chains walked at once by [`walk`].
const CHAINS: usize = 8;
/// Marks a chain's start in `tt` (bits 28–31 are free: indices < 2^20).
const START: u32 = 1 << 31;
const NEXT: u32 = 0x000f_ffff;

/// The inverse BWT walk (see the module docs).
///
/// The text is the cycle `origPtr, next(origPtr), next²(origPtr), …` of
/// the T-vector, each index giving its byte. Following one chain is a
/// series of dependent cache misses, so the cycle is cut into segments
/// that are followed at once: chains start at `origPtr` (the text's
/// first byte) and at evenly spaced indices, whose text positions
/// are not known. Start indices are marked in `tt`; each chain runs until
/// it reaches another chain's start, and then the segments are joined
/// in cycle order. Only `tt` is read (3.6 MB for a full block, like
/// libbzip2), with eight loads in flight instead of one.
///
/// The T-vector is several cycles when the text is periodic (`u` repeated
/// `k` times gives `k` cycles of `|u|`): the text is then origPtr's cycle
/// repeated to the block's length, as libbzip2's `n`-step walk gives.
/// Corrupt data can also make several cycles; the block CRC catches it.
/// Returns `false` when the chains do not close origPtr's cycle.
#[inline(never)]
fn walk(tt: &mut [u32], orig_ptr: usize, bufs: &mut [u8], out: &mut Vec<u8>) -> bool {
    let n = tt.len();
    let first = orig_ptr;
    let mut starts = [usize::MAX; CHAINS];
    let mut count = 0;
    for k in 0..CHAINS {
        let s = if k == 0 { first } else { k * n / CHAINS };
        if !starts[..count].contains(&s) {
            starts[count] = s;
            count += 1;
        }
    }
    for &s in &starts[..count] {
        tt[s] |= START;
    }
    // Chain c writes bufs[c * MAX_BLOCK..]; its state is (x, len), and a
    // chain that is done (or unused) has its bit cleared in `alive`.
    let (mut x, mut len) = ([0u32; CHAINS], [0usize; CHAINS]);
    let mut hit = [usize::MAX; CHAINS];
    for c in 0..count {
        // Each chain's own start is marked: take its byte first.
        let w = tt[starts[c]];
        bufs[c * MAX_BLOCK] = w as u8;
        len[c] = 1;
        x[c] = (w >> 8) & NEXT;
    }
    let mut alive: u32 = (1 << count) - 1;
    macro_rules! step {
        ($c:expr) => {
            if alive & (1 << $c) != 0 {
                let w = tt[x[$c] as usize];
                if w & START != 0 {
                    alive &= !(1 << $c);
                    hit[$c] = x[$c] as usize;
                } else {
                    bufs[$c * MAX_BLOCK + len[$c]] = w as u8;
                    len[$c] += 1;
                    x[$c] = (w >> 8) & NEXT;
                }
            }
        };
    }
    while alive != 0 {
        step!(0);
        step!(1);
        step!(2);
        step!(3);
        step!(4);
        step!(5);
        step!(6);
        step!(7);
    }
    // Join the segments in cycle order, from the text's first byte.
    out.clear();
    let mut c = 0;
    let mut closed = false;
    for _ in 0..count {
        out.extend_from_slice(&bufs[c * MAX_BLOCK..c * MAX_BLOCK + len[c]]);
        if hit[c] == first {
            closed = true;
            break;
        }
        c = starts[..count].iter().position(|&s| s == hit[c]).unwrap_or(0);
    }
    // A periodic text (one pattern repeated end to end) makes the
    // T-vector several cycles of equal length, and origPtr's cycle holds
    // one period. libbzip2 walks `n` steps from origPtr, going round that
    // cycle again and again: the period, repeated to `n` bytes.
    if closed && !out.is_empty() && out.len() < n {
        while out.len() < n {
            let more = (n - out.len()).min(out.len());
            out.extend_from_within(..more);
        }
    }
    out.len() == n
}

/// The first index where four equal bytes start, looking at eight
/// positions per step: a byte of `(x ^ x>>8) | (x ^ x>>16) | (x ^ x>>24)`
/// is zero where four equal bytes start.
#[inline]
fn run_of_four(b: &[u8]) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let mut i = 0;
    while i + 11 <= b.len() {
        let x = u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        let y = u64::from_le_bytes(b[i + 1..i + 9].try_into().unwrap());
        let z = u64::from_le_bytes(b[i + 2..i + 10].try_into().unwrap());
        let w = u64::from_le_bytes(b[i + 3..i + 11].try_into().unwrap());
        let d = (x ^ y) | (x ^ z) | (x ^ w);
        // Lowest zero byte of d (exact for the lowest one).
        let zero = d.wrapping_sub(LO) & !d & HI;
        if zero != 0 {
            return Some(i + (zero.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    // The tail, one position at a time.
    (i..b.len().saturating_sub(3)).find(|&k| b[k] == b[k + 1] && b[k] == b[k + 2] && b[k] == b[k + 3])
}

/// The bzip2 CRC of `data`.
///
/// CRC-32/BZIP2 is CRC-32/ISO-HDLC (zlib's) with every bit order
/// reversed: crc_bzip2(d) = reverse32(crc_zlib(d with each byte's bits
/// reversed)). crc32fast computes the latter with the CPU's CRC
/// instructions, several times faster than any table.
pub fn block_crc(data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    let mut tmp = [0u8; 4096];
    for chunk in data.chunks(tmp.len()) {
        let t = &mut tmp[..chunk.len()];
        for (o, &b) in t.iter_mut().zip(chunk) {
            *o = b.reverse_bits();
        }
        h.update(t);
    }
    h.finalize().reverse_bits()
}

/// The bzip2 CRC with tables (for testing [`block_crc`]).
#[cfg(test)]
fn block_crc_tables(data: &[u8]) -> u32 {
    !CRC.update(0xffff_ffff, data)
}

/// Decode the block whose bits (starting with its magic) are the first
/// `nbits` of `data`, appending its bytes to `out`. Returns the block's
/// CRC (checked).
pub fn decode_block(data: &[u8], nbits: u64, out: &mut Vec<u8>) -> R<u32> {
    SCRATCH.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.get_or_insert_with(|| Scratch {
            ll: Vec::new(),
            tt: Vec::new(),
            bwt: Vec::new(),
            chains: Vec::new(),
            tables: (0..6).map(|_| Table::new()).collect(),
            selectors: Vec::new(),
        });
        decode_with(data, nbits, out, s)
    })
}

fn decode_with(data: &[u8], nbits: u64, out: &mut Vec<u8>, s: &mut Scratch) -> R<u32> {
    let mut r = Bits {
        data,
        pos: 0,
        end: nbits,
    };
    let magic = (u64::from(r.get(24)) << 24) | u64::from(r.get(24));
    if magic != BLOCK_MAGIC {
        return Err(DecodeError::Invalid("no block magic"));
    }
    let stored_crc = r.get(32);
    if r.bit() {
        return Err(DecodeError::Randomised);
    }
    let orig_ptr = r.get(24) as usize;

    // Symbol map.
    let used16 = r.get(16);
    let mut seq_to_unseq = [0u8; 256];
    let mut n_in_use = 0usize;
    for i in 0..16 {
        if used16 & (0x8000 >> i) != 0 {
            let v = r.get(16);
            for j in 0..16 {
                if v & (0x8000 >> j) != 0 {
                    seq_to_unseq[n_in_use] = (i * 16 + j) as u8;
                    n_in_use += 1;
                }
            }
        }
    }
    if n_in_use == 0 {
        return Err(DecodeError::Invalid("no symbols"));
    }
    let alpha = n_in_use + 2;

    // Selectors.
    let n_groups = r.get(3) as usize;
    if !(2..=6).contains(&n_groups) {
        return Err(DecodeError::Invalid("bad group count"));
    }
    let n_selectors = r.get(15) as usize;
    if n_selectors == 0 {
        return Err(DecodeError::Invalid("no selectors"));
    }
    let mut pos = [0u8, 1, 2, 3, 4, 5];
    s.selectors.clear();
    for _ in 0..n_selectors {
        let mut j = 0usize;
        while r.bit() {
            j += 1;
            if j >= n_groups {
                return Err(DecodeError::Invalid("bad selector"));
            }
        }
        if r.overrun() {
            return Err(DecodeError::Invalid("truncated selectors"));
        }
        let v = pos[j];
        pos.copy_within(0..j, 1);
        pos[0] = v;
        if s.selectors.len() < MAX_SELECTORS {
            s.selectors.push(v);
        }
    }

    // Code lengths, then tables.
    let mut lens = [0u8; 258];
    for t in 0..n_groups {
        let mut curr = r.get(5) as i32;
        for len in lens.iter_mut().take(alpha) {
            loop {
                if !(1..=20).contains(&curr) {
                    return Err(DecodeError::Invalid("bad code length"));
                }
                if !r.bit() {
                    break;
                }
                if r.bit() {
                    curr -= 1;
                } else {
                    curr += 1;
                }
                if r.overrun() {
                    return Err(DecodeError::Invalid("truncated code lengths"));
                }
            }
            *len = curr as u8;
        }
        s.tables[t].build(&lens[..alpha])?;
    }

    // Symbols: run lengths of zero (RUNA/RUNB) and MTF indices.
    if s.tt.len() < MAX_BLOCK {
        s.tt.resize(MAX_BLOCK, 0);
        s.ll.resize(MAX_BLOCK, 0);
        s.bwt.reserve(MAX_BLOCK);
        // One region per chain; only what the chains write is touched.
        s.chains.resize(CHAINS * MAX_BLOCK, 0);
    }
    let ll = &mut s.ll[..];
    let eob = (n_in_use + 1) as u16;
    let mut mtf = [0u8; 256];
    for (i, m) in mtf.iter_mut().enumerate() {
        *m = i as u8;
    }
    let mut counts = [0u32; 256];
    let mut nblock = 0usize;
    let mut group = 0usize;
    let mut group_left = 0u32;
    let mut table = &s.tables[0];
    let mut run = 0u32;
    let mut run_bit = 1u32;
    let mut fb = FastBits::new(data, r.pos);
    loop {
        if group_left == 0 {
            let sel = *s
                .selectors
                .get(group)
                .ok_or(DecodeError::Invalid("ran out of selectors"))?;
            table = &s.tables[sel as usize];
            group += 1;
            group_left = GROUP_SIZE;
        }
        group_left -= 1;
        let sym = table.decode(&mut fb)?;
        if sym <= 1 {
            // RUNA adds 1×, RUNB 2× the current power of two.
            run += (u32::from(sym) + 1) * run_bit;
            run_bit <<= 1;
            if run_bit > (2 << 20) {
                return Err(DecodeError::Invalid("run too long"));
            }
            continue;
        }
        if run > 0 {
            let uc = seq_to_unseq[mtf[0] as usize];
            let n = run as usize;
            if nblock + n > MAX_BLOCK {
                return Err(DecodeError::Invalid("block too long"));
            }
            ll[nblock..nblock + n].fill(uc);
            nblock += n;
            run = 0;
            run_bit = 1;
        }
        if sym == eob {
            break;
        }
        let k = (sym - 1) as usize;
        if k >= n_in_use {
            return Err(DecodeError::Invalid("bad MTF index"));
        }
        // Move to front: small indices (most) with one 8-byte shift.
        let v = if k < 8 {
            let w = u64::from_le_bytes(mtf[..8].try_into().unwrap());
            let v = (w >> (8 * k)) as u8;
            let below = w & ((1u64 << (8 * k)) - 1);
            let above = if k == 7 { 0 } else { w & !((1u64 << (8 * (k + 1))) - 1) };
            mtf[..8].copy_from_slice(&(above | (below << 8) | u64::from(v)).to_le_bytes());
            v
        } else {
            let v = mtf[k];
            mtf.copy_within(0..k, 1);
            mtf[0] = v;
            v
        };
        if nblock >= MAX_BLOCK {
            return Err(DecodeError::Invalid("block too long"));
        }
        ll[nblock] = seq_to_unseq[v as usize];
        nblock += 1;
    }
    // Byte counts, four interleaved histograms to break dependencies.
    let mut hist = [[0u32; 256]; 4];
    let mut quads = ll[..nblock].chunks_exact(4);
    for q in &mut quads {
        hist[0][q[0] as usize] += 1;
        hist[1][q[1] as usize] += 1;
        hist[2][q[2] as usize] += 1;
        hist[3][q[3] as usize] += 1;
    }
    for &b in quads.remainder() {
        hist[0][b as usize] += 1;
    }
    for (b, c) in counts.iter_mut().enumerate() {
        *c = hist[0][b] + hist[1][b] + hist[2][b] + hist[3][b];
    }
    r.pos = fb.position();
    if r.overrun() {
        return Err(DecodeError::Invalid("truncated block"));
    }
    if orig_ptr >= nblock {
        return Err(DecodeError::Invalid("bad origin pointer"));
    }

    // The inverse BWT table, written with scattered stores only (no
    // read-modify-write): for i in L order, j = the position of L[i] in F,
    // and tt[j] = i << 8 | F[j], since F[j] = L[i]. Walking it from
    // origPtr yields F[origPtr], F[next(origPtr)], …: the text.
    let mut cftab = [0u32; 256];
    let mut sum = 0u32;
    for (c, &n) in cftab.iter_mut().zip(&counts) {
        *c = sum;
        sum += n;
    }
    // Runs of a byte (frequent in L) are placed together, so cftab is not
    // read, incremented and written back once per byte.
    let tt = &mut s.tt[..];
    let ll = &ll[..nblock];
    let mut i = 0usize;
    while i < nblock {
        let uc = ll[i];
        let mut e = i + 1;
        while e < nblock && ll[e] == uc {
            e += 1;
        }
        let j = cftab[uc as usize] as usize;
        let c = u32::from(uc);
        for (k, t) in tt[j..j + (e - i)].iter_mut().enumerate() {
            *t = (((i + k) as u32) << 8) | c;
        }
        cftab[uc as usize] = (j + e - i) as u32;
        i = e;
    }

    // Walk from both ends at once (see the module docs): output byte n
    // (1-based) is L[next^n(origPtr)], and the last one is L[origPtr].
    if !walk(&mut tt[..nblock], orig_ptr, &mut s.chains, &mut s.bwt) {
        return Err(DecodeError::Invalid("the BWT walk does not close"));
    }
    let bwt = &s.bwt[..];
    // Undo the initial run-length encoding: four equal bytes are followed
    // by a count of further copies.
    let start = out.len();
    out.reserve(nblock + nblock / 8);
    let mut i = 0usize;
    while i < nblock {
        let rest = &bwt[i..];
        let Some(j) = run_of_four(rest) else {
            out.extend_from_slice(rest);
            break;
        };
        // rest[j..j + 4] are equal: copy through them, then the count.
        out.extend_from_slice(&rest[..j + 4]);
        match rest.get(j + 4) {
            Some(&c) => {
                out.resize(out.len() + c as usize, rest[j]);
                i += j + 5;
            }
            None => i += j + 4,
        }
    }
    let crc = block_crc(&out[start..]);
    if crc != stored_crc {
        out.truncate(start);
        return Err(DecodeError::Crc);
    }
    Ok(crc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bz2::{Piece, PieceKind, Scanner};
    use std::io::{Read, Write};

    fn compress(data: &[u8], level: u32) -> Vec<u8> {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn pieces(file: &[u8]) -> Vec<Piece> {
        Scanner::new(file)
            .unwrap()
            .map(|p| p.unwrap())
            .filter(|p| p.kind == PieceKind::Block)
            .collect()
    }

    fn roundtrip(data: &[u8], level: u32) {
        let file = compress(data, level);
        let mut out = Vec::new();
        for (i, p) in pieces(&file).iter().enumerate() {
            if let Err(e) = decode_block(&p.bits, p.nbits, &mut out) {
                panic!("level {level}, {} bytes, block {i}: {e}", data.len());
            }
        }
        let mut want = Vec::new();
        bzip2::read::BzDecoder::new(&file[..]).read_to_end(&mut want).unwrap();
        assert_eq!(want.len(), data.len());
        assert!(out == data, "level {level}, {} bytes", data.len());
    }

    fn rng(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn crc_matches_reference() {
        // CRC-32/BZIP2 of "123456789".
        assert_eq!(block_crc(b"123456789"), 0xfc89_1918);
        assert_eq!(block_crc_tables(b"123456789"), 0xfc89_1918);
        assert_eq!(block_crc(b""), 0);
        let mut seed = 99;
        for n in [1usize, 7, 8, 9, 4095, 4096, 4097, 100_000] {
            let d: Vec<u8> = (0..n).map(|_| rng(&mut seed) as u8).collect();
            assert_eq!(block_crc(&d), block_crc_tables(&d), "{n}");
        }
    }

    #[test]
    fn decodes_like_libbzip2() {
        let mut seed = 0x2545_f491_4f6c_dd1d;
        // Text-like data.
        let mut text = Vec::new();
        while text.len() < 2_000_000 {
            let w = rng(&mut seed);
            text.extend_from_slice(
                format!("{{\"id\":\"Q{}\",\"v\":\"\\u{:04x}\"}},\n", w % 99_999, w >> 48).as_bytes(),
            );
        }
        for level in [1, 5, 9] {
            roundtrip(&text, level);
        }
        // Random bytes (all 256 symbols, long codes).
        let random: Vec<u8> = (0..700_000).map(|_| rng(&mut seed) as u8).collect();
        roundtrip(&random, 9);
        // Runs of every length around the RLE1 thresholds, and long runs.
        let mut runs = Vec::new();
        for n in 0..600usize {
            let b = (n % 7) as u8 + b'a';
            runs.extend(std::iter::repeat_n(b, n % 300));
            runs.push(b'\n');
        }
        runs.extend(std::iter::repeat_n(b'x', 1_000_000));
        roundtrip(&runs, 9);
        roundtrip(&runs, 1);
        // One byte, a few symbols, a skewed distribution.
        roundtrip(b"a", 9);
        roundtrip(&[0u8; 1000], 9);
        let skewed: Vec<u8> = (0..500_000)
            .map(|_| {
                let r = rng(&mut seed);
                if r.is_multiple_of(1000) {
                    (r >> 16) as u8
                } else {
                    b'e' + (r % 3) as u8
                }
            })
            .collect();
        roundtrip(&skewed, 9);
    }

    #[test]
    fn decodes_periodic_blocks() {
        // A block whose text is one pattern repeated end to end has a
        // T-vector of several cycles (the Wikidata 20261012 dump has one at
        // byte 547,678,139); libbzip2 decodes it, and so must this.
        let mut seed = 0x9e37_79b9_7f4a_7c15;
        roundtrip(&b"abc".repeat(200_000), 9);
        roundtrip(&b"abc".repeat(200_000), 1);
        roundtrip(&b"ab".repeat(400_000), 9);
        let unit: Vec<u8> = (0..997).map(|_| b'a' + (rng(&mut seed) % 26) as u8).collect();
        roundtrip(&unit.repeat(890), 9);
        // Periodic only after the initial run-length encoding:
        // 255 'a's are "aaaa" and a count of 251, 3000 times.
        roundtrip(&vec![b'a'; 255 * 3000], 9);
        // Not periodic, for contrast: one byte more.
        let mut almost = b"abc".repeat(200_000);
        almost.push(b'x');
        roundtrip(&almost, 9);
    }

    #[test]
    fn rejects_damage() {
        let mut seed = 7;
        let text: Vec<u8> = (0..300_000).map(|_| b'a' + (rng(&mut seed) % 20) as u8).collect();
        let file = compress(&text, 9);
        let p = &pieces(&file)[0];
        let mut out = Vec::new();
        decode_block(&p.bits, p.nbits, &mut out).unwrap();
        for k in [7usize, 40, 1000, p.bits.len() / 2, p.bits.len() - 20] {
            let mut bad = p.bits.clone();
            bad[k] ^= 0x10;
            let mut out = Vec::new();
            assert!(decode_block(&bad, p.nbits, &mut out).is_err(), "flip at {k}");
        }
        // Truncated.
        let mut out = Vec::new();
        assert!(decode_block(&p.bits, p.nbits / 2, &mut out).is_err());
    }

    #[test]
    fn damage_never_panics() {
        let mut seed = 0x1234_5678;
        let mut text = Vec::new();
        while text.len() < 200_000 {
            let w = rng(&mut seed);
            text.extend_from_slice(format!("{{\"Q{}\":[{}]}},", w % 977, w % 13).as_bytes());
        }
        for level in [1, 9] {
            let file = compress(&text, level);
            let p = &pieces(&file)[0];
            for _ in 0..1500 {
                let mut bad = p.bits.clone();
                let bit = (rng(&mut seed) % p.nbits) as usize;
                bad[bit / 8] ^= 0x80 >> (bit % 8);
                if rng(&mut seed).is_multiple_of(3) {
                    let bit2 = (rng(&mut seed) % p.nbits) as usize;
                    bad[bit2 / 8] ^= 0x80 >> (bit2 % 8);
                }
                let nbits = if rng(&mut seed).is_multiple_of(5) {
                    rng(&mut seed) % p.nbits
                } else {
                    p.nbits
                };
                let mut out = Vec::new();
                // Any result but a panic; damage past the header is caught.
                let r = decode_block(&bad, nbits, &mut out);
                if bit > 200 && nbits == p.nbits {
                    assert!(r.is_err() || out == text[..out.len()], "bit {bit}");
                }
            }
        }
    }
}
