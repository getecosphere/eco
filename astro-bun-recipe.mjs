// astro-bun-recipe.mjs — turns an Astro @astrojs/node build into a
// bun-compilable server whose client assets ship as static files alongside
// the binary (no node_modules on the host).
//
//   node astro-bun-recipe.mjs <build_dir>
//
// Steps:
//   1. Locate the server entry: dist/server/entry.mjs (root layout) or
//      app/dist/server/entry.mjs (workspace layout).
//   2. Patch the bundled @astrojs/node adapter chunk (dist/server/chunks/
//      _@astrojs-ssr-adapter*.mjs) so the client asset dir is env-configurable:
//      `resolveClientDir` must prefer ASTRO_CLIENT_DIR over the build-time
//      file:// URL (inside a compiled binary import.meta.url points at read-only
//      bunfs and the build-time absolute path doesn't exist on the CT).
//   3. Patch dist/server/entry.mjs so `_args.client`/`_args.server` resolve to
//      dirname(process.execPath)/client + server on the CT (baked build paths
//      like /Users/.../dist/client never exist on the production host).
//   4. Emit build-eco/eco-entry.js: point the server at the shipped client
//      dir (dirname(process.execPath)/client on the CT) and start it.
//
// The eco client copies dist/client (or app/dist/client) into the artifact
// dir next to the compiled binary.
import { mkdirSync, readFileSync, writeFileSync, existsSync, readdirSync } from 'node:fs';
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
let serverDir;
if (existsSync(join(buildDir, 'dist', 'server', 'entry.mjs'))) {
  serverRel = '../dist/server/entry.mjs';
  clientDir = join(buildDir, 'dist', 'client');
  serverDir = join(buildDir, 'dist', 'server');
} else if (existsSync(join(buildDir, 'app', 'dist', 'server', 'entry.mjs'))) {
  serverRel = '../app/dist/server/entry.mjs';
  clientDir = join(buildDir, 'app', 'dist', 'client');
  serverDir = join(buildDir, 'app', 'dist', 'server');
} else {
  throw new Error(`Astro server entry not found under ${buildDir}`);
}

// 1. Patch the bundled adapter chunk (the client-dir resolver used at runtime).
//    The build-time `client`/`server` are file:// URLs pointing at the builder's
//    absolute paths; under a compiled binary these must come from env instead.
let patched = false;
for (const chunk of readdirSync(join(serverDir, 'chunks'))) {
  if (!chunk.startsWith('_@astrojs-ssr-adapter')) continue;
  const chunkPath = join(serverDir, 'chunks', chunk);
  const chunkSrc = readFileSync(chunkPath, 'utf8');
  // Guard: only patch once; skip if already patched.
  if (chunkSrc.includes('ASTRO_CLIENT_DIR')) {
    patched = true;
    continue;
  }
  let next = chunkSrc;
  // Make resolveClientDir prefer the env-provided client dir over the baked URL.
  const fnAnchor = 'function resolveClientDir(options) {';
  if (next.includes(fnAnchor)) {
    const override = `function resolveClientDir(options) {
    if (process.env.ASTRO_CLIENT_DIR) {
      return process.env.ASTRO_CLIENT_DIR;
    }
`;
    next = next.replace(fnAnchor, override);
  }
  writeFileSync(chunkPath, next);
  patched = true;
  console.log(`patched adapter chunk: ${chunk}`);
}
if (!patched) {
  throw new Error(`@astrojs/node adapter chunk not found under ${serverDir}/chunks`);
}

// 2. Patch entry.mjs so the runtime client/server paths come from the artifact
//    dir (dirname(process.execPath)) rather than the baked builder paths.
const entryPath = join(serverDir, 'entry.mjs');
const entrySrc = readFileSync(entryPath, 'utf8');
if (!entrySrc.includes('ECO_ASSET_DIR')) {
  const clientRewrite = `
const _assetRoot = process.env.ECO_ASSET_DIR || pathDirname(process.execPath);
`;
  // Insert a helper + rewrite _args after the _args block.
  const rewritten = entrySrc
    .replace(
      /^import \{ renderers \} from '\.\/renderers\.mjs';/,
      `import { dirname as pathDirname, join as pathJoin } from 'node:path';
import { renderers } from './renderers.mjs';`
    )
    .replace(
      /"client":\s*"[^"]*",/,
      `"client": pathJoin(_assetRoot, 'client') + '/',`
    )
    .replace(
      /"server":\s*"[^"]*",/,
      `"server": pathJoin(_assetRoot, 'server') + '/',`
    )
    .replace(
      "const _args = {",
      `${clientRewrite}\nconst _args = {`
    );
  if (rewritten === entrySrc) {
    throw new Error(`could not rewrite _args in ${entryPath}`);
  }
  writeFileSync(entryPath, rewritten);
  console.log('patched server entry: _args.client/server -> ECO_ASSET_DIR');
}

// 3. Emit the wrapper entry.
const entry = `import { dirname, join } from 'node:path';
const assetParent = process.env.ECO_ASSET_DIR || dirname(process.execPath);
process.env.ASTRO_CLIENT_DIR = join(assetParent, 'client');
await import('${serverRel}');
`;
writeFileSync(join(recipeDir, 'eco-entry.js'), entry);

console.log('astro bun recipe ready: client assets ship next to the binary');
