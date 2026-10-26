//! A built database, for queries. Rows come out as JSON text made by DuckDB
//! itself (`to_json` of each row: nested lists and structs, MBIDs and
//! dates as strings), all of a batch in one string, which is what a
//! JavaScript caller parses fastest.
//!
//! Parameters are JSON values: scalars bind as they are; an array or object
//! binds as its JSON text, which SQL casts (`?::INTEGER[]`,
//! `?::UUID[]`, `from_json(?, '["VARCHAR"]')`), since DuckDB cannot bind
//! lists. Each call takes a connection of its own from a pool, so calls
//! from several threads run at once.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use duckdb::types::{Value, ValueRef};
use duckdb::Connection;

use crate::{Error, Result};

/// How to open a database.
#[derive(Clone, Debug)]
pub struct OpenOptions {
    /// Open it read-only (several processes may then read it at once).
    pub read_only: bool,
    /// DuckDB threads (0: every core).
    pub threads: usize,
    /// DuckDB's memory limit (`"4GB"`).
    pub memory_limit: Option<String>,
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions {
            read_only: true,
            threads: 0,
            memory_limit: None,
        }
    }
}

/// An open database (see the module docs).
pub struct Database {
    path: PathBuf,
    base: Mutex<Connection>,
    pool: Mutex<Vec<Connection>>,
}

/// A pooled connection, handed back when dropped.
pub struct Pooled<'a> {
    db: &'a Database,
    conn: Option<Connection>,
}

impl Deref for Pooled<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("a pooled connection")
    }
}

impl DerefMut for Pooled<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("a pooled connection")
    }
}

/// Connections kept for reuse; more, made for a burst of calls at once, are
/// closed when handed back.
const POOL_SIZE: usize = 16;

impl Drop for Pooled<'_> {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            let mut pool = self.db.pool.lock().unwrap_or_else(|e| e.into_inner());
            if pool.len() < POOL_SIZE {
                pool.push(c);
            }
        }
    }
}

/// JSON to a value DuckDB binds.
fn bind_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::BigInt(i),
            None => Value::Double(n.as_f64().unwrap_or(f64::NAN)),
        },
        serde_json::Value::String(s) => Value::Text(s.clone()),
        other => Value::Text(other.to_string()),
    }
}

/// A value of a statement DuckDB could not wrap in `to_json` (`SHOW`,
/// `PRAGMA`, `DESCRIBE`), as JSON.
fn json_of(v: ValueRef<'_>) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        ValueRef::Null => J::Null,
        ValueRef::Boolean(b) => J::Bool(b),
        ValueRef::TinyInt(i) => i.into(),
        ValueRef::SmallInt(i) => i.into(),
        ValueRef::Int(i) => i.into(),
        ValueRef::BigInt(i) => i.into(),
        ValueRef::UTinyInt(i) => i.into(),
        ValueRef::USmallInt(i) => i.into(),
        ValueRef::UInt(i) => i.into(),
        ValueRef::UBigInt(i) => i.into(),
        ValueRef::HugeInt(i) => J::from(i as f64),
        ValueRef::Float(f) => J::from(f as f64),
        ValueRef::Double(f) => J::from(f),
        ValueRef::Text(t) => J::String(String::from_utf8_lossy(t).into_owned()),
        other => J::String(format!("{:?}", other.to_owned())),
    }
}

/// The SQL without trailing semicolons and blanks.
fn trimmed(sql: &str) -> &str {
    sql.trim().trim_end_matches(';').trim_end()
}

impl Database {
    /// Opens the database at `path`.
    pub fn open(path: impl AsRef<Path>, opts: &OpenOptions) -> Result<Database> {
        let path = path.as_ref().to_path_buf();
        if !path.is_file() {
            return Err(Error::Invalid(format!("{}: no such database", path.display())));
        }
        let conn = crate::build::connect(&path, opts.threads, opts.memory_limit.as_deref(), opts.read_only)?;
        Ok(Database {
            path,
            base: Mutex::new(conn),
            pool: Mutex::new(Vec::new()),
        })
    }

    /// Its file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A connection of the pool (a new one when all are in use).
    pub fn connection(&self) -> Result<Pooled<'_>> {
        // A poisoned lock (a panic while holding it) still guards a
        // connection list that is whole: take it back rather than fail.
        let conn = match self.pool.lock().unwrap_or_else(|e| e.into_inner()).pop() {
            Some(c) => c,
            None => self.base.lock().unwrap_or_else(|e| e.into_inner()).try_clone()?,
        };
        Ok(Pooled {
            db: self,
            conn: Some(conn),
        })
    }

    /// Runs statements that return nothing.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        execute_batch_on(&*self.connection()?, sql)
    }

    /// Runs one statement; the rows it changed.
    pub fn execute(&self, sql: &str, params: &[serde_json::Value]) -> Result<usize> {
        execute_on(&*self.connection()?, sql, params)
    }

    /// A query's rows as one JSON array of objects.
    pub fn query_json(&self, sql: &str, params: &[serde_json::Value]) -> Result<String> {
        query_json_on(&*self.connection()?, sql, params)
    }

    /// A query's rows, `batch` at a time (each row a JSON object), to `f`;
    /// `f` returns false to stop. The rows read.
    pub fn query_batches(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        batch: usize,
        f: &mut dyn FnMut(&[String]) -> bool,
    ) -> Result<u64> {
        query_batches_on(&*self.connection()?, sql, params, batch, f)
    }

    /// A query's columns and their DuckDB types (`DESCRIBE`), without
    /// running it.
    pub fn describe(&self, sql: &str, params: &[serde_json::Value]) -> Result<Vec<(String, String)>> {
        describe_on(&*self.connection()?, sql, params)
    }

    /// A session: one connection of its own, for temporary tables and
    /// settings that later statements see (the pool's connections are
    /// shared, so they may not).
    pub fn session(&self) -> Result<Session> {
        Ok(Session {
            conn: Mutex::new(self.base.lock().unwrap().try_clone()?),
        })
    }

    /// The `mb_meta` table: what the database was built from.
    pub fn meta(&self) -> Result<BTreeMap<String, Option<String>>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT key, value FROM mb_meta ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The tables loaded, with their rows (`mb_tables`).
    pub fn tables(&self) -> Result<Vec<(String, u64)>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT name, rows FROM mb_tables ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

