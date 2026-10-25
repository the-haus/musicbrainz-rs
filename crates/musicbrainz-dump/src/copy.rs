//! PostgreSQL's `COPY … TO` text format, as the dump's tables are written:
//! one row a line, fields separated by tabs, `\N` for `NULL`, and
//! backslash escapes (`\\`, `\t`, `\n`, `\r`, `\b`, `\f`, `\v`; octal
//! `\NNN` and hex `\xHH` are read too, though `COPY TO` never writes them).

use std::borrow::Cow;

/// A line's raw fields (escapes not undone).
pub fn fields(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.split(|&b| b == b'\t')
}

/// The number of fields of a line.
pub fn field_count(line: &[u8]) -> usize {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    memchr::memchr_iter(b'\t', line).count() + 1
}

/// Whether a raw field is `NULL`.
pub fn is_null(raw: &[u8]) -> bool {
    raw == b"\\N"
}

/// A raw field's text, escapes undone; `None` for `NULL`.
pub fn text(raw: &[u8]) -> Option<Cow<'_, str>> {
    if is_null(raw) {
        return None;
    }
    if memchr::memchr(b'\\', raw).is_none() {
        return Some(String::from_utf8_lossy(raw));
    }
    Some(Cow::Owned(String::from_utf8_lossy(&unescape(raw)).into_owned()))
}

/// Undoes the backslash escapes of a raw field.
pub fn unescape(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        if b != b'\\' || i + 1 == raw.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let c = raw[i + 1];
        i += 2;
        match c {
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                for _ in 0..2 {
                    match raw.get(i) {
                        Some(&d @ b'0'..=b'7') => {
                            v = v * 8 + (d - b'0') as u32;
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(v as u8);
            }
            b'x' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 2 {
                    match raw.get(i).and_then(|d| (*d as char).to_digit(16)) {
                        Some(d) => {
                            v = v * 16 + d;
                            i += 1;
                            n += 1;
                        }
                        None => break,
                    }
                }
                if n == 0 {
                    out.push(b'x');
                } else {
                    out.push(v as u8);
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_unescapes() {
        let line = b"1\tAC\\\\DC\t\\N\ttab\\there\tnew\\nline\t\n";
        let f: Vec<_> = fields(line).collect();
        assert_eq!(f.len(), 6);
        assert_eq!(field_count(line), 6);
        assert_eq!(text(f[1]).unwrap(), "AC\\DC");
        assert!(text(f[2]).is_none());
        assert_eq!(text(f[3]).unwrap(), "tab\there");
        assert_eq!(text(f[4]).unwrap(), "new\nline");
        assert_eq!(text(f[5]).unwrap(), "");
        assert_eq!(unescape(b"\\101\\x42\\q"), b"ABq");
    }
}
