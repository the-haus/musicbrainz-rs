//! MetaBrainz's full exports over HTTP: the newest export's name
//! (`LATEST`), an export's checksums (`SHA256SUMS`), and downloads that
//! resume where they stopped and are checked against the published SHA-256
//! before they get their final name.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::{DumpError, Result, USER_AGENT};

/// Where MetaBrainz publishes the full exports.
pub const FULL_EXPORTS: &str = "https://data.metabrainz.org/pub/musicbrainz/data/fullexport";

/// HTTP settings.
#[derive(Clone, Debug)]
pub struct HttpOptions {
    /// The User-Agent (MetaBrainz asks for one that names the application).
    pub user_agent: String,
    /// Connect and response timeout.
    pub timeout: Duration,
    /// Attempts after a dropped connection without progress.
    pub retries: u32,
}

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions {
            user_agent: USER_AGENT.to_string(),
            timeout: Duration::from_secs(60),
            retries: 8,
        }
    }
}

fn agent(opts: &HttpOptions) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(opts.user_agent.as_str())
        .timeout_connect(Some(opts.timeout))
        .timeout_recv_response(Some(opts.timeout))
        .timeout_recv_body(None)
        .http_status_as_error(false)
        .build()
        .into()
}

fn base_url(base: &str) -> &str {
    base.trim_end_matches('/')
}

fn get_text(agent: &ureq::Agent, url: &str) -> Result<String> {
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| DumpError::Http(format!("{url}: {e}")))?;
    match resp.status().as_u16() {
        200 => {
            let (_, body) = resp.into_parts();
            let mut s = String::new();
            body.into_reader().take(16 << 20).read_to_string(&mut s)?;
            Ok(s)
        }
        s => Err(DumpError::Http(format!("{url}: HTTP status {s}"))),
    }
}

/// Whether a name is an export's (`20261017-002244`).
pub fn is_export_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 15 && b[8] == b'-' && b.iter().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit())
}

/// The newest export's name (`LATEST`), e.g. `20261017-002244`.
pub fn latest_export(base: &str, http: &HttpOptions) -> Result<String> {
    let name = get_text(&agent(http), &format!("{}/LATEST", base_url(base)))?
        .trim()
        .to_string();
    if !is_export_name(&name) {
        return Err(DumpError::Format(format!("LATEST names {name:?}, not an export")));
    }
    Ok(name)
}

/// An export's published checksums, as `(file, sha256 hex)`.
pub fn export_checksums(base: &str, name: &str, http: &HttpOptions) -> Result<Vec<(String, String)>> {
    let text = get_text(&agent(http), &format!("{}/{name}/SHA256SUMS", base_url(base)))?;
    Ok(parse_checksums(&text))
}

/// `<hex> *<file>` (or two spaces) lines.
pub fn parse_checksums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (hash, file) = l.trim().split_once(char::is_whitespace)?;
            let file = file.trim().trim_start_matches('*');
            (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| (file.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// Download progress.
#[derive(Clone, Debug, Default)]
pub struct DownloadProgress {
    /// The file being fetched.
    pub file: String,
    /// Its bytes on disk.
    pub bytes: u64,
    /// Its size, when the server says.
    pub total: Option<u64>,
    /// Bytes fetched in this run (not counting a resumed part).
    pub fetched: u64,
    /// Milliseconds since the start.
    pub elapsed_ms: u64,
}

/// Options for [`download_export`].
pub struct DownloadOptions<'a> {
    /// Where the exports are (default [`FULL_EXPORTS`]; a mirror's).
    pub base: String,
    /// The files of the export to fetch (default `mbdump.tar.bz2`).
    pub files: Vec<String>,
    /// HTTP settings.
    pub http: HttpOptions,
    /// Called about every 250 ms.
    pub on_progress: Option<&'a mut dyn FnMut(&DownloadProgress)>,
    /// Polled while fetching: stop when it returns true (the part is kept,
    /// and the next download resumes it).
    pub should_stop: Option<&'a (dyn Fn() -> bool + Sync)>,
}

impl Default for DownloadOptions<'_> {
    fn default() -> Self {
        DownloadOptions {
            base: FULL_EXPORTS.to_string(),
            files: vec!["mbdump.tar.bz2".to_string()],
            http: HttpOptions::default(),
            on_progress: None,
            should_stop: None,
        }
    }
}

/// What a download did.
#[derive(Clone, Debug, Default)]
pub struct DownloadStats {
    /// The export's folder (`<dir>/<name>`).
    pub dir: PathBuf,
    /// The files in place, checked.
    pub files: Vec<PathBuf>,
    /// Bytes fetched in this run.
    pub fetched: u64,
    /// Files that were in place and checked already.
    pub already_done: u64,
    /// Milliseconds it took.
    pub elapsed_ms: u64,
}

