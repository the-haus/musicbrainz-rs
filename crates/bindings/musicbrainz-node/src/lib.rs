#![deny(clippy::all)]

//! Node.js bindings: build, refresh and query MusicBrainz DuckDB databases,
//! read and download full exports.
//!
//! Long tasks (builds, downloads, extractions, streamed queries) run on
//! threads of their own and send events to JS through a threadsafe
//! function; a streamed query's batches go under credit-based flow control
//! ([`credits`]), so a slow consumer bounds memory instead of filling it.
//! Rows cross as JSON text made by DuckDB, one string a batch.

mod credits;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use napi::bindgen_prelude::AsyncTask;
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, JsFunction, JsObject, JsUnknown, Result, Task};
use napi_derive::napi;
use serde::Deserialize;

use musicbrainz_db::{BuildOptions, BuildProgress, Database, IndexLevel, OpenOptions, Session};
use musicbrainz_dump::download::{self, DownloadOptions, HttpOptions, FULL_EXPORTS};
use musicbrainz_dump::{Archive, ExportLayout, ExtractOptions};
use musicbrainz_schema::Schema;

use crate::credits::Credits;

// Not on macOS: mimalloc keeps each thread's heap in fixed TLS slots of the
// process, so two addons that each bundle it crash when loaded together.
#[cfg(all(feature = "mimalloc", not(target_vendor = "apple")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn err(e: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

fn parse_params(json: Option<String>) -> Result<Vec<serde_json::Value>> {
    match json {
        None => Ok(Vec::new()),
        Some(s) if s.is_empty() => Ok(Vec::new()),
        Some(s) => match serde_json::from_str(&s).map_err(err)? {
            serde_json::Value::Array(a) => Ok(a),
            other => Ok(vec![other]),
        },
    }
}

// ── Events and streams ──────────────────────────────────────────────────

/// What a task sends to JS: `{type, data}`, `data` JSON text.
pub(crate) struct Event {
    kind: &'static str,
    data: String,
}

impl Event {
    fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        o.set("type", self.kind)?;
        o.set("data", env.create_string_from_std(self.data)?)?;
        Ok(o)
    }
}

type Callback = ThreadsafeFunction<Event, ErrorStrategy::Fatal>;

fn callback(f: &JsFunction) -> Result<Callback> {
    f.create_threadsafe_function(0, |ctx: ThreadSafeCallContext<Event>| {
        let v: JsUnknown = ctx.value.into_js(&ctx.env)?.into_unknown();
        Ok(vec![v])
    })
}

/// The handle JS holds for a running task.
#[napi]
pub struct NativeTask {
    credits: Arc<Credits>,
    stop: Arc<AtomicBool>,
    callback: Mutex<Option<Callback>>,
}

impl Drop for NativeTask {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.credits.close();
    }
}

#[napi]
impl NativeTask {
    /// Whether the task keeps the event loop alive.
    #[napi]
    pub fn set_ref(&self, env: Env, referenced: bool) -> Result<()> {
        if let Some(cb) = self.callback.lock().unwrap().as_mut() {
            if referenced {
                cb.refer(&env)?;
            } else {
                cb.unref(&env)?;
            }
        }
        Ok(())
    }

    /// Drop this handle's reference to the callback (after the end).
    #[napi]
    pub fn release(&self) {
        self.callback.lock().unwrap().take();
    }

    /// Allow `n` more batches.
    #[napi]
    pub fn request(&self, n: u32) {
        self.credits.grant(n);
    }

    /// Stop the task; `end` (or an error) still arrives.
    #[napi]
    pub fn close(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.credits.close();
    }
}

