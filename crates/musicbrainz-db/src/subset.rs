//! A consistent subset of a database, written back as a dump: some
//! artists, what is theirs, and every row those refer to, followed through
//! the schema's foreign keys until nothing is missing. Written as
//! PostgreSQL COPY text (`<dir>/mbdump/<table>`, with the metadata files),
//! it builds a database like any export: for test fixtures, and for
//! examples small enough to ship.
//!
//! Only the seed artists and the members and groups one "member of band"
//! away are *expanded*: their aliases, identifiers and relationships
//! ([`SubsetOptions::follow`]), the release groups they are credited on,
//! those groups' releases with their media, tracks, recordings, ISRCs, the
//! works recorded and who wrote them, labels, events, places and series.
//! Each step is capped ([`SubsetOptions::caps`]), so a prolific artist does
//! not bring the catalogue in. Every other row taken (a co-credited
//! artist, a label, an area) brings only the rows it refers to, and what
//! makes it whole: a credit's names, a link's attributes, an area's
//! parents and ISO codes. The small tables that name things (types,
//! formats, languages, link types) are taken whole.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use duckdb::Connection;
use musicbrainz_schema::{link_entities, Kind, Schema, Table};

use crate::ddl::{ident, table_ident};
use crate::{Error, Result};

/// Tables of at most this many rows are taken whole (types, formats).
const SMALL_TABLE: u64 = 5_000;

/// How a subset is chosen.
#[derive(Clone, Debug)]
pub struct SubsetOptions {
    /// The seed artists' MBIDs.
    pub artists: Vec<String>,
    /// Relationship tables of the expanded artists to follow (either side).
    pub follow: Vec<String>,
    /// Caps: release groups per artist credit, releases per release group,
    /// relationships per entity and table, events per artist.
    pub caps: Caps,
}

/// The caps of a subset (see [`SubsetOptions`]).
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// Release groups per artist credit.
    pub release_groups: usize,
    /// Releases per release group.
    pub releases: usize,
    /// Relationships per entity and table.
    pub relationships: usize,
    /// Credits per artist.
    pub credits: usize,
}

impl Default for SubsetOptions {
    fn default() -> Self {
        SubsetOptions {
            artists: Vec::new(),
            follow: [
                "l_artist_artist",
                "l_artist_event",
                "l_artist_label",
                "l_artist_url",
                "l_artist_work",
                "l_artist_place",
                "l_artist_series",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            caps: Caps {
                release_groups: 4,
                releases: 2,
                relationships: 25,
                credits: 4,
            },
        }
    }
}

/// The rows of a table taken, by `rowid`.
type Taken = BTreeMap<String, BTreeSet<i64>>;

fn rowids(conn: &Connection, sql: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn list(ids: &BTreeSet<i64>) -> String {
    if ids.is_empty() {
        return "NULL".into();
    }
    ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
}

/// The values of a column in the rows taken.
fn values(conn: &Connection, table: &str, column: &str, taken: &BTreeSet<i64>) -> Result<BTreeSet<i64>> {
    if taken.is_empty() {
        return Ok(BTreeSet::new());
    }
    let sql = format!(
        "SELECT DISTINCT CAST({c} AS BIGINT) FROM {t} WHERE rowid IN ({}) AND {c} IS NOT NULL",
        list(taken),
        c = ident(column),
        t = table_ident(table)
    );
    Ok(rowids(conn, &sql)?.into_iter().collect())
}

/// Rows of `table` whose `column` is one of `keys`, at most `cap` per key.
fn rows_where(conn: &Connection, table: &str, column: &str, keys: &BTreeSet<i64>, cap: usize) -> Result<Vec<i64>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT r FROM (SELECT rowid AS r, row_number() OVER (PARTITION BY {c} ORDER BY rowid) AS n \
         FROM {t} WHERE {c} IN ({})) WHERE n <= {cap}",
        list(keys),
        c = ident(column),
        t = table_ident(table)
    );
    rowids(conn, &sql)
}

fn add(taken: &mut Taken, table: &str, rows: Vec<i64>) -> bool {
    let set = taken.entry(table.to_string()).or_default();
    let before = set.len();
    set.extend(rows);
    set.len() > before
}

