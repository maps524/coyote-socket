# Pipeline refactor — handoff (refactor complete)

This doc gets a fresh Claude session up to speed on the pipeline refactor — what shipped, what was deferred, and where to look for follow-ups. **Read `docs/plans/pipeline-refactor.md` first** — it's the source of truth for goals, deletion manifest, and substep details.

---

## What this is

A multi-week refactor of the Coyote-Socket backend pipeline from a tangled "T-Code first, everything else bolted on" model into a four-layer architecture: **InputSource → InputBus → Resolver → Engine**. The plan was reviewed by GPT and Gemini before any code landed; their feedback is folded into the plan doc.

**Status:** Steps 1–4.5 shipped. Steps 5+6+7 (bundled, subs A–G) shipped. Step 8 (`InputSource` trait) deferred per CLAUDE.md ("don't introduce abstractions beyond what the task requires") — `ProcessingState::bus_write` is already the unifying boundary every input source funnels through, so the trait would be decorative until a fifth source actually needs the polymorphism.

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

## What sub G shipped (the bundled-phase finale)

| Commit | Sub | What |
|---|---|---|
| `bcdb836` + `b4e70a5` | G.0 | Backend `resolved-update` event at 10Hz device tick. `ResolvedSampleSnapshot` + per-channel `ChannelResolvedSnapshot`. `Channel.last_intensity_sample` stash threads bp:-routed resolver output to telemetry without re-running phase state. Engine-path Linked synthesizes from V2 ramp / V3 lookahead. |
| `645f0d3` + `9073ffd` | G.1 | Frontend `resolvedState.ts` store consumes `resolved-update` (RAF-smoothed). Per-parameter position indicators read `normalized_pre_range` straight from the resolver. Drops `inputPosition.ts`, frontend curve eval (`applySourceTransform` etc.), `is_static` wire field, and inert example file. Follow-up adds `indicatorOf` helper + RAF-after-stop race guard + `debug_assert!` on bp:→engine-path-Linked stash invariant. |
| `69f92e3` + `f4f8cb9` | G.2 | `TransformsEditor.svelte` (10 variants, list/add/reorder/delete). `Transform` discriminated TS union mirrors `TransformConfig`. Backend gains `ParameterSourceSettings.transforms`. Convert layer prefers editor-supplied transforms, falls back to legacy `buttplug_links_to_transforms` for old saves (with `log_warn!`). Follow-up adds inert-transforms hint banner + empty-modifier-axis red border. |
| `8b756c7` | G.3.0 | `bus-update` per-write event in place of batched `axis-update` / `buttplug-features`. New `inputBus.ts` store. `ProcessingState::bus_write` helper threads every per-axis write through one path. InputMonitor migrates from two listeners to one reactive `$inputBus` block. Drops `emit_axis_update`, `emit_buttplug_features`, `get_buttplug_features` projection. |
| `48600c6` | G.3.1 | Drops legacy Buttplug surface: `ButtplugLinkPanel.svelte` (475 lines), `buttplug_links` schema field, `ButtplugLinksSettings` + `ButtplugFeatureLinkSettings` + `ButtplugFeatureConfigSettings`, settings convert layer's `buttplug_links → Vec<TransformConfig>` translation, `inputMode` ecosystem split, `effectiveInputMode`, `settingsToButtplugLinks` + `buttplugLinksToSettings` helpers. The unified `transforms` vector replaces all of it. |
| `cc7d3e4` | G.3.2 + G.3.3 | TransformsEditor's six modifier-axis text inputs gain a shared `<datalist>` populated from `inputBus.knownAxes` (autocomplete from live bus axes). Dead `RangeSlider.svelte` (258 lines) deleted. |

`cargo test`: 105 pass, 1 pre-existing failure unchanged.
`npm run check`: 25 pre-existing errors / 17 pre-existing warnings (improved from 26/17 — G.3.1 fixed one stale-reference). All in unrelated files; refactor introduced zero new diagnostics.

---

## Step 8 — explicitly deferred

The plan-doc Step 8 was: wrap input handlers in an `InputSource` trait, move them into `src-tauri/src/input/`. Per CLAUDE.md ("Don't add features, refactor, or introduce abstractions beyond what the task requires") and the maintainer's stated values ("Don't design for hypothetical future requirements"):

- The four current sources (T-Code, gamepad, buttplug, lovense) have divergent lifecycles: WebSocket-driven message handlers, polling loops, and TCP servers don't share a useful interface beyond "writes to the bus".
- `ProcessingState::bus_write` already is the unifying boundary every source funnels through. The bus + resolver + engine layers below are source-agnostic; a fifth source would only need to call `state_guard.process_command(...)` (T-Code-shaped) or `state_guard.set_buttplug_feature(...)` (bp:-namespaced).
- A trait at this point would be marker-only and decorative; moving files would be ~50+ import-statement churn for no runtime benefit.

