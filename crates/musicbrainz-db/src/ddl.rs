//! The SQL a database is built with, generated from the schema: each
//! table's `CREATE TABLE` with DuckDB types, the `INSERT … SELECT` that
//! reads its dump file with DuckDB's parallel CSV reader (as PostgreSQL
//! COPY text: tabs, `\N`, no quoting) and types and unescapes each field,
//! and its indexes.

use std::path::Path;

use musicbrainz_schema::{Column, Kind, Table};

/// Which indexes a build makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IndexLevel {
    /// None: lookups scan (fine for batch work, which joins anyway).
    None,
    /// Primary keys and MBIDs (`gid`): lookups by id and by MBID.
    #[default]
    Keys,
    /// Also every foreign key column (`artist_alias.artist`,
    /// `l_artist_url.entity0`, …): lookups of an entity's rows.
    Foreign,
}

impl IndexLevel {
    /// `"none"`, `"keys"` or `"foreign"`.
    pub fn parse(s: &str) -> Option<IndexLevel> {
        match s {
            "none" => Some(IndexLevel::None),
            "keys" => Some(IndexLevel::Keys),
            "foreign" | "all" => Some(IndexLevel::Foreign),
            _ => None,
        }
    }

    /// Its name.
    pub fn as_str(self) -> &'static str {
        match self {
            IndexLevel::None => "none",
            IndexLevel::Keys => "keys",
            IndexLevel::Foreign => "foreign",
        }
    }
}

/// `"name"`, with quotes doubled.
pub fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `'text'`, with quotes doubled.
pub fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A table's name in the database: `"artist"`, or `"cover_art_archive"."art_type"`
/// for one of another PostgreSQL schema (a DuckDB schema of that name).
pub fn table_ident(name: &str) -> String {
    match name.split_once('.') {
        Some((schema, table)) => format!("{}.{}", ident(schema), ident(table)),
        None => ident(name),
    }
}

/// The DuckDB schema a table is in, when not `main`.
pub fn table_schema(name: &str) -> Option<&str> {
    name.split_once('.').map(|(s, _)| s)
}

/// A column's DuckDB type.
pub fn duck_type(c: &Column) -> &'static str {
    let sql = c.sql_type.to_ascii_uppercase();
    match c.kind {
        Kind::Integer if sql.starts_with("SMALLINT") || sql.starts_with("INT2") => "SMALLINT",
        Kind::Integer if sql.starts_with("BIGINT") || sql.starts_with("BIGSERIAL") || sql.starts_with("INT8") => {
            "BIGINT"
        }
        Kind::Integer => "INTEGER",
        Kind::Float => "DOUBLE",
        Kind::Boolean => "BOOLEAN",
        Kind::Uuid => "UUID",
        Kind::Timestamp if sql.contains("WITH TIME ZONE") || sql.starts_with("TIMESTAMPTZ") => "TIMESTAMPTZ",
        Kind::Timestamp => "TIMESTAMP",
        Kind::Date => "DATE",
        Kind::Time => "TIME",
        Kind::Interval => "INTERVAL",
        Kind::Point => "DOUBLE[]",
        Kind::Json => "JSON",
        Kind::IntegerArray if sql.starts_with("SMALLINT") => "SMALLINT[]",
        Kind::IntegerArray => "INTEGER[]",
        // Text arrays stay in PostgreSQL's literal form (`{a,"b c"}`).
        Kind::Text | Kind::TextArray | Kind::Other => "VARCHAR",
    }
}

