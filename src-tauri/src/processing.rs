//! Processing Engine Module
//!
//! Handles T-Code command processing and intensity management.
//! Extracted from websocket.rs to support multiple input sources (WebSocket, Serial).

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;

// Import types from modulation module (single source of truth)
use crate::modulation::{apply_curve, AxisState, ChannelConfig, NoInputBehavior};

// Import Buttplug types for link configuration and pipeline
use crate::buttplug::{process_buttplug_pipeline, ButtplugChannelState, ButtplugLinkConfig};
use std::time::Instant;

lazy_static::lazy_static! {
    static ref TCODE_REGEX: Regex = Regex::new(r"(?:([LRVAD])(\d)([^\s]*))+").unwrap();
    static ref POSITION_REGEX: Regex = Regex::new(r"^(\d+)(?:I|$)").unwrap();
    static ref INTERVAL_REGEX: Regex = Regex::new(r"I(\d+)").unwrap();
}

// ============================================================================
// Processing Engine Type
// ============================================================================

/// Processing engine type - determines how input is processed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessingEngineType {
    /// Original queue-based implementation
    #[default]
    V1,
    /// State-based with smooth (averaging) downsampling
    V2Smooth,
    /// State-based with balanced (linear interpolation) downsampling
    V2Balanced,
    /// State-based with detailed (peak-preserving) downsampling
    V2Detailed,
    /// State-based with dynamic (oscillation-preserving) downsampling
    V2Dynamic,
    /// Lookahead-based with 1s buffer for smooth ramps
    V3Predictive,
}

impl ProcessingEngineType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "v1" => Self::V1,
            "v2-smooth" => Self::V2Smooth,
            "v2-balanced" => Self::V2Balanced,
            "v2-detailed" => Self::V2Detailed,
            "v2-dynamic" => Self::V2Dynamic,
            "v3-predictive" => Self::V3Predictive,
            _ => Self::V1,
        }
    }
}

// ============================================================================
// Channel Interplay
// ============================================================================

/// Channel interplay mode - determines how channels A and B interact
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelInterplay {
    /// Independent channels - no interaction
    #[default]
    None,
    /// B mirrors A (same values)
    Mirror,
    /// B mirrors A inverted (200 - A)
    MirrorInverted,
    /// B follows A with delay (movement sensation)
    Chase,
    /// B follows A inverted with delay (ripple effect)
    ChaseInverted,
    /// A and B alternate within each 100ms window (ping-pong)
    Alternating,
}

impl ChannelInterplay {
    pub fn from_str(s: &str) -> Self {
        match s {
            "none" => Self::None,
            "mirror" => Self::Mirror,
            "mirror-inverted" => Self::MirrorInverted,
            "chase" => Self::Chase,
            "chase-inverted" => Self::ChaseInverted,
            "alternating" => Self::Alternating,
            _ => Self::None,
        }
    }

