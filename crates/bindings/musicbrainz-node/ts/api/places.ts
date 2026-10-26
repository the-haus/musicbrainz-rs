// Labels, areas, places, events, series and genres: a label's catalogue and
// imprints, the area tree, what is near a point, a venue's events, an
// event's lineup, a festival's editions, and the genre taxonomy.

import type { Queries } from "../database.js";
import type { Area, EntityInclude, Event, Genre, Label, Place, Series } from "../entities.js";
import { type EntityType, MusicBrainzError } from "../types.js";
import { type ArtistRef, type ReleaseSummary, releaseSummaries } from "./artists.js";
import { checkDate, dateOf, type EntityKey, has, idOf, inStrings, need, type PageOptions, pageSql, type Ref } from "./common.js";
import type { ExternalIds } from "./external.js";

/** An event as a list gives it. */
export interface EventSummary extends Ref {
  disambiguation: string | null;
  type: string | null;
  begin: string | null;
  end: string | null;
  time: string | null;
  cancelled: boolean;
}

/** A performer of an event. */
export interface Performer {
  artist: ArtistRef;
  /** `main performer`, `support act`, `guest performer`, `supporting DJ`, … */
  role: string;
  /** Its place in the bill (`link_order`; 0: unordered). */
  order: number;
  /** The name it is billed as, when another. */
  credit: string | null;
}

/** An item of a series (an event of a festival, a release of a box set, …). */
export interface SeriesItem extends Ref {
  type: EntityType;
  /** Its number in the series as MusicBrainz gives it (`2019`, `Vol. 3`), when it has one. */
  number: string | null;
  order: number;
}

/** An area in a list, with its type. */
export interface AreaRef extends Ref {
  type: string | null;
}

/** A place near a point. */
export interface NearbyPlace extends Ref {
  type: string | null;
  address: string;
  latitude: number;
  longitude: number;
  /** Kilometres from the point. */
  distanceKm: number;
}

const eventColumns = (db: Queries, e = "e") => `${e}.id, CAST(${e}.gid AS VARCHAR) AS gid, ${e}.name, nullif(${e}.comment, '') AS disambiguation,
  ${has(db, "event_type") ? "et.name" : "NULL"} AS type, ${dateOf(e, "begin_date")} AS begin, ${dateOf(e, "end_date")} AS "end",
  CAST(${e}.time AS VARCHAR) AS time, coalesce(${e}.cancelled, false) AS cancelled`;
const eventTypeJoin = (db: Queries, e = "e") => (has(db, "event_type") ? `LEFT JOIN event_type et ON et.id = ${e}.type` : "");

/** The date filters of event lists. */
function eventDates(from: string | undefined, to: string | undefined, e = "e"): { where: string[]; params: unknown[] } {
  const where: string[] = [];
  const params: unknown[] = [];
  if (checkDate(from, "from")) {
    where.push(`${dateOf(e, "begin_date")} >= ?`);
    params.push(from);
  }
  if (checkDate(to, "to")) {
    where.push(`${dateOf(e, "begin_date")} <= ? || '-99'`);
    params.push(to);
  }
  return { where, params };
}

/** Labels (`db.labels`). */
export class LabelsApi {
  constructor(private readonly db: Queries) {}

  /** A label. */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Label | undefined> {
    return this.db.entity("label", key, include);
  }

  /** Labels by label code (`LC 0542`, `542`). */
  async byLabelCode(code: string | number): Promise<Label[]> {
    need(this.db, "byLabelCode", "label");
    const n = Number(String(code).replace(/^\s*LC[\s-]*/i, ""));
    if (!Number.isInteger(n) || n <= 0) throw new MusicBrainzError(`not a label code: ${code}`, "invalid");
    const ids = (await this.db.query<{ id: number }>("SELECT id FROM label WHERE label_code = ? ORDER BY id", [n])).map((r) => r.id);
    return [...(await this.db.entities("label", ids)).values()];
  }

