//! `musicbrainz`: download MusicBrainz full exports, build and refresh
//! DuckDB databases from them, and query those.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use clap::{Args, Parser, Subcommand, ValueEnum};
use musicbrainz_db::{BuildOptions, BuildProgress, Database, IndexLevel, OpenOptions};
use musicbrainz_dump::download::{self, DownloadOptions, HttpOptions, FULL_EXPORTS};
use musicbrainz_dump::{Archive, ExportLayout, ExtractOptions};
use musicbrainz_schema::Schema;

#[cfg(all(feature = "mimalloc", not(target_vendor = "apple")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(
    name = "musicbrainz",
    version,
    about = "MusicBrainz full exports: download, build DuckDB databases, refresh them with a diff, query."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The newest full export's name.
    Latest {
        /// Where the exports are (a mirror's).
        #[arg(long, default_value = FULL_EXPORTS)]
        base: String,
    },
    /// Download an export's archives, checked against SHA256SUMS.
    Download {
        /// The export (`20261017-002244`), or `latest`.
        #[arg(default_value = "latest")]
        name: String,
        /// The folder exports are kept in (`<dir>/<name>/`).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// The archives to fetch.
        #[arg(long, value_delimiter = ',', default_value = "mbdump.tar.bz2")]
        files: Vec<String>,
        /// Where the exports are (a mirror's).
        #[arg(long, default_value = FULL_EXPORTS)]
        base: String,
    },
    /// An export's metadata, or a database's.
    Info {
        /// An archive, export folder, tables folder, or `.duckdb` database.
        path: PathBuf,
    },
    /// List an archive's members (decompresses it all).
    Members {
        /// The archive.
        archive: PathBuf,
    },
    /// Take tables out of an archive.
    Extract {
        /// The archive.
        archive: PathBuf,
        /// The folder to write `mbdump/<table>` and the metadata into.
        dir: PathBuf,
        /// Only these tables.
        #[arg(long, value_delimiter = ',')]
        tables: Option<Vec<String>>,
        /// bzip2 threads (0: every core).
        #[arg(long, default_value_t = 0)]
        threads: usize,
    },
    /// Build a database from an export.
    Build(BuildArgs),
    /// Build a database anew from a newer export, diff it against the old
    /// one (`mb_changes`), and put it in its place.
    Refresh {
        #[command(flatten)]
        build: BuildArgs,
        /// Keep the old database as `<database>.previous`.
        #[arg(long)]
        keep_previous: bool,
    },
    /// Bring a database built by an earlier release up to this one's views
    /// and macros.
    Upgrade {
        /// The database.
        database: PathBuf,
    },
    /// Diff two databases, writing `mb_changes` into the newer.
    Diff {
        /// The older database.
        old: PathBuf,
        /// The newer database.
        new: PathBuf,
    },
    /// Run SQL against a database.
    Query {
        /// The database.
        database: PathBuf,
        /// The SQL.
        sql: String,
        /// Parameters, as JSON (`--param 1 --param '"text"' --param '[1,2]'`).
        #[arg(long = "param")]
        params: Vec<String>,
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Write a consistent subset of a database as an export's tables
    /// folder: some entities, their rows, and every row those refer to.
    Subset {
        /// The database.
        database: PathBuf,
        /// The folder to write (`<dir>/mbdump/<table>`).
        dir: PathBuf,
        /// Seeds: artists' MBIDs (`artist:` prefix optional), and release
        /// groups to take whatever the caps (`release-group:<mbid>`).
        #[arg(long = "seed", required = true)]
        seeds: Vec<String>,
    },
    /// The schema: tables, or one table's columns and keys.
    Schema {
        /// A table.
        table: Option<String>,
        /// As JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// Aligned columns.
    Table,
    /// One JSON array.
    Json,
    /// One JSON object a line.
    Jsonl,
    /// Tab-separated, with a header.
    Tsv,
}

#[derive(Args)]
struct BuildArgs {
    /// An archive, an export folder, or a folder of tables taken out.
    source: PathBuf,
    /// The database file.
    database: PathBuf,
    /// Only these tables.
    #[arg(long, value_delimiter = ',')]
    tables: Option<Vec<String>>,
    /// Also the tables of these archives of the export (`mbdump-derived.tar.bz2`).
    #[arg(long, value_delimiter = ',')]
    archives: Vec<String>,
    /// Indexes: none, keys (ids and MBIDs) or foreign (also every foreign key).
    #[arg(long, default_value = "keys")]
    indexes: String,
    /// Where to extract tables (default `<database>.work`).
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Keep the extracted tables.
    #[arg(long)]
    keep_tables: bool,
    /// The schema's SQL (musicbrainz-server's admin/sql) instead of the bundled or fetched one.
    #[arg(long)]
    schema_dir: Option<PathBuf>,
    /// Never fetch a schema.
    #[arg(long)]
    offline: bool,
    /// Threads (0: every core).
    #[arg(long, default_value_t = 0)]
    threads: usize,
    /// DuckDB's memory limit (`8GB`).
    #[arg(long)]
    memory: Option<String>,
    /// Replace an existing database.
    #[arg(long)]
    replace: bool,
    /// For hand-made test dumps: no NOT NULLs, MBID columns as text.
    #[arg(long)]
    lenient: bool,
}

type Fallible<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn mb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn progress_line(p: &BuildProgress) -> String {
    let secs = p.elapsed_ms as f64 / 1000.0;
    let read = if p.bytes_total > 0 {
        format!(" · read {}/{}", mb(p.bytes_in), mb(p.bytes_total))
    } else {
        String::new()
    };
    format!(
        "[{secs:>6.1}s] {:<7} {:>3}/{:<3} tables · {:>11} rows{read} · {}",
        p.phase,
        p.tables_loaded,
        p.tables_expected,
        p.rows,
        p.table.as_deref().unwrap_or("")
    )
}

fn build_options(a: BuildArgs, stop: Arc<AtomicBool>) -> Fallible<BuildOptions> {
    let mut o = BuildOptions::new(a.source, a.database);
    o.tables = a.tables;
    o.archives = a.archives;
    o.indexes = IndexLevel::parse(&a.indexes).ok_or("--indexes is none, keys or foreign")?;
    o.work_dir = a.work_dir;
    o.keep_tables = a.keep_tables;
    o.schema_dir = a.schema_dir;
    o.offline = a.offline;
    o.threads = a.threads;
    o.memory_limit = a.memory;
    o.replace = a.replace;
    o.lenient = a.lenient;
    o.stop = Some(stop);
    let tty = io::stderr().is_terminal();
    let last = std::sync::Mutex::new(Instant::now());
    o.on_progress = Some(Arc::new(move |p: &BuildProgress| {
        let mut l = last.lock().unwrap();
        let force = p.phase != "extract" && p.phase != "load" || p.table.is_none();
        if !force && l.elapsed().as_millis() < if tty { 200 } else { 5000 } {
            return;
        }
        *l = Instant::now();
        if tty {
            eprint!("\r\x1b[2K{}", progress_line(p));
        } else {
            eprintln!("{}", progress_line(p));
        }
    }));
    Ok(o)
}

fn print_rows(rows: &[serde_json::Value], format: Format, out: &mut impl Write) -> Fallible {
    let columns: Vec<String> = match rows.first() {
        Some(serde_json::Value::Object(o)) => o.keys().cloned().collect(),
        _ => Vec::new(),
    };
    let cell = |v: &serde_json::Value| match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match format {
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(rows)?)?,
        Format::Jsonl => {
            for r in rows {
                writeln!(out, "{r}")?;
            }
        }
        Format::Tsv => {
            writeln!(out, "{}", columns.join("\t"))?;
            for r in rows {
                let cells: Vec<String> = columns.iter().map(|c| cell(&r[c]).replace(['\t', '\n'], " ")).collect();
                writeln!(out, "{}", cells.join("\t"))?;
            }
        }
        Format::Table => {
            let table: Vec<Vec<String>> = rows
                .iter()
                .map(|r| columns.iter().map(|c| cell(&r[c]).replace(['\t', '\n'], " ")).collect())
                .collect();
            let widths: Vec<usize> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    table
                        .iter()
                        .map(|r| r[i].chars().count().min(60))
                        .chain([c.chars().count()])
                        .max()
                        .unwrap_or(0)
                })
                .collect();
            let line = |cells: &[String]| {
                cells
                    .iter()
                    .zip(&widths)
                    .map(|(c, w)| {
                        let c: String = c.chars().take(60).collect();
                        format!("{c:<w$}")
                    })
                    .collect::<Vec<_>>()
                    .join("  ")
            };
            writeln!(out, "{}", line(&columns))?;
            writeln!(
                out,
                "{}",
                widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("  ")
            )?;
            for r in &table {
                writeln!(out, "{}", line(r))?;
            }
            writeln!(out, "({} rows)", rows.len())?;
        }
    }
    Ok(())
}

