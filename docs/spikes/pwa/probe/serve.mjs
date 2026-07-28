// Serves index.html over HTTPS so the probe gets a secure context on a phone.
// Web Bluetooth, Gamepad and Wake Lock all refuse to run over plain http://<lan-ip>.
//
//   node docs/spikes/pwa/probe/serve.mjs
//
// Generates a self-signed cert on first run (requires openssl on PATH — Git Bash ships one).
// The phone will warn about the certificate; accept it, then reload.

import { createServer as createHttps } from 'node:https';
import { createServer as createHttp } from 'node:http';
import { readFileSync, existsSync, writeFileSync, unlinkSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { networkInterfaces } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT || 8443);
// PLAIN=1 serves over http instead — for putting a tunnel (ngrok, cloudflared)
// in front, where the tunnel terminates TLS and a self-signed upstream just
// gets in the way.
const PLAIN = process.env.PLAIN === '1';

// Windows machines usually have several IPv4 addresses — WSL and Hyper-V create
// virtual adapters that a phone cannot reach. Collect them all, rank the ones
// most likely to be the real LAN first, and put every one in the certificate.
function lanAddresses() {
  const found = [];
  for (const [name, addrs] of Object.entries(networkInterfaces())) {
    for (const a of addrs ?? []) {
      if (a.family === 'IPv4' && !a.internal) found.push({ name, ip: a.address });
    }
  }
  const rank = ({ ip }) => (ip.startsWith('192.168.') ? 0 : ip.startsWith('10.') ? 1 : 2);
  return found.sort((a, b) => rank(a) - rank(b));
}

const candidates = lanAddresses();
const ip = candidates.length ? candidates[0].ip : '127.0.0.1';
const keyPath = join(here, 'key.pem');
const certPath = join(here, 'cert.pem');

if (!PLAIN && (!existsSync(keyPath) || !existsSync(certPath))) {
  console.log(`generating self-signed certificate for ${ip} …`);
  const cnfPath = join(here, 'openssl.tmp.cnf');
  writeFileSync(cnfPath, [
    '[req]', 'distinguished_name=dn', 'x509_extensions=v3', 'prompt=no',
    '[dn]', `CN=${ip}`,
    '[v3]', 'basicConstraints=CA:FALSE', 'keyUsage=digitalSignature,keyEncipherment',
    'extendedKeyUsage=serverAuth',
    'subjectAltName=' + ['IP:127.0.0.1', 'DNS:localhost']
      .concat(candidates.map((c) => `IP:${c.ip}`)).join(',')
  ].join('\n'));
  try {
    execFileSync('openssl', [
      'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '365',
      '-keyout', keyPath, '-out', certPath, '-config', cnfPath
    ], { stdio: 'inherit' });
  } catch (e) {
    console.error('\nopenssl failed or is not on PATH.');
    console.error('Install it, or open the published artifact URL on the phone instead — that is already real HTTPS.');
    process.exit(1);
  } finally {
    if (existsSync(cnfPath)) unlinkSync(cnfPath);
  }
}

// The page is one self-contained file — the QR library is inlined at serve time
// rather than fetched, so the same HTML works offline and as a published artifact.
function buildPage() {
  const html = readFileSync(join(here, 'index.html'), 'utf8');
  const lib = readFileSync(join(here, 'vendor', 'qrcode.js'), 'utf8');
  // replacer function, not a string — the library contains `case '$'` and a
  // string replacement would expand `$'` as a substitution pattern
  return html.replace('<!--QRLIB-->', () => '<script>' + lib + '</script>\n');
}

function handler(req, res) {
  if (req.url === '/favicon.ico') { res.writeHead(204).end(); return; }
  res.writeHead(200, {
    'Content-Type': 'text/html; charset=utf-8',
    'Cache-Control': 'no-store'
  });
  // re-read each request so edits show up on refresh
  res.end(buildPage());
}

const server = PLAIN
  ? createHttp(handler)
  : createHttps({ key: readFileSync(keyPath), cert: readFileSync(certPath) }, handler);

server.listen(PORT, '0.0.0.0', () => {
  if (PLAIN) {
    console.log(`\n  probe running on http://localhost:${PORT} (plain — put a tunnel in front)\n`);
    return;
  }
  console.log(`\n  probe running\n`);
  console.log(`  this machine : https://localhost:${PORT}\n`);
  console.log(`  from the phone, try these in order:`);
  for (const c of candidates) {
    console.log(`    https://${c.ip}:${PORT}   (${c.name})`);
  }
  console.log(`\n  Adapters named vEthernet / WSL are virtual and unreachable from a phone —`);
  console.log(`  use the one matching your Wi-Fi network.\n`);
  console.log(`  The phone will warn about the self-signed certificate — accept it and reload.`);
  console.log(`  On iOS, open that URL in Bluefy (Safari has no Web Bluetooth).\n`);
});
