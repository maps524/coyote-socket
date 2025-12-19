/// Buttplug Feature Pipeline - Backend Foundation
///
/// This module implements the composable processing pipeline for Buttplug integration,
/// where each output type serves a distinct role in shaping the final output.
///
/// Pipeline order: Position → Motion (Rotate/Oscillate) → Vibrate → Constrict → Output

pub mod handler;
pub mod messages;
pub mod pipeline;
pub mod state;
pub mod types;

// Re-export commonly used items
pub use handler::handle_buttplug_message;
pub use messages::{
    parse_buttplug_messages, serialize_buttplug_messages, ButtplugClientMessage,
    ButtplugServerMessage,
};
pub use pipeline::process_buttplug_pipeline;
pub use state::{ButtplugChannelState, ButtplugFeatureValues, PositionDurationState};
pub use types::{
    ButtplugFeatureConfig, ButtplugFeatureType, ButtplugLinkConfig, ConstrictionMethod,
    FeatureTypeConfig,
};
