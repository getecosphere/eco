// sveltekit-bun-recipe.mjs — turns a SvelteKit adapter-node build into a
// bun-compilable server whose client assets ship as static files alongside
// the binary (no node_modules on the host).
//
//   node sveltekit-bun-recipe.mjs <build_dir>
//
// Steps:
//   1. Patch build/handler.js so the client asset dir is env-configurable
//      (adapter-node computes it from the server file location, which inside a
//      compiled binary is the read-only embedded bunfs).
//   2. Emit build-eco/eco-entry.js: point the server at the shipped client
//      dir (dirname(process.execPath)/client on the CT) and start it.
//
// The eco client is responsible for copying build/client into the artifact dir
// next to the compiled binary.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const buildDir = process.argv[2];
if (!buildDir) {
  console.error('usage: node sveltekit-bun-recipe.mjs <build_dir>');
  process.exit(1);
}
const handlerPath = join(buildDir, 'build', 'handler.js');
const recipeDir = join(buildDir, 'build-eco');
mkdirSync(recipeDir, { recursive: true });

// 1. Patch handler.js.
const handler = readFileSync(handlerPath, 'utf8');
const marker = 'const dir = path.dirname(fileURLToPath(import.meta.url));';
const patched = 'const dir = process.env.ECO_ASSET_DIR || path.dirname(fileURLToPath(import.meta.url));';
if (!handler.includes(patched)) {
  if (!handler.includes(marker)) {
    throw new Error(`adapter-node handler pattern not found in ${handlerPath}`);
  }
  writeFileSync(handlerPath, handler.replace(marker, patched));
}

// 2. Emit the wrapper entry. ECO_ASSET_DIR must be the parent of the client
//    dir (adapter-node computes asset_dir = `${dir}/client`).
const entry = `import { dirname } from 'node:path';
const assetParent = process.env.ECO_ASSET_DIR || dirname(process.execPath);
process.env.ECO_ASSET_DIR = assetParent;
await import('../build/index.js');
`;
writeFileSync(join(recipeDir, 'eco-entry.js'), entry);

console.log('sveltekit bun recipe ready: client assets ship next to the binary');