/// Runs `work` on a thread of its own, with a sender of events; its result
/// is the `end` event (JSON), its error the `error` event.
fn spawn_task(
    f: JsFunction,
    name: &str,
    work: impl FnOnce(&Sender) -> std::result::Result<serde_json::Value, String> + Send + 'static,
) -> Result<NativeTask> {
    let cb = callback(&f)?;
    let credits = Arc::new(Credits::default());
    let stop = Arc::new(AtomicBool::new(false));
    let sender = Sender {
        cb: cb.clone(),
        credits: credits.clone(),
        stop: stop.clone(),
    };
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&sender)));
            let event = match outcome {
                Ok(Ok(v)) => Event {
                    kind: "end",
                    data: v.to_string(),
                },
                Ok(Err(e)) => Event {
                    kind: "error",
                    data: serde_json::Value::String(e).to_string(),
                },
                Err(_) => Event {
                    kind: "error",
                    data: "\"the task panicked\"".into(),
                },
            };
            sender.cb.call(event, ThreadsafeFunctionCallMode::Blocking);
        })
        .map_err(err)?;
    Ok(NativeTask {
        credits,
        stop,
        callback: Mutex::new(Some(cb)),
    })
}

/// Sends events of a running task.
pub(crate) struct Sender {
    cb: Callback,
    credits: Arc<Credits>,
    stop: Arc<AtomicBool>,
}

impl Sender {
    /// A progress report: dropped when JS is behind.
    fn progress(&self, data: String) {
        self.cb.call(
            Event { kind: "progress", data },
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    /// A batch, once JS granted a credit; false once closed.
    fn batch(&self, data: String) -> bool {
        if !self.credits.acquire() {
            return false;
        }
        self.cb
            .call(Event { kind: "batch", data }, ThreadsafeFunctionCallMode::Blocking);
        true
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

// ── Databases ───────────────────────────────────────────────────────────

/// Options for `NativeDatabase.open`.
#[napi(object)]
#[derive(Default)]
pub struct NativeOpenOptions {
    /// Open read-only (the default; several processes may read at once).
    pub read_only: Option<bool>,
    /// DuckDB threads (0: every core).
    pub threads: Option<u32>,
    /// DuckDB's memory limit (`"4GB"`).
    pub memory_limit: Option<String>,
}

/// An open database.
#[napi]
pub struct NativeDatabase {
    db: Mutex<Option<Arc<Database>>>,
}

impl NativeDatabase {
    fn database(&self) -> Result<Arc<Database>> {
        self.db
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| err("the database is closed"))
    }

    fn get(&self) -> Result<Target> {
        Ok(Target::Database(self.database()?))
    }
}

/// What a query runs on: the database's pool, or a session's connection.
#[derive(Clone)]
pub(crate) enum Target {
    Database(Arc<Database>),
    Session(Arc<Session>),
}

impl Target {
    fn query_json(&self, sql: &str, params: &[serde_json::Value]) -> musicbrainz_db::Result<String> {
        match self {
            Target::Database(d) => d.query_json(sql, params),
            Target::Session(s) => s.query_json(sql, params),
        }
    }

    fn execute(&self, sql: &str, params: &[serde_json::Value]) -> musicbrainz_db::Result<usize> {
        match self {
            Target::Database(d) => d.execute(sql, params),
            Target::Session(s) => s.execute(sql, params),
        }
    }

    fn execute_batch(&self, sql: &str) -> musicbrainz_db::Result<()> {
        match self {
            Target::Database(d) => d.execute_batch(sql),
            Target::Session(s) => s.execute_batch(sql),
        }
    }

    fn describe(&self, sql: &str, params: &[serde_json::Value]) -> musicbrainz_db::Result<Vec<(String, String)>> {
        match self {
            Target::Database(d) => d.describe(sql, params),
            Target::Session(s) => s.describe(sql, params),
        }
    }

    fn query_batches(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        batch: usize,
        f: &mut dyn FnMut(&[String]) -> bool,
    ) -> musicbrainz_db::Result<u64> {
        match self {
            Target::Database(d) => d.query_batches(sql, params, batch, f),
            Target::Session(s) => s.query_batches(sql, params, batch, f),
        }
    }

    /// Streams a query's rows to JS in batches.
    fn stream(self, sql: String, params: Vec<serde_json::Value>, batch_size: u32, f: JsFunction) -> Result<NativeTask> {
        let batch = batch_size.max(1) as usize;
        spawn_task(f, "musicbrainz-query", move |s| {
            let mut stopped = false;
            let n = self
                .query_batches(&sql, &params, batch, &mut |rows| {
                    if rows.is_empty() {
                        return true;
                    }
                    let mut json = String::with_capacity(rows.iter().map(|r| r.len() + 1).sum::<usize>() + 2);
                    json.push('[');
                    json.push_str(&rows.join(","));
                    json.push(']');
                    if s.stopped() || !s.batch(json) {
                        stopped = true;
                        return false;
                    }
                    true
                })
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "rows": n as f64, "stopped": stopped }))
        })
    }
}

