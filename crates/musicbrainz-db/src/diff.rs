//! What changed between two databases, per entity: which MBIDs were added,
//! removed, merged into another, or changed — in their own row or in the
//! rows that are theirs (aliases, identifiers, attributes, relationships;
//! a release's media, tracks, events and labels; a recording's ISRCs; a
//! work's ISWCs and languages). The result is the table `mb_changes` of the
//! newer database:
//!
//! | column | |
//! |---|---|
//! | `entity` | `artist`, `release`, … |
//! | `id`, `gid` | the entity (for a removed one, its last id and MBID) |
//! | `change` | `added`, `changed`, `removed` or `merged` |
//! | `tables` | the tables whose rows of it changed (`artist`, `artist_alias`, `l_artist_url`, …) |
//! | `merged_into` | for `merged`: the MBID it redirects to now |
//!
//! Rows are compared by DuckDB's hash of the whole row, an entity's rows in a
//! table by the XOR of their hashes and their count, so a pass reads each
//! table of both databases once and holds nothing but the changed keys.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use duckdb::Connection;
use musicbrainz_schema::{link_entities, Schema, ENTITY_TYPES};
use serde::Serialize;

use crate::ddl::{ident, literal};
use crate::Result;

/// Counts of one entity type's changes.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDiff {
    /// MBIDs new in the newer database.
    pub added: u64,
    /// Entities whose rows changed.
    pub changed: u64,
    /// MBIDs gone without a redirect.
    pub removed: u64,
    /// MBIDs merged into another.
    pub merged: u64,
}

/// What a diff found.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStats {
    /// The older database's export (`20261014-001756`).
    pub from_export: Option<String>,
    /// The newer database's export.
    pub to_export: Option<String>,
    /// By entity type, for those both databases have.
    pub entities: BTreeMap<String, EntityDiff>,
    /// Milliseconds it took.
    pub elapsed_ms: u64,
}

/// A table whose rows belong to an entity: by `column`, or through a
/// parent table (`track` belongs to a release through `medium.release`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Dependent {
    table: String,
    column: String,
    via: Option<(String, String)>,
}

/// Tables that refer to an entity but are no part of it.
const NOT_OWNED: &[&str] = &["artist_credit", "artist_credit_name", "artist_credit_gid_redirect"];

/// Rows of an entity that are not named `<entity>_…`.
const EXTRA: &[(&str, &str, &str)] = &[
    ("recording", "isrc", "recording"),
    ("work", "iswc", "work"),
    ("release", "medium", "release"),
    ("area", "iso_3166_1", "area"),
    ("area", "iso_3166_2", "area"),
    ("area", "iso_3166_3", "area"),
    ("area", "country_area", "area"),
];

/// The tables whose rows are an entity's (see the module docs).
fn dependents(schema: &Schema, entity: &str) -> Vec<Dependent> {
    let mut out = Vec::new();
    let prefix = format!("{entity}_");
    for t in schema.tables() {
        if t.name == entity || NOT_OWNED.contains(&t.name.as_str()) || ENTITY_TYPES.contains(&t.name.as_str()) {
            continue;
        }
        if let Some((a, b)) = link_entities(&t.name) {
            if a == entity {
                out.push(Dependent {
                    table: t.name.clone(),
                    column: "entity0".into(),
                    via: None,
                });
            }
            if b == entity {
                out.push(Dependent {
                    table: t.name.clone(),
                    column: "entity1".into(),
                    via: None,
                });
            }
            continue;
        }
        let owned = t.name.starts_with(&prefix) || EXTRA.iter().any(|(e, table, _)| *e == entity && *table == t.name);
        if !owned {
            continue;
        }
        for fk in &t.foreign_keys {
            if fk.table == entity && fk.references == "id" {
                out.push(Dependent {
                    table: t.name.clone(),
                    column: fk.column.clone(),
                    via: None,
                });
            }
        }
    }
    if entity == "release" {
        out.push(Dependent {
            table: "track".into(),
            column: "medium".into(),
            via: Some(("medium".into(), "release".into())),
        });
    }
    out
}

