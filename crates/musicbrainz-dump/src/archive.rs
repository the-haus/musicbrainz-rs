//! A MusicBrainz export archive (`mbdump.tar.bz2`, `mbdump-derived.tar.bz2`,
//! …): a tar of the tables (`mbdump/<table>`, PostgreSQL COPY text) and the
//! export's metadata (`TIMESTAMP`, `SCHEMA_SEQUENCE`, `REPLICATION_SEQUENCE`,
//! `COPYING`, `README`), read in one pass with bzip2 decoded on every core.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use crate::decompress::{open_decompressed, ReadCounters};
use crate::tar::{EntryKind, TarReader};
use crate::{DumpError, Result};

/// One member of an archive.
#[derive(Clone, Debug)]
pub struct Member {
    /// Its path inside the archive (`mbdump/artist`, `TIMESTAMP`).
    pub name: String,
    /// Its size in bytes.
    pub size: u64,
    /// The table it holds (`artist`), for a member under `mbdump/`.
    pub table: Option<String>,
}

impl Member {
    fn of(name: &str, size: u64) -> Member {
        let table = name
            .strip_prefix("mbdump/")
            .filter(|t| !t.is_empty() && !t.contains('/'))
            .map(str::to_string);
        Member {
            name: name.to_string(),
            size,
            table,
        }
    }
}

/// What to do after a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Go on.
    Continue,
    /// Stop reading (the rest of the archive is not decompressed).
    Stop,
}

/// Progress of a pass over an archive.
#[derive(Clone, Debug, Default)]
pub struct ExtractProgress {
    /// Compressed bytes read.
    pub bytes_in: u64,
    /// The archive's size.
    pub bytes_total: u64,
    /// Decompressed bytes.
    pub bytes_out: u64,
    /// The member being read.
    pub member: Option<String>,
    /// Members written so far.
    pub members: u64,
    /// Milliseconds since the start.
    pub elapsed_ms: u64,
}

/// Called once a member is in place (see [`ExtractOptions::on_member`]).
pub type OnMember<'a> = &'a mut dyn FnMut(&Member, &Path) -> Result<Flow>;

/// Options for [`Archive::extract`].
pub struct ExtractOptions<'a> {
    /// The folder members are written to, at their path (`<dir>/mbdump/artist`).
    pub dir: PathBuf,
    /// Tables to write (`None`: all). Metadata files are always written.
    pub tables: Option<Vec<String>>,
    /// Members to write, besides `tables` (`None`: all).
    pub filter: Option<&'a (dyn Fn(&Member) -> bool + Sync)>,
    /// Keep a member already there (same size) instead of writing it again.
    pub skip_existing: bool,
    /// bzip2 decoder threads (0: every core).
    pub threads: usize,
    /// Called about every 250 ms and after each member.
    pub on_progress: Option<&'a mut dyn FnMut(&ExtractProgress)>,
    /// Called once a member is in place (written beside and renamed),
    /// with its final path; `Flow::Stop` ends the pass.
    pub on_member: Option<OnMember<'a>>,
    /// Polled between writes: stop when it returns true.
    pub should_stop: Option<&'a (dyn Fn() -> bool + Sync)>,
}

impl ExtractOptions<'_> {
    /// Every member into `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        ExtractOptions {
            dir: dir.into(),
            tables: None,
            filter: None,
            skip_existing: true,
            threads: 0,
            on_progress: None,
            on_member: None,
            should_stop: None,
        }
    }
}

/// What an extraction did.
#[derive(Clone, Debug, Default)]
pub struct ExtractStats {
    /// Members written.
    pub written: Vec<Member>,
    /// Members already in place.
    pub kept: Vec<Member>,
    /// Members of the archive not asked for.
    pub skipped: u64,
    /// Whether it stopped before the archive's end.
    pub stopped: bool,
    /// Decompressed bytes.
    pub bytes_out: u64,
    /// Milliseconds it took.
    pub elapsed_ms: u64,
}

