use serde::{Deserialize, Serialize};

/// Buttplug feature output types — wire-format identifiers used by the
/// device descriptor. Sub F dropped the runtime pipeline that consumed
/// these as routing tags; the enum stays for `buttplug/handler.rs` to
/// label features advertised over the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ButtplugFeatureType {
    Position,
    PositionWithDuration,
    Vibrate,
    Rotate,
    Oscillate,
    Constrict,
}

/// Per-feature-type slot count for the device descriptor advertisement.
/// Determines how many features of each type the virtual Buttplug
/// device reports as available.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtplugFeatureConfig {
    pub position: usize,
    pub position_with_duration: usize,
    pub vibrate: usize,
    pub rotate: usize,
    pub oscillate: usize,
    pub constrict: usize,
}

impl Default for ButtplugFeatureConfig {
    fn default() -> Self {
        Self {
            // Clients prefer LinearCmd (PositionWithDuration); raw Position is off by default.
            position: 0,
            position_with_duration: 2,
            vibrate: 2,
            rotate: 2,
            oscillate: 2,
            constrict: 2,
        }
    }
}

impl ButtplugFeatureConfig {
    pub fn total_features(&self) -> usize {
        self.position
            + self.position_with_duration
            + self.vibrate
            + self.rotate
            + self.oscillate
            + self.constrict
    }
}