/// A query on the libuv pool.
pub struct QueryTask {
    db: Target,
    sql: String,
    params: Vec<serde_json::Value>,
    execute: bool,
}

impl Task for QueryTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<String> {
        // A panic on the libuv pool would take the process down: an error instead.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if self.execute {
                self.db.execute(&self.sql, &self.params).map(|n| n.to_string())
            } else {
                self.db.query_json(&self.sql, &self.params)
            }
        }))
        .map_err(|_| err("the query panicked"))?
        .map_err(err)
    }

    fn resolve(&mut self, _: Env, out: String) -> Result<String> {
        Ok(out)
    }
}

#[napi]
impl NativeDatabase {
    /// Opens the database at `path`.
    #[napi(factory)]
    pub fn open(path: String, options: Option<NativeOpenOptions>) -> Result<NativeDatabase> {
        let o = options.unwrap_or_default();
        let opts = OpenOptions {
            read_only: o.read_only.unwrap_or(true),
            threads: o.threads.unwrap_or(0) as usize,
            memory_limit: o.memory_limit,
        };
        let db = Database::open(&path, &opts).map_err(err)?;
        Ok(NativeDatabase {
            db: Mutex::new(Some(Arc::new(db))),
        })
    }

    /// A query's rows as JSON text (an array of objects), off the event loop.
    #[napi(ts_return_type = "Promise<string>")]
    pub fn query(&self, sql: String, params: Option<String>) -> Result<AsyncTask<QueryTask>> {
        Ok(AsyncTask::new(QueryTask {
            db: self.get()?,
            sql,
            params: parse_params(params)?,
            execute: false,
        }))
    }

    /// A query's rows as JSON text, on the calling thread.
    #[napi]
    pub fn query_sync(&self, sql: String, params: Option<String>) -> Result<String> {
        self.get()?.query_json(&sql, &parse_params(params)?).map_err(err)
    }

    /// Runs a statement; resolves with the rows it changed (as text).
    #[napi(ts_return_type = "Promise<string>")]
    pub fn execute(&self, sql: String, params: Option<String>) -> Result<AsyncTask<QueryTask>> {
        Ok(AsyncTask::new(QueryTask {
            db: self.get()?,
            sql,
            params: parse_params(params)?,
            execute: true,
        }))
    }

    /// Runs statements that return nothing.
    #[napi]
    pub fn execute_batch(&self, sql: String) -> Result<()> {
        self.get()?.execute_batch(&sql).map_err(err)
    }

    /// A query's columns and types, as JSON `[[name, type], …]`.
    #[napi]
    pub fn describe(&self, sql: String, params: Option<String>) -> Result<String> {
        let cols = self.get()?.describe(&sql, &parse_params(params)?).map_err(err)?;
        serde_json::to_string(&cols).map_err(err)
    }

