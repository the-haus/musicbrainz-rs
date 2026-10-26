// Finding entities by name: their names, sort names and aliases, matched
// case- and accent-insensitively, exactly, by prefix, by substring, or
// fuzzily (Jaro–Winkler), ranked by how well they match and then by how
// much of MusicBrainz refers to them.

import type { Queries } from "../database.js";
import { type EntityType, ENTITY_TYPES, MusicBrainzError } from "../types.js";
import { aliasTable, has, ident, normalize, normalized, type PageOptions, pageSql, type Ref } from "./common.js";

/** How a name matches: equal, starting with, containing the text, or like it (fuzzy). */
export type MatchMode = "exact" | "prefix" | "contains" | "fuzzy";

/** Options of a search. */
export interface SearchOptions extends PageOptions {
  /** How names match (default `contains`). */
  mode?: MatchMode;
  /** Match aliases (and sort names) too (default true). */
  aliases?: boolean;
  /** Fuzzy matches' least Jaro–Winkler similarity, 0–1 (default 0.88). */
  minSimilarity?: number;
  /**
   * Rank equal matches by popularity (default true). Off, they are ranked by
   * id, and a search of recordings (40 M names) is about a third faster.
   */
  popularity?: boolean;
}

/** A match. */
export interface SearchHit extends Ref {
  type: EntityType;
  disambiguation: string | null;
  /** The name, sort name or alias that matched. */
  matched: string;
  matchedBy: "name" | "sort name" | "alias";
  /** 1 an exact match, 0.9 a prefix, 0.8 a substring, below that fuzzy. */
  score: number;
  /** How much refers to it: an artist's or label's release groups, a recording's tracks, … */
  popularity: number;
}

/** The table a search of a type reads, and how to count what refers to its rows. */
const POPULARITY: Partial<Record<EntityType, (id: string) => string>> = {
  artist: (id) =>
    `(SELECT count(DISTINCT rg.id) FROM artist_credit_name acn JOIN release_group rg ON rg.artist_credit = acn.artist_credit WHERE acn.artist = ${id})`,
  release_group: (id) => `(SELECT count(*) FROM release r WHERE r.release_group = ${id})`,
  release: (id) => `(SELECT count(*) FROM medium m WHERE m.release = ${id})`,
  recording: (id) => `(SELECT count(*) FROM track t WHERE t.recording = ${id})`,
  work: (id) => `(SELECT count(*) FROM l_recording_work l WHERE l.entity1 = ${id})`,
  label: (id) => `(SELECT count(*) FROM release_label rl WHERE rl.label = ${id})`,
  area: (id) => `(SELECT count(*) FROM artist a WHERE a.area = ${id})`,
  place: (id) => `(SELECT count(*) FROM l_event_place l WHERE l.entity1 = ${id})`,
  series: (id) => `(SELECT count(*) FROM l_event_series l WHERE l.entity1 = ${id})`,
};

/** Which tables a popularity count reads. */
const POPULARITY_TABLES: Partial<Record<EntityType, string[]>> = {
  artist: ["artist_credit_name", "release_group"],
  release_group: ["release"],
  release: ["medium"],
  recording: ["track"],
  work: ["l_recording_work"],
  label: ["release_label"],
  area: ["artist"],
  place: ["l_event_place"],
  series: ["l_event_series"],
};

/** Searches (`db.search`). */
export class SearchApi {
  constructor(private readonly db: Queries) {}

  /** Artists by name (see {@link entities}). */
  artists(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("artist", text, options);
  }

  /** Release groups (albums, singles, …) by name. */
  releaseGroups(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("release_group", text, options);
  }

  /** Releases by name. */
  releases(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("release", text, options);
  }

  /** Recordings by name. */
  recordings(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("recording", text, options);
  }

  /** Works by name. */
  works(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("work", text, options);
  }

  /** Labels by name. */
  labels(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("label", text, options);
  }

  /** Areas by name. */
  areas(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("area", text, options);
  }

  /** Places by name. */
  places(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("place", text, options);
  }

  /** Events by name. */
  events(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("event", text, options);
  }

  /** Series by name. */
  series(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("series", text, options);
  }

  /** Genres by name. */
  genres(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("genre", text, options);
  }

  /** Instruments by name. */
  instruments(text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    return this.entities("instrument", text, options);
  }

