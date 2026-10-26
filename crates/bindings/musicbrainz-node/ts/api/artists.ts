// What is asked about artists: their discography, releases, recordings and
// works, their bands and members, who they work with, where they played,
// the labels they are on, where they are from, and who they are elsewhere.

import type { Alias, Queries } from "../database.js";
import type { Artist, EntityInclude } from "../entities.js";
import { MusicBrainzError } from "../types.js";
import {
  checkDate,
  dateOf,
  type EntityKey,
  has,
  idOf,
  inStrings,
  need,
  type PageOptions,
  pageSql,
  type Ref,
} from "./common.js";
import type { ExternalIds, UrlOwner } from "./external.js";
import type { SearchHit, SearchOptions } from "./search.js";

/** A release group as a discography lists it. */
export interface ReleaseGroupSummary extends Ref {
  disambiguation: string | null;
  primaryType: string | null;
  secondaryTypes: string[];
  /** The earliest date of any of its releases. */
  firstReleaseDate: string | null;
  /** Its artist credit as printed: `Daft Punk feat. Pharrell Williams`. */
  artistCredit: string;
}

/** A release as an artist's or label's list gives it. */
export interface ReleaseSummary extends Ref {
  disambiguation: string | null;
  status: string | null;
  /** Its earliest release event's date. */
  date: string | null;
  /** The ISO 3166-1 of its earliest dated release event (null: worldwide or unknown). */
  country: string | null;
  barcode: string | null;
  releaseGroup: Ref;
  artistCredit: string;
}

/** A recording as an artist's list gives it. */
export interface RecordingSummary extends Ref {
  disambiguation: string | null;
  /** Milliseconds. */
  length: number | null;
  video: boolean;
  artistCredit: string;
}

/** An artist as a list of others names it. */
export interface ArtistRef extends Ref {
  sortName: string;
  type: string | null;
  disambiguation: string | null;
}

/** A membership of a band. */
export interface Membership {
  /** The member (`members`) or the band (`memberOf`). */
  artist: ArtistRef;
  begin: string | null;
  end: string | null;
  ended: boolean;
  /** Its attributes: instruments, `vocal`, `original`, `additional`, … */
  roles: string[];
  /** The name the member is credited as in the band, when another. */
  credit: string | null;
}

/** A relationship to another artist. */
export interface ArtistRelation {
  artist: ArtistRef;
  /** `is person`, `collaboration`, `subgroup`, `founder`, `parent`, `sibling`, … */
  type: string;
  direction: "forward" | "backward";
  begin: string | null;
  end: string | null;
  ended: boolean;
}

/** A work an artist wrote (or arranged, translated, …). */
export interface ArtistWork extends Ref {
  disambiguation: string | null;
  type: string | null;
  /** How: `composer`, `lyricist`, `writer`, `arranger`, … */
  roles: string[];
}

/** An event an artist took part in. */
export interface ArtistEvent extends Ref {
  disambiguation: string | null;
  type: string | null;
  begin: string | null;
  end: string | null;
  time: string | null;
  cancelled: boolean;
  /** How: `main performer`, `support act`, `guest performer`, … */
  roles: string[];
  /** Where it was held. */
  place: Ref | null;
}

/** Options of {@link ArtistsApi.discography}. */
export interface DiscographyOptions extends PageOptions {
  /** Only these primary types (`Album`, `Single`, `EP`, `Broadcast`, `Other`). */
  primaryTypes?: string[];
  /**
   * Leave out groups of these secondary types (`Compilation`, `Live`,
   * `Remix`, `Soundtrack`, `Interview`, `Spokenword`, `DJ-mix`, …); `true`:
   * every group with a secondary type.
   */
  excludeSecondary?: boolean | string[];
  /** Only groups where the artist is credited first (default: anywhere in the credit). */
  leadOnly?: boolean;
  /** Only groups with an official release (default true). */
  officialOnly?: boolean;
}

/** The SQL of release groups' earliest release dates, for `ids` (a CTE named so). */
export const firstReleaseDates = (ids: string) => `
  SELECT r.release_group AS id, min(d) AS first FROM (
    SELECT rc.release, mb_partial_date(rc.date_year, rc.date_month, rc.date_day) AS d FROM release_country rc
    UNION ALL
    SELECT ru.release, mb_partial_date(ru.date_year, ru.date_month, ru.date_day) FROM release_unknown_country ru
  ) e JOIN release r ON r.id = e.release
  WHERE r.release_group IN (SELECT id FROM ${ids}) GROUP BY r.release_group`;