    /// Check if this interplay mode derives B from A
    pub fn derives_b_from_a(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Apply channel interplay pattern to raw values
/// a_history: History of A values (most recent at end), each slot is 25ms
/// delay_ms: Delay in milliseconds for chase modes
/// Returns (values_a, values_b) after applying the interplay pattern
pub fn apply_interplay(
    values_a: [u8; 4],
    values_b: [u8; 4],
    interplay: ChannelInterplay,
    a_history: &VecDeque<u8>,
    delay_ms: u32,
) -> ([u8; 4], [u8; 4]) {
    match interplay {
        ChannelInterplay::None => {
            // Independent channels - no modification
            (values_a, values_b)
        }
        ChannelInterplay::Mirror => {
            // B mirrors A exactly
            (values_a, values_a)
        }
        ChannelInterplay::MirrorInverted => {
            // B is inverse of A
            let inverted = [
                200u8.saturating_sub(values_a[0]),
                200u8.saturating_sub(values_a[1]),
                200u8.saturating_sub(values_a[2]),
                200u8.saturating_sub(values_a[3]),
            ];
            (values_a, inverted)
        }
        ChannelInterplay::Alternating => {
            // Ping-pong: A gets slots 0,2 and B gets slots 1,3
            let new_a = [values_a[0], 0, values_a[2], 0];
            let new_b = [0, values_a[1], 0, values_a[3]];
            (new_a, new_b)
        }
        ChannelInterplay::Chase => {
            // B follows A with configurable delay
            let new_b = get_delayed_values(a_history, delay_ms, false);
            (values_a, new_b)
        }
        ChannelInterplay::ChaseInverted => {
            // B follows A inverted with configurable delay
            let new_b = get_delayed_values(a_history, delay_ms, true);
            (values_a, new_b)
        }
    }
}

/// Get 4 values from history with the specified delay
/// delay_ms: How far back to look (50-500ms)
/// invert: Whether to invert the values (200 - value)
fn get_delayed_values(a_history: &VecDeque<u8>, delay_ms: u32, invert: bool) -> [u8; 4] {
    // Convert delay to number of 25ms slots
    let delay_slots = (delay_ms / 25) as usize;

    // History has most recent values at the end
    // We want values from delay_slots ago
    let history_len = a_history.len();

    let mut result = [0u8; 4];
    for i in 0..4 {
        // For slot i, we want the value from (delay_slots - i) positions back from current
        // But history doesn't include current values yet, so we offset
        let slots_back = delay_slots.saturating_sub(i);

        let value = if slots_back <= history_len {
            // Index from end: history_len - slots_back
            a_history
                .get(history_len - slots_back)
                .copied()
                .unwrap_or(0)
        } else {
            0 // Not enough history yet
        };

        result[i] = if invert {
            200u8.saturating_sub(value)
        } else {
            value
        };
    }

    result
}

// ============================================================================
// T-Code Parsing
// ============================================================================

/// Parsed T-Code command
#[derive(Debug, Clone, Serialize)]
pub struct TCodeCommand {
    pub axis: String,
    pub value: f64,               // Normalized 0.0-1.0
    pub interval_ms: Option<u32>, // Ramp time in ms
    pub received_at: u64,         // Timestamp when received
}

/// Parse T-Code string into commands
pub fn parse_tcode(input: &str) -> Vec<TCodeCommand> {
    let mut commands = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Match patterns like L0500, R2750I1000, etc.
    for cap in TCODE_REGEX.captures_iter(input) {
        if let (Some(axis_type), Some(axis_num), Some(value_str)) =
            (cap.get(1), cap.get(2), cap.get(3))
        {
            let axis = format!("{}{}", axis_type.as_str(), axis_num.as_str());
            let value_part = value_str.as_str();

            // Extract position value
            if let Some(pos_cap) = POSITION_REGEX.captures(value_part) {
                if let Some(pos_match) = pos_cap.get(1) {
                    let pos_str = pos_match.as_str();
                    if let Ok(pos_val) = pos_str.parse::<u32>() {
                        // Normalize based on digit count (e.g., 500 out of 1000 = 0.5)
                        // Direct mapping: L00000 = 0% (off), L09999 = 100% (max)
                        let max_val = 10u32.pow(pos_str.len() as u32);
                        let normalized = pos_val as f64 / max_val as f64;

                        // Extract interval if present
                        let interval_ms = INTERVAL_REGEX
                            .captures(value_part)
                            .and_then(|c| c.get(1))
                            .and_then(|m| m.as_str().parse::<u32>().ok());

                        commands.push(TCodeCommand {
                            axis,
                            value: normalized.clamp(0.0, 1.0),
                            interval_ms,
                            received_at: now,
                        });
                    }
                }
            }
        }
    }

    commands
}

// ============================================================================
// V2 Channel State (Interpolation-based)
// ============================================================================

/// V2 Channel state using interpolation instead of queues
#[derive(Debug, Clone)]
pub struct V2ChannelState {
    /// Current intensity value (0-200)
    pub current_value: u8,
    /// Target intensity value (0-200)
    pub target_value: u8,
    /// Value when ramp started
    pub ramp_start_value: u8,
    /// Timestamp when ramp started (ms)
    pub ramp_start_time: u64,
    /// Timestamp when ramp should complete (ms)
    pub ramp_end_time: u64,
}

impl Default for V2ChannelState {
    fn default() -> Self {
        Self {
            current_value: 0,
            target_value: 0,
            ramp_start_value: 0,
            ramp_start_time: 0,
            ramp_end_time: 0,
        }
    }
}

impl V2ChannelState {
    /// Set a new target value with optional ramp duration
    pub fn set_target(&mut self, target: u8, duration_ms: u32, timestamp: u64) {
        let clamped_target = target.min(200);

        // Get current interpolated value as the new start point
        self.ramp_start_value = self.get_value_at(timestamp);
        self.target_value = clamped_target;
        self.ramp_start_time = timestamp;
        self.ramp_end_time = timestamp + duration_ms as u64;

        // Update current value for instant commands
        if duration_ms == 0 {
            self.current_value = clamped_target;
        }
    }

    /// Get the interpolated value at a specific timestamp
    pub fn get_value_at(&self, timestamp: u64) -> u8 {
        // If ramp is complete, return target
        if timestamp >= self.ramp_end_time {
            return self.target_value;
        }

        // If before ramp started
        if timestamp <= self.ramp_start_time {
            return self.ramp_start_value;
        }

        // Linear interpolation
        let total_duration = self.ramp_end_time - self.ramp_start_time;
        if total_duration == 0 {
            return self.target_value;
        }

        let elapsed = timestamp - self.ramp_start_time;
        let progress = (elapsed as f64) / (total_duration as f64);
        let progress = progress.min(1.0);

        let start = self.ramp_start_value as f64;
        let end = self.target_value as f64;
        let interpolated = start + progress * (end - start);

        interpolated.round().clamp(0.0, 200.0) as u8
    }

    /// Generate 4 values for the next 100ms window
    pub fn get_next_four_values(&self, window_start: u64) -> [u8; 4] {
        [
            self.get_value_at(window_start),
            self.get_value_at(window_start + 25),
            self.get_value_at(window_start + 50),
            self.get_value_at(window_start + 75),
        ]
    }

    /// Reset to zero
    pub fn reset(&mut self) {
        let now = current_time_ms();
        self.current_value = 0;
        self.target_value = 0;
        self.ramp_start_value = 0;
        self.ramp_start_time = now;
        self.ramp_end_time = now;
    }
}

// ============================================================================
// V3 Channel State (Predictive/Lookahead-based)
// ============================================================================

/// A buffered command with timing info for V3 predictive processing
#[derive(Debug, Clone, Copy)]
pub struct BufferedCommand {
    /// When this command was received (real time)
    pub received_at: u64,
    /// When this command should take effect (playback time)
    pub effective_at: u64,
    /// Target position (0-200)
    pub position: u8,
}

/// A critical point (peak or valley) in the signal
#[derive(Debug, Clone, Copy)]
struct CriticalPoint {
    /// When this critical point occurs
    time: u64,
    /// Position at this point (0-200)
    position: u8,
    /// True if this is a peak (local maximum), false if valley (local minimum)
    is_peak: bool,
}

/// V3 Predictive channel state - buffers commands for lookahead interpolation
///
/// The key insight is that MFP sends instant positions without ramp durations.
/// By buffering commands and treating them as "effective 1s from now", we can
/// look ahead and generate smooth ramps between positions instead of square waves.
#[derive(Debug, Clone)]
pub struct V3ChannelState {
    /// Buffered commands sorted by effective time
    command_buffer: VecDeque<BufferedCommand>,
    /// Current position (0-200) - tracks what we last output
    current_position: u8,
    /// How far ahead commands arrive (ms) - default 1000ms
    lookahead_ms: u64,
    /// How long to keep commands in buffer (ms) - default 2000ms
    buffer_retention_ms: u64,
}

impl Default for V3ChannelState {
    fn default() -> Self {
        Self {
            command_buffer: VecDeque::with_capacity(100),
            current_position: 0,
            lookahead_ms: 1000,        // Commands take effect 1s after arrival
            buffer_retention_ms: 2000, // Keep 2s of command history
        }
    }
}

impl V3ChannelState {
    /// Buffer a new command
    ///
    /// Commands are buffered with an effective time = received_at + lookahead_ms.
    /// This allows us to know where we're going BEFORE we need to output.
    pub fn buffer_command(&mut self, position: u8, received_at: u64) {
        let effective_at = received_at + self.lookahead_ms;

        let cmd = BufferedCommand {
            received_at,
            effective_at,
            position: position.min(200),
        };

        // Insert in sorted order by effective time
        let insert_pos = self
            .command_buffer
            .iter()
            .position(|c| c.effective_at > effective_at)
            .unwrap_or(self.command_buffer.len());

        self.command_buffer.insert(insert_pos, cmd);

        // Prune old commands
        self.prune_buffer(received_at);
    }

    /// Remove old commands from buffer
    fn prune_buffer(&mut self, now: u64) {
        let cutoff = now.saturating_sub(self.buffer_retention_ms);
        while let Some(front) = self.command_buffer.front() {
            if front.received_at < cutoff {
                self.command_buffer.pop_front();
            } else {
                break;
            }
        }
    }

    /// Generate 4 values for the next 100ms window using peak-aware lookahead
    ///
    /// This algorithm looks at a wider window to identify peaks and valleys,
    /// then intelligently assigns the 4 output slots to preserve these critical points.
    pub fn get_next_four_values(&mut self, window_start: u64) -> [u8; 4] {
        let window_end = window_start + 100;

        // Look at a wider analysis window to understand the signal shape
        // Include 100ms before (for context) and 150ms after (for lookahead)
        let analysis_start = window_start.saturating_sub(100);
        let analysis_end = window_start + 250;

        // Get all commands in the analysis window, sorted by effective time
        let commands: Vec<_> = self
            .command_buffer
            .iter()
            .filter(|c| c.effective_at >= analysis_start && c.effective_at <= analysis_end)
            .cloned()
            .collect();

        // If no commands, hold current position
        if commands.is_empty() {
            return [
                self.current_position,
                self.current_position,
                self.current_position,
                self.current_position,
            ];
        }

        // Find critical points (peaks and valleys where direction changes)
        let critical_points = self.find_critical_points(&commands, window_start);

        // Generate output for each 25ms slot
        let mut result = [0u8; 4];
        let slot_times = [
            window_start,
            window_start + 25,
            window_start + 50,
            window_start + 75,
        ];

        // For each slot, find the best value
        for (slot, &slot_time) in slot_times.iter().enumerate() {
            let slot_center = slot_time + 12; // Center of the 25ms slot

            // Check if any critical point should be captured by this slot
            // A critical point "belongs" to a slot if it's the closest slot to that point
            let captured_critical = critical_points.iter().find(|cp| {
                // Critical point is within our output window
                if cp.time < window_start || cp.time >= window_end {
                    return false;
                }
                // This slot is the best match for this critical point
                let slot_for_cp = ((cp.time - window_start) / 25).min(3) as usize;
                slot_for_cp == slot
            });

            if let Some(cp) = captured_critical {
                // This slot captures a critical point - use its value
                result[slot] = cp.position;
            } else {
                // Interpolate based on surrounding critical points
                result[slot] = self.interpolate_at_time(&critical_points, &commands, slot_center);
            }
        }

        // Update state
        self.current_position = result[3];

        result
    }

    /// Find critical points (peaks and valleys) in the command sequence
    fn find_critical_points(
        &self,
        commands: &[BufferedCommand],
        window_start: u64,
    ) -> Vec<CriticalPoint> {
        let mut critical_points = Vec::new();

        if commands.len() < 2 {
            // With 0 or 1 commands, just return the command as a critical point
            if let Some(cmd) = commands.first() {
                critical_points.push(CriticalPoint {
                    time: cmd.effective_at,
                    position: cmd.position,
                    is_peak: true, // Doesn't matter for single point
                });
            }
            return critical_points;
        }

        // Add the starting position as a reference point
        let start_pos = self.find_position_at(window_start);
        critical_points.push(CriticalPoint {
            time: window_start,
            position: start_pos,
            is_peak: false,
        });

        // Find direction changes
        let mut prev_direction: Option<i16> = None;

        for i in 0..commands.len() {
            let curr = &commands[i];

            // Determine direction from previous point
            let prev_pos = if i == 0 {
                start_pos
            } else {
                commands[i - 1].position
            };

            let direction = (curr.position as i16) - (prev_pos as i16);

            if let Some(prev_dir) = prev_direction {
                // Check for direction change (peak or valley)
                if (prev_dir > 0 && direction < 0) || (prev_dir < 0 && direction > 0) {
                    // The previous point was a critical point
                    if i > 0 {
                        let prev_cmd = &commands[i - 1];
                        critical_points.push(CriticalPoint {
                            time: prev_cmd.effective_at,
                            position: prev_cmd.position,
                            is_peak: prev_dir > 0,
                        });
                    }
                }
            }

            if direction != 0 {
                prev_direction = Some(direction);
            }
        }

        // Add the last command as a reference point
        if let Some(last) = commands.last() {
            critical_points.push(CriticalPoint {
                time: last.effective_at,
                position: last.position,
                is_peak: false,
            });
        }

        critical_points
    }

    /// Interpolate position at a specific time using critical points
    fn interpolate_at_time(
        &self,
        critical_points: &[CriticalPoint],
        commands: &[BufferedCommand],
        time: u64,
    ) -> u8 {
        // Find the critical points before and after this time
        let mut before: Option<&CriticalPoint> = None;
        let mut after: Option<&CriticalPoint> = None;

        for cp in critical_points {
            if cp.time <= time {
                before = Some(cp);
            } else if after.is_none() {
                after = Some(cp);
                break;
            }
        }

        match (before, after) {
            (Some(b), Some(a)) => {
                // Interpolate between the two points
                let duration = (a.time - b.time) as f64;
                if duration <= 0.0 {
                    return a.position;
                }
                let elapsed = (time - b.time) as f64;
                let progress = (elapsed / duration).clamp(0.0, 1.0);
                let interpolated =
                    b.position as f64 + progress * (a.position as f64 - b.position as f64);
                interpolated.round().clamp(0.0, 200.0) as u8
            }
            (Some(b), None) => b.position,
            (None, Some(a)) => a.position,
            (None, None) => {
                // Fallback to commands directly
                commands
                    .last()
                    .map(|c| c.position)
                    .unwrap_or(self.current_position)
            }
        }
    }

    /// Find the position that should be active at a given timestamp
    fn find_position_at(&self, timestamp: u64) -> u8 {
        // Find the last command with effective_at <= timestamp
        let mut position = self.current_position;

        for cmd in self.command_buffer.iter().rev() {
            if cmd.effective_at <= timestamp {
                position = cmd.position;
                break;
            }
        }

        position
    }

    /// Get buffer status (for debugging)
    pub fn buffer_size(&self) -> usize {
        self.command_buffer.len()
    }

    /// Reset to zero
    pub fn reset(&mut self) {
        self.command_buffer.clear();
        self.current_position = 0;
    }
}

// ============================================================================
// V2 Downsampler
// ============================================================================

/// A timestamped sample
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub timestamp: u64,
    pub value: u8,
}

/// Downsampler for handling high-rate input
#[derive(Debug)]
pub struct Downsampler {
    samples: VecDeque<Sample>,
    last_value: u8,
    buffer_duration_ms: u64,
}

impl Default for Downsampler {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            last_value: 0,
            buffer_duration_ms: 200, // Keep 200ms of history
        }
    }
}