  /** A label's releases, oldest first, with the catalogue number each has on it. */
  async releases(key: EntityKey, options: PageOptions & { status?: string[] } = {}): Promise<(ReleaseSummary & { catalogNumber: string | null })[]> {
    need(this.db, "releases", "release_label", "release", "artist_credit");
    const id = await idOf(this.db, "label", key);
    if (id === undefined) return [];
    const list = await releaseSummaries(this.db, "SELECT DISTINCT release AS id FROM release_label WHERE label = ?", [id], options);
    const cats = new Map(
      (
        await this.db.query<{ release: number; c: string | null }>(
          `SELECT release, min(nullif(catalog_number, '')) AS c FROM release_label WHERE label = ? AND release IN (SELECT unnest(?::INTEGER[])) GROUP BY release`,
          [id, list.map((r) => r.id)],
        )
      ).map((r) => [r.release, r.c]),
    );
    return list.map((r) => ({ ...r, catalogNumber: cats.get(r.id) ?? null }));
  }

  /** The artists with the most releases on a label. */
  async artists(key: EntityKey, options: PageOptions = {}): Promise<(ArtistRef & { releases: number })[]> {
    need(this.db, "artists", "release_label", "release", "artist_credit_name");
    const id = await idOf(this.db, "label", key);
    if (id === undefined) return [];
    const rows = await this.db.query<{ artist: number; releases: number }>(
      `SELECT acn.artist, count(DISTINCT r.id) AS releases FROM release_label rl JOIN release r ON r.id = rl.release
       JOIN artist_credit_name acn ON acn.artist_credit = r.artist_credit
       WHERE rl.label = ? GROUP BY acn.artist ORDER BY releases DESC, acn.artist ${pageSql(options, 50)}`,
      [id],
    );
    const refs = await this.db.artists.refs(rows.map((r) => r.artist));
    return rows.flatMap((r) => {
      const a = refs.get(r.artist);
      return a ? [{ ...a, releases: Number(r.releases) }] : [];
    });
  }

  /** The labels a label is an imprint of, or owned by. */
  parents(key: EntityKey): Promise<(Ref & { relationship: string })[]> {
    return this.family(key, "backward");
  }

  /** A label's imprints and the labels it owns. */
  imprints(key: EntityKey): Promise<(Ref & { relationship: string })[]> {
    return this.family(key, "forward");
  }

  private async family(key: EntityKey, direction: "forward" | "backward"): Promise<(Ref & { relationship: string })[]> {
    if (!has(this.db, "l_label_label", "link", "link_type")) return [];
    const id = await idOf(this.db, "label", key);
    if (id === undefined) return [];
    const rels = (await this.db.relationships("label", [id], { direction, targetTypes: ["label"], attributes: false })).get(id) ?? [];
    return rels
      .filter((r) => r.type === "imprint" || r.type === "label ownership" || r.type === "label rename" || r.type === "label reissue")
      .map((r) => ({ id: r.target.id, gid: r.target.gid ?? "", name: r.target.name ?? "", relationship: r.type }));
  }

  /** A label's URLs. */
  async urls(key: EntityKey): Promise<{ type: string; url: string; ended: boolean }[]> {
    const id = await idOf(this.db, "label", key);
    if (id === undefined || !has(this.db, "l_label_url")) return [];
    return ((await this.db.urls("label", [id])).get(id) ?? []).map(({ type, url, ended }) => ({ type, url, ended }));
  }

  /** Who a label is elsewhere. */
  externalIds(key: EntityKey): Promise<ExternalIds> {
    return this.db.external.ids("label", key);
  }
}

/** Areas (`db.areas`): countries, subdivisions, cities, districts. */
export class AreasApi {
  constructor(private readonly db: Queries) {}

  /** An area. */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Area | undefined> {
    return this.db.entity("area", key, include);
  }

  /** The area of an ISO 3166 code: a country (`NL`), a subdivision (`NL-NH`), a former country (`SUHH`). */
  async byIsoCode(code: string): Promise<Area | undefined> {
    const c = code.trim().toUpperCase();
    const tables = ["iso_3166_1", "iso_3166_2", "iso_3166_3"].filter((t) => this.db.hasTable(t));
    if (!c || !tables.length) return undefined;
    const row = await this.db.one<{ area: number }>(
      tables.map((t) => `SELECT area FROM ${t} WHERE code = ?`).join(" UNION ALL ") + " LIMIT 1",
      tables.map(() => c),
    );
    return row ? this.db.entity("area", row.area) : undefined;
  }

  /** The ISO 3166-1 code of the country an area is in (its own for a country). */
  async country(key: EntityKey): Promise<string | null> {
    const id = await idOf(this.db, "area", key);
    if (id === undefined || !has(this.db, "iso_3166_1", "l_area_area", "link", "link_type")) return null;
    return (await this.db.areaCountries([id])).get(id) ?? null;
  }

