// What the helper namespaces share: entity keys (an id or an MBID), paging,
// summaries, and the checks that turn a misuse into a clear error rather
// than SQL that fails somewhere inside DuckDB.

import type { Queries } from "../database.js";
import { type EntityType, MusicBrainzError, type Param } from "../types.js";

/** An entity, by its id or its MBID (merges followed). */
export type EntityKey = number | string;

/** An entity as other entities name it. */
export interface Ref {
  id: number;
  gid: string;
  name: string;
}

/** Paging of a list: at most `limit` items (default 100, at most 10,000), from `offset`. */
export interface PageOptions {
  limit?: number;
  offset?: number;
}

/** The SQL quoting of an identifier. */
export const ident = (name: string) => `"${name.replace(/"/g, '""')}"`;

/** An entity type's alias table. */
export const aliasTable = (type: EntityType) => `${type}_alias`;

/** `LIMIT … OFFSET …` for a page, checked. */
export function pageSql(o: PageOptions = {}, fallback = 100, max = 10_000): string {
  const limit = o.limit ?? fallback;
  const offset = o.offset ?? 0;
  if (!Number.isInteger(limit) || limit < 0 || limit > max)
    throw new MusicBrainzError(`limit must be an integer from 0 to ${max}, not ${limit}`, "invalid");
  if (!Number.isInteger(offset) || offset < 0)
    throw new MusicBrainzError(`offset must be a non-negative integer, not ${offset}`, "invalid");
  return `LIMIT ${limit} OFFSET ${offset}`;
}

/** Checks the tables a helper reads were loaded. */
export function need(db: Queries, what: string, ...tables: string[]): void {
  const missing = tables.filter((t) => !db.hasTable(t));
  if (missing.length)
    throw new MusicBrainzError(
      `${what} needs ${missing.join(", ")}, which the database was built without`,
      "unavailable",
    );
}

/** Whether every one of these tables was loaded. */
export const has = (db: Queries, ...tables: string[]) => tables.every((t) => db.hasTable(t));

/** Checks an id: a safe integer. */
export function checkId(id: number): number {
  if (!Number.isSafeInteger(id)) throw new MusicBrainzError(`not an id: ${id}`, "invalid");
  return id;
}

/**
 * An entity's id from its key: an id as it is (whether or not there is
 * one), an MBID resolved (merges followed). Undefined for an MBID of no
 * entity.
 */
export async function idOf(db: Queries, type: EntityType, key: EntityKey): Promise<number | undefined> {
  if (typeof key === "number") return checkId(key);
  if (typeof key !== "string") throw new MusicBrainzError(`not an id or MBID: ${String(key)}`, "invalid");
  return (await db.resolveGids(type, [key])).get(key.toLowerCase())?.id;
}

/** Entities' ids from their keys, in the order given (keys of no entity left out). */
export async function idsOf(db: Queries, type: EntityType, keys: Iterable<EntityKey>): Promise<number[]> {
  const list = [...keys];
  const gids = list.filter((k): k is string => typeof k === "string");
  const resolved = gids.length ? await db.resolveGids(type, gids) : new Map();
  const out: number[] = [];
  for (const k of list) {
    if (typeof k === "number") out.push(checkId(k));
    else {
      const r = resolved.get(k.toLowerCase());
      if (r) out.push(r.id);
    }
  }
  return [...new Set(out)];
}

/** `CAST(x AS VARCHAR)`: MBIDs as text, whatever the database's type. */
export const text = (expr: string) => `CAST(${expr} AS VARCHAR)`;

/** A partial date of a table's `<prefix>_year/_month/_day` columns. */
export const dateOf = (alias: string, prefix: string) =>
  `mb_partial_date(${alias}.${prefix}_year, ${alias}.${prefix}_month, ${alias}.${prefix}_day)`;

/** A partial date as `YYYY-MM-DD`-sortable text (checked). */
export function checkDate(d: string | undefined, name: string): string | undefined {
  if (d === undefined) return undefined;
  if (!/^\d{4}(-\d{2}(-\d{2})?)?$/.test(d))
    throw new MusicBrainzError(`${name} must be YYYY, YYYY-MM or YYYY-MM-DD, not ${JSON.stringify(d)}`, "invalid");
  return d;
}

/**
 * Normalised text for matching names: lower case, accents off, trimmed. (Runs
 * of blanks are not squeezed: a regular expression over 40 M recording names
 * costs more than the rest of a search.)
 */
export const normalized = (expr: string) => `trim(lower(strip_accents(${expr})))`;

/** The same normalisation in JS (for parameters). */
export const normalize = (s: string) =>
  s
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .trim();

/** A list parameter of strings: `IN (SELECT unnest(from_json(?, '["VARCHAR"]')))`. */
export const inStrings = `(SELECT unnest(from_json(?, '["VARCHAR"]')))`;
/** A list parameter of integers. */
export const inInts = `(SELECT unnest(?::BIGINT[]))`;

/** Parameters, typed. */
export type Params = Param[];
