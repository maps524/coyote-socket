//! Buttplug protocol surface — wire format + message handler only.
//!
//! Sub F dropped the runtime pipeline (`pipeline.rs`, `state.rs`) and the
//! `ButtplugLinkConfig` / `FeatureTypeConfig` shape. Feature samples flow
//! into `InputBus` under the `bp:` namespace and are read by the unified
//! resolver via `ParameterLinkConfig.transforms`. The Buttplug client
//! still talks to this module for handshake + device descriptor +
//! command parsing; everything downstream of that lives in the resolver.
#[allow(dead_code)]
pub mod handler;
#[allow(dead_code, non_snake_case)]
pub mod messages;
#[allow(dead_code)]
pub mod types;

pub use types::ButtplugFeatureConfig;
