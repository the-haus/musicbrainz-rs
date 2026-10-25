// Entities as MusicBrainz's web service shapes them (in camelCase): an
// entity's row with its type's name, life span and the summaries it points
// to, plus what `include` asks for. Each part is one query for all the
// entities asked for, whatever their number.

import type { Alias, ArtistCredit, Queries, Relationship } from "./database.js";
import { MusicBrainzError, type EntityType, type Row } from "./types.js";

/** What {@link Queries.entities} adds to each entity. */
export type EntityInclude =
  /** `aliases`: its aliases. */
  | "aliases"
  /** `relationships`: all its relationships (with attributes and the other entities). */
  | "relationships"
  /** `urls`: its URL relationships only. */
  | "urls"
  /** `redirects`: the MBIDs merged into it. */
  | "redirects"
  /** `identifiers`: IPI and ISNI codes (artists, labels). */
  | "identifiers"
  /** `media`: a release's media and their tracks, with each track's recording. */
  | "media"
  /** `releases`: a release group's releases. */
  | "releases";

/** A partial date range: `1994`, `1994-05`, `1994-05-17`. */
export interface LifeSpan {
  begin: string | null;
  end: string | null;
  ended: boolean;
}

/** An entity as other entities name it. */
export interface Summary {
  id: number;
  gid: string;
  name: string;
}

/** An area as an entity names it, with the ISO 3166-1 code it has (countries). */
export interface AreaSummary extends Summary {
  iso3166_1: string[];
}

/** What every entity has. */
export interface EntityBase {
  id: number;
  gid: string;
  /** Its disambiguation comment (`comment` in the database). */
  disambiguation: string | null;
  aliases?: Alias[];
  relationships?: Relationship[];
  urls?: { type: string; url: string; ended: boolean }[];
  /** MBIDs merged into it. */
  redirects?: string[];
}

export interface Artist extends EntityBase {
  name: string;
  sortName: string;
  type: string | null;
  gender: string | null;
  area: AreaSummary | null;
  beginArea: AreaSummary | null;
  endArea: AreaSummary | null;
  lifeSpan: LifeSpan;
  ipis?: string[];
  isnis?: string[];
}

export interface Label extends EntityBase {
  name: string;
  type: string | null;
  labelCode: number | null;
  area: AreaSummary | null;
  lifeSpan: LifeSpan;
  ipis?: string[];
  isnis?: string[];
}

export interface Area extends EntityBase {
  name: string;
  type: string | null;
  iso3166_1: string[];
  iso3166_2: string[];
  iso3166_3: string[];
  lifeSpan: LifeSpan;
}

export interface Place extends EntityBase {
  name: string;
  type: string | null;
  address: string;
  area: AreaSummary | null;
  coordinates: { latitude: number; longitude: number } | null;
  lifeSpan: LifeSpan;
}

export interface Event extends EntityBase {
  name: string;
  type: string | null;
  time: string | null;
  cancelled: boolean;
  setlist: string | null;
  lifeSpan: LifeSpan;
}

export interface Series extends EntityBase {
  name: string;
  type: string | null;
  orderingType: string | null;
}

export interface Instrument extends EntityBase {
  name: string;
  type: string | null;
  description: string;
}

export interface Genre extends EntityBase {
  name: string;
}

export interface Url extends EntityBase {
  /** The URL. */
  resource: string;
}

export interface Recording extends EntityBase {
  name: string;
  /** Milliseconds. */
  length: number | null;
  video: boolean;
  artistCredit: ArtistCredit | null;
  isrcs: string[];
}

export interface Work extends EntityBase {
  name: string;
  type: string | null;
  /** ISO 639-3 codes. */
  languages: string[];
  iswcs: string[];
  attributes: { type: string; value: string }[];
}

export interface ReleaseGroup extends EntityBase {
  name: string;
  primaryType: string | null;
  secondaryTypes: string[];
  artistCredit: ArtistCredit | null;
  /** The earliest release date of its releases. */
  firstReleaseDate: string | null;
  releases?: Omit<Release, "releaseGroup">[];
}

