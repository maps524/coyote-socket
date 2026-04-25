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

The outer repo's `main` may have unrelated commits from the maintainer landing in parallel. Don't merge or rebase onto main from inside the worktree without checking with the maintainer first.

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

## Next concrete action: sub E

Sub E is the bundled phase's load-bearing commit. The previous session shipped subs A-D (foundation: bus + lifted Buttplug + runtime split + transforms module); sub E is what makes any of it produce different output. **Land it carefully — this is the feel-affecting change.**

### What changes

The unified resolver replaces the Buttplug pipeline short-circuit in `processing.rs::get_next_waveform_data`. Today (post-sub D) the function still calls `process_buttplug_pipeline` for every channel whose `ch.buttplug_link.has_any_links()` is true and short-circuits the engine path with the result. Sub E rips that out and routes intensity through the same `resolve_parameter`-style path that already handles frequency / freq_balance / int_balance, with `ParameterLinkConfig.transforms` as the new shaping pipeline.

| Today (post-sub D) | After sub E |
|---|---|
| `process_buttplug_pipeline` runs in `get_next_waveform_data` and short-circuits the engine | All four parameters (frequency, freq_balance, int_balance, intensity) resolve via one unified function. Buttplug-driven intensity flows through midpoint → curve → transforms → range like T-Code. |
| `Channel.buttplug_link: ButtplugLinkConfig` is read by the pipeline | Read by sub E's settings-load conversion **once** to seed `ChannelConfig.intensity.transforms`, then never read again. (Field stays for sub F to delete.) |
| Resolver fns take `&InputBus` | Introduce `InputBusSnapshot` type alias / wrapper (even `&'a InputBus` named for clarity). Plan doc says "even if it's just `&'a InputBus`, name it." |
| Pre-fetch contract is implicit | For each transform: `let modifiers: Vec<f64> = cfg.declared_axes().iter().map(|axis| snapshot.value_at(axis, target).unwrap_or(0.0)).collect()`. Pass into `apply_transform`. |

### Settings-load conversion (the bit sub C deferred)

`settings_convert::convert_parameter_source` currently sets `transforms: Vec::new()`. Sub E should populate it: when the persisted `ParameterSourceSettings.buttplug_links` is `Some(_)`, translate each non-None feature link into the matching `TransformConfig` variant:

| `ButtplugLinksSettings` field | Sub E's emitted `TransformConfig` |
|---|---|
| `vibrate_feature: Some(i)` + `vibrate_config { distance }` | `TransformConfig::Vibrate { speed_axis: format!("bp:Vibrate_{i}"), distance: distance.unwrap_or(0.2) }` |
| `oscillate_feature: Some(i)` + `oscillate_config { scale, max_speed }` | `TransformConfig::Oscillate { speed_axis: format!("bp:Oscillate_{i}"), scale: ..., max_speed_hz: ... }` |
| `constrict_feature: Some(i)` + `constrict_config { min_floor, use_midpoint, method }` | `TransformConfig::Constrict { amount_axis: format!("bp:Constrict_{i}"), min_floor: ..., use_midpoint: ..., method: ... }` |
| `position_feature` / `pos_dur_feature` | The base linked axis, NOT a transform. The persisted `intensity_source.source_axis` should already be `bp:Position_{i}` or `bp:PositionWithDuration_{i}` (post-sub-B handler shapes the namespace). Verify and update `convert_parameter_source` to set the source axis correspondingly when `buttplug_links` carries Position. |
| `rotate_feature` | Rotate uses paired `bp:Rotate_{i}` + `bp:RotateDir_{i}` — sub D didn't define a Rotate transform. **Decision needed**: either (a) add a new `TransformConfig::Rotate` variant (matches the deletion-manifest "process_buttplug_pipeline replaced by ordered transforms" claim), or (b) compose Rotate from `Oscillate` + a sign-flip via `Mix`/`Scale` (closer to "generic primitives compose"). Plan doc lists Vibrate/Oscillate/Constrict as the 3 wrappers; Rotate is absent. **Recommend asking the maintainer** before extending the variant set. |

The order matters for the resolver loop. Plan doc's "transforms apply post-curve, pre-range, in declared order" — so the conversion should produce the same effective stage order as `process_buttplug_pipeline`: Position is the source (not a transform), then Oscillate / Rotate, then Vibrate, then Constrict. Mirror that ordering when emitting the vec.

### Files touched (estimated)

