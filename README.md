# musicbrainz-rs

MusicBrainz full exports as [DuckDB](https://duckdb.org) databases — built
straight from `mbdump.tar.bz2` in one pass, typed and indexed from
MusicBrainz's own schema, refreshed from the next export with a diff of
everything that changed, and queried from Rust, the command line or Node.js.

```sh
musicbrainz download latest --dir dumps          # 7.6 GB, checked against SHA256SUMS
musicbrainz build dumps/20261017-002244 mb.duckdb # every core table, typed and indexed
musicbrainz query mb.duckdb "SELECT name, sort_name FROM artist WHERE gid = '056e4f3e-d505-4dad-8ec1-d04f521cbb56'"
```

```ts
import { buildDatabase, MusicBrainzDatabase } from "@musicbrainz-rs/node";

await buildDatabase({ source: "dumps/20261017-002244", database: "mb.duckdb" });
const db = MusicBrainzDatabase.open("mb.duckdb");

const [daftPunk] = await db.search.artists("daft punk", { mode: "exact" });
const albums = await db.artists.discography(daftPunk.gid, { primaryTypes: ["Album"], excludeSecondary: true });
const members = await db.artists.members(daftPunk.gid);        // with years and instruments
const ids = await db.artists.externalIds(daftPunk.gid);        // { wikidata: ["Q185828"], discogs: […], spotify: […] }
// …or any SQL:
const rows = await db.query("SELECT name FROM release_group WHERE artist_credit IN (SELECT artist_credit FROM artist_credit_name WHERE artist = ?)", [daftPunk.id]);
```

## Why

A MusicBrainz full export is ~180 PostgreSQL tables in COPY text (25 GB
inside a 7.6 GB bzip2 tar). Reading it without PostgreSQL meant decompressing
on one core (a quarter of an hour), then sorting tables on disk to join them.
Here:

| | |
|---|---|
| Decompress the whole core archive (7.6 GB → 25.6 GB) | **49 s** (525 MB/s) |
| Build the database from it: 177 tables, 269 M rows, typed and indexed | **3 min 56 s** |
| An artist by MBID | ~10 ms (Node.js, with the round trip) |
| 10,000 artists by id / their aliases / their areas' countries | 109 / 27 / 111 ms |
| The relationships of 10,000 artists (every `l_artist_*` table) | 0.7 s |
| Stream every artist (3 M rows) into Node.js | 1.2 s |
| Join anything with anything | DuckDB: parallel, vectorised, spills to disk |

Measured on an Apple M1 Max (10 cores, 32 GB) with the 2026-10-17 export on
a USB disk and the database (20 GB) on the internal SSD. A build's peak memory
is DuckDB's (15 GB here; bound it with `memoryLimit` / `--memory`).

## What it does

- **Reads every archive of a full export** — `mbdump.tar.bz2` (CC0) and the
  others (`mbdump-derived`, `-cdstubs`, `-cover-art-archive`, …) — with the
  bzip2 decoder of [wikidata-rs](https://github.com/the-haus/wikidata-rs)
  (block-parallel, every CRC checked) and a streaming tar reader. Tables
  already taken out (`mbdump/<table>`) are read where they are.
- **Types every column from MusicBrainz's own SQL.** The columns, types,
  `NOT NULL`s, primary and foreign keys of all 370 tables come from
  musicbrainz-server's `CreateTables.sql`, `CreatePrimaryKeys.sql` and
  `CreateFKConstraints.sql`, bundled for schema sequence 31. A dump of
  **another schema sequence** loads with that sequence's SQL, fetched from
  musicbrainz-server once and cached — no new release needed.
- **Loads in one pass.** Each table goes into DuckDB's parallel CSV reader as
  soon as it is out of the archive, and is removed again: the disk never
  holds more than a few tables besides the database. `NULL`s, booleans,
  UUIDs, timestamps, arrays, points and escapes all come out right.
- **Indexes** ids and MBIDs (or every foreign key), and adds views over all
  relationship tables (`mb_relationship`, `mb_link_attribute`) and macros
  (`mb_partial_date`, `mb_wikidata_id`).
- **Refreshes** from a newer export: builds beside, diffs, swaps atomically.
  `mb_changes` lists every MBID **added, changed (and in which tables:
  `artist_alias`, `l_artist_url`, `track`, …), removed or merged** — what an
  incremental ingest needs.
- **Downloads** exports from MetaBrainz (resumable, SHA-256 checked).
- **Queries**: SQL with JSON rows, streams with backpressure, and batched
  lookups — by id, by MBID with merges followed, an entity's rows,
  relationships with attributes and targets, artist credits, aliases, the
  country an area is in — and whole entities shaped like MusicBrainz's web
  service (artists with areas and life spans, releases with media, tracks
  and recordings, …).
- **Helpers** for the questions MusicBrainz is asked, each one call:
  search by name (exact, prefix, substring, fuzzy; aliases; ranked), an
  artist's discography, releases, recordings, works, members and bands,
  collaborators, events, labels and country; a release group's original
  release and tracklist; releases by barcode or catalogue number;
  recordings by ISRC and where they appear; works by ISWC and their
  writers; labels by label code and their catalogues; the area tree; places
  near a point; an event's lineup; a festival's editions in order; the genre
  taxonomy; and who an entity is on 30 other services (Wikidata, Discogs,
  Spotify, Apple Music, …) and back.
- **Sessions** hold one connection for temporary tables (stage a scope,
  then join everything against it).
- **Subsets**: `musicbrainz subset` writes a consistent slice of a database
  (some artists, what is theirs, and every row those refer to) back as an
  export, for fixtures: `fixtures/daft-punk` (1 MB) is one.

## Packages

| | |
|---|---|
| [`@musicbrainz-rs/node`](crates/bindings/musicbrainz-node) | Node.js (napi-rs): build, refresh, query, download. Prebuilt for Linux (glibc, musl; x64, arm64), macOS (x64, arm64), Windows (x64). |
| [`musicbrainz-cli`](crates/musicbrainz-cli) | the `musicbrainz` command |
| [`musicbrainz-db`](crates/musicbrainz-db) | Rust: build, refresh, diff and query databases |
| [`musicbrainz-dump`](crates/musicbrainz-dump) | Rust: archives, tar, COPY text, downloads |
| [`musicbrainz-schema`](crates/musicbrainz-schema) | Rust: the schema as data |

## The command line

```text
musicbrainz latest                         the newest export's name
musicbrainz download [name|latest] --dir D download an export (mbdump.tar.bz2; --files for others)
musicbrainz info <export|database>         what an export or a database is
musicbrainz members <archive>              list an archive's members
musicbrainz extract <archive> <dir>        take tables out (--tables a,b)
musicbrainz build <export> <db>            build a database (--tables, --archives, --indexes, --keep-tables, …)
musicbrainz refresh <export> <db>          build anew, diff against <db>, swap (--keep-previous)
musicbrainz diff <old-db> <new-db>         write mb_changes into <new-db>
musicbrainz upgrade <db>                   bring a database's views and macros up to this release
musicbrainz query <db> <sql>               run SQL (--param JSON…, --format table|json|jsonl|tsv)
musicbrainz subset <db> <dir> --seed <artist mbid>    a consistent slice, as an export
musicbrainz schema [table]                 the bundled schema
```

## The database

Every table keeps its MusicBrainz name and columns (tables of other
PostgreSQL schemas are in DuckDB schemas of that name:
`cover_art_archive.cover_art`). `rowid` is a row's line in the dump.

| Object | |
|---|---|
| `mb_meta` | `export_name`, `timestamp`, `schema_sequence`, `replication_sequence`, `schema`, `built_at`, `indexes`, … |
| `mb_tables` | rows, bytes and load time of each table |
| `mb_changes` | after a refresh: `entity`, `id`, `gid`, `change`, `tables`, `merged_into` |
| `mb_relationship` | every `l_<a>_<b>` row with `entity0_type`, `entity1_type`, `link_type` (name), `link_type_gid`, phrases, dates, `ended` |
| `mb_link_attribute` | each link's attributes with their type, credit and text value |
| `mb_partial_date(y, m, d)` | `1994`, `1994-05`, `1994-05-17` |
| `mb_wikidata_id(url)` | `Q123` from a Wikidata URL |

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how it works and
[docs/DATABASE.md](docs/DATABASE.md) for querying it.

## Licences

This code: [LICENCE.txt](LICENCE.txt). MusicBrainz's data: the core
(`mbdump.tar.bz2`) is CC0; the other archives are CC BY-NC-SA 3.0
(`musicbrainz schema` shows which archive each table is in, and
`TableSchema.licence` in Node). A build reads only the core unless asked
for others.
