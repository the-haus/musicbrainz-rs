# @musicbrainz-rs/node

MusicBrainz full exports as [DuckDB](https://duckdb.org) databases, in
Node.js: built from `mbdump.tar.bz2` in one pass (parallel bzip2 in Rust),
typed from MusicBrainz's own schema, refreshed from the next export with a
diff of what changed, and queried with SQL or batched lookups.

```sh
npm install @musicbrainz-rs/node
```

Prebuilt for Linux (glibc 2.31+ and musl; x64 and arm64), macOS (x64 and
arm64) and Windows (x64), Node 18 and up. DuckDB is built in: nothing else
to install.

```ts
import { buildDatabase, downloadExport, MusicBrainzDatabase } from "@musicbrainz-rs/node";

// 1. Download the newest export (7.6 GB; resumable, SHA-256 checked).
const { dir } = await downloadExport({ dir: "dumps" });

// 2. Build a database: every core table, typed and indexed (a few minutes).
await buildDatabase(
  { source: dir, database: "mb.duckdb" },
  { onProgress: (p) => console.log(p.phase, p.table, p.rows) },
);

// 3. Query it.
const db = MusicBrainzDatabase.open("mb.duckdb");
const artist = await db.entity("artist", "056e4f3e-d505-4dad-8ec1-d04f521cbb56", ["aliases", "relationships"]);
console.log(artist?.name, artist?.area?.name, artist?.lifeSpan);
```

## Building and refreshing

### `buildDatabase(options, { onProgress?, signal? }): Promise<BuildStats>`

| option | |
|---|---|
| `source` | an archive (`…/mbdump.tar.bz2`), an export folder, or a folder of tables taken out (`<dir>/mbdump/<table>`) |
| `database` | the file to write |
| `tables` | only these tables (default: every table of the archives read) |
| `archives` | other archives of the export to load too (`["mbdump-derived.tar.bz2"]`; mind their CC BY-NC-SA licence) |
| `indexes` | `"keys"` (default: ids and MBIDs), `"foreign"` (also every foreign key), `"none"` |
| `workDir`, `keepTables` | where tables are extracted to (default `<database>.work`), and whether to keep them |
| `schemaDir`, `schemaCache`, `offline` | the schema SQL to read the dump with (default: bundled for its `SCHEMA_SEQUENCE`, or fetched from musicbrainz-server into `schemaCache`) |
| `threads`, `memoryLimit` | DuckDB's (and bzip2's) threads, DuckDB's memory limit (`"8GB"`) |
| `lenient` | for hand-made test dumps: no `NOT NULL`s, MBID columns as text |
| `replace` | replace an existing database (refused otherwise) |

Tables already taken out are read where they are; the rest are decompressed
on every core and loaded as each arrives. The database is written beside
(`.building`) and renamed when whole. `signal` stops it cleanly.

`BuildStats`: `{ database, export: { name, timestamp, schemaSequence, replicationSequence }, schema, tables: [{ name, rows, bytes, loadMs, from }], skipped, diff, elapsedMs }`.

### `refreshDatabase(options, task): Promise<BuildStats>`

Builds the (newer) export beside `options.database`, diffs the two, and
swaps the new one in (`keepPrevious: true` keeps the old as
`<database>.previous`). `stats.diff` counts, per entity type, the MBIDs
`added`, `changed`, `removed` and `merged`; the rows are in `mb_changes`:

```ts
const stats = await refreshDatabase({ source: "dumps/20261021-001758", database: "mb.duckdb" });
const db = MusicBrainzDatabase.open("mb.duckdb");
for (const c of await db.changes({ entity: "artist" }))
  console.log(c.gid, c.change, c.tables, c.merged_into); // "changed", ["artist_alias", "l_artist_url"]
```

### `diffDatabases(older, newer)`

The diff alone, writing `mb_changes` into `newer`.

## Querying

### `MusicBrainzDatabase.open(path, { readOnly = true, threads, memoryLimit })`

| method | |
|---|---|
| `query<T>(sql, params?)` | rows (off the event loop) |
| `querySync<T>(sql, params?)` | rows, blocking |
| `one<T>(sql, params?)` | the first row |
| `stream<T>(sql, params?, { batchSize, readAhead, signal })` | a `RowStream`: `for await (const row of …)`, `.batches()`, `.toArray()`, `.close()`; backpressure-bounded |
| `execute(sql, params?)` | rows changed (needs `readOnly: false`) |
| `exec(sql)` | statements that return nothing (`CREATE TEMP TABLE …`) |
| `describe(sql)` | columns and types, without running it |
| `info()` | `mb_meta` (export, timestamp, sequences, …) and rows per table |
| `session()`, `withSession(f)` | a connection of its own (below) |
| `close()` | |

### Sessions

