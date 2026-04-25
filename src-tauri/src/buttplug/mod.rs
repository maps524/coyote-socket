/// Buttplug Feature Pipeline - Backend Foundation
///
/// This module implements the composable processing pipeline for Buttplug integration,
/// where each output type serves a distinct role in shaping the final output.
///
/// Pipeline order: Position → Motion (Rotate/Oscillate) → Vibrate → Constrict → Output
#[allow(dead_code)]
pub mod handler;
#[allow(dead_code, non_snake_case)]
pub mod messages;
#[allow(dead_code)]
pub mod pipeline;
#[allow(dead_code)]
pub mod state;
#[allow(dead_code)]
pub mod types;

// Re-export commonly used items. The `process_buttplug_pipeline`
// re-export is gone in sub E — no live caller, full deletion happens
// in sub F alongside the pipeline.rs file itself.
pub use state::{ButtplugChannelState, ButtplugFeatureValues};
pub use types::{ButtplugFeatureConfig, ButtplugLinkConfig, ConstrictionMethod, FeatureTypeConfig};
