import { describe, expect, it } from "vitest";

import { MusicBrainzDatabase, MusicBrainzError, schema, tableSchema, version } from "../ts/index.js";
import { DAFT_PUNK, fixtureDatabase } from "./helpers.js";

const open = async () => MusicBrainzDatabase.open((await fixtureDatabase()).database);

describe("queries", () => {
  it("binds scalars, and arrays as JSON text", async () => {
    const db = await open();
    const ids = (await db.query<{ id: number }>("SELECT id FROM artist ORDER BY id LIMIT 3")).map((r) => r.id);
    const rows = await db.query<{ id: number }>("SELECT id FROM artist WHERE id IN (SELECT unnest(?::INTEGER[])) ORDER BY id", [ids]);
    expect(rows.map((r) => r.id)).toEqual(ids);
    expect(db.querySync<{ n: number }>("SELECT ?::INTEGER + 1 AS n", [41])).toEqual([{ n: 42 }]);
    expect(await db.one<{ s: string }>("SELECT $1 || $2 AS s", ["a", "b"])).toEqual({ s: "ab" });
    const names = await db.query<{ n: number }>(
      `SELECT count(*) AS n FROM artist WHERE name IN (SELECT unnest(from_json(?, '["VARCHAR"]')))`,
      [["Daft Punk", "nobody's name"]],
    );
    expect(names[0]!.n).toBe(1);
    db.close();
  });

  it("gives nested values, MBIDs, dates and NULLs as JSON", async () => {
    const db = await open();
    const [r] = await db.query<Record<string, unknown>>(
      "SELECT gid, last_updated, [1, 2] AS list, {'a': 1} AS s, NULL AS n FROM artist WHERE gid = ?",
      [DAFT_PUNK],
    );
    expect(r!.gid).toBe(DAFT_PUNK);
    expect(typeof r!.last_updated).toBe("string");
    expect(r!.list).toEqual([1, 2]);
    expect(r!.s).toEqual({ a: 1 });
    expect(r!.n).toBeNull();
    expect(db.describe("SELECT id, gid FROM artist")).toEqual([
      { name: "id", type: "INTEGER" },
      { name: "gid", type: "UUID" },
    ]);
    db.close();
  });

  it("streams rows in batches, and stops when the loop does", async () => {
    const db = await open();
    const stream = db.stream<{ i: number }>("SELECT range AS i FROM range(12345)", [], { batchSize: 1000 });
    let batches = 0;
    let rows = 0;
    for await (const batch of stream.batches()) {
      batches++;
      rows += batch.length;
    }
    expect([batches, rows]).toEqual([13, 12345]);
    expect((await stream.stats).rows).toBe(12345);
    let seen = 0;
    const early = db.stream("SELECT range AS i FROM range(1000000)", [], { batchSize: 100 });
    for await (const _ of early) if (++seen === 250) break;
    expect((await early.stats).stopped).toBe(true);
    expect(await db.stream("SELECT 1 AS x WHERE false").toArray()).toEqual([]);
    db.close();
  });

  it("reports SQL errors as MusicBrainzErrors, and refuses writes when read-only", async () => {
    const db = await open();
    await expect(db.query("SELECT * FROM no_such_table")).rejects.toBeInstanceOf(MusicBrainzError);
    await expect(db.execute("DELETE FROM artist")).rejects.toThrow(/read-only|read only/i);
    db.close();
    expect(() => db.querySync("SELECT 1")).toThrow(/closed/);
  });

  it("keeps temporary tables in a session, which has every lookup", async () => {
    const db = await open();
    await db.withSession(async (s) => {
      s.exec("CREATE TEMP TABLE scope AS SELECT id FROM artist WHERE gid = '" + DAFT_PUNK + "'");
      const rows = await s.query<{ name: string }>("SELECT a.name FROM scope JOIN artist a USING (id)");
      expect(rows).toEqual([{ name: "Daft Punk" }]);
      expect((await s.stream<{ id: number }>("SELECT id FROM scope").toArray()).length).toBe(1);
      expect((await s.byGid<{ name: string }>("artist", [DAFT_PUNK])).get(DAFT_PUNK)?.name).toBe("Daft Punk");
    });
    // Another session does not see it.
    const other = db.session();
    await expect(other.query("SELECT * FROM scope")).rejects.toThrow(/scope/);
    other.close();
    db.close();
  });

  it("has the views and macros", async () => {
    const db = await open();
    expect(await db.query("SELECT mb_partial_date(1994, 5, NULL) AS d, mb_partial_date(NULL, 1, 1) AS e")).toEqual([
      { d: "1994-05", e: null },
    ]);
    expect(await db.one("SELECT mb_wikidata_id('https://www.wikidata.org/wiki/Q1') AS q")).toEqual({ q: "Q1" });
    const rels = await db.query<{ n: number }>("SELECT count(*) AS n FROM mb_relationship WHERE link_table = 'l_artist_url'");
    expect(rels[0]!.n).toBeGreaterThan(0);
    db.close();
  });
});

describe("schema", () => {
  it("is the bundled schema", () => {
    expect(schema().sequence).toBe(31);
    const artist = tableSchema("artist")!;
    expect(artist.columns[1]).toMatchObject({ name: "gid", kind: "uuid", duckdbType: "UUID", nullable: false });
    expect(artist.archive).toBe("mbdump.tar.bz2");
    expect(artist.licence).toBe("CC0");
    expect(tableSchema("l_artist_url")!.linkEntities).toEqual(["artist", "url"]);
    expect(tableSchema("artist_tag")!.licence).toMatch(/NC-SA/);
    expect(version).toMatch(/^\d+\.\d+\.\d+/);
  });
});
