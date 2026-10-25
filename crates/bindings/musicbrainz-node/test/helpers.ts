import { cpSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { buildDatabase, type BuildStats } from "../ts/index.js";

/** The fixture export: a consistent slice of the 2026-10-17 core export around Daft Punk. */
export const FIXTURE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../../../fixtures/daft-punk");

export const DAFT_PUNK = "056e4f3e-d505-4dad-8ec1-d04f521cbb56";

/** A temporary folder, removed when the process exits. */
export function tempDir(prefix = "mb-node-"): string {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  process.on("exit", () => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** A copy of the fixture export in a folder of its own (tests change it). */
export function fixtureCopy(dir: string, name = "20261017-002244"): string {
  const to = path.join(dir, name);
  cpSync(FIXTURE, to, { recursive: true });
  return to;
}

const built = new Map<string, Promise<{ database: string; stats: BuildStats }>>();

/** The fixture built once per test file (per `indexes`). */
export function fixtureDatabase(indexes: "keys" | "foreign" = "foreign"): Promise<{ database: string; stats: BuildStats }> {
  let p = built.get(indexes);
  if (!p) {
    const dir = tempDir();
    const database = path.join(dir, "mb.duckdb");
    p = buildDatabase({ source: FIXTURE, database, indexes }).then((stats) => ({ database, stats }));
    built.set(indexes, p);
  }
  return p;
}