    /// `mb_meta` and `mb_tables`, as JSON.
    #[napi]
    pub fn info(&self) -> Result<String> {
        let db = self.database()?;
        let meta = db.meta().map_err(err)?;
        let tables = db.tables().map_err(err)?;
        let tables: serde_json::Map<String, serde_json::Value> =
            tables.into_iter().map(|(k, v)| (k, (v as f64).into())).collect();
        Ok(serde_json::json!({ "path": db.path(), "meta": meta, "tables": tables }).to_string())
    }

    /// Streams a query's rows in batches of JSON text.
    #[napi]
    pub fn stream(&self, sql: String, params: Option<String>, batch_size: u32, f: JsFunction) -> Result<NativeTask> {
        self.get()?.stream(sql, parse_params(params)?, batch_size, f)
    }

    /// A session: a connection of its own (temporary tables, settings).
    #[napi]
    pub fn session(&self) -> Result<NativeSession> {
        let session = self.database()?.session().map_err(err)?;
        Ok(NativeSession {
            session: Mutex::new(Some(Arc::new(session))),
        })
    }

    /// Closes it (queries running keep their connection until they end).
    #[napi]
    pub fn close(&self) {
        self.db.lock().unwrap().take();
    }
}

/// A session: one connection of the database's, held.
#[napi]
pub struct NativeSession {
    session: Mutex<Option<Arc<Session>>>,
}

impl NativeSession {
    fn get(&self) -> Result<Target> {
        self.session
            .lock()
            .unwrap()
            .clone()
            .map(Target::Session)
            .ok_or_else(|| err("the session is closed"))
    }
}

#[napi]
impl NativeSession {
    /// A query's rows as JSON text, off the event loop.
    #[napi(ts_return_type = "Promise<string>")]
    pub fn query(&self, sql: String, params: Option<String>) -> Result<AsyncTask<QueryTask>> {
        Ok(AsyncTask::new(QueryTask {
            db: self.get()?,
            sql,
            params: parse_params(params)?,
            execute: false,
        }))
    }

    /// A query's rows as JSON text, on the calling thread.
    #[napi]
    pub fn query_sync(&self, sql: String, params: Option<String>) -> Result<String> {
        self.get()?.query_json(&sql, &parse_params(params)?).map_err(err)
    }

    /// Runs a statement; resolves with the rows it changed (as text).
    #[napi(ts_return_type = "Promise<string>")]
    pub fn execute(&self, sql: String, params: Option<String>) -> Result<AsyncTask<QueryTask>> {
        Ok(AsyncTask::new(QueryTask {
            db: self.get()?,
            sql,
            params: parse_params(params)?,
            execute: true,
        }))
    }

    /// Runs statements that return nothing.
    #[napi]
    pub fn execute_batch(&self, sql: String) -> Result<()> {
        self.get()?.execute_batch(&sql).map_err(err)
    }

    /// A query's columns and types, as JSON.
    #[napi]
    pub fn describe(&self, sql: String, params: Option<String>) -> Result<String> {
        let cols = self.get()?.describe(&sql, &parse_params(params)?).map_err(err)?;
        serde_json::to_string(&cols).map_err(err)
    }

    /// Streams a query's rows in batches of JSON text.
    #[napi]
    pub fn stream(&self, sql: String, params: Option<String>, batch_size: u32, f: JsFunction) -> Result<NativeTask> {
        self.get()?.stream(sql, parse_params(params)?, batch_size, f)
    }

    /// Closes it (its temporary tables go with it).
    #[napi]
    pub fn close(&self) {
        self.session.lock().unwrap().take();
    }
}

// ── Builds ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildJson {
    source: String,
    database: String,
    tables: Option<Vec<String>>,
    #[serde(default)]
    archives: Vec<String>,
    work_dir: Option<String>,
    #[serde(default)]
    keep_tables: bool,
    indexes: Option<String>,
    schema_dir: Option<String>,
    schema_cache: Option<String>,
    #[serde(default)]
    offline: bool,
    threads: Option<usize>,
    memory_limit: Option<String>,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    lenient: bool,
    #[serde(default)]
    keep_previous: bool,
}

