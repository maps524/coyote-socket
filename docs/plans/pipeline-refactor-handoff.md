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
- **Current HEAD:** `80ec841` (will drift as you commit)

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

The two pre-existing test failures (`buttplug::pipeline::tests::test_pipeline_oscillate`, `processing::tests::test_parse_tcode_with_interval`) are not new — they live on `main` as well and are out of scope for this refactor.

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

## Next concrete action: sub B

Lift the three Buttplug HashMaps from `ProcessingState` into the unified `InputBus`. This is the most semantically rich substep in the bundle because it requires changing how the Buttplug pipeline tracks "new arrivals" and time.

### What changes

| Today | After sub B |
|---|---|
| `ProcessingState.buttplug_features: HashMap<String, f64>` | `bus.update("bp:Vibrate_0", v, ts, None)` etc. |
| `ProcessingState.buttplug_linear_commands: HashMap<usize, (f64, u32, Instant)>` | `bus.update("bp:LinearCmd_0", v, ts, Some(duration_ms))` |
| `ProcessingState.buttplug_rotate_directions: HashMap<usize, bool>` | `bus.update("bp:RotateDir_0", encoded, ts, None)` (encode `true` as `1.0`, `false` as `0.0` — or stick with a boolean axis type if you want — single channel, simple) |
| Pipeline + `ButtplugFeatureValues` use `Instant` | Convert to `u64 ms` so bus's `AxisState.timestamp` (wall-clock ms) round-trips through interpolation |
| `clear()` of `buttplug_linear_commands` after pipeline tick | Per-channel `last_buttplug_replay_ts: u64` watermark on `Channel`; "new arrival" check is `bus.latest_timestamp(axis) > self.last_buttplug_replay_ts` |

### Files touched

- `src-tauri/src/processing.rs` — drop the 3 HashMap fields + their default initializers. Rewrite `set_buttplug_*`, `get_buttplug_*`, `clear_all_buttplug_features`, `has_buttplug_input`, `get_buttplug_feature_values` to use `self.input_bus`. The methods stay (handlers don't need to change) — only their internals change. `Channel` gains `last_buttplug_replay_ts: u64`. `get_next_waveform_data` updates the watermark after pipeline runs (replaces the `clear()`).
- `src-tauri/src/buttplug/state.rs` — `PositionDurationState.start_time: Instant` → `u64`. `ButtplugFeatureValues.position_with_duration: Vec<Option<(f64, u32, Instant)>>` → `Vec<Option<(f64, u32, u64)>>`. `from_hashmap` signature stays (still takes the 3 maps); `get_buttplug_feature_values` rebuilds them from bus reads. Remove the `Instant` import.
- `src-tauri/src/buttplug/pipeline.rs` — `process_buttplug_pipeline(state, features, config, now: Instant, dt_ms: u32)` → `now_ms: u64`. Interpolation: `let elapsed_ms = now_ms.saturating_sub(pds.start_time) as f64;`. Tests update.
- `src-tauri/src/buttplug/handler.rs` and `src-tauri/src/lovense/handler.rs` — call sites of `set_buttplug_feature` / `set_buttplug_linear_cmd` / `set_buttplug_rotate_direction` are unchanged from caller's POV. Remove the `Instant::now()` call from `set_buttplug_linear_cmd` (timestamp is `current_time_ms()` instead, fed via the bus update).

### Acceptance for sub B

- `cargo check` clean.
- `cargo test` shows 67+ passes, 2 pre-existing failures, no new failures.
- `git grep "ProcessingState\.\(buttplug_features\|buttplug_linear_commands\|buttplug_rotate_directions\)"` returns zero hits.
- Tests for sub B specifically: bus-backed `set_buttplug_feature` + `get_buttplug_features` round-trips through `bp:` namespace; LinearCmd watermark detects fresh arrivals once per channel; `clear_all_buttplug_features` only drops `bp:*` axes (T-Code / gamepad axes survive).

### Subs C–G — sketches

Each is its own commit. Plan doc has full descriptions; what follows is the half-page version.

- **Sub C — `ParameterSource` → `ParameterLinkConfig` + `ParameterLinkRuntime`.** Rename and split. `Channel` gains `link_runtime: ChannelLinkRuntime` holding per-parameter mutable transform state (initially empty `Vec<TransformState>`). `ParameterSource.buttplug_links` field comes off — its consumer `Channel.buttplug_link` becomes a `Vec<TransformConfig>` for now, populated by `convert_channel_settings`. Settings serialization must stay backward-compatible via `#[serde(alias = "...")]`.
- **Sub D — `TransformConfig` enum + variants.** New module `transforms.rs` for generic primitives (`Smooth`, `Scale`, `Clamp`, `Invert`, `Hold`, `Mix`). New module `transforms/buttplug.rs` for semantic wrappers (`Vibrate`, `Oscillate`, `Constrict`) built atop the generics. Each `TransformConfig::declared_axes()` returns the bus channels its modifier inputs come from. **Per the GPT/Gemini reviews, transforms must NOT read the bus directly** — the resolver pre-fetches modifier values at the link's `target_time` and passes them in as `apply(value: f64, modifiers: &[f64], state: &mut TransformState, target_time: u64)`.
- **Sub E — Resolver rewrite.** `resolve_parameter*` takes `&InputBusSnapshot` (introduce the type — even if it's just `&'a InputBus`, name it for clarity) and threads `(cfg, runtime, snapshot, now)`. Pre-fetch every transform's declared axes at `target_time` before `apply`. Remove the intensity short-circuit in `get_next_waveform_data`. Buttplug-driven intensity now flows through curve / midpoint / range like every other source. **This is the feel-affecting change.** Validate on a beta branch with a saved Buttplug-driven preset; identity defaults should keep behavior approximate, but not exact.
- **Sub F — Delete.** `git rm src-tauri/src/buttplug/pipeline.rs src-tauri/src/buttplug/state.rs`. Trim `buttplug/types.rs` down to whatever `buttplug/handler.rs` still needs (`ButtplugFeatureConfig`?). Run the deletion-manifest grep checklist (plan doc, near the bottom).
- **Sub G — Frontend resolved-state stream.** New Tauri event `resolved-update` carrying `ResolvedUpdatePayload { channel_a, channel_b: ChannelResolvedSnapshot { frequency, frequency_balance, intensity_balance, intensity: ResolvedSampleSnapshot { raw_input, normalized_pre_range, device_value, target_time_ms, source_axis } } }`. Emitted at 10Hz tick from `device.rs` send path (resolver already runs there). New stores `src/lib/stores/resolvedState.ts` + `src/lib/stores/inputBus.ts` replacing `src/lib/stores/inputPosition.ts`. Linked-parameter UI cards render the post-curve position line on their curve plot.

  **Sub D migration note for Constrict feel:** sub D's `TransformConfig::Constrict` centers its narrowing band on the running `value` (post-prior-transforms) when `use_midpoint=false`, where the pre-refactor `process_buttplug_pipeline` centered on `state.base_position` (the pipeline's running base). In the new model `value` IS the running base by the time Constrict runs, so it's the closest analogue — but a saved preset whose intensity link has both Vibrate AND Constrict in the chain will now constrict around the wobbled value rather than the un-wobbled base. Capture in the release-notes "behavioral changes" section; flag a beta-branch feel-test for any preset that uses the combination.

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