/** Artists (`db.artists`). */
export class ArtistsApi {
  constructor(private readonly db: Queries) {}

  /** An artist, as the web service gives it (see `entities`). */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Artist | undefined> {
    return this.db.entity("artist", key, include);
  }

  /** Artists by name (`db.search.artists`; default: exact, aliases included). */
  find(name: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.db.search.artists(name, { mode: "exact", ...options });
  }

  /** The artists an id elsewhere (`wikidata` `Q…`, `discogs` `artist/…`, `spotify` `artist/…`) is. */
  async byExternalId(...args: Parameters<Queries["external"]["find"]>): Promise<UrlOwner[]> {
    const [service, id, options] = args;
    return this.db.external.find(service, id, { ...options, types: ["artist"] });
  }

  /**
   * An artist's release groups: the albums, singles, EPs and others it is
   * credited on, oldest first.
   */
  async discography(key: EntityKey, options: DiscographyOptions = {}): Promise<ReleaseGroupSummary[]> {
    need(this.db, "discography", "artist_credit_name", "release_group", "artist_credit");
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    const page = pageSql(options, 1_000);
    const params: unknown[] = [id];
    const where: string[] = [];
    if (options.primaryTypes?.length) {
      where.push(`pt.name IN ${inStrings}`);
      params.push(options.primaryTypes);
    }
    const dated = has(this.db, "release", "release_country", "release_unknown_country");
    const secondary = has(this.db, "release_group_secondary_type_join", "release_group_secondary_type");
    if (options.excludeSecondary && secondary) {
      if (options.excludeSecondary === true) where.push("len(s.types) = 0");
      else {
        where.push(`NOT list_has_any(s.types, from_json(?, '["VARCHAR"]'))`);
        params.push(options.excludeSecondary);
      }
    }
    if (options.officialOnly !== false && has(this.db, "release", "release_status"))
      where.push(
        `EXISTS (SELECT 1 FROM release r JOIN release_status st ON st.id = r.status WHERE r.release_group = rg.id AND st.name = 'Official')`,
      );
    const rows = await this.db.query<ReleaseGroupSummary & { primary_type: string | null; first: string | null; credit: string; types: string[] | null }>(
      `WITH ids AS (
         SELECT DISTINCT rg.id FROM artist_credit_name acn JOIN release_group rg ON rg.artist_credit = acn.artist_credit
         WHERE acn.artist = ? ${options.leadOnly ? "AND acn.position = 0" : ""}),
       firsts AS (${dated ? firstReleaseDates("ids") : "SELECT NULL::INTEGER AS id, NULL::VARCHAR AS first WHERE false"}),
       s AS (${
         secondary
           ? `SELECT i.id, coalesce(list(st.name ORDER BY st.child_order, st.id) FILTER (WHERE st.name IS NOT NULL), []) AS types
              FROM ids i LEFT JOIN release_group_secondary_type_join j ON j.release_group = i.id
              LEFT JOIN release_group_secondary_type st ON st.id = j.secondary_type GROUP BY i.id`
           : "SELECT id, []::VARCHAR[] AS types FROM ids"
       })
       SELECT rg.id, CAST(rg.gid AS VARCHAR) AS gid, rg.name, nullif(rg.comment, '') AS disambiguation,
              pt.name AS primary_type, s.types, f.first, ac.name AS credit
       FROM ids JOIN release_group rg ON rg.id = ids.id JOIN artist_credit ac ON ac.id = rg.artist_credit
       LEFT JOIN release_group_primary_type pt ON pt.id = rg.type
       LEFT JOIN s ON s.id = rg.id LEFT JOIN firsts f ON f.id = rg.id
       ${where.length ? `WHERE ${where.join(" AND ")}` : ""}
       ORDER BY f.first NULLS LAST, rg.name, rg.id ${page}`,
      params as never,
    );
    return rows.map((r) => ({
      id: r.id,
      gid: r.gid,
      name: r.name,
      disambiguation: r.disambiguation,
      primaryType: r.primary_type,
      secondaryTypes: r.types ?? [],
      firstReleaseDate: r.first,
      artistCredit: r.credit,
    }));
  }

