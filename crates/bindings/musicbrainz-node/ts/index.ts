// @musicbrainz-rs/node: MusicBrainz full exports as DuckDB databases, in
// Node.js. Build one from mbdump.tar.bz2 in one pass, refresh it from a
// newer export with a diff of what changed, and query it with SQL or the
// lookups here.

import * as native from "./native.js";
import { runTask, type TaskOptions } from "./task.js";
import type {
  BuildOptions,
  BuildProgress,
  BuildStats,
  DiffStats,
  DownloadOptions,
  DownloadProgress,
  DownloadStats,
  ExportLayout,
  ExtractOptions,
  ExtractProgress,
  ExtractStats,
  Member,
  RefreshOptions,
  TableSchema,
} from "./types.js";

export { MusicBrainzDatabase, Queries, Session, tableName } from "./database.js";
export type { Alias, ArtistCredit, OpenOptions, Relationship, RelationshipOptions } from "./database.js";
export type * from "./entities.js";
export { RowStream } from "./task.js";
export type { StreamOptions, StreamStats, TaskOptions } from "./task.js";
export * from "./types.js";

/** Where MetaBrainz publishes the full exports. */
export const FULL_EXPORTS = "https://data.metabrainz.org/pub/musicbrainz/data/fullexport";

/**
 * Builds a database from an export (see {@link BuildOptions}): every table
 * of `mbdump.tar.bz2` (or those asked for), typed from the schema SQL of
 * the dump's `SCHEMA_SEQUENCE`, indexed. Tables already taken out are read
 * where they are; the others are decompressed on every core and loaded as
 * each one arrives. Refuses to replace an existing database unless
 * `replace` (see {@link refreshDatabase}).
 */
export function buildDatabase(options: BuildOptions, task: TaskOptions<BuildProgress> = {}): Promise<BuildStats> {
  return runTask((cb) => native.build(JSON.stringify(options), cb), task);
}

/**
 * Builds a database anew from a (newer) export beside the one at
 * `options.database`, diffs the two (`mb_changes` in the new one says
 * which MBIDs were added, changed, removed or merged, and in which
 * tables), and puts the new one in its place. Readers of the old file
 * keep reading it until they reopen. Use `changes()` on the reopened
 * database to see what changed.
 */
export function refreshDatabase(options: RefreshOptions, task: TaskOptions<BuildProgress> = {}): Promise<BuildStats> {
  return runTask((cb) => native.refresh(JSON.stringify(options), cb), task);
}

/** Diffs two databases, writing `mb_changes` into the newer. */
export function diffDatabases(older: string, newer: string, task: TaskOptions<never> = {}): Promise<DiffStats> {
  return runTask((cb) => native.diff(older, newer, cb), task);
}

/**
 * Takes tables out of an archive into `dir` (`<dir>/mbdump/<table>`, the
 * metadata files beside), each written beside and renamed when whole.
 */
export function extractArchive(
  archive: string,
  dir: string,
  options: ExtractOptions & TaskOptions<ExtractProgress> = {},
): Promise<ExtractStats> {
  const { onMember, onProgress, signal, ...rest } = options;
  return runTask(
    (cb) => native.extract(archive, dir, JSON.stringify(rest), cb),
    { onProgress, signal },
    (type, data) => {
      if (type === "member") onMember?.(data as Member & { path: string });
    },
  );
}

/**
 * Downloads an export's archives (the newest without `name`) into
 * `<dir>/<name>/`, resuming a part left by a stopped download, checked
 * against the published SHA-256 before they get their final name.
 */
export function downloadExport(options: DownloadOptions, task: TaskOptions<DownloadProgress> = {}): Promise<DownloadStats> {
  return runTask((cb) => native.download(JSON.stringify(options), cb), task);
}

/** The newest full export's name (`20261017-002244`). */
export async function latestExport(options: { base?: string; userAgent?: string } = {}): Promise<string> {
  return JSON.parse(await native.latestExport(options.base, options.userAgent)) as string;
}

/** An export's published SHA-256 checksums, by file. */
export async function exportChecksums(
  name: string,
  options: { base?: string; userAgent?: string } = {},
): Promise<Record<string, string>> {
  return JSON.parse(await native.exportChecksums(name, options.base, options.userAgent)) as Record<string, string>;
}

/** An archive's members (reads the whole archive). */
export async function archiveMembers(archive: string, options: { threads?: number } = {}): Promise<Member[]> {
  return JSON.parse(await native.archiveMembers(archive, options.threads)) as Member[];
}

/** Where an export's files are and what its metadata says (an archive, export folder or tables folder). */
export async function exportInfo(path: string): Promise<ExportLayout> {
  return JSON.parse(await native.exportInfo(path)) as ExportLayout;
}

let schemaCache: { sequence: number; tables: TableSchema[] } | undefined;

/** The schema bundled with this package: its sequence and every table. */
export function schema(): { sequence: number; tables: TableSchema[] } {
  return (schemaCache ??= JSON.parse(native.schema()) as { sequence: number; tables: TableSchema[] });
}

/** One table of the bundled schema. */
export function tableSchema(name: string): TableSchema | undefined {
  return schema().tables.find((t) => t.name === name);
}

/** The native addon's version. */
export const version: string = native.nativeVersion();