/// Downloads files of an export into `<dir>/<name>/` (see the module
/// docs), with `SHA256SUMS` beside them. A file is fetched into
/// `<file>.part`, resumed from there after a stop or a dropped
/// connection, hashed as it arrives, and renamed once the hash matches.
/// A file already in place is checked, not fetched again.
pub fn download_export(dir: &Path, name: &str, mut opts: DownloadOptions<'_>) -> Result<DownloadStats> {
    if !is_export_name(name) {
        return Err(DumpError::Format(format!("{name:?} is not an export's name")));
    }
    let started = Instant::now();
    let http = agent(&opts.http);
    let base = base_url(&opts.base).to_string();
    let sums_text = get_text(&http, &format!("{base}/{name}/SHA256SUMS"))?;
    let sums = parse_checksums(&sums_text);
    let out_dir = dir.join(name);
    fs::create_dir_all(&out_dir)?;
    fs::write(out_dir.join("SHA256SUMS"), &sums_text)?;
    let mut stats = DownloadStats {
        dir: out_dir.clone(),
        ..Default::default()
    };
    for file in opts.files.clone() {
        let digest = sums
            .iter()
            .find(|(f, _)| *f == file)
            .map(|(_, h)| h.clone())
            .ok_or_else(|| DumpError::Format(format!("{name}: SHA256SUMS lists no {file}")))?;
        let target = out_dir.join(&file);
        if target.is_file() {
            if sha256_file(&target)? == digest {
                stats.already_done += 1;
                stats.files.push(target);
                continue;
            }
            fs::remove_file(&target)?;
        }
        let url = format!("{base}/{name}/{file}");
        let part = out_dir.join(format!("{file}.part"));
        let fetched = fetch_resumable(&http, &url, &part, &file, &digest, &mut opts, started)?;
        stats.fetched += fetched;
        fs::rename(&part, &target)?;
        stats.files.push(target);
    }
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(stats)
}

/// The SHA-256 of a file, in hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Fetches `url` into `part` (resuming it), checking `digest` at the end.
fn fetch_resumable(
    http: &ureq::Agent,
    url: &str,
    part: &Path,
    file: &str,
    digest: &str,
    opts: &mut DownloadOptions<'_>,
    started: Instant,
) -> Result<u64> {
    let mut out = OpenOptions::new().create(true).read(true).append(true).open(part)?;
    // What is there already is hashed first.
    let mut hasher = Sha256::new();
    let mut have = 0u64;
    {
        let mut r = File::open(part)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            have += n as u64;
        }
    }
    out.seek(SeekFrom::End(0))?;
    let mut fetched = 0u64;
    let mut total: Option<u64> = None;
    let mut failures = 0u32;
    let mut last = Instant::now();
    let mut buf = vec![0u8; 1 << 20];
    'connect: loop {
        if total.is_some_and(|t| have >= t) {
            break;
        }
        let mut req = http.get(url);
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(e) => {
                failures += 1;
                if failures > opts.http.retries {
                    return Err(DumpError::Http(format!("{url}: {e}")));
                }
                std::thread::sleep(Duration::from_millis(500 * failures as u64));
                continue;
            }
        };
        let status = resp.status().as_u16();
        let header = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
        match (have, status) {
            (0, 200) => total = header("content-length").and_then(|v| v.parse().ok()),
            (h, 206) if h > 0 => {
                let range = header("content-range").unwrap_or_default();
                let start = range
                    .strip_prefix("bytes ")
                    .and_then(|r| r.split('-').next())
                    .and_then(|s| s.parse::<u64>().ok());
                if start != Some(h) {
                    return Err(DumpError::Http(format!(
                        "{url}: resumed at the wrong place ({range:?})"
                    )));
                }
                total = range.rsplit('/').next().and_then(|t| t.parse().ok());
            }
            (h, 416) if h > 0 => break,
            (h, 200) if h > 0 => {
                // No ranges: start again.
                out.set_len(0)?;
                out.seek(SeekFrom::Start(0))?;
                hasher = Sha256::new();
                have = 0;
                total = header("content-length").and_then(|v| v.parse().ok());
            }
            (_, s) => return Err(DumpError::Http(format!("{url}: HTTP status {s}"))),
        }
        let (_, body) = resp.into_parts();
        let mut body = body.into_reader();
        loop {
            if opts.should_stop.is_some_and(|s| s()) {
                out.flush()?;
                return Err(DumpError::Stopped);
            }
            let n = match body.read(&mut buf) {
                Ok(0) => break 'connect,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    failures += 1;
                    if failures > opts.http.retries {
                        return Err(DumpError::Http(format!("{url}: the connection keeps dropping")));
                    }
                    continue 'connect;
                }
            };
            out.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            have += n as u64;
            fetched += n as u64;
            failures = 0;
            if last.elapsed() >= Duration::from_millis(250) {
                last = Instant::now();
                if let Some(cb) = opts.on_progress.as_mut() {
                    cb(&DownloadProgress {
                        file: file.to_string(),
                        bytes: have,
                        total,
                        fetched,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    });
                }
            }
        }
    }
    out.sync_data()?;
    let got = hex(&hasher.finalize());
    if got != digest {
        // A corrupt part cannot be resumed: start again next time.
        drop(out);
        fs::remove_file(part)?;
        return Err(DumpError::Format(format!(
            "{file}: SHA-256 {got} does not match the published {digest}"
        )));
    }
    if let Some(cb) = opts.on_progress.as_mut() {
        cb(&DownloadProgress {
            file: file.to_string(),
            bytes: have,
            total: Some(have),
            fetched,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        let text = "77d19541246a78828e364163fb2076113cfdbd737ca40775579def0c9c502228 *mbdump-cdstubs.tar.bz2\n\
                    5fb9f7f685eb8ab0189fca6ae8d28a2a155a98dff37b3c6ada6db13c40eec51f  mbdump.tar.bz2\n";
        let sums = parse_checksums(text);
        assert_eq!(sums.len(), 2);
        assert_eq!(sums[1].0, "mbdump.tar.bz2");
        assert!(is_export_name("20261017-002244"));
        assert!(!is_export_name("LATEST"));
    }

    #[test]
    fn hashes_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
