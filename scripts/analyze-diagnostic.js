#!/usr/bin/env node

/**
 * Diagnostic Capture Analyzer
 *
 * Reads a `diagnostic-<unix_ms>.csv` produced by the in-app Capture button
 * and reports timing stats: input arrival rate, device tick stability,
 * input→next-tick latency, and V1 queue-backlog symptoms (ramp completion
 * lag). Intended to answer "why does output feel like it can't keep up?"
 *
 * Usage:
 *   node scripts/analyze-diagnostic.js path/to/diagnostic-XXXX.csv [--json]
 *
 * No deps. ESM. Run from anywhere.
 */

import fs from 'node:fs';
import path from 'node:path';

const args = process.argv.slice(2);
const wantJson = args.includes('--json');
const csvPath = args.find((a) => !a.startsWith('--'));

if (!csvPath) {
    console.error('Usage: node scripts/analyze-diagnostic.js <csv> [--json]');
    process.exit(1);
}
if (!fs.existsSync(csvPath)) {
    console.error(`File not found: ${csvPath}`);
    process.exit(1);
}

// ---------------------------------------------------------------------------
// CSV parse
// ---------------------------------------------------------------------------

const raw = fs.readFileSync(csvPath, 'utf8');
const lines = raw.split(/\r?\n/);

let header = null;
let metaStartWallMs = null;
let metaDurationMs = null;
const inputs = [];
const ticks = [];

for (const line of lines) {
    if (!line) continue;
    if (line.startsWith('#')) {
        const m = line.match(/start_wall_ms=(\d+).*duration_ms=(\d+)/);
        if (m) {
            metaStartWallMs = Number(m[1]);
            metaDurationMs = Number(m[2]);
        }
        continue;
    }
    if (!header) {
        header = line.split(',');
        continue;
    }
    const cells = line.split(',');
    const row = Object.fromEntries(header.map((h, i) => [h, cells[i] ?? '']));
    if (row.kind === 'input') {
        inputs.push({
            t_us: Number(row.ts_us),
            axis: row.axis,
            value: Number(row.value),
            interval_ms: row.interval_ms === '' ? null : Number(row.interval_ms),
        });
    } else if (row.kind === 'tick') {
        // raw_aX / raw_bX are pre-normalization 0-200 device-unit slot
        // values. Older CSVs (before raw capture was added) won't have
        // these columns; fall back to NaN so the analyzer still runs.
        const hasRaw = row.raw_a0 !== undefined && row.raw_a0 !== '';
        ticks.push({
            t_us: Number(row.ts_us),
            connected: row.connected === '1',
            intensity_a: Number(row.intensity_a),
            intensity_b: Number(row.intensity_b),
            wa: [row.wa0, row.wa1, row.wa2, row.wa3].map(Number),
            wb: [row.wb0, row.wb1, row.wb2, row.wb3].map(Number),
            raw_a: hasRaw ? [row.raw_a0, row.raw_a1, row.raw_a2, row.raw_a3].map(Number) : null,
            raw_b: hasRaw ? [row.raw_b0, row.raw_b1, row.raw_b2, row.raw_b3].map(Number) : null,
            freq_a: Number(row.freq_a),
            freq_b: Number(row.freq_b),
        });
    }
}

