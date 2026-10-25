# Changelog

## 0.1.0 — 2026-10-25

The first release: MusicBrainz full exports as DuckDB databases, from Rust,
the command line and Node.js.

- **Schema** (`musicbrainz-schema`): every table of every archive of a full
  export, its columns in the dump's order, types, `NOT NULL`s, primary and
  foreign keys, parsed from musicbrainz-server's SQL (schema sequence 31,
  bundled), and which archive (and licence) holds each table.
- **Archives** (`musicbrainz-dump`): wikidata-rs's parallel bzip2 decoder
  (the 7.6 GB core archive in 49 s on ten cores), a streaming tar reader,
  extraction that renames each member into place and calls back as it
  lands, PostgreSQL COPY text, export layouts and metadata, and resumable,
  SHA-256-checked downloads from MetaBrainz.
- **Databases** (`musicbrainz-db`): one-pass builds (tables loaded by
  DuckDB's parallel CSV reader as they come out of the archive, typed and
  unescaped from the schema, then indexed); schemas of other sequences
  fetched from musicbrainz-server and cached; empty tables the archive
  leaves out created; `mb_relationship` and `mb_link_attribute` views,
  `mb_partial_date` and `mb_wikidata_id` macros, `mb_meta` and `mb_tables`;
  refreshes that build beside, diff (`mb_changes`: added, changed with the
  tables that changed, removed, merged) and swap; JSON queries in batches
  from a connection pool, and sessions for temporary tables; consistent
  subsets written back as exports; `upgrade` for an older database's views
  and macros; `lenient` builds for hand-made test dumps; builds that fit a
  small memory limit (DuckDB's CSV buffers kept small). The full core export
  builds in 3 min 56 s (177 tables, 269 M rows, indexed).
- **CLI** (`musicbrainz`): `latest`, `download`, `info`, `members`,
  `extract`, `build`, `refresh`, `diff`, `upgrade`, `query`, `subset`,
  `schema`.
- **Node.js** (`@musicbrainz-rs/node`): `buildDatabase`, `refreshDatabase`,
  `diffDatabases`, `downloadExport`, `extractArchive`, `exportInfo`,
  `archiveMembers`, `latestExport`, `schema`; `MusicBrainzDatabase` with
  `query`, `querySync`, `stream` (backpressure-bounded), sessions, lookups
  by id, MBID (merges followed) and foreign key, relationships, credits,
  aliases, area countries, web-service-shaped entities, and `changes`.
  Prebuilt for Linux (glibc and musl; x64 and arm64), macOS (x64 and arm64)
  and Windows (x64).
