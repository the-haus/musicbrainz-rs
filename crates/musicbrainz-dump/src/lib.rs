#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

pub mod archive;
pub mod bz2;
pub mod bzdecode;
pub mod copy;
pub mod decompress;
pub mod download;
pub mod export;
pub mod parallel;
pub mod tar;

use std::io::{self, Read};

pub use archive::{Archive, ExtractOptions, ExtractProgress, ExtractStats, Flow, Member};
pub use decompress::{open_decompressed, Compression, Decompressed, ReadCounters};
pub use export::{ExportInfo, ExportLayout};
pub use parallel::{default_threads, live_threads, ordered_map, resolve_threads, OrderedMap};

/// Errors reading, extracting or downloading dumps.
#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    /// File system or stream error.
    #[error("{0}")]
    Io(String),
    /// HTTP error.
    #[error("HTTP: {0}")]
    Http(String),
    /// Not what was expected, or corrupt.
    #[error("{0}")]
    Format(String),
    /// The input ended early.
    #[error("truncated input: {0}")]
    Truncated(String),
    /// Stopped by the caller.
    #[error("stopped")]
    Stopped,
}

impl From<io::Error> for DumpError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            DumpError::Truncated(e.to_string())
        } else {
            DumpError::Io(e.to_string())
        }
    }
}

impl From<DumpError> for io::Error {
    fn from(e: DumpError) -> Self {
        match e {
            DumpError::Io(m) => io::Error::other(m),
            DumpError::Truncated(m) => io::Error::new(io::ErrorKind::UnexpectedEof, m),
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, DumpError>;

/// The User-Agent of HTTP requests.
pub const USER_AGENT: &str = concat!(
    "musicbrainz-rs/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/the-haus/musicbrainz-rs)"
);

/// Where a bzip2 block starts (kept for the vendored decoder's API).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockStart {
    /// Its first bit.
    pub bit: u64,
    /// The combined CRC of the stream's blocks before it.
    pub crc: u32,
    /// The blocks before it.
    pub blocks: u64,
    /// The streams that ended before it.
    pub streams: u64,
    /// Unused here: always false.
    pub plain: bool,
}

/// Read until `buf` is full or the end of input; returns the bytes read.
pub(crate) fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
