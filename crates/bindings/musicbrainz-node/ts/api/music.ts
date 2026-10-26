// Release groups, releases, recordings and works: what an album came out as,
// a release's tracklist, labels and versions, where a recording appears, a
// song's writers and recordings, and the codes they are known by (barcodes,
// catalogue numbers, ISRCs, ISWCs).

import type { Queries } from "../database.js";
import type { EntityInclude, Medium, Recording, Release, ReleaseGroup, Work } from "../entities.js";
import { MusicBrainzError } from "../types.js";
import { type ArtistRef, type ReleaseSummary, releaseSummaries } from "./artists.js";
import { type EntityKey, has, idOf, need, type PageOptions, pageSql, type Ref } from "./common.js";
import type { ExternalIds } from "./external.js";

/** Where a recording appears: a track of a release. */
export interface Appearance {
  release: Ref;
  releaseGroup: Ref;
  /** The release's earliest date. */
  date: string | null;
  status: string | null;
  /** The medium's position (disc 1, 2, …) and format (`CD`, `Digital Media`, `12" Vinyl`). */
  medium: number;
  format: string | null;
  /** The track's position on its medium, and its number as printed (`A1`, `3`). */
  position: number;
  number: string;
  /** The track's title and length (they may differ from the recording's). */
  title: string;
  length: number | null;
}

/** A label of a release. */
export interface ReleaseLabel {
  label: Ref | null;
  catalogNumber: string | null;
}

/** A writer of a work. */
export interface Writer {
  artist: ArtistRef;
  /** `composer`, `lyricist`, `writer`, `arranger`, `translator`, … */
  roles: string[];
}

/** A recording of a work. */
export interface WorkRecording extends Ref {
  disambiguation: string | null;
  length: number | null;
  artistCredit: string;
  /** Its attributes on the relationship: `live`, `instrumental`, `cover`, `partial`, … */
  attributes: string[];
}

/** Release groups (`db.releaseGroups`). */
export class ReleaseGroupsApi {
  constructor(private readonly db: Queries) {}

  /** A release group (`include: ["releases"]` for its releases too). */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<ReleaseGroup | undefined> {
    return this.db.entity("release_group", key, include);
  }

  /** A group's releases, oldest first (`status: ["Official"]` for the official ones only). */
  async releases(key: EntityKey, options: PageOptions & { status?: string[] } = {}): Promise<ReleaseSummary[]> {
    need(this.db, "releases", "release", "artist_credit");
    const id = await idOf(this.db, "release_group", key);
    if (id === undefined) return [];
    return releaseSummaries(this.db, "SELECT id FROM release WHERE release_group = ?", [id], options);
  }

  /**
   * The release that is the group as it first came out: of its official
   * releases (else all), the earliest, then the one on the fewest media,
   * then the one with the fewest tracks, then the lowest id.
   */
  async original(key: EntityKey): Promise<Release | undefined> {
    need(this.db, "original", "release", "medium");
    const id = await idOf(this.db, "release_group", key);
    if (id === undefined) return undefined;
    const statuses = has(this.db, "release_status");
    const dated = has(this.db, "release_country", "release_unknown_country");
    const row = await this.db.one<{ id: number }>(
      `WITH rel AS (SELECT r.id, ${statuses ? "coalesce(st.name = 'Official', false)" : "true"} AS official FROM release r
                    ${statuses ? "LEFT JOIN release_status st ON st.id = r.status" : ""} WHERE r.release_group = ?),
       dates AS (${
         dated
           ? `SELECT release, min(d) AS d FROM (
                SELECT release, mb_partial_date(date_year, date_month, date_day) AS d FROM release_country WHERE release IN (SELECT id FROM rel)
                UNION ALL SELECT release, mb_partial_date(date_year, date_month, date_day) FROM release_unknown_country WHERE release IN (SELECT id FROM rel))
              GROUP BY release`
           : "SELECT NULL::INTEGER AS release, NULL::VARCHAR AS d WHERE false"
       }),
       shape AS (SELECT m.release, count(DISTINCT m.id) AS media, sum(m.track_count) AS tracks FROM medium m
                 WHERE m.release IN (SELECT id FROM rel) GROUP BY m.release)
       SELECT rel.id FROM rel LEFT JOIN dates ON dates.release = rel.id LEFT JOIN shape ON shape.release = rel.id
       ORDER BY rel.official DESC, dates.d NULLS LAST, shape.media NULLS LAST, shape.tracks NULLS LAST, rel.id LIMIT 1`,
      [id],
    );
    return row ? this.db.entity("release", row.id, ["media"]) : undefined;
  }