fn build_options(json: &str, s: &Sender) -> std::result::Result<(BuildOptions, bool), String> {
    let j: BuildJson = serde_json::from_str(json).map_err(|e| format!("build options: {e}"))?;
    let mut o = BuildOptions::new(j.source, j.database);
    o.tables = j.tables;
    o.archives = j.archives;
    o.work_dir = j.work_dir.map(PathBuf::from);
    o.keep_tables = j.keep_tables;
    if let Some(i) = j.indexes {
        o.indexes = IndexLevel::parse(&i).ok_or_else(|| format!("indexes: {i:?} is not none, keys or foreign"))?;
    }
    o.schema_dir = j.schema_dir.map(PathBuf::from);
    o.schema_cache = j.schema_cache.map(PathBuf::from);
    o.offline = j.offline;
    o.threads = j.threads.unwrap_or(0);
    o.memory_limit = j.memory_limit;
    o.replace = j.replace;
    o.lenient = j.lenient;
    o.stop = Some(s.stop.clone());
    let cb = s.cb.clone();
    o.on_progress = Some(Arc::new(move |p: &BuildProgress| {
        if let Ok(data) = serde_json::to_string(p) {
            cb.call(
                Event { kind: "progress", data },
                ThreadsafeFunctionCallMode::NonBlocking,
            );
        }
    }));
    Ok((o, j.keep_previous))
}

/// Builds a database (`options`: JSON of `BuildOptions`); events:
/// `progress`, then `end` (the stats) or `error`.
#[napi]
pub fn build(options: String, f: JsFunction) -> Result<NativeTask> {
    spawn_task(f, "musicbrainz-build", move |s| {
        let (o, _) = build_options(&options, s)?;
        let stats = musicbrainz_db::build(o).map_err(|e| e.to_string())?;
        serde_json::to_value(stats).map_err(|e| e.to_string())
    })
}

/// Refreshes a database: builds anew, diffs, swaps.
#[napi]
pub fn refresh(options: String, f: JsFunction) -> Result<NativeTask> {
    spawn_task(f, "musicbrainz-refresh", move |s| {
        let (o, keep) = build_options(&options, s)?;
        let stats = musicbrainz_db::refresh(o, keep).map_err(|e| e.to_string())?;
        serde_json::to_value(stats).map_err(|e| e.to_string())
    })
}

/// Diffs two databases, writing `mb_changes` into the newer.
#[napi]
pub fn diff(old: String, new: String, f: JsFunction) -> Result<NativeTask> {
    spawn_task(f, "musicbrainz-diff", move |_| {
        let stats =
            musicbrainz_db::diff_databases(old.as_ref(), new.as_ref(), Schema::bundled()).map_err(|e| e.to_string())?;
        serde_json::to_value(stats).map_err(|e| e.to_string())
    })
}

// ── Exports ─────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExtractJson {
    tables: Option<Vec<String>>,
    #[serde(default = "yes")]
    skip_existing: bool,
    threads: Option<usize>,
}

fn yes() -> bool {
    true
}

