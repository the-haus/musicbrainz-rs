//! Building a database from an export, in one pass:
//!
//! ```text
//! archive ─▶ bzip2 (every core) ─▶ tar ─▶ member file ─▶ DuckDB read_csv (every core) ─▶ table
//!          └──────────── extract thread ──────────┘  └──────────── loader thread ────────────┘
//! ```
//!
//! Tables taken out already (a `mbdump/` folder) are loaded where they are;
//! the others are extracted into a work folder one at a time, loaded as soon
//! as each is in place, and removed (unless kept). The channel between the
//! two threads holds one member, so the work folder never holds more than
//! about three tables. The database is written beside its final name
//! (`<db>.building`) and renamed when whole: a reader never sees a half-built
//! one, and a failed build leaves the previous database as it was.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use duckdb::{Config, Connection};
use musicbrainz_dump::archive::Flow;
use musicbrainz_dump::{Archive, ExportInfo, ExportLayout, ExtractOptions, ExtractProgress, Member};
use musicbrainz_schema::{Schema, Table, DUMP_FILES};
use serde::Serialize;

use crate::ddl::{self, IndexLevel};
use crate::diff::{diff_into, DiffStats};
use crate::schema_source::{resolve_schema, SchemaOrigin};
use crate::{Error, Result, DATABASE_FORMAT};

/// Progress reports (from the extract and loader threads).
pub type ProgressFn = Arc<dyn Fn(&BuildProgress) + Send + Sync>;

/// Options for [`build`] and [`refresh`].
#[derive(Clone)]
pub struct BuildOptions {
    /// An export: its archive, its folder, or a folder of tables taken out
    /// (see [`ExportLayout`]).
    pub source: PathBuf,
    /// The database file to write (`musicbrainz.duckdb`).
    pub database: PathBuf,
    /// Only these tables (`None`: every table of the archives read).
    pub tables: Option<Vec<String>>,
    /// Archives besides the core (`mbdump.tar.bz2`) whose tables to load,
    /// by file name (`mbdump-derived.tar.bz2`); they must be in the
    /// export's folder. Mind their licences ([`musicbrainz_schema::DumpFile`]).
    pub archives: Vec<String>,
    /// Where members are extracted to (default `<database>.work`).
    pub work_dir: Option<PathBuf>,
    /// Keep the extracted tables in the work folder (a later build of the
    /// same export then loads them without decompressing).
    pub keep_tables: bool,
    /// Which indexes to make.
    pub indexes: IndexLevel,
    /// The schema's SQL folder (default: by the dump's `SCHEMA_SEQUENCE`).
    pub schema_dir: Option<PathBuf>,
    /// Where schemas fetched for other sequences are cached (default
    /// `<database's folder>/.musicbrainz-schema`).
    pub schema_cache: Option<PathBuf>,
    /// Never fetch a schema.
    pub offline: bool,
    /// DuckDB and bzip2 threads (0: every core).
    pub threads: usize,
    /// DuckDB's memory limit (`"8GB"`; default DuckDB's, 80% of RAM).
    pub memory_limit: Option<String>,
    /// Replace an existing database (a plain build refuses to).
    pub replace: bool,
    /// Make the schema's `NOT NULL` constraints (default true). Off, a dump
    /// that leaves such columns `\N` (a hand-made test dump) still loads.
    pub constraints: bool,
    /// Progress reports.
    pub on_progress: Option<ProgressFn>,
    /// Set to stop: the build ends with [`Error::Stopped`], leaving any
    /// previous database as it was.
    pub stop: Option<Arc<AtomicBool>>,
}

impl BuildOptions {
    /// A build of `source` into `database`, with defaults.
    pub fn new(source: impl Into<PathBuf>, database: impl Into<PathBuf>) -> Self {
        BuildOptions {
            source: source.into(),
            database: database.into(),
            tables: None,
            archives: Vec::new(),
            work_dir: None,
            keep_tables: false,
            indexes: IndexLevel::Keys,
            schema_dir: None,
            schema_cache: None,
            offline: false,
            threads: 0,
            memory_limit: None,
            replace: false,
            constraints: true,
            on_progress: None,
            stop: None,
        }
    }
}