/// An archive on disk (see the module docs).
#[derive(Clone, Debug)]
pub struct Archive {
    path: PathBuf,
}

struct Reporter<'a> {
    counters: Arc<ReadCounters>,
    total: u64,
    started: Instant,
    last: Instant,
    members: u64,
    callback: Option<&'a mut dyn FnMut(&ExtractProgress)>,
}

impl Reporter<'_> {
    fn report(&mut self, member: Option<&str>, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last).as_millis() < 250 {
            return;
        }
        self.last = now;
        if let Some(cb) = self.callback.as_mut() {
            cb(&ExtractProgress {
                bytes_in: self.counters.bytes_in.load(Ordering::Relaxed),
                bytes_total: self.total,
                bytes_out: self.counters.bytes_out.load(Ordering::Relaxed),
                member: member.map(str::to_string),
                members: self.members,
                elapsed_ms: now.duration_since(self.started).as_millis() as u64,
            });
        }
    }
}

impl Archive {
    /// The archive at `path` (opened when read).
    pub fn open(path: impl Into<PathBuf>) -> Result<Archive> {
        let path = path.into();
        if !path.is_file() {
            return Err(DumpError::Io(format!("{}: no such archive", path.display())));
        }
        Ok(Archive { path })
    }

    /// Its path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads every member in order: `f` gets its header and a reader of its
    /// data (what it does not read is skipped).
    pub fn for_each(&self, threads: usize, mut f: impl FnMut(&Member, &mut dyn Read) -> Result<Flow>) -> Result<bool> {
        let d = open_decompressed(&self.path, threads)?;
        let mut tar = TarReader::new(d);
        while let Some(entry) = tar.next_entry()? {
            if entry.kind != EntryKind::File {
                continue;
            }
            let member = Member::of(&entry.name, entry.size);
            if f(&member, &mut tar)? == Flow::Stop {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Every member's name and size (one pass over the whole archive).
    pub fn members(&self, threads: usize) -> Result<Vec<Member>> {
        let mut out = Vec::new();
        self.for_each(threads, |m, _| {
            out.push(m.clone());
            Ok(Flow::Continue)
        })?;
        Ok(out)
    }

    /// One small member's bytes (`TIMESTAMP`, `SCHEMA_SEQUENCE`): the
    /// archive is read until it is found. Metadata comes first in
    /// MetaBrainz's archives, so this is quick.
    pub fn read_member(&self, name: &str, threads: usize) -> Result<Option<Vec<u8>>> {
        let mut found = None;
        self.for_each(threads, |m, r| {
            if m.name == name {
                let mut buf = Vec::with_capacity(m.size as usize);
                r.read_to_end(&mut buf)?;
                found = Some(buf);
                return Ok(Flow::Stop);
            }
            // Tables come after the metadata: past the first table, the
            // member is not in the archive's head; keep looking anyway.
            Ok(Flow::Continue)
        })?;
        Ok(found)
    }

    /// Writes members into a folder at their paths (see
    /// [`ExtractOptions`]). Each is written beside (`.partial`) and
    /// renamed, so an interrupted pass leaves no member that looks whole.
    pub fn extract(&self, mut opts: ExtractOptions<'_>) -> Result<ExtractStats> {
        let started = Instant::now();
        let d = open_decompressed(&self.path, opts.threads)?;
        let mut reporter = Reporter {
            counters: d.counters(),
            total: d.input_len(),
            started,
            last: started,
            members: 0,
            callback: opts.on_progress.take(),
        };
        let mut on_member = opts.on_member.take();
        let should_stop = opts.should_stop;
        let stopping = || should_stop.is_some_and(|s| s());
        let mut tar = TarReader::new(d);
        let mut stats = ExtractStats::default();
        fs::create_dir_all(&opts.dir)?;
        let mut buf = vec![0u8; 1 << 20];
        while let Some(entry) = tar.next_entry()? {
            if entry.kind != EntryKind::File {
                continue;
            }
            let member = Member::of(&entry.name, entry.size);
            let wanted = match (&member.table, &opts.tables) {
                (Some(t), Some(list)) => list.iter().any(|w| w == t),
                _ => true,
            };
            let wanted = wanted && opts.filter.is_none_or(|f| f(&member));
            if !wanted || !safe_path(&entry.name) {
                stats.skipped += 1;
                continue;
            }
            let target = opts.dir.join(&entry.name);
            if opts.skip_existing && fs::metadata(&target).is_ok_and(|m| m.len() == entry.size) {
                stats.kept.push(member.clone());
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let partial = partial_path(&target);
                let mut out = BufWriter::with_capacity(1 << 20, File::create(&partial)?);
                loop {
                    if stopping() {
                        drop(out);
                        let _ = fs::remove_file(&partial);
                        stats.stopped = true;
                        reporter.report(None, true);
                        return Ok(finish(stats, &reporter, started));
                    }
                    let n = match tar.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) => {
                            drop(out);
                            let _ = fs::remove_file(&partial);
                            return Err(e.into());
                        }
                    };
                    out.write_all(&buf[..n])?;
                    reporter.report(Some(&entry.name), false);
                }
                out.into_inner()
                    .map_err(|e| io::Error::other(e.to_string()))?
                    .sync_data()?;
                fs::rename(&partial, &target)?;
                stats.written.push(member.clone());
            }
            reporter.members += 1;
            reporter.report(Some(&entry.name), true);
            if let Some(cb) = on_member.as_mut() {
                if cb(&member, &target)? == Flow::Stop {
                    stats.stopped = true;
                    return Ok(finish(stats, &reporter, started));
                }
            }
        }
        reporter.report(None, true);
        Ok(finish(stats, &reporter, started))
    }
}

fn finish(mut stats: ExtractStats, reporter: &Reporter<'_>, started: Instant) -> ExtractStats {
    stats.bytes_out = reporter.counters.bytes_out.load(Ordering::Relaxed);
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    stats
}

/// A member's path stays inside the folder: no `..`, no absolute paths.
fn safe_path(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && Path::new(name)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    target.with_file_name(name)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tar::tests::tar_of;

    pub(crate) fn archive_of(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let tar = tar_of(entries);
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(1));
        e.write_all(&tar).unwrap();
        let path = dir.join("mbdump.tar.bz2");
        fs::write(&path, e.finish().unwrap()).unwrap();
        path
    }