The database runs each call on a connection of a pool, so a temporary table
made by one call may not be there for the next. A session holds one
connection: everything above works on it, and its temporary tables last
until it is closed.

```ts
await db.withSession(async (s) => {
  s.exec(`CREATE TEMP TABLE scope AS SELECT id FROM artist WHERE area IN (SELECT area FROM iso_3166_1 WHERE code = 'NL')`);
  for await (const batch of s.stream("SELECT rg.* FROM scope JOIN artist_credit_name acn ON acn.artist = scope.id JOIN release_group rg ON rg.artist_credit = acn.artist_credit").batches())
    handle(batch);
});
```

Placeholders are `?` or `$1`. DuckDB cannot bind lists: an array parameter
binds as its JSON text, which you cast:

```ts
await db.query("SELECT * FROM artist WHERE id IN (SELECT unnest(?::INTEGER[]))", [[1, 2, 3]]);
await db.query("SELECT * FROM artist WHERE gid IN (SELECT unnest(?::UUID[]))", [gids]);
await db.query(`SELECT * FROM artist WHERE name IN (SELECT unnest(from_json(?, '["VARCHAR"]')))`, [names]);
```

Rows come out as DuckDB makes them JSON: MBIDs, dates and timestamps are
strings, lists and structs nested, `BIGINT`s numbers.

### Lookups

Each is one query for all the keys, whatever their number.

| method | returns |
|---|---|
| `byId(table, ids, { column, columns })` | `Map<id, row>` |
| `byGid(entity, gids)` | `Map<gid asked, row>`, merges followed |
| `resolveGids(entity, gids)` | `Map<gid asked, { id, gid, redirected }>` |
| `rowsOf(table, column, keys, { columns, orderBy })` | `Map<key, rows[]>` in the dump's order |
| `names(table)` | `Map<id, name>` of a small table (`artist_type`, `medium_format`) |
| `relationships(entity, ids, { targetTypes, types, direction, attributes, targets })` | `Map<id, Relationship[]>`: type, direction, target `{ id, gid, name }`, dates, attributes, credits |
| `urls(entity, ids, { types })` | `Map<id, { type, url, ended }[]>` |
| `credits(creditIds)` | `Map<id, ArtistCredit>`: the names in order, each with its artist |
| `aliases(entity, ids)` | `Map<id, Alias[]>` with type names, locales, dates |
| `areaCountries(areaIds)` | `Map<area id, ISO 3166-1>`, walking "part of" up |
| `entities(entity, idsOrGids, include)` / `entity(…)` | entities shaped like MusicBrainz's web service |
| `changes({ entity, change })` | `mb_changes` rows |

`entities` gives, for every type, `id`, `gid`, `name`, `disambiguation`,
`type`, `lifeSpan` where the entity has one, and:

| entity | also |
|---|---|
| artist | `sortName`, `gender`, `area`, `beginArea`, `endArea` (with ISO codes) |
| release group | `primaryType`, `secondaryTypes`, `artistCredit`, `firstReleaseDate`; `releases` with `include: ["releases"]` |
| release | `status`, `packaging`, `language`, `script`, `barcode`, `releaseGroup`, `artistCredit`, `releaseEvents`, `date`, `labels`; `media` (tracks, recordings, credits) with `include: ["media"]` |
| recording | `length`, `video`, `artistCredit`, `isrcs` |
| work | `languages`, `iswcs`, `attributes` |
| label | `labelCode`, `area` |
| area | `iso3166_1`, `iso3166_2`, `iso3166_3` |
| place | `address`, `area`, `coordinates` |
| event | `time`, `cancelled`, `setlist` |
| series, instrument, genre, url | `orderingType`, `description`, —, `resource` |

and with `include`: `aliases`, `relationships`, `urls`, `redirects` (MBIDs
merged in), `identifiers` (IPIs, ISNIs).

## Exports

| function | |
|---|---|
| `latestExport({ base })` | the newest export's name |
| `exportChecksums(name)` | `SHA256SUMS` as `{ file: sha256 }` |
| `downloadExport({ dir, name, files, base }, task)` | download into `<dir>/<name>/` |
| `exportInfo(path)` | an export's archives, tables folder, timestamp and sequences |
| `archiveMembers(archive)` | its members |
| `extractArchive(archive, dir, { tables, onMember, onProgress, signal })` | take tables out |
| `schema()`, `tableSchema(name)` | the bundled schema: columns, types, keys, archive and licence of every table |

Errors are `MusicBrainzError`s with a `code`: `aborted`, `invalid` or
`failed`.

## The database

See the repository's [docs/DATABASE.md](https://github.com/the-haus/musicbrainz-rs/blob/main/docs/DATABASE.md):
its tables, views (`mb_relationship`, `mb_link_attribute`), macros and
examples.

## Licence

This package: see LICENSE. MusicBrainz's core data is CC0; the other
archives are CC BY-NC-SA 3.0.
