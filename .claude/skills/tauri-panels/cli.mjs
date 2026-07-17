#!/usr/bin/env node
// panels — thin sugar over `agent-browser` for CoyoteSocket's Tauri WebView
// windows (main app + splashscreen). See SKILL.md for usage.

import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import process from "node:process";

// 9224: distinct from ai-notifications' CDP (9223) and personal Edge (9222).
// Kept in sync with COYOTE_REMOTE_DEBUG_PORT in the dev-server coyote-bin plugin.
const CDP_PORT = process.env.COYOTE_REMOTE_DEBUG_PORT || "9224";
const SKILL_DIR = resolve(new URL(".", import.meta.url).pathname.replace(/^\//, ""));
const REPO_ROOT = resolve(SKILL_DIR, "..", "..", "..");
const DEFAULT_SHOT_DIR = join(REPO_ROOT, ".claude", "tmp", "panels");
const PANELS_DIR = join(SKILL_DIR, "panels");

// ── plugin loader ──────────────────────────────────────────────────────────
// Each .mjs in panels/ exports { name, match }. Loaded fresh every invocation
// (CLI is short-lived, no hot-reload needed). Drop a file → it's discovered.
const PANELS = {};
for (const f of readdirSync(PANELS_DIR).filter((f) => f.endsWith(".mjs")).sort()) {
  const mod = await import(pathToFileURL(join(PANELS_DIR, f)).href);
  const panel = mod.default;
  if (!panel?.name || typeof panel.match !== "function") {
    throw new Error(`Panel plugin ${f} must export { name, match }`);
  }
  PANELS[panel.name] = panel;
}
const PANEL_NAMES = Object.keys(PANELS);

// ── agent-browser shell helpers ─────────────────────────────────────────────

// Resolve agent-browser to a real executable, skipping .cmd shims entirely.
// The npm install drops a Windows .cmd shim at PATH that wraps a real
// platform-specific .exe inside node_modules/agent-browser/bin. We find that
// .exe and invoke it directly — no cmd.exe, no shell quoting, no window flash.
const IS_WIN = process.platform === "win32";
function resolveAbBin() {
  const finder = IS_WIN ? "where" : "which";
  const res = spawnSync(finder, ["agent-browser"], { encoding: "utf8", shell: false, windowsHide: true });
  if (res.status !== 0) throw new Error("agent-browser not found in PATH");
  const paths = res.stdout.trim().split(/\r?\n/).filter(Boolean);
  if (IS_WIN) {
    // Read the .cmd shim to extract the real .exe path it points to.
    const cmd = paths.find((p) => p.toLowerCase().endsWith(".cmd"));
    if (cmd) {
      try {
        const content = readFileSync(cmd, "utf8");
        const m = content.match(/"([^"]+\.exe)"/);
        if (m) return m[1].replace(/%~dp0/g, cmd.replace(/[^\\]+$/, ""));
      } catch {}
    }
    // Fallback: first .exe in the results
    return paths.find((p) => p.toLowerCase().endsWith(".exe")) || paths[0];
  }
  return paths[0];
}
const AB_BIN = resolveAbBin();

function ab(args, { capture = true, check = true, timeout = 15000, swallowTimeout = false } = {}) {
  // Direct spawn of the native exe — no shell, no window flash, no quoting pain.
  const res = spawnSync(AB_BIN, args, {
    encoding: "utf8",
    shell: false,
    windowsHide: true,
    timeout,
    stdio: capture ? ["ignore", "pipe", "pipe"] : "inherit",
  });
  if (res.error?.code === "ETIMEDOUT" && !swallowTimeout) {
    throw new Error(`agent-browser ${args.join(" ")} → timed out after ${timeout}ms`);
  }
  if (check && res.status !== 0) {
    const stderr = res.stderr?.trim() || "";
    const stdout = res.stdout?.trim() || "";
    throw new Error(`agent-browser ${args.join(" ")} → exit ${res.status}\n${stderr || stdout}`);
  }
  return res;
}

function abEval(js) {
  const res = ab(["--cdp", CDP_PORT, "eval", js]);
  return res.stdout?.trim() ?? "";
}

function abJson(js) {
  // Wrap eval in JSON.stringify so output is a single line we can parse.
  const wrapped = `JSON.stringify((() => { ${js} })())`;
  const raw = abEval(wrapped);
  // agent-browser prints the result as-is. Strip outer quotes if double-stringified.
  try {
    const parsed = JSON.parse(raw);
    return typeof parsed === "string" ? JSON.parse(parsed) : parsed;
  } catch {
    return raw;
  }
}

// ── tab discovery ───────────────────────────────────────────────────────────

function listTabs() {
  const res = ab(["--cdp", CDP_PORT, "tab"]);
  const lines = res.stdout.split(/\r?\n/).filter(Boolean);
  // Format: "→ [0] Title - url" or "  [1] Title - url"
  return lines.map((line) => {
    const m = line.match(/\[(\d+)\]\s+(.+?)\s+-\s+(.+)$/);
    if (!m) return null;
    return { index: Number(m[1]), title: m[2], url: m[3] };
  }).filter(Boolean);
}

function identifyTab(index) {
  ab(["--cdp", CDP_PORT, "tab", String(index)]);
  const info = abJson(`return { w: innerWidth, h: innerHeight, url: location.pathname };`);
  return { index, ...info };
}

let _warmed = false;
// Guarantee agent-browser's daemon is warm + bound to our port before the
// first real call. Cheap (~0.5s) when already warm, ~8s once when cold. See
// primeSession for the underlying quirk.
function ensureWarm() {
  if (_warmed) return;
  primeSession();
  _warmed = true;
}

function identifyAllTabs() {
  ensureWarm();
  const tabs = listTabs();
  const identified = [];
  for (const t of tabs) {
    try {
      const info = identifyTab(t.index);
      const name = PANEL_NAMES.find((n) => PANELS[n].match(info)) || "unknown";
      identified.push({ ...t, ...info, name });
    } catch {
      // Tab unresponsive (e.g. a closed splashscreen, devtools) — skip it.
      identified.push({ ...t, w: 0, h: 0, url: "", name: "unresponsive" });
    }
  }
  return identified;
}

function findPanel(name) {
  if (!PANEL_NAMES.includes(name)) {
    throw new Error(`Unknown panel "${name}". Valid: ${PANEL_NAMES.join(", ")}`);
  }
  const tabs = identifyAllTabs();
  const match = tabs.find((t) => t.name === name);
  if (!match) {
    const available = tabs.map((t) => `${t.name}(${t.index})`).join(", ");
    throw new Error(`Panel "${name}" not found. Available: ${available || "none"}`);
  }
  // Switch to it and leave it active.
  ab(["--cdp", CDP_PORT, "tab", String(match.index)]);
  return match;
}

// ── selector helpers ────────────────────────────────────────────────────────

// Svelte scopes class names into something like `.foo.s-aBcDeF`. A plain `.foo`
// selector will miss them, so we rewrite a single-class selector to a
// class-contains attribute selector. Anything else passes through.
function normalizeSelector(sel) {
  if (!sel) return sel;
  const single = sel.match(/^\.([a-zA-Z_][\w-]*)$/);
  if (single) return `[class*=${single[1]}]`;
  return sel;
}

function tagAndScroll(selector, tag) {
  const js = `
    const el = document.querySelector(${JSON.stringify(selector)});
    if (!el) return { ok: false };
    el.setAttribute('data-panels-target', ${JSON.stringify(tag)});
    el.scrollIntoView({ block: 'center' });
    return { ok: true };
  `;
  const result = abJson(js);
  return result?.ok === true;
}

function defaultShotPath(name) {
  mkdirSync(DEFAULT_SHOT_DIR, { recursive: true });
  const ts = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  return join(DEFAULT_SHOT_DIR, `${name}-${ts}.png`);
}

// ── subcommands ─────────────────────────────────────────────────────────────

// The first agent-browser call after `close --all` cold-starts its background
// daemon and then hangs the foreground process even though the work already
// succeeded (agent-browser 0.25.x quirk on Windows/WebView2). Fire one bounded
// call to warm the daemon and bind the session to our CDP port, swallowing the
// timeout — every later call then returns in ~0.5s. Idempotent.
function primeSession() {
  ab(["--cdp", CDP_PORT, "tab", "list"], { check: false, timeout: 8000, swallowTimeout: true });
}

function cmdConnect() {
  // Reset any cached session that might be pointed at another port (personal
  // Edge on 9222, ai-notifications on 9223). identifyAllTabs → ensureWarm
  // re-primes a fresh session bound to our port below.
  ab(["close", "--all"], { check: false });
  // Preflight: is the CDP endpoint live?
  const probe = spawnSync("curl", ["-s", "--max-time", "3", `http://localhost:${CDP_PORT}/json/version`], { encoding: "utf8" });
  if (probe.status !== 0 || !probe.stdout.includes("Browser")) {
    console.error(`CDP endpoint not responding on :${CDP_PORT}.`);
    console.error(`Ensure coyote-bin is running with remote debugging:`);
    console.error(`  node .claude/skills/dev-server/cli.mjs status`);
    console.error(`If it's stopped: node .claude/skills/dev-server/cli.mjs start coyote-bin`);
    console.error(`For the plumbing, see .claude/skills/tauri-panels/references/wiring.md`);
    process.exit(2);
  }
  const tabs = identifyAllTabs();
  console.log(`Connected to CDP :${CDP_PORT}. ${tabs.length} tab(s):`);
  for (const t of tabs) {
    console.log(`  ${t.name.padEnd(14)} idx=${t.index}  ${t.w}x${t.h}  ${t.url}`);
  }
}

function cmdTabs() {
  const tabs = identifyAllTabs();
  for (const t of tabs) {
    console.log(`${t.name.padEnd(14)} idx=${t.index}  ${t.w}x${t.h}  ${t.title}`);
  }
}

function cmdShot(args) {
  const [name, selectorArg, pathArg] = args;
  if (!name) {
    console.error(`Usage: panels shot <${PANEL_NAMES.join("|")}> [selector] [path]`);
    process.exit(1);
  }
  findPanel(name);
  const outPath = pathArg || defaultShotPath(name);

  if (selectorArg) {
    const selector = normalizeSelector(selectorArg);
    const tag = `shot-${Date.now()}`;
    if (!tagAndScroll(selector, tag)) {
      console.error(`Element not found: ${selectorArg} (normalized: ${selector})`);
      process.exit(3);
    }
    ab(["--cdp", CDP_PORT, "screenshot", `[data-panels-target=${tag}]`, outPath]);
  } else {
    ab(["--cdp", CDP_PORT, "screenshot", outPath]);
  }
  console.log(resolve(outPath));
}

function cmdFullShot(args) {
  const [name, pathArg] = args;
  if (!name) {
    console.error(`Usage: panels full <${PANEL_NAMES.join("|")}> [path]`);
    process.exit(1);
  }
  findPanel(name);
  const outPath = pathArg || defaultShotPath(`${name}-full`);
  ab(["--cdp", CDP_PORT, "screenshot", "--full", outPath]);
  console.log(resolve(outPath));
}

function cmdEval(args) {
  const [name, ...jsParts] = args;
  const js = jsParts.join(" ");
  if (!name || !js) {
    console.error('Usage: panels eval <panel> "<js>"');
    process.exit(1);
  }
  findPanel(name);
  const res = ab(["--cdp", CDP_PORT, "eval", js]);
  process.stdout.write(res.stdout);
}

function cmdStyles(args) {
  const [name, selectorArg] = args;
  if (!name || !selectorArg) {
    console.error("Usage: panels styles <panel> <selector>");
    process.exit(1);
  }
  findPanel(name);
  const selector = normalizeSelector(selectorArg);
  const tag = `styles-${Date.now()}`;
  if (!tagAndScroll(selector, tag)) {
    console.error(`Element not found: ${selectorArg} (normalized: ${selector})`);
    process.exit(3);
  }
  const target = `[data-panels-target=${tag}]`;
  console.log("── box ──");
  process.stdout.write(ab(["--cdp", CDP_PORT, "get", "box", target]).stdout);
  console.log("── styles ──");
  process.stdout.write(ab(["--cdp", CDP_PORT, "get", "styles", target]).stdout);
}

// Install error/rejection taps so all uncaught exceptions route through
// console.error, where `agent-browser console` will pick them up.
const TAP_INSTALL_JS = `
  if (!window.__panels_taps_installed__) {
    window.__panels_taps_installed__ = true;
    window.addEventListener('error', (e) => {
      const stack = e.error && e.error.stack ? e.error.stack : e.message;
      console.error('[UNCAUGHT]', stack, 'at', e.filename + ':' + e.lineno + ':' + e.colno);
    });
    window.addEventListener('unhandledrejection', (e) => {
      const reason = e.reason;
      const stack = reason && reason.stack ? reason.stack : String(reason);
      console.error('[REJECT]', stack);
    });
    'installed';
  } else {
    'already-installed';
  }
`;

function cmdLogs(args) {
  const flags = new Set();
  const positional = [];
  for (const a of args) {
    if (a.startsWith("--")) flags.add(a);
    else positional.push(a);
  }
  const [name] = positional;
  if (!name) {
    console.error("Usage: panels logs <panel> [--errors] [--clear] [--no-taps]");
    process.exit(1);
  }
  findPanel(name);

  if (!flags.has("--no-taps")) {
    // Install once per session; safe to re-run.
    abEval(TAP_INSTALL_JS);
  }

  if (flags.has("--clear")) {
    ab(["--cdp", CDP_PORT, "console", "--clear"]);
    console.log("cleared");
    return;
  }

  const res = ab(["--cdp", CDP_PORT, "console"]);
  const lines = res.stdout.split(/\r?\n/);
  const filtered = flags.has("--errors")
    ? lines.filter((l) => /^\[(error|warning)\]/i.test(l) || l.includes("[UNCAUGHT]") || l.includes("[REJECT]"))
    : lines;
  process.stdout.write(filtered.join("\n"));
  if (!filtered.at(-1)?.endsWith("\n")) process.stdout.write("\n");
}

function cmdReload(args) {
  // Support: panels reload [panel] [--wait-for <selector>] [--timeout <ms>]
  let name;
  let waitFor;
  let timeoutMs = 5000;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--wait-for") waitFor = args[++i];
    else if (a === "--timeout") timeoutMs = Number(args[++i]);
    else if (!name && !a.startsWith("--")) name = a;
  }
  if (name) findPanel(name);
  ab(["--cdp", CDP_PORT, "reload"]);

  if (waitFor) {
    const selector = normalizeSelector(waitFor);
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const found = abJson(`return !!document.querySelector(${JSON.stringify(selector)});`);
      if (found === true) {
        console.log(`reloaded, ${selector} mounted`);
        return;
      }
      spawnSync(process.platform === "win32" ? "cmd.exe" : "sh",
        process.platform === "win32" ? ["/c", "timeout /t 1 /nobreak >nul"] : ["-c", "sleep 0.1"],
        { windowsHide: true });
    }
    console.error(`reloaded but timeout waiting for ${selector} (${timeoutMs}ms)`);
    process.exit(4);
  }
  console.log("reloaded");
  return;
}

