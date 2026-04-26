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

The two pre-existing test failures (`buttplug::pipeline::tests::test_pipeline_oscillate`, `processing::tests::test_parse_tcode_with_interval`) are not new — they live on `main` as well and are out of scope for this refactor. The same two failures persist across subs B-D; total pass count grew from 67 (pre-refactor) → 72 (sub B) → 73 (sub C) → 93 (sub D's 20 transform tests).

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

## Next concrete action: sub F (`git rm` + field deletion)

Sub E shipped the unified resolver and the gate that routes Buttplug-namespaced intensity through it. Sub F is the cleanup: now that `process_buttplug_pipeline` has no live callers and `Channel.buttplug_link` has no readers, delete them.

### What goes away

| Symbol | Where | Why it can go now |
|---|---|---|
| `process_buttplug_pipeline` (fn + 5 tests) | `src-tauri/src/buttplug/pipeline.rs` | Sub E removed the only caller in `processing.rs::get_next_waveform_data`. The re-export in `buttplug/mod.rs` is already gone. |
| `ButtplugChannelState`, `PositionDurationState` | `src-tauri/src/buttplug/state.rs` | Pipeline-only state. `ButtplugFeatureValues::from_input_bus` is the only thing in this file the bundled phase still uses (called by `processing.rs::get_buttplug_feature_values` — also dead in sub E, slated for the same delete). |
| `ButtplugFeatureValues`, `get_buttplug_feature_values`, `has_buttplug_input` | `state.rs` + `processing.rs` | Both methods on `ProcessingState` are now dead-flagged by `cargo check`. The bus snapshot replaces them. |
| `Channel.buttplug_link`, `Channel.buttplug_state` | `processing.rs` | `#[deprecated]` markers in sub E follow-up; sub F drops the fields plus the constructor entries plus `set_buttplug_link_config`. |
| `set_buttplug_link_config`, `apply_channel_config_to_state`'s `bp_config` write | `processing.rs` + `main.rs` | Write-only path kept alive in sub E so the field had a coherent value. With the field gone, the call goes too. |
| `ButtplugLinkConfig`, `FeatureTypeConfig`, `ButtplugLinksSettings::to_link_config` | `buttplug/types.rs`, `settings.rs` | `to_link_config` is the only caller of `ButtplugLinkConfig::default()` left after sub E. Settings-load goes through `settings_convert::buttplug_links_to_transforms` directly now. Trim `types.rs` to what `buttplug/handler.rs` still needs (`ButtplugFeatureConfig` for advertising the device descriptor; possibly `ButtplugFeatureType`). |
| `ConstrictionMethod` re-export from `buttplug` | `buttplug/types.rs` (canonical definition) → `transforms/buttplug.rs` | Move the canonical enum into `transforms/buttplug.rs` so the resolver layer owns its own type. Drop the `pub use crate::buttplug::ConstrictionMethod` re-export in `transforms/mod.rs`. |
| `BUTTPLUG_MAX_FEATURES` constant | `processing.rs:22` | Used only by `get_buttplug_feature_values`; dies with it. |
| `InputBus::has_any_with_prefix`, `clear`, `age_ms`, `latest_timestamp` | `input_bus.rs` | All flagged dead by `cargo check`. Kept for sub G's input-monitor work — verify before deleting. |

### Files deleted entirely (per deletion manifest, plan doc lines 365-371)

- `src-tauri/src/buttplug/pipeline.rs`
- `src-tauri/src/buttplug/state.rs`

### Files trimmed

- `src-tauri/src/buttplug/types.rs` — keep only what `buttplug/handler.rs` reads (verify; likely `ButtplugFeatureConfig`, possibly `ButtplugFeatureType`).
- `src-tauri/src/buttplug/mod.rs` — drop the re-exports for the dropped types. Keep `pub mod handler` and whatever still survives.
- `src-tauri/src/processing.rs` — drop `Channel.buttplug_link`, `Channel.buttplug_state`, `set_buttplug_link_config`, `get_buttplug_feature_values`, `has_buttplug_input`, `BUTTPLUG_MAX_FEATURES`, the `use crate::buttplug::{ButtplugChannelState, ButtplugLinkConfig}` import.
- `src-tauri/src/main.rs` — drop the `bp_config` block in `apply_channel_config_to_state` and the `_touch_buttplug_links` test marker if it references the removed type.
- `src-tauri/src/settings.rs` — `ButtplugLinksSettings::to_link_config` goes (no caller). The struct itself stays for the field sub F preserves on `ParameterSourceSettings` until the schema-deletion step. Cross-reference plan doc deletion-manifest table.
- `src-tauri/src/transforms/mod.rs` — replace `pub use crate::buttplug::ConstrictionMethod` with the canonical definition (move from `buttplug/types.rs`).

### Acceptance for sub F

- `cargo check` clean — most of the dead-code warnings sub E left behind go away.
- `cargo test` — 109+ pass, 2 pre-existing failures unchanged. The 5 tests inside `buttplug/pipeline.rs` are deleted with the file (drop from the count).
- `git grep` deletion-manifest checklist (plan doc lines 484-510): `process_buttplug_pipeline`, `ButtplugChannelState`, `ButtplugFeatureValues`, `ButtplugLinkConfig`, `buttplug_features`, `buttplug_linear_commands`, `buttplug_rotate_directions`, `buttplug_link`, `buttplug_state` — zero hits outside `docs/plans/*` and `git log`.
- The settings schema field `ChannelSettings.intensity_source.buttplug_links` survives sub F (settings-load converts it); the deletion manifest moves it to a later cleanup once the frontend-side editor for transforms lands in sub G. Document this in the sub F commit body.

### Sub F — likely commit shape

One commit. The deletes are tightly coupled (deleting the field and deleting `process_buttplug_pipeline` together prevents an intermediate state where `set_buttplug_link_config` writes a field nothing reads). Expect ~600-800 lines deleted, ~50 added (move of `ConstrictionMethod`, possibly a few `#[allow(dead_code)]` cleanups).

### Sub G + Step 8 — sketches (unchanged from previous session)

- **Sub G — Frontend resolved-state stream.** New Tauri event `resolved-update` carrying `ResolvedUpdatePayload { channel_a, channel_b: ChannelResolvedSnapshot { frequency, frequency_balance, intensity_balance, intensity: ResolvedSampleSnapshot { raw_input, normalized_pre_range, device_value, target_time_ms, source_axis } } }`. Emitted at 10Hz tick from `device.rs` send path. New stores `src/lib/stores/resolvedState.ts` + `src/lib/stores/inputBus.ts` replacing `src/lib/stores/inputPosition.ts`. Linked-parameter UI cards render the post-curve position line on their curve plot. **Sub E left a gap here**: the engine-path channels (T-Code intensity) don't run the resolver, so a naive `resolved-update` emission would be blank for them. Sub G must either synthesize a `ResolvedSample` from `Channel.v2.get_value_at(now)` / `Channel.v3.current_position` for engine channels, or close the gate fully (run the resolver after the engine produces its 4-slot output and capture only the post-range value into `ResolvedSample`). Frontend transform editor lands here too — match the kebab-case discriminator (`{"type": "vibrate", ...}`) sub D shipped, and add the new `{"type": "rotate", ...}` variant sub E added.

### Step 8 — sketch

Wrap existing input handlers in an `InputSource` trait. Mostly cosmetic by then. New `src-tauri/src/input/` module with `input/mod.rs` declaring the trait, `input/tcode.rs` (wraps `tcode_input.rs`), `input/gamepad.rs`, `input/buttplug.rs` (with `lovense` as adapter inside). The win: adding a future input source (MIDI, OSC, audio amplitude) is one self-contained file.

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