impl Downsampler {
    /// Add a sample to the buffer
    pub fn add_sample(&mut self, value: u8, timestamp: u64) {
        self.samples.push_back(Sample { timestamp, value });
        self.last_value = value;
        self.prune(timestamp);
    }

    /// Remove old samples from the buffer
    fn prune(&mut self, current_time: u64) {
        let cutoff = current_time.saturating_sub(self.buffer_duration_ms);
        while let Some(sample) = self.samples.front() {
            if sample.timestamp < cutoff {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Check if we have samples in a given time window
    pub fn has_samples_in_window(&self, start_time: u64, end_time: u64) -> bool {
        self.samples
            .iter()
            .any(|s| s.timestamp >= start_time && s.timestamp < end_time)
    }

    /// Downsample to 4 values using smooth (averaging) algorithm
    pub fn downsample_smooth(&self, window_start: u64, window_end: u64) -> [u8; 4] {
        let window_samples: Vec<_> = self
            .samples
            .iter()
            .filter(|s| s.timestamp >= window_start && s.timestamp < window_end)
            .copied()
            .collect();

        if window_samples.is_empty() {
            return [self.last_value; 4];
        }

        if window_samples.len() == 1 {
            return [window_samples[0].value; 4];
        }

        let bucket_duration = (window_end - window_start) / 4;
        let mut result = [0u8; 4];

        for i in 0..4 {
            let bucket_start = window_start + i as u64 * bucket_duration;
            let bucket_end = bucket_start + bucket_duration;

            let bucket_samples: Vec<_> = window_samples
                .iter()
                .filter(|s| s.timestamp >= bucket_start && s.timestamp < bucket_end)
                .collect();

            if bucket_samples.is_empty() {
                // Use previous bucket's value or last known
                result[i] = if i > 0 {
                    result[i - 1]
                } else {
                    self.last_value
                };
            } else {
                // Average all samples in bucket
                let sum: u32 = bucket_samples.iter().map(|s| s.value as u32).sum();
                result[i] = (sum / bucket_samples.len() as u32) as u8;
            }
        }

        result
    }

    /// Downsample to 4 values using balanced (linear interpolation) algorithm
    pub fn downsample_balanced(&self, window_start: u64, window_end: u64) -> [u8; 4] {
        let mut window_samples: Vec<_> = self
            .samples
            .iter()
            .filter(|s| s.timestamp >= window_start && s.timestamp < window_end)
            .copied()
            .collect();

        if window_samples.is_empty() {
            return [self.last_value; 4];
        }

        if window_samples.len() == 1 {
            return [window_samples[0].value; 4];
        }

        // Sort by timestamp
        window_samples.sort_by_key(|s| s.timestamp);

        let bucket_duration = (window_end - window_start) / 4;
        let mut result = [0u8; 4];

        for i in 0..4 {
            let target_time = window_start + i as u64 * bucket_duration + bucket_duration / 2;
            result[i] = self.interpolate_at(&window_samples, target_time);
        }

        result
    }

    /// Downsample to 4 values using detailed (peak-preserving) algorithm
    pub fn downsample_detailed(&self, window_start: u64, window_end: u64) -> [u8; 4] {
        let window_samples: Vec<_> = self
            .samples
            .iter()
            .filter(|s| s.timestamp >= window_start && s.timestamp < window_end)
            .copied()
            .collect();

        if window_samples.is_empty() {
            return [self.last_value; 4];
        }

        if window_samples.len() == 1 {
            return [window_samples[0].value; 4];
        }

        let bucket_duration = (window_end - window_start) / 4;
        let mut result = [0u8; 4];

        for i in 0..4 {
            let bucket_start = window_start + i as u64 * bucket_duration;
            let bucket_end = bucket_start + bucket_duration;

            let bucket_samples: Vec<_> = window_samples
                .iter()
                .filter(|s| s.timestamp >= bucket_start && s.timestamp < bucket_end)
                .collect();

            if bucket_samples.is_empty() {
                // Use previous bucket's value or last known
                result[i] = if i > 0 {
                    result[i - 1]
                } else {
                    self.last_value
                };
            } else {
                // Take maximum value in bucket (preserves peaks)
                result[i] = bucket_samples.iter().map(|s| s.value).max().unwrap_or(0);
            }
        }

        result
    }

    /// Linear interpolation at a specific timestamp
    fn interpolate_at(&self, sorted_samples: &[Sample], target_time: u64) -> u8 {
        if sorted_samples.is_empty() {
            return self.last_value;
        }

        if sorted_samples.len() == 1 {
            return sorted_samples[0].value;
        }

        // Find surrounding samples
        let mut before = sorted_samples[0];
        let mut after = sorted_samples[sorted_samples.len() - 1];

        for i in 0..sorted_samples.len() - 1 {
            if sorted_samples[i].timestamp <= target_time
                && sorted_samples[i + 1].timestamp >= target_time
            {
                before = sorted_samples[i];
                after = sorted_samples[i + 1];
                break;
            }
        }

        // If target is before all samples
        if target_time <= before.timestamp {
            return before.value;
        }

        // If target is after all samples
        if target_time >= after.timestamp {
            return after.value;
        }

        // Linear interpolation
        let time_delta = after.timestamp - before.timestamp;
        if time_delta == 0 {
            return before.value;
        }

        let progress = (target_time - before.timestamp) as f64 / time_delta as f64;
        let interpolated =
            before.value as f64 + progress * (after.value as f64 - before.value as f64);

        interpolated.round().clamp(0.0, 200.0) as u8
    }

    /// Downsample to 4 values using dynamic (oscillation-preserving) algorithm
    /// When significant oscillation is detected, outputs alternating min/max to preserve movement
    pub fn downsample_dynamic(&self, window_start: u64, window_end: u64) -> [u8; 4] {
        let window_samples: Vec<_> = self
            .samples
            .iter()
            .filter(|s| s.timestamp >= window_start && s.timestamp < window_end)
            .copied()
            .collect();

        if window_samples.is_empty() {
            return [self.last_value; 4];
        }

        if window_samples.len() == 1 {
            return [window_samples[0].value; 4];
        }

        // Calculate window min/max
        let window_min = window_samples.iter().map(|s| s.value).min().unwrap_or(0);
        let window_max = window_samples.iter().map(|s| s.value).max().unwrap_or(0);
        let range = window_max.saturating_sub(window_min);

        // Threshold: if range is significant (>20% of full scale), preserve oscillation
        const OSCILLATION_THRESHOLD: u8 = 40; // 20% of 200

        if range < OSCILLATION_THRESHOLD {
            // Not enough variation - fall back to detailed (peak-preserving)
            return self.downsample_detailed(window_start, window_end);
        }

        // Significant oscillation detected - analyze per-bucket behavior
        let bucket_duration = (window_end - window_start) / 4;
        let mut result = [0u8; 4];

        // Track direction to create alternating pattern
        let midpoint = (window_min as u16 + window_max as u16) / 2;
        let mut expecting_high = (self.last_value as u16) <= midpoint;

        for i in 0..4 {
            let bucket_start = window_start + i as u64 * bucket_duration;
            let bucket_end = bucket_start + bucket_duration;

            let bucket_samples: Vec<_> = window_samples
                .iter()
                .filter(|s| s.timestamp >= bucket_start && s.timestamp < bucket_end)
                .copied()
                .collect();

            if bucket_samples.is_empty() {
                // No samples in bucket - alternate based on expectation
                result[i] = if expecting_high {
                    window_max
                } else {
                    window_min
                };
                expecting_high = !expecting_high;
                continue;
            }

            let bucket_min = bucket_samples.iter().map(|s| s.value).min().unwrap_or(0);
            let bucket_max = bucket_samples.iter().map(|s| s.value).max().unwrap_or(0);
            let bucket_range = bucket_max.saturating_sub(bucket_min);

            if bucket_range > OSCILLATION_THRESHOLD / 2 {
                // This bucket has its own oscillation - check for direction changes
                let first_value = bucket_samples[0].value;
                let last_value = bucket_samples[bucket_samples.len() - 1].value;
                let has_direction_change = self.count_direction_changes(&bucket_samples) > 0;

                if has_direction_change {
                    // Multiple direction changes - output extreme matching expectation
                    result[i] = if expecting_high {
                        bucket_max
                    } else {
                        bucket_min
                    };
                    expecting_high = !expecting_high;
                } else {
                    // Single direction - output based on trend
                    if last_value > first_value {
                        result[i] = bucket_max;
                        expecting_high = false;
                    } else {
                        result[i] = bucket_min;
                        expecting_high = true;
                    }
                }
            } else {
                // Bucket is relatively stable - use value that maintains pattern
                if expecting_high {
                    result[i] = bucket_max;
                    expecting_high = false;
                } else {
                    result[i] = bucket_min;
                    expecting_high = true;
                }
            }
        }

        result
    }

    /// Count direction changes (peaks/valleys) in a sequence of samples
    fn count_direction_changes(&self, samples: &[Sample]) -> usize {
        if samples.len() < 3 {
            return 0;
        }

        let mut changes = 0;
        let mut last_direction: i8 = 0; // 0 = unknown, 1 = rising, -1 = falling

        for i in 1..samples.len() {
            let delta = samples[i].value as i16 - samples[i - 1].value as i16;
            let direction = if delta > 5 {
                1i8
            } else if delta < -5 {
                -1i8
            } else {
                last_direction
            };

            if last_direction != 0 && direction != 0 && direction != last_direction {
                changes += 1;
            }
            if direction != 0 {
                last_direction = direction;
            }
        }

        changes
    }

    /// Clear all samples
    pub fn clear(&mut self) {
        self.samples.clear();
        self.last_value = 0;
    }
}

// ============================================================================
// V1 Channel State (Queue-based - original implementation)
// ============================================================================

/// V1 Channel state using queue-based ramping (original implementation)
#[derive(Debug, Clone)]
pub struct V1ChannelState {
    pub current_intensity: u8, // 0-200 (device native units)
    pub target_intensity: u8,  // 0-200 (device native units)
    pub ramp_queue: Vec<u8>,   // Queue of intensity values for ramping (0-200)
    pub prev_intensity: u8,    // Last sent intensity (for repeating when queue empty)
}

impl Default for V1ChannelState {
    fn default() -> Self {
        Self {
            current_intensity: 0,
            target_intensity: 0,
            ramp_queue: Vec::new(),
            prev_intensity: 0,
        }
    }
}

impl V1ChannelState {
    /// Apply intensity command with optional ramping (V1 queue-based)
    pub fn apply_command(&mut self, target: f64, interval_ms: Option<u32>) {
        // Convert normalized (0.0-1.0) to device units (0-200)
        let target_u8 = (target * 200.0).round().clamp(0.0, 200.0) as u8;

        if let Some(interval) = interval_ms {
            // Calculate ramp steps (at 25ms intervals like the old implementation)
            let steps = (interval / 25).max(1) as i32;

            // Get current power level from queue or previous value
            let current_pwr = self.prev_intensity as i32;

            // Reduce previous data if queue is getting too long
            if self.ramp_queue.len() > 4 {
                let last = *self.ramp_queue.last().unwrap_or(&self.prev_intensity) as i32;
                let second = self
                    .ramp_queue
                    .get(1)
                    .copied()
                    .unwrap_or(self.prev_intensity) as i32;
                let delta = (last - second) / 2;
                self.ramp_queue = vec![second as u8, (second + delta) as u8, last as u8];
            }

            // Generate list of intensities to output every 100ms ramping towards the target
            let target_pwr = target_u8 as i32;
            let increment = (target_pwr - current_pwr) as f64 / steps as f64;

            for i in 1..=steps {
                let val = (current_pwr as f64 + (i as f64 * increment)).round();
                self.ramp_queue.push(val.clamp(0.0, 200.0) as u8);
            }

            self.target_intensity = target_u8;
        } else {
            // Immediate set - clear queue and set directly
            self.ramp_queue.clear();
            self.ramp_queue.push(target_u8);
            self.current_intensity = target_u8;
            self.target_intensity = target_u8;
        }
    }

    /// Get next 4 values from queue
    pub fn get_next_four(&mut self) -> [u8; 4] {
        let prev = self.prev_intensity;

        // Get next 4 values from queue
        let mut next_four: Vec<u8> = self
            .ramp_queue
            .drain(..self.ramp_queue.len().min(4))
            .collect();

        // Pad to 4 values if needed
        if next_four.is_empty() {
            // No new data - repeat previous value
            next_four = vec![prev, prev, prev, prev];
        } else if next_four.len() < 4 {
            // Pad with last value
            let last = *next_four.last().unwrap();
            while next_four.len() < 4 {
                next_four.push(last);
            }
        }

        // Update prev_intensity to last value sent
        self.prev_intensity = next_four[3];
        self.current_intensity = next_four[3];

        [next_four[0], next_four[1], next_four[2], next_four[3]]
    }

    /// Reset to zero
    pub fn reset(&mut self) {
        self.current_intensity = 0;
        self.target_intensity = 0;
        self.prev_intensity = 0;
        self.ramp_queue.clear();
    }
}

// ============================================================================
// Output Options
// ============================================================================

/// Output options that affect how channels are linked/processed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputOptions {
    pub channel_interplay: ChannelInterplay,
    pub processing_engine: ProcessingEngineType,
    pub chase_delay_ms: u32, // 50-500ms delay for chase modes
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            channel_interplay: ChannelInterplay::None,
            processing_engine: ProcessingEngineType::V1,
            chase_delay_ms: 100, // Default 100ms (1 full window)
        }
    }
}