  /** An artist's releases (credited on the release itself), oldest first. */
  async releases(
    key: EntityKey,
    options: PageOptions & { status?: string[]; leadOnly?: boolean } = {},
  ): Promise<ReleaseSummary[]> {
    need(this.db, "releases", "artist_credit_name", "release", "release_group", "artist_credit");
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    return releaseSummaries(
      this.db,
      `SELECT DISTINCT r.id FROM artist_credit_name acn JOIN release r ON r.artist_credit = acn.artist_credit
       WHERE acn.artist = ? ${options.leadOnly ? "AND acn.position = 0" : ""}`,
      [id],
      options,
    );
  }

  /** An artist's recordings (credited on the recording), by name. */
  async recordings(key: EntityKey, options: PageOptions & { leadOnly?: boolean } = {}): Promise<RecordingSummary[]> {
    need(this.db, "recordings", "artist_credit_name", "recording", "artist_credit");
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    const rows = await this.db.query<RecordingSummary & { credit: string }>(
      `SELECT r.id, CAST(r.gid AS VARCHAR) AS gid, r.name, nullif(r.comment, '') AS disambiguation, r.length, r.video,
              ac.name AS credit
       FROM (SELECT DISTINCT r.id FROM artist_credit_name acn JOIN recording r ON r.artist_credit = acn.artist_credit
             WHERE acn.artist = ? ${options.leadOnly ? "AND acn.position = 0" : ""}) ids
       JOIN recording r ON r.id = ids.id JOIN artist_credit ac ON ac.id = r.artist_credit
       ORDER BY r.name, r.id ${pageSql(options, 1_000)}`,
      [id],
    );
    return rows.map(({ credit, ...r }) => ({ ...r, video: r.video === true, artistCredit: credit }));
  }

  /** The works an artist wrote, composed, arranged, … (`l_artist_work`), by name. */
  async works(key: EntityKey, options: PageOptions = {}): Promise<ArtistWork[]> {
    need(this.db, "works", "l_artist_work", "work", "link", "link_type");
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    return this.db.query<ArtistWork>(
      `SELECT w.id, CAST(w.gid AS VARCHAR) AS gid, w.name, nullif(w.comment, '') AS disambiguation,
              ${has(this.db, "work_type") ? "wt.name" : "NULL"} AS type, list(DISTINCT lt.name ORDER BY lt.name) AS roles
       FROM l_artist_work r JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type JOIN work w ON w.id = r.entity1
       ${has(this.db, "work_type") ? "LEFT JOIN work_type wt ON wt.id = w.type" : ""}
       WHERE r.entity0 = ?
       GROUP BY ALL ORDER BY w.name, w.id ${pageSql(options, 1_000)}`,
      [id],
    );
  }

  /** A group's members ("member of band"), in the order MusicBrainz lists them. */
  members(key: EntityKey, options: { current?: boolean } = {}): Promise<Membership[]> {
    return this.memberships(key, "backward", options);
  }

  /** The groups an artist is or was a member of. */
  memberOf(key: EntityKey, options: { current?: boolean } = {}): Promise<Membership[]> {
    return this.memberships(key, "forward", options);
  }

  private async memberships(key: EntityKey, direction: "forward" | "backward", options: { current?: boolean }): Promise<Membership[]> {
    if (!has(this.db, "l_artist_artist", "link", "link_type")) return [];
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    const rels = (await this.db.relationships("artist", [id], { direction, types: ["member of band"], targetTypes: ["artist"] })).get(id) ?? [];
    const kept = options.current ? rels.filter((r) => !r.ended) : rels;
    const refs = await this.refs(kept.map((r) => r.target.id));
    return kept.flatMap((r) => {
      const artist = refs.get(r.target.id);
      return artist
        ? [{ artist, begin: r.begin, end: r.end, ended: r.ended, roles: r.attributes.map((a) => a.type), credit: r.targetCredit ?? r.credit }]
        : [];
    });
  }

