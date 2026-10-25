//! A streaming tar reader: one pass, never seeking, so it reads a tar
//! straight out of the decompressor. ustar, GNU long names and pax headers
//! (`path`, `size`) are understood; MetaBrainz's archives are plain ustar.

use std::io::{self, Read};

use crate::{read_full, DumpError, Result};

const BLOCK: usize = 512;

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// Anything else (links, devices): its data is skipped.
    Other,
}

/// One entry's header.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Its path inside the archive (`mbdump/artist`).
    pub name: String,
    /// Its data's size in bytes.
    pub size: u64,
    /// What it is.
    pub kind: EntryKind,
    /// Its modification time (seconds since the epoch).
    pub mtime: u64,
}

/// Reads a tar archive entry by entry (see the module docs).
pub struct TarReader<R> {
    inner: R,
    /// Data bytes of the current entry not read yet.
    remaining: u64,
    /// Padding after the current entry's data.
    padding: u64,
    /// Bytes of the archive consumed.
    position: u64,
    done: bool,
}

/// An octal (or GNU base-256) number field.
fn number(field: &[u8]) -> Result<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        let mut n: u64 = (field[0] & 0x7f) as u64;
        for &b in &field[1..] {
            n = n
                .checked_mul(256)
                .and_then(|n| n.checked_add(b as u64))
                .ok_or_else(|| DumpError::Format("tar: size overflows".into()))?;
        }
        return Ok(n);
    }
    let text = field
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect::<String>();
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| DumpError::Format(format!("tar: bad number {text:?}")))
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// `len key=value\n` records of a pax header.
fn pax_records(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let Some(space) = rest.iter().position(|&b| b == b' ') else {
            break;
        };
        let Ok(len) = std::str::from_utf8(&rest[..space]).unwrap_or("").parse::<usize>() else {
            break;
        };
        if len <= space || len > rest.len() {
            break;
        }
        let record = &rest[space + 1..len];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(eq) = record.iter().position(|&b| b == b'=') {
            out.push((
                String::from_utf8_lossy(&record[..eq]).into_owned(),
                String::from_utf8_lossy(&record[eq + 1..]).into_owned(),
            ));
        }
        rest = &rest[len..];
    }
    out
}