/// Takes tables out of an archive into `dir`; events: `progress`, `member`
/// (as each is in place), then `end` or `error`.
#[napi]
pub fn extract(archive: String, dir: String, options: Option<String>, f: JsFunction) -> Result<NativeTask> {
    let o: ExtractJson = match options {
        Some(s) => serde_json::from_str(&s).map_err(err)?,
        None => ExtractJson {
            skip_existing: true,
            ..Default::default()
        },
    };
    spawn_task(f, "musicbrainz-extract", move |s| {
        let a = Archive::open(&archive).map_err(|e| e.to_string())?;
        let mut on_progress = |p: &musicbrainz_dump::ExtractProgress| {
            s.progress(
                serde_json::json!({
                    "bytesIn": p.bytes_in as f64, "bytesTotal": p.bytes_total as f64,
                    "bytesOut": p.bytes_out as f64, "member": p.member, "members": p.members as f64,
                    "elapsedMs": p.elapsed_ms as f64,
                })
                .to_string(),
            )
        };
        let mut on_member = |m: &musicbrainz_dump::Member, path: &std::path::Path| {
            s.cb.call(
                Event {
                    kind: "member",
                    data: serde_json::json!({"name": m.name, "size": m.size as f64, "table": m.table, "path": path})
                        .to_string(),
                },
                ThreadsafeFunctionCallMode::Blocking,
            );
            Ok(musicbrainz_dump::Flow::Continue)
        };
        let should_stop = || s.stopped();
        let mut x = ExtractOptions::new(dir);
        x.tables = o.tables;
        x.skip_existing = o.skip_existing;
        x.threads = o.threads.unwrap_or(0);
        x.on_progress = Some(&mut on_progress);
        x.on_member = Some(&mut on_member);
        x.should_stop = Some(&should_stop);
        let stats = a.extract(x).map_err(|e| e.to_string())?;
        let names = |v: &[musicbrainz_dump::Member]| v.iter().map(|m| m.name.clone()).collect::<Vec<_>>();
        Ok(serde_json::json!({
            "written": names(&stats.written), "kept": names(&stats.kept), "skipped": stats.skipped as f64,
            "stopped": stats.stopped, "bytesOut": stats.bytes_out as f64, "elapsedMs": stats.elapsed_ms as f64,
        }))
    })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DownloadJson {
    dir: String,
    name: Option<String>,
    base: Option<String>,
    files: Option<Vec<String>>,
    user_agent: Option<String>,
    retries: Option<u32>,
}

fn http(user_agent: Option<String>, retries: Option<u32>) -> HttpOptions {
    let mut h = HttpOptions::default();
    if let Some(ua) = user_agent {
        h.user_agent = ua;
    }
    if let Some(r) = retries {
        h.retries = r;
    }
    h
}

/// Downloads an export's archives (the newest without a `name`), checked
/// against `SHA256SUMS`; events: `progress`, then `end` or `error`.
#[napi]
pub fn download(options: String, f: JsFunction) -> Result<NativeTask> {
    let o: DownloadJson = serde_json::from_str(&options).map_err(err)?;
    spawn_task(f, "musicbrainz-download", move |s| {
        let h = http(o.user_agent, o.retries);
        let base = o.base.unwrap_or_else(|| FULL_EXPORTS.to_string());
        let name = match o.name {
            Some(n) if n != "latest" => n,
            _ => download::latest_export(&base, &h).map_err(|e| e.to_string())?,
        };
        let mut on_progress = |p: &download::DownloadProgress| {
            s.progress(
                serde_json::json!({
                    "file": p.file, "bytes": p.bytes as f64, "total": p.total.map(|t| t as f64),
                    "fetched": p.fetched as f64, "elapsedMs": p.elapsed_ms as f64,
                })
                .to_string(),
            )
        };
        let should_stop = || s.stopped();
        let stats = download::download_export(
            o.dir.as_ref(),
            &name,
            DownloadOptions {
                base,
                files: o.files.unwrap_or_else(|| vec!["mbdump.tar.bz2".into()]),
                http: h,
                on_progress: Some(&mut on_progress),
                should_stop: Some(&should_stop),
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "name": name, "dir": stats.dir, "files": stats.files, "fetched": stats.fetched as f64,
            "alreadyDone": stats.already_done as f64, "elapsedMs": stats.elapsed_ms as f64,
        }))
    })
}

/// Blocking work on the libuv pool, resolving with JSON text.
pub struct JsonTask(Option<Box<dyn FnOnce() -> std::result::Result<String, String> + Send>>);

impl Task for JsonTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<String> {
        let f = self.0.take().ok_or_else(|| err("the task ran twice"))?;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .map_err(|_| err("the task panicked"))?
            .map_err(err)
    }

    fn resolve(&mut self, _: Env, out: String) -> Result<String> {
        Ok(out)
    }
}

