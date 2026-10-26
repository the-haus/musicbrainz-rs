// Public types of @musicbrainz-rs/node.

/**
 * Why something failed: `aborted` (a signal or `close()`), `invalid` (a
 * misuse: a bad argument), `unavailable` (the database was built without a
 * table it needs), `failed`.
 */
export type ErrorCode = "aborted" | "invalid" | "unavailable" | "failed";

/** Every error this package throws. */
export class MusicBrainzError extends Error {
  constructor(
    message: string,
    readonly code: ErrorCode = "failed",
  ) {
    super(message);
    this.name = "MusicBrainzError";
  }
}

/** Which indexes a build makes. */
export type IndexLevel = "none" | "keys" | "foreign";

/** Options of {@link buildDatabase} and {@link refreshDatabase}. */
export interface BuildOptions {
  /**
   * An export: its archive (`…/20261017-002244/mbdump.tar.bz2`), its
   * folder, or a folder of tables taken out of it (`<dir>/mbdump/<table>`).
   */
  source: string;
  /** The database file to write (`musicbrainz.duckdb`). */
  database: string;
  /** Only these tables (default: every table of the archives read). */
  tables?: string[];
  /**
   * Archives besides the CC0 core whose tables to load, by file name
   * (`mbdump-derived.tar.bz2`); mind their licences ({@link TableSchema.licence}).
   */
  archives?: string[];
  /** Where tables are extracted to (default `<database>.work`). */
  workDir?: string;
  /** Keep the extracted tables in `workDir`. */
  keepTables?: boolean;
  /**
   * `keys` (default): ids and MBIDs. `foreign`: also every foreign key
   * column, for fast lookups of an entity's rows. `none`.
   */
  indexes?: IndexLevel;
  /**
   * musicbrainz-server's `admin/sql` folder to read the dump with (default:
   * the bundled schema, or the one of the dump's `SCHEMA_SEQUENCE` fetched
   * from GitHub into `schemaCache`).
   */
  schemaDir?: string;
  /** Where fetched schemas are cached (default `<database's folder>/.musicbrainz-schema`). */
  schemaCache?: string;
  /** Never fetch a schema. */
  offline?: boolean;
  /** Threads (default: every core). */
  threads?: number;
  /** DuckDB's memory limit (`"8GB"`; default 80% of RAM). */
  memoryLimit?: string;
  /** Replace an existing database ({@link buildDatabase} refuses otherwise). */
  replace?: boolean;
  /**
   * For hand-made test dumps: no `NOT NULL`s (the dump may leave such a
   * column `\N`), and MBID columns as text (it may name things `artist-a`).
   * Queries that compare MBIDs as text work on both kinds of database.
   */
  lenient?: boolean;
}

/** Options of {@link refreshDatabase}. */
export interface RefreshOptions extends Omit<BuildOptions, "replace"> {
  /** Keep the old database as `<database>.previous`. */
  keepPrevious?: boolean;
}

/** A build's progress. */
export interface BuildProgress {
  phase: "schema" | "extract" | "load" | "index" | "views" | "diff" | "done";
  /** The table being loaded or indexed. */
  table: string | null;
  tablesLoaded: number;
  /** An estimate for a schema other than the bundled one. */
  tablesExpected: number;
  rows: number;
  /** Compressed bytes read of the archives. */
  bytesIn: number;
  bytesTotal: number;
  bytesOut: number;
  elapsedMs: number;
}

/** One table a build loaded. */
export interface TableStats {
  name: string;
  rows: number;
  bytes: number;
  loadMs: number;
  /** `tables` (a folder of tables) or `archive`. */
  from: "tables" | "archive";
}

/** An export's metadata. */
export interface ExportInfo {
  /** `20261017-002244`. */
  name: string | null;
  /** When it was taken: `2026-10-17 00:22:45.12891+00`. */
  timestamp: string | null;
  schemaSequence: number | null;
  replicationSequence: number | null;
}

/** Counts of one entity type's changes. */
export interface EntityDiff {
  added: number;
  changed: number;
  removed: number;
  merged: number;
}

/** What a diff found (the rows are in `mb_changes`). */
export interface DiffStats {
  fromExport: string | null;
  toExport: string | null;
  entities: Record<string, EntityDiff>;
  elapsedMs: number;
}

/** What a build did. */
export interface BuildStats {
  database: string;
  export: ExportInfo;
  /** `bundled`, or the SQL folder read. */
  schema: string;
  tables: TableStats[];
  /** Members not loaded, with why. */
  skipped: [string, string][];
  /** Refreshes: what changed since the previous database. */
  diff: DiffStats | null;
  elapsedMs: number;
}

