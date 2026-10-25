import { describe, expect, it } from "vitest";

import { MusicBrainzDatabase } from "../ts/index.js";
import { DAFT_PUNK, fixtureDatabase } from "./helpers.js";

const open = async (indexes: "keys" | "foreign" = "foreign") =>
  MusicBrainzDatabase.open((await fixtureDatabase(indexes)).database);

describe("lookups", () => {
  it("resolves MBIDs, in any case, following merges", async () => {
    const db = await open();
    const redirect = await db.one<{ gid: string; target: string }>(
      `SELECT CAST(r.gid AS VARCHAR) AS gid, CAST(a.gid AS VARCHAR) AS target
       FROM artist_gid_redirect r JOIN artist a ON a.id = r.new_id LIMIT 1`,
    );
    const asked = [DAFT_PUNK.toUpperCase(), "00000000-0000-0000-0000-000000000000", "not an mbid"];
    if (redirect) asked.push(redirect.gid);
    const resolved = await db.resolveGids("artist", asked);
    expect(resolved.get(DAFT_PUNK)).toMatchObject({ gid: DAFT_PUNK, redirected: false });
    expect(resolved.has("00000000-0000-0000-0000-000000000000")).toBe(false);
    if (redirect) expect(resolved.get(redirect.gid)).toMatchObject({ gid: redirect.target, redirected: true });
    const rows = await db.byGid<{ name: string }>("artist", [DAFT_PUNK]);
    expect(rows.get(DAFT_PUNK)?.name).toBe("Daft Punk");
    db.close();
  });

  it("finds rows by id and by a foreign key, in the dump's order", async () => {
    const db = await open();
    const dp = (await db.byGid<{ id: number }>("artist", [DAFT_PUNK])).get(DAFT_PUNK)!;
    const byId = await db.byId<{ name: string }>("artist", [dp.id, dp.id, -1]);
    expect([...byId.keys()]).toEqual([dp.id]);
    const aliases = await db.rowsOf<{ name: string }>("artist_alias", "artist", [dp.id]);
    const direct = await db.query<{ name: string }>("SELECT name FROM artist_alias WHERE artist = ? ORDER BY rowid", [dp.id]);
    expect(aliases.get(dp.id)?.map((a) => a.name) ?? []).toEqual(direct.map((a) => a.name));
    const types = await db.names("artist_type");
    expect([...types.values()]).toContain("Group");
    db.close();
  });

  it("gives relationships from either side, with types, targets and attributes", async () => {
    const db = await open();
    const dp = (await db.byGid<{ id: number }>("artist", [DAFT_PUNK])).get(DAFT_PUNK)!;
    const rels = (await db.relationships("artist", [dp.id])).get(dp.id) ?? [];
    const members = rels.filter((r) => r.type === "member of band" && r.direction === "backward");
    expect(members.map((m) => m.target.name).sort()).toEqual(
      expect.arrayContaining(["Guy\u2010Manuel de Homem\u2010Christo", "Thomas Bangalter"]),
    );
    expect(members[0]!.targetType).toBe("artist");
    expect(members[0]!.target.gid).toMatch(/^[0-9a-f-]{36}$/);
    const urls = (await db.urls("artist", [dp.id])).get(dp.id) ?? [];
    expect(urls.length).toBeGreaterThan(0);
    expect(urls.every((u) => u.url.startsWith("http"))).toBe(true);
    const onlyUrls = (await db.relationships("artist", [dp.id], { targetTypes: ["url"] })).get(dp.id) ?? [];
    expect(onlyUrls.every((r) => r.targetType === "url")).toBe(true);
    const forward = (await db.relationships("artist", [dp.id], { direction: "forward", types: ["member of band"] })).get(dp.id);
    expect(forward ?? []).toEqual([]);
    // Attributes: a membership with an instrument or a vocal, wherever there is one.
    const withAttrs = await db.one<{ entity0: number; type: string }>(
      `SELECT r.entity0, r.link_table FROM mb_relationship r JOIN link_attribute la ON la.link = r.link
       WHERE r.entity0_type = 'artist' LIMIT 1`,
    );
    if (withAttrs) {
      const list = (await db.relationships("artist", [withAttrs.entity0])).get(withAttrs.entity0) ?? [];
      expect(list.some((r) => r.attributes.length > 0)).toBe(true);
    }
    db.close();
  });

  it("gives artist credits with every name in order, and aliases with their types", async () => {
    const db = await open();
    const credit = await db.one<{ id: number; n: number }>(
      "SELECT artist_credit AS id, count(*) AS n FROM artist_credit_name GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 1",
    );
    const c = (await db.credits([credit!.id])).get(credit!.id)!;
    expect(c.names.length).toBe(credit!.n);
    expect(c.names.map((n) => n.position)).toEqual([...c.names.map((n) => n.position)].sort((a, b) => a - b));
    expect(c.names[0]!.artist.gid).toMatch(/^[0-9a-f-]{36}$/);
    const dp = (await db.byGid<{ id: number }>("artist", [DAFT_PUNK])).get(DAFT_PUNK)!;
    const aliases = (await db.aliases("artist", [dp.id])).get(dp.id) ?? [];
    for (const a of aliases) expect(typeof a.primary).toBe("boolean");
    db.close();
  });

  it("finds the country an area is in", async () => {
    const db = await open();
    const dp = (await db.byGid<{ area: number }>("artist", [DAFT_PUNK])).get(DAFT_PUNK)!;
    const countries = await db.areaCountries([dp.area]);
    expect(countries.get(dp.area)).toBe("FR");
    db.close();
  });

  it("works the same without foreign key indexes", async () => {
    const a = await open("keys");
    const b = await open("foreign");
    const dp = (await a.byGid<{ id: number }>("artist", [DAFT_PUNK])).get(DAFT_PUNK)!;
    const ra = (await a.relationships("artist", [dp.id])).get(dp.id);
    const rb = (await b.relationships("artist", [dp.id])).get(dp.id);
    expect(ra).toEqual(rb);
    a.close();
    b.close();
  });
});

