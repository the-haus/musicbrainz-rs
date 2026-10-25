// Set the release version everywhere: the Cargo workspace and its pinned
// internal dependencies, the Node package, Cargo.lock, and a new CHANGELOG
// section (left for you to fill in).
//
//   node scripts/bump-version.mjs 0.2.0

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const next = process.argv[2];
if (!/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(next ?? "")) {
  console.error("usage: node scripts/bump-version.mjs X.Y.Z");
  process.exit(1);
}
const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const edit = (p, f) => writeFileSync(join(root, p), f(readFileSync(join(root, p), "utf8")));

edit("Cargo.toml", (s) =>
  s
    .replace(/(\[workspace\.package\][^[]*?\nversion = ")[^"]+"/s, `$1${next}"`)
    .replace(/^(musicbrainz-[\w-]+ = \{[^}]*version = "=)[^"]+"/gm, `$1${next}"`),
);
edit("crates/bindings/musicbrainz-node/package.json", (s) => s.replace(/("version": ")[^"]+"/, `$1${next}"`));
edit("CHANGELOG.md", (s) =>
  s.includes(`## ${next}`) ? s : s.replace(/^## /m, `## ${next} — unreleased\n\n- …\n\n## `),
);
// Cargo.lock follows the workspace version.
execFileSync("cargo", ["update", "--workspace", "--offline"], { cwd: root, stdio: "inherit" });
console.log(`now ${next}: fill in CHANGELOG.md, commit, then tag v${next}`);