// ============================================================================
// Waveform Data
// ============================================================================

/// Waveform data for a channel (4 intensity values for the 100ms window)
#[derive(Debug, Clone)]
pub struct WaveformData {
    pub intensity: u8,               // Max intensity (0-200)
    pub waveform_intensity: [u8; 4], // Relative intensity (0-100) for each slot
}

impl WaveformData {
    /// Create waveform data from 4 raw intensity values
    pub fn from_values(values: [u8; 4]) -> Self {
        let max_intensity = *values.iter().max().unwrap_or(&0);

        let waveform_intensity = if max_intensity > 0 {
            [
                ((values[0] as f64 / max_intensity as f64) * 100.0).ceil() as u8,
                ((values[1] as f64 / max_intensity as f64) * 100.0).ceil() as u8,
                ((values[2] as f64 / max_intensity as f64) * 100.0).ceil() as u8,
                ((values[3] as f64 / max_intensity as f64) * 100.0).ceil() as u8,
            ]
        } else {
            [100, 100, 100, 100]
        };

        Self {
            intensity: max_intensity,
            waveform_intensity,
        }
    }
}

// ============================================================================
// Processing State (Main struct that holds everything)
// ============================================================================

/// Main processing state that manages V1, V2, and V3 channel states
pub struct ProcessingState {
    // V1 channel states (queue-based)
    pub v1_channel_a: V1ChannelState,
    pub v1_channel_b: V1ChannelState,