export interface Track {
  id: number;
  gid: string;
  position: number;
  /** As printed: `A1`, `3`. */
  number: string;
  name: string;
  length: number | null;
  artistCredit: ArtistCredit | null;
  recording: Summary & { length: number | null; video: boolean };
  isDataTrack: boolean;
}

export interface Medium {
  id: number;
  gid: string;
  position: number;
  name: string;
  format: string | null;
  trackCount: number;
  tracks: Track[];
}

export interface Release extends EntityBase {
  name: string;
  status: string | null;
  packaging: string | null;
  /** ISO 639-3. */
  language: string | null;
  /** ISO 15924. */
  script: string | null;
  barcode: string | null;
  quality: number;
  releaseGroup: Summary & { primaryType: string | null };
  artistCredit: ArtistCredit | null;
  /** When and where it came out (`area` null: worldwide or unknown). */
  releaseEvents: { date: string | null; area: AreaSummary | null }[];
  /** The earliest of its release events' dates. */
  date: string | null;
  labels: { catalogNumber: string | null; label: Summary | null }[];
  media?: Medium[];
}

/** The shape of an entity type. */
export type EntityOf<E extends EntityType> = {
  area: Area;
  artist: Artist;
  event: Event;
  genre: Genre;
  instrument: Instrument;
  label: Label;
  place: Place;
  recording: Recording;
  release: Release;
  release_group: ReleaseGroup;
  series: Series;
  url: Url;
  work: Work;
}[E];

const lookups = new WeakMap<Queries, Map<string, Map<number, string>>>();

/** A small table as `id → name`, read once per database. */
async function lookup(db: Queries, table: string, column = "name"): Promise<Map<number, string>> {
  let tables = lookups.get(db);
  if (!tables) lookups.set(db, (tables = new Map()));
  const key = `${table}.${column}`;
  let m = tables.get(key);
  if (!m) {
    m = db.hasTable(table) ? await db.names(table, column) : new Map();
    tables.set(key, m);
  }
  return m;
}

const date = (r: Row, prefix: string): string | null => {
  const y = r[`${prefix}_year`] as number | null;
  if (y == null) return null;
  const m = r[`${prefix}_month`] as number | null;
  const d = r[`${prefix}_day`] as number | null;
  const pad = (n: number, w: number) => String(n).padStart(w, "0");
  return pad(y, 4) + (m != null ? `-${pad(m, 2)}` + (d != null ? `-${pad(d, 2)}` : "") : "");
};

const lifeSpan = (r: Row): LifeSpan => ({
  begin: date(r, "begin_date"),
  end: date(r, "end_date"),
  ended: r.ended === true,
});

const grouped = <K, V>(rows: Iterable<[K, V]>): Map<K, V[]> => {
  const m = new Map<K, V[]>();
  for (const [k, v] of rows) {
    const l = m.get(k);
    if (l) l.push(v);
    else m.set(k, [v]);
  }
  return m;
};

const ids = (rows: Iterable<Row>, column: string): number[] =>
  [...rows].map((r) => r[column] as number | null).filter((v): v is number => v != null);

/** Areas as entities name them. */
async function areaSummaries(db: Queries, areaIds: number[]): Promise<Map<number, AreaSummary>> {
  const out = new Map<number, AreaSummary>();
  if (!areaIds.length) return out;
  for (const a of await db.query<AreaSummary>(
    `SELECT a.id, CAST(a.gid AS VARCHAR) AS gid, a.name,
            coalesce(list(i.code ORDER BY i.code) FILTER (WHERE i.code IS NOT NULL), []) AS iso3166_1
     FROM area a LEFT JOIN iso_3166_1 i ON i.area = a.id
     WHERE a.id IN (SELECT unnest(?::INTEGER[])) GROUP BY a.id, a.gid, a.name`,
    [[...new Set(areaIds)]],
  ))
    out.set(a.id, a);
  return out;
}

