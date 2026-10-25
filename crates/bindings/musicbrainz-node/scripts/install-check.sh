#!/bin/sh
# Install the packed @musicbrainz-rs/node from packages/ (see release-node.yml)
# into an empty project, with the platform package for one triple, and use it
# from ESM and CommonJS: build a database from the fixture export and query it.
# Run from the repository root:
#
#   sh crates/bindings/musicbrainz-node/scripts/install-check.sh linux-x64-gnu
set -eu
TRIPLE="$1"
rm -rf install-check
mkdir install-check
cd install-check
npm init -y >/dev/null
# Relative paths, which npm on Windows understands too. In a dry run the
# other platform packages are not on the registry; npm skips optional
# dependencies it cannot fetch.
npm install --no-audit --no-fund ../packages/musicbrainz-rs-node-[0-9]*.tgz "../packages/musicbrainz-rs-node-$TRIPLE-"*.tgz
ls node_modules/@musicbrainz-rs
cp -r ../fixtures/daft-punk export
cat > esm.mjs <<'JS'
import { buildDatabase, MusicBrainzDatabase, version } from "@musicbrainz-rs/node";
const stats = await buildDatabase({ source: "export", database: "esm.duckdb" });
const db = MusicBrainzDatabase.open("esm.duckdb");
const [a] = await db.query("SELECT name FROM artist WHERE gid = ?", ["056e4f3e-d505-4dad-8ec1-d04f521cbb56"]);
if (a?.name !== "Daft Punk") throw new Error(`expected Daft Punk, got ${JSON.stringify(a)}`);
console.log(`esm: ${version} on ${process.platform}-${process.arch}, node ${process.version}: ${stats.tables.length} tables`);
db.close();
JS
cat > cjs.cjs <<'JS'
const { MusicBrainzDatabase } = require("@musicbrainz-rs/node");
(async () => {
  const db = MusicBrainzDatabase.open("esm.duckdb");
  const artist = await db.entity("artist", "056e4f3e-d505-4dad-8ec1-d04f521cbb56", ["aliases", "urls"]);
  if (!artist || artist.area?.name !== "France") throw new Error(`expected an artist from France, got ${JSON.stringify(artist?.area)}`);
  console.log(`cjs: ${artist.name}, ${artist.urls.length} urls`);
  db.close();
})();
JS
node esm.mjs
node cjs.cjs
node -e 'require.resolve("@musicbrainz-rs/node/package.json"); console.log("package.json: exported")'
