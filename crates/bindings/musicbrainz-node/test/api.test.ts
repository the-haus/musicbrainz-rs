import { describe, expect, it } from "vitest";

import { externalIds, MusicBrainzDatabase, parseUrl, SERVICES } from "../ts/index.js";
import { DAFT_PUNK, fixtureDatabase } from "./helpers.js";

const DISCOVERY = "48117b90-a16e-34ca-a514-19c702df1158";
const RAM = "aa997ea0-2936-40bd-884d-3af8a0e064dc";
const BANGALTER = "Thomas Bangalter";
const GUY_MAN = "Guy‐Manuel de Homem‐Christo";

const open = async () => MusicBrainzDatabase.open((await fixtureDatabase()).database);

describe("search", () => {
  it("finds artists exactly, ignoring case and accents, best first", async () => {
    const db = await open();
    const exact = await db.search.artists("DAFT punk ", { mode: "exact" });
    expect(exact[0]).toMatchObject({ gid: DAFT_PUNK, name: "Daft Punk", score: 1, matchedBy: "name", type: "artist" });
    expect(exact[0]!.popularity).toBeGreaterThan(0);
    expect(await db.search.artists("dàft púnk", { mode: "exact" })).toHaveLength(1);
    // Containing: Daft Punk first (exact), then the others.
    const contains = await db.search.artists("daft punk");
    expect(contains[0]!.name).toBe("Daft Punk");
    expect(contains.length).toBeGreaterThan(1);
    expect(contains.every((h, i) => i === 0 || h.score <= contains[i - 1]!.score)).toBe(true);
    db.close();
  });

  it("finds by prefix, fuzzily, by alias, and pages", async () => {
    const db = await open();
    expect((await db.search.artists("daft", { mode: "prefix" })).map((h) => h.name)).toContain("Daft Punk");
    const fuzzy = await db.search.artists("daft pank", { mode: "fuzzy" });
    expect(fuzzy[0]!.name).toBe("Daft Punk");
    expect(fuzzy[0]!.score).toBeLessThan(0.8);
    const all = await db.search.artists("a", { limit: 1_000 });
    const page = await db.search.artists("a", { limit: 3, offset: 2 });
    expect(page.map((h) => h.id)).toEqual(all.slice(2, 5).map((h) => h.id));
    const noAliases = await db.search.artists("daft punk", { aliases: false });
    expect(noAliases.every((h) => h.matchedBy === "name")).toBe(true);
    expect(await db.search.artists("   ")).toEqual([]);
    db.close();
  });

  it("searches every other type, and refuses misuse", async () => {
    const db = await open();
    expect((await db.search.releaseGroups("discovery", { mode: "exact" }))[0]).toMatchObject({ gid: DISCOVERY, type: "release_group" });
    expect((await db.search.recordings("one more time", { mode: "exact" }))[0]!.name).toBe("One More Time");
    expect((await db.search.areas("france", { mode: "exact" }))[0]!.name).toBe("France");
    expect((await db.search.genres("house", { mode: "exact" }))[0]!.name).toBe("house");
    await expect(db.search.entities("url", "x")).rejects.toMatchObject({ code: "invalid" });
    await expect(db.search.artists("x", { mode: "nope" as never })).rejects.toMatchObject({ code: "invalid" });
    await expect(db.search.artists("x", { limit: -1 })).rejects.toMatchObject({ code: "invalid" });
    await expect(db.search.artists("x", { minSimilarity: 2 })).rejects.toMatchObject({ code: "invalid" });
    db.close();
  });
});