fn json_task(f: impl FnOnce() -> std::result::Result<String, String> + Send + 'static) -> AsyncTask<JsonTask> {
    AsyncTask::new(JsonTask(Some(Box::new(f))))
}

/// The newest export's name.
#[napi(ts_return_type = "Promise<string>")]
pub fn latest_export(base: Option<String>, user_agent: Option<String>) -> AsyncTask<JsonTask> {
    json_task(move || {
        let name = download::latest_export(base.as_deref().unwrap_or(FULL_EXPORTS), &http(user_agent, None))
            .map_err(|e| e.to_string())?;
        Ok(serde_json::Value::String(name).to_string())
    })
}

/// An export's `SHA256SUMS`, as JSON `{file: sha256}`.
#[napi(ts_return_type = "Promise<string>")]
pub fn export_checksums(name: String, base: Option<String>, user_agent: Option<String>) -> AsyncTask<JsonTask> {
    json_task(move || {
        let sums = download::export_checksums(base.as_deref().unwrap_or(FULL_EXPORTS), &name, &http(user_agent, None))
            .map_err(|e| e.to_string())?;
        let map: serde_json::Map<String, serde_json::Value> = sums.into_iter().map(|(f, h)| (f, h.into())).collect();
        Ok(serde_json::Value::Object(map).to_string())
    })
}

/// An archive's members, as JSON (one pass over all of it).
#[napi(ts_return_type = "Promise<string>")]
pub fn archive_members(archive: String, threads: Option<u32>) -> AsyncTask<JsonTask> {
    json_task(move || {
        let members = Archive::open(&archive)
            .and_then(|a| a.members(threads.unwrap_or(0) as usize))
            .map_err(|e| e.to_string())?;
        let list: Vec<_> = members
            .iter()
            .map(|m| serde_json::json!({"name": m.name, "size": m.size as f64, "table": m.table}))
            .collect();
        Ok(serde_json::Value::Array(list).to_string())
    })
}

/// What an export's files are and what its metadata says, as JSON.
#[napi(ts_return_type = "Promise<string>")]
pub fn export_info(path: String) -> AsyncTask<JsonTask> {
    json_task(move || {
        let layout = ExportLayout::find(path.as_ref()).map_err(|e| e.to_string())?;
        let info = layout.info(0).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "name": info.name, "timestamp": info.timestamp, "schemaSequence": info.schema_sequence,
            "replicationSequence": info.replication_sequence.map(|r| r as f64),
            "archives": layout.archives, "tables": layout.tables, "dir": layout.dir,
        })
        .to_string())
    })
}

/// The bundled schema, as JSON: its sequence, and every table's columns,
/// keys and archive.
#[napi]
pub fn schema() -> String {
    let s = Schema::bundled();
    let tables: Vec<_> = s
        .tables()
        .iter()
        .map(|t| {
            let cols: Vec<_> = t
                .columns
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "name": c.name, "sqlType": c.sql_type, "kind": c.kind.as_str(), "nullable": c.nullable,
                        "duckdbType": musicbrainz_db::ddl::duck_type(c),
                    })
                })
                .collect();
            let fks: Vec<_> = t
                .foreign_keys
                .iter()
                .map(|f| serde_json::json!({"column": f.column, "table": f.table, "references": f.references}))
                .collect();
            serde_json::json!({
                "name": t.name, "schema": t.schema, "columns": cols, "primaryKey": t.primary_key,
                "foreignKeys": fks, "archive": t.dump_file().map(|d| d.file),
                "licence": t.dump_file().map(|d| d.licence),
                "linkEntities": t.link_entities().map(|(a, b)| vec![a, b]),
            })
        })
        .collect();
    serde_json::json!({ "sequence": s.sequence, "tables": tables }).to_string()
}

/// The version of the native addon.
#[napi]
pub fn native_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