/// A progress report.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildProgress {
    /// `schema`, `extract`, `load`, `index`, `views`, `diff` or `done`.
    pub phase: &'static str,
    /// The table being loaded or indexed.
    pub table: Option<String>,
    /// Tables loaded.
    pub tables_loaded: u32,
    /// Tables expected (the archives' as listed for the bundled schema:
    /// an estimate for another one).
    pub tables_expected: u32,
    /// Rows loaded.
    pub rows: u64,
    /// Compressed bytes read.
    pub bytes_in: u64,
    /// The archives' size.
    pub bytes_total: u64,
    /// Decompressed bytes.
    pub bytes_out: u64,
    /// Milliseconds since the start.
    pub elapsed_ms: u64,
}

/// One table loaded.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableStats {
    /// Its name.
    pub name: String,
    /// Its rows.
    pub rows: u64,
    /// The dump file's size.
    pub bytes: u64,
    /// Milliseconds to load it.
    pub load_ms: u64,
    /// Where it came from: `tables` (a folder), `archive`, or `empty` (a
    /// table of the archive's list that the archive left out, as it does
    /// empty ones).
    pub from: &'static str,
}

/// What a build did.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildStats {
    /// The database written.
    pub database: PathBuf,
    /// The export's metadata.
    pub export: ExportInfoJson,
    /// The schema used: `bundled` or the SQL folder's path.
    pub schema: String,
    /// The tables loaded, in load order.
    pub tables: Vec<TableStats>,
    /// Members of the archives not loaded, with why.
    pub skipped: Vec<(String, String)>,
    /// What changed since the previous database ([`refresh`] only).
    pub diff: Option<DiffStats>,
    /// Milliseconds it took.
    pub elapsed_ms: u64,
}

/// [`ExportInfo`] for JSON.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportInfoJson {
    /// The export's name (`20261017-002244`).
    pub name: Option<String>,
    /// When it was taken.
    pub timestamp: Option<String>,
    /// Its schema sequence.
    pub schema_sequence: Option<u32>,
    /// Its replication sequence.
    pub replication_sequence: Option<u64>,
}

impl From<&ExportInfo> for ExportInfoJson {
    fn from(i: &ExportInfo) -> Self {
        ExportInfoJson {
            name: i.name.clone(),
            timestamp: i.timestamp.clone(),
            schema_sequence: i.schema_sequence,
            replication_sequence: i.replication_sequence,
        }
    }
}

/// What the loader is asked to do.
struct LoadJob {
    table: Table,
    file: PathBuf,
    remove_after: bool,
    from: &'static str,
}

/// Shared progress state.
struct Progress {
    started: Instant,
    callback: Option<ProgressFn>,
    tables_loaded: AtomicU64,
    tables_expected: u32,
    rows: AtomicU64,
    extract: Mutex<ExtractProgress>,
}

impl Progress {
    fn report(&self, phase: &'static str, table: Option<&str>) {
        let Some(cb) = &self.callback else {
            return;
        };
        let e = self.extract.lock().unwrap().clone();
        cb(&BuildProgress {
            phase,
            table: table.map(str::to_string),
            tables_loaded: self.tables_loaded.load(Ordering::Relaxed) as u32,
            tables_expected: self.tables_expected,
            rows: self.rows.load(Ordering::Relaxed),
            bytes_in: e.bytes_in,
            bytes_total: e.bytes_total,
            bytes_out: e.bytes_out,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
        });
    }
}

fn building_path(db: &Path) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push(".building");
    PathBuf::from(s)
}

