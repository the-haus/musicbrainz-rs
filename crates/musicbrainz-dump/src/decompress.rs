//! A compressed file as a plain byte stream: bzip2 decoded on every core
//! (the vendored block decoder, [`crate::bz2`]), zstd and gzip on a thread
//! of their own, plain files as they are. MetaBrainz's archives are bzip2.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::parallel::{ordered_map, resolve_threads};
use crate::{bz2, read_full, DumpError, Result};

/// How a file is compressed (from its first bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// bzip2, decoded in parallel.
    Bzip2,
    /// gzip.
    Gzip,
    /// Zstandard.
    Zstd,
    /// None.
    None,
}

impl Compression {
    /// The compression of a file that starts with `head`.
    pub fn sniff(head: &[u8]) -> Compression {
        match head {
            [b'B', b'Z', b'h', _, ..] => Compression::Bzip2,
            [0x1f, 0x8b, ..] => Compression::Gzip,
            [0x28, 0xb5, 0x2f, 0xfd, ..] => Compression::Zstd,
            _ => Compression::None,
        }
    }

    /// `"bzip2"`, `"gzip"`, `"zstd"` or `"none"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Compression::Bzip2 => "bzip2",
            Compression::Gzip => "gzip",
            Compression::Zstd => "zstd",
            Compression::None => "none",
        }
    }
}

/// Bytes read from the file and bytes handed out, updated as it goes.
#[derive(Debug, Default)]
pub struct ReadCounters {
    /// Compressed bytes read.
    pub bytes_in: AtomicU64,
    /// Decompressed bytes handed out.
    pub bytes_out: AtomicU64,
}

/// Counts the bytes read through it.
struct Counted<R> {
    inner: R,
    counters: Arc<ReadCounters>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.counters.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// A decompressed stream (see the module docs).
pub struct Decompressed {
    inner: Box<dyn Read + Send>,
    counters: Arc<ReadCounters>,
    compression: Compression,
    len: u64,
}

impl Decompressed {
    /// The live counters.
    pub fn counters(&self) -> Arc<ReadCounters> {
        self.counters.clone()
    }

    /// The file's compression.
    pub fn compression(&self) -> Compression {
        self.compression
    }

    /// The compressed file's size.
    pub fn input_len(&self) -> u64 {
        self.len
    }
}

impl Read for Decompressed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.counters.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Opens a file, decompressed (`threads` bzip2 decoders, 0: every core).
pub fn open_decompressed(path: &Path, threads: usize) -> Result<Decompressed> {
    let mut file = File::open(path).map_err(|e| DumpError::Io(format!("{}: {e}", path.display())))?;
    let len = file.metadata()?.len();
    let mut head = [0u8; 4];
    let n = read_full(&mut file, &mut head)?;
    let compression = Compression::sniff(&head[..n]);
    let file = File::open(path)?;
    decompressed(file, compression, len, threads)
}

/// A reader decompressed as `compression` says.
pub fn decompressed<R: Read + Send + 'static>(
    reader: R,
    compression: Compression,
    len: u64,
    threads: usize,
) -> Result<Decompressed> {
    let counters = Arc::new(ReadCounters::default());
    let source = Counted {
        inner: reader,
        counters: counters.clone(),
    };
    let inner: Box<dyn Read + Send> = match compression {
        Compression::Bzip2 => {
            let threads = resolve_threads(threads);
            let scanner = bz2::Scanner::new(source)?;
            let decoded = ordered_map(scanner, threads, threads * 2, |p: Result<bz2::Piece>| {
                p.map(bz2::decode_piece)
            });
            Box::new(BlockReader {
                blocks: bz2::Blocks::new(decoded),
                current: Vec::new(),
                at: 0,
            })
        }
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(BufReader::with_capacity(
            1 << 20,
            source,
        ))),
        Compression::Zstd => Box::new(zstd::stream::read::Decoder::new(source)?),
        Compression::None => Box::new(source),
    };
    Ok(Decompressed {
        inner,
        counters,
        compression,
        len,
    })
}

/// bzip2 blocks, in order, as a byte stream.
struct BlockReader<I> {
    blocks: bz2::Blocks<I>,
    current: Vec<u8>,
    at: usize,
}

impl<I: Iterator<Item = Result<bz2::Decoded>>> Read for BlockReader<I> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at == self.current.len() {
            match self.blocks.next() {
                None => return Ok(0),
                Some(Ok(block)) => {
                    self.current = block;
                    self.at = 0;
                }
                Some(Err(e)) => return Err(e.into()),
            }
        }
        let n = buf.len().min(self.current.len() - self.at);
        buf[..n].copy_from_slice(&self.current[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn bzip2(data: &[u8]) -> Vec<u8> {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(1));
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn decodes_bzip2_across_blocks() {
        // Over one 100 kB block at level 1, and not too repetitive.
        let mut data = Vec::new();
        let mut x: u32 = 1;
        for i in 0..400_000u32 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            data.extend_from_slice(format!("{i}\t{}\n", x % 1000).as_bytes());
        }
        let compressed = bzip2(&data);
        let mut d = decompressed(std::io::Cursor::new(compressed), Compression::Bzip2, 0, 4).unwrap();
        let mut out = Vec::new();
        d.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
        assert_eq!(d.counters().bytes_out.load(Ordering::Relaxed), data.len() as u64);
    }

    #[test]
    fn sniffs() {
        assert_eq!(Compression::sniff(b"BZh9"), Compression::Bzip2);
        assert_eq!(Compression::sniff(&[0x28, 0xb5, 0x2f, 0xfd]), Compression::Zstd);
        assert_eq!(Compression::sniff(b"mbdu"), Compression::None);
    }
}