  /** The areas an area is part of, nearest first: a town's municipality, province, country, … */
  async ancestors(key: EntityKey): Promise<AreaRef[]> {
    if (!has(this.db, "l_area_area", "link", "link_type")) return [];
    const id = await idOf(this.db, "area", key);
    if (id === undefined) return [];
    const types = has(this.db, "area_type");
    return this.db.query<AreaRef>(
      `WITH RECURSIVE parent AS (
         SELECT r.entity1 AS child, arg_min(r.entity0, r.id) AS parent FROM l_area_area r
         JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type WHERE lt.name = 'part of' GROUP BY r.entity1),
       up(area, depth) AS (
         SELECT p.parent, 1 FROM parent p WHERE p.child = ?
         UNION ALL SELECT p.parent, up.depth + 1 FROM up JOIN parent p ON p.child = up.area WHERE up.depth < 24)
       SELECT a.id, CAST(a.gid AS VARCHAR) AS gid, a.name, ${types ? "t.name" : "NULL"} AS type
       FROM up JOIN area a ON a.id = up.area ${types ? "LEFT JOIN area_type t ON t.id = a.type" : ""} ORDER BY up.depth`,
      [id],
    );
  }

  /** The areas directly part of an area, by name (`type`: `City`, `Subdivision`, `Municipality`, …). */
  async children(key: EntityKey, options: PageOptions & { type?: string } = {}): Promise<AreaRef[]> {
    if (!has(this.db, "l_area_area", "link", "link_type")) return [];
    const id = await idOf(this.db, "area", key);
    if (id === undefined) return [];
    const types = has(this.db, "area_type");
    const params: unknown[] = [id];
    let typeFilter = "";
    if (options.type) {
      if (!types) return [];
      typeFilter = "AND t.name = ?";
      params.push(options.type);
    }
    return this.db.query<AreaRef>(
      `SELECT a.id, CAST(a.gid AS VARCHAR) AS gid, a.name, ${types ? "t.name" : "NULL"} AS type
       FROM l_area_area r JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type JOIN area a ON a.id = r.entity1
       ${types ? "LEFT JOIN area_type t ON t.id = a.type" : ""}
       WHERE lt.name = 'part of' AND r.entity0 = ? ${typeFilter}
       ORDER BY a.name, a.id ${pageSql(options, 1_000)}`,
      params as never,
    );
  }

  /** The SQL of an area and (with `deep`) every area inside it, as `area_ids(id)`. */
  private within(deep: boolean): string {
    return deep && has(this.db, "l_area_area", "link", "link_type")
      ? `WITH RECURSIVE area_ids(id, depth) AS (
           SELECT ?::INTEGER, 0
           UNION ALL SELECT r.entity1, d.depth + 1 FROM area_ids d JOIN l_area_area r ON r.entity0 = d.id
           JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type WHERE lt.name = 'part of' AND d.depth < 12)`
      : "WITH area_ids(id) AS (SELECT ?::INTEGER)";
  }

  /** The artists of an area (their area or begin area), by name; `deep`: of the areas inside it too. */
  async artists(key: EntityKey, options: PageOptions & { deep?: boolean } = {}): Promise<ArtistRef[]> {
    need(this.db, "artists", "artist");
    const id = await idOf(this.db, "area", key);
    if (id === undefined) return [];
    const rows = await this.db.query<{ id: number }>(
      `${this.within(options.deep ?? false)}
       SELECT a.id FROM artist a WHERE a.area IN (SELECT id FROM area_ids) OR a.begin_area IN (SELECT id FROM area_ids)
       ORDER BY a.sort_name, a.id ${pageSql(options, 1_000)}`,
      [id],
    );
    const refs = await this.db.artists.refs(rows.map((r) => r.id));
    return rows.flatMap((r) => refs.get(r.id) ?? []);
  }

  /** The labels of an area, by name; `deep`: of the areas inside it too. */
  async labels(key: EntityKey, options: PageOptions & { deep?: boolean } = {}): Promise<Ref[]> {
    need(this.db, "labels", "label");
    const id = await idOf(this.db, "area", key);
    if (id === undefined) return [];
    return this.db.query<Ref>(
      `${this.within(options.deep ?? false)}
       SELECT l.id, CAST(l.gid AS VARCHAR) AS gid, l.name FROM label l WHERE l.area IN (SELECT id FROM area_ids)
       ORDER BY l.name, l.id ${pageSql(options, 1_000)}`,
      [id],
    );
  }

