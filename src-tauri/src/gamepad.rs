//! Gamepad input — pluggable engines.
//!
//! Two engines, switchable at runtime via the `gamepadEngine` setting:
//!   - **gilrs**: cross-platform (Linux/macOS/Windows DirectInput+XInput),
//!     supports any controller gilrs recognizes (Xbox, Switch, generic HID).
//!     Less reliable for Xbox-via-Wireless-Adapter on Windows in some cases.
//!   - **xinput** (Windows only): direct XInput polling via `rusty-xinput`.
//!     Rock-solid for Xbox controllers (USB, Wireless Adapter, BT). Ignores
//!     non-XInput devices.
//!
//! Both engines emit identical Tauri events: `input-action` (when a press
//! matches a binding) and `gamepad-raw` (every raw button/axis event, used
//! by the rebind UI).

use crate::settings::{AxisDir, ChordPart, GamepadBinding, GamepadBindings};
use crate::{get_app_handle, log_info, log_warn};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock as StdRwLock};
use tauri::async_runtime::JoinHandle;
use tauri::Emitter;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{sleep, Duration};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

static ACTIVE_BINDINGS: tokio::sync::OnceCell<Arc<RwLock<GamepadBindings>>> =
    tokio::sync::OnceCell::const_new();

/// Currently running engine task (if any). Switching engines aborts the
/// existing task and spawns a fresh one.
static CURRENT_TASK: tokio::sync::OnceCell<Mutex<Option<JoinHandle<()>>>> =
    tokio::sync::OnceCell::const_new();

/// Live input state for combo / chord detection.
#[derive(Default)]
struct GamepadActive {
    /// Currently-held button indices (standard Gamepad API).
    buttons: HashSet<u8>,
    /// Last value seen per axis index. Combo axis parts evaluate threshold
    /// against this on demand.
    axis_values: HashMap<u8, f64>,
    /// Combos that already fired and are waiting for any part to release
    /// before they can fire again.
    armed_combos: HashSet<String>,
    /// Per-action repeat state: (first_observed, last_fired). Used to
    /// generate auto-repeat for button-only bindings while held.
    held_repeat: HashMap<String, (std::time::Instant, std::time::Instant)>,
}

static ACTIVE_STATE: tokio::sync::OnceCell<Arc<Mutex<GamepadActive>>> =
    tokio::sync::OnceCell::const_new();

pub async fn init_active_bindings(initial: GamepadBindings) {
    ACTIVE_BINDINGS
        .get_or_init(|| async { Arc::new(RwLock::new(initial)) })
        .await;
    CURRENT_TASK
        .get_or_init(|| async { Mutex::new(None) })
        .await;
    ACTIVE_STATE
        .get_or_init(|| async { Arc::new(Mutex::new(GamepadActive::default())) })
        .await;
}

pub async fn set_active_bindings(bindings: GamepadBindings) {
    if let Some(state) = ACTIVE_BINDINGS.get() {
        *state.write().await = bindings;
    }
}

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
struct InputActionPayload {
    action: String,
    source: &'static str,
    /// 0..1 deflection magnitude for continuous axis emissions; 1.0 for
    /// discrete (button / axis-cross / combo) emissions.
    magnitude: f64,
    /// True for stick/trigger continuous emissions. Frontend uses this to
    /// gate the user-configurable sensitivity multiplier.
    continuous: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum GamepadRawPayload {
    #[serde(rename_all = "camelCase")]
    Button {
        index: u8,
        #[serde(default)]
        released: bool,
    },
    #[serde(rename_all = "camelCase")]
    Axis {
        index: u8,
        dir: &'static str,
        threshold: f64,
        #[serde(default)]
        released: bool,
    },
}

const REBIND_AXIS_THRESHOLD: f64 = 0.5;

// ---------------------------------------------------------------------------
// Connection tracking
//
// Tracks how many gamepads the active engine sees, and pushes a Tauri event
// whenever that changes. Used by the frontend to gate the T-Code monitor and
// drive the gamepad status pill in the nav.
// ---------------------------------------------------------------------------

static CONNECTED_COUNT: AtomicUsize = AtomicUsize::new(0);
static CONNECTED_FLAG: AtomicBool = AtomicBool::new(false);
static ACTIVE_ENGINE_NAME: OnceLock<StdRwLock<String>> = OnceLock::new();

#[derive(Clone, Serialize)]
pub struct GamepadStatusPayload {
    pub connected: bool,
    pub count: usize,
    pub engine: String,
}

fn engine_lock() -> &'static StdRwLock<String> {
    ACTIVE_ENGINE_NAME.get_or_init(|| StdRwLock::new("off".to_string()))
}