/// The ids of a table's rows taken.
fn ids(conn: &Connection, taken: &Taken, table: &str) -> Result<BTreeSet<i64>> {
    match taken.get(table) {
        Some(rows) => values(conn, table, "id", rows),
        None => Ok(BTreeSet::new()),
    }
}

/// Chooses a subset (see the module docs): the rows taken, by table.
pub fn choose(conn: &Connection, schema: &Schema, opts: &SubsetOptions) -> Result<BTreeMap<String, Vec<i64>>> {
    let loaded: BTreeMap<String, u64> = {
        let mut stmt = conn.prepare("SELECT name, rows FROM mb_tables")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let has = |t: &str| loaded.get(t).is_some_and(|n| *n > 0);
    let caps = opts.caps;
    let mut taken: Taken = BTreeMap::new();
    let take = |taken: &mut Taken, table: &str, column: &str, keys: &BTreeSet<i64>, cap: usize| -> Result<()> {
        if has(table) {
            let rows = rows_where(conn, table, column, keys, cap)?;
            add(taken, table, rows);
        }
        Ok(())
    };

    // The seeds, and one "member of band" hop.
    let quoted: Vec<String> = opts.artists.iter().map(|g| crate::ddl::literal(g)).collect();
    if quoted.is_empty() {
        return Err(Error::Invalid("a subset needs seed artists".into()));
    }
    let seeds = rowids(
        conn,
        &format!("SELECT id FROM artist WHERE gid IN ({})", quoted.join(",")),
    )?;
    let mut artists: BTreeSet<i64> = seeds.iter().copied().collect();
    if has("l_artist_artist") && has("link") {
        let s = list(&artists);
        let hop = rowids(
            conn,
            &format!(
                "SELECT CASE WHEN entity0 IN ({s}) THEN entity1 ELSE entity0 END FROM mb_relationship \
                 WHERE link_table = 'l_artist_artist' AND link_type = 'member of band' \
                 AND (entity0 IN ({s}) OR entity1 IN ({s}))"
            ),
        )?;
        artists.extend(hop);
    }
    take(&mut taken, "artist", "id", &artists, 1)?;

    // What is the expanded artists' own.
    for (t, c) in [
        ("artist_alias", "artist"),
        ("artist_ipi", "artist"),
        ("artist_isni", "artist"),
        ("artist_gid_redirect", "new_id"),
    ] {
        take(&mut taken, t, c, &artists, caps.relationships)?;
    }
    for t in &opts.follow {
        if let Some((a, b)) = link_entities(t) {
            if a == "artist" {
                take(&mut taken, t, "entity0", &artists, caps.relationships)?;
            }
            if b == "artist" {
                take(&mut taken, t, "entity1", &artists, caps.relationships)?;
            }
        }
    }

    // Their release groups, releases, media, tracks and recordings.
    take(&mut taken, "artist_credit_name", "artist", &artists, caps.credits)?;
    let credits = match taken.get("artist_credit_name") {
        Some(r) => values(conn, "artist_credit_name", "artist_credit", r)?,
        None => BTreeSet::new(),
    };
    take(
        &mut taken,
        "release_group",
        "artist_credit",
        &credits,
        caps.release_groups,
    )?;
    let groups = ids(conn, &taken, "release_group")?;
    take(
        &mut taken,
        "release_group_secondary_type_join",
        "release_group",
        &groups,
        10,
    )?;
    take(
        &mut taken,
        "l_release_group_url",
        "entity0",
        &groups,
        caps.relationships,
    )?;
    take(&mut taken, "release", "release_group", &groups, caps.releases)?;
    let releases = ids(conn, &taken, "release")?;
    for t in ["release_country", "release_unknown_country", "release_label", "medium"] {
        take(&mut taken, t, "release", &releases, 50)?;
    }
    take(&mut taken, "l_release_url", "entity0", &releases, caps.relationships)?;
    let media = ids(conn, &taken, "medium")?;
    take(&mut taken, "track", "medium", &media, 1_000)?;
    let recordings = match taken.get("track") {
        Some(r) => values(conn, "track", "recording", r)?,
        None => BTreeSet::new(),
    };
    take(&mut taken, "recording", "id", &recordings, 1)?;
    take(&mut taken, "isrc", "recording", &recordings, 10)?;
    take(&mut taken, "l_recording_work", "entity0", &recordings, 5)?;
    take(&mut taken, "l_recording_recording", "entity0", &recordings, 5)?;

    // The works recorded, and who wrote them.
    let works = match taken.get("l_recording_work") {
        Some(r) => values(conn, "l_recording_work", "entity1", r)?,
        None => BTreeSet::new(),
    };
    take(&mut taken, "work", "id", &works, 1)?;
    for t in ["iswc", "work_language", "work_attribute"] {
        take(&mut taken, t, "work", &works, 10)?;
    }
    take(&mut taken, "l_artist_work", "entity1", &works, 10)?;

    // Labels, events, places and series, with their links.
    let labels = match taken.get("release_label") {
        Some(r) => values(conn, "release_label", "label", r)?,
        None => BTreeSet::new(),
    };
    take(&mut taken, "label", "id", &labels, 1)?;
    take(&mut taken, "l_label_url", "entity0", &labels, caps.relationships)?;
    take(&mut taken, "l_label_label", "entity1", &labels, 5)?;
    let events = match taken.get("l_artist_event") {
        Some(r) => values(conn, "l_artist_event", "entity1", r)?,
        None => BTreeSet::new(),
    };
    for t in ["l_event_place", "l_event_series", "l_event_url"] {
        take(&mut taken, t, "entity0", &events, caps.relationships)?;
    }
    take(&mut taken, "l_area_event", "entity1", &events, 5)?;
    let mut places = match taken.get("l_event_place") {
        Some(r) => values(conn, "l_event_place", "entity1", r)?,
        None => BTreeSet::new(),
    };
    if let Some(r) = taken.get("l_artist_place") {
        places.extend(values(conn, "l_artist_place", "entity1", r)?);
    }
    take(&mut taken, "l_place_url", "entity0", &places, caps.relationships)?;
    let series = match taken.get("l_event_series") {
        Some(r) => values(conn, "l_event_series", "entity1", r)?,
        None => BTreeSet::new(),
    };
    take(&mut taken, "l_series_url", "entity0", &series, caps.relationships)?;

    // The small tables whole.
    for (t, rows) in &loaded {
        let owned = t.starts_with("l_")
            || musicbrainz_schema::ENTITY_TYPES.contains(&t.as_str())
            || ["_alias", "_gid_redirect", "_attribute", "_ipi", "_isni"]
                .iter()
                .any(|x| t.ends_with(x))
            || t.starts_with("iso_3166");
        if *rows > 0 && *rows <= SMALL_TABLE && schema.table(t).is_some() && !owned {
            add(
                &mut taken,
                t,
                rowids(conn, &format!("SELECT rowid FROM {}", table_ident(t)))?,
            );
        }
    }

    // Everything referred to, and what makes it whole.
    loop {
        let mut grew = close(conn, schema, &mut taken, &has)?;
        let credits = match taken.get("artist_credit") {
            Some(r) => values(conn, "artist_credit", "id", r)?,
            None => BTreeSet::new(),
        };
        let before = taken.get("artist_credit_name").map_or(0, |s| s.len());
        take(&mut taken, "artist_credit_name", "artist_credit", &credits, 100)?;
        grew |= taken.get("artist_credit_name").map_or(0, |s| s.len()) > before;
        let links = {
            let mut l = BTreeSet::new();
            for (t, rows) in &taken {
                if link_entities(t).is_some() {
                    l.extend(values(conn, t, "link", rows)?);
                }
            }
            l
        };
        let areas = ids(conn, &taken, "area")?;
        let size = |t: &Taken| t.values().map(|s| s.len()).sum::<usize>();
        let before = size(&taken);
        for t in ["link_attribute", "link_attribute_credit", "link_attribute_text_value"] {
            take(&mut taken, t, "link", &links, 20)?;
        }
        for t in ["iso_3166_1", "iso_3166_2", "iso_3166_3"] {
            take(&mut taken, t, "area", &areas, 5)?;
        }
        // An area's parents ("part of"), up to its country.
        take(&mut taken, "l_area_area", "entity1", &areas, 3)?;
        grew |= size(&taken) > before;
        if !grew {
            break;
        }
    }
    Ok(taken.into_iter().map(|(t, s)| (t, s.into_iter().collect())).collect())
}

/// Adds every row a row taken refers to, until nothing is missing.
fn close(conn: &Connection, schema: &Schema, taken: &mut Taken, has: &dyn Fn(&str) -> bool) -> Result<bool> {
    let mut grew_any = false;
    // Only the rows added since a table was last looked at are followed.
    let mut seen: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    loop {
        let mut grew = false;
        let tables: Vec<String> = taken.keys().cloned().collect();
        for t in tables {
            let Some(table) = schema.table(&t) else {
                continue;
            };
            let all = taken.get(&t).cloned().unwrap_or_default();
            let done = seen.entry(t.clone()).or_default();
            let new: BTreeSet<i64> = all.difference(done).copied().collect();
            done.extend(new.iter().copied());
            if new.is_empty() {
                continue;
            }
            for fk in &table.foreign_keys {
                if !has(&fk.table) || fk.references != "id" {
                    continue;
                }
                let keys = values(conn, &t, &fk.column, &new)?;
                if keys.is_empty() {
                    continue;
                }
                let sql = format!(
                    "SELECT rowid FROM {} WHERE id IN ({})",
                    table_ident(&fk.table),
                    list(&keys)
                );
                grew |= add(taken, &fk.table, rowids(conn, &sql)?);
            }
        }
        grew_any |= grew;
        if !grew {
            return Ok(grew_any);
        }
    }
}

/// The SQL that writes a table's rows back as COPY text lines.
pub fn copy_text_select(t: &Table) -> String {
    let fields: Vec<String> = t
        .columns
        .iter()
        .map(|c| {
            let v = ident(&c.name);
            let text = match c.kind {
                Kind::Boolean => format!("CASE WHEN {v} THEN 't' ELSE 'f' END"),
                Kind::Point => format!("'(' || {v}[1] || ',' || {v}[2] || ')'"),
                Kind::IntegerArray => format!("'{{' || array_to_string({v}, ',') || '}}'"),
                Kind::Timestamp => format!("strftime({v}, '%Y-%m-%d %H:%M:%S.%f+00')"),
                _ => format!("CAST({v} AS VARCHAR)"),
            };
            format!(
                "CASE WHEN {v} IS NULL THEN '\\N' ELSE replace(replace(replace(replace({text}, '\\', '\\\\'), \
                 chr(9), '\\t'), chr(10), '\\n'), chr(13), '\\r') END"
            )
        })
        .collect();
    format!("SELECT {} FROM {}", fields.join(" || chr(9) || "), table_ident(&t.name))
}

/// Writes the rows chosen as an export's tables folder: `<dir>/mbdump/<table>`
/// in the dump's order, and the metadata files of the database's export.
pub fn write(conn: &Connection, schema: &Schema, rows: &BTreeMap<String, Vec<i64>>, dir: &Path) -> Result<u64> {
    fs::create_dir_all(dir.join("mbdump"))?;
    conn.execute_batch("SET TimeZone = 'UTC'")?;
    let meta = |k: &str| -> Option<String> {
        conn.query_row("SELECT value FROM mb_meta WHERE key = ?", [k], |r| r.get(0))
            .ok()
    };
    fs::write(dir.join("SCHEMA_SEQUENCE"), format!("{}\n", schema.sequence))?;
    fs::write(
        dir.join("TIMESTAMP"),
        format!("{}\n", meta("timestamp").unwrap_or_default()),
    )?;
    fs::write(
        dir.join("REPLICATION_SEQUENCE"),
        format!("{}\n", meta("replication_sequence").unwrap_or_default()),
    )?;
    let mut total = 0;
    for (t, ids) in rows {
        let Some(table) = schema.table(t) else {
            continue;
        };
        if ids.is_empty() {
            continue;
        }
        let sql = format!(
            "{} WHERE rowid IN ({}) ORDER BY rowid",
            copy_text_select(table),
            list(&ids.iter().copied().collect())
        );
        let mut stmt = conn.prepare(&sql)?;
        let lines = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = String::new();
        for l in lines {
            out.push_str(&l?);
            out.push('\n');
            total += 1;
        }
        fs::write(dir.join("mbdump").join(t), out)?;
    }
    Ok(total)
}
