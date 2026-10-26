# Querying a database

A database holds MusicBrainz's tables as they are in PostgreSQL: same names,
same columns, DuckDB types. Anything written against MusicBrainz's schema
works, and the
[schema diagrams](https://musicbrainz.org/doc/MusicBrainz_Database/Schema)
apply. This page is about what is particular to these databases, with
examples.

## Conventions

- **Entities** have an integer `id` and an MBID `gid` (`UUID`). Merged
  MBIDs are in `<entity>_gid_redirect (gid, new_id)`.
- **Small tables** name things by id: `artist.type` → `artist_type.name`,
  `release.status` → `release_status.name`, `medium.format` →
  `medium_format.name`, `release.language` → `language.iso_code_3`.
- **Partial dates** are three columns (`begin_date_year`, `…_month`,
  `…_day`): `mb_partial_date(begin_date_year, begin_date_month,
  begin_date_day)` gives `1994`, `1994-05` or `1994-05-17`.
- **Artist credits**: an entity's `artist_credit` is an `artist_credit.id`;
  its names are `artist_credit_name` rows (`position`, `artist`, `name`,
  `join_phrase`).
- **Relationships** are `l_<a>_<b>` tables (`a <= b` alphabetically:
  `l_artist_url`, `l_release_group_url`, `l_area_area`), each row an
  `entity0` of type `a`, an `entity1` of type `b`, and a `link`; the link has
  the `link_type` (its `name`: `member of band`, `wikidata`, `remix`) and the
  dates. `mb_relationship` is all of them in one view, with the link and its
  type joined in (`link_table` names the table, `row_order` is the row's
  line in it).
- **Order**: `rowid` is a row's line in the dump; `ORDER BY rowid` is "in
  the dump's order".
- **Empty tables** that the archive left out are there, empty.

## Examples

An artist by MBID, its type and where it is from:

```sql
SELECT a.name, t.name AS type, area.name AS area, mb_partial_date(a.begin_date_year, a.begin_date_month, a.begin_date_day) AS began
FROM artist a
LEFT JOIN artist_type t ON t.id = a.type
LEFT JOIN area ON area.id = a.area
WHERE a.gid = '056e4f3e-d505-4dad-8ec1-d04f521cbb56';
```

An MBID that may have been merged:

```sql
SELECT a.* FROM artist a WHERE a.gid = $1
UNION ALL
SELECT a.* FROM artist_gid_redirect r JOIN artist a ON a.id = r.new_id WHERE r.gid = $1;
```

The members of a group, with their years:

```sql
SELECT m.name, r.begin_date_year, r.end_date_year, r.ended
FROM mb_relationship r JOIN artist m ON m.id = r.entity0
WHERE r.link_table = 'l_artist_artist' AND r.link_type = 'member of band'
  AND r.entity1 = (SELECT id FROM artist WHERE gid = '056e4f3e-d505-4dad-8ec1-d04f521cbb56');
```

Every artist with a Wikidata item:

```sql
SELECT a.gid, mb_wikidata_id(u.url) AS wikidata
FROM l_artist_url l JOIN link ON link.id = l.link JOIN link_type lt ON lt.id = link.link_type
JOIN url u ON u.id = l.entity1 JOIN artist a ON a.id = l.entity0
WHERE lt.name = 'wikidata';
```

An artist's albums with their first release date:

```sql
SELECT rg.name, min(mb_partial_date(rc.date_year, rc.date_month, rc.date_day)) AS first
FROM artist_credit_name acn
JOIN release_group rg ON rg.artist_credit = acn.artist_credit
JOIN release_group_primary_type pt ON pt.id = rg.type AND pt.name = 'Album'
JOIN release r ON r.release_group = rg.id
LEFT JOIN release_country rc ON rc.release = r.id
WHERE acn.artist = 1234
GROUP BY rg.id, rg.name ORDER BY first;
```

A release's tracklist:

```sql
SELECT m.position AS disc, t.number, t.name, t.length / 1000 AS seconds
FROM medium m JOIN track t ON t.medium = m.id
WHERE m.release = (SELECT id FROM release WHERE gid = $1)
ORDER BY m.position, t.position;
```

The country of each area (walking "part of" up):

```sql
WITH RECURSIVE parent AS (
  SELECT entity1 AS child, arg_min(entity0, r.id) AS parent
  FROM mb_relationship r WHERE link_table = 'l_area_area' AND link_type = 'part of' GROUP BY entity1),
walk(start, area, depth) AS (
  SELECT id, id, 0 FROM area
  UNION ALL
  SELECT w.start, p.parent, w.depth + 1 FROM walk w JOIN parent p ON p.child = w.area
  WHERE w.depth < 16 AND NOT EXISTS (SELECT 1 FROM iso_3166_1 i WHERE i.area = w.area))
SELECT w.start AS area, arg_min(i.code, w.depth) AS country
FROM walk w JOIN iso_3166_1 i ON i.area = w.area GROUP BY w.start;
```

What changed in the last refresh:

```sql
SELECT change, count(*) FROM mb_changes WHERE entity = 'artist' GROUP BY 1;
SELECT gid, tables FROM mb_changes WHERE entity = 'release' AND change = 'changed' AND list_contains(tables, 'track');
```

## Many keys at once

DuckDB cannot bind a list, so pass a list as JSON text and cast it:

```sql
SELECT * FROM artist WHERE id IN (SELECT unnest(?::INTEGER[]));       -- [1, 2, 3]
SELECT * FROM artist WHERE gid IN (SELECT unnest(?::UUID[]));         -- ["…", "…"]
SELECT * FROM artist WHERE name IN (SELECT unnest(from_json(?, '["VARCHAR"]')));
```

The Node.js lookups (`byId`, `byGid`, `rowsOf`, `relationships`, `credits`,
`aliases`, `areaCountries`, `entities`) do this for you: one query per call,
whatever the number of keys. For the common questions (a discography, a
tracklist, an ISRC, a band's members, what is near a venue, who an artist is
on Discogs), the Node.js package has helpers that write these queries: see
its README's *Helpers*.

## Performance notes

- With the default `keys` indexes, lookups by `id` or `gid` use an index;
  lookups by another column (`artist_alias.artist`) scan that column, which
  DuckDB does on every core (tens of milliseconds for millions of rows).
  For many such single lookups, build with `indexes: "foreign"`.
- Batches beat loops: one query with 10,000 ids is about as fast as one with
  ten.
- Large joins spill to the temp folder beside the database
  (`<database>.tmp`); set `memoryLimit` to bound DuckDB's memory.
- Open read-only (the default) to let several processes read one database.
