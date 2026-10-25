# Architecture

musicbrainz-rs turns a MusicBrainz full export into a DuckDB database and
gives Rust, the command line and Node.js the same ways into it.

```text
crates/
  musicbrainz-schema   the schema, parsed from musicbrainz-server's own SQL
  musicbrainz-dump     archives: parallel bzip2, tar, COPY text, downloads
  musicbrainz-db       building, refreshing, diffing and querying databases
  musicbrainz-cli      the `musicbrainz` command
  bindings/musicbrainz-node   @musicbrainz-rs/node (napi-rs)
```

## The schema (`musicbrainz-schema`)

A full export's tables are headerless PostgreSQL `COPY` text: what a field
is depends only on its position. The columns, their order and types, which
may be `NULL`, the primary keys and the foreign keys are all read from
musicbrainz-server's `admin/sql` (`CreateTables.sql`, `CreatePrimaryKeys.sql`,
`CreateFKConstraints.sql`, and those of the `cover_art_archive`,
`event_art_archive`, `documentation`, `statistics` and `wikidocs` schemas),
bundled for schema sequence 31. Nothing is typed in by hand, so the crate is
right exactly as far as the SQL is; the tests check every table every
archive lists, and the column counts were checked against every table of
the 2026-10-17 export.

Which archive holds which table (and so under which licence) comes from
musicbrainz-server's `Constants.pm` (`DUMP_FILES`).

A dump of another sequence is read with that sequence's SQL: given as a
folder, or fetched by `musicbrainz-db` from the musicbrainz-server branch or
schema-change tag whose `DB_SCHEMA_SEQUENCE` matches, and cached.

## Reading archives (`musicbrainz-dump`)

**bzip2** is decoded by the block decoder of wikidata-rs (vendored:
`bz2.rs`, `bzdecode.rs`, `parallel.rs`). A scanner finds the bit-aligned
block magics, workers decode blocks on every core, and the blocks come back
in order with every CRC checked. On the 2026-10-17 core archive (7.6 GB,
25.6 GB of tables) one pass takes 49 s on a 10-core M1 Max reading from a
USB disk: 525 MB/s of output, where single-threaded `bzip2` takes over a
quarter of an hour.

**tar** is read as a stream (`tar.rs`): ustar, GNU long names and pax
headers, checksums checked, never seeking, so members come straight out of
the decompressor. `Archive::extract` writes each member beside its name
(`.partial`) and renames it when whole, and calls back once it is in place:
that is what lets a build load a table while the next one is still being
decompressed.

**COPY text** (`copy.rs`): tabs between fields, `\N` for `NULL`, backslash
escapes. Only the fixtures and checks read it in Rust: loading leaves the
reading to DuckDB.

**Downloads** (`download.rs`) fetch an export's archives into
`<dir>/<name>/`, resuming a `.part` file with `Range` requests, hashing as
they go (a resumed part is hashed first), and renaming only when the SHA-256
matches `SHA256SUMS`.

## Building a database (`musicbrainz-db`)

```text
archive ─▶ bzip2 (every core) ─▶ tar ─▶ member file ─▶ DuckDB read_csv (every core) ─▶ table
         └──────────── extract thread ──────────┘  └──────────── loader thread ────────────┘
```

1. The export's layout and metadata are found (`ExportLayout`): an archive,
   an export folder, or a folder of tables taken out (`mbdump/`).
2. The schema is chosen by the dump's `SCHEMA_SEQUENCE`.
3. A loader thread owns the DuckDB connection. Tables already taken out are
   loaded where they are; the others are extracted into a work folder and
   handed to the loader through a channel of one, so the work folder never
   holds more than about three tables (a loaded table is removed unless
   `keep_tables`).
