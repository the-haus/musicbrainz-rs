// External ids: what an entity's URLs say it is elsewhere (its Wikidata
// item, Discogs artist, Spotify album, …), and which entities a URL or an id
// elsewhere belongs to. MusicBrainz keeps URLs in a canonical form, so each
// service's pages are told apart by one pattern.

import type { Queries } from "../database.js";
import { type EntityType, MusicBrainzError } from "../types.js";
import { type EntityKey, has, idsOf, inInts, type Ref } from "./common.js";

/** A service whose page URLs carry an id: how to read the id out of one. */
export interface Service {
  /** The URL pattern; the id is its capture groups joined with `/`. */
  readonly pattern: RegExp;
  /** A literal every URL of an id contains (for the reverse lookup). */
  readonly contains: (id: string) => string;
}

const join = (m: RegExpExecArray) => m.slice(1).filter((x) => x !== undefined).join("/");

/**
 * The services known by their URLs, and how their ids look: Wikidata `Q42`,
 * Discogs `artist/123`, Spotify `album/<22 characters>`, Wikipedia
 * `en/Title`, OpenStreetMap `node/123`, … (`externalIds`).
 */
export const SERVICES = {
  wikidata: { pattern: /^https?:\/\/(?:www\.)?wikidata\.org\/(?:wiki|entity)\/(Q\d+)$/, contains: (id) => `/${id}` },
  discogs: { pattern: /^https?:\/\/(?:www\.)?discogs\.com\/(artist|label|master|release)\/(\d+)/, contains: (id) => `/${id}` },
  spotify: {
    pattern: /^https?:\/\/open\.spotify\.com\/(?:intl-[a-z-]+\/)?(artist|album|track|playlist|show|episode)\/([0-9A-Za-z]{22})/,
    contains: (id) => id.split("/")[1] ?? id,
  },
  appleMusic: {
    pattern: /^https?:\/\/(?:music|itunes)\.apple\.com\/(?:[a-z]{2}\/)?(artist|album|song|music-video)\/(?:[^/]+\/)?(?:id)?(\d+)/,
    contains: (id) => id.split("/")[1] ?? id,
  },
  deezer: { pattern: /^https?:\/\/(?:www\.)?deezer\.com\/(?:[a-z]{2}\/)?(artist|album|track)\/(\d+)/, contains: (id) => `/${id}` },
  tidal: {
    pattern: /^https?:\/\/(?:listen\.|www\.)?tidal\.com\/(?:browse\/)?(artist|album|track)\/(\d+)/,
    contains: (id) => `/${id}`,
  },
  amazonMusic: { pattern: /^https?:\/\/music\.amazon\.[a-z.]+\/(artists|albums)\/([A-Z0-9]{10})/, contains: (id) => id.split("/")[1] ?? id },
  bandcamp: { pattern: /^https?:\/\/([a-z0-9-]+)\.bandcamp\.com(?:\/|$)/, contains: (id) => `//${id}.bandcamp.com` },
  soundcloud: { pattern: /^https?:\/\/(?:www\.)?soundcloud\.com\/([^/?#]+)\/?$/, contains: (id) => `soundcloud.com/${id}` },
  youtube: {
    pattern: /^https?:\/\/(?:www\.)?youtube\.com\/(channel\/UC[\w-]{22}|user\/[^/?#]+|c\/[^/?#]+|@[^/?#]+)\/?$/,
    contains: (id) => `youtube.com/${id}`,
  },
  youtubeVideo: { pattern: /^https?:\/\/(?:www\.)?youtube\.com\/watch\?v=([\w-]{11})/, contains: (id) => `v=${id}` },
  instagram: { pattern: /^https?:\/\/(?:www\.)?instagram\.com\/([^/?#]+)\/?$/, contains: (id) => `instagram.com/${id}` },
  facebook: { pattern: /^https?:\/\/(?:www\.)?facebook\.com\/([^/?#]+)\/?$/, contains: (id) => `facebook.com/${id}` },
  twitter: { pattern: /^https?:\/\/(?:www\.)?(?:twitter|x)\.com\/([^/?#]+)\/?$/, contains: (id) => `.com/${id}` },
  tiktok: { pattern: /^https?:\/\/(?:www\.)?tiktok\.com\/@([^/?#]+)\/?$/, contains: (id) => `tiktok.com/@${id}` },
  wikipedia: { pattern: /^https?:\/\/([a-z-]+)\.wikipedia\.org\/wiki\/(.+)$/, contains: (id) => id.split("/").slice(1).join("/") },
  allmusic: {
    pattern: /^https?:\/\/(?:www\.)?allmusic\.com\/(artist|album|song|composition)\/(?:[^/]*-)?(m[a-z]\d{10})/,
    contains: (id) => id.split("/")[1] ?? id,
  },
  lastfm: { pattern: /^https?:\/\/(?:www\.)?last\.fm\/(?:[a-z]{2}\/)?music\/([^/?#]+)\/?$/, contains: (id) => `music/${id}` },
  imdb: { pattern: /^https?:\/\/(?:www\.)?imdb\.com\/(name|title|company)\/((?:nm|tt|co)\d+)/, contains: (id) => id.split("/")[1] ?? id },
  viaf: { pattern: /^https?:\/\/viaf\.org\/viaf\/(\d+)/, contains: (id) => `viaf/${id}` },
  isni: { pattern: /^https?:\/\/(?:www\.)?isni\.org\/isni\/(\d{15}[\dX])/, contains: (id) => id },
  songkick: { pattern: /^https?:\/\/(?:www\.)?songkick\.com\/(artists|venues|festivals)\/(\d+)/, contains: (id) => `/${id}` },
  setlistfm: { pattern: /^https?:\/\/(?:www\.)?setlist\.fm\/setlists\/[^/]*-([0-9a-f]{8})\.html/, contains: (id) => id },
  residentAdvisor: {
    pattern: /^https?:\/\/(?:www\.)?(?:ra\.co|residentadvisor\.net)\/(dj|labels|clubs|events)\/([^/?#]+)/,
    contains: (id) => id.split("/")[1] ?? id,
  },
  beatport: {
    pattern: /^https?:\/\/(?:www\.)?beatport\.com\/(artist|label|release|track)\/[^/]+\/(\d+)/,
    contains: (id) => `/${id.split("/")[1] ?? id}`,
  },
  genius: { pattern: /^https?:\/\/(?:www\.)?genius\.com\/artists\/([^/?#]+)/, contains: (id) => `artists/${id}` },
  openstreetmap: { pattern: /^https?:\/\/(?:www\.)?openstreetmap\.org\/(node|way|relation)\/(\d+)/, contains: (id) => `/${id}` },
  geonames: { pattern: /^https?:\/\/(?:www\.|sws\.)?geonames\.org\/(\d+)/, contains: (id) => `/${id}` },
  vgmdb: { pattern: /^https?:\/\/vgmdb\.net\/(artist|album|org|product)\/(\d+)/, contains: (id) => `/${id}` },
  secondHandSongs: {
    pattern: /^https?:\/\/(?:www\.)?secondhandsongs\.com\/(artist|work|performance|release|label)\/(\d+)/,
    contains: (id) => `/${id}`,
  },
  rateYourMusic: { pattern: /^https?:\/\/(?:www\.)?rateyourmusic\.com\/(artist|label|release\/[a-z]+)\/([^/?#]+)/, contains: (id) => id },
} as const satisfies Record<string, Service>;

/** A service of {@link SERVICES}. */
export type ServiceName = keyof typeof SERVICES;

/** Ids elsewhere, by service, in the order the URLs come (once each). */
export type ExternalIds = Partial<Record<ServiceName, string[]>>;

/** The ids a URL carries: `{ service, id }`, or undefined for a URL of no service known. */
export function parseUrl(url: string): { service: ServiceName; id: string } | undefined {
  for (const [service, s] of Object.entries(SERVICES) as [ServiceName, Service][]) {
    const m = s.pattern.exec(url);
    if (m) return { service, id: join(m) };
  }
  return undefined;
}

/** The ids some URLs carry, by service. */
export function externalIds(urls: Iterable<string>): ExternalIds {
  const out: ExternalIds = {};
  for (const url of urls) {
    const p = parseUrl(url);
    if (!p) continue;
    const list = (out[p.service] ??= []);
    if (!list.includes(p.id)) list.push(p.id);
  }
  return out;
}

/** An entity a URL belongs to, and how. */
export interface UrlOwner extends Ref {
  type: EntityType;
  /** The relationship's type: `wikidata`, `discogs`, `official homepage`, `free streaming`, … */
  relationship: string;
  ended: boolean;
  url: string;
}

/** External ids and URLs (`db.external`). */
export class ExternalApi {
  constructor(private readonly db: Queries) {}

  /** The ids an entity's URLs carry, by service (`{ wikidata: ["Q…"], discogs: ["artist/…"] }`). */
  async ids(type: EntityType, key: EntityKey): Promise<ExternalIds> {
    return (await this.idsOf(type, [key])).get(key) ?? {};
  }

  /** {@link ids} for many entities: a map from each key found. */
  async idsOf(type: EntityType, keys: Iterable<EntityKey>): Promise<Map<EntityKey, ExternalIds>> {
    const list = [...keys];
    const out = new Map<EntityKey, ExternalIds>();
    const ids = await idsOf(this.db, type, list);
    if (!ids.length) return out;
    const urls = await this.db.urls(type, ids);
    const resolved = new Map<EntityKey, number>();
    const gids = await this.db.resolveGids(type, list.filter((k): k is string => typeof k === "string"));
    for (const k of list) {
      const id = typeof k === "number" ? k : gids.get(k.toLowerCase())?.id;
      if (id !== undefined) resolved.set(k, id);
    }
    for (const [k, id] of resolved) out.set(k, externalIds((urls.get(id) ?? []).map((u) => u.url)));
    return out;
  }

  /** The entities linked to a URL (exactly as MusicBrainz keeps it). */
  async byUrl(url: string): Promise<UrlOwner[]> {
    if (!has(this.db, "url")) return [];
    const ids = (await this.db.query<{ id: number }>("SELECT id FROM url WHERE url = ?", [url])).map((r) => r.id);
    return this.owners(ids);
  }

  /**
   * The entities a service's id belongs to: `find("wikidata", "Q42")`,
   * `find("discogs", "artist/123")`, `find("spotify", "artist/<id>")`.
   */
  async find(service: ServiceName, id: string, options: { types?: EntityType[] } = {}): Promise<UrlOwner[]> {
    const s: Service | undefined = (SERVICES as Record<string, Service>)[service];
    if (!s) throw new MusicBrainzError(`not a service: ${service}`, "invalid");
    if (!has(this.db, "url")) return [];
    const candidates = await this.db.query<{ id: number; url: string }>(
      "SELECT id, url FROM url WHERE contains(url, ?)",
      [s.contains(id)],
    );
    const ids = candidates.filter((c) => {
      const m = s.pattern.exec(c.url);
      return m && join(m) === id;
    });
    const owners = await this.owners(ids.map((c) => c.id));
    return options.types ? owners.filter((o) => options.types!.includes(o.type)) : owners;
  }

  /** The entities of a Wikidata item (`Q42`). */
  byWikidata(item: string, options: { types?: EntityType[] } = {}): Promise<UrlOwner[]> {
    return this.find("wikidata", item, options);
  }

  /** The entities some URLs (by id) belong to. */
  private async owners(urlIds: number[]): Promise<UrlOwner[]> {
    if (!urlIds.length) return [];
    const rows = await this.db.query<{
      type: EntityType;
      id: number;
      relationship: string;
      ended: boolean;
      url: string;
    }>(
      `SELECT CASE WHEN r.entity1_type = 'url' THEN r.entity0_type ELSE r.entity1_type END AS type,
              CASE WHEN r.entity1_type = 'url' THEN r.entity0 ELSE r.entity1 END AS id,
              r.link_type AS relationship, r.ended, u.url
       FROM mb_relationship r JOIN url u ON u.id = CASE WHEN r.entity1_type = 'url' THEN r.entity1 ELSE r.entity0 END
       WHERE (r.entity1_type = 'url' AND r.entity1 IN ${inInts}) OR (r.entity0_type = 'url' AND r.entity0 IN ${inInts})
       ORDER BY type, id, r.row_order`,
      [urlIds, urlIds],
    );
    const out: UrlOwner[] = [];
    const byType = new Map<EntityType, number[]>();
    for (const r of rows) byType.set(r.type, [...(byType.get(r.type) ?? []), r.id]);
    const names = new Map<string, { gid: string; name: string }>();
    for (const [type, ids] of byType)
      for (const [id, row] of await this.db.byId<{ gid: string; n: string }>(type, ids, {
        columns: `CAST(gid AS VARCHAR) AS gid, ${type === "url" ? "url" : "name"} AS n`,
      }))
        names.set(`${type}:${id}`, { gid: row.gid, name: row.n });
    for (const r of rows) {
      const n = names.get(`${r.type}:${r.id}`);
      if (n) out.push({ type: r.type, id: r.id, gid: n.gid, name: n.name, relationship: r.relationship, ended: r.ended, url: r.url });
    }
    return out;
  }
}

/** For tests: the services' names. */
export const serviceNames = (): ServiceName[] => Object.keys(SERVICES) as ServiceName[];