fn current_engine_name() -> String {
    engine_lock()
        .read()
        .map(|s| s.clone())
        .unwrap_or_else(|_| "off".to_string())
}

fn set_active_engine_name(name: &str) {
    if let Ok(mut guard) = engine_lock().write() {
        *guard = name.to_string();
    }
}

/// Snapshot of the live status — used by the `get_gamepad_status` command for
/// HMR refresh and by the in-process emitter below.
pub fn current_status() -> GamepadStatusPayload {
    GamepadStatusPayload {
        connected: CONNECTED_FLAG.load(Ordering::Relaxed),
        count: CONNECTED_COUNT.load(Ordering::Relaxed),
        engine: current_engine_name(),
    }
}

fn emit_status(payload: &GamepadStatusPayload) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit("gamepad-status", payload.clone());
    }
}

/// Set the absolute connected-gamepad count for the active engine and emit if
/// the connected/disconnected boundary or the count changed. Engines call this
/// whenever their internal "user X is connected" state mutates.
fn update_connected_count(new_count: usize) {
    let was = CONNECTED_COUNT.swap(new_count, Ordering::Relaxed);
    let was_connected = CONNECTED_FLAG.load(Ordering::Relaxed);
    let now_connected = new_count > 0;
    if was_connected != now_connected {
        CONNECTED_FLAG.store(now_connected, Ordering::Relaxed);
    }
    if was != new_count || was_connected != now_connected {
        let payload = current_status();
        log_info!(
            "[gamepad] status changed: engine={} connected={} count={}",
            payload.engine,
            payload.connected,
            payload.count
        );
        emit_status(&payload);
    }
}

fn reset_connection_state() {
    CONNECTED_COUNT.store(0, Ordering::Relaxed);
    CONNECTED_FLAG.store(false, Ordering::Relaxed);
    let payload = current_status();
    emit_status(&payload);
}

// ---------------------------------------------------------------------------
// Engine selection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamepadEngine {
    Off,
    Gilrs,
    Xinput,
}

impl GamepadEngine {
    pub fn from_str(s: &str) -> Self {
        match s {
            "off" => Self::Off,
            "xinput" => Self::Xinput,
            _ => Self::Gilrs,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Gilrs => "gilrs",
            Self::Xinput => "xinput",
        }
    }
}

/// Switch (or start) the active engine. Aborts any prior engine task first.
pub async fn set_engine(engine: GamepadEngine) {
    let cell = match CURRENT_TASK.get() {
        Some(c) => c,
        None => {
            eprintln!("[gamepad] set_engine called before init_active_bindings");
            return;
        }
    };
    let mut guard = cell.lock().await;
    if let Some(task) = guard.take() {
        eprintln!("[gamepad] aborting previous engine task");
        task.abort();
    }

    // Engine swap invalidates any prior connection state — the new engine has
    // not enumerated devices yet. Reset to disconnected so the UI flips off
    // immediately, then the engine loop will re-emit on first detection.
    set_active_engine_name(engine.as_str());
    reset_connection_state();

    let handle = match engine {
        GamepadEngine::Off => {
            eprintln!("[gamepad] engine: off");
            log_info!("[gamepad] engine: off");
            None
        }
        GamepadEngine::Gilrs => {
            eprintln!("[gamepad] engine: gilrs");
            log_info!("[gamepad] engine: gilrs");
            Some(tauri::async_runtime::spawn(gilrs_loop()))
        }
        GamepadEngine::Xinput => {
            eprintln!("[gamepad] engine: xinput");
            log_info!("[gamepad] engine: xinput");
            Some(tauri::async_runtime::spawn(xinput_loop()))
        }
    };
    *guard = handle;
}

// ---------------------------------------------------------------------------
// Shared dispatch
// ---------------------------------------------------------------------------

async fn on_button_press(index: u8) {
    emit_raw(GamepadRawPayload::Button {
        index,
        released: false,
    });

    let active_cell = match ACTIVE_STATE.get() {
        Some(a) => a,
        None => return,
    };
    let bindings = match ACTIVE_BINDINGS.get() {
        Some(b) => b.read().await.clone(),
        None => return,
    };

    let mut active = active_cell.lock().await;
    let was_down = !active.buttons.insert(index);
    if was_down {
        // Already-held button — no transition, skip event-fire path.
        return;
    }

    // Single-button bindings.
    for (action, binding) in bindings.iter_bound() {
        if let GamepadBinding::Button { index: bi } = binding {
            if *bi == index {
                emit_action(action);
            }
        }
    }

    // Combos.
    let fired = evaluate_combos(&mut active, &bindings);
    drop(active);
    for action in fired {
        emit_action(&action);
    }
}

