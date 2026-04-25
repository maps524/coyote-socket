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
use crate::modulation::{AxisState, ChannelConfig, NoInputBehavior};

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
    /// Like V2Dynamic for waveform shape, but adds a rolling 200ms peak-hold
    /// on the master channel intensity. Sustains felt intensity through fast
    /// input zero-crossings instead of letting the master volume dip when a
    /// packet's max ages out. Best for rapid pole-flicking input where
    /// V2Dynamic's per-packet master gate makes oscillation feel weak.
    V2Sustained,
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
            "v2-sustained" => Self::V2Sustained,
            "v3-predictive" => Self::V3Predictive,
            _ => Self::V1,
        }
    }
}

/// Which algorithm fills empty buckets in peak-preserving downsampling.
/// Orthogonal to `ProcessingEngineType`; only affects the `V2Detailed` engine.
/// Exposed alongside the Engine dropdown so users can A/B the behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PeakFillStrategy {
    /// v1 (original): empty buckets inherit the previous bucket, then
    /// `last_value`. Preserves prior presets' feel.
    Legacy,
    /// v2 (new default): empty buckets inherit the next non-empty bucket
    /// (forward-fill). Better peak preservation under sparse input.
    #[default]
    Forward,
}

impl PeakFillStrategy {
    pub fn from_str(s: &str) -> Self {
        match s {
            "legacy" | "v1" => Self::Legacy,
            "forward" | "v2" => Self::Forward,
            _ => Self::default(),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Forward => "forward",
        }
    }
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
    #[allow(dead_code)]
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

    /// Reset to zero
    pub fn reset(&mut self) {
        self.command_buffer.clear();
        self.current_position = 0;
    }

    #[cfg(test)]
    pub fn buffer_size(&self) -> usize {
        self.command_buffer.len()
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

    /// Peak-preserving downsample (v1 = legacy cascade back-fill).
    ///
    /// Original behavior: empty buckets inherit the previous bucket's value
    /// (or `self.last_value` for the first bucket). This weakens peak-preservation
    /// when sample density is sparse — leading buckets can drag window MAX down.
    /// Kept as a selectable variant so existing presets retain their feel.
    pub fn downsample_detailed_v1(&self, window_start: u64, window_end: u64) -> [u8; 4] {
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
                result[i] = if i > 0 {
                    result[i - 1]
                } else {
                    self.last_value
                };
            } else {
                result[i] = bucket_samples.iter().map(|s| s.value).max().unwrap_or(0);
            }
        }

        result
    }

    /// Peak-preserving downsample (v2 = forward-fill).
    ///
    /// Empty buckets inherit the NEXT non-empty bucket's MAX (preferred —
    /// reflects where the signal is heading). Trailing empty buckets fall back
    /// to the previous filled bucket, then `self.last_value` as last resort.
    /// Preserves peaks better than v1 under sparse input.
    pub fn downsample_detailed_v2(&self, window_start: u64, window_end: u64) -> [u8; 4] {
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

        let mut buckets: [Option<u8>; 4] = [None; 4];
        for i in 0..4 {
            let bucket_start = window_start + i as u64 * bucket_duration;
            let bucket_end = bucket_start + bucket_duration;
            buckets[i] = window_samples
                .iter()
                .filter(|s| s.timestamp >= bucket_start && s.timestamp < bucket_end)
                .map(|s| s.value)
                .max();
        }

        let mut result = [0u8; 4];
        for i in 0..4 {
            result[i] = match buckets[i] {
                Some(v) => v,
                None => {
                    let forward = buckets[i + 1..].iter().find_map(|b| *b);
                    forward
                        .or_else(|| if i > 0 { Some(result[i - 1]) } else { None })
                        .unwrap_or(self.last_value)
                }
            };
        }

        result
    }

    /// Peak-preserving downsample dispatch. Picks between v1 (legacy) and v2
    /// (forward-fill) based on the configured `PeakFillStrategy`.
    pub fn downsample_detailed(
        &self,
        window_start: u64,
        window_end: u64,
        strategy: PeakFillStrategy,
    ) -> [u8; 4] {
        match strategy {
            PeakFillStrategy::Legacy => self.downsample_detailed_v1(window_start, window_end),
            PeakFillStrategy::Forward => self.downsample_detailed_v2(window_start, window_end),
        }
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
            // Not enough variation - fall back to detailed (peak-preserving).
            // Dynamic's fallback uses forward-fill unconditionally; its own feel
            // is orthogonal to the V2Detailed v1/v2 variant selector.
            return self.downsample_detailed(window_start, window_end, PeakFillStrategy::Forward);
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
    pub processing_engine: ProcessingEngineType,
    /// Variant for V2Detailed peak-preserving empty-bucket behavior.
    /// Other engines ignore this.
    #[serde(default)]
    pub peak_fill: PeakFillStrategy,
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            processing_engine: ProcessingEngineType::V1,
            peak_fill: PeakFillStrategy::default(),
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
    /// Per-slot engine output BEFORE normalization to relative 0-100. Same
    /// device-units (0-200) scale as `intensity`, before peak-hold + relative
    /// scaling. Kept around purely for diagnostic capture so analyzers can
    /// compute true input→output lag without unwinding the normalization.
    pub raw_values: [u8; 4],
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
            raw_values: values,
        }
    }
}

