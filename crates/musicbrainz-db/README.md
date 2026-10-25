# musicbrainz-db

MusicBrainz full exports as [DuckDB](https://duckdb.org) databases.

- **Build** one from `mbdump.tar.bz2` in one pass: the archive is decoded on
  every core, each table goes into DuckDB's parallel CSV reader as soon as it
  is out of the archive, and is removed again. Tables taken out already (a
  `mbdump/` folder) are read where they are.
- **Typed from the schema SQL.** Columns, types, `NOT NULL`s, primary keys and
  foreign keys come from musicbrainz-server's own SQL: the bundled schema for
  the dump's `SCHEMA_SEQUENCE`, or that sequence's SQL fetched from GitHub
  once and cached. A dump of a newer schema loads without a new release.
- **Indexed**: ids and MBIDs (`keys`), or every foreign key too (`foreign`).
- **Views** over the relationship tables: `mb_relationship` (every `l_<a>_<b>`
  with its link type, dates and entity types) and `mb_link_attribute`;
  macros `mb_partial_date(y, m, d)` and `mb_wikidata_id(url)`.
- **Refresh** from a newer export: build beside, diff, swap. `mb_changes`
  lists every MBID added, changed (and in which tables), removed or merged.
- **Query** with JSON rows made by DuckDB, in batches, from a pool of
  connections.

```rust,no_run
use musicbrainz_db::{build, BuildOptions, Database, OpenOptions};

let stats = build(BuildOptions::new("dumps/20261017-002244", "musicbrainz.duckdb"))?;
let db = Database::open("musicbrainz.duckdb", &OpenOptions::default())?;
let json = db.query_json(
    "SELECT name FROM artist WHERE gid = ?",
    &[serde_json::json!("056e4f3e-d505-4dad-8ec1-d04f521cbb56")],
)?;
# Ok::<(), musicbrainz_db::Error>(())
```

See the repository's README and `docs/` for the database's layout.
