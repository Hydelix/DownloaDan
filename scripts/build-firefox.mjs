// Builds the Firefox version of the extension into dist/firefox.
// It is the same code as the Chromium extension in extension/; only the manifest differs
// (see firefox/manifest.json). Run with:  npm run firefox:build
import { cpSync, copyFileSync, mkdirSync, rmSync, readFileSync } from 'node:fs';

const out = 'dist/firefox';
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
cpSync('extension', out, { recursive: true, filter: (p) => !/[\\/]manifest\.json$/.test(p) });
copyFileSync('firefox/manifest.json', `${out}/manifest.json`);

// the two manifests must describe the same extension version
const a = JSON.parse(readFileSync('extension/manifest.json', 'utf8')).version;
const b = JSON.parse(readFileSync('firefox/manifest.json', 'utf8')).version;
if (a !== b) {
  console.error(`Version mismatch: extension/manifest.json is ${a} but firefox/manifest.json is ${b}. Make them the same.`);
  process.exit(1);
}
console.log(`Firefox extension ${b} written to ${out}`);