function cmdClick(args) {
  // Usage: panels click <panel> <child-sel> --in <container-sel> --where <text>
  // Finds a container matching the substring in its textContent, then clicks
  // the child element inside it. Handles reordering lists.
  let name, childSel, containerSel, whereText;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--in") containerSel = args[++i];
    else if (a === "--where") whereText = args[++i];
    else if (!name) name = a;
    else if (!childSel) childSel = a;
  }
  if (!name || !childSel) {
    console.error('Usage: panels click <panel> <child-sel> --in <container-sel> --where "<text>"');
    process.exit(1);
  }
  findPanel(name);

  const childNorm = normalizeSelector(childSel);
  const containerNorm = containerSel ? normalizeSelector(containerSel) : null;
  const js = `
    const childSel = ${JSON.stringify(childNorm)};
    const containerSel = ${JSON.stringify(containerNorm)};
    const needle = ${JSON.stringify((whereText || "").toLowerCase())};

    let targetChild = null;
    if (containerSel) {
      const containers = document.querySelectorAll(containerSel);
      for (const c of containers) {
        if (!needle || (c.textContent || '').toLowerCase().includes(needle)) {
          targetChild = c.querySelector(childSel);
          if (targetChild) break;
        }
      }
    } else {
      targetChild = document.querySelector(childSel);
    }
    if (!targetChild) return { ok: false };

    targetChild.scrollIntoView({ block: 'center' });
    // Svelte 5 uses delegated listeners — .click() still fires them.
    targetChild.click();
    return { ok: true, text: (targetChild.textContent || '').trim().slice(0, 60) };
  `;
  const result = abJson(js);
  if (!result?.ok) {
    console.error(`No element found: ${childSel}${containerSel ? ` in ${containerSel}` : ""}${whereText ? ` where text~=${whereText}` : ""}`);
    process.exit(3);
  }
  console.log(`clicked: ${result.text}`);
}