// ============================================================================
// Channel (per-channel bundle of engine states + config + buttplug state)
// ============================================================================

/// Identifies one of the two output channels. `repr(usize)` lets the enum
/// index directly into `[Channel; 2]` without a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum ChannelId {
    A = 0,
    B = 1,
}

impl ChannelId {
    pub const ALL: [ChannelId; 2] = [ChannelId::A, ChannelId::B];

    pub fn from_char(c: char) -> Option<Self> {
        match c {
            'A' | 'a' => Some(ChannelId::A),
            'B' | 'b' => Some(ChannelId::B),
            _ => None,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "A" | "a" => Some(ChannelId::A),
            "B" | "b" => Some(ChannelId::B),
            _ => None,
        }
    }

    pub fn as_char(&self) -> char {
        match self {
            ChannelId::A => 'A',
            ChannelId::B => 'B',
        }
    }

    #[cfg(test)]
    pub fn default_axis(&self) -> &'static str {
        match self {
            ChannelId::A => "L0",
            ChannelId::B => "R2",
        }
    }
}

/// Rolling per-channel master-intensity peak tracker for V2Sustained.
///
/// Stores `(timestamp_ms, intensity)` observations and exposes the max
/// over a configurable lookback window. Used to keep the master channel
/// intensity from dipping during fast input swings — the waveform shape
/// keeps oscillating per-slot, but the volume sustains at the recent peak.
#[derive(Debug, Default, Clone)]
pub struct IntensityPeakHold {
    samples: VecDeque<(u64, u8)>,
}

impl IntensityPeakHold {
    pub fn observe(&mut self, now_ms: u64, value: u8) {
        self.samples.push_back((now_ms, value));
        // Generous prune cutoff (1s) — caller picks the actual hold window
        // when reading. Keeps the buffer bounded under sustained input.
        let cutoff = now_ms.saturating_sub(1000);
        while self
            .samples
            .front()
            .map(|(t, _)| *t < cutoff)
            .unwrap_or(false)
        {
            self.samples.pop_front();
        }
    }