fn tables_in(conn: &Connection, database: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT CASE WHEN schema_name = 'main' THEN table_name ELSE schema_name || '.' || table_name END \
         FROM duckdb_tables() WHERE database_name = ?",
    )?;
    let rows = stmt.query_map([database], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn meta(conn: &Connection, db: &str, key: &str) -> Option<String> {
    conn.query_row(
        &format!("SELECT value FROM {}.mb_meta WHERE key = ?", ident(db)),
        [key],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// The name DuckDB gives the database a connection opened.
fn current_database(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("SELECT current_database()", [], |r| r.get(0))?)
}

/// Diffs the database `conn` is on against the older one at `old` and writes
/// `mb_changes` (see the module docs).
pub(crate) fn diff_into(conn: &Connection, old: &Path, schema: &Schema) -> Result<DiffStats> {
    let started = Instant::now();
    let new = current_database(conn)?;
    conn.execute_batch(&format!(
        "ATTACH {} AS mb_old (READ_ONLY)",
        literal(&old.to_string_lossy())
    ))?;
    let result = diff_attached(conn, &new, schema);
    let _ = conn.execute_batch("DETACH mb_old");
    let mut stats = result?;
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(stats)
}

fn diff_attached(conn: &Connection, new: &str, schema: &Schema) -> Result<DiffStats> {
    let have_new = tables_in(conn, new)?;
    let have_old = tables_in(conn, "mb_old")?;
    let both = |t: &str| have_new.iter().any(|x| x == t) && have_old.iter().any(|x| x == t);
    let n = ident(new);
    let o = "mb_old";
    let mut stats = DiffStats {
        from_export: meta(conn, o, "export_name"),
        to_export: meta(conn, new, "export_name"),
        ..Default::default()
    };
    conn.execute_batch(
        "CREATE OR REPLACE TABLE mb_changes (entity VARCHAR, id INTEGER, gid UUID, change VARCHAR, \
         tables VARCHAR[], merged_into UUID)",
    )?;
    for entity in ENTITY_TYPES {
        if !both(entity) {
            continue;
        }
        let e = ident(entity);
        conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE mb_diff_rows AS
             SELECT coalesce(nn.id, oo.id) AS id, nn.gid AS gid, oo.gid AS old_gid,
                    CASE WHEN oo.id IS NULL THEN 'added' WHEN nn.id IS NULL THEN 'removed' ELSE 'changed' END AS change
             FROM (SELECT id, gid, hash(t) AS h FROM {n}.{e} t) nn
             FULL OUTER JOIN (SELECT id, gid, hash(t) AS h FROM {o}.{e} t) oo ON nn.id = oo.id
             WHERE oo.id IS NULL OR nn.id IS NULL OR nn.h <> oo.h;
             CREATE OR REPLACE TEMP TABLE mb_diff_deps (id INTEGER, tbl VARCHAR);"
        ))?;
        for d in dependents(schema, entity) {
            let tables_there = both(&d.table) && d.via.as_ref().is_none_or(|(p, _)| both(p));
            if !tables_there {
                continue;
            }
            let t = crate::ddl::table_ident(&d.table);
            let c = ident(&d.column);
            let grouped = |db: &str| match &d.via {
                None => format!(
                    "SELECT d.{c} AS k, bit_xor(hash(d)) AS h, count(*) AS n FROM {db}.{t} d \
                     WHERE d.{c} IS NOT NULL GROUP BY 1"
                ),
                Some((parent, pc)) => format!(
                    "SELECT p.{} AS k, bit_xor(hash(d)) AS h, count(*) AS n FROM {db}.{t} d \
                     JOIN {db}.{} p ON p.id = d.{c} WHERE p.{} IS NOT NULL GROUP BY 1",
                    ident(pc),
                    crate::ddl::table_ident(parent),
                    ident(pc)
                ),
            };
            conn.execute_batch(&format!(
                "INSERT INTO mb_diff_deps
                 SELECT coalesce(nn.k, oo.k), {} FROM ({}) nn FULL OUTER JOIN ({}) oo ON nn.k = oo.k
                 WHERE nn.k IS NULL OR oo.k IS NULL OR nn.h <> oo.h OR nn.n <> oo.n",
                literal(&d.table),
                grouped(&n),
                grouped(o)
            ))?;
        }
        let redirect = format!("{entity}_gid_redirect");
        let (merge_join, merge_target, merged) = if have_new.contains(&redirect) {
            (
                format!(
                    "LEFT JOIN {n}.{} red ON red.gid = own.old_gid LEFT JOIN {n}.{e} target ON target.id = red.new_id",
                    ident(&redirect)
                ),
                "target.gid",
                "WHEN own.change = 'removed' AND target.id IS NOT NULL THEN 'merged'",
            )
        } else {
            (String::new(), "CAST(NULL AS UUID)", "")
        };
        conn.execute_batch(&format!(
            "INSERT INTO mb_changes
             WITH deps AS (SELECT id, list(DISTINCT tbl ORDER BY tbl) AS tables FROM mb_diff_deps GROUP BY id),
                  ids AS (SELECT id FROM mb_diff_rows UNION SELECT id FROM deps)
             SELECT {ent}, ids.id, coalesce(own.gid, cur.gid, own.old_gid),
                    CASE {merged} WHEN own.change IS NOT NULL THEN own.change ELSE 'changed' END,
                    CASE WHEN own.change = 'changed' THEN list_prepend({ent}, coalesce(deps.tables, []))
                         WHEN own.change IS NULL THEN deps.tables ELSE coalesce(deps.tables, []) END,
                    {merge_target}
             FROM ids LEFT JOIN mb_diff_rows own ON own.id = ids.id
             LEFT JOIN deps ON deps.id = ids.id
             LEFT JOIN {n}.{e} cur ON cur.id = ids.id
             {merge_join}
             WHERE own.change IS NOT NULL OR cur.id IS NOT NULL",
            ent = literal(entity),
        ))?;
        let mut counts = EntityDiff::default();
        let mut stmt = conn.prepare("SELECT change, count(*) FROM mb_changes WHERE entity = ? GROUP BY 1")?;
        let rows = stmt.query_map([entity], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?;
        for row in rows {
            let (change, n) = row?;
            match change.as_str() {
                "added" => counts.added = n,
                "changed" => counts.changed = n,
                "removed" => counts.removed = n,
                "merged" => counts.merged = n,
                _ => {}
            }
        }
        stats.entities.insert(entity.to_string(), counts);
    }
    conn.execute_batch("DROP TABLE IF EXISTS mb_diff_rows; DROP TABLE IF EXISTS mb_diff_deps;")?;
    conn.execute(
        "INSERT OR REPLACE INTO mb_meta VALUES ('diff_from_export', ?)",
        [stats.from_export.clone()],
    )
    .ok();
    Ok(stats)
}

/// Diffs the database at `new` against the older one at `old`, writing
/// `mb_changes` into `new` (see the module docs).
pub fn diff_databases(old: &Path, new: &Path, schema: &Schema) -> Result<DiffStats> {
    let conn = crate::build::connect(new, 0, None, false)?;
    let stats = diff_into(&conn, old, schema)?;
    conn.execute_batch("CHECKPOINT")?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_belongs_to_what() {
        let s = Schema::bundled();
        let release = dependents(s, "release");
        let names: Vec<&str> = release.iter().map(|d| d.table.as_str()).collect();
        for t in [
            "release_label",
            "release_country",
            "medium",
            "track",
            "l_release_url",
            "release_alias",
        ] {
            assert!(names.contains(&t), "{t} in {names:?}");
        }
        assert!(!names.contains(&"release_group"));
        assert!(!names.contains(&"release_group_alias"));
        let artist = dependents(s, "artist");
        let names: Vec<&str> = artist.iter().map(|d| d.table.as_str()).collect();
        assert!(names.contains(&"artist_alias") && names.contains(&"artist_ipi"));
        assert!(!names.contains(&"artist_credit_name"));
        assert_eq!(names.iter().filter(|n| **n == "l_artist_artist").count(), 2);
        let redirect = artist.iter().find(|d| d.table == "artist_gid_redirect").unwrap();
        assert_eq!(redirect.column, "new_id");
    }
}