/// The macros a database carries: `mb_unescape` (COPY text escapes undone,
/// used while loading) and helpers for queries.
pub const MACROS: &str = r#"
CREATE OR REPLACE MACRO mb_unescape(s) AS
    CASE WHEN contains(s, '\') THEN
        replace(replace(replace(replace(replace(replace(replace(replace(
            s, '\\', chr(57344)), '\t', chr(9)), '\n', chr(10)), '\r', chr(13)),
            '\b', chr(8)), '\f', chr(12)), '\v', chr(11)), chr(57344), '\')
    ELSE s END;
CREATE OR REPLACE MACRO mb_partial_date(y, m, d) AS
    CASE WHEN y IS NULL THEN NULL ELSE
        lpad(CAST(y AS VARCHAR), 4, '0')
        || COALESCE('-' || lpad(CAST(m AS VARCHAR), 2, '0')
            || COALESCE('-' || lpad(CAST(d AS VARCHAR), 2, '0'), ''), '')
    END;
CREATE OR REPLACE MACRO mb_wikidata_id(url) AS
    regexp_extract(url, '^https?://(?:www\.)?wikidata\.org/(?:wiki|entity)/(Q[0-9]+)$', 1);
"#;

/// `CREATE TABLE` with DuckDB types (`NOT NULL` where the schema says,
/// when `not_null`).
pub fn create_table(t: &Table, not_null: bool) -> String {
    let cols: Vec<String> = t
        .columns
        .iter()
        .map(|c| {
            format!(
                "{} {}{}",
                ident(&c.name),
                duck_type(c),
                if c.nullable || !not_null { "" } else { " NOT NULL" }
            )
        })
        .collect();
    format!("CREATE TABLE {} ({})", table_ident(&t.name), cols.join(", "))
}

/// The expression that types raw field `raw` (a VARCHAR) as its column.
fn typed(c: &Column, raw: &str) -> String {
    let ty = duck_type(c);
    match c.kind {
        Kind::Text | Kind::TextArray | Kind::Other => format!("mb_unescape({raw})"),
        Kind::Json => format!("CAST(mb_unescape({raw}) AS JSON)"),
        Kind::Point => format!(
            "CASE WHEN {raw} IS NULL THEN NULL ELSE [CAST(split_part(trim({raw}, '()'), ',', 1) AS DOUBLE), \
             CAST(split_part(trim({raw}, '()'), ',', 2) AS DOUBLE)] END"
        ),
        Kind::IntegerArray => format!(
            "CASE WHEN {raw} IS NULL THEN NULL WHEN {raw} = '{{}}' THEN CAST([] AS {ty}) \
             ELSE CAST(string_split(trim({raw}, '{{}}'), ',') AS {ty}) END"
        ),
        _ => format!("CAST({raw} AS {ty})"),
    }
}

/// `read_csv` of a dump file, every field a VARCHAR named `c0`, `c1`, …
pub fn read_raw(file: &Path, columns: usize) -> String {
    let cols: Vec<String> = (0..columns).map(|i| format!("'c{i}': 'VARCHAR'")).collect();
    format!(
        "read_csv({}, delim = '\\t', quote = '', escape = '', header = false, nullstr = '\\N', \
         auto_detect = false, max_line_size = 67108864, columns = {{{}}})",
        literal(&file.to_string_lossy()),
        cols.join(", ")
    )
}

/// `INSERT INTO <table> SELECT <typed fields> FROM read_csv(<file>)`.
pub fn load_table(t: &Table, file: &Path) -> String {
    let exprs: Vec<String> = t
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| typed(c, &format!("c{i}")))
        .collect();
    format!(
        "INSERT INTO {} SELECT {} FROM {}",
        table_ident(&t.name),
        exprs.join(", "),
        read_raw(file, t.columns.len())
    )
}

/// A name for an index of a table (unique in its schema).
fn index_name(t: &Table, suffix: &str) -> String {
    let bare = t.name.split_once('.').map_or(t.name.as_str(), |(_, b)| b);
    ident(&format!("{bare}_{suffix}"))
}

/// The indexes of a table at a level.
pub fn indexes(t: &Table, level: IndexLevel) -> Vec<String> {
    if level == IndexLevel::None {
        return Vec::new();
    }
    let on = |cols: &[&str]| cols.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    let mut out = Vec::new();
    let mut covered: Vec<&str> = Vec::new();
    if !t.primary_key.is_empty() && t.primary_key.iter().all(|k| t.column(k).is_some()) {
        let cols: Vec<&str> = t.primary_key.iter().map(String::as_str).collect();
        out.push(format!(
            "CREATE UNIQUE INDEX {} ON {} ({})",
            index_name(t, "pkey"),
            table_ident(&t.name),
            on(&cols)
        ));
        covered.push(cols[0]);
    }
    for c in &t.columns {
        if c.kind == Kind::Uuid && c.name == "gid" && !covered.contains(&"gid") {
            out.push(format!(
                "CREATE INDEX {} ON {} ({})",
                index_name(t, "gid"),
                table_ident(&t.name),
                ident("gid")
            ));
            covered.push("gid");
        }
    }
    if level == IndexLevel::Foreign {
        for fk in &t.foreign_keys {
            if covered.contains(&fk.column.as_str()) || t.column(&fk.column).is_none() {
                continue;
            }
            out.push(format!(
                "CREATE INDEX {} ON {} ({})",
                index_name(t, &fk.column),
                table_ident(&t.name),
                ident(&fk.column)
            ));
            covered.push(&fk.column);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use musicbrainz_schema::Schema;

    #[test]
    fn artist_sql() {
        let s = Schema::bundled();
        let artist = s.table("artist").unwrap();
        let create = create_table(artist, true);
        assert!(!create_table(artist, false).contains("NOT NULL"));
        assert!(create.starts_with("CREATE TABLE \"artist\" (\"id\" INTEGER NOT NULL, \"gid\" UUID NOT NULL"));
        assert!(create.contains("\"begin_date_year\" SMALLINT,"));
        assert!(create.contains("\"last_updated\" TIMESTAMPTZ,"));
        let load = load_table(artist, Path::new("/x/it's/artist"));
        assert!(load.contains("CAST(c0 AS INTEGER), CAST(c1 AS UUID), mb_unescape(c2)"));
        assert!(load.contains("'/x/it''s/artist'"));
        let idx = indexes(artist, IndexLevel::Foreign);
        assert_eq!(idx[0], "CREATE UNIQUE INDEX \"artist_pkey\" ON \"artist\" (\"id\")");
        assert_eq!(idx[1], "CREATE INDEX \"artist_gid\" ON \"artist\" (\"gid\")");
        assert!(idx.iter().any(|i| i.contains("(\"area\")")));
        assert_eq!(indexes(artist, IndexLevel::Keys).len(), 2);
        let caa = s.table("cover_art_archive.cover_art").unwrap();
        assert!(create_table(caa, true).starts_with("CREATE TABLE \"cover_art_archive\".\"cover_art\""));
    }
}