  /** The places of an area, by name; `deep`: of the areas inside it too. */
  async places(key: EntityKey, options: PageOptions & { deep?: boolean } = {}): Promise<Ref[]> {
    need(this.db, "places", "place");
    const id = await idOf(this.db, "area", key);
    if (id === undefined) return [];
    return this.db.query<Ref>(
      `${this.within(options.deep ?? false)}
       SELECT p.id, CAST(p.gid AS VARCHAR) AS gid, p.name FROM place p WHERE p.area IN (SELECT id FROM area_ids)
       ORDER BY p.name, p.id ${pageSql(options, 1_000)}`,
      [id],
    );
  }
}

/** Places (`db.places`): venues, studios, stadiums, … */
export class PlacesApi {
  constructor(private readonly db: Queries) {}

  /** A place. */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Place | undefined> {
    return this.db.entity("place", key, include);
  }

  /** The events held at a place, newest first (`from`, `to`: partial dates). */
  async events(key: EntityKey, options: PageOptions & { from?: string; to?: string } = {}): Promise<EventSummary[]> {
    if (!has(this.db, "l_event_place", "event")) return [];
    const id = await idOf(this.db, "place", key);
    if (id === undefined) return [];
    const d = eventDates(options.from, options.to);
    return this.db.query<EventSummary>(
      `SELECT DISTINCT ${eventColumns(this.db)} FROM l_event_place r JOIN event e ON e.id = r.entity0 ${eventTypeJoin(this.db)}
       WHERE r.entity1 = ? ${d.where.map((w) => `AND ${w}`).join(" ")}
       ORDER BY begin DESC NULLS LAST, e.id ${pageSql(options, 100)}`,
      [id, ...d.params] as never,
    );
  }

  /** The places within `radiusKm` (default 10) of a point, nearest first. */
  async near(latitude: number, longitude: number, options: { radiusKm?: number; limit?: number; type?: string } = {}): Promise<NearbyPlace[]> {
    need(this.db, "near", "place");
    const r = options.radiusKm ?? 10;
    if (!(Math.abs(latitude) <= 90) || !(Math.abs(longitude) <= 180)) throw new MusicBrainzError("not a point on Earth", "invalid");
    if (!(r > 0 && r <= 20_000)) throw new MusicBrainzError("radiusKm must be from 0 to 20,000", "invalid");
    const dLat = r / 111.0;
    const dLon = r / (111.0 * Math.max(0.01, Math.cos((latitude * Math.PI) / 180)));
    const types = has(this.db, "place_type");
    const params: unknown[] = [latitude, longitude, latitude - dLat, latitude + dLat];
    let lonFilter = "";
    if (longitude - dLon >= -180 && longitude + dLon <= 180) {
      lonFilter = "AND lon BETWEEN ? AND ?";
      params.push(longitude - dLon, longitude + dLon);
    }
    let typeFilter = "";
    if (options.type) {
      if (!types) return [];
      typeFilter = "AND t.name = ?";
      params.push(options.type);
    }
    params.push(r);
    const rows = await this.db.query<NearbyPlace & { km: number }>(
      `WITH pt AS (SELECT ?::DOUBLE AS lat0, ?::DOUBLE AS lon0),
       p AS (SELECT p.*, p.coordinates[1] AS lat, p.coordinates[2] AS lon FROM place p WHERE p.coordinates IS NOT NULL)
       SELECT * FROM (
         SELECT p.id, CAST(p.gid AS VARCHAR) AS gid, p.name, ${types ? "t.name" : "NULL"} AS type, p.address,
                p.lat AS latitude, p.lon AS longitude,
                2 * 6371.0088 * asin(sqrt(pow(sin(radians(p.lat - pt.lat0) / 2), 2)
                  + cos(radians(pt.lat0)) * cos(radians(p.lat)) * pow(sin(radians(p.lon - pt.lon0) / 2), 2))) AS km
         FROM p CROSS JOIN pt ${types ? "LEFT JOIN place_type t ON t.id = p.type" : ""}
         WHERE lat BETWEEN ? AND ? ${lonFilter} ${typeFilter})
       WHERE km <= ? ORDER BY km, id ${pageSql({ limit: options.limit ?? 25 }, 25, 1_000)}`,
      params as never,
    );
    return rows.map(({ km, ...p }) => ({ ...p, distanceKm: Math.round(km * 1000) / 1000 }));
  }