async fn on_button_release(index: u8) {
    emit_raw(GamepadRawPayload::Button {
        index,
        released: true,
    });
    let active_cell = match ACTIVE_STATE.get() {
        Some(a) => a,
        None => return,
    };
    let mut active = active_cell.lock().await;
    active.buttons.remove(&index);
    let bindings = match ACTIVE_BINDINGS.get() {
        Some(b) => b.read().await.clone(),
        None => return,
    };
    // Re-evaluate combos so we disarm any that lost a part.
    let _ = evaluate_combos(&mut active, &bindings);
}

async fn on_axis_change(index: u8, prev: f64, curr: f64) {
    let pos_crossed_in = prev < REBIND_AXIS_THRESHOLD && curr >= REBIND_AXIS_THRESHOLD;
    let neg_crossed_in = prev > -REBIND_AXIS_THRESHOLD && curr <= -REBIND_AXIS_THRESHOLD;
    let pos_crossed_out = prev >= REBIND_AXIS_THRESHOLD && curr < REBIND_AXIS_THRESHOLD;
    let neg_crossed_out = prev <= -REBIND_AXIS_THRESHOLD && curr > -REBIND_AXIS_THRESHOLD;

    if pos_crossed_in {
        emit_raw(GamepadRawPayload::Axis {
            index,
            dir: "pos",
            threshold: REBIND_AXIS_THRESHOLD,
            released: false,
        });
    }
    if neg_crossed_in {
        emit_raw(GamepadRawPayload::Axis {
            index,
            dir: "neg",
            threshold: REBIND_AXIS_THRESHOLD,
            released: false,
        });
    }
    if pos_crossed_out {
        emit_raw(GamepadRawPayload::Axis {
            index,
            dir: "pos",
            threshold: REBIND_AXIS_THRESHOLD,
            released: true,
        });
    }
    if neg_crossed_out {
        emit_raw(GamepadRawPayload::Axis {
            index,
            dir: "neg",
            threshold: REBIND_AXIS_THRESHOLD,
            released: true,
        });
    }

    let active_cell = match ACTIVE_STATE.get() {
        Some(a) => a,
        None => return,
    };
    let bindings = match ACTIVE_BINDINGS.get() {
        Some(b) => b.read().await.clone(),
        None => return,
    };

    let mut active = active_cell.lock().await;
    active.axis_values.insert(index, curr);

    // Single-axis bindings now use continuous mode (see emit_continuous_axis_actions).
    // Edge-trigger here would double-fire on every threshold cross.

    // Combos: re-evaluate on any axis transition (in or out of active state).
    if pos_crossed_in || neg_crossed_in || pos_crossed_out || neg_crossed_out {
        let fired = evaluate_combos(&mut active, &bindings);
        drop(active);
        for action in fired {
            emit_action(&action);
        }
    }
}

/// Emit `input-action` with magnitude for every single Axis binding whose
/// axis is currently past its threshold. Called on a periodic rate-tick from
/// each engine loop. Magnitude = (|value| - threshold) / (1 - threshold),
/// clamped to [0, 1] — gives 0 at the deadzone and 1 at full deflection.
async fn emit_continuous_axis_actions() {
    let active_cell = match ACTIVE_STATE.get() {
        Some(a) => a,
        None => return,
    };
    let bindings = match ACTIVE_BINDINGS.get() {
        Some(b) => b.read().await.clone(),
        None => return,
    };
    let active = active_cell.lock().await;
    for (action, binding) in bindings.iter_bound() {
        match binding {
            GamepadBinding::Axis { .. } => {
                let m = part_magnitude(
                    &match binding {
                        GamepadBinding::Axis {
                            index,
                            dir,
                            threshold,
                        } => ChordPart::Axis {
                            index: *index,
                            dir: *dir,
                            threshold: *threshold,
                        },
                        _ => unreachable!(),
                    },
                    &active.buttons,
                    &active.axis_values,
                );
                if m > 0.0 {
                    emit_action_with_magnitude(action, m);
                }
            }
            GamepadBinding::Combo { parts } => {
                if !combo_has_axis_part(parts) {
                    continue;
                }
                // Min of all part magnitudes — combo is "as strong as its weakest link".
                let mut min_mag = 1.0_f64;
                let mut any_zero = false;
                for part in parts {
                    let m = part_magnitude(part, &active.buttons, &active.axis_values);
                    if m <= 0.0 {
                        any_zero = true;
                        break;
                    }
                    if m < min_mag {
                        min_mag = m;
                    }
                }
                if any_zero || min_mag <= 0.0 {
                    continue;
                }
                if combo_superseded(parts, &bindings, &active) {
                    continue;
                }
                emit_action_with_magnitude(action, min_mag);
            }
            _ => {}
        }
    }
}

const RATE_TICK_MS: u64 = 80;

