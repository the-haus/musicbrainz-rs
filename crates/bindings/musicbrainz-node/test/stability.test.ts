// Misuse, edge cases and load: what a library others depend on must survive.

import { copyFileSync, existsSync, writeFileSync } from "node:fs";
import path from "node:path";

import { describe, expect, it } from "vitest";

import { buildDatabase, MusicBrainzDatabase, MusicBrainzError } from "../ts/index.js";
import { DAFT_PUNK, FIXTURE, fixtureDatabase, tempDir } from "./helpers.js";

const open = async () => MusicBrainzDatabase.open((await fixtureDatabase()).database);

describe("arguments", () => {
  it("refuses table and column names that are not identifiers", async () => {
    const db = await open();
    for (const bad of ["artist; DROP TABLE artist", 'artist"', "", "1abc", "Artist"])
      await expect(db.byId(bad, [1])).rejects.toMatchObject({ code: "invalid" });
    await expect(db.byId("artist", [1], { column: "id OR 1=1" })).rejects.toMatchObject({ code: "invalid" });
    await expect(db.rowsOf("artist_alias", "artist) --", [1])).rejects.toMatchObject({ code: "invalid" });
    await expect(db.names("artist_type", "name, (SELECT 1)")).rejects.toMatchObject({ code: "invalid" });
    db.close();
  });

  it("refuses ids that are not integers, and takes empty lists", async () => {
    const db = await open();
    for (const bad of [1.5, Number.NaN, Infinity, 2 ** 60])
      await expect(db.byId("artist", [bad])).rejects.toMatchObject({ code: "invalid" });
    expect((await db.byId("artist", [])).size).toBe(0);
    expect((await db.relationships("artist", [])).size).toBe(0);
    expect((await db.credits([])).size).toBe(0);
    expect((await db.entities("artist", [])).size).toBe(0);
    expect((await db.resolveGids("artist", ["", "  ", "x", 42 as never])).size).toBe(0);
    await expect(db.artists.discography(1.5)).rejects.toMatchObject({ code: "invalid" });
    await expect(db.artists.discography({} as never)).rejects.toMatchObject({ code: "invalid" });
    await expect(db.entity("nope" as never, 1)).rejects.toMatchObject({ code: "invalid" });
    db.close();
  });

  it("binds any text as a parameter, never as SQL", async () => {
    const db = await open();
    const nasty = ["'; DROP TABLE artist; --", "\\N", "tab\there", "line\nbreak", "quote\"s", "emoji 🎧", "nul\u0000byte"];
    for (const s of nasty) expect(await db.one<{ s: string }>("SELECT ? AS s", [s])).toEqual({ s });
    expect(await db.search.artists("'; DROP TABLE artist; --")).toEqual([]);
    expect((await db.stats()).entities.artist).toBeGreaterThan(0);
    db.close();
  });
});

describe("load and lifetime", () => {
  it("runs many queries at once on the pool", async () => {
    const db = await open();
    const results = await Promise.all(
      Array.from({ length: 64 }, (_, i) => db.one<{ n: number }>("SELECT count(*) + ? AS n FROM artist", [i])),
    );
    const base = results[0]!.n;
    expect(results.map((r) => r!.n)).toEqual(results.map((_, i) => base + i));
    const helpers = await Promise.all(Array.from({ length: 16 }, () => db.artists.members(DAFT_PUNK)));
    expect(new Set(helpers.map((m) => m.length)).size).toBe(1);
    db.close();
  });

  it("runs streams and queries side by side, and streams survive their consumer stopping", async () => {
    const db = await open();
    const streams = Array.from({ length: 8 }, () => db.stream<{ i: number }>("SELECT range AS i FROM range(20000)", [], { batchSize: 500 }));
    const counts = await Promise.all(
      streams.map(async (s, k) => {
        let n = 0;
        for await (const _ of s) if (++n === 1_000 * (k + 1) && k % 2 === 0) break;
        return n;
      }),
    );
    expect(counts.filter((_, k) => k % 2 === 1).every((n) => n === 20_000)).toBe(true);
    expect((await db.one<{ n: number }>("SELECT 1 AS n"))!.n).toBe(1);
    db.close();
  });

  it("fails cleanly after close", async () => {
    const db = await open();
    db.close();
    db.close();
    await expect(db.query("SELECT 1")).rejects.toBeInstanceOf(MusicBrainzError);
    expect(() => db.querySync("SELECT 1")).toThrow(MusicBrainzError);
    await expect(db.stream("SELECT 1").toArray()).rejects.toBeInstanceOf(MusicBrainzError);
    expect(() => db.session()).toThrow(MusicBrainzError);
  });

  it("opens DuckDB files of its own and others', and refuses a newer format", async () => {
    const dir = tempDir();
    await expect(Promise.resolve().then(() => MusicBrainzDatabase.open(path.join(dir, "missing.duckdb")))).rejects.toBeInstanceOf(
      MusicBrainzError,
    );
    // A database built otherwise: SQL works, its info is empty.
    const plain = path.join(dir, "plain.duckdb");
    await buildDatabase({ source: FIXTURE, database: plain, tables: ["artist"] });
    const rw = MusicBrainzDatabase.open(plain, { readOnly: false });
    rw.exec("DROP TABLE mb_meta; DROP TABLE mb_tables; CHECKPOINT;");
    rw.close();
    const other = MusicBrainzDatabase.open(plain);
    expect(other.info().meta).toEqual({});
    expect((await other.one<{ n: number }>("SELECT count(*) AS n FROM artist"))!.n).toBeGreaterThan(0);
    other.close();
    // One of a newer format.
    const newer = path.join(dir, "newer.duckdb");
    copyFileSync((await fixtureDatabase()).database, newer);
    const w = MusicBrainzDatabase.open(newer, { readOnly: false });
    w.exec("UPDATE mb_meta SET value = '999' WHERE key = 'format'; CHECKPOINT;");
    w.close();
    expect(() => MusicBrainzDatabase.open(newer)).toThrow(/format 999/);
    expect(existsSync(newer)).toBe(true);
    writeFileSync(path.join(dir, "junk.duckdb"), "not a database");
    expect(() => MusicBrainzDatabase.open(path.join(dir, "junk.duckdb"))).toThrow(MusicBrainzError);
  });
});

describe("databases built with fewer tables", () => {
  it("says which table a helper needs, or answers with nothing", async () => {
    const dir = tempDir();
    const database = path.join(dir, "few.duckdb");
    await buildDatabase({ source: FIXTURE, database, tables: ["artist", "artist_alias", "area"] });
    const db = MusicBrainzDatabase.open(database);
    await expect(db.artists.discography(DAFT_PUNK)).rejects.toMatchObject({ code: "unavailable", message: /artist_credit_name/ });
    expect(await db.artists.members(DAFT_PUNK)).toEqual([]);
    expect(await db.artists.events(DAFT_PUNK)).toEqual([]);
    expect(await db.artists.urls(DAFT_PUNK)).toEqual([]);
    expect(await db.artists.country(DAFT_PUNK)).toBeNull();
    expect(await db.external.byUrl("https://www.wikidata.org/wiki/Q185828")).toEqual([]);
    expect((await db.search.artists("daft punk", { mode: "exact" }))[0]!.gid).toBe(DAFT_PUNK);
    expect(await db.genres.graph()).toEqual([]);
    expect(await db.linkTypes()).toEqual([]);
    db.close();
  });
});