describe("artists", () => {
  it("gives a discography, filtered by type", async () => {
    const db = await open();
    const albums = await db.artists.discography(DAFT_PUNK, { primaryTypes: ["Album"], excludeSecondary: true });
    expect(albums.map((g) => g.name)).toEqual(["Discovery", "Random Access Memories"]);
    expect(albums[0]).toMatchObject({ gid: DISCOVERY, primaryType: "Album", secondaryTypes: [], artistCredit: "Daft Punk" });
    expect(albums[0]!.firstReleaseDate).toMatch(/^2001/);
    const all = await db.artists.discography(DAFT_PUNK);
    expect(all.length).toBeGreaterThan(albums.length);
    // Oldest first.
    const dates = all.map((g) => g.firstReleaseDate ?? "9999");
    expect([...dates].sort()).toEqual(dates);
    // By id or MBID alike; an unknown artist has none.
    const id = (await db.artists.get(DAFT_PUNK))!.id;
    expect((await db.artists.discography(id)).length).toBe(all.length);
    expect(await db.artists.discography("00000000-0000-0000-0000-000000000000")).toEqual([]);
    db.close();
  });

  it("gives releases, recordings and works", async () => {
    const db = await open();
    const releases = await db.artists.releases(DAFT_PUNK, { status: ["Official"] });
    expect(releases.every((r) => r.status === "Official")).toBe(true);
    expect(releases.some((r) => r.name === "Discovery" && r.releaseGroup.gid === DISCOVERY)).toBe(true);
    const recordings = await db.artists.recordings(DAFT_PUNK, { limit: 500 });
    expect(recordings.map((r) => r.name)).toContain("One More Time");
    const names = recordings.map((r) => r.name);
    expect([...names].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0))).toEqual(names);
    const works = await db.artists.works(DAFT_PUNK);
    expect(works.every((w) => w.roles.length > 0)).toBe(true);
    db.close();
  });

  it("gives members and bands, related artists and collaborators", async () => {
    const db = await open();
    const members = await db.artists.members(DAFT_PUNK);
    expect(members.map((m) => m.artist.name).sort()).toEqual([GUY_MAN, BANGALTER].sort());
    expect(members[0]).toMatchObject({ begin: "1993", ended: true });
    expect(members[0]!.roles).toContain("original");
    expect(await db.artists.members(DAFT_PUNK, { current: true })).toEqual([]);
    const bangalter = members.find((m) => m.artist.name === BANGALTER)!.artist;
    expect((await db.artists.memberOf(bangalter.gid)).map((m) => m.artist.name)).toContain("Daft Punk");
    const related = await db.artists.related(DAFT_PUNK);
    expect(related.every((r) => r.type !== "member of band")).toBe(true);
    const collaborators = await db.artists.collaborators(DAFT_PUNK);
    expect(collaborators.map((c) => c.name)).toContain("Pharrell Williams");
    expect(collaborators.every((c, i) => i === 0 || c.credits <= collaborators[i - 1]!.credits)).toBe(true);
    db.close();
  });

  it("gives events, labels, country, URLs, external ids, aliases and counts", async () => {
    const db = await open();
    const events = await db.artists.events(DAFT_PUNK);
    expect(events.length).toBeGreaterThan(0);
    expect(events[0]!.roles.length).toBeGreaterThan(0);
    const begins = events.map((e) => e.begin ?? "");
    expect([...begins].sort().reverse()).toEqual(begins);
    const since2010 = await db.artists.events(DAFT_PUNK, { from: "2010" });
    expect(since2010.every((e) => (e.begin ?? "") >= "2010")).toBe(true);
    const until2007 = await db.artists.events(DAFT_PUNK, { to: "2007" });
    expect(until2007.every((e) => (e.begin ?? "9999") <= "2007-99")).toBe(true);
    await expect(db.artists.events(DAFT_PUNK, { from: "last year" })).rejects.toMatchObject({ code: "invalid" });
    expect((await db.artists.labels(DAFT_PUNK)).map((l) => l.name)).toContain("Virgin");
    expect(await db.artists.country(DAFT_PUNK)).toBe("FR");
    const urls = await db.artists.urls(DAFT_PUNK);
    expect(urls.length).toBeGreaterThan(5);
    const ids = await db.artists.externalIds(DAFT_PUNK);
    expect(ids.wikidata).toEqual(["Q185828"]);
    expect(ids.discogs).toContain("artist/1289");
    expect(Array.isArray(await db.artists.aliases(DAFT_PUNK))).toBe(true);
    const counts = await db.artists.counts(DAFT_PUNK);
    expect(counts!.releaseGroups).toBe((await db.artists.discography(DAFT_PUNK, { officialOnly: false })).length);
    expect(await db.artists.counts(999_999_999)).toBeUndefined();
    db.close();
  });

  it("finds artists by name and by their ids elsewhere", async () => {
    const db = await open();
    expect((await db.artists.find("Daft Punk"))[0]!.gid).toBe(DAFT_PUNK);
    expect((await db.artists.byExternalId("wikidata", "Q185828")).map((o) => o.gid)).toEqual([DAFT_PUNK]);
    expect((await db.artists.byExternalId("discogs", "artist/1289"))[0]!.name).toBe("Daft Punk");
    expect(await db.artists.byExternalId("wikidata", "Q1")).toEqual([]);
    db.close();
  });
});