/// Runs statements that return nothing (`CREATE TEMP TABLE …; …`).
pub fn execute_batch_on(conn: &Connection, sql: &str) -> Result<()> {
    conn.execute_batch(sql)?;
    Ok(())
}

/// Runs one statement; the rows it changed.
pub fn execute_on(conn: &Connection, sql: &str, params: &[serde_json::Value]) -> Result<usize> {
    let values: Vec<Value> = params.iter().map(bind_value).collect();
    Ok(conn.execute(trimmed(sql), duckdb::params_from_iter(values))?)
}

/// A query's rows as one JSON array of objects.
pub fn query_json_on(conn: &Connection, sql: &str, params: &[serde_json::Value]) -> Result<String> {
    let mut out = String::from("[");
    let mut first = true;
    query_batches_on(conn, sql, params, usize::MAX, &mut |batch: &[String]| {
        for row in batch {
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(row);
        }
        true
    })?;
    out.push(']');
    Ok(out)
}

/// A query's rows, `batch` at a time (each row a JSON object), to `f`;
/// `f` returns false to stop. The rows read.
pub fn query_batches_on(
    conn: &Connection,
    sql: &str,
    params: &[serde_json::Value],
    batch: usize,
    f: &mut dyn FnMut(&[String]) -> bool,
) -> Result<u64> {
    let sql = trimmed(sql);
    let values: Vec<Value> = params.iter().map(bind_value).collect();
    let wrapped = format!("SELECT CAST(to_json(mb_row) AS VARCHAR) FROM ({sql}) AS mb_row");
    match conn.prepare(&wrapped) {
        Ok(mut stmt) => {
            let mut rows = stmt.query(duckdb::params_from_iter(values))?;
            let mut buf = Vec::new();
            let mut n = 0u64;
            while let Some(row) = rows.next()? {
                buf.push(row.get::<_, String>(0)?);
                n += 1;
                if buf.len() >= batch {
                    if !f(&buf) {
                        return Ok(n);
                    }
                    buf.clear();
                }
            }
            if !buf.is_empty() || n == 0 {
                f(&buf);
            }
            Ok(n)
        }
        Err(_) => {
            // Not a query to wrap: run it as it is (its own error, if
            // it is wrong).
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query(duckdb::params_from_iter(values))?;
            let mut buf = Vec::new();
            let mut columns: Vec<String> = Vec::new();
            let mut n = 0u64;
            while let Some(row) = rows.next()? {
                if columns.is_empty() {
                    columns = row.as_ref().column_names();
                }
                let mut obj = serde_json::Map::new();
                for (i, c) in columns.iter().enumerate() {
                    obj.insert(c.clone(), json_of(row.get_ref(i)?));
                }
                buf.push(serde_json::Value::Object(obj).to_string());
                n += 1;
                if buf.len() >= batch {
                    if !f(&buf) {
                        return Ok(n);
                    }
                    buf.clear();
                }
            }
            if !buf.is_empty() || n == 0 {
                f(&buf);
            }
            Ok(n)
        }
    }
}

/// A query's columns and their DuckDB types (`DESCRIBE`), without
/// running it.
pub fn describe_on(conn: &Connection, sql: &str, params: &[serde_json::Value]) -> Result<Vec<(String, String)>> {
    let values: Vec<Value> = params.iter().map(bind_value).collect();
    let mut stmt = conn.prepare(&format!("DESCRIBE {}", trimmed(sql)))?;
    let rows = stmt.query_map(duckdb::params_from_iter(values), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// A session: one connection, held (see [`Database::session`]).
pub struct Session {
    conn: Mutex<Connection>,
}

impl Session {
    /// Runs statements that return nothing.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        execute_batch_on(&self.conn.lock().unwrap(), sql)
    }

    /// Runs one statement; the rows it changed.
    pub fn execute(&self, sql: &str, params: &[serde_json::Value]) -> Result<usize> {
        execute_on(&self.conn.lock().unwrap(), sql, params)
    }

    /// A query's rows as one JSON array of objects.
    pub fn query_json(&self, sql: &str, params: &[serde_json::Value]) -> Result<String> {
        query_json_on(&self.conn.lock().unwrap(), sql, params)
    }

    /// A query's rows in batches (see [`Database::query_batches`]).
    pub fn query_batches(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        batch: usize,
        f: &mut dyn FnMut(&[String]) -> bool,
    ) -> Result<u64> {
        query_batches_on(&self.conn.lock().unwrap(), sql, params, batch, f)
    }

    /// A query's columns and types.
    pub fn describe(&self, sql: &str, params: &[serde_json::Value]) -> Result<Vec<(String, String)>> {
        describe_on(&self.conn.lock().unwrap(), sql, params)
    }
}