/// Auto-repeat held button-only bindings (single Button or pure-button Combo).
/// First fire happens via `on_button_press` / `evaluate_combos`. Once a binding
/// is currently active, this tick records its first-observed time; after
/// `delay_ms` of continuous activity, it fires again every `interval_ms`.
async fn emit_repeat_button_actions(delay_ms: u64, interval_ms: u64) {
    let active_cell = match ACTIVE_STATE.get() {
        Some(a) => a,
        None => return,
    };
    let bindings = match ACTIVE_BINDINGS.get() {
        Some(b) => b.read().await.clone(),
        None => return,
    };
    let now = std::time::Instant::now();

    let mut active = active_cell.lock().await;

    // Build the set of currently-active button-only bindings.
    let mut active_actions: Vec<String> = Vec::new();
    for (action, binding) in bindings.iter_bound() {
        match binding {
            GamepadBinding::Button { index } => {
                if active.buttons.contains(index) {
                    active_actions.push(action.to_string());
                }
            }
            GamepadBinding::Combo { parts } => {
                if combo_has_axis_part(parts) {
                    continue; // axis combos repeat via continuous magnitude
                }
                let all_active = parts.iter().all(|p| match p {
                    ChordPart::Button { index } => active.buttons.contains(index),
                    _ => false,
                });
                if !all_active {
                    continue;
                }
                if combo_superseded(parts, &bindings, &active) {
                    continue;
                }
                active_actions.push(action.to_string());
            }
            _ => {}
        }
    }

    // Drop entries for actions no longer active.
    let active_set: std::collections::HashSet<String> = active_actions.iter().cloned().collect();
    active.held_repeat.retain(|k, _| active_set.contains(k));

    let mut to_fire: Vec<String> = Vec::new();
    for action in &active_actions {
        match active.held_repeat.get(action).copied() {
            None => {
                // First sighting — initial fire already happened on press.
                // Record start time; auto-repeat begins after `delay_ms`.
                active.held_repeat.insert(action.clone(), (now, now));
            }
            Some((first, last)) => {
                if now.duration_since(first) >= Duration::from_millis(delay_ms)
                    && now.duration_since(last) >= Duration::from_millis(interval_ms)
                {
                    to_fire.push(action.clone());
                    active.held_repeat.insert(action.clone(), (first, now));
                }
            }
        }
    }
    drop(active);

    for action in to_fire {
        emit_action(&action);
    }
}

/// Standard gamepad axis names exposed to the parameter linking system.
/// Sticks normalize from -1..1 to 0..1 ((v+1)/2). Triggers are 0..1 native.
/// Buttons get GP_BTN_<n> as 0.0 or 1.0.
const GP_AXIS_LX: &str = "GP_LX";
const GP_AXIS_LY: &str = "GP_LY";
const GP_AXIS_RX: &str = "GP_RX";
const GP_AXIS_RY: &str = "GP_RY";
const GP_AXIS_LT: &str = "GP_LT";
const GP_AXIS_RT: &str = "GP_RT";

/// Returns the set of axis indices (0..5, matching standard Gamepad API
/// indexing) that should be suppressed from parameter-source linkage right
/// now because they're "modulated by combo".
///
/// Rule: for each axis part of each Combo binding, lock that axis iff every
/// OTHER part of the same combo is currently active. "Active" = held for
/// buttons, past threshold for axes (in the part's direction). Triggers
/// (which are axes) work the same as buttons here — holding LT past
/// threshold counts as a satisfied modifier and will lock partner axes.
fn compute_locked_axes(active: &GamepadActive, bindings: &GamepadBindings) -> HashSet<u8> {
    let mut locked = HashSet::new();
    for (_, binding) in bindings.iter_bound() {
        if let GamepadBinding::Combo { parts } = binding {
            for (i, part) in parts.iter().enumerate() {
                let axis_index = match part {
                    ChordPart::Axis { index, .. } => *index,
                    _ => continue,
                };
                let others_active = parts.iter().enumerate().all(|(j, p)| {
                    if i == j {
                        return true;
                    }
                    part_active(p, &active.buttons, &active.axis_values)
                });
                if others_active {
                    locked.insert(axis_index);
                }
            }
        }
    }
    locked
}

