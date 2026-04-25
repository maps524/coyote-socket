#!/usr/bin/env node
// Dev-server CLI — agent-facing interface for service management.
// Auto-starts the daemon if not running. Default port 9860 (override with COYOTE_DEV_PORT).

import http from "node:http";
import { spawn } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// ── paths ──────────────────────────────────────────────────────────────────────
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SKILL_DIR = __dirname;
const DAEMON_SCRIPT = path.join(SKILL_DIR, "scripts", "daemon.mjs");
const PID_FILE = path.join(SKILL_DIR, "state", "daemon.pid");
const PROJECT_ROOT = path.resolve(SKILL_DIR, "..", "..", "..");

const DAEMON_PORT = parseInt(process.env.COYOTE_DEV_PORT || "9860", 10);
const DAEMON_HOST = "127.0.0.1";

// ── service aliases ────────────────────────────────────────────────────────────
const ALIASES = {
  tauri: "tauri-dev",
  bin: "coyote-bin",
  coyote: "coyote-bin",
};

function resolveService(name) {
  if (!name) return null;
  return ALIASES[name] || name;
}

// ── helpers ────────────────────────────────────────────────────────────────────
function out(data) {
  process.stdout.write(JSON.stringify(data, null, 2) + "\n");
}

function err(message, details) {
  out({ error: message, ...details });
  process.exit(1);
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

function request(method, urlPath, timeout = 10000) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { hostname: DAEMON_HOST, port: DAEMON_PORT, path: urlPath, method, timeout },
      (res) => {
        let body = "";
        res.on("data", (c) => (body += c));
        res.on("end", () => {
          try {
            resolve({ status: res.statusCode, data: JSON.parse(body) });
          } catch {
            reject(new Error(`Bad JSON from daemon: ${body.slice(0, 200)}`));
          }
        });
      }
    );
    req.on("error", reject);
    req.on("timeout", () => {
      req.destroy();
      reject(new Error("Request timed out"));
    });
    req.end();
  });
}

async function req(method, urlPath, timeout) {
  const { data } = await request(method, urlPath, timeout);
  return data;
}

async function isDaemonRunning() {
  try {
    const data = await req("GET", "/", 2000);
    return !!data.ok;
  } catch {
    return false;
  }
}

async function ensureDaemon() {
  if (await isDaemonRunning()) return true;

  if (existsSync(PID_FILE)) {
    try {
      const info = JSON.parse(readFileSync(PID_FILE, "utf-8"));
      process.kill(info.pid, 0);
    } catch {
      // Stale PID file
    }
  }

  const child = spawn(process.execPath, [DAEMON_SCRIPT], {
    detached: true,
    stdio: "ignore",
    windowsHide: true,
    cwd: PROJECT_ROOT,
    env: { ...process.env },
  });
  child.unref();

  for (let i = 0; i < 25; i++) {
    await sleep(200);
    if (await isDaemonRunning()) return true;
  }

  return false;
}

// ── log formatting ─────────────────────────────────────────────────────────────
const SVC_LABELS = {
  vite: "vite",
  "coyote-bin": "bin",
  "tauri-dev": "tdev",
  daemon: "daemon",
};

const ERROR_PATTERNS = /\berror\b|\bpanic\b|\bfailed\b|\bFailed\b|\bERROR\b|\bfatal\b/;

