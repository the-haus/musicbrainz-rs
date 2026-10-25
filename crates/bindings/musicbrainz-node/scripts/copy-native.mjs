// The compiled wrappers load `./native.js` relative to themselves; copy
// the loaders and their types next to the emitted files: the CommonJS
// build in dist/, the ESM build in dist/esm/ (marked as ESM by its own
// package.json).
import { copyFileSync, writeFileSync } from "node:fs";

copyFileSync("ts/native.js", "dist/native.js");
copyFileSync("ts/native.d.ts", "dist/native.d.ts");
copyFileSync("ts/native.esm.js", "dist/esm/native.js");
copyFileSync("ts/native.d.ts", "dist/esm/native.d.ts");
writeFileSync("dist/esm/package.json", JSON.stringify({ type: "module" }, null, 2) + "\n");