    // V2 channel states (interpolation-based)
    pub v2_channel_a: V2ChannelState,
    pub v2_channel_b: V2ChannelState,

    // V3 channel states (predictive/lookahead-based)
    pub v3_channel_a: V3ChannelState,
    pub v3_channel_b: V3ChannelState,

    // Downsamplers for V2
    pub downsampler_a: Downsampler,
    pub downsampler_b: Downsampler,

    // Output options
    pub options: OutputOptions,

    // History of A values for delay-based interplay modes (chase, chase-inverted)
    // Stores up to 20 slots (500ms at 25ms per slot) as a circular buffer
    // Most recent values are at the end
    a_history: VecDeque<u8>,

    // ===== Parameter Modulation System =====
    // Track ALL T-Code axes (L0-L2, R0-R2, V0-V3, A0-A1)
    pub axis_values: HashMap<String, AxisState>,

    // Track Buttplug feature values (feature_key → value 0.0-1.0)
    // Keys are like "Vibrate_0", "Position_0", "Oscillate_0", etc.
    pub buttplug_features: HashMap<String, f64>,

    // Track LinearCmd (PositionWithDuration) commands: index → (position, duration_ms)
    // These are stored separately because they include duration information
    /// LinearCmd commands with (position, duration_ms, arrival_time)
    pub buttplug_linear_commands: HashMap<usize, (f64, u32, std::time::Instant)>,

    // Track Rotate directions: index → clockwise
    pub buttplug_rotate_directions: HashMap<usize, bool>,

    // Channel parameter configurations
    pub channel_a_config: ChannelConfig,
    pub channel_b_config: ChannelConfig,

    // General settings for parameter modulation
    pub no_input_behavior: NoInputBehavior,
    pub no_input_decay_ms: u32,

    // ===== Buttplug Pipeline State =====
    // Buttplug link configurations per channel (intensity only for now)
    pub buttplug_link_config_a: ButtplugLinkConfig,
    pub buttplug_link_config_b: ButtplugLinkConfig,

    // Buttplug channel processing state (phases, interpolation, etc.)
    pub buttplug_channel_state_a: ButtplugChannelState,
    pub buttplug_channel_state_b: ButtplugChannelState,
}

