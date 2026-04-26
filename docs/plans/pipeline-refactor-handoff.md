# Pipeline refactor — handoff for next session

This doc gets a fresh Claude session up to speed on the in-flight pipeline refactor without re-walking the whole conversation history. **Read `docs/plans/pipeline-refactor.md` first** — it's the source of truth for goals, deletion manifest, settings policy, and substep details. This handoff is the operational layer on top of it: where we are, how we got here, and what to do next.

---

## What this is

A multi-week refactor of the Coyote-Socket backend pipeline from a tangled "T-Code first, everything else bolted on" model into a four-layer architecture: **InputSource → InputBus → Resolver → Engine**. The plan was reviewed by GPT and Gemini before any code landed; their feedback is folded into the plan doc.

Eight steps total. Steps 1–4.5 are shipped. Steps 5+6+7 are bundled into one logical PR being shipped as seven sub-commits (A–G); only sub A is in. Step 8 is pending.

---

## Where you're working

- **Repo:** `C:\Dev\Repos\hobby\electrado\coyote-socket`
- **Worktree:** `C:\Dev\Repos\hobby\electrado\coyote-socket\.claude\worktrees\pipeline-refactor`
- **Branch:** `worktree-pipeline-refactor`
- **Current HEAD:** `297f10c` (will drift as you commit)

Use the `EnterWorktree` deferred tool to switch the session into the worktree if you aren't already there. The session normally enters via `EnterWorktree({ path: "C:/Dev/Repos/hobby/electrado/coyote-socket/.claude/worktrees/pipeline-refactor" })` or by name if a fresh worktree is needed (don't create a fresh one — work continues in the existing one).

The outer repo's `main` may have unrelated commits landing in parallel. Don't merge or rebase onto main from inside the worktree without checking with the maintainer first.

---

## Shipped so far

Each step shipped with three reviewer passes (correctness / DRY / design gaps) by `general-purpose` Agents and follow-up commits addressing in-scope findings. The plan's "Status" section has the up-to-date table; what follows is the human-readable narrative.