  /** A place's URLs. */
  async urls(key: EntityKey): Promise<{ type: string; url: string; ended: boolean }[]> {
    const id = await idOf(this.db, "place", key);
    if (id === undefined || !has(this.db, "l_place_url")) return [];
    return ((await this.db.urls("place", [id])).get(id) ?? []).map(({ type, url, ended }) => ({ type, url, ended }));
  }

  /** Who a place is elsewhere (its Wikidata item, OpenStreetMap node, Songkick venue, …). */
  externalIds(key: EntityKey): Promise<ExternalIds> {
    return this.db.external.ids("place", key);
  }
}

/** Events (`db.events`): concerts, festivals, … */
export class EventsApi {
  constructor(private readonly db: Queries) {}

  /** An event. */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Event | undefined> {
    return this.db.entity("event", key, include);
  }

  /** Who performed at an event, in the bill's order. */
  async lineup(key: EntityKey): Promise<Performer[]> {
    if (!has(this.db, "l_artist_event", "link", "link_type")) return [];
    const id = await idOf(this.db, "event", key);
    if (id === undefined) return [];
    const rows = await this.db.query<{ artist: number; role: string; order: number; credit: string | null }>(
      `SELECT r.entity0 AS artist, lt.name AS role, r.link_order AS "order", nullif(r.entity0_credit, '') AS credit
       FROM l_artist_event r JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type
       WHERE r.entity1 = ? ORDER BY CASE lt.name WHEN 'main performer' THEN 0 ELSE 1 END, r.link_order, r.rowid`,
      [id],
    );
    const refs = await this.db.artists.refs(rows.map((r) => r.artist));
    return rows.flatMap((r) => {
      const artist = refs.get(r.artist);
      return artist ? [{ artist, role: r.role, order: r.order, credit: r.credit }] : [];
    });
  }

  /** Where an event was held. */
  async place(key: EntityKey): Promise<Ref | undefined> {
    if (!has(this.db, "l_event_place", "place")) return undefined;
    const id = await idOf(this.db, "event", key);
    if (id === undefined) return undefined;
    return this.db.one<Ref>(
      `SELECT p.id, CAST(p.gid AS VARCHAR) AS gid, p.name FROM l_event_place r JOIN place p ON p.id = r.entity1
       WHERE r.entity0 = ? ORDER BY r.rowid LIMIT 1`,
      [id],
    );
  }

  /** The series an event is part of (a festival's), with its number in it. */
  async series(key: EntityKey): Promise<(Ref & { number: string | null })[]> {
    if (!has(this.db, "l_event_series", "series")) return [];
    const id = await idOf(this.db, "event", key);
    if (id === undefined) return [];
    const texts = has(this.db, "link_attribute_text_value");
    return this.db.query<Ref & { number: string | null }>(
      `SELECT s.id, CAST(s.gid AS VARCHAR) AS gid, s.name, ${texts ? "(SELECT min(v.text_value) FROM link_attribute_text_value v WHERE v.link = r.link)" : "NULL"} AS number
       FROM l_event_series r JOIN series s ON s.id = r.entity1 WHERE r.entity0 = ? ORDER BY r.rowid`,
      [id],
    );
  }

  /** The events of a period (partial dates), oldest first (`types`: `Concert`, `Festival`, …). */
  async between(from: string, to: string, options: PageOptions & { types?: string[] } = {}): Promise<EventSummary[]> {
    need(this.db, "between", "event");
    const d = eventDates(from, to);
    const params = [...d.params];
    let typeFilter = "";
    if (options.types?.length) {
      if (!has(this.db, "event_type")) return [];
      typeFilter = `AND et.name IN ${inStrings}`;
      params.push(options.types);
    }
    return this.db.query<EventSummary>(
      `SELECT ${eventColumns(this.db)} FROM event e ${eventTypeJoin(this.db)}
       WHERE ${d.where.join(" AND ")} ${typeFilter} ORDER BY begin, e.id ${pageSql(options, 1_000)}`,
      params as never,
    );
  }

