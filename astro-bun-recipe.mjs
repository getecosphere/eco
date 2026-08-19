// astro-bun-recipe.mjs — turns an Astro @astrojs/node build into a
// bun-compilable server whose client assets ship as static files alongside
// the binary (no node_modules on the host).
//
//   node astro-bun-recipe.mjs <build_dir>
//
// Steps:
//   1. Locate the server entry: dist/server/entry.mjs (root layout) or
//      app/dist/server/entry.mjs (workspace layout).
//   2. Patch node_modules/@astrojs/node/dist/serve-static.js so the client
//      asset dir is env-configurable (inside a compiled binary import.meta.url
//      points at read-only bunfs, so the adapter can't resolve dist/client).
//   3. Emit build-eco/eco-entry.js: point the server at the shipped client
//      dir (dirname(process.execPath)/client on the CT) and start it.
//
// The eco client copies dist/client (or app/dist/client) into the artifact
// dir next to the compiled binary.
import { mkdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const buildDir = process.argv[2];
if (!buildDir) {
  console.error('usage: node astro-bun-recipe.mjs <build_dir>');
  process.exit(1);
}
const recipeDir = join(buildDir, 'build-eco');
mkdirSync(recipeDir, { recursive: true });

// Locate the server entry + client dir (root or workspace layout).
let serverRel;
let clientDir;
if (existsSync(join(buildDir, 'dist', 'server', 'entry.mjs'))) {
  serverRel = '../dist/server/entry.mjs';
  clientDir = join(buildDir, 'dist', 'client');
} else if (existsSync(join(buildDir, 'app', 'dist', 'server', 'entry.mjs'))) {
  serverRel = '../app/dist/server/entry.mjs';
  clientDir = join(buildDir, 'app', 'dist', 'client');
} else {
  throw new Error(`Astro server entry not found under ${buildDir}`);
}

// 1. Patch serve-static.js: compute the client dir from ASTRO_CLIENT_DIR when
//    set (the wrapper sets it to dirname(execPath)/client on the CT).
const serveStatic = join(buildDir, 'node_modules', '@astrojs', 'node', 'dist', 'serve-static.js');
const src = readFileSync(serveStatic, 'utf8');
const anchor = 'let serverEntryFolderURL = path.dirname(import.meta.url);';
const patched = `let serverEntryFolderURL = process.env.ASTRO_CLIENT_DIR ? new URL('file://' + path.join(process.env.ASTRO_CLIENT_DIR, '..')) : path.dirname(import.meta.url);`;
if (!src.includes(patched)) {
  if (!src.includes(anchor)) {
    throw new Error(`@astrojs/node serve-static pattern not found in ${serveStatic}`);
  }
  writeFileSync(serveStatic, src.replace(anchor, patched));
}

// 2. Emit the wrapper entry.
const entry = `import { dirname, join } from 'node:path';
const assetParent = process.env.ECO_ASSET_DIR || dirname(process.execPath);
process.env.ASTRO_CLIENT_DIR = join(assetParent, 'client');
await import('${serverRel}');
`;
writeFileSync(join(recipeDir, 'eco-entry.js'), entry);

console.log('astro bun recipe ready: client assets ship next to the binary');
