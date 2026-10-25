import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

import { describe, expect, it } from "vitest";

import { archiveMembers, buildDatabase, extractArchive, exportInfo, MusicBrainzDatabase } from "../ts/index.js";
import { DAFT_PUNK, FIXTURE, tempDir } from "./helpers.js";

const has = (cmd: string) => {
  try {
    execFileSync(cmd, ["--version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
};

/** The fixture as MetaBrainz ships it: `<dir>/<name>/mbdump.tar.bz2`, made with the system's tar and bzip2. */
function archive(): string {
  const dir = path.join(tempDir(), "20261017-002244");
  execFileSync("mkdir", ["-p", dir]);
  const tar = path.join(dir, "mbdump.tar");
  execFileSync("tar", ["-cf", tar, "-C", FIXTURE, "TIMESTAMP", "SCHEMA_SEQUENCE", "REPLICATION_SEQUENCE", "mbdump"], {
    env: { ...process.env, COPYFILE_DISABLE: "1" },
  });
  execFileSync("bzip2", ["-9", tar]);
  return path.join(dir, "mbdump.tar.bz2");
}

describe.runIf(process.platform !== "win32" && has("bzip2"))("archives", () => {
  const file = archive();
  const tables = readdirSync(path.join(FIXTURE, "mbdump")).sort();

  it("lists members and reads the export's metadata", async () => {
    const members = await archiveMembers(file);
    expect(members.filter((m) => m.table).map((m) => m.table).sort()).toEqual(tables);
    expect(members.find((m) => m.name === "SCHEMA_SEQUENCE")?.size).toBe(3);
    const info = await exportInfo(path.dirname(file));
    expect(info).toMatchObject({ name: "20261017-002244", schemaSequence: 31 });
    expect(info.archives).toEqual([file]);
  });

  it("extracts the tables asked for, byte for byte", async () => {
    const out = tempDir();
    const seen: string[] = [];
    const stats = await extractArchive(file, out, { tables: ["artist", "area"], onMember: (m) => seen.push(m.name) });
    expect(seen).toEqual(expect.arrayContaining(["mbdump/artist", "mbdump/area", "SCHEMA_SEQUENCE"]));
    expect(stats.written.length).toBe(seen.length);
    expect(readFileSync(path.join(out, "mbdump", "artist"))).toEqual(readFileSync(path.join(FIXTURE, "mbdump", "artist")));
    expect(existsSync(path.join(out, "mbdump", "track"))).toBe(false);
    const again = await extractArchive(file, out, { tables: ["artist"] });
    expect(again.kept).toContain("mbdump/artist");
  });

  it("builds a database straight from the archive, leaving no extracted table behind", async () => {
    const database = path.join(path.dirname(file), "..", "mb.duckdb");
    const stats = await buildDatabase({ source: path.dirname(file), database });
    expect(stats.export.name).toBe("20261017-002244");
    const fromArchive = stats.tables.filter((t) => t.from === "archive").map((t) => t.name).sort();
    expect(fromArchive).toEqual(tables);
    // The core's tables the archive lacks are there, empty.
    expect(stats.tables.some((t) => t.from === "empty")).toBe(true);
    expect(existsSync(`${database}.work`)).toBe(false);
    const db = MusicBrainzDatabase.open(database);
    expect((await db.byGid<{ name: string }>("artist", [DAFT_PUNK])).get(DAFT_PUNK)?.name).toBe("Daft Punk");
    db.close();
  });

  it("rejects a corrupt archive", async () => {
    const bad = path.join(tempDir(), "mbdump.tar.bz2");
    const bytes = readFileSync(file);
    bytes[Math.floor(bytes.length / 2)]! ^= 0xff;
    writeFileSync(bad, bytes);
    await expect(archiveMembers(bad)).rejects.toThrow(/bzip2|CRC|corrupt|tar/i);
  });
});