if (inputs.length === 0 && ticks.length === 0) {
    console.error('No events parsed. Is this a valid diagnostic CSV?');
    process.exit(1);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const sortByT = (a, b) => a.t_us - b.t_us;
inputs.sort(sortByT);
ticks.sort(sortByT);

function pct(arr, p) {
    if (arr.length === 0) return null;
    const sorted = [...arr].sort((a, b) => a - b);
    const idx = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
    return sorted[idx];
}

function stats(arr) {
    if (arr.length === 0) return null;
    const sum = arr.reduce((a, b) => a + b, 0);
    return {
        n: arr.length,
        min: Math.min(...arr),
        median: pct(arr, 50),
        p90: pct(arr, 90),
        p99: pct(arr, 99),
        max: Math.max(...arr),
        mean: sum / arr.length,
    };
}

function fmtMs(usOrMs, isUs = true) {
    if (usOrMs == null) return 'n/a';
    const ms = isUs ? usOrMs / 1000 : usOrMs;
    return `${ms.toFixed(2)}ms`;
}

// ---------------------------------------------------------------------------
// Top-level summary
// ---------------------------------------------------------------------------

const allTs = [...inputs.map((i) => i.t_us), ...ticks.map((t) => t.t_us)];
const captureSpanUs = Math.max(...allTs) - Math.min(...allTs);
const captureSpanS = captureSpanUs / 1e6;

const inputRate = inputs.length / captureSpanS;
const tickRate = ticks.length / captureSpanS;

// Per-axis input breakdown
const perAxis = {};
for (const ev of inputs) {
    const a = (perAxis[ev.axis] ||= { count: 0, values: [], intervals: [] });
    a.count++;
    a.values.push(ev.value);
    if (ev.interval_ms != null) a.intervals.push(ev.interval_ms);
}

// Inter-arrival deltas (ms)
function deltas(events) {
    const out = [];
    for (let i = 1; i < events.length; i++) {
        out.push((events[i].t_us - events[i - 1].t_us) / 1000);
    }
    return out;
}

const inputDeltasMs = deltas(inputs);
const tickDeltasMs = deltas(ticks);

// ---------------------------------------------------------------------------
// Input → next-tick phase lag
// ---------------------------------------------------------------------------
// For each input, the gap until the very next tick fires. This is the
// MINIMUM time before that input has any chance of being reflected. With
// a 10Hz tick this is uniform 0-100ms (mean 50ms) if everything is healthy.

const phaseLagMs = [];
let tIdx = 0;
for (const inp of inputs) {
    while (tIdx < ticks.length && ticks[tIdx].t_us <= inp.t_us) tIdx++;
    if (tIdx >= ticks.length) break;
    phaseLagMs.push((ticks[tIdx].t_us - inp.t_us) / 1000);
}

// ---------------------------------------------------------------------------
// V1 queue-backlog symptom: ramp convergence vs intent
// ---------------------------------------------------------------------------
// For each input on the busiest axis with a non-null interval_ms, measure
// how long after arrival the channel intensity *stops changing*. If
// significantly longer than the requested interval_ms, V1's ramp queue is
// backlogged.

const busiestAxis =
    Object.entries(perAxis).sort((a, b) => b[1].count - a[1].count)[0]?.[0] ?? null;

// Heuristic: assume primary axis drives intensity_a (most common preset).
// If the user has linked multiple axes, this is approximate but the
// trajectory is still informative.
const convergenceSamples = [];
if (busiestAxis) {
    const busyInputs = inputs.filter((i) => i.axis === busiestAxis && i.interval_ms != null);
    for (const inp of busyInputs) {
        // Find ticks within (inp + 0, inp + 4 * interval_ms) window.
        const windowEndUs = inp.t_us + inp.interval_ms * 1000 * 4;
        const windowTicks = ticks.filter((t) => t.t_us > inp.t_us && t.t_us <= windowEndUs);
        if (windowTicks.length < 2) continue;
        // Last tick whose intensity differs from the one after it: that's
        // when the ramp settled.
        let settledIdx = -1;
        for (let i = 0; i < windowTicks.length - 1; i++) {
            if (windowTicks[i].intensity_a !== windowTicks[i + 1].intensity_a) {
                settledIdx = i + 1;
            }
        }
        if (settledIdx < 0) continue;
        const settleMs = (windowTicks[settledIdx].t_us - inp.t_us) / 1000;
        convergenceSamples.push({
            requested_ms: inp.interval_ms,
            settled_ms: settleMs,
            ratio: settleMs / inp.interval_ms,
        });
    }
}

// ---------------------------------------------------------------------------
// Cross-correlation lag estimate (raw output ↔ input axis)
// ---------------------------------------------------------------------------
// For every (axis, output-channel) pair, find the time shift that best
// aligns the input axis with the channel's newest raw slot value. This
// directly measures "output trails input by X ms" without depending on
// channel routing knowledge.
//
// Method: build an input timeline interpolated at each tick's timestamp
// minus a candidate shift; correlate against raw_x[3]/200 (newest slot,
// normalized to 0-1). Pick the shift that maximizes Pearson correlation.

function interpAxisAt(axisInputs, t_us) {
    if (axisInputs.length === 0) return null;
    if (t_us <= axisInputs[0].t_us) return axisInputs[0].value;
    if (t_us >= axisInputs[axisInputs.length - 1].t_us)
        return axisInputs[axisInputs.length - 1].value;
    let lo = 0;
    let hi = axisInputs.length - 1;
    while (hi - lo > 1) {
        const mid = (lo + hi) >> 1;
        if (axisInputs[mid].t_us <= t_us) lo = mid;
        else hi = mid;
    }
    const a = axisInputs[lo];
    const b = axisInputs[hi];
    const f = (t_us - a.t_us) / (b.t_us - a.t_us);
    return a.value + (b.value - a.value) * f;
}

function pearson(xs, ys) {
    if (xs.length < 2) return 0;
    const n = xs.length;
    let sx = 0;
    let sy = 0;
    for (let i = 0; i < n; i++) {
        sx += xs[i];
        sy += ys[i];
    }
    const mx = sx / n;
    const my = sy / n;
    let num = 0;
    let dx = 0;
    let dy = 0;
    for (let i = 0; i < n; i++) {
        const a = xs[i] - mx;
        const b = ys[i] - my;
        num += a * b;
        dx += a * a;
        dy += b * b;
    }
    const denom = Math.sqrt(dx * dy);
    return denom === 0 ? 0 : num / denom;
}

const lagResults = [];
const hasRaw = ticks.length > 0 && ticks[0].raw_a !== null;
if (hasRaw) {
    // Pre-bucket inputs by axis and sort
    const byAxis = {};
    for (const inp of inputs) {
        (byAxis[inp.axis] ||= []).push(inp);
    }
    for (const a of Object.values(byAxis)) a.sort((x, y) => x.t_us - y.t_us);

    // Channels: A (raw_a[3]/200) and B (raw_b[3]/200)
    const channels = [
        { name: 'A', series: ticks.map((t) => t.raw_a[3] / 200) },
        { name: 'B', series: ticks.map((t) => t.raw_b[3] / 200) },
    ];

    // Try shifts -50ms .. +500ms in 10ms steps
    const shiftRange = [];
    for (let s = -50; s <= 500; s += 10) shiftRange.push(s);

    for (const ch of channels) {
        for (const [axis, axisInputs] of Object.entries(byAxis)) {
            let best = { shift_ms: null, corr: -2 };
            for (const shift of shiftRange) {
                const xs = [];
                const ys = [];
                for (let i = 0; i < ticks.length; i++) {
                    const v = interpAxisAt(axisInputs, ticks[i].t_us - shift * 1000);
                    if (v == null) continue;
                    xs.push(v);
                    ys.push(ch.series[i]);
                }
                const c = pearson(xs, ys);
                if (c > best.corr) best = { shift_ms: shift, corr: c };
            }
            lagResults.push({
                axis,
                channel: ch.name,
                lag_ms: best.shift_ms,
                correlation: Number(best.corr.toFixed(3)),
            });
        }
    }
    // Sort by descending correlation so the strongest pairs surface first
    lagResults.sort((a, b) => b.correlation - a.correlation);
}

// ---------------------------------------------------------------------------
// Raw output variance per tick (channel A only, picks up engine smoothing)
// ---------------------------------------------------------------------------
let rawVarianceA = null;
let rawVarianceB = null;
if (hasRaw) {
    const aDeltas = ticks.map((t) => Math.max(...t.raw_a) - Math.min(...t.raw_a));
    const bDeltas = ticks.map((t) => Math.max(...t.raw_b) - Math.min(...t.raw_b));
    rawVarianceA = stats(aDeltas);
    rawVarianceB = stats(bDeltas);
}

// ---------------------------------------------------------------------------
// Stale-tick detection: ticks whose intensity is still drifting after a
// long quiet window in input. Indicates queue draining old commands.
// ---------------------------------------------------------------------------

const staleDriftTicks = [];
const QUIET_WINDOW_MS = 500;
let inpIdx = 0;
for (let i = 1; i < ticks.length; i++) {
    const t = ticks[i];
    const prev = ticks[i - 1];
    if (t.intensity_a === prev.intensity_a && t.intensity_b === prev.intensity_b) continue;
    // Find newest input <= t.t_us
    while (inpIdx < inputs.length && inputs[inpIdx].t_us <= t.t_us) inpIdx++;
    const lastInp = inputs[inpIdx - 1];
    if (!lastInp) continue;
    const sinceInputMs = (t.t_us - lastInp.t_us) / 1000;
    if (sinceInputMs >= QUIET_WINDOW_MS) {
        staleDriftTicks.push({ t_us: t.t_us, since_input_ms: sinceInputMs });
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

const report = {
    file: path.basename(csvPath),
    capture: {
        wall_clock_start_ms: metaStartWallMs,
        declared_duration_ms: metaDurationMs,
        observed_span_s: Number(captureSpanS.toFixed(3)),
        input_events: inputs.length,
        tick_events: ticks.length,
        input_rate_hz: Number(inputRate.toFixed(2)),
        tick_rate_hz: Number(tickRate.toFixed(2)),
        connected_ticks: ticks.filter((t) => t.connected).length,
    },
    input_inter_arrival_ms: stats(inputDeltasMs),
    tick_inter_arrival_ms: stats(tickDeltasMs),
    phase_lag_input_to_next_tick_ms: stats(phaseLagMs),
    per_axis: Object.fromEntries(
        Object.entries(perAxis).map(([axis, a]) => [
            axis,
            {
                count: a.count,
                value_min: Math.min(...a.values),
                value_max: Math.max(...a.values),
                interval_count: a.intervals.length,
                interval_median_ms: a.intervals.length ? pct(a.intervals, 50) : null,
            },
        ]),
    ),
    busiest_axis: busiestAxis,
    ramp_convergence: convergenceSamples.length
        ? {
              n: convergenceSamples.length,
              requested_median_ms: pct(convergenceSamples.map((s) => s.requested_ms), 50),
              settled_median_ms: pct(convergenceSamples.map((s) => s.settled_ms), 50),
              ratio_median: pct(convergenceSamples.map((s) => s.ratio), 50),
              ratio_p90: pct(convergenceSamples.map((s) => s.ratio), 90),
              note: 'ratio = settled / requested. >1.5 suggests V1 queue backlog.',
          }
        : null,
    stale_drift: {
        ticks_drifting_after_quiet_input: staleDriftTicks.length,
        quiet_window_ms: QUIET_WINDOW_MS,
        note: 'High count = engine still emitting changes long after last input arrived (queue draining)',
    },
    raw_slot_variance_a: rawVarianceA,
    raw_slot_variance_b: rawVarianceB,
    cross_correlation_lag: lagResults.length ? lagResults : null,
};

if (wantJson) {
    console.log(JSON.stringify(report, null, 2));
    process.exit(0);
}

// Pretty print
const cap = report.capture;
const line = (s = '') => console.log(s);
const fmtStat = (s) =>
    s
        ? `n=${s.n} min=${s.min.toFixed(2)} med=${s.median.toFixed(2)} p90=${s.p90.toFixed(2)} p99=${s.p99.toFixed(2)} max=${s.max.toFixed(2)} mean=${s.mean.toFixed(2)}`
        : 'no data';

line(`Diagnostic: ${report.file}`);
line('─'.repeat(60));
line(`Span:        ${cap.observed_span_s}s  (declared ${cap.declared_duration_ms}ms)`);
line(`Inputs:      ${cap.input_events}  (${cap.input_rate_hz} Hz)`);
line(`Ticks:       ${cap.tick_events}  (${cap.tick_rate_hz} Hz, target 10 Hz)`);
line(`Connected:   ${cap.connected_ticks}/${cap.tick_events} ticks`);
line('');
line('Input inter-arrival (ms):');
line(`  ${fmtStat(report.input_inter_arrival_ms)}`);
line('Tick inter-arrival (ms) — should be ~100ms ± jitter:');
line(`  ${fmtStat(report.tick_inter_arrival_ms)}`);
line('Phase lag input→next-tick (ms) — uniform 0-100 if healthy:');
line(`  ${fmtStat(report.phase_lag_input_to_next_tick_ms)}`);
line('');
line('Per-axis input breakdown:');
for (const [axis, a] of Object.entries(report.per_axis)) {
    const intMed = a.interval_median_ms != null ? `${a.interval_median_ms}ms` : 'n/a';
    line(
        `  ${axis}: ${a.count} events, val ${a.value_min.toFixed(2)}–${a.value_max.toFixed(2)}, intervals ${a.interval_count} (med ${intMed})`,
    );
}
line('');
line(`Busiest axis: ${report.busiest_axis ?? 'n/a'}`);
if (report.ramp_convergence) {
    const r = report.ramp_convergence;
    line('Ramp convergence (intensity_a settled vs requested interval):');
    line(`  n=${r.n}  requested med=${r.requested_median_ms}ms  settled med=${r.settled_median_ms}ms`);
    line(`  ratio: med=${r.ratio_median.toFixed(2)} p90=${r.ratio_p90.toFixed(2)}`);
    line(`  ${r.note}`);
} else {
    line('Ramp convergence: insufficient samples on busiest axis with intervals');
}
line('');
line('Stale drift:');
line(
    `  ${report.stale_drift.ticks_drifting_after_quiet_input} ticks changed intensity after >${report.stale_drift.quiet_window_ms}ms of input quiet`,
);
line(`  ${report.stale_drift.note}`);
line('');
if (report.raw_slot_variance_a) {
    line('Raw per-tick slot variance (max-min within packet, 0-200 scale):');
    line(`  Channel A: ${fmtStat(report.raw_slot_variance_a)}`);
    line(`  Channel B: ${fmtStat(report.raw_slot_variance_b)}`);
    line('  Higher = engine producing more variation per packet (less smoothing).');
}
if (report.cross_correlation_lag) {
    line('');
    line('Cross-correlation lag (raw output trails which input by how much):');
    line('  axis → channel: best_lag_ms (correlation)');
    for (const r of report.cross_correlation_lag.slice(0, 6)) {
        const corrStr = r.correlation.toFixed(3);
        line(`  ${r.axis} → ${r.channel}: ${r.lag_ms}ms  (r=${corrStr})`);
    }
    line('  Strong correlation (|r|>0.7) at non-zero lag = real output delay.');
}
line('');
line('Quick read:');
const phase = report.phase_lag_input_to_next_tick_ms;
if (phase && phase.median > 60) {
    line(`  ! phase lag median ${phase.median.toFixed(1)}ms is high — input arriving just after ticks`);
}
if (report.ramp_convergence && report.ramp_convergence.ratio_median > 1.5) {
    line(`  ! ramp convergence ratio ${report.ramp_convergence.ratio_median.toFixed(2)} — queue backlog likely`);
}
if (report.stale_drift.ticks_drifting_after_quiet_input > 5) {
    line(`  ! ${report.stale_drift.ticks_drifting_after_quiet_input} stale-drift ticks — engine emitting after input stopped`);
}
const tick = report.tick_inter_arrival_ms;
if (tick && tick.p90 > 130) {
    line(`  ! tick p90 ${tick.p90.toFixed(1)}ms — device loop jittery`);
}
line('');
