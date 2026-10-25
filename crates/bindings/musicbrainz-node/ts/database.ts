// A built database: SQL, streamed rows, and lookups that batch any number
// of keys into one query.

import { NativeDatabase, type NativeSession } from "./native.js";
import { loadEntities, type EntityInclude, type EntityOf } from "./entities.js";
import { RowStream, type StreamOptions } from "./task.js";
import {
  type Change,
  type DatabaseInfo,
  type EntityType,
  ENTITY_TYPES,
  MusicBrainzError,
  type Param,
  type Row,
} from "./types.js";

/** Options of {@link MusicBrainzDatabase.open}. */
export interface OpenOptions {
  /** Open read-only (default true: several processes may then read it at once). */
  readOnly?: boolean;
  /** DuckDB threads (default: every core). */
  threads?: number;
  /** DuckDB's memory limit (`"4GB"`). */
  memoryLimit?: string;
}

/** A relationship of an entity, seen from it. */
export interface Relationship {
  /** Its row's id in its table. */
  id: number;
  /** `l_artist_url`. */
  linkTable: string;
  /** Its type's name: `member of band`, `wikidata`, `remix`. */
  type: string;
  typeId: number;
  typeGid: string;
  /** `forward`: the entity is the relationship's `entity0`. */
  direction: "forward" | "backward";
  targetType: EntityType;
  /** The other entity: its id, MBID and name (a URL's text for a URL). */
  target: { id: number; gid: string | null; name: string | null };
  /** Partial dates: `1994`, `1994-05`, `1994-05-17`. */
  begin: string | null;
  end: string | null;
  ended: boolean;
  /** Its place among its kind (`link_order`). */
  order: number;
  /** How the entity, and the other, are credited in it. */
  credit: string | null;
  targetCredit: string | null;
  /** Its attributes (`instrument`, `vocal`, `live`), with credits and values. */
  attributes: { type: string; typeGid: string; credit: string | null; value: string | null }[];
}

/** An artist credit: "A & B feat. C". */
export interface ArtistCredit {
  id: number;
  /** The whole credit as printed. */
  name: string;
  names: {
    position: number;
    /** The name the credit prints for this artist. */
    name: string;
    joinPhrase: string;
    artist: { id: number; gid: string; name: string; sortName: string };
  }[];
}

/** An alias of an entity. */
export interface Alias {
  id: number;
  name: string;
  sortName: string | null;
  locale: string | null;
  primary: boolean;
  /** Its type's name: `Artist name`, `Legal name`, `Search hint`. */
  type: string | null;
  begin: string | null;
  end: string | null;
  ended: boolean;
}

/** Options of {@link MusicBrainzDatabase.relationships}. */
export interface RelationshipOptions {
  /** Only to these entity types. */
  targetTypes?: EntityType[];
  /** Only of these types (by name: `member of band`). */
  types?: string[];
  /** Only where the entity is `entity0` (`forward`) or `entity1` (`backward`). */
  direction?: "forward" | "backward";
  /** Read each relationship's attributes (default true). */
  attributes?: boolean;
  /** Read the other entities' MBIDs and names (default true). */
  targets?: boolean;
}

const quoted = (name: string) => `"${name.replace(/"/g, '""')}"`;
/** A table's name in SQL: `"artist"`, `"cover_art_archive"."art_type"`. */
export const tableName = (name: string) =>
  name.includes(".") ? name.split(".").map(quoted).join(".") : quoted(name);

const isEntity = (e: string): e is EntityType => (ENTITY_TYPES as readonly string[]).includes(e);
function entity(e: string): EntityType {
  if (!isEntity(e)) throw new MusicBrainzError(`not an entity type: ${e}`, "invalid");
  return e;
}

const GID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** The column a summary of an entity names it by (a URL by its text). */
export const nameColumn = (e: EntityType) => (e === "url" ? "url" : "name");

/** What the database and its sessions both run queries on. */
type NativeQueries = Pick<NativeDatabase, "query" | "querySync" | "execute" | "executeBatch" | "describe" | "stream">;

const toError = (e: unknown) => (e instanceof MusicBrainzError ? e : new MusicBrainzError((e as Error).message));