/// Removes a database file and DuckDB's files beside it.
fn remove_database(path: &Path) {
    let _ = fs::remove_file(path);
    for ext in [".wal", ".tmp"] {
        let mut s = path.as_os_str().to_os_string();
        s.push(ext);
        let p = PathBuf::from(s);
        if p.is_dir() {
            let _ = fs::remove_dir_all(&p);
        } else {
            let _ = fs::remove_file(&p);
        }
    }
}

pub(crate) fn connect(path: &Path, threads: usize, memory: Option<&str>, read_only: bool) -> Result<Connection> {
    let mut config = Config::default();
    if threads > 0 {
        config = config.threads(threads as i64)?;
    }
    if let Some(m) = memory {
        config = config.max_memory(m)?;
    }
    if read_only {
        config = config.access_mode(duckdb::AccessMode::ReadOnly)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    config = config.with("temp_directory", tmp.to_string_lossy())?;
    Ok(Connection::open_with_flags(path, config)?)
}

/// The fields of a dump file's first line (`None`: it is empty).
fn first_line_fields(file: &Path) -> Result<Option<usize>> {
    let mut r = BufReader::new(fs::File::open(file)?);
    let mut line = Vec::new();
    r.read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(None);
    }
    Ok(Some(musicbrainz_dump::copy::field_count(&line)))
}

/// Loads one table (on the loader thread).
fn load(conn: &Connection, job: &LoadJob, sequence: u32, not_null: bool) -> Result<TableStats> {
    let started = Instant::now();
    let t = &job.table;
    let bytes = fs::metadata(&job.file)?.len();
    if let Some(n) = first_line_fields(&job.file)? {
        if n != t.columns.len() {
            return Err(Error::Schema(format!(
                "{}: {n} fields in the dump, {} columns in schema {sequence}: the dump is of another schema \
                 (give its SQL with schema_dir)",
                t.name,
                t.columns.len()
            )));
        }
    }
    if let Some(s) = ddl::table_schema(&t.name) {
        conn.execute_batch(&format!("CREATE SCHEMA IF NOT EXISTS {}", ddl::ident(s)))?;
    }
    conn.execute_batch(&ddl::create_table(t, not_null))?;
    let rows = if bytes == 0 {
        0
    } else {
        conn.execute(&ddl::load_table(t, &job.file), [])? as u64
    };
    if job.remove_after {
        let _ = fs::remove_file(&job.file);
    }
    Ok(TableStats {
        name: t.name.clone(),
        rows,
        bytes,
        load_ms: started.elapsed().as_millis() as u64,
        from: job.from,
    })
}

fn stopped(stop: &Option<Arc<AtomicBool>>) -> bool {
    stop.as_ref().is_some_and(|s| s.load(Ordering::Relaxed))
}

/// Writes `mb_meta` and `mb_tables`.
fn write_meta(
    conn: &Connection,
    info: &ExportInfo,
    origin: &SchemaOrigin,
    opts: &BuildOptions,
    tables: &[TableStats],
) -> Result<()> {
    conn.execute_batch(
        "CREATE OR REPLACE TABLE mb_meta (key VARCHAR PRIMARY KEY, value VARCHAR);
         CREATE OR REPLACE TABLE mb_tables (name VARCHAR PRIMARY KEY, rows BIGINT, bytes BIGINT, load_ms BIGINT, source VARCHAR);",
    )?;
    let mut meta: BTreeMap<&str, Option<String>> = BTreeMap::new();
    meta.insert("format", Some(DATABASE_FORMAT.to_string()));
    meta.insert("builder", Some(musicbrainz_dump::USER_AGENT.to_string()));
    meta.insert("export_name", info.name.clone());
    meta.insert("timestamp", info.timestamp.clone());
    meta.insert("schema_sequence", info.schema_sequence.map(|s| s.to_string()));
    meta.insert("replication_sequence", info.replication_sequence.map(|s| s.to_string()));
    meta.insert(
        "schema",
        Some(match origin {
            SchemaOrigin::Bundled => "bundled".to_string(),
            SchemaOrigin::Files(p) => p.display().to_string(),
        }),
    );
    meta.insert("source", Some(opts.source.display().to_string()));
    meta.insert("indexes", Some(opts.indexes.as_str().to_string()));
    let mut stmt = conn.prepare("INSERT INTO mb_meta VALUES (?, ?)")?;
    for (k, v) in &meta {
        stmt.execute(duckdb::params![k, v])?;
    }
    conn.execute("INSERT INTO mb_meta VALUES ('built_at', CAST(now() AS VARCHAR))", [])?;
    let mut stmt = conn.prepare("INSERT INTO mb_tables VALUES (?, ?, ?, ?, ?)")?;
    for t in tables {
        stmt.execute(duckdb::params![
            t.name,
            t.rows as i64,
            t.bytes as i64,
            t.load_ms as i64,
            t.from
        ])?;
    }
    Ok(())
}