  /** A group's URLs. */
  async urls(key: EntityKey): Promise<{ type: string; url: string; ended: boolean }[]> {
    const id = await idOf(this.db, "release_group", key);
    if (id === undefined || !has(this.db, "l_release_group_url")) return [];
    return ((await this.db.urls("release_group", [id])).get(id) ?? []).map(({ type, url, ended }) => ({ type, url, ended }));
  }

  /** Who a group is elsewhere (its Discogs master, Wikidata item, …). */
  externalIds(key: EntityKey): Promise<ExternalIds> {
    return this.db.external.ids("release_group", key);
  }
}

/** Releases (`db.releases`). */
export class ReleasesApi {
  constructor(private readonly db: Queries) {}

  /** A release (`include: ["media"]` for its tracklist). */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Release | undefined> {
    return this.db.entity("release", key, include);
  }

  /** A release's media and their tracks (each with its recording), in order. */
  async tracklist(key: EntityKey): Promise<Medium[]> {
    need(this.db, "tracklist", "medium", "track", "recording");
    return (await this.db.entity("release", key, ["media"]))?.media ?? [];
  }

  /** Releases by barcode (UPC/EAN, as printed: leading zeros count). */
  async byBarcode(barcode: string, options: PageOptions = {}): Promise<ReleaseSummary[]> {
    need(this.db, "byBarcode", "release", "artist_credit");
    const code = barcode.trim();
    if (!code) return [];
    return releaseSummaries(this.db, "SELECT id FROM release WHERE barcode = ?", [code], options);
  }

  /** Releases by catalogue number (case and blanks ignored), optionally of one label. */
  async byCatalogNumber(catalogNumber: string, options: PageOptions & { label?: EntityKey } = {}): Promise<ReleaseSummary[]> {
    need(this.db, "byCatalogNumber", "release", "release_label", "artist_credit");
    const norm = catalogNumber.replace(/\s+/g, "").toLowerCase();
    if (!norm) return [];
    const params: unknown[] = [norm];
    let label = "";
    if (options.label !== undefined) {
      const id = await idOf(this.db, "label", options.label);
      if (id === undefined) return [];
      label = "AND label = ?";
      params.push(id);
    }
    return releaseSummaries(
      this.db,
      `SELECT DISTINCT release AS id FROM release_label WHERE lower(regexp_replace(catalog_number, '\\s+', '', 'g')) = ? ${label}`,
      params,
      options,
    );
  }

  /** A release's labels and catalogue numbers, in the dump's order. */
  async labels(key: EntityKey): Promise<ReleaseLabel[]> {
    return (await this.db.entity("release", key))?.labels ?? [];
  }

  /** When and where a release came out (`area` null: worldwide or unknown). */
  async events(key: EntityKey): Promise<Release["releaseEvents"]> {
    return (await this.db.entity("release", key))?.releaseEvents ?? [];
  }

  /** The other releases of the same release group (versions, reissues, other countries), oldest first. */
  async otherVersions(key: EntityKey, options: PageOptions & { status?: string[] } = {}): Promise<ReleaseSummary[]> {
    need(this.db, "otherVersions", "release", "artist_credit");
    const id = await idOf(this.db, "release", key);
    if (id === undefined) return [];
    return releaseSummaries(
      this.db,
      "SELECT id FROM release WHERE release_group = (SELECT release_group FROM release WHERE id = ?) AND id <> ?",
      [id, id],
      options,
    );
  }

  /** The recordings a release plays, in running order (once each). */
  async recordings(key: EntityKey): Promise<Ref[]> {
    const seen = new Set<number>();
    const out: Ref[] = [];
    for (const m of await this.tracklist(key))
      for (const t of m.tracks)
        if (!seen.has(t.recording.id)) {
          seen.add(t.recording.id);
          out.push({ id: t.recording.id, gid: t.recording.gid, name: t.recording.name });
        }
    return out;
  }

  /** A release's URLs (streaming and store pages, …). */
  async urls(key: EntityKey): Promise<{ type: string; url: string; ended: boolean }[]> {
    const id = await idOf(this.db, "release", key);
    if (id === undefined || !has(this.db, "l_release_url")) return [];
    return ((await this.db.urls("release", [id])).get(id) ?? []).map(({ type, url, ended }) => ({ type, url, ended }));
  }

