//! Unified input bus.
//!
//! Single store for every input source's timestamped sample stream. Replaces
//! the pre-refactor `axis_values: HashMap<String, AxisState>` field on
//! `ProcessingState`, and is the eventual home for the three Buttplug feature
//! HashMaps that the bundled phase folds in.
//!
//! Snapshot consistency: callers grab a `read()` lock on `ProcessingState`,
//! pull a snapshot via `&processing_state.input_bus`, and resolve every
//! parameter for the current tick from that one borrow. No bus mutations can
//! race the snapshot under that lock.

use std::collections::HashMap;

use crate::modulation::AxisState;

/// Resolver-side handle to the bus. Today an alias for `&'a InputBus`,
/// taken under one read/write lock at the start of a tick. Sub G's
/// frozen-frame work (`InputBus::snapshot()` returning a copy-on-write
/// view) replaces the alias body without touching call sites — every
/// `resolve_link*` signature already names the type so the swap is a
/// typedef edit, not per-callsite churn.
pub type InputBusSnapshot<'a> = &'a InputBus;

/// Container for every named axis the runtime cares about. `AxisState`
/// already owns the per-axis history ring + staleness check; the bus is a
/// thin namespace + lookup layer over that.
///
/// `#[allow(dead_code)]` on the impl blocks below covers methods that
/// are exercised by tests + sub G's input-monitor work but unread by
/// the binary in the cargo-check pass.
#[derive(Debug, Default, Clone)]
pub struct InputBus {
    channels: HashMap<String, AxisState>,
}