- `src-tauri/src/modulation.rs` or new `src-tauri/src/resolver_v2.rs` — unified `resolve_link(cfg, runtime, snapshot, now, no_input_behavior, decay_ms) -> ResolvedSample`. `ResolvedSample { raw_input, normalized_pre_range, device_value, target_time_ms, source_axis }`. Pre-fetch transforms' modifiers; thread state through `runtime.transform_state[i]`. Sub G's wire format already specs this struct shape — define it once now.
- `src-tauri/src/processing.rs::get_next_waveform_data` — drop the `bp_intensities` block + `process_buttplug_pipeline` call. Resolve intensity per channel via the new function. Convert resolved 0..1 to 0..200 u8 → feed engines.
- `src-tauri/src/processing.rs::Channel` — initialize `link_runtime.{frequency,freq_balance,int_balance,intensity}.transform_state` from `config.{...}.transforms.iter().map(|t| t.initial_state()).collect()` whenever config changes. The init point is `apply_channel_config_to_state` in `main.rs` — after writing `state_guard.channel_mut(channel_id).config = new_config`, also reset `link_runtime` to match.
- `src-tauri/src/settings_convert.rs` — populate `transforms` from `buttplug_links`. Adjust the source-axis for Position / PositionWithDuration features.
- `src-tauri/src/resolver.rs` — `get_resolved_channel_params` / `get_per_slot_frequencies` migrate to take a snapshot + the new resolver. Frequency is the per-slot 25ms case; verify the new resolver works correctly with varying `target_time_ms` per call (sub D's transforms already handle this).

### Carry-forward notes from sub D's reviewers

- **Must drop the `Channel.buttplug_link` consumer in the same commit that wires `ParameterLinkConfig.transforms`.** Otherwise the resolver double-applies (transforms run via the new path AND `process_buttplug_pipeline` runs via the old short-circuit). Sub D's design reviewer flagged this as a sub E precondition.
- **Constrict centering changed** in sub D: `value` (post-prior-transforms) instead of `state.base_position`. In the new layered model these are the same value once Position is the source axis, but a saved preset with both Vibrate and Constrict will now constrict around the wobbled value rather than the un-wobbled base. Capture in beta release notes; consider a feel-test of a known Vibrate+Constrict preset before merging sub E.
- **Per-write timestamp coherence** (sub B's deferred-to-sub-E note): each `set_buttplug_feature` call stamps its own `current_time_ms()`. A 6-feature `ScalarCmd` produces 6 monotonically-different bus timestamps. For latest-wins reads it's benign; for the LinearCmd new-arrival watermark a tick boundary lands inside the batch could split one logical command across two ticks. Sub E should consider a per-batch timestamp argument on `set_buttplug_feature` (called once per `ScalarCmd` in `buttplug/handler.rs`) so all features in a batch share an arrival timestamp.

### Acceptance for sub E

- `cargo check` clean. `cargo test` shows 93+ passes (sub D's count is the floor), 2 pre-existing failures, no new failures.
- `git grep "process_buttplug_pipeline"` returns hits ONLY in `buttplug/pipeline.rs` itself (the function definition + tests; sub F deletes the file). No callers.
- `git grep "Channel\.buttplug_link"` returns hits ONLY in `processing.rs` field declaration + `apply_channel_config_to_state` write site. No reads.
- Tests pinning the new shape: a `ParameterLinkConfig` with one `Vibrate` transform produces an output that wobbles around the input value; the same config with `Vibrate` + `Constrict` chained narrows around the wobbled value (per the sub D centering shift); changing `range_min`/`range_max` on a Buttplug-driven intensity now visibly changes the device output.
- Beta-branch validation on a real device with a known Vibrate+Constrict preset before merging.

### Subs F–G + Step 8 — sketches

- **Sub F — Delete.** `git rm src-tauri/src/buttplug/pipeline.rs src-tauri/src/buttplug/state.rs`. Trim `buttplug/types.rs` down to whatever `buttplug/handler.rs` still needs (`ButtplugFeatureConfig`). Move `ConstrictionMethod` into `transforms/buttplug.rs` and drop the re-export. Drop `Channel.buttplug_link` + `Channel.buttplug_state` (the TODO-flagged fields from sub C). Run the deletion-manifest grep checklist (plan doc, near the bottom).
- **Sub G — Frontend resolved-state stream.** New Tauri event `resolved-update` carrying `ResolvedUpdatePayload { channel_a, channel_b: ChannelResolvedSnapshot { frequency, frequency_balance, intensity_balance, intensity: ResolvedSampleSnapshot { raw_input, normalized_pre_range, device_value, target_time_ms, source_axis } } }`. Emitted at 10Hz tick from `device.rs` send path (resolver already runs there). New stores `src/lib/stores/resolvedState.ts` + `src/lib/stores/inputBus.ts` replacing `src/lib/stores/inputPosition.ts`. Linked-parameter UI cards render the post-curve position line on their curve plot. Frontend transform editor lands here too — match the kebab-case discriminator (`{"type": "vibrate", ...}`) sub D shipped.

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

He's running on a multi-agent dashboard with TTS. Keep voice updates short (1–3 sentences), set status `goal` once and `task` per substep, use emojis. Caveman mode is active — drop articles / fluff / hedging in conversation. Code, commits, and security writeups stay normal English.

He values: thoughtful migration over speed; deletion over deprecation; tests pinning behavior before refactors; reviewer feedback distinguishing "should have caught" from "deferred to later sub".

He doesn't like: parallel architectures, modal dialogs for migration, dead code left "for compat", silent fallbacks without observability.