/// Push current gamepad axis values into the processing state's axis map
/// (same pool T-Code axes use). Linked parameter sources picking GP_*
/// axes will drive their parameter from these values automatically.
/// Axes locked by an active combo are skipped, so the combo's modulation
/// of those axes doesn't bleed into other parameters that share the axis.
async fn feed_gamepad_axes_to_processing() {
    use crate::processing::{get_processing_state, TCodeCommand};

    // No connected gamepad → don't synthesize stick-centered T-Code commands
    // every tick. Channel intensity bars still update from the device 10 Hz
    // loop, so dropping these is purely a noise reduction.
    if !CONNECTED_FLAG.load(Ordering::Relaxed) {
        return;
    }

    let active_cell = match ACTIVE_STATE.get() {
        Some(a) => a,
        None => return,
    };
    let bindings = match ACTIVE_BINDINGS.get() {
        Some(b) => b.read().await.clone(),
        None => return,
    };

    let (axes, locked) = {
        let active = active_cell.lock().await;
        let axes = [
            active.axis_values.get(&0u8).copied().unwrap_or(0.0),
            active.axis_values.get(&1u8).copied().unwrap_or(0.0),
            active.axis_values.get(&2u8).copied().unwrap_or(0.0),
            active.axis_values.get(&3u8).copied().unwrap_or(0.0),
            active.axis_values.get(&4u8).copied().unwrap_or(0.0),
            active.axis_values.get(&5u8).copied().unwrap_or(0.0),
        ];
        let locked = compute_locked_axes(&active, &bindings);
        (axes, locked)
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Y axes inverted in the parameter-link map so stick UP = 1.0, stick DOWN = 0.0.
    // The XInput backend already flips raw Y (positive = down) so we double-invert here.
    let updates: [(u8, &str, f64); 6] = [
        (0, GP_AXIS_LX, ((axes[0] + 1.0) / 2.0).clamp(0.0, 1.0)),
        (1, GP_AXIS_LY, ((-axes[1] + 1.0) / 2.0).clamp(0.0, 1.0)),
        (2, GP_AXIS_RX, ((axes[2] + 1.0) / 2.0).clamp(0.0, 1.0)),
        (3, GP_AXIS_RY, ((-axes[3] + 1.0) / 2.0).clamp(0.0, 1.0)),
        (4, GP_AXIS_LT, axes[4].clamp(0.0, 1.0)),
        (5, GP_AXIS_RT, axes[5].clamp(0.0, 1.0)),
    ];

    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    let mut axes_snapshot: HashMap<String, f64> = HashMap::new();
    for (idx, name, value) in updates {
        if locked.contains(&idx) {
            continue;
        }
        let cmd = TCodeCommand {
            axis: name.to_string(),
            value,
            interval_ms: None,
            received_at: now,
        };
        state_guard.process_command(&cmd);
    }
    let (channel_a, channel_b) = state_guard.get_current_intensities();
    for (k, v) in state_guard.axis_values.iter() {
        axes_snapshot.insert(k.clone(), v.value);
    }
    drop(state_guard);
    crate::emit_axis_update(axes_snapshot, channel_a, channel_b);
}

fn part_active(part: &ChordPart, buttons: &HashSet<u8>, axis_values: &HashMap<u8, f64>) -> bool {
    match part {
        ChordPart::Button { index } => buttons.contains(index),
        ChordPart::Axis {
            index,
            dir,
            threshold,
        } => {
            let v = axis_values.get(index).copied().unwrap_or(0.0);
            match dir {
                AxisDir::Pos => v >= *threshold,
                AxisDir::Neg => v <= -*threshold,
            }
        }
    }
}

/// Magnitude of one chord part. Buttons: 0 or 1. Axes: 0..1 deflection past threshold.
fn part_magnitude(
    part: &ChordPart,
    buttons: &HashSet<u8>,
    axis_values: &HashMap<u8, f64>,
) -> f64 {
    match part {
        ChordPart::Button { index } => {
            if buttons.contains(index) {
                1.0
            } else {
                0.0
            }
        }
        ChordPart::Axis {
            index,
            dir,
            threshold,
        } => {
            let v = axis_values.get(index).copied().unwrap_or(0.0);
            let signed = match dir {
                AxisDir::Pos => v,
                AxisDir::Neg => -v,
            };
            if signed < *threshold {
                return 0.0;
            }
            let denom = (1.0 - *threshold).max(0.001);
            ((signed - threshold) / denom).clamp(0.0, 1.0)
        }
    }
}

fn combo_has_axis_part(parts: &[ChordPart]) -> bool {
    parts.iter().any(|p| matches!(p, ChordPart::Axis { .. }))
}

/// Two parts are equivalent if they target the same input. Axis threshold
/// is compared with a tolerance so a part typed with 0.5 matches another
/// with 0.5000001 (different floats from JSON round-trip).
fn parts_equivalent(a: &ChordPart, b: &ChordPart) -> bool {
    match (a, b) {
        (ChordPart::Button { index: ai }, ChordPart::Button { index: bi }) => ai == bi,
        (
            ChordPart::Axis {
                index: ai,
                dir: ad,
                threshold: at,
            },
            ChordPart::Axis {
                index: bi,
                dir: bd,
                threshold: bt,
            },
        ) => ai == bi && ad == bd && (at - bt).abs() < 0.001,
        _ => false,
    }
}

/// Returns true if `parts` is a strict subset of some OTHER currently-fully-active
/// combo in `bindings`. Used to suppress less-specific combos in favor of more
/// specific ones (e.g. LB + stick suppressed when LB + A + stick is active).
fn combo_superseded(
    parts: &[ChordPart],
    bindings: &GamepadBindings,
    active: &GamepadActive,
) -> bool {
    for (_, other) in bindings.iter_bound() {
        let other_parts = match other {
            GamepadBinding::Combo { parts } => parts.as_slice(),
            _ => continue,
        };
        if other_parts.len() <= parts.len() {
            continue;
        }
        // Every part in `parts` must be present in `other_parts`.
        let is_subset = parts
            .iter()
            .all(|p| other_parts.iter().any(|op| parts_equivalent(p, op)));
        if !is_subset {
            continue;
        }
        // The superset combo must currently be fully active.
        let all_active = other_parts
            .iter()
            .all(|p| part_active(p, &active.buttons, &active.axis_values));
        if all_active {
            return true;
        }
    }
    false
}

/// Evaluate edge-trigger Combo bindings (button-only chords) against current
/// state. Combos containing any axis part are continuous and handled in
/// `emit_continuous_axis_actions` instead. Returns actions to fire.
fn evaluate_combos(active: &mut GamepadActive, bindings: &GamepadBindings) -> Vec<String> {
    let mut fired: Vec<String> = Vec::new();
    let mut to_arm: Vec<String> = Vec::new();
    let mut to_disarm: Vec<String> = Vec::new();

    for (action, binding) in bindings.iter_bound() {
        if let GamepadBinding::Combo { parts } = binding {
            if parts.is_empty() {
                continue;
            }
            if combo_has_axis_part(parts) {
                continue; // continuous-mode combo, handled by rate tick
            }
            let all_active = parts
                .iter()
                .all(|p| part_active(p, &active.buttons, &active.axis_values));
            let was_armed = active.armed_combos.contains(action);
            // Suppress this combo when a strictly-more-specific combo is also
            // fully active (e.g. LB+A+stick beats LB+stick).
            let superseded = all_active && combo_superseded(parts, bindings, active);
            if all_active && !superseded && !was_armed {
                fired.push(action.to_string());
                to_arm.push(action.to_string());
            } else if (!all_active || superseded) && was_armed {
                to_disarm.push(action.to_string());
            }
        }
    }

    for a in to_arm {
        active.armed_combos.insert(a);
    }
    for a in to_disarm {
        active.armed_combos.remove(&a);
    }
    fired
}

fn emit_action(action: &str) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            "input-action",
            InputActionPayload {
                action: action.to_string(),
                source: "gamepad",
                magnitude: 1.0,
                continuous: false,
            },
        );
    }
}