describe("release groups, releases, recordings and works", () => {
  it("gives a group's releases and its original release with its tracklist", async () => {
    const db = await open();
    const releases = await db.releaseGroups.releases(DISCOVERY);
    expect(releases.length).toBeGreaterThan(0);
    expect(releases.every((r) => r.releaseGroup.gid === DISCOVERY)).toBe(true);
    const original = (await db.releaseGroups.original(DISCOVERY))!;
    expect(original.name).toBe("Discovery");
    const tracks = original.media!.flatMap((m) => m.tracks);
    expect(tracks).toHaveLength(14);
    expect(tracks[0]!.name).toBe("One More Time");
    expect(original.date).toBe(releases.map((r) => r.date).sort()[0]);
    expect(await db.releaseGroups.original("00000000-0000-0000-0000-000000000000")).toBeUndefined();
    db.close();
  });

  it("finds releases by barcode and catalogue number, with their labels, events and versions", async () => {
    const db = await open();
    const original = (await db.releaseGroups.original(DISCOVERY))!;
    expect((await db.releases.tracklist(original.gid))[0]!.tracks[0]!.number).toBe("1");
    expect((await db.releases.byBarcode(original.barcode!)).map((r) => r.id)).toContain(original.id);
    const cat = original.labels.find((l) => l.catalogNumber)!.catalogNumber!;
    expect((await db.releases.byCatalogNumber(cat.toLowerCase().replace(/-/g, "-"))).map((r) => r.id)).toContain(original.id);
    expect((await db.releases.byCatalogNumber(cat, { label: original.labels[0]!.label!.gid })).map((r) => r.id)).toContain(original.id);
    expect(await db.releases.labels(original.id)).toEqual(original.labels);
    expect((await db.releases.events(original.id)).length).toBeGreaterThan(0);
    const others = await db.releases.otherVersions(original.id);
    expect(others.every((r) => r.id !== original.id && r.releaseGroup.gid === DISCOVERY)).toBe(true);
    expect((await db.releases.recordings(original.id)).map((r) => r.name)).toContain("Digital Love");
    expect(await db.releases.byBarcode("  ")).toEqual([]);
    db.close();
  });

  it("finds recordings by ISRC, where they appear, and works by ISWC with their writers", async () => {
    const db = await open();
    const original = (await db.releaseGroups.original(DISCOVERY))!;
    const one = original.media![0]!.tracks[0]!.recording;
    const isrc = (await db.recordings.isrcs(one.id))[0]!;
    expect((await db.recordings.byIsrc(isrc.toLowerCase())).map((r) => r.gid)).toContain(one.gid);
    await expect(db.recordings.byIsrc("not an isrc")).rejects.toMatchObject({ code: "invalid" });
    const appearances = await db.recordings.appearances(one.gid);
    expect(appearances.some((a) => a.release.id === original.id && a.position === 1)).toBe(true);
    const work = (await db.recordings.works(one.id))[0]!;
    expect(work.name).toBe("One More Time");
    const writers = await db.works.writers(work.gid);
    expect(writers.map((w) => w.artist.name)).toContain(BANGALTER);
    expect(writers.find((w) => w.artist.name === BANGALTER)!.roles).toContain("composer");
    const iswc = (await db.works.iswcs(work.id))[0]!;
    expect((await db.works.byIswc(iswc)).map((w) => w.gid)).toContain(work.gid);
    await expect(db.works.byIswc("123")).rejects.toMatchObject({ code: "invalid" });
    expect((await db.works.recordings(work.id)).map((r) => r.id)).toContain(one.id);
    db.close();
  });
});