4. Each table is created with DuckDB types from the schema and loaded with
   one `INSERT … SELECT` over `read_csv(delim = '\t', quote = '', escape = '',
   nullstr = '\N')`: every field read as text, then cast to its column's type
   (`INTEGER`, `UUID`, `BOOLEAN` from `t`/`f`, `TIMESTAMPTZ`, `JSON`,
   `INTEGER[]` from `{1,2}`, `DOUBLE[]` from a point's `(x,y)`), with text
   unescaped by the `mb_unescape` macro only where it holds a backslash.
   An escaped tab or newline is two characters in the file, so splitting on
   tabs and newlines is exact. Before a table is loaded, its first line's
   field count is checked against the schema: a dump of another schema fails
   with the table's name, not with a cast error deep in a column.
5. Tables an archive leaves out (MetaBrainz omits empty ones: 60 of the
   core's 237) are created empty.
6. Indexes: `keys` (default) makes a unique index on every primary key and
   one on every `gid`; `foreign` also indexes every foreign key column.
7. Views (`mb_relationship`, `mb_link_attribute`), macros
   (`mb_partial_date`, `mb_wikidata_id`), and the metadata tables `mb_meta`
   (export, timestamp, sequences, schema used, build time) and `mb_tables`
   (rows, bytes and load time per table).

The database is written as `<database>.building` and renamed when whole, so
no reader ever opens a half-built one; a failed or stopped build removes it
and leaves any previous database alone.

Insertion order is kept (DuckDB's `preserve_insertion_order`), so a table's
`rowid` is its line in the dump: "in the dump's order" is `ORDER BY rowid`.

## Refreshing (`refresh`, `diff`)

A refresh builds the newer export beside the database, diffs the two, and
renames the new file over the old (with `keep_previous`, the old one is kept
as `<database>.previous`, through a hard link where the file system has
them, so the swap is one rename). Readers that have the old file open keep
reading it.

The diff attaches the old database read-only and compares, per entity type:

- the entity's own rows, by id, with DuckDB's `hash()` of the whole row;
- the rows that are the entity's: the tables named `<entity>_…` with a
  foreign key to it (aliases, IPIs, ISNIs, attributes, redirects), a few
  named otherwise (a recording's `isrc`, a work's `iswc`, a release's
  `medium` and, through it, its `track`s, an area's ISO codes), and every
  relationship table on either side. Per entity and table, the XOR of the
  rows' hashes and their count are compared.

Each pass reads each table of both databases once and holds only the keys
that changed. `mb_changes` gets one row per entity that changed: `added`,
`changed` (with the tables whose rows changed), `removed`, or `merged` (its
MBID is in `<entity>_gid_redirect` now, with the MBID it points to).

## Querying

`Database` keeps a pool of connections (one per concurrent caller). Rows come
out as JSON made by DuckDB (`to_json` of each row, so nested lists and structs,
MBIDs and timestamps all have one representation), a batch in one string.
DuckDB cannot bind lists, so an array parameter binds as its JSON text and the
SQL casts it (`?::INTEGER[]`, `?::UUID[]`, `from_json(?, '["VARCHAR"]')`): one
query for any number of keys.

## Node.js (`@musicbrainz-rs/node`)

Builds, refreshes, diffs, downloads and extractions run on threads of their
own and report through a threadsafe function (progress events are dropped
while JS is busy; the end and errors never are). `query` runs on the libuv
pool; `querySync` on the calling thread. `stream` runs a query on a thread of
its own and sends batches under credit-based flow control (as in
wikidata-rs): JS grants one batch per batch it takes, so at most `readAhead`
batches wait in memory however slow the consumer. The stream keeps the event
loop alive only while JS awaits a batch.

The lookups (`byId`, `byGid`, `resolveGids`, `rowsOf`, `relationships`,
`urls`, `credits`, `aliases`, `areaCountries`) and `entities` are TypeScript
over SQL: each is one query for all the keys asked, whatever their number.

The addon uses the system allocator on macOS and mimalloc elsewhere: on macOS
mimalloc keeps its heaps in fixed TLS slots of the process, and two addons that
bundle it crash when loaded together.
