import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

import { describe, expect, it } from "vitest";

import {
  type BuildProgress,
  buildDatabase,
  exportInfo,
  MusicBrainzDatabase,
  MusicBrainzError,
  refreshDatabase,
} from "../ts/index.js";
import { DAFT_PUNK, FIXTURE, fixtureCopy, fixtureDatabase, tempDir } from "./helpers.js";

describe("buildDatabase", () => {
  it("loads every table of the fixture, typed, with its metadata", async () => {
    const { database, stats } = await fixtureDatabase();
    expect(stats.export.schemaSequence).toBe(31);
    expect(stats.export.name).toBeNull();
    const loaded = stats.tables.filter((t) => t.from === "tables");
    expect(loaded.length).toBeGreaterThan(40);
    expect(loaded.every((t) => t.rows > 0)).toBe(true);
    const db = MusicBrainzDatabase.open(database);
    const info = db.info();
    expect(info.meta.schema_sequence).toBe("31");
    expect(info.meta.indexes).toBe("foreign");
    expect(info.tables.artist).toBeGreaterThan(0);
    const [artist] = await db.query<{ name: string; gid: string; ended: boolean; type: number }>(
      "SELECT name, gid, ended, type FROM artist WHERE gid = ?",
      [DAFT_PUNK],
    );
    expect(artist).toMatchObject({ name: "Daft Punk", gid: DAFT_PUNK });
    expect(typeof artist!.ended).toBe("boolean");
    db.close();
  });

  it("reports progress, refuses to replace a database, and stops on a signal", async () => {
    const dir = tempDir();
    const database = path.join(dir, "a.duckdb");
    const phases = new Set<BuildProgress["phase"]>();
    await buildDatabase({ source: FIXTURE, database, tables: ["artist", "area"] }, { onProgress: (p) => phases.add(p.phase) });
    expect(phases.has("schema")).toBe(true);
    await expect(buildDatabase({ source: FIXTURE, database })).rejects.toThrow(/exists/);
    const controller = new AbortController();
    controller.abort();
    const stopped = buildDatabase({ source: FIXTURE, database: path.join(dir, "b.duckdb") }, { signal: controller.signal });
    await expect(stopped).rejects.toMatchObject({ code: "aborted" });
    expect(existsSync(path.join(dir, "b.duckdb"))).toBe(false);
  });

  it("fails on a dump of another schema, naming the table", async () => {
    const dir = tempDir();
    const source = fixtureCopy(dir);
    writeFileSync(path.join(source, "mbdump", "artist"), "1\ttoo few\n");
    const err = await buildDatabase({ source, database: path.join(dir, "x.duckdb") }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(MusicBrainzError);
    expect(String((err as Error).message)).toMatch(/artist: 2 fields in the dump, 19 columns/);
    expect(existsSync(path.join(dir, "x.duckdb"))).toBe(false);
  });

  it("loads a hand-made dump leniently: names for MBIDs, NULLs anywhere", async () => {
    const dir = tempDir();
    const source = path.join(dir, "mini");
    mkdirSync(path.join(source, "mbdump"), { recursive: true });
    writeFileSync(path.join(source, "SCHEMA_SEQUENCE"), "31\n");
    // id, gid, name and \N for the rest (sort_name is NOT NULL in the schema).
    writeFileSync(path.join(source, "mbdump", "artist"), `1\tartist-a\tA${"\t\\N".repeat(16)}\n`);
    writeFileSync(path.join(source, "mbdump", "artist_gid_redirect"), "artist-old\t1\t\\N\n");
    const database = path.join(dir, "mini.duckdb");
    await expect(buildDatabase({ source, database })).rejects.toThrow(/artist/);
    await buildDatabase({ source, database, lenient: true });
    const db = MusicBrainzDatabase.open(database);
    expect(db.isLenient()).toBe(true);
    const resolved = await db.resolveGids("artist", ["artist-a", "artist-old"]);
    expect(resolved.get("artist-a")).toMatchObject({ id: 1, redirected: false });
    expect(resolved.get("artist-old")).toMatchObject({ id: 1, gid: "artist-a", redirected: true });
    db.close();
  });

  it("finds an export's layout", async () => {
    const info = await exportInfo(FIXTURE);
    expect(info.schemaSequence).toBe(31);
    expect(info.tables).toBe(FIXTURE);
    expect(info.archives).toEqual([]);
  });
});

describe("refreshDatabase", () => {
  it("builds the newer export, lists what changed, and swaps it in", async () => {
    const dir = tempDir();
    const older = fixtureCopy(dir, "20261014-001756");
    const database = path.join(dir, "mb.duckdb");
    await buildDatabase({ source: older, database, tables: ["artist", "artist_alias", "artist_gid_redirect", "area"] });

    // The next export: Daft Punk renamed, a new artist.
    const newer = fixtureCopy(dir, "20261017-002244");
    const file = path.join(newer, "mbdump", "artist");
    const lines = readFileSync(file, "utf8").split("\n");
    const i = lines.findIndex((l) => l.split("\t")[1] === DAFT_PUNK);
    const fields = lines[i]!.split("\t");
    fields[2] = "Daft Punk (renamed)";
    lines[i] = fields.join("\t");
    writeFileSync(file, lines.join("\n"));
    const added = [...fields];
    added[0] = "999999999";
    added[1] = "11111111-2222-4333-8444-555555555555";
    appendFileSync(file, `${added.join("\t")}\n`);

    const stats = await refreshDatabase({
      source: newer,
      database,
      tables: ["artist", "artist_alias", "artist_gid_redirect", "area"],
      keepPrevious: true,
    });
    expect(stats.diff?.fromExport).toBe("20261014-001756");
    expect(stats.diff?.toExport).toBe("20261017-002244");
    expect(stats.diff?.entities.artist).toMatchObject({ added: 1, changed: 1, removed: 0, merged: 0 });
    expect(existsSync(`${database}.previous`)).toBe(true);

    const db = MusicBrainzDatabase.open(database);
    const changes = await db.changes({ entity: "artist" });
    expect(changes.map((c) => [c.gid, c.change, c.tables])).toEqual(
      expect.arrayContaining([
        [DAFT_PUNK, "changed", ["artist"]],
        ["11111111-2222-4333-8444-555555555555", "added", []],
      ]),
    );
    expect(db.info().meta.export_name).toBe("20261017-002244");
    db.close();
  });
});