/// The archives a build reads: the core, then those asked for.
fn archives_of(layout: &ExportLayout, extra: &[String]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if let Some(core) = layout.core_archive() {
        out.push(core.to_path_buf());
    }
    for name in extra {
        let found = layout
            .archives
            .iter()
            .find(|p| p.file_name().is_some_and(|n| n == name.as_str()))
            .ok_or_else(|| Error::Invalid(format!("no {name} beside the export's core archive")))?;
        if !out.contains(found) {
            out.push(found.clone());
        }
    }
    Ok(out)
}

/// Builds the database at `opts.database` from `opts.source` (see the
/// module docs). Refuses to replace an existing database unless
/// `opts.replace`; [`refresh`] replaces it and diffs.
pub fn build(opts: BuildOptions) -> Result<BuildStats> {
    if opts.database.exists() && !opts.replace {
        return Err(Error::Invalid(format!(
            "{} exists: refresh it, or build with replace",
            opts.database.display()
        )));
    }
    run(opts, false)
}

/// Builds a new database from `opts.source`, diffs it against the one at
/// `opts.database` (when there is one: `mb_changes` in the new database
/// says what was added, changed, removed or merged), and puts it in the
/// old one's place. The old file is kept as `<database>.previous` when
/// `keep_previous`.
pub fn refresh(opts: BuildOptions, keep_previous: bool) -> Result<BuildStats> {
    let stats = run(opts.clone(), true)?;
    if keep_previous && opts.database.exists() {
        let mut prev = opts.database.as_os_str().to_os_string();
        prev.push(".previous");
        let prev = PathBuf::from(prev);
        remove_database(&prev);
        // A hard link keeps the old file readable at both names, so the
        // rename below replaces the database in one step.
        if fs::hard_link(&opts.database, &prev).is_err() {
            fs::copy(&opts.database, &prev)?;
        }
    }
    fs::rename(building_path(&opts.database), &opts.database)?;
    Ok(BuildStats {
        database: opts.database.clone(),
        ..stats
    })
}

