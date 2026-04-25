#!/usr/bin/env node
// Dev-server daemon — background HTTP server managing dev services + build locking.
// Default port 9860 (override with COYOTE_DEV_PORT). Auto-started by cli.mjs.
//
// Ported from ai-notifications dev-server skill, trimmed for single-binary
// Tauri projects (no hub, no workers, no crash-loop alerts).

import http from "node:http";
import net from "node:net";
import fs from "node:fs";
import { spawn, execSync } from "node:child_process";
import { writeFileSync, unlinkSync, mkdirSync, existsSync, readFileSync, copyFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// ── paths ──────────────────────────────────────────────────────────────────────
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SKILL_DIR = path.resolve(__dirname, "..");
const STATE_DIR = path.join(SKILL_DIR, "state");
const PID_FILE = path.join(STATE_DIR, "daemon.pid");
const PROJECT_ROOT = path.resolve(SKILL_DIR, "..", "..", "..");
const SRC_TAURI_DIR = path.join(PROJECT_ROOT, "src-tauri");
const RELEASE_DIR = path.join(SRC_TAURI_DIR, "target", "release");

// ── config ─────────────────────────────────────────────────────────────────────
const PORT = parseInt(process.env.COYOTE_DEV_PORT || "9860", 10);
const VITE_DEV_URL = process.env.COYOTE_VITE_URL || "http://localhost:1421";
const MAX_LOG_LINES = 2000;
const LOG_DIR = path.join(SKILL_DIR, "state", "logs");
const MAX_LOG_FILE_BYTES = 2 * 1024 * 1024;
const IS_WIN = process.platform === "win32";

const RESTART_DELAY_MS = 750;
const CRASH_LOOP_WINDOW_MS = 60_000;
const CRASH_LOOP_THRESHOLD = 3;

const SERVICES_DIR = path.join(SKILL_DIR, "services");

/** Derive shadow-copy path from a binary path (.exe → .active.exe, else → .active) */
function shadowPath(binaryPath) {
  if (IS_WIN) return binaryPath.replace(/\.exe$/, ".active.exe");
  return binaryPath + ".active";
}

function sleepSync(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function killByImageName(imageName) {
  try {
    if (IS_WIN) {
      execSync(`taskkill /f /im "${imageName}"`, { windowsHide: true, stdio: "ignore", timeout: 5000 });
    } else {
      execSync(`pkill -f "${imageName}"`, { stdio: "ignore", timeout: 5000 });
    }
    return true;
  } catch {
    return false;
  }
}

function cleanStaleShadows() {
  try {
    if (!existsSync(RELEASE_DIR)) return;
    for (const f of readdirSync(RELEASE_DIR)) {
      if (f.endsWith(".active.exe") || f.endsWith(".active")) {
        const fullPath = path.join(RELEASE_DIR, f);
        try {
          unlinkSync(fullPath);
        } catch {
          killByImageName(f);
          sleepSync(1000);
          try { unlinkSync(fullPath); } catch {}
        }
      }
    }
  } catch {}
}

// ── pluggable service definitions ─────────────────────────────────────────────
let SERVICE_DEFS = {};
let BUILD_DEFS = {};
let CLEAN_DEFS = {};
let PLUGINS = new Map();

function makePluginCtx() {
  return {
    IS_WIN,
    RELEASE_DIR,
    PROJECT_ROOT,
    SRC_TAURI_DIR,
    VITE_DEV_URL,
    bin: (name) => path.join(RELEASE_DIR, IS_WIN ? `${name}.exe` : name),
  };
}

async function loadPlugins() {
  const ctx = makePluginCtx();
  const newPlugins = new Map();
  const newService = {};
  const newBuild = {};
  const newClean = {};

  if (!existsSync(SERVICES_DIR)) {
    throw new Error(`Services directory not found: ${SERVICES_DIR}`);
  }

  const files = readdirSync(SERVICES_DIR).filter((f) => f.endsWith(".mjs")).sort();
  for (const f of files) {
    const filePath = path.join(SERVICES_DIR, f);
    const url = pathToFileURL(filePath).href + `?v=${statSync(filePath).mtimeMs}`;
    let mod;
    try {
      mod = await import(url);
    } catch (e) {
      throw new Error(`Failed to load plugin ${f}: ${e.message}`);
    }
    const factory = mod.default;
    if (typeof factory !== "function") {
      throw new Error(`Plugin ${f} must export a default factory function`);
    }
    let plugin;
    try {
      plugin = factory(ctx);
    } catch (e) {
      throw new Error(`Plugin ${f} factory threw: ${e.message}`);
    }
    if (!plugin || !plugin.name) {
      throw new Error(`Plugin ${f} must return an object with a name field`);
    }
    if (newPlugins.has(plugin.name)) {
      throw new Error(`Plugin ${f}: duplicate plugin name "${plugin.name}"`);
    }
    newPlugins.set(plugin.name, plugin);

    if (plugin.service) newService[plugin.name] = plugin.service;
    if (plugin.build) {
      newBuild[plugin.name] = plugin.build;
      for (const alias of plugin.build.aliases || []) {
        if (newBuild[alias] && newBuild[alias] !== plugin.build) {
          throw new Error(`Plugin ${plugin.name}: build alias "${alias}" collides with another plugin`);
        }
        newBuild[alias] = plugin.build;
      }
    }
    if (plugin.clean) {
      newClean[plugin.name] = plugin.clean;
      for (const alias of plugin.clean.aliases || []) {
        if (newClean[alias] && newClean[alias] !== plugin.clean) {
          throw new Error(`Plugin ${plugin.name}: clean alias "${alias}" collides with another plugin`);
        }
        newClean[alias] = plugin.clean;
      }
    }
  }

  const usedLogKeys = new Map();
  const usedBuildKeys = new Map();
  for (const [name, plugin] of newPlugins) {
    const d = plugin.dashboard;
    if (!d) continue;
    if (d.logFilterKey) {
      if (usedLogKeys.has(d.logFilterKey)) {
        throw new Error(`Duplicate logFilterKey "${d.logFilterKey}": ${name} vs ${usedLogKeys.get(d.logFilterKey)}`);
      }
      usedLogKeys.set(d.logFilterKey, name);
    }
    if (d.buildKey) {
      if (usedBuildKeys.has(d.buildKey)) {
        throw new Error(`Duplicate buildKey "${d.buildKey}": ${name} vs ${usedBuildKeys.get(d.buildKey)}`);
      }
      usedBuildKeys.set(d.buildKey, name);
    }
  }

  SERVICE_DEFS = newService;
  BUILD_DEFS = newBuild;
  CLEAN_DEFS = newClean;
  PLUGINS = newPlugins;

  return { names: Array.from(newPlugins.keys()), count: newPlugins.size };
}

// ── state ──────────────────────────────────────────────────────────────────────
const daemonStartedAt = Date.now();
let globalLogIndex = 0;

const services = new Map();

function syncServiceStates() {
  const added = [];
  const removed = [];
  for (const name of Object.keys(SERVICE_DEFS)) {
    if (!services.has(name)) {
      services.set(name, {
        process: null,
        logBuffer: [],
        startedAt: null,
        exitCode: null,
        phase: "idle",
        phaseChangedAt: null,
        intended: false,
        crashTimes: [],
      });
      added.push(name);
    }
  }
  for (const name of Array.from(services.keys())) {
    if (!SERVICE_DEFS[name]) {
      const svc = services.get(name);
      if (svc?.process && isProcessAlive(svc.process)) {
        stopServiceSync(name);
      }
      services.delete(name);
      removed.push(name);
    }
  }
  return { added, removed };
}

let shuttingDown = false;
let buildLock = null;
let orchestration = { active: false, phase: "idle", error: null };

// ── logging ────────────────────────────────────────────────────────────────────
mkdirSync(LOG_DIR, { recursive: true });
const logStreams = new Map();
function getLogStream(name) {
  if (logStreams.has(name)) return logStreams.get(name);
  const filePath = path.join(LOG_DIR, `${name}.log`);
  const stream = fs.createWriteStream(filePath, { flags: "a" });
  logStreams.set(name, { stream, filePath, bytesWritten: 0 });
  try {
    const stat = fs.statSync(filePath);
    logStreams.get(name).bytesWritten = stat.size;
  } catch {}
  return logStreams.get(name);
}

function appendToLogFile(serviceName, timestamp, level, message) {
  const handle = getLogStream(serviceName);
  const line = `${timestamp} [${level}] ${message}\n`;
  handle.stream.write(line);
  handle.bytesWritten += Buffer.byteLength(line);
  if (handle.bytesWritten > MAX_LOG_FILE_BYTES) {
    handle.stream.end();
    const prevPath = handle.filePath.replace(/\.log$/, ".prev.log");
    try { fs.renameSync(handle.filePath, prevPath); } catch {}
    handle.stream = fs.createWriteStream(handle.filePath, { flags: "a" });
    handle.bytesWritten = 0;
  }
}

function pushServiceLog(service, level, message) {
  const svc = services.get(service);
  if (!svc) return;
  const timestamp = new Date().toISOString();
  const entry = { index: globalLogIndex++, timestamp, level, service, message };
  svc.logBuffer.push(entry);
  if (svc.logBuffer.length > MAX_LOG_LINES) {
    svc.logBuffer = svc.logBuffer.slice(-MAX_LOG_LINES);
  }
  appendToLogFile(service, timestamp, level, message);
  sseEmit("log", entry);
}

function pushBuildLog(level, message) {
  if (!buildLock) return;
  const timestamp = new Date().toISOString();
  const serviceName = `build:${buildLock.name}`;
  const entry = { index: globalLogIndex++, timestamp, level, service: serviceName, message };
  buildLock.logBuffer.push(entry);
  if (buildLock.logBuffer.length > MAX_LOG_LINES) {
    buildLock.logBuffer = buildLock.logBuffer.slice(-MAX_LOG_LINES);
  }
  appendToLogFile("build", timestamp, level, message);
  sseEmit("log", entry);
}

let daemonLogs = [];
function pushDaemonLog(message) {
  const timestamp = new Date().toISOString();
  const entry = { index: globalLogIndex++, timestamp, level: "daemon", service: "daemon", message };
  daemonLogs.push(entry);
  if (daemonLogs.length > 200) daemonLogs = daemonLogs.slice(-200);
  appendToLogFile("daemon", timestamp, "daemon", message);
  sseEmit("log", entry);
}

// ── SSE broadcast ───────────────────────────────────────────────────────────
const sseClients = new Set();
function sseEmit(event, data) {
  if (sseClients.size === 0) return;
  const payload = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
  for (const res of sseClients) {
    try { res.write(payload); } catch { sseClients.delete(res); }
  }
}

let statusEmitScheduled = false;
function scheduleStatusEmit() {
  if (statusEmitScheduled || sseClients.size === 0) return;
  statusEmitScheduled = true;
  setImmediate(() => {
    statusEmitScheduled = false;
    try { sseEmit("status", fullStatus()); } catch {}
  });
}

setInterval(() => {
  if (sseClients.size > 0) {
    try { sseEmit("status", fullStatus()); } catch {}
  }
}, 5000).unref();

// ── rebuild phase detection (legacy tauri-dev only) ─────────────────────────
// eslint-disable-next-line no-control-regex
const ANSI_RE = /\x1b\[[0-9;]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b\\\\/g;
function stripAnsi(s) { return s.replace(ANSI_RE, ""); }

function detectRebuildPhase(name, line) {
  const svc = services.get(name);
  if (!svc) return;
  const clean = stripAnsi(line);
  if (/Rebuilding application/.test(clean)) {
    if (svc.phase !== "rebuilding") {
      svc.phase = "rebuilding";
      svc.phaseChangedAt = Date.now();
    }
  } else if (/Compiling coyote.socket/.test(clean)) {
    if (svc.phase !== "rebuilding") {
      svc.phase = "rebuilding";
      svc.phaseChangedAt = Date.now();
    }
  } else if (/Finished.*target/.test(clean)) {
    svc.phase = "idle";
    svc.phaseChangedAt = Date.now();
  } else if (/error\[E\d+\]/.test(clean) || /could not compile/.test(clean)) {
    svc.phase = "error";
    svc.phaseChangedAt = Date.now();
  }
}

// ── process helpers ────────────────────────────────────────────────────────────
function isProcessAlive(proc) {
  if (!proc || proc.exitCode !== null) return false;
  try { process.kill(proc.pid, 0); return true; } catch { return false; }
}

function isServiceRunning(name) {
  const svc = services.get(name);
  return svc?.process ? isProcessAlive(svc.process) : false;
}

async function killProcess(proc, label, { noTreeKill = false } = {}) {
  if (!proc || !isProcessAlive(proc)) return;
  const pid = proc.pid;
  const treeFlag = (!noTreeKill) ? ["/t"] : [];
  try {
    if (IS_WIN) {
      spawn("taskkill", ["/f", ...treeFlag, "/pid", String(pid)], { windowsHide: true, stdio: "ignore" });
    } else {
      process.kill(pid, "SIGINT");
    }
  } catch (e) {
    pushDaemonLog(`Kill error (${label} pid ${pid}): ${e.message}`);
  }
  const exited = await new Promise((resolve) => {
    if (!isProcessAlive(proc)) return resolve(true);
    const timeout = setTimeout(() => resolve(false), 5000);
    proc.on("exit", () => { clearTimeout(timeout); resolve(true); });
  });
  if (!exited && isProcessAlive(proc)) {
    try {
      if (IS_WIN) {
        spawn("taskkill", ["/f", ...treeFlag, "/pid", String(pid)], { windowsHide: true, stdio: "ignore" });
      } else {
        process.kill(pid, "SIGKILL");
      }
    } catch {}
  }
}

/**
 * TCP-probe a URL's host:port. Used to detect externally-running services
 * (e.g. a Vite the user already started outside the daemon) so we don't
 * spawn a duplicate. Resolves to true if connect succeeds within 500ms.
 */
function probeHealthUrl(url) {
  return new Promise((resolve) => {
    try {
      const u = new URL(url);
      const port = parseInt(u.port || (u.protocol === "https:" ? "443" : "80"), 10);
      const sock = net.connect({ host: u.hostname, port, timeout: 500 }, () => {
        sock.destroy();
        resolve(true);
      });
      sock.on("error", () => resolve(false));
      sock.on("timeout", () => { sock.destroy(); resolve(false); });
    } catch {
      resolve(false);
    }
  });
}

// ── service management ─────────────────────────────────────────────────────────
function startService(name) {
  const def = SERVICE_DEFS[name];
  if (!def) return { action: "error", message: `Unknown service: ${name}` };

  if (isServiceRunning(name)) {
    const svc = services.get(name);
    return { action: "already_running", service: name, pid: svc.process.pid, uptime: uptimeFor(svc) };
  }

  const autoStarted = [];
  if (def.requires) {
    for (const dep of def.requires) {
      if (!isServiceRunning(dep)) {
        pushServiceLog(name, "daemon", `Auto-starting required service: ${dep}`);
        const depResult = startService(dep);
        if (depResult.action === "started" || depResult.action === "already_running") {
          autoStarted.push(dep);
        } else {
          pushServiceLog(name, "daemon", `Failed to start required service ${dep}: ${depResult.message || depResult.action}`);
        }
      }
    }
  }

  const stopped = [];
  if (def.group) {
    for (const [otherName, otherDef] of Object.entries(SERVICE_DEFS)) {
      if (otherName !== name && otherDef.group === def.group && isServiceRunning(otherName)) {
        stopServiceSync(otherName);
        stopped.push(otherName);
      }
    }
  }

  const svc = services.get(name);
  svc.logBuffer = [];
  svc.exitCode = null;
  svc.phase = name.startsWith("tauri-dev") ? "rebuilding" : "idle";
  svc.phaseChangedAt = name.startsWith("tauri-dev") ? Date.now() : null;

  let command = def.binary || def.command;
  if (def.binary && def.shadowCopy) {
    if (!existsSync(def.binary)) {
      pushServiceLog(name, "daemon", `Binary not found: ${def.binary} — build first`);
      return { action: "error", message: `Binary not found: ${path.basename(def.binary)}. Run build first.` };
    }
    const activeBin = shadowPath(def.binary);
    try {
      try { unlinkSync(activeBin); } catch {}
      copyFileSync(def.binary, activeBin);
      command = activeBin;
      pushServiceLog(name, "daemon", `Shadow-copied to ${path.basename(activeBin)}`);
    } catch (e) {
      const imageName = path.basename(activeBin);
      pushServiceLog(name, "daemon", `Shadow copy blocked by locked ${imageName}, killing orphan...`);
      killByImageName(imageName);
      sleepSync(1500);
      try {
        try { unlinkSync(activeBin); } catch {}
        copyFileSync(def.binary, activeBin);
        command = activeBin;
        pushServiceLog(name, "daemon", `Shadow-copied to ${imageName} after killing orphan`);
      } catch (e2) {
        pushServiceLog(name, "daemon", `Shadow copy failed after retry: ${e2.message}`);
        return { action: "error", message: `Shadow copy failed after killing orphan: ${e2.message}` };
      }
    }
  }

  pushServiceLog(name, "daemon", `Starting service: ${command} ${(def.args || []).join(" ")}`);
  if (def.env && Object.keys(def.env).length > 0) {
    pushServiceLog(name, "daemon", `Env: ${Object.keys(def.env).join(", ")}`);
  }

  svc.process = spawn(command, def.args || [], {
    cwd: def.cwd || PROJECT_ROOT,
    shell: true,
    windowsHide: true,
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, ...(def.env || {}) },
  });

  svc.startedAt = Date.now();
  scheduleStatusEmit();

  svc.process.stdout.on("data", (chunk) => {
    for (const line of chunk.toString().split(/\r?\n/).filter(Boolean)) {
      pushServiceLog(name, "stdout", line);
      if (name.startsWith("tauri-dev")) detectRebuildPhase(name, line);
    }
  });

  svc.process.stderr.on("data", (chunk) => {
    for (const line of chunk.toString().split(/\r?\n/).filter(Boolean)) {
      pushServiceLog(name, "stderr", line);
      if (name.startsWith("tauri-dev")) detectRebuildPhase(name, line);
    }
  });

  svc.process.on("exit", (code, signal) => {
    pushServiceLog(name, "daemon", `Service exited: code=${code} signal=${signal}`);
    svc.exitCode = code;
    svc.process = null;
    svc.startedAt = null;
    scheduleStatusEmit();
    if (def.shadowCopy && def.binary) {
      try { unlinkSync(shadowPath(def.binary)); } catch {}
    }
    if (svc.intended && !shuttingDown) handleServiceCrash(name);
  });

  svc.process.on("error", (err) => {
    pushServiceLog(name, "daemon", `Service error: ${err.message}`);
    svc.exitCode = -1;
    svc.process = null;
    svc.startedAt = null;
    if (def.shadowCopy && def.binary) {
      try { unlinkSync(shadowPath(def.binary)); } catch {}
    }
    if (svc.intended && !shuttingDown) handleServiceCrash(name);
  });

  svc.intended = true;

  const result = { action: "started", service: name, pid: svc.process.pid };
  if (stopped.length) result.stopped = stopped;
  if (autoStarted.length) result.autoStarted = autoStarted;
  return result;
}

function stopServiceSync(name) {
  const svc = services.get(name);
  if (!svc?.process || !isProcessAlive(svc.process)) {
    if (svc) svc.intended = false;
    return;
  }
  svc.intended = false;
  const pid = svc.process.pid;
  const def = SERVICE_DEFS[name];
  const treeFlag = def?.noTreeKill ? [] : ["/t"];
  pushServiceLog(name, "daemon", `Stopping service (pid ${pid})`);
  try {
    if (IS_WIN) {
      spawn("taskkill", ["/f", ...treeFlag, "/pid", String(pid)], { windowsHide: true, stdio: "ignore" });
    } else {
      process.kill(pid, "SIGINT");
    }
  } catch {}
}

async function stopService(name) {
  const svc = services.get(name);
  if (!svc) return { action: "error", message: `Unknown service: ${name}` };
  if (!svc.process || !isProcessAlive(svc.process)) {
    svc.intended = false;
    return { action: "not_running", service: name };
  }
  svc.intended = false;
  const def = SERVICE_DEFS[name];
  const pid = svc.process.pid;
  pushServiceLog(name, "daemon", `Stopping service (pid ${pid})`);
  await killProcess(svc.process, name, { noTreeKill: !!def?.noTreeKill });
  svc.process = null;
  svc.startedAt = null;
  svc.phase = "idle";
  svc.phaseChangedAt = null;
  if (def?.shadowCopy && def?.binary) {
    try { unlinkSync(shadowPath(def.binary)); } catch {}
  }
  return { action: "stopped", service: name };
}

async function stopAll() {
  const results = {};
  for (const name of services.keys()) {
    if (isServiceRunning(name)) {
      results[name] = await stopService(name);
    }
  }
  return { action: "stopped_all", services: results };
}

function uptimeFor(svc) {
  return svc.startedAt ? Math.floor((Date.now() - svc.startedAt) / 1000) : 0;
}

// ── auto-restart (no audio alerts) ──────────────────────────────────────────
function handleServiceCrash(name) {
  const svc = services.get(name);
  if (!svc) return;
  const now = Date.now();
  svc.crashTimes.push(now);
  svc.crashTimes = svc.crashTimes.filter(t => now - t <= CRASH_LOOP_WINDOW_MS);
  const count = svc.crashTimes.length;
  pushServiceLog(
    name, "daemon",
    `Crash detected — auto-restart in ${RESTART_DELAY_MS}ms (${count} crash${count === 1 ? "" : "es"} in last ${CRASH_LOOP_WINDOW_MS / 1000}s)`,
  );
  if (count >= CRASH_LOOP_THRESHOLD) {
    svc.intended = false;
    pushServiceLog(name, "daemon", `CRASH LOOP — giving up auto-restart for ${name} (${count} crashes in ${CRASH_LOOP_WINDOW_MS / 1000}s)`);
    return;
  }
  setTimeout(() => {
    if (!svc.intended || shuttingDown || isServiceRunning(name)) return;
    pushServiceLog(name, "daemon", `Auto-restarting ${name}`);
    const result = startService(name);
    if (result.action === "error") {
      pushServiceLog(name, "daemon", `Auto-restart failed: ${result.message}`);
    }
  }, RESTART_DELAY_MS);
}

// ── build lock ─────────────────────────────────────────────────────────────────
function spawnBuildCommand(command, args, cwd) {
  const proc = spawn(command, args, {
    cwd: cwd || PROJECT_ROOT,
    shell: true,
    windowsHide: true,
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env },
  });
  proc.stdout.on("data", (chunk) => {
    for (const line of chunk.toString().split(/\r?\n/).filter(Boolean)) {
      pushBuildLog("stdout", line);
    }
  });
  proc.stderr.on("data", (chunk) => {
    for (const line of chunk.toString().split(/\r?\n/).filter(Boolean)) {
      pushBuildLog("stderr", line);
    }
  });
  return proc;
}

function waitForExit(proc) {
  return new Promise((resolve) => {
    if (!proc || proc.exitCode !== null) return resolve(proc?.exitCode ?? -1);
    proc.on("exit", (code) => resolve(code ?? -1));
    proc.on("error", () => resolve(-1));
  });
}

async function runBuildPipeline(name, def) {
  buildLock.phase = "build";
  pushBuildLog("daemon", `Building: ${def.command} ${def.args.join(" ")}`);
  const proc = spawnBuildCommand(def.command, def.args, def.cwd);
  buildLock.process = proc;
  const code = await waitForExit(proc);
  if (buildLock) {
    buildLock.exitCode = code;
    buildLock.process = null;
    buildLock.phase = null;
  }
  const buildDuration = buildLock ? Math.floor((Date.now() - buildLock.startedAt) / 1000) : 0;
  if (code !== 0) {
    pushBuildLog("daemon", `Build failed (exit ${code}) after ${buildDuration}s`);
    if (buildLock) buildLock.duration = buildDuration;
    return;
  }
  pushBuildLog("daemon", `Build succeeded in ${buildDuration}s`);
  if (buildLock) buildLock.duration = buildDuration;

  if (def.service) {
    const wasRunning = isServiceRunning(def.service);
    if (wasRunning) {
      pushBuildLog("daemon", `Stopping ${def.service} to swap in new binary...`);
      await stopService(def.service);
    }
    pushBuildLog("daemon", `${wasRunning ? "Restarting" : "Starting"} ${def.service} with new binary...`);
    const startResult = startService(def.service);
    if (startResult.action === "error") {
      pushBuildLog("daemon", `Failed to ${wasRunning ? "restart" : "start"} ${def.service}: ${startResult.message}`);
      return;
    }
    const svcDef = SERVICE_DEFS[def.service];
    if (svcDef?.healthUrl) {
      const healthy = await waitForHealth(svcDef.healthUrl, 30000);
      pushBuildLog("daemon", `${def.service} ${wasRunning ? "restarted" : "started"}${healthy ? " and healthy" : " (health check timed out)"}`);
    } else {
      pushBuildLog("daemon", `${def.service} ${wasRunning ? "restarted" : "started"}`);
    }
  }
}

async function requestBuild(name) {
  const def = BUILD_DEFS[name];
  if (!def) return { action: "error", message: `No build defined for: ${name}. Available: ${Object.keys(BUILD_DEFS).join(", ")}` };

  if (buildLock && buildLock.process && isProcessAlive(buildLock.process)) {
    pushBuildLog("daemon", `Build interrupted — another agent requested a rebuild`);
    await killProcess(buildLock.process, "build");
    buildLock.startedAt = Date.now();
    buildLock.exitCode = null;
    buildLock.phase = null;
    runBuildPipeline(name, def);
    return { action: "restarted", build: name };
  }

  buildLock = { name, startedAt: Date.now(), logBuffer: [], process: null, exitCode: null, phase: null };
  runBuildPipeline(name, def);
  return { action: "building", build: name };
}

function buildStatus() {
  if (!buildLock) return { locked: false, name: null, phase: null, startedAt: null, logCount: 0, exitCode: null, duration: null };
  const running = buildLock.process && isProcessAlive(buildLock.process);
  return {
    locked: running || (buildLock.exitCode === null && buildLock.phase !== null),
    name: buildLock.name,
    phase: buildLock.phase,
    startedAt: new Date(buildLock.startedAt).toISOString(),
    logCount: buildLock.logBuffer.length,
    exitCode: buildLock.exitCode,
    duration: buildLock.duration || null,
  };
}

function buildLogs(since, limit, level) {
  if (!buildLock) return { logs: [], total: 0 };
  let entries = buildLock.logBuffer;
  if (since >= 0) entries = entries.filter(e => e.index > since);
  if (level) entries = entries.filter(e => e.level === level);
  if (limit > 0) entries = entries.slice(-limit);
  return { logs: entries, total: buildLock.logBuffer.length };
}

// ── clean ───────────────────────────────────────────────────────────────────────
async function requestClean(name) {
  const def = CLEAN_DEFS[name];
  if (!def) return { action: "error", message: `No clean target: ${name}. Available: ${Object.keys(CLEAN_DEFS).join(", ")}` };

  if (!def.safe) {
    return { action: "error", message: def.warning || `Clean target '${name}' is not safe. Use explicit confirmation.`, unsafe: true };
  }
  if (buildLock && buildLock.process && isProcessAlive(buildLock.process)) {
    return { action: "error", message: "A build is in progress. Wait for it to finish before cleaning." };
  }

  const wasRunning = def.service ? isServiceRunning(def.service) : false;
  if (wasRunning) {
    pushDaemonLog(`Clean: stopping ${def.service} before cleaning ${name}`);
    await stopService(def.service);
  }

  const args = [];
  for (const pkg of def.packages) {
    args.push("-p", pkg);
  }
  args.push("--release");

  buildLock = { name: `clean:${name}`, startedAt: Date.now(), logBuffer: [], process: null, exitCode: null, phase: "cleaning" };
  pushBuildLog("daemon", `Cleaning: cargo clean ${args.join(" ")}`);
  const proc = spawnBuildCommand("cargo", ["clean", ...args], def.cwd);
  buildLock.process = proc;

  (async () => {
    const code = await waitForExit(proc);
    if (buildLock) {
      buildLock.exitCode = code;
      buildLock.process = null;
      buildLock.phase = null;
    }
    if (code === 0) {
      pushBuildLog("daemon", `Clean succeeded for ${name}`);
    } else {
      pushBuildLog("daemon", `Clean failed for ${name} (exit ${code})`);
    }
    if (wasRunning && def.service && code === 0) {
      pushBuildLog("daemon", `Restarting ${def.service} after clean...`);
      startService(def.service);
    }
  })();

  return { action: "cleaning", target: name, packages: def.packages, wasRunning };
}

async function requestCleanUnsafe(name) {
  const def = CLEAN_DEFS[name];
  if (!def) return { action: "error", message: `No clean target: ${name}` };
  const safeDef = { ...def, safe: true };
  CLEAN_DEFS[name] = safeDef;
  const result = await requestClean(name);
  CLEAN_DEFS[name] = def;
  return result;
}

// ── orchestration ───────────────────────────────────────────────────────────
function waitForBuildDone() {
  return new Promise((resolve) => {
    const check = setInterval(() => {
      if (!buildLock?.process || !isProcessAlive(buildLock.process)) {
        clearInterval(check);
        resolve();
      }
    }, 500);
  });
}

async function orchBuild(target) {
  orchestration.phase = `building-${target}`;
  const result = await requestBuild(target);
  if (result.action === "error") {
    orchestration.phase = "error";
    orchestration.error = result.message;
    orchestration.active = false;
    return false;
  }
  await waitForBuildDone();
  if (buildLock?.exitCode !== 0) {
    orchestration.phase = "error";
    const lastLogs = buildLock?.logBuffer.slice(-5).map(e => e.message).join("\n") || "No logs";
    orchestration.error = `Build ${target} failed (exit ${buildLock?.exitCode}):\n${lastLogs}`;
    orchestration.active = false;
    return false;
  }
  return true;
}

async function startViteAndHealth() {
  // Honor an already-running Vite (user may have started it manually outside
  // the daemon). Skip spawn entirely so we don't crash-loop on port conflict.
  if (await probeHealthUrl(VITE_DEV_URL)) {
    pushDaemonLog(`Vite already serving ${VITE_DEV_URL} externally — skipping spawn`);
    return true;
  }
  if (!isServiceRunning("vite")) {
    const result = startService("vite");
    if (result.action === "error") return false;
  }
  return waitForHealth(VITE_DEV_URL, 30000);
}

async function orchestrateDev(rebuild) {
  orchestration = { active: true, phase: "checking", error: null };

  // Kick off vite in parallel with cargo build.
  const viteReadyPromise = startViteAndHealth();

  try {
    const binPath = PLUGINS.get("coyote-bin")?.service?.binary;
    if (rebuild || !binPath || !existsSync(binPath)) {
      if (!(await orchBuild("coyote-bin"))) return;
    }

    orchestration.phase = "starting-vite";
    const viteReady = await viteReadyPromise;
    if (!viteReady) {
      orchestration.phase = "error";
      const svc = services.get("vite");
      const lastLogs = svc?.logBuffer.slice(-5).map(e => e.message).join("\n") || "No logs";
      orchestration.error = `Vite failed to become healthy within 30s:\n${lastLogs}`;
      orchestration.active = false;
      return;
    }

    orchestration.phase = "starting-tauri";
    if (!isServiceRunning("coyote-bin")) {
      const result = startService("coyote-bin");
      if (result.action === "error") {
        orchestration.phase = "error";
        orchestration.error = result.message;
        orchestration.active = false;
        return;
      }
    }

    orchestration.phase = "ready";
    orchestration.active = false;
  } catch (err) {
    orchestration.phase = "error";
    orchestration.error = err.message;
    orchestration.active = false;
  }
}

function waitForHealth(url, timeoutMs) {
  return new Promise((resolve) => {
    const deadline = Date.now() + timeoutMs;
    const check = () => {
      if (Date.now() > deadline) return resolve(false);
      const urlObj = new URL(url);
      const req = http.get({ hostname: urlObj.hostname, port: urlObj.port, path: urlObj.pathname, timeout: 2000, agent: false }, (res) => {
        resolve(res.statusCode >= 200 && res.statusCode < 400);
      });
      req.on("error", () => setTimeout(check, 1000));
      req.on("timeout", () => { req.destroy(); setTimeout(check, 1000); });
    };
    check();
  });
}

// ── status ─────────────────────────────────────────────────────────────────────
function fullStatus() {
  const svcStatus = {};
  for (const [name, svc] of services) {
    const running = isServiceRunning(name);
    svcStatus[name] = {
      status: running ? "running" : "stopped",
      phase: svc.phase || "idle",
      phaseChangedAt: svc.phaseChangedAt,
      pid: svc.process?.pid ?? null,
      startedAt: svc.startedAt ? new Date(svc.startedAt).toISOString() : null,
      uptime: uptimeFor(svc),
      logCount: svc.logBuffer.length,
      exitCode: svc.exitCode,
    };
  }
  return {
    daemon: true,
    daemonStartedAt: new Date(daemonStartedAt).toISOString(),
    port: PORT,
    services: svcStatus,
    build: buildStatus(),
    orchestration: { active: orchestration.active, phase: orchestration.phase, error: orchestration.error },
  };
}

function interleavedLogs(since, limit, level) {
  let all = [];
  for (const svc of services.values()) all = all.concat(svc.logBuffer);
  all = all.concat(daemonLogs);
  if (buildLock) all = all.concat(buildLock.logBuffer);
  all.sort((a, b) => a.index - b.index);
  if (since >= 0) all = all.filter(e => e.index > since);
  if (level) all = all.filter(e => e.level === level);
  if (limit > 0) all = all.slice(-limit);
  return { logs: all, total: globalLogIndex };
}

function serviceLogs(name, since, limit, level) {
  const svc = services.get(name);
  if (!svc) return { logs: [], total: 0, error: `Unknown service: ${name}` };
  let entries = svc.logBuffer;
  if (since >= 0) entries = entries.filter(e => e.index > since);
  if (level) entries = entries.filter(e => e.level === level);
  if (limit > 0) entries = entries.slice(-limit);
  return { logs: entries, total: svc.logBuffer.length };
}

function buildMeta() {
  const list = [];
  for (const [name, plugin] of PLUGINS) {
    list.push({
      name,
      hasService: !!plugin.service,
      hasBuild: !!plugin.build,
      hasClean: !!plugin.clean,
      requires: plugin.service?.requires || [],
      group: plugin.service?.group || null,
      dashboard: plugin.dashboard || null,
    });
  }
  list.sort((a, b) => {
    const ao = a.dashboard?.order ?? 999;
    const bo = b.dashboard?.order ?? 999;
    if (ao !== bo) return ao - bo;
    return a.name.localeCompare(b.name);
  });
  return { services: list };
}

// ── HTTP server ────────────────────────────────────────────────────────────────
function parseQuery(url) {
  const u = new URL(url, "http://localhost");
  const params = {};
  for (const [k, v] of u.searchParams) params[k] = v;
  return { pathname: u.pathname, params };
}

function json(res, data, status = 200) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(data));
}