#[allow(dead_code)]
impl InputBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a sample for `axis`. Creates the slot on first write. Stores
    /// `interval_ms` (T-Code ramp duration / Buttplug LinearCmd duration) on
    /// the history sample so tick-side replay can reproduce ramp semantics.
    pub fn update(&mut self, axis: &str, value: f64, timestamp: u64, interval_ms: Option<u32>) {
        self.channels
            .entry(axis.to_string())
            .or_default()
            .update(value, timestamp, interval_ms);
    }

    /// Latest value for `axis`, ignoring history.
    pub fn value(&self, axis: &str) -> Option<f64> {
        self.channels.get(axis).filter(|s| s.has_data).map(|s| s.value)
    }

    /// Value at `target_time`, walking the history ring (zero-order hold).
    /// Returns the latest known value when target is in the future or no
    /// sample matches; `None` only when the axis has never received data.
    pub fn value_at(&self, axis: &str, target_time: u64) -> Option<f64> {
        self.channels
            .get(axis)
            .filter(|s| s.has_data)
            .and_then(|s| s.value_at(target_time))
    }

    /// Milliseconds since the most recent sample on `axis`, or `None` if
    /// the axis has never received data. Used by the resolver's staleness
    /// branch to decide whether to fall through to `no_input` behavior.
    pub fn age_ms(&self, axis: &str, now: u64) -> Option<u64> {
        self.channels
            .get(axis)
            .filter(|s| s.has_data)
            .map(|s| now.saturating_sub(s.timestamp))
    }

    /// Direct access to an axis's `AxisState`. Used by the resolver when it
    /// needs the bundled (value, timestamp, has_data) tuple to decide which
    /// `no_input` branch to take.
    pub fn get(&self, axis: &str) -> Option<&AxisState> {
        self.channels.get(axis)
    }

    /// Timestamp of the latest sample on `axis`, or `None` if never written.
    /// Used by per-channel watermarks (e.g. the bundled phase's
    /// `bp:LinearCmd_<n>` "new arrival since last tick" check) to detect
    /// fresh writes without re-reading the full `AxisState`.
    pub fn latest_timestamp(&self, axis: &str) -> Option<u64> {
        self.channels
            .get(axis)
            .filter(|s| s.has_data)
            .map(|s| s.timestamp)
    }

    /// Iterate every named axis. Order is unspecified (HashMap).
    pub fn iter(&self) -> impl Iterator<Item = (&String, &AxisState)> {
        self.channels.iter()
    }

    /// Clear every axis. Used by `ProcessingState::stop` etc.
    pub fn clear(&mut self) {
        self.channels.clear();
    }

    /// Drop axes whose name starts with `prefix`. Lets the Buttplug handler
    /// clear out `bp:*` on disconnect without touching T-Code or gamepad
    /// axes.
    pub fn clear_prefix(&mut self, prefix: &str) {
        self.channels.retain(|k, _| !k.starts_with(prefix));
    }

    /// True iff at least one axis under `prefix` has received any sample.
    pub fn has_any_with_prefix(&self, prefix: &str) -> bool {
        self.channels
            .iter()
            .any(|(k, s)| k.starts_with(prefix) && s.has_data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_creates_axis_on_first_write() {
        let mut bus = InputBus::new();
        assert_eq!(bus.value("L0"), None);
        bus.update("L0", 0.5, 100, None);
        assert_eq!(bus.value("L0"), Some(0.5));
    }

    #[test]
    fn value_at_walks_history() {
        let mut bus = InputBus::new();
        bus.update("L0", 0.2, 100, None);
        bus.update("L0", 0.8, 200, None);
        assert_eq!(bus.value_at("L0", 100), Some(0.2));
        assert_eq!(bus.value_at("L0", 200), Some(0.8));
        assert_eq!(bus.value_at("L0", 150), Some(0.2));
        assert_eq!(bus.value_at("L0", 50), Some(0.2)); // clamps to oldest
    }

    #[test]
    fn value_returns_none_for_unknown_axis() {
        let bus = InputBus::new();
        assert_eq!(bus.value("R2"), None);
        assert_eq!(bus.value_at("R2", 100), None);
        assert_eq!(bus.age_ms("R2", 100), None);
    }

    #[test]
    fn age_ms_anchors_on_now() {
        let mut bus = InputBus::new();
        bus.update("L0", 0.5, 100, None);
        assert_eq!(bus.age_ms("L0", 250), Some(150));
        assert_eq!(bus.age_ms("L0", 100), Some(0));
        // saturating_sub prevents underflow for boot-time edge.
        assert_eq!(bus.age_ms("L0", 50), Some(0));
    }

    #[test]
    fn clear_prefix_only_drops_matching() {
        let mut bus = InputBus::new();
        bus.update("L0", 0.5, 100, None);
        bus.update("bp:Vibrate_0", 0.7, 100, None);
        bus.update("bp:Position_0", 0.3, 100, None);

        bus.clear_prefix("bp:");
        assert_eq!(bus.value("L0"), Some(0.5));
        assert_eq!(bus.value("bp:Vibrate_0"), None);
        assert_eq!(bus.value("bp:Position_0"), None);
    }

    #[test]
    fn has_any_with_prefix_distinguishes_unwritten_axes() {
        let mut bus = InputBus::new();
        bus.update("L0", 0.5, 100, None);
        assert!(!bus.has_any_with_prefix("bp:"));

        bus.update("bp:Vibrate_0", 0.4, 100, None);
        assert!(bus.has_any_with_prefix("bp:"));
        assert!(bus.has_any_with_prefix("L"));
    }

    #[test]
    fn latest_timestamp_tracks_most_recent_write() {
        let mut bus = InputBus::new();
        assert_eq!(bus.latest_timestamp("L0"), None);
        bus.update("L0", 0.2, 100, None);
        assert_eq!(bus.latest_timestamp("L0"), Some(100));
        bus.update("L0", 0.5, 250, None);
        assert_eq!(bus.latest_timestamp("L0"), Some(250));
        // Never-written axis stays None.
        assert_eq!(bus.latest_timestamp("R2"), None);
    }

    #[test]
    fn iter_yields_every_named_axis() {
        let mut bus = InputBus::new();
        bus.update("L0", 0.1, 100, None);
        bus.update("R2", 0.9, 100, None);
        let names: std::collections::HashSet<_> = bus.iter().map(|(k, _)| k.clone()).collect();
        assert!(names.contains("L0"));
        assert!(names.contains("R2"));
        assert_eq!(names.len(), 2);
    }
}