  /** An artist's other relationships to artists (not memberships): `is person`, `collaboration`, `subgroup`, … */
  async related(key: EntityKey, options: { types?: string[] } = {}): Promise<ArtistRelation[]> {
    if (!has(this.db, "l_artist_artist", "link", "link_type")) return [];
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    const rels = ((await this.db.relationships("artist", [id], { targetTypes: ["artist"], ...(options.types ? { types: options.types } : {}) })).get(id) ?? []).filter(
      (r) => options.types || r.type !== "member of band",
    );
    const refs = await this.refs(rels.map((r) => r.target.id));
    return rels.flatMap((r) => {
      const artist = refs.get(r.target.id);
      return artist ? [{ artist, type: r.type, direction: r.direction, begin: r.begin, end: r.end, ended: r.ended }] : [];
    });
  }

  /** The artists credited with this one most often (on recordings, releases and release groups). */
  async collaborators(key: EntityKey, options: PageOptions = {}): Promise<(ArtistRef & { credits: number })[]> {
    need(this.db, "collaborators", "artist_credit_name");
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    const rows = await this.db.query<{ id: number; credits: number }>(
      `SELECT o.artist AS id, count(DISTINCT o.artist_credit) AS credits
       FROM artist_credit_name me JOIN artist_credit_name o ON o.artist_credit = me.artist_credit AND o.artist <> me.artist
       WHERE me.artist = ? GROUP BY o.artist ORDER BY credits DESC, o.artist ${pageSql(options, 25)}`,
      [id],
    );
    const refs = await this.refs(rows.map((r) => r.id));
    return rows.flatMap((r) => {
      const a = refs.get(r.id);
      return a ? [{ ...a, credits: Number(r.credits) }] : [];
    });
  }

  /** The events an artist performed at (or took part in), newest first. */
  async events(key: EntityKey, options: PageOptions & { from?: string; to?: string; roles?: string[] } = {}): Promise<ArtistEvent[]> {
    if (!has(this.db, "l_artist_event", "event", "link", "link_type")) return [];
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    const from = checkDate(options.from, "from");
    const to = checkDate(options.to, "to");
    const params: unknown[] = [id];
    const where = ["r.entity0 = ?"];
    if (options.roles?.length) {
      where.push(`lt.name IN ${inStrings}`);
      params.push(options.roles);
    }
    if (from) {
      where.push(`${dateOf("e", "begin_date")} >= ?`);
      params.push(from);
    }
    if (to) {
      where.push(`${dateOf("e", "begin_date")} <= ? || '-99'`);
      params.push(to);
    }
    const places = has(this.db, "l_event_place", "place");
    const rows = await this.db.query<ArtistEvent & { place_id: number | null; place_gid: string | null; place_name: string | null }>(
      `SELECT e.id, CAST(e.gid AS VARCHAR) AS gid, e.name, nullif(e.comment, '') AS disambiguation,
              ${has(this.db, "event_type") ? "et.name" : "NULL"} AS type,
              ${dateOf("e", "begin_date")} AS begin, ${dateOf("e", "end_date")} AS "end",
              CAST(e.time AS VARCHAR) AS time, e.cancelled, list(DISTINCT lt.name ORDER BY lt.name) AS roles,
              ${places ? "any_value(p.id)" : "NULL"} AS place_id, ${places ? "any_value(CAST(p.gid AS VARCHAR))" : "NULL"} AS place_gid,
              ${places ? "any_value(p.name)" : "NULL"} AS place_name
       FROM l_artist_event r JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type JOIN event e ON e.id = r.entity1
       ${has(this.db, "event_type") ? "LEFT JOIN event_type et ON et.id = e.type" : ""}
       ${places ? "LEFT JOIN l_event_place ep ON ep.entity0 = e.id LEFT JOIN place p ON p.id = ep.entity1" : ""}
       WHERE ${where.join(" AND ")}
       GROUP BY ALL
       ORDER BY begin DESC NULLS LAST, e.id ${pageSql(options, 100)}`,
      params as never,
    );
    return rows.map(({ place_id, place_gid, place_name, ...e }) => ({
      ...e,
      cancelled: e.cancelled === true,
      place: place_id != null ? { id: place_id, gid: place_gid!, name: place_name! } : null,
    }));
  }