impl Default for ProcessingState {
    fn default() -> Self {
        Self {
            v1_channel_a: V1ChannelState::default(),
            v1_channel_b: V1ChannelState::default(),
            v2_channel_a: V2ChannelState::default(),
            v2_channel_b: V2ChannelState::default(),
            v3_channel_a: V3ChannelState::default(),
            v3_channel_b: V3ChannelState::default(),
            downsampler_a: Downsampler::default(),
            downsampler_b: Downsampler::default(),
            options: OutputOptions::default(),
            a_history: VecDeque::with_capacity(24), // 24 slots = 600ms of history
            axis_values: HashMap::new(),
            buttplug_features: HashMap::new(),
            buttplug_linear_commands: HashMap::new(),
            buttplug_rotate_directions: HashMap::new(),
            channel_a_config: ChannelConfig::channel_a_default(),
            channel_b_config: ChannelConfig::channel_b_default(),
            no_input_behavior: NoInputBehavior::Hold,
            no_input_decay_ms: 1000, // 1 second default decay time
            buttplug_link_config_a: ButtplugLinkConfig::default(),
            buttplug_link_config_b: ButtplugLinkConfig::default(),
            buttplug_channel_state_a: ButtplugChannelState::default(),
            buttplug_channel_state_b: ButtplugChannelState::default(),
        }
    }
}

impl ProcessingState {
    /// Process a T-Code command
    pub fn process_command(&mut self, cmd: &TCodeCommand) {
        let now = cmd.received_at;

        // Update axis tracking - store ALL axes for parameter modulation
        self.axis_values.insert(
            cmd.axis.clone(),
            AxisState {
                value: cmd.value,
                timestamp: now,
                has_data: true,
            },
        );

        // Check if this axis should affect Channel A intensity
        if self.should_apply_to_channel_a(&cmd.axis) {
            self.apply_to_channel_a(cmd.value, cmd.interval_ms, now);

            // For mirror modes, also update B's state so it has data
            // (the actual interplay pattern is applied at output time)
            match self.options.channel_interplay {
                ChannelInterplay::Mirror => {
                    self.apply_to_channel_b(cmd.value, cmd.interval_ms, now);
                }
                ChannelInterplay::MirrorInverted => {
                    self.apply_to_channel_b(1.0 - cmd.value, cmd.interval_ms, now);
                }
                _ => {
                    // Other interplay modes derive B from A at output time
                }
            }
        }

        // Check if this axis should affect Channel B intensity
        if self.should_apply_to_channel_b(&cmd.axis) {
            self.apply_to_channel_b(cmd.value, cmd.interval_ms, now);
        }
    }