fn emit_action_with_magnitude(action: &str, magnitude: f64) {
    if let Some(handle) = get_app_handle() {
        let _ = handle.emit(
            "input-action",
            InputActionPayload {
                action: action.to_string(),
                source: "gamepad",
                magnitude,
                continuous: true,
            },
        );
    }
}

fn emit_raw(payload: GamepadRawPayload) {
    if let Some(handle) = get_app_handle() {
        eprintln!("[gamepad] emit gamepad-raw: {:?}", &payload);
        let _ = handle.emit("gamepad-raw", payload);
    }
}

// ---------------------------------------------------------------------------
// gilrs engine
// ---------------------------------------------------------------------------

async fn gilrs_loop() {
    use gilrs::{EventType, Gilrs};

    let mut gilrs = match Gilrs::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[gamepad/gilrs] init failed: {}", e);
            log_warn!("[gamepad/gilrs] init failed: {}", e);
            return;
        }
    };

    let mut connected_ids: HashSet<gilrs::GamepadId> = HashSet::new();
    for (id, pad) in gilrs.gamepads() {
        connected_ids.insert(id);
        eprintln!(
            "[gamepad/gilrs] {:?}: name='{}' uuid={:?}",
            id,
            pad.name(),
            pad.uuid()
        );
    }
    let startup_count = connected_ids.len();
    eprintln!(
        "[gamepad/gilrs] initialized, {} gamepad(s) detected at startup",
        startup_count
    );
    log_info!("[gamepad/gilrs] initialized, {} gamepad(s)", startup_count);
    update_connected_count(startup_count);

    let mut axis_state: HashMap<(usize, u8), f32> = HashMap::new();
    let mut last_rate_tick = std::time::Instant::now();

    loop {
        while let Some(event) = gilrs.next_event() {
            match event.event {
                EventType::ButtonPressed(btn, _code) => {
                    let index = gilrs_button_to_index(btn);
                    eprintln!("[gamepad/gilrs] ButtonPressed btn={:?} index={}", btn, index);
                    if index == 255 {
                        continue;
                    }
                    on_button_press(index).await;
                }
                EventType::ButtonReleased(btn, _code) => {
                    let index = gilrs_button_to_index(btn);
                    if index == 255 {
                        continue;
                    }
                    on_button_release(index).await;
                }
                EventType::AxisChanged(axis, value, _code) => {
                    let index = gilrs_axis_to_index(axis);
                    if index == 255 {
                        continue;
                    }
                    let key = (gilrs_id_to_usize(event.id), index);
                    let prev = axis_state.get(&key).copied().unwrap_or(0.0);
                    axis_state.insert(key, value);
                    on_axis_change(index, prev as f64, value as f64).await;
                }
                EventType::Connected => {
                    eprintln!("[gamepad/gilrs] Connected: {:?}", event.id);
                    log_info!("[gamepad/gilrs] connected: {:?}", event.id);
                    if connected_ids.insert(event.id) {
                        update_connected_count(connected_ids.len());
                    }
                }
                EventType::Disconnected => {
                    eprintln!("[gamepad/gilrs] Disconnected: {:?}", event.id);
                    log_info!("[gamepad/gilrs] disconnected: {:?}", event.id);
                    if connected_ids.remove(&event.id) {
                        update_connected_count(connected_ids.len());
                    }
                }
                _ => {}
            }
        }

        if last_rate_tick.elapsed() >= Duration::from_millis(RATE_TICK_MS) {
            last_rate_tick = std::time::Instant::now();
            emit_continuous_axis_actions().await;
            let g = crate::settings::get_settings().await.general;
            emit_repeat_button_actions(
                g.gamepad_button_repeat_delay_ms as u64,
                g.gamepad_button_repeat_interval_ms as u64,
            )
            .await;
            feed_gamepad_axes_to_processing().await;
        }

        sleep(Duration::from_millis(8)).await;
    }
}