  /** Who a release is elsewhere (its Discogs release, Spotify album, …). */
  externalIds(key: EntityKey): Promise<ExternalIds> {
    return this.db.external.ids("release", key);
  }
}

/** Recordings (`db.recordings`). */
export class RecordingsApi {
  constructor(private readonly db: Queries) {}

  /** A recording. */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Recording | undefined> {
    return this.db.entity("recording", key, include);
  }

  /** Recordings by ISRC (`USRC17607839`; case and dashes ignored). */
  async byIsrc(isrc: string): Promise<Recording[]> {
    need(this.db, "byIsrc", "isrc", "recording");
    const code = isrc.replace(/[-\s]/g, "").toUpperCase();
    if (!/^[A-Z]{2}[A-Z0-9]{3}\d{7}$/.test(code)) throw new MusicBrainzError(`not an ISRC: ${isrc}`, "invalid");
    const ids = (await this.db.query<{ id: number }>("SELECT DISTINCT recording AS id FROM isrc WHERE isrc = ? ORDER BY id", [code])).map((r) => r.id);
    return [...(await this.db.entities("recording", ids)).values()];
  }

  /** A recording's ISRCs. */
  async isrcs(key: EntityKey): Promise<string[]> {
    return (await this.db.entity("recording", key))?.isrcs ?? [];
  }

  /** The releases a recording appears on, oldest first: each track of it. */
  async appearances(key: EntityKey, options: PageOptions & { status?: string[] } = {}): Promise<Appearance[]> {
    need(this.db, "appearances", "track", "medium", "release", "release_group");
    const id = await idOf(this.db, "recording", key);
    if (id === undefined) return [];
    const statuses = has(this.db, "release_status");
    const formats = has(this.db, "medium_format");
    const dated = has(this.db, "release_country", "release_unknown_country");
    const params: unknown[] = [id];
    let where = "";
    if (options.status?.length) {
      if (!statuses) throw new MusicBrainzError("filtering by status needs release_status", "unavailable");
      where = `WHERE st.name IN (SELECT unnest(from_json(?, '["VARCHAR"]')))`;
      params.push(options.status);
    }
    const rows = await this.db.query<{
      r_id: number;
      r_gid: string;
      r_name: string;
      g_id: number;
      g_gid: string;
      g_name: string;
      date: string | null;
      status: string | null;
      medium: number;
      format: string | null;
      position: number;
      number: string;
      title: string;
      length: number | null;
    }>(
      `WITH t AS (SELECT t.*, m.release, m.position AS medium_position, m.format FROM track t JOIN medium m ON m.id = t.medium WHERE t.recording = ?),
       dates AS (${
         dated
           ? `SELECT release, min(d) AS d FROM (
                SELECT release, mb_partial_date(date_year, date_month, date_day) AS d FROM release_country WHERE release IN (SELECT release FROM t)
                UNION ALL SELECT release, mb_partial_date(date_year, date_month, date_day) FROM release_unknown_country WHERE release IN (SELECT release FROM t))
              GROUP BY release`
           : "SELECT NULL::INTEGER AS release, NULL::VARCHAR AS d WHERE false"
       })
       SELECT r.id AS r_id, CAST(r.gid AS VARCHAR) AS r_gid, r.name AS r_name,
              g.id AS g_id, CAST(g.gid AS VARCHAR) AS g_gid, g.name AS g_name, dates.d AS date,
              ${statuses ? "st.name" : "NULL"} AS status, t.medium_position AS medium,
              ${formats ? "mf.name" : "NULL"} AS format, t.position, t.number, t.name AS title, t.length
       FROM t JOIN release r ON r.id = t.release JOIN release_group g ON g.id = r.release_group
       ${statuses ? "LEFT JOIN release_status st ON st.id = r.status" : ""}
       ${formats ? "LEFT JOIN medium_format mf ON mf.id = t.format" : ""}
       LEFT JOIN dates ON dates.release = r.id
       ${where}
       ORDER BY date NULLS LAST, r.id, t.medium_position, t.position ${pageSql(options, 1_000)}`,
      params as never,
    );
    return rows.map((r) => ({
      release: { id: r.r_id, gid: r.r_gid, name: r.r_name },
      releaseGroup: { id: r.g_id, gid: r.g_gid, name: r.g_name },
      date: r.date,
      status: r.status,
      medium: r.medium,
      format: r.format,
      position: r.position,
      number: r.number,
      title: r.title,
      length: r.length,
    }));
  }

