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

// Re-export commonly used items
pub use pipeline::process_buttplug_pipeline;
pub use state::{ButtplugChannelState, ButtplugFeatureValues};
pub use types::{ButtplugFeatureConfig, ButtplugLinkConfig, ConstrictionMethod, FeatureTypeConfig};