function parseLogParams(params) {
  return {
    since: params.since !== undefined ? parseInt(params.since, 10) : -1,
    limit: params.limit !== undefined ? parseInt(params.limit, 10) : 0,
    level: params.level || null,
  };
}

const server = http.createServer(async (req, res) => {
  const { pathname, params } = parseQuery(req.url);
  const method = req.method;

  try {
    if (method === "GET" && pathname === "/") {
      return json(res, { ok: true, pid: process.pid, port: PORT });
    }

    if (method === "GET" && pathname === "/events") {
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache, no-transform",
        "Connection": "keep-alive",
        "X-Accel-Buffering": "no",
      });
      try { req.socket.setNoDelay(true); } catch {}
      res.write(`event: status\ndata: ${JSON.stringify(fullStatus())}\n\n`);
      res.write(`event: meta\ndata: ${JSON.stringify(buildMeta())}\n\n`);
      sseClients.add(res);
      const drop = () => { sseClients.delete(res); };
      req.on("close", drop);
      req.on("error", drop);
      res.on("error", drop);
      return;
    }

    if (method === "GET" && pathname === "/status") return json(res, fullStatus());
    if (method === "GET" && pathname === "/meta") return json(res, buildMeta());

    if (method === "POST" && pathname === "/reload-services") {
      try {
        const before = new Set(PLUGINS.keys());
        const result = await loadPlugins();
        const after = new Set(PLUGINS.keys());
        const stateDiff = syncServiceStates();
        const added = result.names.filter((n) => !before.has(n));
        const removed = [...before].filter((n) => !after.has(n));
        pushDaemonLog(`Reloaded plugins: ${result.count} loaded, +${added.length} -${removed.length}`);
        sseEmit("meta", buildMeta());
        scheduleStatusEmit();
        return json(res, {
          action: "reloaded", count: result.count, added, removed,
          stateAdded: stateDiff.added, stateRemoved: stateDiff.removed,
        });
      } catch (err) {
        pushDaemonLog(`Reload failed: ${err.message}`);
        return json(res, { error: err.message }, 400);
      }
    }

    const svcMatch = pathname.match(/^\/services\/([^/]+)(?:\/(.+))?$/);
    if (svcMatch) {
      const name = svcMatch[1];
      const action = svcMatch[2] || null;
      if (!SERVICE_DEFS[name]) {
        return json(res, { error: `Unknown service: ${name}`, available: Object.keys(SERVICE_DEFS) }, 404);
      }
      if (method === "POST" && action === "start") return json(res, startService(name));
      if (method === "POST" && action === "stop") return json(res, await stopService(name));
      if (method === "POST" && action === "restart") {
        await stopService(name);
        return json(res, startService(name));
      }
      if (method === "GET" && action === "logs") {
        const { since, limit, level } = parseLogParams(params);
        return json(res, serviceLogs(name, since, limit, level));
      }
      return json(res, { error: "Unknown action", actions: ["start", "stop", "restart", "logs"] }, 404);
    }

    if (method === "POST" && pathname.match(/^\/build\/([^/]+)$/)) {
      const name = pathname.split("/")[2];
      return json(res, await requestBuild(name));
    }
    if (method === "GET" && pathname === "/build/status") return json(res, buildStatus());
    if (method === "GET" && pathname === "/build/logs") {
      const { since, limit, level } = parseLogParams(params);
      return json(res, buildLogs(since, limit, level));
    }

    if (method === "POST" && pathname.match(/^\/clean\/([^/]+)$/)) {
      const name = pathname.split("/")[2];
      const force = params.force === "1";
      const result = force ? await requestCleanUnsafe(name) : await requestClean(name);
      return json(res, result, result.action === "error" ? 400 : 200);
    }
    if (method === "GET" && pathname === "/clean/targets") {
      const targets = {};
      for (const [name, def] of Object.entries(CLEAN_DEFS)) {
        targets[name] = { packages: def.packages, safe: def.safe, warning: def.warning || null };
      }
      return json(res, targets);
    }

    if (method === "POST" && pathname === "/dev") {
      if (orchestration.active) {
        return json(res, { action: "already_orchestrating", phase: orchestration.phase }, 409);
      }
      const rebuild = params.rebuild === "1";
      orchestrateDev(rebuild);
      return json(res, { action: "orchestrating", phase: "checking" }, 202);
    }

    if (method === "POST" && pathname === "/stop-all") return json(res, await stopAll());

    if (method === "POST" && pathname === "/shutdown") {
      pushDaemonLog("Shutdown requested");
      shuttingDown = true;
      await stopAll();
      json(res, { action: "shutdown" });
      cleanup();
      setTimeout(() => process.exit(0), 200);
      return;
    }

    if (method === "GET" && pathname === "/logs") {
      const { since, limit, level } = parseLogParams(params);
      return json(res, interleavedLogs(since, limit, level));
    }

    json(res, { error: "not_found", path: pathname }, 404);
  } catch (err) {
    pushDaemonLog(`HTTP error: ${err.message}`);
    json(res, { error: err.message }, 500);
  }
});