function cmdHelp() {
  console.log(`panels — UI inspection helper for CoyoteSocket's Tauri WebView windows

Usage:
  panels connect                                    Reset session, verify CDP on :${CDP_PORT}, list windows
  panels tabs                                       Show currently-open windows with indices
  panels shot <panel> [sel] [path]                  Screenshot a window (or element in it)
  panels full <panel> [path]                        Full-page screenshot of a window
  panels logs <panel> [--errors] [--clear]          Console buffer + uncaught exceptions
  panels click <panel> <child-sel> [--in <c>] [--where <text>]
                                                    Click a nested element in a container matched by text
  panels styles <panel> <selector>                  Computed styles + box for an element
  panels eval <panel> "<js>"                        Run JS in a window
  panels reload [panel] [--wait-for <sel>] [--timeout <ms>]
                                                    Hard reload, optionally wait for mount
  panels help                                       This message

Panels:  ${PANEL_NAMES.join(" | ")}

Selectors: plain ".foo" is auto-rewritten to [class*=foo] to survive Svelte's
scoped class hashes. Use [attr=value] or #id for exact matches.

Screenshots default to .claude/tmp/panels/<panel>-<timestamp>.png (gitignored).

Examples:
  panels connect
  panels shot main
  panels shot main ".preset-select" .claude/tmp/preset.png
  panels logs main --errors
  panels reload main --wait-for "main"
  panels click main "button" --in "[role=dialog]" --where "Reorder"
  panels styles main ".preset-select"
  panels eval main "document.querySelectorAll('[role=dialog]').length"
`);
}

// ── dispatch ────────────────────────────────────────────────────────────────

const [cmd, ...rest] = process.argv.slice(2);
try {
  switch (cmd) {
    case "connect": cmdConnect(); break;
    case "tabs":    cmdTabs(); break;
    case "shot":    cmdShot(rest); break;
    case "full":    cmdFullShot(rest); break;
    case "logs":    cmdLogs(rest); break;
    case "click":   cmdClick(rest); break;
    case "eval":    cmdEval(rest); break;
    case "styles":  cmdStyles(rest); break;
    case "reload":  cmdReload(rest); break;
    case "help":
    case "--help":
    case "-h":
    case undefined: cmdHelp(); break;
    default:
      console.error(`Unknown command: ${cmd}`);
      cmdHelp();
      process.exit(1);
  }
} catch (err) {
  console.error(err.message || err);
  process.exit(1);
}