fn run(cli: Cli, stop: Arc<AtomicBool>) -> Fallible {
    let http = HttpOptions::default();
    match cli.command {
        Command::Latest { base } => println!("{}", download::latest_export(&base, &http)?),
        Command::Download { name, dir, files, base } => {
            let name = if name == "latest" {
                download::latest_export(&base, &http)?
            } else {
                name
            };
            let mut on_progress = |p: &download::DownloadProgress| {
                let total = p.total.map(mb).unwrap_or_else(|| "?".into());
                let rate = p.fetched as f64 / 1e6 / (p.elapsed_ms.max(1) as f64 / 1000.0);
                eprint!("\r\x1b[2K{}: {} of {total} ({rate:.1} MB/s)", p.file, mb(p.bytes));
            };
            let should_stop = || stop.load(Ordering::Relaxed);
            let stats = download::download_export(
                &dir,
                &name,
                DownloadOptions {
                    base,
                    files,
                    http,
                    on_progress: Some(&mut on_progress),
                    should_stop: Some(&should_stop),
                },
            )?;
            eprintln!();
            for f in &stats.files {
                println!("{}", f.display());
            }
        }
        Command::Info { path } => {
            if path.extension().is_some_and(|e| e == "duckdb") {
                let db = Database::open(&path, &OpenOptions::default())?;
                for (k, v) in db.meta()? {
                    println!("{k}\t{}", v.unwrap_or_default());
                }
                let tables = db.tables()?;
                println!("tables\t{}", tables.len());
                println!("rows\t{}", tables.iter().map(|t| t.1).sum::<u64>());
            } else {
                let layout = ExportLayout::find(&path)?;
                let info = layout.info(0)?;
                println!("name\t{}", info.name.unwrap_or_default());
                println!("timestamp\t{}", info.timestamp.unwrap_or_default());
                println!(
                    "schema_sequence\t{}",
                    info.schema_sequence.map(|s| s.to_string()).unwrap_or_default()
                );
                println!(
                    "replication_sequence\t{}",
                    info.replication_sequence.map(|s| s.to_string()).unwrap_or_default()
                );
                for a in &layout.archives {
                    println!("archive\t{}", a.display());
                }
                if let Some(t) = &layout.tables {
                    println!("tables\t{}", t.display());
                }
            }
        }
        Command::Members { archive } => {
            for m in Archive::open(&archive)?.members(0)? {
                println!("{}\t{}", m.name, m.size);
            }
        }
        Command::Extract {
            archive,
            dir,
            tables,
            threads,
        } => {
            let a = Archive::open(&archive)?;
            let mut on_progress = |p: &musicbrainz_dump::ExtractProgress| {
                eprint!(
                    "\r\x1b[2Kread {} of {}, wrote {} · {}",
                    mb(p.bytes_in),
                    mb(p.bytes_total),
                    mb(p.bytes_out),
                    p.member.as_deref().unwrap_or("")
                );
            };
            let should_stop = || stop.load(Ordering::Relaxed);
            let mut opts = ExtractOptions::new(dir);
            opts.tables = tables;
            opts.threads = threads;
            opts.on_progress = Some(&mut on_progress);
            opts.should_stop = Some(&should_stop);
            let stats = a.extract(opts)?;
            eprintln!(
                "\n{} written, {} kept, {} skipped in {:.1} s",
                stats.written.len(),
                stats.kept.len(),
                stats.skipped,
                stats.elapsed_ms as f64 / 1000.0
            );
        }
        Command::Build(args) => {
            let stats = musicbrainz_db::build(build_options(args, stop)?)?;
            eprintln!();
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        Command::Refresh { build, keep_previous } => {
            let stats = musicbrainz_db::refresh(build_options(build, stop)?, keep_previous)?;
            eprintln!();
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        Command::Upgrade { database } => {
            let views = musicbrainz_db::upgrade(&database)?;
            eprintln!("{}: {} views and the macros made anew", database.display(), views.len());
        }
        Command::Diff { old, new } => {
            let schema = Schema::bundled();
            let stats = musicbrainz_db::diff_databases(&old, &new, schema)?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        Command::Query {
            database,
            sql,
            params,
            format,
        } => {
            let params: Vec<serde_json::Value> = params
                .iter()
                .map(|p| serde_json::from_str(p).unwrap_or_else(|_| serde_json::Value::String(p.clone())))
                .collect();
            let db = Database::open(&database, &OpenOptions::default())?;
            let stdout = io::stdout();
            let mut out = io::BufWriter::new(stdout.lock());
            if let Format::Jsonl = format {
                db.query_batches(&sql, &params, 10_000, &mut |batch| {
                    batch.iter().all(|r| writeln!(out, "{r}").is_ok())
                })?;
            } else {
                let rows: Vec<serde_json::Value> = serde_json::from_str(&db.query_json(&sql, &params)?)?;
                print_rows(&rows, format, &mut out)?;
            }
            out.flush()?;
        }
        Command::Subset { database, dir, seeds } => {
            let db = Database::open(&database, &OpenOptions::default())?;
            let conn = db.connection()?;
            let mut opts = musicbrainz_db::subset::SubsetOptions::default();
            for s in seeds {
                match s.strip_prefix("release-group:") {
                    Some(gid) => opts.release_groups.push(gid.to_string()),
                    None => opts.artists.push(s.strip_prefix("artist:").unwrap_or(&s).to_string()),
                }
            }
            let schema = Schema::bundled();
            let rows = musicbrainz_db::subset::choose(&conn, schema, &opts)?;
            let n = musicbrainz_db::subset::write(&conn, schema, &rows, &dir)?;
            eprintln!("{n} rows of {} tables in {}", rows.len(), dir.display());
        }
        Command::Schema { table, json } => {
            let schema = Schema::bundled();
            match table {
                None => {
                    for t in schema.tables() {
                        let file = t.dump_file().map(|d| d.file).unwrap_or("-");
                        println!("{}\t{}\t{}", t.name, t.columns.len(), file);
                    }
                }
                Some(name) => {
                    let t = schema.table(&name).ok_or_else(|| format!("no table {name}"))?;
                    if json {
                        let cols: Vec<_> = t
                            .columns
                            .iter()
                            .map(|c| {
                                serde_json::json!({
                                    "name": c.name, "type": c.sql_type, "kind": c.kind.as_str(),
                                    "nullable": c.nullable,
                                    "references": t.foreign_key(&c.name).map(|f| format!("{}.{}", f.table, f.references)),
                                })
                            })
                            .collect();
                        let out = serde_json::json!({"name": t.name, "primaryKey": t.primary_key, "columns": cols});
                        println!("{}", serde_json::to_string_pretty(&out)?);
                    } else {
                        for c in &t.columns {
                            let fk = t
                                .foreign_key(&c.name)
                                .map(|f| format!(" → {}.{}", f.table, f.references))
                                .unwrap_or_default();
                            let pk = if t.primary_key.contains(&c.name) { " (key)" } else { "" };
                            println!(
                                "{:<24} {:<28} {}{pk}{fk}",
                                c.name,
                                c.sql_type,
                                if c.nullable { "null" } else { "not null" }
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stop = Arc::new(AtomicBool::new(false));
    {
        // The first Ctrl-C stops the task cleanly (partial files removed,
        // the previous database left as it was); the second exits at once.
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || {
            if stop.swap(true, Ordering::Relaxed) {
                std::process::exit(130);
            }
            eprintln!("\nstopping…");
        });
    }
    match run(cli, stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\nmusicbrainz: {e}");
            ExitCode::FAILURE
        }
    }
}