/**
 * Queries and lookups, on a database ({@link MusicBrainzDatabase}: a pool of
 * connections) or a {@link Session} (one connection).
 */
export abstract class Queries {
  #native: NativeQueries;

  /** @internal */
  protected constructor(native: NativeQueries) {
    this.#native = native;
  }

  /** Whether a table was loaded. */
  abstract hasTable(name: string): boolean;

  static #params(params: readonly Param[] | undefined): string | undefined {
    return params && params.length ? JSON.stringify(params) : undefined;
  }

  /** A query's rows (`?` or `$1` placeholders; arrays bind as JSON text: `?::INTEGER[]`). */
  async query<T = Row>(sql: string, params?: readonly Param[]): Promise<T[]> {
    try {
      return JSON.parse(await this.#native.query(sql, Queries.#params(params))) as T[];
    } catch (e) {
      throw toError(e);
    }
  }

  /** A query's rows, blocking the event loop until they are there. */
  querySync<T = Row>(sql: string, params?: readonly Param[]): T[] {
    try {
      return JSON.parse(this.#native.querySync(sql, Queries.#params(params))) as T[];
    } catch (e) {
      throw new MusicBrainzError((e as Error).message);
    }
  }

  /** The first row of a query, or undefined. */
  async one<T = Row>(sql: string, params?: readonly Param[]): Promise<T | undefined> {
    return (await this.query<T>(sql, params))[0];
  }

  /** A query's rows as they come, in batches (see {@link RowStream}). */
  stream<T = Row>(sql: string, params?: readonly Param[], options: StreamOptions = {}): RowStream<T> {
    const batch = Math.max(1, options.batchSize ?? 5_000);
    const p = Queries.#params(params);
    return new RowStream<T>((cb) => this.#native.stream(sql, p, batch, cb), options);
  }

  /** Runs a statement; the rows it changed. Needs a database opened with `readOnly: false` to change it. */
  async execute(sql: string, params?: readonly Param[]): Promise<number> {
    try {
      return Number(await this.#native.execute(sql, Queries.#params(params)));
    } catch (e) {
      throw new MusicBrainzError((e as Error).message);
    }
  }

  /** Runs statements that return nothing (`CREATE TEMP TABLE …; …`). */
  exec(sql: string): void {
    try {
      this.#native.executeBatch(sql);
    } catch (e) {
      throw new MusicBrainzError((e as Error).message);
    }
  }

  /** A query's columns and their DuckDB types, without running it. */
  describe(sql: string, params?: readonly Param[]): { name: string; type: string }[] {
    const cols = JSON.parse(this.#native.describe(sql, Queries.#params(params))) as [string, string][];
    return cols.map(([name, type]) => ({ name, type }));
  }

  // ── Lookups ──────────────────────────────────────────────────────────

  /**
   * Rows of a table by a key column (default `id`), one query for all the
   * keys: a map from each key found to its row (the first, if several).
   */
  async byId<T = Row>(
    table: string,
    ids: Iterable<number>,
    options: { column?: string; columns?: string } = {},
  ): Promise<Map<number, T & Row>> {
    const list = [...new Set(ids)];
    const out = new Map<number, T & Row>();
    if (!list.length) return out;
    const col = quoted(options.column ?? "id");
    const rows = await this.query<Row>(
      `SELECT ${col} AS mb_key, ${options.columns ?? "*"} FROM ${tableName(table)} WHERE ${col} IN (SELECT unnest(?::BIGINT[])) ORDER BY rowid`,
      [list],
    );
    for (const { mb_key, ...row } of rows) if (!out.has(mb_key as number)) out.set(mb_key as number, row as T & Row);
    return out;
  }

  /**
   * Rows of a table grouped by a column: for each key, its rows in the
   * dump's order (or `orderBy`). One query for all the keys.
   */
  async rowsOf<T = Row>(
    table: string,
    column: string,
    keys: Iterable<number>,
    options: { columns?: string; orderBy?: string } = {},
  ): Promise<Map<number, (T & Row)[]>> {
    const list = [...new Set(keys)];
    const out = new Map<number, (T & Row)[]>();
    if (!list.length) return out;
    const col = quoted(column);
    const rows = await this.query<Row>(
      `SELECT ${col} AS mb_key, ${options.columns ?? "*"} FROM ${tableName(table)} WHERE ${col} IN (SELECT unnest(?::BIGINT[])) ORDER BY ${options.orderBy ?? "rowid"}`,
      [list],
    );
    for (const { mb_key, ...row } of rows) {
      const k = mb_key as number;
      const group = out.get(k);
      if (group) group.push(row as T & Row);
      else out.set(k, [row as T & Row]);
    }
    return out;
  }

  /**
   * MBIDs to entities: each MBID asked for (in any case) to its entity's id
   * and current MBID, following merges (`<entity>_gid_redirect`). MBIDs of
   * no entity are left out.
   */
  async resolveGids(
    type: EntityType,
    gids: Iterable<string>,
  ): Promise<Map<string, { id: number; gid: string; redirected: boolean }>> {
    const e = entity(type);
    const asked = [...new Set([...gids].map((g) => g.toLowerCase()))].filter((g) => GID.test(g));
    const out = new Map<string, { id: number; gid: string; redirected: boolean }>();
    if (!asked.length) return out;
    const redirects = this.hasTable(`${e}_gid_redirect`);
    const rows = await this.query<{ asked: string; id: number; gid: string; redirected: boolean }>(
      `WITH asked AS (SELECT unnest(?::UUID[]) AS g)
       SELECT CAST(a.g AS VARCHAR) AS asked, t.id, CAST(t.gid AS VARCHAR) AS gid, false AS redirected
       FROM asked a JOIN ${quoted(e)} t ON t.gid = a.g
       ${
         redirects
           ? `UNION ALL SELECT CAST(a.g AS VARCHAR), t.id, CAST(t.gid AS VARCHAR), true
              FROM asked a JOIN ${quoted(`${e}_gid_redirect`)} r ON r.gid = a.g JOIN ${quoted(e)} t ON t.id = r.new_id`
           : ""
       }`,
      [asked],
    );
    for (const r of rows) out.set(r.asked, { id: r.id, gid: r.gid, redirected: r.redirected });
    return out;
  }

  /** Entities' rows by MBID, merges followed: a map from each MBID asked for to its entity's row. */
  async byGid<T = Row>(type: EntityType, gids: Iterable<string>, options: { columns?: string } = {}): Promise<Map<string, T & Row>> {
    const resolved = await this.resolveGids(type, gids);
    const rows = await this.byId<T>(type, [...resolved.values()].map((r) => r.id), options);
    const out = new Map<string, T & Row>();
    for (const [gid, r] of resolved) {
      const row = rows.get(r.id);
      if (row) out.set(gid, row);
    }
    return out;
  }

  /** A small table (`artist_type`, `medium_format`, `link_type`) as `id → name`. */
  async names(table: string, column = "name"): Promise<Map<number, string>> {
    const rows = await this.query<{ id: number; n: string }>(`SELECT id, ${quoted(column)} AS n FROM ${tableName(table)}`);
    return new Map(rows.map((r) => [r.id, r.n]));
  }

  // ── Relationships, credits, aliases, areas ──────────────────────────

  /**
   * Entities' relationships, from either side, with their type, dates,
   * attributes and the other entity: for each id, its relationships
   * (by table, then in the dump's order).
   */
  async relationships(type: EntityType, ids: Iterable<number>, options: RelationshipOptions = {}): Promise<Map<number, Relationship[]>> {
    const e = entity(type);
    const list = [...new Set(ids)];
    const out = new Map<number, Relationship[]>();
    if (!list.length) return out;
    const sides: string[] = [];
    const where = (other: string) => {
      const parts = [];
      if (options.targetTypes?.length) parts.push(`AND r.${other}_type IN (SELECT unnest(from_json(?, '["VARCHAR"]')))`);
      if (options.types?.length) parts.push(`AND r.link_type IN (SELECT unnest(from_json(?, '["VARCHAR"]')))`);
      return parts.join(" ");
    };
    const params: Param[] = [];
    const side = (self: "entity0" | "entity1", other: "entity0" | "entity1", dir: string) => {
      sides.push(
        `SELECT r.${self} AS source, '${dir}' AS direction, r.${other}_type AS target_type, r.${other} AS target_id,
                r.${self}_credit AS credit, r.${other}_credit AS target_credit, r.link_table, r.id, r.link,
                r.link_type, r.link_type_id, CAST(r.link_type_gid AS VARCHAR) AS link_type_gid,
                mb_partial_date(r.begin_date_year, r.begin_date_month, r.begin_date_day) AS begin,
                mb_partial_date(r.end_date_year, r.end_date_month, r.end_date_day) AS "end",
                r.ended, r.link_order, r.row_order AS mb_order
         FROM mb_relationship r
         WHERE r.${self}_type = '${e}' AND r.${self} IN (SELECT unnest(?::INTEGER[])) ${where(other)}`,
      );
      params.push(list);
      if (options.targetTypes?.length) params.push(options.targetTypes);
      if (options.types?.length) params.push(options.types);
    };
    if (options.direction !== "backward") side("entity0", "entity1", "forward");
    if (options.direction !== "forward") side("entity1", "entity0", "backward");
    const rows = await this.query<{
      source: number;
      direction: "forward" | "backward";
      target_type: EntityType;
      target_id: number;
      credit: string;
      target_credit: string;
      link_table: string;
      id: number;
      link: number;
      link_type: string;
      link_type_id: number;
      link_type_gid: string;
      begin: string | null;
      end: string | null;
      ended: boolean;
      link_order: number;
    }>(`${sides.join(" UNION ALL ")} ORDER BY source, link_table, direction DESC, mb_order`, params);

    const attributes = new Map<number, Relationship["attributes"]>();
    if (options.attributes !== false && rows.length && this.hasTable("link_attribute")) {
      const links = [...new Set(rows.map((r) => r.link))];
      for (const a of await this.query<{ link: number; type: string; gid: string; credit: string | null; value: string | null }>(
        `SELECT link, attribute_type AS type, CAST(attribute_type_gid AS VARCHAR) AS gid, credited_as AS credit, text_value AS value
         FROM mb_link_attribute WHERE link IN (SELECT unnest(?::INTEGER[])) ORDER BY link, row_order`,
        [links],
      )) {
        const list = attributes.get(a.link) ?? [];
        list.push({ type: a.type, typeGid: a.gid, credit: a.credit || null, value: a.value });
        attributes.set(a.link, list);
      }
    }

    const targets = new Map<string, { gid: string | null; name: string | null }>();
    if (options.targets !== false) {
      const byType = new Map<EntityType, number[]>();
      for (const r of rows) byType.set(r.target_type, [...(byType.get(r.target_type) ?? []), r.target_id]);
      for (const [t, tids] of byType)
        for (const [id, row] of await this.byId<{ gid: string; n: string }>(t, tids, {
          columns: `CAST(gid AS VARCHAR) AS gid, ${quoted(nameColumn(t))} AS n`,
        }))
          targets.set(`${t}:${id}`, { gid: row.gid, name: row.n });
    }

    for (const r of rows) {
      const t = targets.get(`${r.target_type}:${r.target_id}`);
      const rel: Relationship = {
        id: r.id,
        linkTable: r.link_table,
        type: r.link_type,
        typeId: r.link_type_id,
        typeGid: r.link_type_gid,
        direction: r.direction,
        targetType: r.target_type,
        target: { id: r.target_id, gid: t?.gid ?? null, name: t?.name ?? null },
        begin: r.begin,
        end: r.end,
        ended: r.ended,
        order: r.link_order,
        credit: r.credit || null,
        targetCredit: r.target_credit || null,
        attributes: attributes.get(r.link) ?? [],
      };
      const list = out.get(r.source);
      if (list) list.push(rel);
      else out.set(r.source, [rel]);
    }
    return out;
  }

  /** Entities' URLs (their URL relationships): for each id, `{type, url, ended}` in the dump's order. */
  async urls(
    type: EntityType,
    ids: Iterable<number>,
    options: { types?: string[] } = {},
  ): Promise<Map<number, { id: number; type: string; url: string; ended: boolean; gid: string }[]>> {
    const rels = await this.relationships(type, ids, {
      targetTypes: ["url"],
      attributes: false,
      ...(options.types ? { types: options.types } : {}),
    });
    const out = new Map<number, { id: number; type: string; url: string; ended: boolean; gid: string }[]>();
    for (const [id, list] of rels)
      out.set(
        id,
        list.map((r) => ({ id: r.id, type: r.type, url: r.target.name ?? "", ended: r.ended, gid: r.target.gid ?? "" })),
      );
    return out;
  }

  /** Artist credits by id: their names in order, each with its artist. */
  async credits(ids: Iterable<number>): Promise<Map<number, ArtistCredit>> {
    const list = [...new Set(ids)];
    const out = new Map<number, ArtistCredit>();
    if (!list.length) return out;
    for (const c of await this.query<ArtistCredit>(
      `SELECT ac.id, ac.name,
              list({'position': acn.position, 'name': acn.name, 'joinPhrase': acn.join_phrase,
                    'artist': {'id': a.id, 'gid': CAST(a.gid AS VARCHAR), 'name': a.name, 'sortName': a.sort_name}}
                   ORDER BY acn.position) AS names
       FROM artist_credit ac JOIN artist_credit_name acn ON acn.artist_credit = ac.id JOIN artist a ON a.id = acn.artist
       WHERE ac.id IN (SELECT unnest(?::INTEGER[])) GROUP BY ac.id, ac.name`,
      [list],
    ))
      out.set(c.id, c);
    return out;
  }

  /** Entities' aliases (`<entity>_alias`), in the dump's order. */
  async aliases(type: EntityType, ids: Iterable<number>): Promise<Map<number, Alias[]>> {
    const e = entity(type);
    const list = [...new Set(ids)];
    const out = new Map<number, Alias[]>();
    if (!list.length || !this.hasTable(`${e}_alias`)) return out;
    const types = this.hasTable(`${e}_alias_type`);
    for (const a of await this.query<Alias & { owner: number }>(
      `SELECT x.${quoted(e)} AS owner, x.id, x.name, x.sort_name AS sortName, x.locale, x.primary_for_locale AS "primary",
              ${types ? "t.name" : "NULL"} AS type,
              mb_partial_date(x.begin_date_year, x.begin_date_month, x.begin_date_day) AS begin,
              mb_partial_date(x.end_date_year, x.end_date_month, x.end_date_day) AS "end", x.ended
       FROM ${quoted(`${e}_alias`)} x ${types ? `LEFT JOIN ${quoted(`${e}_alias_type`)} t ON t.id = x.type` : ""}
       WHERE x.${quoted(e)} IN (SELECT unnest(?::INTEGER[])) ORDER BY x.rowid`,
      [list],
    )) {
      const { owner, ...alias } = a;
      const l = out.get(owner);
      if (l) l.push(alias);
      else out.set(owner, [alias]);
    }
    return out;
  }

  /**
   * The ISO 3166-1 code of the country each area is in: the area's own, or
   * the first up its "part of" relationships (a town's parent of the lowest
   * relationship id, up to 16 steps). Areas in none are left out.
   */
  async areaCountries(areaIds: Iterable<number>): Promise<Map<number, string>> {
    const list = [...new Set(areaIds)];
    const out = new Map<number, string>();
    if (!list.length) return out;
    for (const r of await this.query<{ area: number; code: string }>(
      `WITH RECURSIVE parent AS (
         SELECT r.entity1 AS child, arg_min(r.entity0, r.id) AS parent FROM l_area_area r
         JOIN link l ON l.id = r.link JOIN link_type lt ON lt.id = l.link_type
         WHERE lt.name = 'part of' GROUP BY r.entity1),
       walk(start, area, depth) AS (
         SELECT id, id, 0 FROM (SELECT unnest(?::INTEGER[]) AS id)
         UNION ALL
         SELECT w.start, p.parent, w.depth + 1 FROM walk w JOIN parent p ON p.child = w.area
         WHERE w.depth < 16 AND NOT EXISTS (SELECT 1 FROM iso_3166_1 i WHERE i.area = w.area))
       SELECT w.start AS area, arg_min(i.code, w.depth) AS code FROM walk w JOIN iso_3166_1 i ON i.area = w.area GROUP BY w.start`,
      [list],
    ))
      out.set(r.area, r.code);
    return out;
  }

  // ── Entities ─────────────────────────────────────────────────────────

  /**
   * Entities as MusicBrainz's web service shapes them (camelCase): by id
   * or MBID (merges followed), with what `include` asks for. A map from
   * each key asked for that was found to its entity.
   */
  async entities<E extends EntityType>(
    type: E,
    keys: Iterable<number | string>,
    include: readonly EntityInclude[] = [],
  ): Promise<Map<number | string, EntityOf<E>>> {
    return loadEntities(this, entity(type) as E, keys, include);
  }

  /** One entity by id or MBID, or undefined. */
  async entity<E extends EntityType>(type: E, key: number | string, include: readonly EntityInclude[] = []): Promise<EntityOf<E> | undefined> {
    return (await this.entities(type, [key], include)).get(key);
  }

  // ── Changes ──────────────────────────────────────────────────────────

  /** What changed since the previous export (a refreshed database's `mb_changes`). */
  async changes(filter: { entity?: EntityType; change?: Change["change"] } = {}): Promise<Change[]> {
    if (!(await this.query(`SELECT 1 FROM duckdb_tables() WHERE table_name = 'mb_changes'`)).length) return [];
    const where: string[] = [];
    const params: Param[] = [];
    if (filter.entity) {
      where.push("entity = ?");
      params.push(filter.entity);
    }
    if (filter.change) {
      where.push("change = ?");
      params.push(filter.change);
    }
    return this.query<Change>(
      `SELECT entity, id, CAST(gid AS VARCHAR) AS gid, change, tables, CAST(merged_into AS VARCHAR) AS merged_into
       FROM mb_changes ${where.length ? `WHERE ${where.join(" AND ")}` : ""} ORDER BY entity, id`,
      params,
    );
  }
}

/**
 * A MusicBrainz database built by {@link buildDatabase}. Queries run on
 * DuckDB's threads, each on a connection of a pool; `query` and the lookups
 * resolve off the event loop, `querySync` blocks it. For temporary tables,
 * use a {@link Session}.
 */
export class MusicBrainzDatabase extends Queries {
  #db: NativeDatabase;
  #tables: Set<string> | undefined;
  readonly path: string;

  private constructor(native: NativeDatabase, path: string) {
    super(native);
    this.#db = native;
    this.path = path;
  }

  /** Opens a database file. */
  static open(path: string, options: OpenOptions = {}): MusicBrainzDatabase {
    try {
      return new MusicBrainzDatabase(NativeDatabase.open(path, options), path);
    } catch (e) {
      throw toError(e);
    }
  }

  /** What the database was built from (`mb_meta`), and its tables' rows (`mb_tables`). */
  info(): DatabaseInfo {
    return JSON.parse(this.#db.info()) as DatabaseInfo;
  }

  hasTable(name: string): boolean {
    this.#tables ??= new Set(Object.keys(this.info().tables));
    return this.#tables.has(name);
  }

  /**
   * A session: one connection of its own, so temporary tables and settings
   * made in it are there for its later queries (the pool's connections are
   * shared). It has every query and lookup of the database.
   */
  session(): Session {
    try {
      return new Session(this.#db.session(), this);
    } catch (e) {
      throw toError(e);
    }
  }

  /** Runs `f` with a session, closing it after. */
  async withSession<T>(f: (session: Session) => Promise<T>): Promise<T> {
    const s = this.session();
    try {
      return await f(s);
    } finally {
      s.close();
    }
  }

  /** Closes it. */
  close(): void {
    this.#db.close();
  }
}

/** A connection of a database's, held: see {@link MusicBrainzDatabase.session}. */
export class Session extends Queries {
  #session: NativeSession;
  readonly database: MusicBrainzDatabase;

  /** @internal */
  constructor(native: NativeSession, database: MusicBrainzDatabase) {
    super(native);
    this.#session = native;
    this.database = database;
  }

  hasTable(name: string): boolean {
    return this.database.hasTable(name);
  }

  /** Closes it; its temporary tables go with it. */
  close(): void {
    this.#session.close();
  }
}