describe("labels, areas, places, events, series, genres", () => {
  it("gives a label's releases, artists and family, and labels by code", async () => {
    const db = await open();
    const original = (await db.releaseGroups.original(DISCOVERY))!;
    const label = original.labels.find((l) => l.label)!.label!;
    const releases = await db.labels.releases(label.id);
    expect(releases.some((r) => r.id === original.id && r.catalogNumber)).toBe(true);
    expect((await db.labels.artists(label.id)).map((a) => a.name)).toContain("Daft Punk");
    expect(Array.isArray(await db.labels.parents(label.id))).toBe(true);
    const full = (await db.labels.get(label.id))!;
    if (full.labelCode) expect((await db.labels.byLabelCode(`LC ${full.labelCode}`)).map((l) => l.id)).toContain(label.id);
    await expect(db.labels.byLabelCode("none")).rejects.toMatchObject({ code: "invalid" });
    db.close();
  });

  it("walks the area tree", async () => {
    const db = await open();
    const france = (await db.areas.byIsoCode("fr"))!;
    expect(france.name).toBe("France");
    expect(await db.areas.country(france.id)).toBe("FR");
    const dp = (await db.artists.get(DAFT_PUNK))!;
    const beginArea = dp.beginArea!;
    const up = await db.areas.ancestors(beginArea.id);
    expect(up.map((a) => a.name)).toContain("France");
    expect(await db.areas.country(beginArea.gid)).toBe("FR");
    expect((await db.areas.children(up[0]!.id)).map((a) => a.id)).toContain(beginArea.id);
    expect((await db.areas.artists(france.id, { deep: true })).map((a) => a.gid)).toContain(DAFT_PUNK);
    expect(await db.areas.byIsoCode("ZZ")).toBeUndefined();
    db.close();
  });

  it("gives places' events and what is near a point", async () => {
    const db = await open();
    const event = (await db.artists.events(DAFT_PUNK)).find((e) => e.place)!;
    const place = (await db.places.get(event.place!.id))!;
    expect((await db.places.events(place.id)).map((e) => e.id)).toContain(event.id);
    if (place.coordinates) {
      const near = await db.places.near(place.coordinates.latitude, place.coordinates.longitude, { radiusKm: 1 });
      expect(near[0]).toMatchObject({ id: place.id });
      expect(near[0]!.distanceKm).toBeLessThan(0.01);
    }
    await expect(db.places.near(91, 0)).rejects.toMatchObject({ code: "invalid" });
    await expect(db.places.near(0, 0, { radiusKm: 0 })).rejects.toMatchObject({ code: "invalid" });
    db.close();
  });

  it("gives events' lineups, places and a period's events", async () => {
    const db = await open();
    const event = (await db.artists.events(DAFT_PUNK)).find((e) => e.place)!;
    expect((await db.events.lineup(event.id)).map((p) => p.artist.gid)).toContain(DAFT_PUNK);
    expect((await db.events.place(event.gid))!.id).toBe(event.place!.id);
    expect(Array.isArray(await db.events.series(event.id))).toBe(true);
    const day = event.begin!.slice(0, 10);
    expect((await db.events.between(day, day)).map((e) => e.id)).toContain(event.id);
    db.close();
  });

  it("gives genres", async () => {
    const db = await open();
    const all = await db.genres.all();
    expect(all.length).toBeGreaterThan(1000);
    expect((await db.genres.byName("House"))!.name).toBe("house");
    expect(Array.isArray(await db.genres.subgenres((await db.genres.byName("house"))!.id))).toBe(true);
    db.close();
  });

  it("gives link types and statistics", async () => {
    const db = await open();
    const types = await db.linkTypes("url", "artist");
    expect(types.map((t) => t.name)).toContain("wikidata");
    expect(types.every((t) => t.entityType0 === "artist" && t.entityType1 === "url")).toBe(true);
    const stats = await db.stats();
    expect(stats.entities.artist).toBeGreaterThan(0);
    db.close();
  });
});

describe("external ids", () => {
  it("reads ids out of URLs", () => {
    expect(parseUrl("https://www.wikidata.org/wiki/Q185828")).toEqual({ service: "wikidata", id: "Q185828" });
    expect(parseUrl("https://www.discogs.com/artist/1289")).toEqual({ service: "discogs", id: "artist/1289" });
    expect(parseUrl("https://open.spotify.com/artist/4tZwfgrHOc3mvqYlEYSvVi")).toEqual({
      service: "spotify",
      id: "artist/4tZwfgrHOc3mvqYlEYSvVi",
    });
    expect(parseUrl("https://en.wikipedia.org/wiki/Daft_Punk")).toEqual({ service: "wikipedia", id: "en/Daft_Punk" });
    expect(parseUrl("https://www.openstreetmap.org/node/123")).toEqual({ service: "openstreetmap", id: "node/123" });
    expect(parseUrl("https://example.com/")).toBeUndefined();
    expect(externalIds(["https://www.discogs.com/artist/1", "https://www.discogs.com/artist/1", "https://x.com/daftpunk"])).toEqual({
      discogs: ["artist/1"],
      twitter: ["daftpunk"],
    });
    expect(Object.keys(SERVICES).length).toBeGreaterThan(25);
  });

  it("finds whose a URL is", async () => {
    const db = await open();
    const owners = await db.external.byUrl("https://www.wikidata.org/wiki/Q185828");
    expect(owners).toEqual([expect.objectContaining({ type: "artist", gid: DAFT_PUNK, relationship: "wikidata" })]);
    expect(await db.external.byUrl("https://example.com/nothing")).toEqual([]);
    const releaseIds = await db.external.idsOf("release_group", [RAM, DISCOVERY]);
    expect(releaseIds.size).toBeLessThanOrEqual(2);
    await expect(db.external.find("nope" as never, "x")).rejects.toMatchObject({ code: "invalid" });
    db.close();
  });
});