    /// Check if a T-Code axis should control Channel A intensity
    fn should_apply_to_channel_a(&self, axis: &str) -> bool {
        use crate::modulation::ParameterSourceType;

        // Only apply if intensity is linked to this axis
        if self.channel_a_config.intensity.source_type == ParameterSourceType::Linked {
            if let Some(source_axis) = &self.channel_a_config.intensity.source_axis {
                source_axis == axis
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Check if a T-Code axis should control Channel B intensity
    fn should_apply_to_channel_b(&self, axis: &str) -> bool {
        use crate::modulation::ParameterSourceType;

        // Don't apply if channel interplay derives B from A
        if self.options.channel_interplay.derives_b_from_a() {
            return false;
        }

        // Only apply if intensity is linked to this axis
        if self.channel_b_config.intensity.source_type == ParameterSourceType::Linked {
            if let Some(source_axis) = &self.channel_b_config.intensity.source_axis {
                return source_axis == axis;
            }
        }
        false
    }

    /// Apply command to channel A (both V1 and V2 states)
    /// Applies midpoint and curve transformations from channel config
    /// NOTE: Range mapping is NOT applied here - it's done in device.rs scale_intensity()
    /// to avoid double-application of range limits
    fn apply_to_channel_a(&mut self, value: f64, interval_ms: Option<u32>, timestamp: u64) {
        use crate::modulation::apply_midpoint;

        // Apply midpoint transformation if enabled (before curve)
        let midpoint_value = if self.channel_a_config.intensity.midpoint.unwrap_or(false) {
            apply_midpoint(value)
        } else {
            value
        };

        // Apply curve transformation from channel config
        let curve = &self.channel_a_config.intensity.curve;
        let strength = self
            .channel_a_config
            .intensity
            .curve_strength
            .unwrap_or(2.0);
        let curved_value = apply_curve(midpoint_value, curve, strength);

        // Convert to device units (0-200) WITHOUT range mapping
        // Range mapping is applied in device.rs scale_intensity() to avoid double-application
        let intensity_u8 = (curved_value * 200.0).round().clamp(0.0, 200.0) as u8;

        // V1: Queue-based (uses the curved value for its own processing)
        self.v1_channel_a.apply_command(curved_value, interval_ms);

        // V2: Interpolation-based
        self.v2_channel_a
            .set_target(intensity_u8, interval_ms.unwrap_or(0), timestamp);
        self.downsampler_a.add_sample(intensity_u8, timestamp);

        // V3: Predictive/Lookahead-based - buffer command for future processing
        self.v3_channel_a.buffer_command(intensity_u8, timestamp);
    }

    /// Apply command to channel B (both V1 and V2 states)
    /// Applies midpoint and curve transformations from channel config
    /// NOTE: Range mapping is NOT applied here - it's done in device.rs scale_intensity()
    /// to avoid double-application of range limits
    fn apply_to_channel_b(&mut self, value: f64, interval_ms: Option<u32>, timestamp: u64) {
        use crate::modulation::apply_midpoint;

        // Apply midpoint transformation if enabled (before curve)
        let midpoint_value = if self.channel_b_config.intensity.midpoint.unwrap_or(false) {
            apply_midpoint(value)
        } else {
            value
        };

        // Apply curve transformation from channel config
        let curve = &self.channel_b_config.intensity.curve;
        let strength = self
            .channel_b_config
            .intensity
            .curve_strength
            .unwrap_or(2.0);
        let curved_value = apply_curve(midpoint_value, curve, strength);

        // Convert to device units (0-200) WITHOUT range mapping
        // Range mapping is applied in device.rs scale_intensity() to avoid double-application
        let intensity_u8 = (curved_value * 200.0).round().clamp(0.0, 200.0) as u8;

        // V1: Queue-based (uses the curved value for its own processing)
        self.v1_channel_b.apply_command(curved_value, interval_ms);

        // V2: Interpolation-based
        self.v2_channel_b
            .set_target(intensity_u8, interval_ms.unwrap_or(0), timestamp);
        self.downsampler_b.add_sample(intensity_u8, timestamp);

        // V3: Predictive/Lookahead-based - buffer command for future processing
        self.v3_channel_b.buffer_command(intensity_u8, timestamp);
    }

    /// Get the next waveform data for both channels (called at 10Hz)
    pub fn get_next_waveform_data(&mut self) -> (WaveformData, WaveformData) {
        use crate::modulation::ParameterSourceType;

        let now_ms = current_time_ms();
        let window_start = now_ms.saturating_sub(100);
        let now_instant = Instant::now();
        let dt_ms = 100u32; // 10Hz tick rate = 100ms per tick

        // Check if Buttplug input is active and should be processed
        let has_buttplug = self.has_buttplug_input();
        let a_has_bp_links = self.buttplug_link_config_a.has_any_links();
        let b_has_bp_links = self.buttplug_link_config_b.has_any_links();

        // DEBUG: Log Buttplug pipeline status
        if has_buttplug || a_has_bp_links || b_has_bp_links {
            println!("[Buttplug Pipeline] has_buttplug={}, a_has_links={}, b_has_links={}, features={:?}, linear_cmds={:?}, pos_dur_feature_a={:?}",
                has_buttplug, a_has_bp_links, b_has_bp_links,
                self.buttplug_features.keys().collect::<Vec<_>>(),
                self.buttplug_linear_commands.keys().collect::<Vec<_>>(),
                self.buttplug_link_config_a.pos_dur_feature);
        }

        // Process Buttplug pipeline if active
        let bp_intensity_a = if has_buttplug && a_has_bp_links {
            let features = self.get_buttplug_feature_values();
            let output = process_buttplug_pipeline(
                &mut self.buttplug_channel_state_a,
                &features,
                &self.buttplug_link_config_a,
                now_instant,
                dt_ms,
            );
            let intensity = (output * 200.0).round().clamp(0.0, 200.0) as u8;
            println!(
                "[Buttplug Pipeline] Channel A: pipeline_output={:.3}, intensity={}",
                output, intensity
            );
            Some(intensity)
        } else {
            None
        };

        let bp_intensity_b = if has_buttplug && b_has_bp_links {
            let features = self.get_buttplug_feature_values();
            let output = process_buttplug_pipeline(
                &mut self.buttplug_channel_state_b,
                &features,
                &self.buttplug_link_config_b,
                now_instant,
                dt_ms,
            );
            Some((output * 200.0).round().clamp(0.0, 200.0) as u8)
        } else {
            None
        };

        // Clear linear commands after processing (they're one-shot)
        if has_buttplug {
            self.buttplug_linear_commands.clear();
        }

        // Check if intensity sources are static - if so, use static values directly
        let a_is_static =
            self.channel_a_config.intensity.source_type == ParameterSourceType::Static;
        let b_is_static =
            self.channel_b_config.intensity.source_type == ParameterSourceType::Static;

        // Helper to create 4-value array from single intensity
        let intensity_to_values = |i: u8| -> [u8; 4] { [i, i, i, i] };

        // Determine channel A values - Buttplug takes priority when active
        let values_a = if let Some(bp_val) = bp_intensity_a {
            intensity_to_values(bp_val)
        } else if a_is_static {
            let static_val = self.channel_a_config.intensity.static_value.unwrap_or(0.0);
            intensity_to_values(static_val.round().clamp(0.0, 200.0) as u8)
        } else {
            match self.options.processing_engine {
                ProcessingEngineType::V1 => self.v1_channel_a.get_next_four(),
                ProcessingEngineType::V2Smooth => {
                    if self
                        .downsampler_a
                        .has_samples_in_window(window_start, now_ms)
                    {
                        self.downsampler_a.downsample_smooth(window_start, now_ms)
                    } else {
                        self.v2_channel_a.get_next_four_values(window_start)
                    }
                }
                ProcessingEngineType::V2Balanced => {
                    if self
                        .downsampler_a
                        .has_samples_in_window(window_start, now_ms)
                    {
                        self.downsampler_a.downsample_balanced(window_start, now_ms)
                    } else {
                        self.v2_channel_a.get_next_four_values(window_start)
                    }
                }
                ProcessingEngineType::V2Detailed => {
                    if self
                        .downsampler_a
                        .has_samples_in_window(window_start, now_ms)
                    {
                        self.downsampler_a.downsample_detailed(window_start, now_ms)
                    } else {
                        self.v2_channel_a.get_next_four_values(window_start)
                    }
                }
                ProcessingEngineType::V2Dynamic => {
                    if self
                        .downsampler_a
                        .has_samples_in_window(window_start, now_ms)
                    {
                        self.downsampler_a.downsample_dynamic(window_start, now_ms)
                    } else {
                        self.v2_channel_a.get_next_four_values(window_start)
                    }
                }
                ProcessingEngineType::V3Predictive => {
                    self.v3_channel_a.get_next_four_values(now_ms)
                }
            }
        };

        // Determine channel B values - Buttplug takes priority when active
        let values_b = if let Some(bp_val) = bp_intensity_b {
            intensity_to_values(bp_val)
        } else if b_is_static {
            let static_val = self.channel_b_config.intensity.static_value.unwrap_or(0.0);
            intensity_to_values(static_val.round().clamp(0.0, 200.0) as u8)
        } else {
            match self.options.processing_engine {
                ProcessingEngineType::V1 => self.v1_channel_b.get_next_four(),
                ProcessingEngineType::V2Smooth => {
                    if self
                        .downsampler_b
                        .has_samples_in_window(window_start, now_ms)
                    {
                        self.downsampler_b.downsample_smooth(window_start, now_ms)
                    } else {
                        self.v2_channel_b.get_next_four_values(window_start)
                    }
                }
                ProcessingEngineType::V2Balanced => {
                    if self
                        .downsampler_b
                        .has_samples_in_window(window_start, now_ms)
                    {
                        self.downsampler_b.downsample_balanced(window_start, now_ms)
                    } else {
                        self.v2_channel_b.get_next_four_values(window_start)
                    }
                }
                ProcessingEngineType::V2Detailed => {
                    if self
                        .downsampler_b
                        .has_samples_in_window(window_start, now_ms)
                    {
                        self.downsampler_b.downsample_detailed(window_start, now_ms)
                    } else {
                        self.v2_channel_b.get_next_four_values(window_start)
                    }
                }
                ProcessingEngineType::V2Dynamic => {
                    if self
                        .downsampler_b
                        .has_samples_in_window(window_start, now_ms)
                    {
                        self.downsampler_b.downsample_dynamic(window_start, now_ms)
                    } else {
                        self.v2_channel_b.get_next_four_values(window_start)
                    }
                }
                ProcessingEngineType::V3Predictive => {
                    self.v3_channel_b.get_next_four_values(now_ms)
                }
            }
        };

        // Apply channel interplay pattern (using history for delay-based modes)
        let (final_a, final_b) = apply_interplay(
            values_a,
            values_b,
            self.options.channel_interplay,
            &self.a_history,
            self.options.chase_delay_ms,
        );

        // Add current A values to history (for chase modes)
        // Keep history limited to 24 slots (600ms)
        for &val in &values_a {
            self.a_history.push_back(val);
        }
        while self.a_history.len() > 24 {
            self.a_history.pop_front();
        }

        (
            WaveformData::from_values(final_a),
            WaveformData::from_values(final_b),
        )
    }

    /// Get V2 raw values with support for static intensity sources
    fn get_v2_raw_values_with_static<F>(
        &self,
        window_start: u64,
        window_end: u64,
        a_is_static: bool,
        b_is_static: bool,
        downsample_fn: F,
    ) -> ([u8; 4], [u8; 4])
    where
        F: Fn(&Downsampler, u64, u64) -> [u8; 4],
    {
        // Channel A - use static value if configured (already in 0-200 range)
        let values_a = if a_is_static {
            let static_val = self.channel_a_config.intensity.static_value.unwrap_or(0.0);
            let intensity = static_val.round().clamp(0.0, 200.0) as u8;
            [intensity, intensity, intensity, intensity]
        } else if self
            .downsampler_a
            .has_samples_in_window(window_start, window_end)
        {
            downsample_fn(&self.downsampler_a, window_start, window_end)
        } else {
            self.v2_channel_a.get_next_four_values(window_start)
        };

        // Channel B - use static value if configured (already in 0-200 range)
        let values_b = if b_is_static {
            let static_val = self.channel_b_config.intensity.static_value.unwrap_or(0.0);
            let intensity = static_val.round().clamp(0.0, 200.0) as u8;
            [intensity, intensity, intensity, intensity]
        } else if self
            .downsampler_b
            .has_samples_in_window(window_start, window_end)
        {
            downsample_fn(&self.downsampler_b, window_start, window_end)
        } else {
            self.v2_channel_b.get_next_four_values(window_start)
        };

        (values_a, values_b)
    }

    /// Get current intensity values (for UI display)
    pub fn get_current_intensities(&self) -> (f64, f64) {
        match self.options.processing_engine {
            ProcessingEngineType::V1 => {
                let a = self.v1_channel_a.current_intensity as f64 / 200.0;
                let b = self.v1_channel_b.current_intensity as f64 / 200.0;
                (a, b)
            }
            _ => {
                let now = current_time_ms();
                let a = self.v2_channel_a.get_value_at(now) as f64 / 200.0;
                let b = self.v2_channel_b.get_value_at(now) as f64 / 200.0;
                (a, b)
            }
        }
    }

    /// Stop all channels
    pub fn stop(&mut self) {
        self.v1_channel_a.reset();
        self.v1_channel_b.reset();
        self.v2_channel_a.reset();
        self.v2_channel_b.reset();
        self.v3_channel_a.reset();
        self.v3_channel_b.reset();
        self.downsampler_a.clear();
        self.downsampler_b.clear();
    }

    /// Update output options
    pub fn set_options(&mut self, options: OutputOptions) {
        self.options = options;
    }

    // ===== Parameter Modulation Methods =====

    // ===== Buttplug Feature Methods =====

    /// Update a Buttplug feature value
    pub fn set_buttplug_feature(&mut self, key: String, value: f64) {
        self.buttplug_features.insert(key, value.clamp(0.0, 1.0));
    }

    /// Clear a specific Buttplug feature
    pub fn clear_buttplug_feature(&mut self, key: &str) {
        self.buttplug_features.remove(key);
    }

    /// Clear all Buttplug features (on stop commands)
    pub fn clear_all_buttplug_features(&mut self) {
        self.buttplug_features.clear();
        self.buttplug_linear_commands.clear();
        self.buttplug_rotate_directions.clear();
    }

    /// Get all Buttplug feature values
    pub fn get_buttplug_features(&self) -> HashMap<String, f64> {
        self.buttplug_features.clone()
    }

    /// Set a LinearCmd (PositionWithDuration) value with arrival timestamp
    pub fn set_buttplug_linear_cmd(&mut self, index: usize, position: f64, duration_ms: u32) {
        self.buttplug_linear_commands.insert(
            index,
            (
                position.clamp(0.0, 1.0),
                duration_ms,
                std::time::Instant::now(),
            ),
        );
    }

    /// Clear processed linear commands (call after pipeline processing)
    pub fn clear_buttplug_linear_commands(&mut self) {
        self.buttplug_linear_commands.clear();
    }

    /// Set a Rotate direction
    pub fn set_buttplug_rotate_direction(&mut self, index: usize, clockwise: bool) {
        self.buttplug_rotate_directions.insert(index, clockwise);
    }

    /// Get ButtplugFeatureValues struct for pipeline processing
    pub fn get_buttplug_feature_values(&self) -> crate::buttplug::ButtplugFeatureValues {
        crate::buttplug::ButtplugFeatureValues::from_hashmap(
            &self.buttplug_features,
            &self.buttplug_linear_commands,
            &self.buttplug_rotate_directions,
            8, // Max features per type
        )
    }

    /// Check if there are any active Buttplug features
    pub fn has_buttplug_input(&self) -> bool {
        !self.buttplug_features.is_empty() || !self.buttplug_linear_commands.is_empty()
    }

    // ===== Buttplug Link Config Methods =====

    /// Update the Buttplug link configuration for a channel's intensity parameter
    pub fn set_buttplug_link_config(&mut self, channel: char, config: ButtplugLinkConfig) {
        match channel {
            'A' | 'a' => {
                self.buttplug_link_config_a = config;
            }
            'B' | 'b' => {
                self.buttplug_link_config_b = config;
            }
            _ => {
                println!(
                    "[ProcessingState] Unknown channel for Buttplug link config: {}",
                    channel
                );
            }
        }
    }

    /// Get the Buttplug link configuration for a channel
    pub fn get_buttplug_link_config(&self, channel: char) -> &ButtplugLinkConfig {
        match channel {
            'A' | 'a' => &self.buttplug_link_config_a,
            'B' | 'b' => &self.buttplug_link_config_b,
            _ => &self.buttplug_link_config_a, // Default to A
        }
    }
}

// ============================================================================
// Global Processing State
// ============================================================================

pub static PROCESSING_STATE: tokio::sync::OnceCell<Arc<RwLock<ProcessingState>>> =
    tokio::sync::OnceCell::const_new();

pub async fn get_processing_state() -> &'static Arc<RwLock<ProcessingState>> {
    PROCESSING_STATE
        .get_or_init(|| async { Arc::new(RwLock::new(ProcessingState::default())) })
        .await
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get current time in milliseconds
pub fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tcode_simple() {
        let commands = parse_tcode("L0500");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].axis, "L0");
        assert!((commands[0].value - 0.5).abs() < 0.01);
        assert!(commands[0].interval_ms.is_none());
    }