  /** The labels an artist's releases came out on, the most releases first. */
  async labels(key: EntityKey, options: PageOptions = {}): Promise<(Ref & { releases: number })[]> {
    need(this.db, "labels", "artist_credit_name", "release", "release_label", "label");
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    return this.db.query<Ref & { releases: number }>(
      `SELECT l.id, CAST(l.gid AS VARCHAR) AS gid, l.name, count(DISTINCT r.id) AS releases
       FROM artist_credit_name acn JOIN release r ON r.artist_credit = acn.artist_credit
       JOIN release_label rl ON rl.release = r.id JOIN label l ON l.id = rl.label
       WHERE acn.artist = ? AND ${text("l.gid")} <> '157afde4-4bf5-4039-8ad2-5a15acc85176'
       GROUP BY l.id, l.gid, l.name ORDER BY releases DESC, l.id ${pageSql(options, 50)}`,
      [id],
    );
  }

  /** The ISO 3166-1 code of the country an artist is from (its area's, else its begin area's). */
  async country(key: EntityKey): Promise<string | null> {
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return null;
    const row = (await this.db.byId<{ area: number | null; begin_area: number | null }>("artist", [id], { columns: "area, begin_area" })).get(id);
    if (!row) return null;
    const areas = [row.area, row.begin_area].filter((a): a is number => a != null);
    if (!areas.length || !has(this.db, "iso_3166_1")) return null;
    const countries = has(this.db, "l_area_area", "link", "link_type")
      ? await this.db.areaCountries(areas)
      : new Map(
          (await this.db.query<{ area: number; code: string }>(`SELECT area, code FROM iso_3166_1 WHERE area IN (SELECT unnest(?::INTEGER[]))`, [areas])).map((r) => [r.area, r.code]),
        );
    for (const a of areas) {
      const c = countries.get(a);
      if (c) return c;
    }
    return null;
  }

  /** An artist's URLs: `{ type, url, ended }` in the dump's order. */
  async urls(key: EntityKey): Promise<{ type: string; url: string; ended: boolean }[]> {
    if (!has(this.db, "l_artist_url", "url")) return [];
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    return ((await this.db.urls("artist", [id])).get(id) ?? []).map(({ type, url, ended }) => ({ type, url, ended }));
  }

  /** Who an artist is elsewhere: its Wikidata item, Discogs, Spotify, … ids. */
  externalIds(key: EntityKey): Promise<ExternalIds> {
    return this.db.external.ids("artist", key);
  }

  /** An artist's aliases. */
  async aliases(key: EntityKey): Promise<Alias[]> {
    const id = await idOf(this.db, "artist", key);
    if (id === undefined) return [];
    return (await this.db.aliases("artist", [id])).get(id) ?? [];
  }

  /** How much there is of an artist: its release groups, releases, recordings, works and events. */
  async counts(key: EntityKey): Promise<{ releaseGroups: number; releases: number; recordings: number; works: number; events: number } | undefined> {
    const id = await idOf(this.db, "artist", key);
    if (id === undefined || !(await this.db.byId("artist", [id], { columns: "id" })).size) return undefined;
    const count = (sql: string, ...tables: string[]) => (has(this.db, ...tables) ? `(${sql})` : "0");
    const credited = (table: string) =>
      count(
        `SELECT count(DISTINCT x.id) FROM artist_credit_name acn JOIN ${table} x ON x.artist_credit = acn.artist_credit WHERE acn.artist = $1`,
        "artist_credit_name",
        table,
      );
    const row = await this.db.one<Record<string, number>>(
      `SELECT ${credited("release_group")} AS release_groups, ${credited("release")} AS releases, ${credited("recording")} AS recordings,
              ${count("SELECT count(DISTINCT entity1) FROM l_artist_work WHERE entity0 = $1", "l_artist_work")} AS works,
              ${count("SELECT count(DISTINCT entity1) FROM l_artist_event WHERE entity0 = $1", "l_artist_event")} AS events,
              $1 AS id`,
      [id],
    );
    return {
      releaseGroups: Number(row?.release_groups ?? 0),
      releases: Number(row?.releases ?? 0),
      recordings: Number(row?.recordings ?? 0),
      works: Number(row?.works ?? 0),
      events: Number(row?.events ?? 0),
    };
  }