    pub fn peak_in_last_ms(&self, now_ms: u64, hold_ms: u64) -> u8 {
        let cutoff = now_ms.saturating_sub(hold_ms);
        self.samples
            .iter()
            .filter(|(t, _)| *t >= cutoff)
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(0)
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// Per-channel bundle: all engine states + config + buttplug state for one output.
///
/// Centralizes every `*_a`/`*_b` pair from the original `ProcessingState` so that
/// per-channel logic lives in one place. `ProcessingState` holds `[Channel; 2]`
/// and only owns truly shared state (axis values, options, interplay history).
pub struct Channel {
    #[allow(dead_code)]
    pub id: ChannelId,
    pub config: ChannelConfig,
    pub v1: V1ChannelState,
    pub v2: V2ChannelState,
    pub v3: V3ChannelState,
    pub downsampler: Downsampler,
    pub buttplug_link: ButtplugLinkConfig,
    pub buttplug_state: ButtplugChannelState,
    /// Master-intensity peak history. Only populated when V2Sustained is
    /// the active engine; idle otherwise. Lives on Channel so each channel
    /// holds its own peak independently.
    pub peak_hold: IntensityPeakHold,
    /// Watermark for delayed-intensity tick replay. Tracks the upper bound
    /// of axis-history timestamps already fed into engines, so each tick
    /// only replays new samples in `(watermark, now - delay_ms]`.
    pub last_intensity_replay_ts: u64,
}

impl Channel {
    pub fn new(id: ChannelId) -> Self {
        let config = match id {
            ChannelId::A => ChannelConfig::channel_a_default(),
            ChannelId::B => ChannelConfig::channel_b_default(),
        };
        Self {
            id,
            config,
            v1: V1ChannelState::default(),
            v2: V2ChannelState::default(),
            v3: V3ChannelState::default(),
            downsampler: Downsampler::default(),
            buttplug_link: ButtplugLinkConfig::default(),
            buttplug_state: ButtplugChannelState::default(),
            peak_hold: IntensityPeakHold::default(),
            last_intensity_replay_ts: 0,
        }
    }

    /// Apply a T-Code command to all engine states for this channel.
    /// Runs midpoint → curve transform, converts to device 0-200 u8, then
    /// feeds V1 queue, V2 ramp, downsampler, and V3 lookahead buffer.
    /// Range mapping is intentionally skipped here — it happens later in
    /// `device.rs::scale_intensity` using the intensity source's range.
    pub fn apply_tcode(&mut self, value: f64, interval_ms: Option<u32>, timestamp: u64) {
        use crate::modulation::{apply_curve, apply_midpoint};

        let midpoint_value = if self.config.intensity.midpoint.unwrap_or(false) {
            apply_midpoint(value)
        } else {
            value
        };

        let curve = &self.config.intensity.curve;
        let strength = self.config.intensity.curve_strength.unwrap_or(2.0);
        let curved = apply_curve(midpoint_value, curve, strength);

        let intensity_u8 = (curved * 200.0).round().clamp(0.0, 200.0) as u8;

        self.v1.apply_command(curved, interval_ms);
        self.v2
            .set_target(intensity_u8, interval_ms.unwrap_or(0), timestamp);
        self.downsampler.add_sample(intensity_u8, timestamp);
        self.v3.buffer_command(intensity_u8, timestamp);
    }

    /// Compute this channel's raw waveform values for the current tick.
    /// Caller chooses engine + peak_fill strategy; returns the 4 per-slot
    /// device intensities (0-200) before range scaling / interplay.
    pub fn next_raw_values(
        &mut self,
        engine: ProcessingEngineType,
        window_start: u64,
        now_ms: u64,
        peak_fill: PeakFillStrategy,
    ) -> [u8; 4] {
        match engine {
            ProcessingEngineType::V1 => self.v1.get_next_four(),
            ProcessingEngineType::V2Smooth => {
                if self.downsampler.has_samples_in_window(window_start, now_ms) {
                    self.downsampler.downsample_smooth(window_start, now_ms)
                } else {
                    self.v2.get_next_four_values(window_start)
                }
            }
            ProcessingEngineType::V2Balanced => {
                if self.downsampler.has_samples_in_window(window_start, now_ms) {
                    self.downsampler.downsample_balanced(window_start, now_ms)
                } else {
                    self.v2.get_next_four_values(window_start)
                }
            }
            ProcessingEngineType::V2Detailed => {
                if self.downsampler.has_samples_in_window(window_start, now_ms) {
                    self.downsampler
                        .downsample_detailed(window_start, now_ms, peak_fill)
                } else {
                    self.v2.get_next_four_values(window_start)
                }
            }
            ProcessingEngineType::V2Dynamic | ProcessingEngineType::V2Sustained => {
                // V2Sustained shares V2Dynamic's per-slot oscillation logic.
                // The "sustained" part is applied later in get_next_waveform_data
                // by overriding the master intensity with a rolling peak-hold.
                if self.downsampler.has_samples_in_window(window_start, now_ms) {
                    self.downsampler.downsample_dynamic(window_start, now_ms)
                } else {
                    self.v2.get_next_four_values(window_start)
                }
            }
            ProcessingEngineType::V3Predictive => self.v3.get_next_four_values(now_ms),
        }
    }

    /// Reset all engine states to zero; leaves config + buttplug_link untouched.
    pub fn reset_engines(&mut self) {
        self.v1.reset();
        self.v2.reset();
        self.v3.reset();
        self.downsampler.clear();
        self.peak_hold.clear();
    }

    /// Current intensity in 0-1 range (for UI display).
    pub fn current_intensity_normalized(&self, engine: ProcessingEngineType, now: u64) -> f64 {
        match engine {
            ProcessingEngineType::V1 => self.v1.current_intensity as f64 / 200.0,
            _ => self.v2.get_value_at(now) as f64 / 200.0,
        }
    }
}

// ============================================================================
// Processing State (Main struct that holds everything)
// ============================================================================

/// Main processing state. Per-channel data lives on `channels[0]` / `channels[1]`
/// indexed via `ChannelId`. Truly shared state (axes, options, interplay history)
/// stays on `ProcessingState` itself.
pub struct ProcessingState {
    /// Per-channel engine states + config + buttplug link/state.
    pub channels: [Channel; 2],

    // Output options
    pub options: OutputOptions,

    // ===== Parameter Modulation System =====
    // Track ALL T-Code axes (L0-L2, R0-R2, V0-V3, A0-A1)
    pub axis_values: HashMap<String, AxisState>,

    // Track Buttplug feature values (feature_key → value 0.0-1.0)
    // Keys are like "Vibrate_0", "Position_0", "Oscillate_0", etc.
    pub buttplug_features: HashMap<String, f64>,

    /// LinearCmd commands with (position, duration_ms, arrival_time)
    pub buttplug_linear_commands: HashMap<usize, (f64, u32, std::time::Instant)>,

    // Track Rotate directions: index → clockwise
    pub buttplug_rotate_directions: HashMap<usize, bool>,

    // General settings for parameter modulation
    pub no_input_behavior: NoInputBehavior,
    pub no_input_decay_ms: u32,
}

impl Default for ProcessingState {
    fn default() -> Self {
        Self {
            channels: [Channel::new(ChannelId::A), Channel::new(ChannelId::B)],
            options: OutputOptions::default(),
            axis_values: HashMap::new(),
            buttplug_features: HashMap::new(),
            buttplug_linear_commands: HashMap::new(),
            buttplug_rotate_directions: HashMap::new(),
            no_input_behavior: NoInputBehavior::Hold,
            no_input_decay_ms: 1000,
        }
    }
}

impl ProcessingState {
    /// Immutable access to one channel.
    pub fn channel(&self, id: ChannelId) -> &Channel {
        &self.channels[id as usize]
    }

    /// Mutable access to one channel.
    pub fn channel_mut(&mut self, id: ChannelId) -> &mut Channel {
        &mut self.channels[id as usize]
    }

    #[cfg(test)]
    pub fn get_axis_value(&self, axis: &str) -> Option<f64> {
        self.axis_values.get(axis).map(|s| s.value)
    }
}

impl ProcessingState {
    /// Process a T-Code command. Stores the sample in the axis history so
    /// `resolve_parameter*` and the device-tick replay can read it. Engine
    /// ingestion happens later in `replay_pending_intensity_samples` at tick
    /// time so a per-parameter `delay_ms` can offset *when* the engines see
    /// each sample without duplicating sample state.
    pub fn process_command(&mut self, cmd: &TCodeCommand) {
        let now = cmd.received_at;

        self.axis_values
            .entry(cmd.axis.clone())
            .or_default()
            .update(cmd.value, now, cmd.interval_ms);
    }

    /// Drain pending TCode samples from axis history into engine state for
    /// each linked-intensity channel, honoring `delay_ms`. Samples in
    /// `(last_intensity_replay_ts, now - delay_ms]` are fed through
    /// `apply_tcode` in chronological order. The watermark is bumped to
    /// `now - delay_ms` so future ticks pick up where this one left off.
    ///
    /// Bounded by `INTENSITY_REPLAY_FLOOR_MS` so a stale watermark (e.g.
    /// after a long static→linked switch) doesn't replay ancient history.
    pub fn replay_pending_intensity_samples(&mut self, now_ms: u64) {
        use crate::modulation::ParameterSourceType;
        const INTENSITY_REPLAY_FLOOR_MS: u64 = 200;

        for id in ChannelId::ALL {
            let (axis_name, delay) = {
                let cfg = &self.channel(id).config.intensity;
                if cfg.source_type != ParameterSourceType::Linked {
                    continue;
                }
                let axis = match cfg.source_axis.as_deref() {
                    Some(a) => a.to_string(),
                    None => continue,
                };
                (axis, cfg.delay_ms.unwrap_or(0) as u64)
            };

            let upper = now_ms.saturating_sub(delay);
            // Cap how far back a stale watermark can pull us.
            let floor = upper.saturating_sub(INTENSITY_REPLAY_FLOOR_MS);
            let after = self.channel(id).last_intensity_replay_ts.max(floor);

            if upper <= after {
                // Nothing new to replay (delay just increased, or no time
                // has passed). Don't bump watermark backwards.
                continue;
            }

            // Snapshot to release the immutable borrow before mutating.
            let pending: Vec<(f64, Option<u32>, u64)> = self
                .axis_values
                .get(&axis_name)
                .map(|state| {
                    state
                        .samples_in_range(after, upper)
                        .map(|s| (s.value, s.interval_ms, s.timestamp))
                        .collect()
                })
                .unwrap_or_default();

            let ch = self.channel_mut(id);
            for (value, interval, ts) in pending {
                ch.apply_tcode(value, interval, ts);
            }
            ch.last_intensity_replay_ts = upper;
        }
    }

    /// Get the next waveform data for both channels (called at 10Hz).
    /// Output priority per channel: Buttplug pipeline > static source > engine.
    pub fn get_next_waveform_data(&mut self) -> (WaveformData, WaveformData) {
        use crate::modulation::ParameterSourceType;

        let now_ms = current_time_ms();
        // Drain pending TCode samples (with optional delay) into engine state
        // before computing the output for this tick.
        self.replay_pending_intensity_samples(now_ms);
        let window_start = now_ms.saturating_sub(100);
        let now_instant = Instant::now();
        let dt_ms = 100u32; // 10Hz tick rate = 100ms per tick

        let has_buttplug = self.has_buttplug_input();
        let a_has_bp_links = self.channel(ChannelId::A).buttplug_link.has_any_links();
        let b_has_bp_links = self.channel(ChannelId::B).buttplug_link.has_any_links();

        if has_buttplug || a_has_bp_links || b_has_bp_links {
            println!("[Buttplug Pipeline] has_buttplug={}, a_has_links={}, b_has_links={}, features={:?}, linear_cmds={:?}, pos_dur_feature_a={:?}",
                has_buttplug, a_has_bp_links, b_has_bp_links,
                self.buttplug_features.keys().collect::<Vec<_>>(),
                self.buttplug_linear_commands.keys().collect::<Vec<_>>(),
                self.channel(ChannelId::A).buttplug_link.pos_dur_feature);
        }

        // Run Buttplug pipeline per channel if links are configured.
        // Features are snapshotted ONCE so the second call sees the same input.
        let bp_intensities: [Option<u8>; 2] = if has_buttplug {
            let features = self.get_buttplug_feature_values();
            let mut out = [None, None];
            for id in ChannelId::ALL {
                let ch = self.channel_mut(id);
                if !ch.buttplug_link.has_any_links() {
                    continue;
                }
                let output = process_buttplug_pipeline(
                    &mut ch.buttplug_state,
                    &features,
                    &ch.buttplug_link,
                    now_instant,
                    dt_ms,
                );
                let intensity = (output * 200.0).round().clamp(0.0, 200.0) as u8;
                if id == ChannelId::A {
                    println!(
                        "[Buttplug Pipeline] Channel A: pipeline_output={:.3}, intensity={}",
                        output, intensity
                    );
                }
                out[id as usize] = Some(intensity);
            }
            self.buttplug_linear_commands.clear();
            out
        } else {
            [None, None]
        };

        let engine = self.options.processing_engine;
        let peak_fill = self.options.peak_fill;

        let intensity_to_values = |i: u8| -> [u8; 4] { [i, i, i, i] };

        // Compute per-channel raw values. Static sources bypass the engine.
        let raw: [[u8; 4]; 2] = std::array::from_fn(|i| {
            if let Some(bp_val) = bp_intensities[i] {
                return intensity_to_values(bp_val);
            }
            let ch = &mut self.channels[i];
            if ch.config.intensity.source_type == ParameterSourceType::Static {
                let static_val = ch.config.intensity.static_value.unwrap_or(0.0);
                return intensity_to_values(static_val.round().clamp(0.0, 200.0) as u8);
            }
            ch.next_raw_values(engine, window_start, now_ms, peak_fill)
        });

        let mut wf_a = WaveformData::from_values(raw[0]);
        let mut wf_b = WaveformData::from_values(raw[1]);

        // V2Sustained: feed each channel's per-packet max into a 200ms
        // rolling peak tracker, then override the master intensity with the
        // recent peak. Sub-slot waveform shape (waveform_intensity) is
        // untouched, so pulses still oscillate per-slot — only the channel
        // master volume is held up between fast input swings.
        if engine == ProcessingEngineType::V2Sustained {
            const PEAK_HOLD_MS: u64 = 200;
            self.channels[0].peak_hold.observe(now_ms, wf_a.intensity);
            self.channels[1].peak_hold.observe(now_ms, wf_b.intensity);
            wf_a.intensity = self.channels[0]
                .peak_hold
                .peak_in_last_ms(now_ms, PEAK_HOLD_MS);
            wf_b.intensity = self.channels[1]
                .peak_hold
                .peak_in_last_ms(now_ms, PEAK_HOLD_MS);
        }

        (wf_a, wf_b)
    }

    /// Get current intensity values (for UI display)
    pub fn get_current_intensities(&self) -> (f64, f64) {
        let engine = self.options.processing_engine;
        let now = current_time_ms();
        let a = self
            .channel(ChannelId::A)
            .current_intensity_normalized(engine, now);
        let b = self
            .channel(ChannelId::B)
            .current_intensity_normalized(engine, now);
        (a, b)
    }

    /// Stop all channels
    pub fn stop(&mut self) {
        for id in ChannelId::ALL {
            self.channel_mut(id).reset_engines();
        }
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
        match ChannelId::from_char(channel) {
            Some(id) => self.channel_mut(id).buttplug_link = config,
            None => println!(
                "[ProcessingState] Unknown channel for Buttplug link config: {}",
                channel
            ),
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
    use crate::modulation::{CurveType, ParameterSource};

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

    // ========================================================================
    // ChannelId + PeakFillStrategy
    // ========================================================================

    #[test]
    fn test_channel_id_index_and_roundtrip() {
        assert_eq!(ChannelId::A as usize, 0);
        assert_eq!(ChannelId::B as usize, 1);
        assert_eq!(ChannelId::ALL, [ChannelId::A, ChannelId::B]);

        assert_eq!(ChannelId::from_char('A'), Some(ChannelId::A));
        assert_eq!(ChannelId::from_char('a'), Some(ChannelId::A));
        assert_eq!(ChannelId::from_char('B'), Some(ChannelId::B));
        assert_eq!(ChannelId::from_char('b'), Some(ChannelId::B));
        assert_eq!(ChannelId::from_char('X'), None);

        assert_eq!(ChannelId::from_str("A"), Some(ChannelId::A));
        assert_eq!(ChannelId::from_str("b"), Some(ChannelId::B));
        assert_eq!(ChannelId::from_str("nope"), None);

        assert_eq!(ChannelId::A.as_char(), 'A');
        assert_eq!(ChannelId::B.as_char(), 'B');

        assert_eq!(ChannelId::A.default_axis(), "L0");
        assert_eq!(ChannelId::B.default_axis(), "R2");
    }

    #[test]
    fn test_peak_fill_strategy_roundtrip() {
        assert_eq!(PeakFillStrategy::default(), PeakFillStrategy::Forward);
        assert_eq!(PeakFillStrategy::from_str("legacy"), PeakFillStrategy::Legacy);
        assert_eq!(PeakFillStrategy::from_str("v1"), PeakFillStrategy::Legacy);
        assert_eq!(PeakFillStrategy::from_str("forward"), PeakFillStrategy::Forward);
        assert_eq!(PeakFillStrategy::from_str("v2"), PeakFillStrategy::Forward);
        assert_eq!(PeakFillStrategy::from_str("garbage"), PeakFillStrategy::default());

        assert_eq!(PeakFillStrategy::Legacy.as_str(), "legacy");
        assert_eq!(PeakFillStrategy::Forward.as_str(), "forward");
    }

    // ========================================================================
    // Channel behavior
    // ========================================================================

    #[test]
    fn test_channel_defaults_per_id() {
        let a = Channel::new(ChannelId::A);
        let b = Channel::new(ChannelId::B);

        // A defaults to L0, B to R2 (preserved asymmetry)
        assert_eq!(a.config.intensity.source_axis.as_deref(), Some("L0"));
        assert_eq!(b.config.intensity.source_axis.as_deref(), Some("R2"));
    }

    #[test]
    fn test_channel_apply_tcode_feeds_all_engines() {
        let mut ch = Channel::new(ChannelId::A);
        ch.apply_tcode(0.5, None, 1000);

        // V1 queue should have been populated via apply_command (immediate set)
        assert!(!ch.v1.ramp_queue.is_empty() || ch.v1.current_intensity > 0);

        // V2 target should reflect curved value × 200
        assert_eq!(ch.v2.target_value, 100);

        // Downsampler should contain the sample
        assert!(ch.downsampler.has_samples_in_window(1000, 1001));

        // V3 command buffer should have one entry
        assert_eq!(ch.v3.buffer_size(), 1);
    }

    #[test]
    fn test_channel_reset_engines_clears_state() {
        let mut ch = Channel::new(ChannelId::A);
        ch.apply_tcode(1.0, None, 1000);
        assert_eq!(ch.v2.target_value, 200);

        ch.reset_engines();
        assert_eq!(ch.v2.target_value, 0);
        assert_eq!(ch.v1.current_intensity, 0);
        assert_eq!(ch.v3.buffer_size(), 0);
        // Config untouched by reset
        assert_eq!(ch.config.intensity.source_axis.as_deref(), Some("L0"));
    }

    // ========================================================================
    // process_command dispatch (core state-sync invariant)
    // ========================================================================

    #[test]
    fn test_process_command_routes_to_linked_channel_only() {
        let mut state = ProcessingState::default();
        // Defaults: A ← L0, B ← R2.
        let cmd = TCodeCommand {
            axis: "L0".to_string(),
            value: 0.8,
            interval_ms: None,
            received_at: 1000,
        };
        state.process_command(&cmd);
        // Engine ingestion happens at tick time, not on command arrival.
        state.replay_pending_intensity_samples(1000);

        // A was linked to L0 → v2 target set to curved 0.8 × 200 = 160.
        assert_eq!(state.channel(ChannelId::A).v2.target_value, 160);
        // B was linked to R2, not L0 → untouched.
        assert_eq!(state.channel(ChannelId::B).v2.target_value, 0);
    }

    #[test]
    fn test_process_command_same_axis_hits_both_when_linked() {
        let mut state = ProcessingState::default();
        // Re-link B's intensity to L0 so both channels share the axis.
        state.channel_mut(ChannelId::B).config.intensity =
            ParameterSource::linked_source("L0", 0.0, 200.0, CurveType::Linear);

        let cmd = TCodeCommand {
            axis: "L0".to_string(),
            value: 0.5,
            interval_ms: None,
            received_at: 1000,
        };
        state.process_command(&cmd);
        state.replay_pending_intensity_samples(1000);

        assert_eq!(state.channel(ChannelId::A).v2.target_value, 100);
        assert_eq!(state.channel(ChannelId::B).v2.target_value, 100);
    }

    #[test]
    fn test_process_command_ignored_when_no_channel_linked() {
        let mut state = ProcessingState::default();
        let cmd = TCodeCommand {
            axis: "V2".to_string(),
            value: 1.0,
            interval_ms: None,
            received_at: 1000,
        };
        state.process_command(&cmd);
        state.replay_pending_intensity_samples(1000);

        // Neither channel linked to V2 — no engine side-effects.
        assert_eq!(state.channel(ChannelId::A).v2.target_value, 0);
        assert_eq!(state.channel(ChannelId::B).v2.target_value, 0);
        // But axis value IS cached (for later resolve_parameter lookups).
        assert_eq!(state.get_axis_value("V2"), Some(1.0));
    }

    #[test]
    fn test_replay_honors_delay_ms() {
        // With a 50ms delay, a sample at t=1000 shouldn't reach engines until
        // a tick at t >= 1050. A tick at t=1020 should leave engines untouched.
        let mut state = ProcessingState::default();
        state.channel_mut(ChannelId::A).config.intensity.delay_ms = Some(50);

        let cmd = TCodeCommand {
            axis: "L0".to_string(),
            value: 0.8,
            interval_ms: None,
            received_at: 1000,
        };
        state.process_command(&cmd);

        state.replay_pending_intensity_samples(1020);
        assert_eq!(state.channel(ChannelId::A).v2.target_value, 0);

        state.replay_pending_intensity_samples(1080);
        assert_eq!(state.channel(ChannelId::A).v2.target_value, 160);
    }

    // ========================================================================
    // downsample_detailed: v1 (cascade) vs v2 (forward-fill) divergence
    // ========================================================================

    #[test]
    fn test_downsample_detailed_v1_vs_v2_empty_bucket_handling() {
        // Two samples in a 100ms window: a low value in bucket 0 and a peak
        // in bucket 3. Buckets 1 and 2 are empty. This is the fixture that
        // exposes the v1/v2 divergence — a single-sample window gets
        // short-circuited earlier in the function, so 2+ samples are required.
        let ws: u64 = 0;
        let we: u64 = 100;

        let mut ds = Downsampler::default();
        ds.last_value = 0;
        ds.samples.push_back(Sample { timestamp: ws + 5,  value: 20 });
        ds.samples.push_back(Sample { timestamp: ws + 80, value: 200 });

        // v1 (cascade back-fill): empty buckets 1 & 2 inherit bucket 0 = 20.
        // Peak at bucket 3 survives, but the window MAX-driven device
        // intensity is pulled down because only slot 3 shows the peak.
        let v1 = ds.downsample_detailed_v1(ws, we);
        assert_eq!(v1, [20, 20, 20, 200]);

        // v2 (forward-fill): empty buckets 1 & 2 inherit bucket 3 = 200.
        // Peak gets spread across more slots → stronger perceived intensity.
        let v2 = ds.downsample_detailed_v2(ws, we);
        assert_eq!(v2, [20, 200, 200, 200]);

        assert_ne!(v1, v2);
    }

    #[test]
    fn test_downsample_detailed_dispatch_matches_variant() {
        let ws: u64 = 0;
        let we: u64 = 100;
        let mut ds = Downsampler::default();
        ds.samples.push_back(Sample { timestamp: ws + 5,  value: 20 });
        ds.samples.push_back(Sample { timestamp: ws + 80, value: 200 });

        let legacy = ds.downsample_detailed(ws, we, PeakFillStrategy::Legacy);
        let forward = ds.downsample_detailed(ws, we, PeakFillStrategy::Forward);
        assert_eq!(legacy, ds.downsample_detailed_v1(ws, we));
        assert_eq!(forward, ds.downsample_detailed_v2(ws, we));
        assert_ne!(legacy, forward, "v1 and v2 must differ on this fixture");
    }

    #[test]
    fn test_downsample_detailed_preserves_peak_in_middle_bucket() {
        // Peak in bucket 2, stable low values elsewhere. Both variants should
        // preserve it since the peak bucket itself is non-empty.
        let ws: u64 = 0;
        let we: u64 = 100;
        let mut ds = Downsampler::default();
        ds.samples.push_back(Sample { timestamp: ws + 5, value: 50 });
        ds.samples.push_back(Sample { timestamp: ws + 30, value: 50 });
        ds.samples.push_back(Sample { timestamp: ws + 60, value: 180 });
        ds.samples.push_back(Sample { timestamp: ws + 90, value: 50 });
        ds.last_value = 50;

        let v1 = ds.downsample_detailed_v1(ws, we);
        let v2 = ds.downsample_detailed_v2(ws, we);

        // Bucket 2 MAX = 180 in both variants.
        assert_eq!(v1[2], 180);
        assert_eq!(v2[2], 180);
    }
}