describe("entities", () => {
  it("gives an artist as the web service would", async () => {
    const db = await open();
    const artist = await db.entity("artist", DAFT_PUNK, ["aliases", "urls", "relationships", "redirects", "identifiers"]);
    expect(artist).toMatchObject({ gid: DAFT_PUNK, name: "Daft Punk", type: "Group", sortName: "Daft Punk" });
    expect(artist!.area).toMatchObject({ name: "France", iso3166_1: ["FR"] });
    expect(artist!.lifeSpan.begin).toMatch(/^1993/);
    expect(artist!.lifeSpan.ended).toBe(true);
    expect(Array.isArray(artist!.aliases)).toBe(true);
    expect(artist!.urls!.length).toBeGreaterThan(0);
    expect(artist!.relationships!.length).toBeGreaterThan(0);
    expect(Array.isArray(artist!.redirects)).toBe(true);
    expect(Array.isArray(artist!.ipis)).toBe(true);
    // By id too, and both at once.
    const both = await db.entities("artist", [artist!.id, DAFT_PUNK]);
    expect(both.get(artist!.id)).toEqual(both.get(DAFT_PUNK));
    db.close();
  });

  it("gives release groups with their releases, and releases with media, tracks and recordings", async () => {
    const db = await open();
    const rgId = (await db.one<{ id: number }>("SELECT release_group AS id FROM release ORDER BY id LIMIT 1"))!.id;
    const rg = (await db.entity("release_group", rgId, ["releases"]))!;
    expect(rg.artistCredit?.names.length).toBeGreaterThan(0);
    expect(rg.releases!.length).toBeGreaterThan(0);
    expect(rg.firstReleaseDate === null || /^\d{4}/.test(rg.firstReleaseDate)).toBe(true);
    const releaseId = rg.releases![0]!.id;
    const release = (await db.entity("release", releaseId, ["media"]))!;
    expect(release.releaseGroup).toMatchObject({ id: rgId, gid: rg.gid });
    const tracks = await db.one<{ n: number }>(
      "SELECT count(*) AS n FROM track t JOIN medium m ON m.id = t.medium WHERE m.release = ?",
      [releaseId],
    );
    expect(release.media!.flatMap((m) => m.tracks).length).toBe(tracks!.n);
    const track = release.media![0]!.tracks[0]!;
    expect(track.recording.gid).toMatch(/^[0-9a-f-]{36}$/);
    const recording = (await db.entity("recording", track.recording.id))!;
    expect(recording.name).toBe(track.recording.name);
    expect(Array.isArray(recording.isrcs)).toBe(true);
    db.close();
  });

  it("gives the other types", async () => {
    const db = await open();
    for (const type of ["area", "label", "work", "place", "event", "series", "url", "genre", "instrument"] as const) {
      const first = await db.one<{ id: number }>(`SELECT id FROM ${type} ORDER BY id LIMIT 1`).catch(() => undefined);
      if (!first) continue;
      const e = await db.entity(type, first.id, ["aliases", "relationships"]);
      expect(e?.id, type).toBe(first.id);
      expect(e!.gid, type).toMatch(/^[0-9a-f-]{36}$/);
    }
    const area = await db.entity("area", (await db.one<{ area: number }>("SELECT area FROM iso_3166_1 WHERE code = 'FR'"))!.area);
    expect(area).toMatchObject({ name: "France", iso3166_1: ["FR"] });
    db.close();
  });
});