  /** Artists as lists name them, by id. */
  async refs(ids: Iterable<number>): Promise<Map<number, ArtistRef>> {
    const list = [...new Set(ids)];
    if (!list.length) return new Map();
    const types = has(this.db, "artist_type");
    const rows = await this.db.query<ArtistRef & { sort_name: string }>(
      `SELECT a.id, CAST(a.gid AS VARCHAR) AS gid, a.name, a.sort_name, ${types ? "t.name" : "NULL"} AS type,
              nullif(a.comment, '') AS disambiguation
       FROM artist a ${types ? "LEFT JOIN artist_type t ON t.id = a.type" : ""} WHERE a.id IN (SELECT unnest(?::INTEGER[]))`,
      [list],
    );
    return new Map(
      rows.map(({ sort_name, ...a }) => [a.id, { id: a.id, gid: a.gid, name: a.name, sortName: sort_name, type: a.type, disambiguation: a.disambiguation }]),
    );
  }
}

const text = (expr: string) => `lower(CAST(${expr} AS VARCHAR))`;

/** Releases from a SQL of their ids (with `params`), oldest first, paged. */
export async function releaseSummaries(
  db: Queries,
  idsSql: string,
  params: unknown[],
  options: PageOptions & { status?: string[] } = {},
): Promise<ReleaseSummary[]> {
  const statuses = has(db, "release_status");
  const dated = has(db, "release_country", "release_unknown_country");
  const where: string[] = [];
  const all = [...params];
  if (options.status?.length) {
    if (!statuses) throw new MusicBrainzError("filtering by status needs release_status", "unavailable");
    where.push(`st.name IN ${inStrings}`);
    all.push(options.status);
  }
  const rows = await db.query<{
    id: number;
    gid: string;
    name: string;
    disambiguation: string | null;
    status: string | null;
    date: string | null;
    country: string | null;
    barcode: string | null;
    rg_id: number;
    rg_gid: string;
    rg_name: string;
    credit: string;
  }>(
    `WITH ids AS (${idsSql}),
     ev AS (${
       dated
         ? `SELECT release, arg_min(country, coalesce(d, '9999')) AS area, min(d) AS date FROM (
              SELECT rc.release, rc.country, mb_partial_date(rc.date_year, rc.date_month, rc.date_day) AS d FROM release_country rc
              WHERE rc.release IN (SELECT id FROM ids)
              UNION ALL SELECT ru.release, NULL, mb_partial_date(ru.date_year, ru.date_month, ru.date_day) FROM release_unknown_country ru
              WHERE ru.release IN (SELECT id FROM ids)) GROUP BY release`
         : "SELECT NULL::INTEGER AS release, NULL::INTEGER AS area, NULL::VARCHAR AS date WHERE false"
     })
     SELECT r.id, CAST(r.gid AS VARCHAR) AS gid, r.name, nullif(r.comment, '') AS disambiguation,
            ${statuses ? "st.name" : "NULL"} AS status, ev.date,
            ${has(db, "iso_3166_1") ? "(SELECT min(code) FROM iso_3166_1 i WHERE i.area = ev.area)" : "NULL"} AS country,
            nullif(r.barcode, '') AS barcode,
            rg.id AS rg_id, CAST(rg.gid AS VARCHAR) AS rg_gid, rg.name AS rg_name, ac.name AS credit
     FROM ids JOIN release r ON r.id = ids.id JOIN release_group rg ON rg.id = r.release_group
     JOIN artist_credit ac ON ac.id = r.artist_credit
     ${statuses ? "LEFT JOIN release_status st ON st.id = r.status" : ""} LEFT JOIN ev ON ev.release = r.id
     ${where.length ? `WHERE ${where.join(" AND ")}` : ""}
     ORDER BY ev.date NULLS LAST, r.name, r.id ${pageSql(options, 1_000)}`,
    all as never,
  );
  return rows.map((r) => ({
    id: r.id,
    gid: r.gid,
    name: r.name,
    disambiguation: r.disambiguation,
    status: r.status,
    date: r.date,
    country: r.country,
    barcode: r.barcode,
    releaseGroup: { id: r.rg_id, gid: r.rg_gid, name: r.rg_name },
    artistCredit: r.credit,
  }));
}