impl<R: Read> TarReader<R> {
    /// Read the tar in `inner`.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            remaining: 0,
            padding: 0,
            position: 0,
            done: false,
        }
    }

    /// Bytes of the archive consumed so far.
    pub fn position(&self) -> u64 {
        self.position
    }

    fn skip(&mut self, mut n: u64) -> Result<()> {
        let mut buf = [0u8; 64 * 1024];
        while n > 0 {
            let k = (n as usize).min(buf.len());
            let got = read_full(&mut self.inner, &mut buf[..k])?;
            if got < k {
                return Err(DumpError::Truncated("tar: an entry ends early".into()));
            }
            self.position += got as u64;
            n -= got as u64;
        }
        Ok(())
    }

    fn read_exact_counted(&mut self, buf: &mut [u8]) -> Result<bool> {
        let got = read_full(&mut self.inner, buf)?;
        self.position += got as u64;
        if got == 0 {
            return Ok(false);
        }
        if got < buf.len() {
            return Err(DumpError::Truncated("tar: a header ends early".into()));
        }
        Ok(true)
    }

    /// The next entry, skipping what is left of the current one; `None` at
    /// the end of the archive.
    pub fn next_entry(&mut self) -> Result<Option<Entry>> {
        if self.done {
            return Ok(None);
        }
        self.skip(self.remaining + self.padding)?;
        self.remaining = 0;
        self.padding = 0;
        let mut long_name: Option<String> = None;
        let mut pax: Vec<(String, String)> = Vec::new();
        loop {
            let mut header = [0u8; BLOCK];
            if !self.read_exact_counted(&mut header)? {
                // No end-of-archive blocks: lenient, as GNU tar is.
                self.done = true;
                return Ok(None);
            }
            if header.iter().all(|&b| b == 0) {
                self.done = true;
                return Ok(None);
            }
            let stored: u64 = number(&header[148..156])?;
            let sum: u64 = header
                .iter()
                .enumerate()
                .map(|(i, &b)| if (148..156).contains(&i) { b' ' as u64 } else { b as u64 })
                .sum();
            if stored != sum {
                return Err(DumpError::Format(format!(
                    "tar: header checksum mismatch at byte {}",
                    self.position - BLOCK as u64
                )));
            }
            let mut size = number(&header[124..136])?;
            let flag = header[156];
            let padding = |size: u64| (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
            match flag {
                b'L' | b'x' | b'g' => {
                    let mut data = vec![0u8; size as usize];
                    if !data.is_empty() && !self.read_exact_counted(&mut data)? {
                        return Err(DumpError::Truncated("tar: an extended header ends early".into()));
                    }
                    self.skip(padding(size))?;
                    match flag {
                        b'L' => long_name = Some(text(&data)),
                        b'x' => pax = pax_records(&data),
                        _ => {}
                    }
                    continue;
                }
                _ => {}
            }
            let mut name = text(&header[0..100]);
            if &header[257..262] == b"ustar" {
                let prefix = text(&header[345..500]);
                if !prefix.is_empty() {
                    name = format!("{prefix}/{name}");
                }
            }
            if let Some(n) = long_name.take() {
                name = n;
            }
            for (k, v) in &pax {
                match k.as_str() {
                    "path" => name = v.clone(),
                    "size" => size = v.parse().unwrap_or(size),
                    _ => {}
                }
            }
            let kind = match flag {
                b'0' | 0 | b'7' => EntryKind::File,
                b'5' => EntryKind::Directory,
                _ => EntryKind::Other,
            };
            let mtime = number(&header[136..148]).unwrap_or(0);
            self.remaining = size;
            self.padding = padding(size);
            if kind == EntryKind::Directory {
                self.skip(self.remaining + self.padding)?;
                self.remaining = 0;
                self.padding = 0;
            }
            return Ok(Some(Entry {
                name,
                size,
                kind,
                mtime,
            }));
        }
    }

    /// Bytes of the current entry's data not read yet.
    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

impl<R: Read> Read for TarReader<R> {
    /// Reads the current entry's data (0 at its end).
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let k = (self.remaining as usize).min(buf.len());
        let n = self.inner.read(&mut buf[..k])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "tar: an entry ends early"));
        }
        self.remaining -= n as u64;
        self.position += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A ustar archive of `(name, data)` entries (for tests).
    pub(crate) fn tar_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in entries {
            let mut h = [0u8; BLOCK];
            let long = name.len() > 100;
            if long {
                let mut l = [0u8; BLOCK];
                l[..13].copy_from_slice(b"././@LongLink");
                l[100..108].copy_from_slice(b"0000644\0");
                l[124..136].copy_from_slice(format!("{:011o}\0", name.len() + 1).as_bytes());
                l[156] = b'L';
                l[257..263].copy_from_slice(b"ustar ");
                checksum(&mut l);
                out.extend_from_slice(&l);
                let mut n = name.as_bytes().to_vec();
                n.push(0);
                n.resize(n.len().div_ceil(BLOCK) * BLOCK, 0);
                out.extend_from_slice(&n);
            }
            let short = &name.as_bytes()[..name.len().min(100)];
            h[..short.len()].copy_from_slice(short);
            h[100..108].copy_from_slice(b"0000644\0");
            h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
            h[136..148].copy_from_slice(b"14000000000\0");
            h[156] = b'0';
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            checksum(&mut h);
            out.extend_from_slice(&h);
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
        out.extend_from_slice(&[0u8; BLOCK * 2]);
        out
    }

    fn checksum(h: &mut [u8; BLOCK]) {
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }

    #[test]
    fn reads_entries_and_skips_unread_data() {
        let long = format!("mbdump/{}", "x".repeat(120));
        let tar = tar_of(&[
            ("TIMESTAMP", b"2026-10-17\n"),
            ("mbdump/artist", b"1\tabc\n2\tdef\n"),
            (&long, b"long"),
        ]);
        let mut r = TarReader::new(&tar[..]);
        let e = r.next_entry().unwrap().unwrap();
        assert_eq!((e.name.as_str(), e.size), ("TIMESTAMP", 11));
        // Not read: skipped.
        let e = r.next_entry().unwrap().unwrap();
        assert_eq!(e.name, "mbdump/artist");
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        assert_eq!(s, "1\tabc\n2\tdef\n");
        let e = r.next_entry().unwrap().unwrap();
        assert_eq!(e.name, long);
        assert!(r.next_entry().unwrap().is_none());
    }

    #[test]
    fn detects_corruption_and_truncation() {
        let mut tar = tar_of(&[("a", b"hello")]);
        tar[0] = b'b';
        assert!(TarReader::new(&tar[..]).next_entry().is_err());
        let tar = tar_of(&[("a", &[7u8; 2000])]);
        let mut r = TarReader::new(&tar[..1000]);
        r.next_entry().unwrap();
        let mut sink = Vec::new();
        assert!(r.read_to_end(&mut sink).is_err());
    }

    #[test]
    fn pax_paths() {
        assert_eq!(
            pax_records(b"19 path=mbdump/abc\n12 size=123\n"),
            vec![("path".into(), "mbdump/abc".into()), ("size".into(), "123".into())]
        );
    }
}