fn run(opts: BuildOptions, diff_previous: bool) -> Result<BuildStats> {
    let started = Instant::now();
    let threads = musicbrainz_dump::resolve_threads(opts.threads);
    let layout = ExportLayout::find(&opts.source)?;
    let info = layout.info(threads)?;
    let cache = opts.schema_cache.clone().unwrap_or_else(|| {
        opts.database
            .parent()
            .unwrap_or(Path::new("."))
            .join(".musicbrainz-schema")
    });
    let (schema, origin) = resolve_schema(
        info.schema_sequence,
        opts.schema_dir.as_deref(),
        Some(&cache),
        opts.offline,
    )?;
    let sequence = schema.sequence;
    if let Some(list) = &opts.tables {
        let unknown: Vec<&String> = list.iter().filter(|t| schema.table(t).is_none()).collect();
        if !unknown.is_empty() {
            return Err(Error::Invalid(format!("not tables of schema {sequence}: {unknown:?}")));
        }
    }
    let archives = archives_of(&layout, &opts.archives)?;
    let archive_names: Vec<String> = archives
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let wanted = |table: &str| match &opts.tables {
        Some(list) => list.iter().any(|t| t == table),
        None => true,
    };
    let expected = match &opts.tables {
        Some(list) => list.len(),
        None => DUMP_FILES
            .iter()
            .filter(|d| {
                archive_names.iter().any(|a| a == d.file) || (archives.is_empty() && d.file == "mbdump.tar.bz2")
            })
            .map(|d| d.tables.len())
            .sum(),
    };
    let progress = Arc::new(Progress {
        started,
        callback: opts.on_progress.clone(),
        tables_loaded: AtomicU64::new(0),
        tables_expected: expected as u32,
        rows: AtomicU64::new(0),
        extract: Mutex::new(ExtractProgress::default()),
    });
    progress.report("schema", None);

    let building = building_path(&opts.database);
    remove_database(&building);
    if let Some(p) = building.parent() {
        fs::create_dir_all(p)?;
    }
    let conn = connect(&building, opts.threads, opts.memory_limit.as_deref(), false)?;
    conn.execute_batch(ddl::MACROS)?;

    // The loader thread: loads jobs in order, then indexes.
    let (tx, rx) = crossbeam_channel::bounded::<LoadJob>(1);
    let failed = Arc::new(AtomicBool::new(false));
    let loader = {
        let progress = progress.clone();
        let failed = failed.clone();
        let stop = opts.stop.clone();
        let not_null = opts.constraints;
        thread::Builder::new().name("musicbrainz-loader".into()).spawn(
            move || -> Result<(Connection, Vec<TableStats>)> {
                let mut loaded = Vec::new();
                for job in rx {
                    if stopped(&stop) {
                        return Err(Error::Stopped);
                    }
                    progress.report("load", Some(&job.table.name));
                    match load(&conn, &job, sequence, not_null) {
                        Ok(stats) => {
                            progress.rows.fetch_add(stats.rows, Ordering::Relaxed);
                            progress.tables_loaded.fetch_add(1, Ordering::Relaxed);
                            progress.report("load", Some(&job.table.name));
                            loaded.push(stats);
                        }
                        Err(e) => {
                            failed.store(true, Ordering::Relaxed);
                            if job.remove_after {
                                let _ = fs::remove_file(&job.file);
                            }
                            return Err(e);
                        }
                    }
                }
                Ok((conn, loaded))
            },
        )?
    };

    let work = opts.work_dir.clone().unwrap_or_else(|| {
        let mut s = opts.database.as_os_str().to_os_string();
        s.push(".work");
        PathBuf::from(s)
    });
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut queued: Vec<String> = Vec::new();
    let feed = (|| -> Result<()> {
        // 1. Tables taken out already.
        if let Some(dir) = &layout.tables {
            let mut names: Vec<String> = fs::read_dir(dir.join("mbdump"))?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| !n.ends_with(".partial"))
                .collect();
            names.sort();
            for name in names {
                if !wanted(&name) {
                    continue;
                }
                let Some(t) = schema.table(&name) else {
                    skipped.push((name, format!("not a table of schema {sequence}")));
                    continue;
                };
                if tx
                    .send(LoadJob {
                        table: t.clone(),
                        file: dir.join("mbdump").join(&name),
                        remove_after: false,
                        from: "tables",
                    })
                    .is_err()
                {
                    return Ok(()); // The loader stopped: its error is reported.
                }
                queued.push(name);
            }
        }
        // 2. The rest, from the archives.
        let missing = |t: &str| wanted(t) && !queued.iter().any(|q| q == t);
        let any_missing = match &opts.tables {
            Some(list) => list.iter().any(|t| missing(t)),
            None => true,
        };
        if !any_missing || archives.is_empty() {
            return Ok(());
        }
        for archive in &archives {
            if stopped(&opts.stop) || failed.load(Ordering::Relaxed) {
                break;
            }
            let a = Archive::open(archive)?;
            let base = progress.extract.lock().unwrap().clone();
            let mut on_progress = |p: &ExtractProgress| {
                let mut e = progress.extract.lock().unwrap();
                e.bytes_in = base.bytes_in + p.bytes_in;
                e.bytes_out = base.bytes_out + p.bytes_out;
                e.bytes_total = base.bytes_total + p.bytes_total;
                e.member = p.member.clone();
                drop(e);
                progress.report("extract", p.member.as_deref().and_then(|m| m.strip_prefix("mbdump/")));
            };
            let mut newly: Vec<String> = Vec::new();
            let mut on_member = |m: &Member, path: &Path| -> musicbrainz_dump::Result<Flow> {
                let Some(name) = &m.table else {
                    return Ok(Flow::Continue);
                };
                let Some(t) = schema.table(name) else {
                    skipped.push((name.clone(), format!("not a table of schema {sequence}")));
                    let _ = fs::remove_file(path);
                    return Ok(Flow::Continue);
                };
                let job = LoadJob {
                    table: t.clone(),
                    file: path.to_path_buf(),
                    remove_after: !opts.keep_tables,
                    from: "archive",
                };
                if tx.send(job).is_err() {
                    return Ok(Flow::Stop);
                }
                newly.push(name.clone());
                // Asked for a list, and all of it is there: stop reading.
                if let Some(list) = &opts.tables {
                    if list.iter().all(|t| queued.contains(t) || newly.contains(t)) {
                        return Ok(Flow::Stop);
                    }
                }
                Ok(Flow::Continue)
            };
            let should_stop = || stopped(&opts.stop) || failed.load(Ordering::Relaxed);
            let filter = |m: &Member| {
                m.table
                    .as_deref()
                    .is_none_or(|t| wanted(t) && !queued.iter().any(|q| q == t))
            };
            let mut x = ExtractOptions::new(&work);
            x.threads = opts.threads;
            x.filter = Some(&filter);
            x.on_progress = Some(&mut on_progress);
            x.on_member = Some(&mut on_member);
            x.should_stop = Some(&should_stop);
            x.skip_existing = true;
            let stats = a.extract(x)?;
            queued.extend(newly);
            if stats.stopped && (stopped(&opts.stop) || failed.load(Ordering::Relaxed)) {
                break;
            }
            let mut e = progress.extract.lock().unwrap();
            e.bytes_total = base.bytes_total + a.path().metadata().map(|m| m.len()).unwrap_or(0);
        }
        Ok(())
    })();
    drop(tx);
    let loaded = loader
        .join()
        .map_err(|_| Error::Invalid("the loader thread panicked".into()))?;
    if !opts.keep_tables {
        // Loaded tables were removed; only metadata files are left.
        let _ = fs::remove_dir_all(&work);
    }
    let cleanup = || remove_database(&building);
    let (conn, tables) = match (feed, loaded) {
        (_, Err(e)) | (Err(e), _) => {
            cleanup();
            return Err(e);
        }
        (Ok(()), Ok(x)) => x,
    };
    if stopped(&opts.stop) {
        drop(conn);
        cleanup();
        return Err(Error::Stopped);
    }
    if let Some(list) = &opts.tables {
        let absent: Vec<&String> = list.iter().filter(|t| !tables.iter().any(|s| &s.name == *t)).collect();
        if !absent.is_empty() {
            drop(conn);
            cleanup();
            return Err(Error::Invalid(format!("not in the export: {absent:?}")));
        }
    }

    // MetaBrainz leaves empty tables out of the archives: the tables listed
    // for the archives read (and the core's, for a folder of tables) and not
    // in them are made, empty, so queries and views over them work.
    let mut tables = tables;
    let made = (|| -> Result<()> {
        if opts.tables.is_none() {
            // The core always (a folder of tables is the core's), and the
            // other archives read.
            let lists = DUMP_FILES
                .iter()
                .filter(|d| d.file == "mbdump.tar.bz2" || archive_names.iter().any(|a| a == d.file));
            for d in lists {
                for name in d.tables {
                    if tables.iter().any(|t| t.name == *name) {
                        continue;
                    }
                    let Some(t) = schema.table(name) else {
                        continue;
                    };
                    if let Some(s) = ddl::table_schema(&t.name) {
                        conn.execute_batch(&format!("CREATE SCHEMA IF NOT EXISTS {}", ddl::ident(s)))?;
                    }
                    conn.execute_batch(&ddl::create_table(t, opts.constraints))?;
                    tables.push(TableStats {
                        name: t.name.clone(),
                        rows: 0,
                        bytes: 0,
                        load_ms: 0,
                        from: "empty",
                    });
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = made {
        drop(conn);
        cleanup();
        return Err(e);
    }

    let result = (|| -> Result<Option<DiffStats>> {
        // Indexes.
        for t in &tables {
            if stopped(&opts.stop) {
                return Err(Error::Stopped);
            }
            let sql = ddl::indexes(schema.table(&t.name).expect("a loaded table"), opts.indexes);
            if !sql.is_empty() {
                progress.report("index", Some(&t.name));
                conn.execute_batch(&sql.join(";\n"))?;
            }
        }
        progress.report("views", None);
        let names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
        for v in crate::views::views(&schema, &names) {
            conn.execute_batch(&v)?;
        }
        write_meta(&conn, &info, &origin, &opts, &tables)?;
        let diff = if diff_previous && opts.database.exists() {
            progress.report("diff", None);
            Some(diff_into(&conn, &opts.database, &schema)?)
        } else {
            None
        };
        conn.execute_batch("CHECKPOINT")?;
        Ok(diff)
    })();
    drop(conn);
    let diff = match result {
        Ok(d) => d,
        Err(e) => {
            cleanup();
            return Err(e);
        }
    };
    if !diff_previous {
        if opts.replace {
            remove_database(&opts.database);
        }
        fs::rename(&building, &opts.database)?;
    }
    progress.report("done", None);
    Ok(BuildStats {
        database: if diff_previous { building } else { opts.database.clone() },
        export: (&info).into(),
        schema: match &origin {
            SchemaOrigin::Bundled => "bundled".into(),
            SchemaOrigin::Files(p) => p.display().to_string(),
        },
        tables,
        skipped,
        diff,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Brings a database built by an earlier release up to this one's views
/// and macros (its tables are as they were loaded). Opens it for writing.
pub fn upgrade(database: &Path) -> Result<Vec<String>> {
    let conn = connect(database, 0, None, false)?;
    conn.execute_batch(ddl::MACROS)?;
    let mut stmt = conn.prepare("SELECT name FROM mb_tables ORDER BY name")?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);
    let meta = |k: &str| -> Option<String> {
        conn.query_row("SELECT value FROM mb_meta WHERE key = ?", [k], |r| r.get(0))
            .ok()
    };
    let sequence = meta("schema_sequence").and_then(|s| s.parse().ok());
    let (schema, _) = resolve_schema(sequence, None, None, true)?;
    let views = crate::views::views(&schema, &names);
    for v in &views {
        conn.execute_batch(v)?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO mb_meta VALUES ('format', ?)",
        [DATABASE_FORMAT.to_string()],
    )?;
    conn.execute_batch("CHECKPOINT")?;
    Ok(views)
}

/// The tables of a schema that are in the CC0 core archive.
pub fn core_tables(schema: &Schema) -> Vec<&str> {
    let core = &DUMP_FILES[0];
    schema
        .tables()
        .iter()
        .filter(|t| core.tables.contains(&t.name.as_str()))
        .map(|t| t.name.as_str())
        .collect()
}