    #[test]
    fn extracts_tables_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = archive_of(
            dir.path(),
            &[
                ("TIMESTAMP", b"2026-10-17 00:22:45.12891+00\n"),
                ("SCHEMA_SEQUENCE", b"31\n"),
                ("mbdump/artist", b"1\tx\n"),
                ("mbdump/area", b"2\ty\n"),
                ("../evil", b"no"),
            ],
        );
        let a = Archive::open(&path).unwrap();
        assert_eq!(a.members(2).unwrap().len(), 5);
        assert_eq!(a.read_member("SCHEMA_SEQUENCE", 2).unwrap().unwrap(), b"31\n");
        let out = dir.path().join("tables");
        let mut seen = Vec::new();
        let mut on_member = |m: &Member, p: &Path| {
            assert!(p.is_file());
            seen.push(m.name.clone());
            Ok(Flow::Continue)
        };
        let mut opts = ExtractOptions::new(&out);
        opts.tables = Some(vec!["artist".into()]);
        opts.on_member = Some(&mut on_member);
        let stats = a.extract(opts).unwrap();
        assert_eq!(seen, vec!["TIMESTAMP", "SCHEMA_SEQUENCE", "mbdump/artist"]);
        assert_eq!(stats.skipped, 2);
        assert_eq!(fs::read(out.join("mbdump/artist")).unwrap(), b"1\tx\n");
        assert!(!out.join("mbdump/area").exists());
        assert!(!dir.path().join("evil").exists());
        // Again: kept, not written.
        let stats = a.extract(ExtractOptions::new(&out)).unwrap();
        assert_eq!(stats.kept.len(), 3);
        assert_eq!(stats.written.len(), 1);
    }
}