// ── lifecycle ──────────────────────────────────────────────────────────────────
function writePidFile() {
  mkdirSync(STATE_DIR, { recursive: true });
  writeFileSync(
    PID_FILE,
    JSON.stringify({ pid: process.pid, port: PORT, startedAt: new Date().toISOString() })
  );
}

function cleanup() {
  try { unlinkSync(PID_FILE); } catch {}
}

process.on("SIGINT", () => { shuttingDown = true; cleanup(); process.exit(0); });
process.on("SIGTERM", () => { shuttingDown = true; cleanup(); process.exit(0); });
process.on("exit", cleanup);

// ── dependency watchdog ─────────────────────────────────────────────────────────
const WATCHDOG_INTERVAL = 10_000;
setInterval(() => {
  for (const [name, def] of Object.entries(SERVICE_DEFS)) {
    if (!def.requires || !isServiceRunning(name)) continue;
    for (const dep of def.requires) {
      if (!isServiceRunning(dep)) {
        const depSvc = services.get(dep);
        // Skip if the dep gave up auto-restarting after a crash loop —
        // forcing it would just crash-loop again. User must intervene.
        if (depSvc && depSvc.crashTimes.length >= CRASH_LOOP_THRESHOLD) continue;
        if (depSvc && depSvc.exitCode !== null) {
          pushServiceLog(dep, "daemon", `Watchdog: restarting crashed dependency (required by ${name})`);
          startService(dep);
        } else if (depSvc && depSvc.startedAt === null && depSvc.exitCode === null) {
          pushServiceLog(dep, "daemon", `Watchdog: starting missing dependency (required by ${name})`);
          startService(dep);
        }
      }
    }
  }
}, WATCHDOG_INTERVAL);

// ── start ──────────────────────────────────────────────────────────────────────
try {
  await loadPlugins();
} catch (err) {
  console.error(`Plugin load failed: ${err.stack || err.message}`);
  process.exit(1);
}
syncServiceStates();

server.listen(PORT, "127.0.0.1", () => {
  writePidFile();
  cleanStaleShadows();
  pushDaemonLog(`Daemon listening on 127.0.0.1:${PORT}`);
  pushDaemonLog(`Loaded ${PLUGINS.size} plugins: ${Array.from(PLUGINS.keys()).join(", ")}`);
});

server.on("error", (err) => {
  if (err.code === "EADDRINUSE") {
    process.exit(0);
  }
  console.error(`Daemon error: ${err.message}`);
  process.exit(1);
});
