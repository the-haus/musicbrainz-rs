# Changelog

## 0.2.0 — 2026-10-26

Helpers for the questions MusicBrainz is asked, and a sturdier package.

- **Helpers** (`@musicbrainz-rs/node`), each one call, taking an entity by
  id or MBID (merges followed), paged, empty for an entity that is not there:
  - `db.search`: every entity type by name, sort name and alias; exact,
    prefix, substring or fuzzy (Jaro–Winkler); case and accents ignored;
    ranked by score, then popularity.
  - `db.artists`: `discography`, `releases`, `recordings`, `works`,
    `members`, `memberOf`, `related`, `collaborators`, `events`, `labels`,
    `country`, `urls`, `externalIds`, `aliases`, `counts`, `find`,
    `byExternalId`, `refs`.
  - `db.releaseGroups` (`releases`, `original`), `db.releases` (`tracklist`,
    `byBarcode`, `byCatalogNumber`, `labels`, `events`, `otherVersions`,
    `recordings`), `db.recordings` (`byIsrc`, `isrcs`, `appearances`,
    `works`), `db.works` (`byIswc`, `iswcs`, `writers`, `recordings`).
  - `db.labels` (`byLabelCode`, `releases`, `artists`, `parents`,
    `imprints`), `db.areas` (`byIsoCode`, `country`, `ancestors`,
    `children`, `artists`, `labels`, `places`), `db.places` (`near`,
    `events`), `db.events` (`lineup`, `place`, `series`, `between`),
    `db.series` (`items`, `events`), `db.genres` (`all`, `byName`,
    `parents`, `subgenres`, `fusionOf`, `graph`).
  - `db.external`: ids on 30 services read from URLs (`parseUrl`,
    `externalIds`, `SERVICES`), and the entities of a URL or an id elsewhere
    (`byUrl`, `find`, `byWikidata`).
  - `db.stats()`, `db.linkTypes(a, b)`.
- **Stability**: table and column names a lookup takes must be identifiers;
  ids must be safe integers, dates partial dates, pages bounded; a panic in
  native code is an error, never a crash; the connection pool keeps at most
  16 connections; streams and tasks whose native side cannot start fail
  with a `MusicBrainzError`; a database of a newer format is refused at
  `open`, a DuckDB file built otherwise opens for SQL; a new error code,
  `unavailable`, for a helper the database lacks a table for. A stability
  test suite (misuse, injection, 64 concurrent calls, parallel streams, use
  after close, databases with few tables).
- **Subsets** take release groups to include whatever the caps
  (`--seed release-group:<mbid>`) and the genre taxonomy whole; the fixture
  has Daft Punk's *Discovery* and *Random Access Memories*.

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