Revisit Step 8 when a fifth source actually lands and the polymorphism would pay for itself.

---

## Carry-forward into post-refactor work

These items came up during reviewer passes but are out of scope for the refactor itself:

- **Inert-transforms UX hazard**: the sub E gate routes only `bp:`-prefixed Linked links through the resolver's transforms pipeline. Transforms attached to T-Code / gamepad / Static links are saved-but-inert. G.2 follow-up surfaces a warning banner in the editor; closing the gap (running engine-path Linked through a resolver tail, or unifying engine + resolver) is a separate plan.
- **Constrict centering shift release-notes**: "Buttplug presets that combine Vibrate and Constrict will now constrict around the wobbled position, not the un-wobbled base. The change is intentional and matches the layered transform model." Land in the release-tag commit (`release.js` reads notes at tag time).
- **`dispatch('sourceChange', { ...source, ... })` repetition**: 11+ identical calls in `RangeSliderWithIndicator.svelte` were flagged by the G.2 DRY reviewer as pre-existing. A `preserve()` helper or a `$:`-derived `currentSource` reactive value would DRY this; deferred to a focused refactor commit.
- **`ArrivalTs(u64)` / `TargetTs(u64)` newtypes**: sub E reviewer flagged that `set_buttplug_feature` and `resolve_link_at_time` both take bare `u64` for semantically distinct timestamps. Wrap in newtypes when a future caller misroutes them.
- **`Rotate` variant asymmetry**: every other Buttplug semantic transform carries one declared axis; `Rotate` carries two. If a future variant declares three+ modifier axes, migrate `step_phase_state` to `modifiers: Vec<AxisRef>` rather than widening the helper again.

---

## Done criteria for the refactor (validated)

`git grep` for the deletion-manifest names (plan doc lines 484–510):

- ✅ Zero live consumers of the dropped names (`buttplug_features`, `buttplug_linear_commands`, `buttplug_rotate_directions`, `process_buttplug_pipeline`, `ButtplugChannelState`, `ButtplugFeatureValues`, `ButtplugLinkConfig`, `ButtplugLinksSettings`, `V1ChannelState`, `axis-update`, `buttplug-features`, `inputPosition`). Remaining hits are docstring / commit-message / inline-comment historical references explaining why current code looks how it does — accepted per the plan-doc note ("Hits inside `docs/plans/...` and in `git log` are fine — they're the historical record"). Some live API names (e.g. `process_command`, `clear_all_buttplug_features`, `convert_parameter_source`) match the deletion-manifest globs but were always intended to keep their names; the manifest was about the legacy dual-path code, not the names themselves.
- ✅ `src-tauri/src/buttplug/pipeline.rs`, `buttplug/state.rs` no longer exist (sub F).
- ✅ `src-tauri/src/websocket.rs` no longer exists (Step 4).
- ✅ `src/lib/stores/inputPosition.ts` no longer exists (sub G.1).
- ✅ `src/lib/components/ui/ButtplugLinkPanel.svelte` no longer exists (sub G.3.1).
- ✅ `src/lib/components/RangeSlider.svelte` no longer exists (sub G.3.3, dead-code cleanup).
- ✅ Frontend renders a post-curve position line on every linked-parameter card (sub G.1's UX win, indicator inside `RangeSliderWithIndicator`).
- ✅ Frontend transforms editor lets users attach `Smooth` / `Scale` / `Clamp` / `Invert` / `Hold` / `Mix` / `Vibrate` / `Oscillate` / `Rotate` / `Constrict` to any link (sub G.2).
- ✅ `cargo test`: 105 pass, 1 pre-existing failure (`processing::tests::test_parse_tcode_with_interval`) unchanged.
- ⏳ Beta-branch validation on a real device is the maintainer's job — pre-merge feel-test of a Buttplug preset that combines Vibrate + Constrict (Constrict centering shifted; release-notes bullet drafted above).

---

## Communication style the maintainer uses

The maintainer runs a multi-agent dashboard with TTS. Keep voice updates short (1–3 sentences), set status `goal` once and `task` per substep, use emojis. Caveman mode is active — drop articles / fluff / hedging in conversation. Code, commits, and security writeups stay normal English.

The maintainer values: thoughtful migration over speed; deletion over deprecation; tests pinning behavior before refactors; reviewer feedback distinguishing "should have caught" from "deferred to later sub".

The maintainer doesn't like: parallel architectures, modal dialogs for migration, dead code left "for compat", silent fallbacks without observability.