| Commit | What |
|---|---|
| `d170c96` | `wip: pre-refactor in-flight work (axis history, channelid, etc.)` — the uncommitted scaffolding from main's working tree (AxisState VecDeque history, ChannelId enum, ParameterSource.delay_ms field). Carried over so the refactor branch starts from a stable baseline. |
| `a03419f` | `docs: add pipeline refactor plan` — the 654-line plan that drives everything. |
| `9b5a6e8` | Step 1: `resolve_parameter` collapsed into thin wrapper over `resolve_parameter_at_time`; `delay_ms` honored. |
| `663249a` | Step 1 follow-up: edge-case tests (history clamp, staleness anchor, `saturating_sub` underflow). |
| `9eebd24` | Step 2: `convert_parameter_source` / `convert_channel_settings` lifted into new `settings_convert.rs`. |
| `7c22882` | Step 2 follow-up: `pub(crate)` visibility tightening + 7 conversion tests. |
| `f4fcb02` | Step 3: four sync paths (`save_channel_settings`, `update_channel_config`, `update_parameter_source`, `update_buttplug_links`) collapsed into single `apply_channel_config { channel, channel_settings, persist }` Tauri command + helper. ~135-line `ButtplugLinkConfig` builder deleted. Frontend migrated. |
| `de5819e` | Step 3 follow-up: `derive_hmr_channel_params` tests + race-contract documentation on `apply_channel_config`. |
| `09173ca` | Step 4: `websocket.rs` (703 lines) split into `net.rs`, `tcode_input.rs`, `resolver.rs`. `apply_saved_settings_to_processing` moved into `main.rs`. |
| `86f1885` | Step 4 follow-up: **fixed pre-existing buttplug-link-not-restored bug** by routing startup load through `apply_channel_config_to_state`. `set_output_options` moved out of `net.rs` into `main.rs`. `pub(crate)` consistency. |
| `a30d115` | Step 4.5: V1 engine dropped. Custom `Deserialize` impl falls back to default (`V2Balanced`) on legacy `"v1"` strings — serde-only, no migration module. |
| `6379c22` | Step 4.5 follow-up: **fixed V3 UI fidelity regression** — `current_intensity_normalized` reads V3's `current_position` for V3 (not V2's ramp). `as_str()` symmetry on `ProcessingEngineType`. |
| `df8a715` | Bundled sub A: `InputBus` introduced. `ProcessingState.axis_values` renamed to `input_bus`. Resolver signatures take `&InputBus`. |
| `80ec841` | Bundled sub A follow-up: `latest_timestamp(axis)` shortcut + plan-doc status table. |
| `1b12a36` | Bundled sub B: 3 buttplug HashMaps lifted into `input_bus` under the `bp:` namespace. Pipeline + state timestamps converted from `Instant` to `u64 ms`. Per-channel `Channel.last_buttplug_replay_ts: u64` watermark replaces the post-tick `linear_commands.clear()`. Five new tests pin namespace round-trip, control-axis filter, scoped clear, watermark one-shot, and feature-value projection. |
| `3b19e3a` | Bundled sub B follow-up: PositionWithDuration in clear test fixture; `set_buttplug_feature` flagged with sub E TODO for per-batch timestamp coherence; out-of-scope timebase / namespace-parsing / rotate-pairing notes captured in commit body. |
| `7539bc7` | Bundled sub C: `ParameterSource` → `ParameterLinkConfig` rename + `ParameterLinkRuntime` / `ChannelLinkRuntime` runtime split. `Channel.link_runtime` field added. Vestigial `ParameterSource.buttplug_links` field dropped (consumer reads `ChannelSettings.intensity_source.buttplug_links` directly). `TransformState::None` placeholder until sub D. |
| `db17255` | Bundled sub C follow-up: `TODO(sub F)` markers on `Channel.buttplug_link` + `buttplug_state` so future engineers don't mirror the soon-deprecated fields. |
| `695cd9a` | Bundled sub D: `transforms/` module. `TransformConfig` + `TransformState` enums with generic primitives (Smooth, Scale, Clamp, Invert, Hold, Mix) and Buttplug semantic wrappers (Vibrate, Oscillate, Constrict). `apply_transform` dispatch with pre-fetched modifier slice — transforms cannot read the bus directly. `ParameterLinkConfig.transforms: Vec<TransformConfig>` field with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`. 20 new tests; module gated `#[allow(dead_code)]` for the staging window. |
| `f37aa41` | Bundled sub D follow-up: deduped Vibrate/Oscillate phase-state guards into `step_phase_state` helper; dropped the third `lerp` copy (now imports `crate::modulation::lerp`); renamed `oscillate_passes_through_on_first_call` test to reflect that the assertion pins the trough, not pass-through; captured Constrict centering shift in handoff sub G migration note. |
| `297f10c` | Plan doc: bundled-phase substep table marked subs A-D shipped with commit refs + sub E carry-forward notes. |
| `b575447` | Bundled sub E: unified resolver via transforms — drop Buttplug pipeline short-circuit. `TransformConfig::Rotate` variant + `apply_rotate` (paired speed/direction modifier axes). `ResolvedSample` struct in `modulation.rs` (sub G's wire format shape). `resolve_link` / `resolve_link_at_time` thread `&mut ParameterLinkRuntime` through midpoint → curve → transforms → range. `settings_convert::convert_parameter_source` translates `ButtplugLinksSettings` → ordered `Vec<TransformConfig>` (Position is source axis, then Motion/Vibrate/Constrict). `apply_channel_config_to_state` resets `Channel.link_runtime` via `ChannelLinkRuntime::for_config`. Per-batch `arrival_ts_ms` argument on `set_buttplug_feature` / `set_buttplug_linear_cmd` / `set_buttplug_rotate_direction` — handlers compute once per logical command. `Channel.buttplug_link` consumer gone, field marked `#[deprecated]` (write-only path until sub F). `get_resolved_channel_params` + `get_per_slot_frequencies` switched to write locks for stateful transform threading. **Gate intentionally narrow**: only `bp:`-prefixed source axes route through the resolver — T-Code / gamepad intensity stays on V2/V3 engine path, transforms attached to non-`bp:` links are silently ignored until sub G. |
| `09ebfea` | Bundled sub E follow-up: tightened resolver gate to `bp:` only (a T-Code intensity preset with attached transforms had silently bypassed engines). Extracted `ProcessingState::split_bus_and_channels` helper, deduping the deref + disjoint-borrow dance at three call sites. `pub type InputBusSnapshot<'a> = &'a InputBus` alias (sub G's frozen-frame is a typedef edit, not per-callsite churn). `#[deprecated]` markers on `Channel.buttplug_link` / `buttplug_state` / `set_buttplug_link_config` so any future maintainer re-introducing a reader trips a compiler warning. |
| `47f1039` | Bundled sub F: deleted `buttplug/pipeline.rs` (311 lines + 5 tests) and `buttplug/state.rs` (210 lines, `ButtplugChannelState` / `ButtplugFeatureValues` / `PositionDurationState`). Trimmed `buttplug/types.rs` to descriptor-only (`ButtplugFeatureType`, `ButtplugFeatureConfig`). Moved canonical `ConstrictionMethod` into `transforms/mod.rs`. Dropped `Channel.buttplug_link` + `Channel.buttplug_state` fields, `set_buttplug_link_config` / `get_buttplug_feature_values` / `has_buttplug_input` methods, `BUTTPLUG_MAX_FEATURES` const. Dropped `ButtplugLinksSettings::to_link_config`. Removed `bp_config` write in `apply_channel_config_to_state`. The schema-side `buttplug_links` field on `ParameterSourceSettings` survives intentionally — sub G's frontend transforms editor lands first. 983 lines deleted, 139 added. |
| `7fddc0f` | Bundled sub F follow-up: deleted `ButtplugFeatureType` enum (zero live readers; handler hardcodes wire strings; frontend has its own TS copy) and `_touch_buttplug_links` test fixture (residue from a prior compile state). Refreshed plan-doc deletion-manifest rows for `update_parameter_source` / `update_buttplug_links` Tauri commands — both shipped in step 3, plan doc had them stale at "Step 5+6+7". |
| `bcdb836` | Bundled sub G.0 (backend half): `resolved-update` Tauri event emitted at the 10Hz device tick alongside `waveform-sample`. Wire types `ResolvedSampleSnapshot` / `ChannelResolvedSnapshot` / `ResolvedUpdatePayload` defined in `resolver.rs`. `Channel.last_intensity_sample: Option<ResolvedSample>` stash captures the bp:-path resolver output inside `get_next_waveform_data` — the telemetry pass `.take()`s it rather than re-resolving (re-resolve would double-advance Vibrate / Oscillate phase). `get_resolved_channel_params` returns both device-output params + per-channel snapshots in one resolver pass. Engine-path Linked intensity synthesizes from V2 ramp / V3 `current_position` so the UI sees the device's actual transmitted value (faithful "what device receives" reading); Static synthesizes from the static byte. 5 new tests; 109 pass. |
| `b4e70a5` | Bundled sub G.0 follow-up: fixed bp:→Static transition stash leak (Static early-return now clears `last_intensity_sample`). Extracted `build_channel_snapshot` free function so the production resolver pass + the in-crate test helper share one body (~80 line dedup). `from_sample` consumes `ResolvedSample` by value (drops 3 String clones per channel per tick). `Channel.last_intensity_sample` tightened from `pub` to `pub(crate)`. `INTENSITY_DEVICE_MAX: f64 = 200.0` named constant replacing the magic divisor. Misnamed `..._with_camel_case_fields` test renamed to `..._with_snake_case_fields` (matches the actual assertion). 110 pass. |

The two pre-existing test failures (`buttplug::pipeline::tests::test_pipeline_oscillate`, `processing::tests::test_parse_tcode_with_interval`) are not new — they live on `main` as well and are out of scope for this refactor. Sub F deleted the pipeline.rs file, so its failure is now gone with the file. Total pass count: 67 (pre-refactor) → 72 (sub B) → 73 (sub C) → 93 (sub D) → 109 (sub E) → 103 (sub F, deleted tests for removed methods) → 109 (sub G.0) → 110 (sub G.0 follow-up).

---

## Cadence

This is the rhythm the maintainer set, and it's working. Don't change it without a reason.

1. **One sub-commit per substep.** Each substep in the plan's "Bundled phase substeps" table is its own commit. Don't bundle two together.
2. **`cargo check` + `cargo test` after every sub.** Pre-existing failures are fine; don't introduce new ones.
3. **Three reviewer passes per substantive commit** — spawn three `general-purpose` Agents in parallel with focused prompts (correctness / DRY / design gaps). Templates below.
4. **Follow-up commit for in-scope reviewer findings.** Out-of-scope or "deferred to later phase" findings stay flagged in the commit message but don't block.
5. **Speak via `mcp__hub-channel__speak` and update status via `mcp__hub-channel__update_status` every response** — the maintainer runs multiple agents and reads the dashboard.
6. **Use `pub(crate)` for new modules** to match Step 2's convention. Bare `mod` works but breaks the established style.

### Reviewer prompt templates

```
Review commit <SHA> on branch worktree-pipeline-refactor in
`.claude/worktrees/pipeline-refactor`. Sub <X> of bundled Steps 5+6+7
in `docs/plans/pipeline-refactor.md`. <one-sentence summary of what
the commit does>.

**Your focus: <correctness | DRYness | design gaps>.**

1. `git show <SHA> --stat` then read the diff.
2. <Specific things to check, ideally 4-6 bullets with named files/lines>
3. <Known concerns to look for — surface drag from the plan>

Distinguish "<sub X> should have caught" from "deferred to later sub".

Output: "Sub X <focus>: [verdict]" + bullets. Under 350 words. No code changes.
```

Spawn three with that template, varying focus. Don't pile reviewers on a pure-rename or pure-move commit — those don't need three angles.

---

## Pitfalls hit in the previous session

These cost time. Don't repeat them.

1. **`cd ..` from `src-tauri/` may not put you in the worktree.** Background tasks (e.g. agent completions) can reset the shell cwd to the worktree root mid-bash chain. Always use absolute paths in commit commands or check `pwd` first. When `git add -A` ran from a stale outer-repo cwd, it created a misdirected commit on the outer `main` that included the worktree as a `gitlink`. If this happens: `git -C C:/Dev/Repos/hobby/electrado/coyote-socket reset --hard HEAD~1` to undo, then re-stage + commit inside the worktree.
2. **`cargo check` / `cargo test` need a `dist/` directory at the repo root** because Tauri's `generate_context!()` macro panics if it can't find the frontend bundle. The session creates an empty `dist/index.html` once: `mkdir -p dist && touch dist/index.html`. It's gitignored.
3. **Two pre-existing test failures.** `buttplug::pipeline::tests::test_pipeline_oscillate` and `processing::tests::test_parse_tcode_with_interval` fail on this branch and on main. Don't fix them as part of this refactor — they're out of scope. Verify the failure count stays at 2.
4. **`replace_all: true` on edits with trailing whitespace can break syntax.** I once asked `replace_all` to swap `pub fn ` (trailing space) for `pub(crate) fn` (no trailing space) and ended up with `pub(crate) fnconvert_*`. Always include enough context in `old_string` and `new_string` to preserve token boundaries.
5. **The frontend has many places that default to `'v1'`** as engine string. Step 4.5 caught the obvious ones (App.svelte `?? 'v1'` etc.). If you find more during sub-D's frontend-stream work, also map them to `'v2-balanced'`.
6. **`processing.rs::Channel.v3.current_position` is `pub`** as of Step 4.5 follow-up. The bundled phase will likely make more V3 internals public so the resolver can read them — that's fine; document each one.

---

## Sub E reviewer carry-forward (for sub F + sub G)

Sub E shipped at `b575447` with three reviewer passes (correctness / DRY / design gaps). Out-of-scope findings captured for the right substep:

- **T-Code intensity ↔ resolver split (sub G).** Sub E's gate is `bp:`-prefix only. Any `transforms` attached to a T-Code-driven intensity link is silently dropped because the engine path runs instead of the resolver. Sub G's `resolved-update` event needs to either (a) synthesize a `ResolvedSample` from V2 ramp / V3 lookahead state for engine-path channels so frontend telemetry isn't blank, or (b) close the gate fully (run resolver after the engine path, or thread engine output through a tail of the resolver pipeline). The settings UI already exposes the transforms editor for any link, so option (a) without option (b) leaves a "transforms attached but inert" UX hazard.

- **Constrict centering migration (release notes).** Sub D's centering shift (Constrict centers on post-prior-transforms `value`, not `state.base_position`) now affects real Buttplug-driven sessions as of sub E. A saved Vibrate+Constrict preset will narrow around the wobbled value rather than the un-wobbled base. Draft release-notes line: *"Buttplug presets that combine Vibrate and Constrict will now constrict around the wobbled position, not the un-wobbled base. The change is intentional and matches the layered transform model."* Beta feel-test against a known fixture before tagging the release that includes the bundled phase.

- **Rotate variant asymmetry (future-work note).** `TransformConfig::Rotate` carries two declared axes (`speed_axis`, `direction_axis`); other Buttplug variants carry one. `step_phase_state` has been widened twice (sub D for Vibrate/Oscillate, sub E for Rotate). A third semantic variant with `N >= 3` modifiers would widen it again. If the variant set keeps growing, migrate to `modifiers: Vec<AxisRef>` rather than named slots. Not in scope for sub F or sub G; capture only.

- **`ArrivalTs(u64)` / `TargetTs(u64)` newtypes (sub G or step 8).** Sub E uses `u64` for both arrival timestamps (`set_buttplug_feature` / `*_linear_cmd` / `*_rotate_direction`) and target timestamps (`resolve_link_at_time`). Two semantically distinct timestamps share a primitive type. A future caller could pass `target_time_ms` to a `set_buttplug_*` write site (or vice versa) without a compile error. Wrap in newtypes when the resolver-engine unification work in step 8 cleans up the timestamp story.

- **`ResolvedSample` additional fields (sub G).** Plan doc (line 572-585) and sub E ship the same 5-field shape. Sub G's UI cards may want `delay_applied_ms: u32` (= `current_time_ms - target_time_ms`) and `transforms_applied: u8` for the debug overlay. Both are pure additive — sub G can land them without breaking sub E's struct shape.

Follow-up commit `<TBD>` after sub E addressed the in-scope reviewer findings: tightened the routing gate to `bp:`-only (correctness), extracted `ProcessingState::split_bus_and_channels` helper (DRY), added `pub type InputBusSnapshot<'a>` typedef (design), and `#[deprecated]` markers on `Channel.buttplug_link` / `buttplug_state` / `set_buttplug_link_config` (design — prevents accidental sub-F-blocking re-use).

---

## Next concrete action: sub G.1 (frontend resolved-state stream)

Subs A-G.0 shipped the backend half of the bundled phase. Sub G.1 is the
frontend half: consume the `resolved-update` event the backend now emits at
10Hz, replace `inputPosition.ts`, render post-curve position dots on every
linked-parameter card, and ship the transforms editor so users can attach
`Vibrate` / `Oscillate` / `Rotate` / `Constrict` (or generic `Smooth` /
`Scale` / etc.) to any link. Plus the wire-format renames: drop
`axis-update` (replaced by `bus-update`), drop `buttplug-features`
(superseded by `bus-update` filtered to `bp:*`).

### Wire format already shipped (sub G.0)

The backend half is done. Sub G.1 only needs to consume what's already
on the wire. Snake_case JSON keys (matches the existing `WaveformSample`
shape).

```rust
// resolver.rs (sub G.0)
pub struct ResolvedUpdatePayload {
    pub timestamp_ms: u64,
    pub channel_a: ChannelResolvedSnapshot,
    pub channel_b: ChannelResolvedSnapshot,
}

pub struct ChannelResolvedSnapshot {
    pub frequency: ResolvedSampleSnapshot,
    pub frequency_balance: ResolvedSampleSnapshot,
    pub intensity_balance: ResolvedSampleSnapshot,
    pub intensity: ResolvedSampleSnapshot,
}

pub struct ResolvedSampleSnapshot {
    pub raw_input: f64,             // pre-curve, pre-transforms
    pub normalized_pre_range: f64,  // 0..1, post-curve+transforms
    pub device_value: f64,          // post-range, in device units
    pub target_time_ms: u64,        // now - delay_ms
    pub source_axis: Option<String>,// None for Static (key omitted)
    pub is_static: bool,
}
```

### Files to add / change

Frontend:

- **`src/lib/stores/resolvedState.ts`** (new) — subscribe to
  `resolved-update`. Keyed by `(channel, parameter)` → latest
  `ResolvedSampleSnapshot`. Cadence: 10Hz arrivals; consumers can
  RAF-interpolate. Shape mirrors backend: pull `is_static` straight
  through, render the position line only when `!is_static`.
- **`src/lib/stores/inputBus.ts`** (new, can defer to sub G.2) — flat
  axis-keyed store of raw bus values. Currently no backend
  `bus-update` event exists; sub G.2 introduces it. For G.1 the
  store can stay empty / fed by the existing `axis-update` payload's
  `axes` map until G.2 swaps the source.
- **`src/lib/stores/inputPosition.ts`** (deleted) — replaced by
  `resolvedState.ts` + `inputBus.ts`. Plan-doc done-criteria
  requires zero hits.
- **`src/lib/types/modulation.ts`** — add `Transform` union mirroring
  `TransformConfig` discriminator. Kebab-case `type` field per sub D
  (`{type: "vibrate", speedAxis, distance}`, etc.). Variants: `smooth`,
  `scale`, `clamp`, `invert`, `hold`, `mix`, `vibrate`, `oscillate`,
  `rotate`, `constrict`. Field names mirror the Rust struct
  (`speedAxis`, `directionAxis`, `maxSpeedHz`, etc. — sub D shipped
  `#[serde(rename = "...")]` for camelCase on the wire).
- **`src/lib/components/curve plot component`** (locate via grep for
  the existing curve renderer in `ChannelControl.svelte` /
  similar) — render two new dots: input-at-target-time + resolved
  position. Existing curve plot already shows the curve shape; sub G.1
  overlays the dots.
- **`src/lib/components/`** transforms editor (new) — list / add /
  reorder / delete `TransformConfig` entries on a `ParameterLinkConfig`.
  One sub-component per variant. Posts the updated channel config
  through the existing `apply_channel_config` Tauri command.
- **`src/lib/components/InputMonitor.svelte`** — drop the
  `axis-update` and `buttplug-features` listeners; subscribe to
  `inputBus` store instead. (Defer to sub G.2 if `bus-update` event
  isn't shipped yet.)

Backend (sub G.2):

- **`emit_bus_update` + `BusUpdatePayload`** in `main.rs`. Fired from
  `input_bus::update` per-write (T-Code, gamepad, Buttplug, Lovense
  all funnel through this). Source-tagged by axis name prefix
  (`L0` / `R2` for T-Code, `GP_*` for gamepad, `bp:*` for
  Buttplug/Lovense).
- Drop `emit_axis_update` and `emit_buttplug_features` once all
  frontend listeners have migrated.

### Acceptance for sub G

- `cargo check` + `cargo test` clean (110+ pass, 1 pre-existing
  failure unchanged).
- `npm run check` (svelte-check) clean.
- The dev-server skill confirms a build that boots without runtime
  errors. Manual smoke: a linked-intensity preset shows a moving
  position dot on the curve plot when input arrives.
- `git grep` deletion-manifest checklist (plan doc lines 484-510):
  `axis-update`, `buttplug-features`, `inputPosition` — zero hits
  outside `docs/plans/*` and `git log`. (Sub G.1 may leave
  `axis-update` for sub G.2 if `bus-update` isn't shipped yet —
  document the choice in the commit body.)
- Beta-branch validation on a real device with both a T-Code preset
  and a Buttplug preset — UI cards show the post-curve dot moving
  in sync with the input.

### Sub G — likely commit shape

Multiple commits inside the substep:

- **G.1** — Frontend stores + UI integration (no transforms editor,
  no `bus-update` rename). Just consume `resolved-update` and render
  the position dots. Smallest user-visible win.
- **G.2** — Transforms editor UI. Adds the per-variant editors and
  the parameter-card affordance to attach / reorder / delete.
- **G.3** — `axis-update` → `bus-update` rename + drop
  `buttplug-features`. Touch the listeners after the editor lands so
  the rename PR doesn't have to also handle UI churn.

Pull from this list incrementally; each commit can ship + get its own
reviewer pass. A single G.1+G.2+G.3 mega-commit is allowed but harder
to review.

### Carry-forward findings from sub G.0 reviewers

- **`is_static` flag departure from plan-doc spec** (sub G.0 design
  reviewer flagged): the explicit boolean co-varies with
  `source_axis: None` today. Frontend can infer Static from
  `source_axis === undefined` (the JSON omits the key for None). If
  sub G.1 chooses to read `is_static` directly, document the choice;
  if it infers, drop the field from the wire format in a follow-up.
- **`debug_assert!` for stash invariant** (sub G.0 design reviewer
  flagged): a future write-lock-split refactor could break the bp:
  → stash → telemetry ordering. Adding a `debug_assert!` that bp:-
  routed Linked links produce a Some stash before the snapshot pass
  catches regressions loudly. Cheap; defer to sub G or later.
- **Beta release-notes draft for Constrict centering** (sub E
  reviewer carry-forward): "Buttplug presets that combine Vibrate
  and Constrict will now constrict around the wobbled position, not
  the un-wobbled base. The change is intentional and matches the
  layered transform model." Land in the release-tag commit
  (`release.js` reads notes at tag time).

### Step 8 — sketch (unchanged from prior sessions)

Wrap existing input handlers in an `InputSource` trait. Mostly
cosmetic by then. New `src-tauri/src/input/` module with `input/mod.rs`
declaring the trait, `input/tcode.rs` (wraps `tcode_input.rs`),
`input/gamepad.rs`, `input/buttplug.rs` (with `lovense` as adapter
inside). The win: adding a future input source (MIDI, OSC, audio
amplitude) is one self-contained file.

---

## Done criteria for the whole refactor

When all subs A–G + Step 8 are in:

- `git grep` returns zero hits for: `buttplug_features`, `buttplug_linear_commands`, `buttplug_rotate_directions`, `process_buttplug_pipeline`, `ButtplugChannelState`, `ButtplugFeatureValues`, `ButtplugLinkConfig`, `ButtplugLinksSettings`, `apply_tcode`, `convert_parameter_source`, `convert_channel_settings`, `get_resolved_channel_params`, `get_per_slot_frequencies`, `apply_saved_settings_to_processing`, `sync_settings_to_state`, `ResolvedChannelParams`, `scale_intensity`, `axis-update`, `buttplug-features`, `inputPosition`. (Hits inside `docs/plans/pipeline-refactor.md` and `docs/plans/pipeline-refactor-handoff.md` are fine — they're the historical record.)
- `src-tauri/src/buttplug/pipeline.rs`, `buttplug/state.rs` no longer exist.
- `src-tauri/src/websocket.rs` no longer exists (already true after Step 4).
- `src/lib/stores/inputPosition.ts` no longer exists.
- Frontend renders a post-curve position line on every linked-parameter card (sub G's UX win).
- `cargo test` passes (modulo the 2 pre-existing failures unless they get fixed separately).
- A Buttplug-driven preset still feels equivalent to today on a beta validation pass. Curve / range knobs visibly affect Buttplug intensity.

---

## Communication style the maintainer uses

The maintainer runs a multi-agent dashboard with TTS. Keep voice updates short (1–3 sentences), set status `goal` once and `task` per substep, use emojis. Caveman mode is active — drop articles / fluff / hedging in conversation. Code, commits, and security writeups stay normal English.

The maintainer values: thoughtful migration over speed; deletion over deprecation; tests pinning behavior before refactors; reviewer feedback distinguishing "should have caught" from "deferred to later sub".

The maintainer doesn't like: parallel architectures, modal dialogs for migration, dead code left "for compat", silent fallbacks without observability.