function fmtLocalTimeCli(isoStr) {
  if (!isoStr) return "";
  const d = new Date(isoStr);
  return d.toLocaleTimeString("en-US", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function formatLogLine(entry, showService) {
  const ts = fmtLocalTimeCli(entry.timestamp);
  const svcLabel = SVC_LABELS[entry.service] || entry.service?.replace("build:", "build:") || "???";
  const prefix = showService ? `[${svcLabel.padEnd(6)}]` : "";
  let lvl;
  if (entry.level === "daemon") {
    lvl = "\x1b[33mDAE\x1b[0m";
  } else if (entry.level === "stderr" && ERROR_PATTERNS.test(entry.message)) {
    lvl = "\x1b[31mERR\x1b[0m";
  } else {
    lvl = "   ";
  }
  return `${ts} ${lvl} ${prefix} ${entry.message}`;
}

// ── commands ───────────────────────────────────────────────────────────────────
async function cmdStatus() {
  const data = await req("GET", "/status");
  out(data);
}

async function cmdStart(name) {
  const svc = resolveService(name) || "coyote-bin";
  const data = await req("POST", `/services/${svc}/start`);
  out(data);
}

async function cmdStop(name) {
  if (!name) {
    const data = await req("POST", "/stop-all");
    out(data);
  } else {
    const svc = resolveService(name);
    const data = await req("POST", `/services/${svc}/stop`);
    out(data);
  }
}

async function cmdRestart(name) {
  const svc = resolveService(name) || "coyote-bin";
  const data = await req("POST", `/services/${svc}/restart`);
  out(data);
}

async function cmdLogs(args) {
  let serviceName = null;
  const flags = [];
  for (const a of args) {
    if (a.startsWith("--")) flags.push(a);
    else if (!serviceName) serviceName = a;
    else flags.push(a);
  }

  const params = new URLSearchParams();
  if (flags.includes("--all")) {
    // No since/limit
  } else if (flags.some((a) => a.startsWith("--since"))) {
    const idx = flags.findIndex((a) => a.startsWith("--since"));
    const val = flags[idx].includes("=") ? flags[idx].split("=")[1] : flags[idx + 1];
    if (val !== undefined) params.set("since", val);
  } else {
    params.set("limit", "50");
  }
  if (flags.some((a) => a.startsWith("--limit"))) {
    const idx = flags.findIndex((a) => a.startsWith("--limit"));
    const val = flags[idx].includes("=") ? flags[idx].split("=")[1] : flags[idx + 1];
    if (val !== undefined) params.set("limit", val);
  }
  if (flags.some((a) => a.startsWith("--level"))) {
    const idx = flags.findIndex((a) => a.startsWith("--level"));
    const val = flags[idx].includes("=") ? flags[idx].split("=")[1] : flags[idx + 1];
    if (val !== undefined) params.set("level", val);
  }

  const qs = params.toString();
  const svc = resolveService(serviceName);

  if (svc) {
    const data = await req("GET", `/services/${svc}/logs${qs ? "?" + qs : ""}`);
    out(data);
  } else {
    const data = await req("GET", `/logs${qs ? "?" + qs : ""}`);
    out(data);
  }
}

async function cmdTail(name) {
  const svc = resolveService(name);
  let cursor = -1;

  const status = await req("GET", "/status");
  if (svc) {
    const svcInfo = status.services?.[svc];
    if (svcInfo?.logCount > 20) cursor = svcInfo.logCount - 20;
  } else {
    let maxLog = 0;
    for (const s of Object.values(status.services || {})) {
      maxLog = Math.max(maxLog, s.logCount || 0);
    }
    if (maxLog > 20) cursor = maxLog - 20;
  }

  const label = svc || "all services";
  process.stdout.write(`[tail] Watching ${label} (Ctrl+C to stop)...\n\n`);
  const showService = !svc;

  const poll = async () => {
    try {
      const endpoint = svc ? `/services/${svc}/logs?since=${cursor}` : `/logs?since=${cursor}`;
      const data = await req("GET", endpoint);
      for (const entry of data.logs) {
        process.stdout.write(formatLogLine(entry, showService) + "\n");
        cursor = entry.index;
      }
    } catch {
      process.stdout.write("\n[tail] Daemon disconnected.\n");
      process.exit(0);
    }
  };

  await poll();
  setInterval(poll, 1000);
}

async function cmdBuild(args) {
  const name = args[0];
  if (!name) err("build_requires_name", { message: "Usage: build <name> (e.g. build coyote)" });

  const buildName = ALIASES[name] || name;
  const { data } = await request("POST", `/build/${buildName}`);

  if (data.action === "restarted") {
    process.stdout.write(`Build restarted (previous build interrupted)\n`);
  } else if (data.action === "building") {
    process.stdout.write(`Building ${buildName}...\n`);
  } else if (data.action === "error") {
    err("build_error", data);
    return;
  }

  let cursor = -1;
  while (true) {
    await sleep(1000);
    const bStatus = await req("GET", "/build/status");
    const logs = await req("GET", `/build/logs?since=${cursor}`);
    for (const entry of logs.logs) {
      const ts = entry.timestamp.split("T")[1]?.slice(0, 8) || "";
      const lvl = (entry.level === "stderr" && ERROR_PATTERNS.test(entry.message)) ? "\x1b[31mERR\x1b[0m" : "   ";
      process.stdout.write(`${ts} ${lvl} ${entry.message}\n`);
      cursor = entry.index;
    }
    if (!bStatus.locked) {
      if (bStatus.exitCode === 0) {
        process.stdout.write(`\n\x1b[32mBuild succeeded.\x1b[0m\n`);
      } else {
        process.stdout.write(`\n\x1b[31mBuild failed (exit ${bStatus.exitCode}).\x1b[0m\n`);
        process.exit(1);
      }
      break;
    }
  }
}

async function cmdClean(args) {
  const name = args[0];
  if (!name) err("clean_requires_name", { message: "Usage: clean <name> (e.g. clean coyote)" });

  const force = args.includes("--force");
  const qs = force ? "?force=1" : "";
  const { data } = await request("POST", `/clean/${name}${qs}`);

  if (data.action === "error") {
    if (data.unsafe) {
      process.stdout.write(`\x1b[33m${data.message}\x1b[0m\n`);
      process.stdout.write(`Use --force to confirm: clean ${name} --force\n`);
    } else {
      err("clean_error", data);
    }
    return;
  }

  process.stdout.write(`Cleaning ${name}${data.wasRunning ? ` (stopped service)` : ""}...\n`);

  let cursor = -1;
  while (true) {
    await sleep(500);
    const bStatus = await req("GET", "/build/status");
    const logs = await req("GET", `/build/logs?since=${cursor}`);
    for (const entry of logs.logs) {
      const ts = entry.timestamp.split("T")[1]?.slice(0, 8) || "";
      process.stdout.write(`${ts}     ${entry.message}\n`);
      cursor = entry.index;
    }
    if (!bStatus.locked) {
      if (bStatus.exitCode === 0) {
        process.stdout.write(`\n\x1b[32mClean succeeded.\x1b[0m\n`);
      } else {
        process.stdout.write(`\n\x1b[31mClean failed (exit ${bStatus.exitCode}).\x1b[0m\n`);
        process.exit(1);
      }
      break;
    }
  }
}

async function cmdDev(args) {
  const rebuild = args.includes("--rebuild") || args.includes("-r");
  const qs = rebuild ? "?rebuild=1" : "";

  const { status: httpStatus, data } = await request("POST", `/dev${qs}`);
  if (httpStatus === 409) {
    process.stdout.write(`Orchestration already in progress (phase: ${data.phase}). Watching...\n`);
  } else {
    process.stdout.write("Starting dev orchestration (Vite HMR + compiled binary with shadow-copy)...\n");
  }

  let lastPhase = "";
  let buildLogCursor = -1;
  const phaseLabels = {
    checking: "Checking environment...",
    "building-coyote-bin": "\nBuilding coyote-socket...",
    "starting-vite": "Waiting for Vite dev server...",
    "starting-tauri": "Starting compiled binary...",
    ready: "\x1b[32mAll services ready.\x1b[0m",
  };
  const isBuildPhase = (p) => p?.startsWith("building-");

  while (true) {
    await sleep(1000);
    const status = await req("GET", "/status");
    const orch = status.orchestration;

    if (orch.phase !== lastPhase) {
      lastPhase = orch.phase;
      if (orch.phase === "error") {
        process.stdout.write(`\x1b[31mOrchestration error: ${orch.error}\x1b[0m\n`);
      } else {
        process.stdout.write((phaseLabels[orch.phase] || `Phase: ${orch.phase}`) + "\n");
      }
    }

    if (isBuildPhase(orch.phase) || (isBuildPhase(lastPhase) && orch.active)) {
      try {
        const logs = await req("GET", `/build/logs?since=${buildLogCursor}`);
        for (const entry of logs.logs) {
          const ts = entry.timestamp.split("T")[1]?.slice(0, 8) || "";
          const lvl = (entry.level === "stderr" && ERROR_PATTERNS.test(entry.message)) ? "\x1b[31mERR\x1b[0m" : "   ";
          process.stdout.write(`${ts} ${lvl} ${entry.message}\n`);
          buildLogCursor = entry.index;
        }
      } catch {}
    }

    if (!orch.active) {
      if (buildLogCursor >= 0) {
        try {
          const logs = await req("GET", `/build/logs?since=${buildLogCursor}`);
          for (const entry of logs.logs) {
            const ts = entry.timestamp.split("T")[1]?.slice(0, 8) || "";
            const lvl = (entry.level === "stderr" && ERROR_PATTERNS.test(entry.message)) ? "\x1b[31mERR\x1b[0m" : "   ";
            process.stdout.write(`${ts} ${lvl} ${entry.message}\n`);
          }
        } catch {}
      }
      if (orch.phase === "ready") {
        process.stdout.write("\nServices:\n");
        for (const [name, svc] of Object.entries(status.services)) {
          const icon = svc.status === "running" ? "\x1b[32m●\x1b[0m" : "\x1b[90m○\x1b[0m";
          const info = svc.status === "running" ? `pid ${svc.pid}, uptime ${svc.uptime}s` : "";
          process.stdout.write(`  ${icon} ${name}: ${svc.status} ${info}\n`);
        }
        process.stdout.write(`\nNext: \x1b[1mnpm run dev:dash\x1b[0m for live TUI, or \x1b[1mnpm run dev:tail\x1b[0m for logs.\n`);
      } else if (orch.phase === "error") {
        process.exit(1);
      }
      break;
    }
  }
}

async function cmdHelp() {
  const help = `Dev Server CLI — Single-binary Tauri dev server with shadow-copy hot-swap

NPM SHORTCUTS:
  npm run dev               Build + run: Vite HMR + compiled coyote-socket binary (shadow-copy)
  npm run dev:dash          Live TUI dashboard
  npm run dev:tauri         Legacy mode: tauri dev (auto-rebuilds Rust on file changes)
  npm run dev:vite          Bare \`vite\` (used internally by Tauri's beforeDevCommand)
  npm run dev:status        Service overview
  npm run dev:tail          Watch all logs (live)
  npm run dev:logs          Last 50 log lines
  npm run dev:stop          Stop all services
  npm run dev:build         Rebuild coyote-socket binary
  npm run dev:kill          Kill daemon + all services

FULL CLI USAGE: node .claude/skills/dev-server/cli.mjs <command> [name] [flags]

COMMANDS:
  status                    Service overview (default)
  start [name]              Start service (default: coyote-bin)
  stop [name]               Stop service (default: all)
  restart [name]            Restart service (default: coyote-bin)
  logs [name] [flags]       Service logs (default: all interleaved, last 50)
  tail [name]               Continuous polling
  build <name>              Build a target. Auto-restarts linked service after success.
  clean <name> [--force]    Clean build artifacts. Stops/restarts linked service.
  dev [--rebuild]           Build coyote-socket + start Vite + start binary with shadow-copy
  dashboard (alias: dash)   Live TUI — status panel, log stream, hotkeys
  help                      Show this help
  shutdown                  Kill all services + daemon

SERVICES:
  vite                      Vite dev server on :1421 (HMR for Svelte/CSS/TS)
  coyote-bin (alias: bin)   Compiled coyote-socket binary, shadow-copy hot-swap, requires vite
  tauri-dev (alias: tauri)  Legacy: \`npx tauri dev\` (auto-rebuilds Rust on file change)

BUILD TARGETS:
  coyote-bin (alias: coyote, bin) cargo build --release (in src-tauri/)

CLEAN TARGETS:
  coyote-bin (alias: coyote, tauri) Clean coyote-socket crate

LOG FLAGS:
  --since N     Lines since log index N (incremental polling)
  --limit N     Last N lines
  --level L     Filter: stdout, stderr, daemon
  --all         All buffered lines (up to 2000)

ENV:
  COYOTE_DEV_PORT  Daemon port (default 9860)
  COYOTE_VITE_URL  Vite URL injected into compiled binary (default http://localhost:1421)

Persistent log files: .claude/skills/dev-server/state/logs/*.log (rotates at 2MB)
`;
  process.stdout.write(help);
}

async function cmdShutdown() {
  try {
    await req("POST", "/shutdown");
    out({ action: "shutdown" });
  } catch {
    out({ action: "shutdown" });
  }
}

// ── dashboard TUI ─────────────────────────────────────────────────────────────
async function cmdDashboard() {
  if (!process.stdout.isTTY) {
    console.error("Dashboard requires a TTY terminal.");
    process.exit(1);
  }

  const write = (s) => process.stdout.write(s);

  const ALT_ON = "\x1b[?1049h";
  const ALT_OFF = "\x1b[?1049l";
  const CUR_HIDE = "\x1b[?25l";
  const CUR_SHOW = "\x1b[?25h";
  const HOME = "\x1b[H";
  const CLR_LINE = "\x1b[2K";
  const CLR_BELOW = "\x1b[J";

  const C = {
    r: "\x1b[0m", b: "\x1b[1m", d: "\x1b[2m",
    dim: "\x1b[90m", red: "\x1b[31m", grn: "\x1b[32m",
    ylw: "\x1b[33m", blu: "\x1b[34m", mag: "\x1b[35m",
    cyn: "\x1b[36m", wht: "\x1b[37m", bgBlu: "\x1b[44m",
  };

  let SVC_ORDER = [];
  let SVC_COLOR = {};
  let SVC_SHORT = { daemon: "daemon" };
  let logFilterBindings = {};
  let buildBindings = {};
  let svcMeta = [];

  function rebuildMetaTables(meta) {
    const order = [];
    const color = {};
    const short = { daemon: "daemon" };
    const filterMap = {};
    const buildMap = {};
    const flat = [];
    for (const svc of meta.services || []) {
      const d = svc.dashboard;
      if (!d) continue;
      order.push(svc.name);
      color[svc.name] = C[d.color] || C.ylw;
      short[svc.name] = d.short || svc.name;
      if (d.logFilterKey) filterMap[d.logFilterKey] = svc.name;
      if (d.buildKey && svc.hasBuild) buildMap[d.buildKey] = svc.name;
      flat.push({ name: svc.name, short: d.short || svc.name, color: d.color, logFilterKey: d.logFilterKey, buildKey: d.buildKey, hasBuild: svc.hasBuild });
    }
    SVC_ORDER = order;
    SVC_COLOR = color;
    SVC_SHORT = short;
    logFilterBindings = filterMap;
    buildBindings = buildMap;
    svcMeta = flat;
  }

  let logFilter = null;
  let logCursor = -1;
  let logLines = [];
  let lastStatus = null;
  let running = true;
  let actionMsg = null;
  let actionTimer = null;
  let daemonStartedAt = null;
  let cleanSubMenu = false;

  function flash(msg) {
    actionMsg = msg;
    if (actionTimer) clearTimeout(actionTimer);
    actionTimer = setTimeout(() => { actionMsg = null; markDirty(); }, 4000);
    markDirty();
  }

  let cols = process.stdout.columns || 100;
  let rows = process.stdout.rows || 30;
  process.stdout.on("resize", () => {
    cols = process.stdout.columns || 100;
    rows = process.stdout.rows || 30;
  });

  write(ALT_ON + CUR_HIDE);
  process.stdin.setRawMode(true);
  process.stdin.resume();
  process.stdin.setEncoding("utf8");

  let exitDash = () => {
    if (!running) return;
    running = false;
    write(CUR_SHOW + ALT_OFF);
    try { process.stdin.setRawMode(false); } catch {}
    process.exit(0);
  };

  process.on("SIGINT", () => exitDash());
  process.on("SIGTERM", () => exitDash());

  process.stdin.on("data", async (key) => {
    if (key.length > 1 && key[0] === "\x1b") {
      if (cleanSubMenu) { cleanSubMenu = false; flash("Clean cancelled"); }
      return;
    }

    if (cleanSubMenu) {
      cleanSubMenu = false;
      const cleanTargets = { "1": "coyote-bin" };
      const target = cleanTargets[key];
      if (!target) { flash("Clean cancelled"); return; }
      flash(`Cleaning ${target}...`);
      try {
        const r = await req("POST", `/clean/${target}`);
        if (r.action === "error") {
          flash(`Clean: ${r.message}`);
        } else {
          flash(`Cleaning ${target}...`);
          logFilter = "build"; logCursor = -1; logLines = [];
        }
      } catch (e) { flash(`Clean error: ${e.message}`); }
      return;
    }

    const k = (key === "K" || key === "S" || key === "R") ? key : key.toLowerCase();

    if (logFilterBindings[k]) {
      const name = logFilterBindings[k];
      logFilter = logFilter === name ? null : name;
      logCursor = -1; logLines = [];
      flash(logFilter ? `Filter: ${name}` : "Filter: all");
      return;
    }
    if (buildBindings[k]) {
      const name = buildBindings[k];
      flash(`Build: ${name}...`);
      try {
        const r = await req("POST", `/build/${name}`);
        flash(r.action === "building" ? `${name} build started` : `Build: ${r.action}`);
        logFilter = "build"; logCursor = -1; logLines = [];
      } catch (e) { flash(`Build error: ${e.message}`); }
      return;
    }

    switch (k) {
      case "q":
      case "\x03":
        exitDash();
        break;

      case "3":
        logFilter = logFilter === "build" ? null : "build";
        logCursor = -1; logLines = [];
        flash(logFilter ? "Filter: build logs" : "Filter: all");
        break;

      case "a":
        logFilter = null; logCursor = -1; logLines = [];
        flash("Filter: all");
        break;

      case "r":
        flash("Reloading plugins...");
        try {
          const result = await req("POST", "/reload-services");
          if (result.error) flash(`Reload failed: ${result.error}`);
          else flash(`Reloaded: ${result.count} plugins (+${result.added.length} -${result.removed.length})`);
        } catch (e) { flash(`Reload error: ${e.message}`); }
        break;

      case "R":
        flash("Restarting services...");
        try {
          const st = await req("GET", "/status");
          for (const name of Object.keys(st.services)) {
            if (st.services[name].status === "running") {
              await req("POST", `/services/${name}/restart`);
            }
          }
          flash("Services restarted");
        } catch (e) { flash(`Restart failed: ${e.message}`); }
        break;

      case "S":
        flash("Stopping all...");
        try {
          await req("POST", "/stop-all");
          flash("All services stopped");
        } catch (e) { flash(`Stop failed: ${e.message}`); }
        break;

      case "c":
        cleanSubMenu = true;
        flash("Clean: [1] coyote-bin  [ESC] cancel");
        break;

      case "d":
        flash("Starting dev orchestration...");
        try {
          await req("POST", "/dev");
          flash("Orchestration started");
        } catch (e) { flash(`Dev error: ${e.message}`); }
        break;

      case "K":
        flash("Shutting down daemon...");
        try { await req("POST", "/shutdown"); } catch {}
        exitDash();
        break;
    }
  });

  function fmtUptime(sec) {
    if (!sec || sec <= 0) return "";
    if (sec < 60) return `${sec}s`;
    if (sec < 3600) return `${Math.floor(sec / 60)}m${sec % 60 ? " " + (sec % 60) + "s" : ""}`;
    const h = Math.floor(sec / 3600);
    const m = Math.floor((sec % 3600) / 60);
    return `${h}h ${m}m`;
  }

  // eslint-disable-next-line no-control-regex
  const ANSI_RE = /\x1b\[[0-9;]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b\\\\/g;
  function stripAnsi(s) { return s.replace(ANSI_RE, ""); }

  // eslint-disable-next-line no-control-regex
  const ANSI_TOKEN_RE = /\x1b\[[0-9;]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b\\\\/;
  function truncVis(s, maxCols) {
    if (maxCols <= 0) return "";
    let out = "", vis = 0, i = 0;
    while (i < s.length && vis < maxCols) {
      if (s[i] === "\x1b") {
        const tail = s.slice(i);
        const m = tail.match(ANSI_TOKEN_RE);
        if (m && m.index === 0) {
          out += m[0];
          i += m[0].length;
          continue;
        }
      }
      out += s[i];
      vis += 1;
      i += 1;
    }
    return out + "\x1b[0m";
  }

  function fmtLocalTime(isoStr) {
    if (!isoStr) return "";
    const d = new Date(isoStr);
    return d.toLocaleTimeString("en-US", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
  }

  function detectViteLog(msg) {
    return /\[vite\]|\[vite-plugin-svelte\]|VITE\s+v\d/.test(stripAnsi(msg));
  }

  function fmtLogEntry(entry, maxWidth) {
    const ts = fmtLocalTime(entry.timestamp);
    const isVite = entry.service?.startsWith("tauri-dev") && detectViteLog(entry.message || "");
    const svc = isVite ? "vite" : (SVC_SHORT[entry.service] || entry.service?.replace("build:", "bld:") || "?");
    const svcCol = isVite ? C.grn : (SVC_COLOR[entry.service] || C.ylw);

    let lvl = "   ";
    if (entry.level === "daemon") lvl = `${C.ylw}dae${C.r}`;
    else if (entry.level === "stderr" && ERROR_PATTERNS.test(entry.message)) lvl = `${C.red}ERR${C.r}`;

    const prefix = `${C.dim}${ts}${C.r} ${lvl} ${svcCol}${svc.padEnd(5)}${C.r} `;
    const prefixLen = 19;
    const msgMax = maxWidth - prefixLen - 1;
    let msg = stripAnsi(entry.message || "");
    if (msg.length > msgMax && msgMax > 0) msg = msg.slice(0, msgMax);
    return prefix + msg;
  }

  function renderHeader() {
    const title = ` Coyote Dev Daemon :${DAEMON_PORT} `;
    const daemonUp = lastStatus && daemonStartedAt
      ? `started ${fmtLocalTime(new Date(daemonStartedAt).toISOString())} │ up ${fmtUptime(Math.floor((Date.now() - daemonStartedAt) / 1000))}`
      : "connecting...";
    const pad = Math.max(0, cols - title.length - daemonUp.length - 1);
    const line = C.bgBlu + C.wht + C.b + title + " ".repeat(pad) + C.r + C.bgBlu + C.dim + daemonUp + " " + C.r;
    return CLR_LINE + truncVis(line, cols) + "\n";
  }

  function renderServices() {
    const svcs = lastStatus?.services || {};
    const tauriGroup = ["tauri-dev", "coyote-bin"];
    const runningTauri = tauriGroup.filter((n) => svcs[n]?.status === "running");
    const visibleServices = SVC_ORDER.filter((name) => {
      if (tauriGroup.includes(name)) {
        if (runningTauri.length > 0) return runningTauri.includes(name);
        return name === "coyote-bin";
      }
      return true;
    });

    let out = CLR_LINE + "\n";
    for (const name of visibleServices) {
      const svc = svcs[name];
      const up = svc?.status === "running";
      const phase = svc?.phase || "idle";
      const isRebuilding = up && phase === "rebuilding";
      const isError = up && phase === "error";
      const icon = isRebuilding ? `${C.ylw}●${C.r}` : isError ? `${C.red}●${C.r}` : up ? `${C.grn}●${C.r}` : `${C.dim}○${C.r}`;
      const nameStr = (up ? C.wht : C.dim) + name.padEnd(20) + C.r;
      const statusStr = isRebuilding ? `${C.ylw}rebuilding ⟳${C.r}` : isError ? `${C.red}error ✗${C.r}` : up ? `${C.grn}running${C.r}` : `${C.dim}stopped${C.r}`;
      const pid = up ? `${C.dim}pid${C.r} ${svc.pid}` : "";
      const startedStr = up && svc.startedAt ? `${C.dim}since${C.r} ${fmtLocalTime(svc.startedAt)}` : "";
      const uptime = up ? fmtUptime(svc.uptime) : "";
      const logCount = svc ? `${svc.logCount} logs` : "";
      let phaseElapsed = "";
      if ((isRebuilding || isError) && svc.phaseChangedAt) {
        const sec = Math.floor((Date.now() - svc.phaseChangedAt) / 1000);
        const label = sec >= 60 ? fmtUptime(sec) : `${sec}s`;
        phaseElapsed = `${C.dim}${label}${C.r}`;
      }
      const exitStr = (!up && svc?.exitCode !== null && svc?.exitCode !== undefined)
        ? `${C.red}exit ${svc.exitCode}${C.r}` : "";
      const row = `  ${icon} ${nameStr} ${statusStr.padEnd(18)} ${pid.padEnd(16)} ${startedStr ? startedStr + "  " : ""}${uptime.padEnd(8)} ${C.dim}${logCount}${C.r} ${phaseElapsed ? "  " + phaseElapsed : ""} ${exitStr}`;
      out += CLR_LINE + truncVis(row, cols) + "\n";
    }
    if (visibleServices.length < SVC_ORDER.length) out += CLR_LINE + "\n";
    return out;
  }

  function renderBuildStatus() {
    const bld = lastStatus?.build;
    let buildLine;
    if (bld?.locked) {
      const phaseLabel = bld.phase === "cleaning" ? "cleaning" : "building";
      const icon = bld.phase === "cleaning" ? "✸" : "◈";
      let elapsed = "";
      if (bld.startedAt) {
        const sec = Math.floor((Date.now() - new Date(bld.startedAt).getTime()) / 1000);
        elapsed = `  ${C.dim}${fmtUptime(sec)}${C.r}`;
      }
      buildLine = `  ${C.ylw}${icon}${C.r} ${C.ylw}${phaseLabel}: ${bld.name}${C.r}  ${bld.logCount} logs${elapsed}`;
    } else if (bld?.exitCode !== null && bld?.exitCode !== undefined && bld?.name) {
      const ec = bld.exitCode === 0 ? C.grn : C.red;
      const wasClean = bld.name?.startsWith("clean:");
      const label = wasClean ? "clean" : "build";
      const dur = bld.duration ? `  ${C.dim}${fmtUptime(bld.duration)}${C.r}` : "";
      buildLine = `  ${ec}◈${C.r} ${label}: ${bld.name} → ${ec}exit ${bld.exitCode}${C.r}${dur}`;
    } else {
      buildLine = `  ${C.dim}◈ build: idle${C.r}`;
    }
    return CLR_LINE + truncVis(buildLine, cols) + "\n";
  }

  function renderOrchestration() {
    const orch = lastStatus?.orchestration;
    if (orch?.active) return CLR_LINE + truncVis(`  ${C.cyn}⟳ ${orch.phase}${C.r}`, cols) + "\n";
    return CLR_LINE + "\n";
  }

  function renderLogSeparator() {
    const filterLabel = logFilter || "all";
    const sepText = `── logs: ${filterLabel} `;
    return CLR_LINE + C.dim + sepText + "─".repeat(Math.max(0, cols - sepText.length)) + C.r + "\n";
  }

  function renderLogArea() {
    const HEADER_ROWS = 10;
    const FOOTER_ROWS = 2;
    const logAreaRows = Math.max(1, rows - HEADER_ROWS - FOOTER_ROWS);
    const visible = logLines.slice(-logAreaRows);
    let out = "";
    for (let i = 0; i < logAreaRows; i++) {
      const entry = visible[i];
      out += entry ? CLR_LINE + truncVis(fmtLogEntry(entry, cols), cols) + "\n" : CLR_LINE + "\n";
    }
    return out;
  }

  function renderFooterRule() {
    return CLR_LINE + C.dim + "─".repeat(cols) + C.r + "\n";
  }

  function renderShortcutBar() {
    if (actionMsg) return CLR_LINE + truncVis(` ${C.ylw}${actionMsg}${C.r}`, cols) + CLR_BELOW;

    const kPrefix = (key, rest) => `${C.b}${key}${C.r} ${C.dim}${rest}${C.r}`;
    const kInline = (key, word) => {
      const idx = word.toLowerCase().indexOf(key.toLowerCase());
      if (idx === -1) return kPrefix(key, word);
      const before = word.slice(0, idx);
      const after = word.slice(idx + 1);
      return `${C.dim}${before}${C.r}${C.b}${key}${C.r}${C.dim}${after}${C.r}`;
    };
    const sep = `  ${C.dim}│${C.r}  `;
    const label = (text) => `${C.dim}${text}${C.r} `;

    const filterChips = [];
    for (const s of svcMeta) {
      if (!s.logFilterKey) continue;
      filterChips.push(`${C.b}${s.logFilterKey}${C.r} ${C.dim}${s.short}${C.r}`);
    }
    filterChips.push(`${C.b}3${C.r} ${C.dim}bld${C.r}`);
    filterChips.push(kInline("a", "all"));
    const filters = label(" logs:") + filterChips.join("  ");

    const buildChips = [];
    for (const s of svcMeta) {
      if (!s.buildKey || !s.hasBuild) continue;
      buildChips.push(kInline(s.buildKey, s.short));
    }
    buildChips.push(kInline("d", "dev"));
    const builds = label("build:") + buildChips.join("  ");

    const global =
      label("bulk:") +
      `${kInline("r", "reload")}  ` +
      `${kInline("R", "restart")}  ` +
      `${kInline("S", "stop")}  ` +
      `${kInline("c", "clean")}  ` +
      `${kInline("q", "quit")}  ` +
      `${kInline("K", "kill")}`;

    return CLR_LINE + truncVis(filters + sep + builds + sep + global, cols) + CLR_BELOW;
  }

  let lastFrame = "";
  let renderDirty = true;
  process.stdout.on("resize", () => { lastFrame = ""; renderDirty = true; });

  function markDirty() { renderDirty = true; }

  function render() {
    if (!renderDirty) return;
    renderDirty = false;
    const frame =
      HOME
      + renderHeader()
      + renderServices()
      + renderBuildStatus()
      + renderOrchestration()
      + renderLogSeparator()
      + renderLogArea()
      + renderFooterRule()
      + renderShortcutBar();
    if (frame === lastFrame) return;
    lastFrame = frame;
    if (process.stdout.cork) process.stdout.cork();
    write(frame);
    if (process.stdout.uncork) process.stdout.uncork();
  }

  let logFetchTag = 0;
  async function fetchLogHistory() {
    const tag = ++logFetchTag;
    const filter = logFilter;
    try {
      let urlPath;
      if (filter === "build") urlPath = "/build/logs?tail=200";
      else if (filter) urlPath = `/services/${filter}/logs?tail=200`;
      else urlPath = "/logs?tail=200";
      const data = await req("GET", urlPath);
      if (tag !== logFetchTag) return;
      const entries = data.logs || [];
      logLines = entries;
      if (entries.length > 0) logCursor = entries[entries.length - 1].index;
      markDirty();
    } catch {}
  }

  let lastLogFilter = "__init__";
  function checkFilterChange() {
    if (logFilter !== lastLogFilter) {
      lastLogFilter = logFilter;
      logLines = [];
      logCursor = -1;
      markDirty();
      fetchLogHistory();
    }
  }

  const httpMod = await import("node:http");
  let sseReq = null;
  async function connectSSE() {
    return new Promise((resolve) => {
      const req = httpMod.get(
        { hostname: DAEMON_HOST, port: DAEMON_PORT, path: "/events", headers: { Accept: "text/event-stream" } },
        (res) => {
          let buf = "";
          res.setEncoding("utf8");
          res.on("data", (chunk) => {
            if (!running) return;
            buf += chunk;
            const parts = buf.split("\n\n");
            buf = parts.pop();
            for (const raw of parts) {
              if (!raw.trim()) continue;
              let eventType = "message";
              let data = "";
              for (const line of raw.split("\n")) {
                if (line.startsWith("event: ")) eventType = line.slice(7);
                else if (line.startsWith("data: ")) data = line.slice(6);
              }
              if (!data) continue;
              let parsed;
              try { parsed = JSON.parse(data); } catch { continue; }

              if (eventType === "status") {
                lastStatus = parsed;
                if (!daemonStartedAt && parsed?.daemonStartedAt) {
                  daemonStartedAt = new Date(parsed.daemonStartedAt).getTime();
                }
                markDirty();
              } else if (eventType === "meta") {
                rebuildMetaTables(parsed);
                markDirty();
              } else if (eventType === "log") {
                const entry = parsed;
                const service = entry.service || "";
                let keep = false;
                if (!logFilter) keep = true;
                else if (logFilter === "build") keep = service.startsWith("build:");
                else keep = service === logFilter;
                if (keep && entry.index > logCursor) {
                  logLines.push(entry);
                  logCursor = entry.index;
                  if (logLines.length > 1000) logLines = logLines.slice(-500);
                  markDirty();
                }
              }
            }
          });
          res.on("end", resolve);
          res.on("error", resolve);
        }
      );
      req.on("error", resolve);
      sseReq = req;
    });
  }

  (async () => {
    while (running) {
      await connectSSE();
      if (!running) break;
      markDirty();
      await sleep(1000);
    }
  })();

  const renderTimer = setInterval(() => {
    checkFilterChange();
    render();
  }, 250);
  const forceTick = setInterval(() => { markDirty(); }, 1000);

  const origExit = exitDash;
  exitDash = () => {
    if (!running) return;
    clearInterval(renderTimer);
    clearInterval(forceTick);
    try { sseReq?.destroy(); } catch {}
    origExit();
  };

  lastLogFilter = logFilter;
  await fetchLogHistory();

  await new Promise((resolve) => {
    const check = setInterval(() => {
      if (!running) { clearInterval(check); resolve(); }
    }, 100);
  });
}

// ── main ───────────────────────────────────────────────────────────────────────
async function main() {
  const args = process.argv.slice(2);
  const command = args[0] || "status";
  const rest = args.slice(1);

  if (command === "help" || command === "--help" || command === "-h") {
    return cmdHelp();
  }

  if (command === "shutdown") {
    if (!(await isDaemonRunning())) {
      out({ action: "not_running" });
      return;
    }
    return cmdShutdown();
  }

  const daemonOk = await ensureDaemon();
  if (!daemonOk) {
    err("daemon_start_failed", { message: "Could not start or reach daemon on port " + DAEMON_PORT });
  }

  switch (command) {
    case "status": return cmdStatus();
    case "start": return cmdStart(rest[0]);
    case "stop": return cmdStop(rest[0]);
    case "restart": return cmdRestart(rest[0]);
    case "logs": return cmdLogs(rest);
    case "tail": return cmdTail(rest[0]);
    case "build": return cmdBuild(rest);
    case "clean": return cmdClean(rest);
    case "dev": return cmdDev(rest);
    case "dashboard":
    case "dash": return cmdDashboard();
    default:
      err("unknown_command", {
        command,
        available: ["status", "start", "stop", "restart", "logs", "tail", "build", "clean", "dev", "dashboard", "help", "shutdown"],
      });
  }
}

main().catch((e) => err("unexpected_error", { message: e.message }));