    #[test]
    fn test_parse_tcode_with_interval() {
        let commands = parse_tcode("R2750I1000");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].axis, "R2");
        assert!((commands[0].value - 0.25).abs() < 0.01);
        assert_eq!(commands[0].interval_ms, Some(1000));
    }

    #[test]
    fn test_parse_tcode_multiple_axes() {
        let commands = parse_tcode("L0500 R0750 V1300");
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].axis, "L0");
        assert_eq!(commands[1].axis, "R0");
        assert_eq!(commands[2].axis, "V1");
    }

    #[test]
    fn test_v2_channel_state_interpolation() {
        let mut state = V2ChannelState::default();
        let start = 1000u64;

        // Set target with 100ms ramp
        state.set_target(100, 100, start);

        // At start, should be at start value (0)
        assert_eq!(state.get_value_at(start), 0);

        // At midpoint, should be halfway
        assert_eq!(state.get_value_at(start + 50), 50);

        // At end, should be at target
        assert_eq!(state.get_value_at(start + 100), 100);
    }

    #[test]
    fn test_processing_engine_from_str() {
        assert_eq!(
            ProcessingEngineType::from_str("v1"),
            ProcessingEngineType::V1
        );
        assert_eq!(
            ProcessingEngineType::from_str("v2-smooth"),
            ProcessingEngineType::V2Smooth
        );
        assert_eq!(
            ProcessingEngineType::from_str("v2-balanced"),
            ProcessingEngineType::V2Balanced
        );
        assert_eq!(
            ProcessingEngineType::from_str("v2-detailed"),
            ProcessingEngineType::V2Detailed
        );
        assert_eq!(
            ProcessingEngineType::from_str("v2-dynamic"),
            ProcessingEngineType::V2Dynamic
        );
        assert_eq!(
            ProcessingEngineType::from_str("v3-predictive"),
            ProcessingEngineType::V3Predictive
        );
    }

    // Note: Curve transformation tests are in modulation.rs

    #[test]
    fn test_axis_tracking() {
        let mut state = ProcessingState::default();
        let cmd = TCodeCommand {
            axis: "V1".to_string(),
            value: 0.75,
            interval_ms: None,
            received_at: 1000,
        };

        state.process_command(&cmd);

        let axis_value = state.get_axis_value("V1");
        assert!(axis_value.is_some());
        assert!((axis_value.unwrap() - 0.75).abs() < 0.01);
    }
}