  /** The works a recording performs. */
  async works(key: EntityKey): Promise<Ref[]> {
    if (!has(this.db, "l_recording_work", "work")) return [];
    const id = await idOf(this.db, "recording", key);
    if (id === undefined) return [];
    return this.db.query<Ref>(
      `SELECT w.id, CAST(w.gid AS VARCHAR) AS gid, w.name FROM l_recording_work r JOIN work w ON w.id = r.entity1
       WHERE r.entity0 = ? ORDER BY r.rowid`,
      [id],
    );
  }

  /** Who a recording is elsewhere (its Spotify track, …). */
  externalIds(key: EntityKey): Promise<ExternalIds> {
    return this.db.external.ids("recording", key);
  }
}

/** Works (`db.works`). */
export class WorksApi {
  constructor(private readonly db: Queries) {}

  /** A work. */
  get(key: EntityKey, include: readonly EntityInclude[] = []): Promise<Work | undefined> {
    return this.db.entity("work", key, include);
  }

  /** Works by ISWC (`T-034.524.680-1`; punctuation ignored). */
  async byIswc(iswc: string): Promise<Work[]> {
    need(this.db, "byIswc", "iswc", "work");
    const digits = iswc.replace(/[^0-9Tt]/g, "").toUpperCase();
    if (!/^T\d{10}$/.test(digits)) throw new MusicBrainzError(`not an ISWC: ${iswc}`, "invalid");
    const ids = (
      await this.db.query<{ id: number }>("SELECT DISTINCT work AS id FROM iswc WHERE upper(regexp_replace(iswc, '[^0-9Tt]', '', 'g')) = ? ORDER BY id", [digits])
    ).map((r) => r.id);
    return [...(await this.db.entities("work", ids)).values()];
  }

  /** Who wrote a work, and how (composer, lyricist, …), in the dump's order. */
  async writers(key: EntityKey): Promise<Writer[]> {
    if (!has(this.db, "l_artist_work", "link", "link_type")) return [];
    const id = await idOf(this.db, "work", key);
    if (id === undefined) return [];
    const rows = await this.db.query<{ artist: number; roles: string[] }>(
      `SELECT r.entity0 AS artist, list(DISTINCT lt.name ORDER BY lt.name) AS roles, min(r.rowid) AS o
       FROM l_artist_work r JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type
       WHERE r.entity1 = ? GROUP BY r.entity0 ORDER BY o`,
      [id],
    );
    const refs = await this.db.artists.refs(rows.map((r) => r.artist));
    return rows.flatMap((r) => {
      const artist = refs.get(r.artist);
      return artist ? [{ artist, roles: r.roles }] : [];
    });
  }

  /** A work's recordings, by name, with how they perform it (`live`, `cover`, `instrumental`, …). */
  async recordings(key: EntityKey, options: PageOptions = {}): Promise<WorkRecording[]> {
    need(this.db, "recordings", "l_recording_work", "recording", "artist_credit");
    const id = await idOf(this.db, "work", key);
    if (id === undefined) return [];
    const attrs = this.db.hasTable("link_attribute") && this.db.hasTable("link_attribute_type");
    const rows = await this.db.query<WorkRecording & { credit: string }>(
      `SELECT rec.id, CAST(rec.gid AS VARCHAR) AS gid, rec.name, nullif(rec.comment, '') AS disambiguation, rec.length,
              ac.name AS credit,
              ${attrs ? "coalesce(list(DISTINCT lat.name ORDER BY lat.name) FILTER (WHERE lat.name IS NOT NULL), [])" : "[]::VARCHAR[]"} AS attributes
       FROM l_recording_work r JOIN recording rec ON rec.id = r.entity0 JOIN artist_credit ac ON ac.id = rec.artist_credit
       ${attrs ? "LEFT JOIN link_attribute la ON la.link = r.link LEFT JOIN link_attribute_type lat ON lat.id = la.attribute_type" : ""}
       WHERE r.entity1 = ?
       GROUP BY rec.id, rec.gid, rec.name, rec.comment, rec.length, ac.name
       ORDER BY rec.name, rec.id ${pageSql(options, 1_000)}`,
      [id],
    );
    return rows.map(({ credit, ...r }) => ({ ...r, artistCredit: credit }));
  }

  /** A work's ISWCs. */
  async iswcs(key: EntityKey): Promise<string[]> {
    return (await this.db.entity("work", key))?.iswcs ?? [];
  }
}
