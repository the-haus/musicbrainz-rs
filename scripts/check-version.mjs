// Check that every version agrees before a release: the Cargo workspace,
// the pinned internal crate dependencies, the Node package, and a
// CHANGELOG heading. With an argument (a tag such as v0.1.0), that too.
//
//   node scripts/check-version.mjs [v0.1.0]

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(join(root, p), "utf8");
const problems = [];

const cargo = read("Cargo.toml");
const version = /\[workspace\.package\][^[]*?\nversion = "([^"]+)"/s.exec(cargo)?.[1];
if (!version) problems.push("no version in [workspace.package]");
for (const [, name, pin] of cargo.matchAll(/^(musicbrainz-[\w-]+) = \{[^}]*version = "=([^"]+)"/gm)) {
  if (pin !== version) problems.push(`Cargo.toml pins ${name} to =${pin}, not =${version}`);
}
const node = JSON.parse(read("crates/bindings/musicbrainz-node/package.json"));
if (node.version !== version) problems.push(`@musicbrainz-rs/node is ${node.version}, not ${version}`);
if (!new RegExp(`^## ${version.replaceAll(".", "\\.")}( |$)`, "m").test(read("CHANGELOG.md"))) {
  problems.push(`CHANGELOG.md has no "## ${version}" section`);
}
const tag = process.argv[2];
if (tag && tag.replace(/^v/, "") !== version) problems.push(`tag ${tag} is not v${version}`);

if (problems.length) {
  for (const p of problems) console.error(`version check: ${p}`);
  process.exit(1);
}
console.log(version);
