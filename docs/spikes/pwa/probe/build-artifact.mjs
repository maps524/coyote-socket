// Produces the single-file version for publishing as a Claude artifact:
// inlines the QR library and strips the document scaffolding the artifact host
// supplies itself (<!doctype>, <html>, <head>, <body>).
//
//   node docs/spikes/pwa/probe/build-artifact.mjs <output-path>

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const out = process.argv[2];
if (!out) {
  console.error('usage: node build-artifact.mjs <output-path>');
  process.exit(1);
}

const lib = readFileSync(join(here, 'vendor', 'qrcode.js'), 'utf8');

const html = readFileSync(join(here, 'index.html'), 'utf8')
  // replacer function, not a string — the library contains `case '$'` and a
  // string replacement would expand `$'` as a substitution pattern
  .replace('<!--QRLIB-->', () => '<script>' + lib + '</script>\n')
  .replace(/^<!doctype html>\r?\n/i, '')
  .replace(/<html lang="en">\r?\n/, '')
  .replace(/<head>\r?\n/, '')
  .replace(/<meta charset[^>]*>\r?\n/, '')
  .replace(/<meta name="viewport"[^>]*>\r?\n/, '')
  .replace(/<\/head>\r?\n/, '')
  .replace(/<body>\r?\n/, '')
  .replace(/<\/body>\r?\n/, '')
  .replace(/<\/html>\r?\n?/, '');

// Fail loudly rather than publishing a page with a broken script tag.
for (const m of html.matchAll(/<script>([\s\S]*?)<\/script>/g)) new Function(m[1]);

writeFileSync(out, html);
console.log('wrote ' + out + ' (' + html.length + ' bytes)');
