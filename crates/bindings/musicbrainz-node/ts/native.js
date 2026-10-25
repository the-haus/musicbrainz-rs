// Native module loader. Picks the platform-specific prebuilt binary if
// present, otherwise the dev build produced by `napi build` at the crate
// root.
//
// napi-rs appends an ABI suffix on some platforms:
//   - Linux GNU:   musicbrainz-node.linux-x64-gnu.node
//   - Linux musl:  musicbrainz-node.linux-x64-musl.node
//   - Windows:     musicbrainz-node.win32-x64-msvc.node
//   - macOS:       musicbrainz-node.darwin-arm64.node  (no ABI suffix)
//
// Published installs resolve the binary through an optional platform
// subpackage named `@musicbrainz-rs/node-<triple>`. `MUSICBRAINZ_RS_NODE_BINARY`
// names a binary to load instead (for bundled apps that ship it apart).
//
// native.esm.js is the same loader for the ESM build; keep them in step.

"use strict";

// Intentional CJS: `.node` addons are loaded with require().
/* eslint-disable @typescript-eslint/no-require-imports */

const { platform, arch } = process;
const { existsSync } = require("node:fs");
const { join } = require("node:path");

// A glibc build of Node reports the glibc it runs on; a musl build does not.
// (`ldd --version` is no help: musl's prints to stderr and exits 1.)
function isMuslLibc() {
  try {
    const header = process.report?.getReport()?.header;
    if (header) return !header.glibcVersionRuntime;
  } catch {
    /* ignore */
  }
  try {
    return require("node:fs").readFileSync("/usr/bin/ldd", "utf8").includes("musl");
  } catch {
    return false;
  }
}

function candidateTriples() {
  if (platform === "linux") {
    const libc = isMuslLibc() ? "musl" : "gnu";
    return [`${platform}-${arch}-${libc}`, `${platform}-${arch}`];
  }
  if (platform === "win32") {
    return [`${platform}-${arch}-msvc`, `${platform}-${arch}`];
  }
  return [`${platform}-${arch}`];
}

function loadNative() {
  const override = process.env.MUSICBRAINZ_RS_NODE_BINARY;
  if (override) return require(override);
  const triples = candidateTriples();
  const localCandidates = [...triples.map((t) => `musicbrainz-node.${t}.node`), "musicbrainz-node.node"];
  for (const name of localCandidates) {
    const candidate = join(__dirname, "..", name);
    if (existsSync(candidate)) {
      return require(candidate);
    }
  }
  const subpackageNames = triples.map((t) => `@musicbrainz-rs/node-${t}`);
  for (const name of subpackageNames) {
    try {
      return require(name);
    } catch (err) {
      if (err && err.code !== "MODULE_NOT_FOUND") throw err;
    }
  }
  throw new Error(
    `@musicbrainz-rs/node: no native binary found for ${platform}-${arch}. ` +
      `Tried local: ${localCandidates.join(", ")}. ` +
      `Tried npm: ${subpackageNames.join(", ")}. ` +
      "Set MUSICBRAINZ_RS_NODE_BINARY to the binary's path, or, for local development, run `yarn build` in crates/bindings/musicbrainz-node.",
  );
}

module.exports = loadNative();