fn gilrs_button_to_index(btn: gilrs::Button) -> u8 {
    use gilrs::Button as B;
    match btn {
        B::South => 0,
        B::East => 1,
        B::West => 2,
        B::North => 3,
        B::LeftTrigger => 4,
        B::RightTrigger => 5,
        B::LeftTrigger2 => 6,
        B::RightTrigger2 => 7,
        B::Select => 8,
        B::Start => 9,
        B::LeftThumb => 10,
        B::RightThumb => 11,
        B::DPadUp => 12,
        B::DPadDown => 13,
        B::DPadLeft => 14,
        B::DPadRight => 15,
        B::Mode => 16,
        _ => 255,
    }
}

fn gilrs_axis_to_index(axis: gilrs::Axis) -> u8 {
    use gilrs::Axis as A;
    match axis {
        A::LeftStickX => 0,
        A::LeftStickY => 1,
        A::RightStickX => 2,
        A::RightStickY => 3,
        A::LeftZ => 4,
        A::RightZ => 5,
        A::DPadX => 6,
        A::DPadY => 7,
        _ => 255,
    }
}

fn gilrs_id_to_usize(id: gilrs::GamepadId) -> usize {
    let s = format!("{:?}", id);
    s.bytes()
        .fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize))
}

// ---------------------------------------------------------------------------
// XInput engine (Windows only). Falls back to a no-op on other platforms.
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
async fn xinput_loop() {
    use rusty_xinput::{XInputHandle, XInputUsageError};

    let handle = match XInputHandle::load_default() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[gamepad/xinput] load_default failed: {:?}", e);
            log_warn!("[gamepad/xinput] load_default failed: {:?}", e);
            return;
        }
    };

    eprintln!("[gamepad/xinput] initialized, polling all 4 user indices");
    log_info!("[gamepad/xinput] initialized");

    // Per-user-index state: previous button bitmask + previous axis values.
    let mut prev_buttons: [u16; 4] = [0; 4];
    let mut prev_axes: [[f32; 6]; 4] = [[0.0; 6]; 4];
    let mut last_diag = std::time::Instant::now();
    let mut last_rate_tick = std::time::Instant::now();
    let mut connected_seen = [false; 4];

    loop {
        for user_index in 0..4u32 {
            let state = match handle.get_state(user_index) {
                Ok(s) => s,
                Err(XInputUsageError::DeviceNotConnected) => {
                    if connected_seen[user_index as usize] {
                        eprintln!("[gamepad/xinput] user {} disconnected", user_index);
                        log_info!("[gamepad/xinput] user {} disconnected", user_index);
                        connected_seen[user_index as usize] = false;
                        prev_buttons[user_index as usize] = 0;
                        prev_axes[user_index as usize] = [0.0; 6];
                        let live = connected_seen.iter().filter(|c| **c).count();
                        update_connected_count(live);
                    }
                    continue;
                }
                Err(e) => {
                    eprintln!("[gamepad/xinput] user {} error: {:?}", user_index, e);
                    continue;
                }
            };

            if !connected_seen[user_index as usize] {
                eprintln!("[gamepad/xinput] user {} connected", user_index);
                log_info!("[gamepad/xinput] user {} connected", user_index);
                connected_seen[user_index as usize] = true;
                let live = connected_seen.iter().filter(|c| **c).count();
                update_connected_count(live);
            }

            let raw = state.raw.Gamepad;
            let buttons = raw.wButtons;
            let prev = prev_buttons[user_index as usize];

            // Detect button transitions (both directions) for each XInput button bit.
            for (bit, idx) in XINPUT_BUTTON_MAP {
                let was = prev & bit != 0;
                let now = buttons & bit != 0;
                if !was && now {
                    eprintln!(
                        "[gamepad/xinput] user {} press bit=0x{:04x} index={}",
                        user_index, bit, idx
                    );
                    on_button_press(*idx).await;
                } else if was && !now {
                    on_button_release(*idx).await;
                }
            }
            prev_buttons[user_index as usize] = buttons;

            // Sticks: i16 → -1.0..1.0. Y axes inverted to match standard Gamepad API
            // (positive = down in browser convention, opposite of XInput).
            let lx = raw.sThumbLX as f32 / 32767.0;
            let ly = -(raw.sThumbLY as f32 / 32767.0);
            let rx = raw.sThumbRX as f32 / 32767.0;
            let ry = -(raw.sThumbRY as f32 / 32767.0);
            // Triggers: u8 → 0.0..1.0
            let lt = raw.bLeftTrigger as f32 / 255.0;
            let rt = raw.bRightTrigger as f32 / 255.0;
            let new_axes = [lx, ly, rx, ry, lt, rt];

            for (i, &v) in new_axes.iter().enumerate() {
                let prev_v = prev_axes[user_index as usize][i];
                if (prev_v - v).abs() > 0.001 {
                    on_axis_change(i as u8, prev_v as f64, v as f64).await;
                    prev_axes[user_index as usize][i] = v;
                }
            }

            if last_diag.elapsed() > Duration::from_secs(2) {
                eprintln!(
                    "[gamepad/xinput] poll user={} buttons=0x{:04x} L=({:.2},{:.2}) R=({:.2},{:.2}) LT={:.2} RT={:.2}",
                    user_index, buttons, lx, ly, rx, ry, lt, rt
                );
            }
        }

        if last_diag.elapsed() > Duration::from_secs(2) {
            last_diag = std::time::Instant::now();
        }

        if last_rate_tick.elapsed() >= Duration::from_millis(RATE_TICK_MS) {
            last_rate_tick = std::time::Instant::now();
            emit_continuous_axis_actions().await;
            let g = crate::settings::get_settings().await.general;
            emit_repeat_button_actions(
                g.gamepad_button_repeat_delay_ms as u64,
                g.gamepad_button_repeat_interval_ms as u64,
            )
            .await;
            feed_gamepad_axes_to_processing().await;
        }

        sleep(Duration::from_millis(8)).await;
    }
}

#[cfg(not(target_os = "windows"))]
async fn xinput_loop() {
    eprintln!("[gamepad/xinput] not supported on this platform, engine disabled");
    log_warn!("[gamepad/xinput] not supported on this platform");
}

/// XInput button bitmask → standard Gamepad API index.
/// Reference: <https://learn.microsoft.com/en-us/windows/win32/api/xinput/ns-xinput-xinput_gamepad>
const XINPUT_BUTTON_MAP: &[(u16, u8)] = &[
    (0x1000, 0),  // A          → 0
    (0x2000, 1),  // B          → 1
    (0x4000, 2),  // X          → 2
    (0x8000, 3),  // Y          → 3
    (0x0100, 4),  // LB         → 4
    (0x0200, 5),  // RB         → 5
    // 6 (LT) / 7 (RT) are analog only — handled as axes 4/5
    (0x0020, 8),  // Back/View  → 8
    (0x0010, 9),  // Start/Menu → 9
    (0x0040, 10), // L stick    → 10
    (0x0080, 11), // R stick    → 11
    (0x0001, 12), // DPad up    → 12
    (0x0002, 13), // DPad down  → 13
    (0x0004, 14), // DPad left  → 14
    (0x0008, 15), // DPad right → 15
];