  /** Who an event is elsewhere (its Songkick, setlist.fm, Wikidata pages, …). */
  externalIds(key: EntityKey): Promise<ExternalIds> {
    return this.db.external.ids("event", key);
  }
}

/** Series (`db.series`): festivals' editions, box sets, tours, award ceremonies, … */
export class SeriesApi {
  constructor(private readonly db: Queries) {}

  /** A series. */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Series | undefined> {
    return this.db.entity("series", key, include);
  }

  /** A series' items, in the series' order: by their number when they have one, else by `link_order`. */
  async items(key: EntityKey, options: PageOptions & { types?: EntityType[] } = {}): Promise<SeriesItem[]> {
    const id = await idOf(this.db, "series", key);
    if (id === undefined) return [];
    const rels = ((await this.db.relationships("series", [id], { attributes: true })).get(id) ?? []).filter(
      (r) => r.targetType !== "url" && r.targetType !== "series" && (!options.types || options.types.includes(r.targetType)),
    );
    const items = rels.map((r) => ({
      type: r.targetType,
      id: r.target.id,
      gid: r.target.gid ?? "",
      name: r.target.name ?? "",
      number: r.attributes.find((a) => a.type === "number")?.value ?? null,
      order: r.order,
    }));
    const num = (s: string | null) => (s == null ? Infinity : Number.parseFloat(s.replace(/[^0-9.]/g, "")) || Infinity);
    items.sort((a, b) => a.order - b.order || num(a.number) - num(b.number) || (a.number ?? "").localeCompare(b.number ?? "") || a.id - b.id);
    const offset = options.offset ?? 0;
    pageSql(options, 1_000);
    return items.slice(offset, offset + (options.limit ?? 1_000));
  }

  /** A series' events (a festival's editions), in the series' order. */
  async events(key: EntityKey): Promise<SeriesItem[]> {
    return this.items(key, { types: ["event"] });
  }
}

/** Genres (`db.genres`): MusicBrainz's genre taxonomy. */
export class GenresApi {
  constructor(private readonly db: Queries) {}

  /** Every genre, by name. */
  async all(): Promise<(Ref & { disambiguation: string | null })[]> {
    need(this.db, "genres", "genre");
    return this.db.query(`SELECT id, CAST(gid AS VARCHAR) AS gid, name, nullif(comment, '') AS disambiguation FROM genre ORDER BY name, id`);
  }

  /** A genre. */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Genre | undefined> {
    return this.db.entity("genre", key, include);
  }

  /** A genre by its name (case and accents ignored). */
  async byName(name: string): Promise<Genre | undefined> {
    const hit = (await this.db.search.genres(name, { mode: "exact", limit: 1 }))[0];
    return hit ? this.get(hit.id) : undefined;
  }

  /** The genres a genre is a subgenre of. */
  parents(key: EntityKey): Promise<Ref[]> {
    return this.links(key, "backward", "subgenre");
  }

  /** A genre's subgenres. */
  subgenres(key: EntityKey): Promise<Ref[]> {
    return this.links(key, "forward", "subgenre");
  }

  /** The genres a genre is a fusion of. */
  fusionOf(key: EntityKey): Promise<Ref[]> {
    return this.links(key, "forward", "fusion of");
  }

  /** Every genre↔genre link: `{ from, to, type }` (`subgenre`: `from` has the subgenre `to`). */
  async graph(): Promise<{ from: number; to: number; type: string }[]> {
    if (!has(this.db, "l_genre_genre", "link", "link_type")) return [];
    return this.db.query(
      `SELECT r.entity0 AS "from", r.entity1 AS "to", lt.name AS type FROM l_genre_genre r
       JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type ORDER BY r.id`,
    );
  }

  private async links(key: EntityKey, direction: "forward" | "backward", type: string): Promise<Ref[]> {
    if (!has(this.db, "l_genre_genre", "link", "link_type")) return [];
    const id = await idOf(this.db, "genre", key);
    if (id === undefined) return [];
    const rels = (await this.db.relationships("genre", [id], { direction, types: [type], targetTypes: ["genre"], attributes: false })).get(id) ?? [];
    return rels.map((r) => ({ id: r.target.id, gid: r.target.gid ?? "", name: r.target.name ?? "" }));
  }
}