async function codesOf(db: Queries, table: string, column: string, owner: string, keys: number[]) {
  if (!db.hasTable(table)) return new Map<number, string[]>();
  const rows = await db.rowsOf<{ v: string }>(table, owner, keys, { columns: `${column} AS v` });
  return new Map([...rows].map(([k, l]) => [k, l.map((r) => r.v)]));
}

/** Implements {@link Queries.entities}. */
export async function loadEntities<E extends EntityType>(
  db: Queries,
  type: E,
  keys: Iterable<number | string>,
  include: readonly EntityInclude[],
): Promise<Map<number | string, EntityOf<E>>> {
  const asked = [...keys];
  const gids = asked.filter((k): k is string => typeof k === "string");
  const keyOf = new Map<number, (number | string)[]>();
  for (const k of asked) if (typeof k === "number") keyOf.set(k, [...(keyOf.get(k) ?? []), k]);
  for (const [gid, r] of await db.resolveGids(type, gids)) {
    const original = gids.find((g) => g.toLowerCase() === gid) ?? gid;
    keyOf.set(r.id, [...(keyOf.get(r.id) ?? []), original]);
  }
  const rowsById = await db.byId(type, keyOf.keys());
  const rows = [...rowsById.values()];
  const entityIds = [...rowsById.keys()];
  const out = new Map<number, EntityBase & Record<string, unknown>>();

  const typeNames = db.hasTable(`${type}_type`) ? await lookup(db, `${type}_type`) : new Map<number, string>();
  for (const r of rows) {
    const base: EntityBase & Record<string, unknown> = {
      id: r.id as number,
      gid: r.gid as string,
      disambiguation: (r.comment as string | undefined) || null,
    };
    if ("name" in r) base.name = r.name;
    if ("type" in r && typeNames.size) base.type = typeNames.get(r.type as number) ?? null;
    if ("begin_date_year" in r) base.lifeSpan = lifeSpan(r);
    out.set(r.id as number, base);
  }

  // What each type has of its own.
  const credits = async (column = "artist_credit") => db.credits(ids(rows, column));
  switch (type) {
    case "artist": {
      const areas = await areaSummaries(db, [...ids(rows, "area"), ...ids(rows, "begin_area"), ...ids(rows, "end_area")]);
      const genders = await lookup(db, "gender");
      for (const r of rows) {
        const e = out.get(r.id as number)!;
        e.sortName = r.sort_name;
        e.gender = genders.get(r.gender as number) ?? null;
        e.area = areas.get(r.area as number) ?? null;
        e.beginArea = areas.get(r.begin_area as number) ?? null;
        e.endArea = areas.get(r.end_area as number) ?? null;
      }
      break;
    }
    case "label": {
      const areas = await areaSummaries(db, ids(rows, "area"));
      for (const r of rows) {
        const e = out.get(r.id as number)!;
        e.labelCode = r.label_code ?? null;
        e.area = areas.get(r.area as number) ?? null;
      }
      break;
    }
    case "area": {
      const [i1, i2, i3] = await Promise.all([
        codesOf(db, "iso_3166_1", "code", "area", entityIds),
        codesOf(db, "iso_3166_2", "code", "area", entityIds),
        codesOf(db, "iso_3166_3", "code", "area", entityIds),
      ]);
      for (const [id, e] of out) {
        e.iso3166_1 = i1.get(id) ?? [];
        e.iso3166_2 = i2.get(id) ?? [];
        e.iso3166_3 = i3.get(id) ?? [];
      }
      break;
    }
    case "place": {
      const areas = await areaSummaries(db, ids(rows, "area"));
      for (const r of rows) {
        const e = out.get(r.id as number)!;
        const c = r.coordinates as [number, number] | null;
        e.address = r.address;
        e.area = areas.get(r.area as number) ?? null;
        e.coordinates = c ? { latitude: c[0], longitude: c[1] } : null;
      }
      break;
    }
    case "event":
      for (const r of rows) {
        const e = out.get(r.id as number)!;
        e.time = r.time ?? null;
        e.cancelled = r.cancelled === true;
        e.setlist = (r.setlist as string | null) || null;
      }
      break;
    case "series": {
      const ordering = await lookup(db, "series_ordering_type");
      for (const r of rows) out.get(r.id as number)!.orderingType = ordering.get(r.ordering_type as number) ?? null;
      break;
    }
    case "instrument":
      for (const r of rows) out.get(r.id as number)!.description = r.description;
      break;
    case "url":
      for (const r of rows) out.get(r.id as number)!.resource = r.url;
      break;
    case "recording": {
      const [c, isrcs] = await Promise.all([credits(), codesOf(db, "isrc", "isrc", "recording", entityIds)]);
      for (const r of rows) {
        const e = out.get(r.id as number)!;
        e.length = r.length ?? null;
        e.video = r.video === true;
        e.artistCredit = c.get(r.artist_credit as number) ?? null;
        e.isrcs = [...new Set(isrcs.get(r.id as number) ?? [])];
      }
      break;
    }
    case "work": {
      const [iswcs, languages, attrs] = await Promise.all([
        codesOf(db, "iswc", "iswc", "work", entityIds),
        db.hasTable("work_language")
          ? db.query<{ work: number; code: string }>(
              `SELECT wl.work, l.iso_code_3 AS code FROM work_language wl JOIN language l ON l.id = wl.language
               WHERE wl.work IN (SELECT unnest(?::INTEGER[])) ORDER BY wl.rowid`,
              [entityIds],
            )
          : [],
        db.hasTable("work_attribute") && db.hasTable("work_attribute_type")
          ? db.query<{ work: number; type: string; value: string }>(
              `SELECT wa.work, t.name AS type, coalesce(v.value, wa.work_attribute_text) AS value
               FROM work_attribute wa JOIN work_attribute_type t ON t.id = wa.work_attribute_type
               LEFT JOIN work_attribute_type_allowed_value v ON v.id = wa.work_attribute_type_allowed_value
               WHERE wa.work IN (SELECT unnest(?::INTEGER[])) ORDER BY wa.rowid`,
              [entityIds],
            )
          : [],
      ]);
      const langs = grouped(languages.map((l) => [l.work, l.code] as const));
      const at = grouped(attrs.map((a) => [a.work, { type: a.type, value: a.value }] as const));
      for (const [id, e] of out) {
        e.iswcs = iswcs.get(id) ?? [];
        e.languages = langs.get(id) ?? [];
        e.attributes = at.get(id) ?? [];
      }
      break;
    }
    case "release_group": {
      const [c, primary, secondary, firsts] = await Promise.all([
        credits(),
        lookup(db, "release_group_primary_type"),
        db.query<{ release_group: number; name: string }>(
          `SELECT j.release_group, t.name FROM release_group_secondary_type_join j
           JOIN release_group_secondary_type t ON t.id = j.secondary_type
           WHERE j.release_group IN (SELECT unnest(?::INTEGER[])) ORDER BY t.child_order, t.id`,
          [entityIds],
        ),
        db.query<{ release_group: number; first: string | null }>(
          `SELECT r.release_group, min(mb_partial_date(e.date_year, e.date_month, e.date_day)) AS first
           FROM release r JOIN (SELECT release, date_year, date_month, date_day FROM release_country
                                UNION ALL SELECT release, date_year, date_month, date_day FROM release_unknown_country) e
             ON e.release = r.id
           WHERE r.release_group IN (SELECT unnest(?::INTEGER[])) GROUP BY r.release_group`,
          [entityIds],
        ),
      ]);
      const sec = grouped(secondary.map((s) => [s.release_group, s.name] as const));
      const first = new Map(firsts.map((f) => [f.release_group, f.first]));
      for (const r of rows) {
        const e = out.get(r.id as number)!;
        e.primaryType = primary.get(r.type as number) ?? null;
        delete e.type;
        e.secondaryTypes = sec.get(r.id as number) ?? [];
        e.artistCredit = c.get(r.artist_credit as number) ?? null;
        e.firstReleaseDate = first.get(r.id as number) ?? null;
      }
      if (include.includes("releases")) {
        const releases = await db.rowsOf<{ id: number }>("release", "release_group", entityIds, { columns: "id", orderBy: "id" });
        const all = await loadEntities(db, "release", [...releases.values()].flat().map((r) => r.id), []);
        for (const [rg, list] of releases) {
          const e = out.get(rg);
          if (e)
            e.releases = list.flatMap((r) => {
              const rel = all.get(r.id);
              if (!rel) return [];
              const { releaseGroup: _, ...rest } = rel;
              return [rest];
            });
        }
      }
      break;
    }
    case "release": {
      const [c, status, packaging, languages, scripts, groups, events, labels] = await Promise.all([
        credits(),
        lookup(db, "release_status"),
        lookup(db, "release_packaging"),
        lookup(db, "language", "iso_code_3"),
        lookup(db, "script", "iso_code"),
        db.byId<{ gid: string; name: string; type: number | null }>("release_group", ids(rows, "release_group"), {
          columns: "id, CAST(gid AS VARCHAR) AS gid, name, type",
        }),
        db.query<{ release: number; date: string | null; area: number | null }>(
          `SELECT release, mb_partial_date(date_year, date_month, date_day) AS date, country AS area, rowid AS o FROM release_country
           WHERE release IN (SELECT unnest(?::INTEGER[]))
           UNION ALL SELECT release, mb_partial_date(date_year, date_month, date_day), NULL, rowid FROM release_unknown_country
           WHERE release IN (SELECT unnest(?::INTEGER[])) ORDER BY release, date NULLS LAST, o`,
          [entityIds, entityIds],
        ),
        db.query<{ release: number; catalog_number: string | null; id: number | null; gid: string | null; name: string | null }>(
          `SELECT rl.release, rl.catalog_number, l.id, CAST(l.gid AS VARCHAR) AS gid, l.name FROM release_label rl
           LEFT JOIN label l ON l.id = rl.label WHERE rl.release IN (SELECT unnest(?::INTEGER[])) ORDER BY rl.rowid`,
          [entityIds],
        ),
      ]);
      const primary = await lookup(db, "release_group_primary_type");
      const areas = await areaSummaries(db, ids(events as unknown as Row[], "area"));
      const ev = grouped(events.map((x) => [x.release, { date: x.date, area: x.area != null ? (areas.get(x.area) ?? null) : null }] as const));
      const lab = grouped(
        labels.map((l) => [
          l.release,
          { catalogNumber: l.catalog_number || null, label: l.id != null ? { id: l.id, gid: l.gid!, name: l.name! } : null },
        ] as const),
      );
      for (const r of rows) {
        const e = out.get(r.id as number)!;
        const g = groups.get(r.release_group as number);
        e.status = status.get(r.status as number) ?? null;
        e.packaging = packaging.get(r.packaging as number) ?? null;
        e.language = languages.get(r.language as number) ?? null;
        e.script = scripts.get(r.script as number) ?? null;
        e.barcode = (r.barcode as string | null) ?? null;
        e.quality = r.quality;
        e.releaseGroup = g
          ? { id: r.release_group as number, gid: g.gid, name: g.name, primaryType: primary.get(g.type as number) ?? null }
          : null;
        e.artistCredit = c.get(r.artist_credit as number) ?? null;
        const list = ev.get(r.id as number) ?? [];
        e.releaseEvents = list;
        e.date = list.map((x) => x.date).filter((d): d is string => d != null).sort()[0] ?? null;
        e.labels = lab.get(r.id as number) ?? [];
      }
      if (include.includes("media")) {
        const formats = await lookup(db, "medium_format");
        const media = await db.query<Row>(
          `SELECT id, CAST(gid AS VARCHAR) AS gid, release, position, name, format, track_count FROM medium
           WHERE release IN (SELECT unnest(?::INTEGER[])) ORDER BY release, position`,
          [entityIds],
        );
        const tracks = await db.query<Row>(
          `SELECT t.id, CAST(t.gid AS VARCHAR) AS gid, t.medium, t.position, t.number, t.name, t.length, t.artist_credit,
                  t.is_data_track, rec.id AS rec_id, CAST(rec.gid AS VARCHAR) AS rec_gid, rec.name AS rec_name,
                  rec.length AS rec_length, rec.video AS rec_video
           FROM track t JOIN recording rec ON rec.id = t.recording
           WHERE t.medium IN (SELECT unnest(?::INTEGER[])) ORDER BY t.medium, t.position`,
          [ids(media, "id")],
        );
        const tc = await db.credits(ids(tracks, "artist_credit"));
        const byMedium = grouped(
          tracks.map((t) => [
            t.medium as number,
            {
              id: t.id,
              gid: t.gid,
              position: t.position,
              number: t.number,
              name: t.name,
              length: t.length ?? null,
              artistCredit: tc.get(t.artist_credit as number) ?? null,
              recording: { id: t.rec_id, gid: t.rec_gid, name: t.rec_name, length: t.rec_length ?? null, video: t.rec_video === true },
              isDataTrack: t.is_data_track === true,
            } as Track,
          ] as const),
        );
        const byRelease = grouped(
          media.map((m) => [
            m.release as number,
            {
              id: m.id,
              gid: m.gid,
              position: m.position,
              name: m.name,
              format: formats.get(m.format as number) ?? null,
              trackCount: m.track_count,
              tracks: byMedium.get(m.id as number) ?? [],
            } as Medium,
          ] as const),
        );
        for (const [id, e] of out) e.media = byRelease.get(id) ?? [];
      }
      break;
    }
    case "genre":
      break;
    default:
      throw new MusicBrainzError(`not an entity type: ${String(type)}`, "invalid");
  }

  // What any type may include.
  if (include.includes("aliases")) {
    const aliases = await db.aliases(type, entityIds);
    for (const [id, e] of out) e.aliases = aliases.get(id) ?? [];
  }
  if (include.includes("relationships")) {
    const rels = await db.relationships(type, entityIds);
    for (const [id, e] of out) e.relationships = rels.get(id) ?? [];
  } else if (include.includes("urls")) {
    const urls = await db.urls(type, entityIds);
    for (const [id, e] of out) e.urls = (urls.get(id) ?? []).map(({ type: t, url, ended }) => ({ type: t, url, ended }));
  }
  if (include.includes("relationships") && include.includes("urls"))
    for (const e of out.values())
      e.urls = (e.relationships ?? [])
        .filter((r) => r.targetType === "url")
        .map((r) => ({ type: r.type, url: r.target.name ?? "", ended: r.ended }));
  if (include.includes("redirects") && db.hasTable(`${type}_gid_redirect`)) {
    const red = await db.rowsOf<{ g: string }>(`${type}_gid_redirect`, "new_id", entityIds, {
      columns: "CAST(gid AS VARCHAR) AS g",
      orderBy: "gid",
    });
    for (const [id, e] of out) e.redirects = (red.get(id) ?? []).map((r) => r.g);
  }
  if (include.includes("identifiers") && (type === "artist" || type === "label")) {
    const [ipis, isnis] = await Promise.all([
      codesOf(db, `${type}_ipi`, "ipi", type, entityIds),
      codesOf(db, `${type}_isni`, "isni", type, entityIds),
    ]);
    for (const [id, e] of out) {
      e.ipis = ipis.get(id) ?? [];
      e.isnis = isnis.get(id) ?? [];
    }
  }

  const result = new Map<number | string, EntityOf<E>>();
  for (const [id, e] of out) for (const k of keyOf.get(id) ?? []) result.set(k, e as unknown as EntityOf<E>);
  return result;
}