  /**
   * Entities of a type whose name (or sort name, or alias) matches `text`,
   * best first: by score, then popularity, then id. Case, accents and the
   * blanks around it do not count (`Beyoncé` is `beyonce`).
   */
  async entities(type: EntityType, text: string, options: SearchOptions = {}): Promise<SearchHit[]> {
    if (!(ENTITY_TYPES as readonly string[]).includes(type) || type === "url")
      throw new MusicBrainzError(`not a searchable type: ${type}`, "invalid");
    if (typeof text !== "string") throw new MusicBrainzError("search text must be a string", "invalid");
    const q = normalize(text);
    if (!q) return [];
    const mode = options.mode ?? "contains";
    if (!["exact", "prefix", "contains", "fuzzy"].includes(mode))
      throw new MusicBrainzError(`not a match mode: ${mode}`, "invalid");
    const min = options.minSimilarity ?? 0.88;
    if (!(min >= 0 && min <= 1)) throw new MusicBrainzError("minSimilarity must be from 0 to 1", "invalid");
    const page = pageSql(options, 25, 1_000);
    if (!this.db.hasTable(type)) return [];

    const t = ident(type);
    const names = [`SELECT e.id, e.name AS matched, 'name' AS matched_by FROM ${t} e`];
    if (options.aliases !== false) {
      if (type === "artist") names.push(`SELECT e.id, e.sort_name, 'sort name' FROM artist e WHERE e.sort_name <> e.name`);
      if (this.db.hasTable(aliasTable(type)))
        names.push(`SELECT a.${ident(type)}, a.name, 'alias' FROM ${ident(aliasTable(type))} a`);
    }
    // Cheap filters on the raw name first, implied by the match: an exact or
    // fuzzy match is about as long as the text, a prefix match longer; all
    // three start with the text's first letter.
    const L = q.length;
    const first = `strip_accents(lower(left(ltrim(matched), 1))) = left($1, 1)`;
    const length = {
      exact: `length(matched) BETWEEN ${Math.max(0, L - 4)} AND ${L + 8} AND ${first}`,
      prefix: `length(matched) >= ${Math.max(0, L - 4)} AND ${first}`,
      contains: `length(matched) >= ${Math.max(0, L - 4)}`,
      fuzzy: `length(matched) BETWEEN ${Math.floor(L * 0.5)} AND ${Math.ceil(L * 2) + 4} AND ${first}`,
    }[mode];
    const filter = {
      exact: "n = $1",
      prefix: "starts_with(n, $1)",
      contains: "contains(n, $1)",
      fuzzy: "(contains(n, $1) OR (left(n, 1) = left($1, 1) AND jaro_winkler_similarity(n, $1) >= $2))",
    }[mode];
    const popularity =
      options.popularity !== false && POPULARITY[type] && has(this.db, ...(POPULARITY_TABLES[type] ?? []))
        ? POPULARITY[type]!("b.id")
        : "0";
    const rows = await this.db.query<{
      id: number;
      gid: string;
      name: string;
      disambiguation: string | null;
      matched: string;
      matched_by: SearchHit["matchedBy"];
      score: number;
      popularity: number;
    }>(
      `WITH names AS (${names.join(" UNION ALL ")}),
       scored AS (
         SELECT id, matched, matched_by,
                CASE WHEN n = $1 THEN 1.0 WHEN starts_with(n, $1) THEN 0.9 WHEN contains(n, $1) THEN 0.8
                     ELSE round(0.8 * jaro_winkler_similarity(n, $1), 4) END AS score
         FROM (SELECT *, ${normalized("matched")} AS n FROM names WHERE matched IS NOT NULL AND ${length})
         WHERE ${filter} AND $2 >= 0),
       best AS (
         -- Of equal scores, the name before the sort name before an alias.
         SELECT id, max(score) AS score,
                arg_max(matched, score * 4 + CASE matched_by WHEN 'name' THEN 2 WHEN 'sort name' THEN 1 ELSE 0 END) AS matched,
                arg_max(matched_by, score * 4 + CASE matched_by WHEN 'name' THEN 2 WHEN 'sort name' THEN 1 ELSE 0 END) AS matched_by
         FROM scored GROUP BY id ORDER BY score DESC, id LIMIT 2000)
       SELECT b.id, CAST(e.gid AS VARCHAR) AS gid, e.name, nullif(e.comment, '') AS disambiguation,
              b.matched, b.matched_by, b.score, ${popularity} AS popularity
       FROM best b JOIN ${t} e ON e.id = b.id
       ORDER BY b.score DESC, popularity DESC, b.id ${page}`,
      [q, min],
    );
    return rows.map((r) => ({
      type,
      id: r.id,
      gid: r.gid,
      name: r.name,
      disambiguation: r.disambiguation,
      matched: r.matched,
      matchedBy: r.matched_by,
      score: Number(r.score),
      popularity: Number(r.popularity),
    }));
  }
}