/** A change between two exports, as `mb_changes` lists it. */
export interface Change {
  entity: EntityType;
  id: number;
  gid: string;
  change: "added" | "changed" | "removed" | "merged";
  /** The tables whose rows of it changed (`artist`, `artist_alias`, `l_artist_url`, …). */
  tables: string[];
  /** For `merged`: the MBID it redirects to now. */
  merged_into: string | null;
}

/** Download progress. */
export interface DownloadProgress {
  file: string;
  bytes: number;
  total: number | null;
  fetched: number;
  elapsedMs: number;
}

/** Options of {@link downloadExport}. */
export interface DownloadOptions {
  /** The folder exports are kept in: files go to `<dir>/<name>/`. */
  dir: string;
  /** The export (`20261017-002244`; default the newest). */
  name?: string;
  /** Where the exports are (a mirror's; default MetaBrainz's). */
  base?: string;
  /** The archives to fetch (default `["mbdump.tar.bz2"]`). */
  files?: string[];
  userAgent?: string;
  /** Attempts after a dropped connection without progress (default 8). */
  retries?: number;
}

/** What a download did. */
export interface DownloadStats {
  name: string;
  dir: string;
  files: string[];
  fetched: number;
  alreadyDone: number;
  elapsedMs: number;
}

/** Extraction progress. */
export interface ExtractProgress {
  bytesIn: number;
  bytesTotal: number;
  bytesOut: number;
  member: string | null;
  members: number;
  elapsedMs: number;
}

/** A member of an archive. */
export interface Member {
  /** `mbdump/artist`, `TIMESTAMP`. */
  name: string;
  size: number;
  /** The table, for members under `mbdump/`. */
  table: string | null;
}

/** Options of {@link extractArchive}. */
export interface ExtractOptions {
  /** Only these tables (metadata files are always written). */
  tables?: string[];
  /** Keep members already there with the right size (default true). */
  skipExisting?: boolean;
  threads?: number;
  /** As each member is in place. */
  onMember?: (member: Member & { path: string }) => void;
}

/** What an extraction did. */
export interface ExtractStats {
  written: string[];
  kept: string[];
  skipped: number;
  stopped: boolean;
  bytesOut: number;
  elapsedMs: number;
}

/** An export's files and metadata. */
export interface ExportLayout extends ExportInfo {
  archives: string[];
  /** The folder holding `mbdump/`, when tables were taken out. */
  tables: string | null;
  dir: string | null;
}

/** A column of the schema. */
export interface ColumnSchema {
  name: string;
  /** As musicbrainz-server's SQL writes it: `VARCHAR(255)`. */
  sqlType: string;
  kind:
    | "integer"
    | "float"
    | "boolean"
    | "uuid"
    | "text"
    | "timestamp"
    | "date"
    | "time"
    | "interval"
    | "point"
    | "json"
    | "integerArray"
    | "textArray"
    | "other";
  nullable: boolean;
  /** Its type in the database. */
  duckdbType: string;
}

/** A table of the schema. */
export interface TableSchema {
  name: string;
  /** Its PostgreSQL schema: `musicbrainz`, `cover_art_archive`, … */
  schema: string;
  columns: ColumnSchema[];
  primaryKey: string[];
  foreignKeys: { column: string; table: string; references: string }[];
  /** The archive of a full export it is in. */
  archive: string | null;
  /** That archive's licence: `CC0` (the core) or `CC BY-NC-SA 3.0`. */
  licence: string | null;
  /** For a relationship table (`l_artist_url`): its two entity types. */
  linkEntities: [EntityType, EntityType] | null;
}

/** The entity types relationships join. */
export const ENTITY_TYPES = [
  "area",
  "artist",
  "event",
  "genre",
  "instrument",
  "label",
  "place",
  "recording",
  "release",
  "release_group",
  "series",
  "url",
  "work",
] as const;

/** An entity type, as its table is named. */
export type EntityType = (typeof ENTITY_TYPES)[number];

/** A database's metadata (`mb_meta`) and tables (`mb_tables`). */
export interface DatabaseInfo {
  path: string;
  meta: {
    format?: string;
    builder?: string;
    export_name?: string;
    timestamp?: string;
    schema_sequence?: string;
    replication_sequence?: string;
    schema?: string;
    source?: string;
    indexes?: IndexLevel;
    built_at?: string;
    lenient?: "true" | "false";
    diff_from_export?: string;
    [key: string]: string | undefined;
  };
  /** Rows by table. */
  tables: Record<string, number>;
}

/** A row: column name to value. */
export type Row = Record<string, unknown>;

/** A query parameter: scalars bind as they are, arrays and objects as JSON text (cast them: `?::INTEGER[]`). */
export type Param = string | number | boolean | null | readonly unknown[] | { readonly [key: string]: unknown };
