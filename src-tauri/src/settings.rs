//! Settings Module
//!
//! Manages application settings with persistence to a JSON file alongside the executable.
//! This enables portable settings that travel with the application.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::processing::{ChannelInterplay, ProcessingEngineType};

// ============================================================================
// Settings Structs
// ============================================================================

/// Bluetooth device info for persistence
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SavedBluetoothDevice {
    pub address: String,
    pub name: Option<String>,
}

/// Connection settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSettings {
    /// WebSocket server port (displayed as ws://127.0.0.1:{port})
    pub websocket_port: u16,
    pub auto_open: bool,
    pub show_tcode_monitor: bool,
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            websocket_port: 12346,
            auto_open: true,
            show_tcode_monitor: false,
        }
    }
}

/// Bluetooth settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BluetoothSettings {
    pub selected_interface: usize,
    pub auto_scan: bool,
    pub auto_connect: bool,
    pub saved_devices: Vec<SavedBluetoothDevice>,
    pub last_device: Option<String>,
}

impl Default for BluetoothSettings {
    fn default() -> Self {
        Self {
            selected_interface: 0,
            auto_scan: true,
            auto_connect: true,
            saved_devices: Vec::new(),
            last_device: None,
        }
    }
}

/// Output/processing settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSettings {
    pub channel_interplay: ChannelInterplay,
    pub processing_engine: ProcessingEngineType,
    #[serde(default = "default_chase_delay")]
    pub chase_delay_ms: u32,
}

fn default_chase_delay() -> u32 {
    100
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            channel_interplay: ChannelInterplay::None,
            processing_engine: ProcessingEngineType::V2Smooth,
            chase_delay_ms: 100,
        }
    }
}

/// Parameter source type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ParameterSourceType {
    Static,
    Linked,
}

impl Default for ParameterSourceType {
    fn default() -> Self {
        Self::Static
    }
}

/// Buttplug feature link configuration for persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButtplugFeatureLinkSettings {
    pub feature_type: String, // "Position", "Vibrate", etc.
    pub feature_index: u32,
    #[serde(default)]
    pub config: ButtplugFeatureConfigSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ButtplugFeatureConfigSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotate_scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotate_max_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oscillate_scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oscillate_max_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrict_min_floor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrict_use_midpoint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrict_method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ButtplugLinksSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<ButtplugFeatureLinkSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<ButtplugFeatureLinkSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vibrate: Option<ButtplugFeatureLinkSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrict: Option<ButtplugFeatureLinkSettings>,
}

impl ButtplugLinksSettings {
    /// Convert settings-based links to processing-ready ButtplugLinkConfig
    pub fn to_link_config(&self) -> crate::buttplug::ButtplugLinkConfig {
        use crate::buttplug::{ButtplugLinkConfig, ConstrictionMethod, FeatureTypeConfig};

        let mut config = ButtplugLinkConfig::default();

        // Position link - can be Position or PositionWithDuration
        if let Some(ref pos) = self.position {
            match pos.feature_type.as_str() {
                "Position" => {
                    config.position_feature = Some(pos.feature_index as usize);
                }
                "PositionWithDuration" => {
                    config.pos_dur_feature = Some(pos.feature_index as usize);
                }
                _ => {}
            }
        }

        // Motion link - Rotate OR Oscillate (mutually exclusive)
        if let Some(ref motion) = self.motion {
            match motion.feature_type.as_str() {
                "Rotate" => {
                    config.rotate_feature = Some(motion.feature_index as usize);
                    config.rotate_config = Some(FeatureTypeConfig {
                        scale: motion.config.rotate_scale,
                        max_speed: motion.config.rotate_max_speed,
                        ..Default::default()
                    });
                }
                "Oscillate" => {
                    config.oscillate_feature = Some(motion.feature_index as usize);
                    config.oscillate_config = Some(FeatureTypeConfig {
                        scale: motion.config.oscillate_scale,
                        max_speed: motion.config.oscillate_max_speed,
                        ..Default::default()
                    });
                }
                _ => {}
            }
        }

        // Vibrate link
        if let Some(ref vib) = self.vibrate {
            config.vibrate_feature = Some(vib.feature_index as usize);
            config.vibrate_config = Some(FeatureTypeConfig {
                distance: vib.config.distance,
                ..Default::default()
            });
        }

        // Constrict link
        if let Some(ref con) = self.constrict {
            config.constrict_feature = Some(con.feature_index as usize);
            let method = con
                .config
                .constrict_method
                .as_ref()
                .and_then(|m| match m.as_str() {
                    "downsample" | "Downsample" => Some(ConstrictionMethod::Downsample),
                    "clamp" | "Clamp" => Some(ConstrictionMethod::Clamp),
                    _ => None,
                });
            config.constrict_config = Some(FeatureTypeConfig {
                min_floor: con.config.constrict_min_floor,
                use_midpoint: con.config.constrict_use_midpoint,
                method,
                ..Default::default()
            });
        }

        config
    }
}

/// Parameter source settings - stores both static value and linked range
/// so switching between modes preserves both values
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterSourceSettings {
    #[serde(rename = "type")]
    pub source_type: ParameterSourceType,
    pub static_value: f64,
    pub source_axis: String,
    pub range_min: f64,
    pub range_max: f64,
    pub curve: String,
    pub curve_strength: f64,
    #[serde(default)]
    pub midpoint: bool,
    /// Buttplug feature links for this parameter (used when input source is Buttplug)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttplug_links: Option<ButtplugLinksSettings>,
}

impl ParameterSourceSettings {
    pub fn new_static(value: f64, default_axis: &str, min: f64, max: f64) -> Self {
        Self {
            source_type: ParameterSourceType::Static,
            static_value: value,
            source_axis: default_axis.to_string(),
            range_min: min,
            range_max: max,
            curve: "linear".to_string(),
            curve_strength: 2.0,
            midpoint: false,
            buttplug_links: None,
        }
    }

    pub fn new_linked(axis: &str, min: f64, max: f64, static_fallback: f64) -> Self {
        Self {
            source_type: ParameterSourceType::Linked,
            static_value: static_fallback,
            source_axis: axis.to_string(),
            range_min: min,
            range_max: max,
            curve: "linear".to_string(),
            curve_strength: 2.0,
            midpoint: false,
            buttplug_links: None,
        }
    }
}

/// Input ecosystem type for preset
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PresetEcosystem {
    Tcode,
    Buttplug,
}

impl Default for PresetEcosystem {
    fn default() -> Self {
        Self::Tcode
    }
}

/// A preset stores channel configuration for both channels
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPreset {
    pub name: String,
    #[serde(default)]
    pub ecosystem: PresetEcosystem,
    pub channel_a: ChannelSettings,
    pub channel_b: ChannelSettings,
}

/// Channel parameters with full parameter source support
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSettings {
    pub frequency_source: ParameterSourceSettings,
    pub frequency_balance_source: ParameterSourceSettings,
    pub intensity_balance_source: ParameterSourceSettings,
    pub intensity_source: ParameterSourceSettings,
}

impl ChannelSettings {
    pub fn default_for_channel(channel: char) -> Self {
        let default_axis = if channel == 'A' { "L0" } else { "R2" };
        Self {
            frequency_source: ParameterSourceSettings::new_static(100.0, default_axis, 1.0, 200.0),
            frequency_balance_source: ParameterSourceSettings::new_static(
                128.0,
                default_axis,
                0.0,
                255.0,
            ),
            intensity_balance_source: ParameterSourceSettings::new_static(
                128.0,
                default_axis,
                0.0,
                255.0,
            ),
            intensity_source: ParameterSourceSettings::new_linked(default_axis, 0.0, 20.0, 100.0),
        }
    }
}

impl Default for ChannelSettings {
    fn default() -> Self {
        Self::default_for_channel('A')
    }
}

/// Legacy channel settings for migration from old format
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyChannelSettings {
    pub frequency: f64,
    pub freq_balance: u8,
    pub int_balance: u8,
    pub range_min: u8,
    pub range_max: u8,
}

impl LegacyChannelSettings {
    /// Migrate to new ChannelSettings format
    pub fn migrate(self, channel: char) -> ChannelSettings {
        let default_axis = if channel == 'A' { "L0" } else { "R2" };
        ChannelSettings {
            frequency_source: ParameterSourceSettings::new_static(
                self.frequency,
                default_axis,
                1.0,
                200.0,
            ),
            frequency_balance_source: ParameterSourceSettings::new_static(
                self.freq_balance as f64,
                default_axis,
                0.0,
                255.0,
            ),
            intensity_balance_source: ParameterSourceSettings::new_static(
                self.int_balance as f64,
                default_axis,
                0.0,
                255.0,
            ),
            intensity_source: ParameterSourceSettings::new_linked(
                default_axis,
                self.range_min as f64,
                self.range_max as f64,
                100.0,
            ),
        }
    }
}

/// Keyboard shortcuts
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardShortcuts {
    pub channel_a_freq_up: String,
    pub channel_a_freq_down: String,
    pub channel_a_int_up: String,
    pub channel_a_int_down: String,
    pub channel_a_freq_bal_up: String,
    pub channel_a_freq_bal_down: String,
    pub channel_a_int_bal_up: String,
    pub channel_a_int_bal_down: String,
    pub channel_b_freq_up: String,
    pub channel_b_freq_down: String,
    pub channel_b_int_up: String,
    pub channel_b_int_down: String,
    pub channel_b_freq_bal_up: String,
    pub channel_b_freq_bal_down: String,
    pub channel_b_int_bal_up: String,
    pub channel_b_int_bal_down: String,
    pub help: String,
    pub settings: String,
    #[serde(default = "default_toggle_output_pause")]
    pub toggle_output_pause: String,
}

fn default_toggle_output_pause() -> String {
    " ".to_string() // Space bar
}

/// General application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneralSettings {
    pub no_input_behavior: String,
    pub no_input_decay_ms: u32,
    pub update_rate_ms: u32,
    pub save_rate_ms: u32,
    pub show_tcode_monitor: bool,
    pub processing_engine: ProcessingEngineType,
    #[serde(default)]
    pub output_paused: bool,
}

impl Default for KeyboardShortcuts {
    fn default() -> Self {
        Self {
            channel_a_freq_up: "q".to_string(),
            channel_a_freq_down: "a".to_string(),
            channel_a_int_up: "r".to_string(),
            channel_a_int_down: "f".to_string(),
            channel_a_freq_bal_up: "w".to_string(),
            channel_a_freq_bal_down: "s".to_string(),
            channel_a_int_bal_up: "e".to_string(),
            channel_a_int_bal_down: "d".to_string(),
            channel_b_freq_up: "[".to_string(),
            channel_b_freq_down: "'".to_string(),
            channel_b_int_up: "i".to_string(),
            channel_b_int_down: "k".to_string(),
            channel_b_freq_bal_up: "p".to_string(),
            channel_b_freq_bal_down: ";".to_string(),
            channel_b_int_bal_up: "o".to_string(),
            channel_b_int_bal_down: "l".to_string(),
            help: "?".to_string(),
            settings: ",".to_string(),
            toggle_output_pause: " ".to_string(), // Space bar
        }
    }
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            no_input_behavior: "hold".to_string(),
            no_input_decay_ms: 1000,
            update_rate_ms: 50,
            save_rate_ms: 500,
            show_tcode_monitor: false,
            processing_engine: ProcessingEngineType::V2Smooth,
            output_paused: false,
        }
    }
}

/// Complete application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub connection: ConnectionSettings,
    pub bluetooth: BluetoothSettings,
    pub output: OutputSettings,
    pub channel_a: ChannelSettings,
    pub channel_b: ChannelSettings,
    pub shortcuts: KeyboardShortcuts,
    pub general: GeneralSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            connection: ConnectionSettings::default(),
            bluetooth: BluetoothSettings::default(),
            output: OutputSettings::default(),
            channel_a: ChannelSettings::default_for_channel('A'),
            channel_b: ChannelSettings::default_for_channel('B'),
            shortcuts: KeyboardShortcuts::default(),
            general: GeneralSettings::default(),
        }
    }
}

// ============================================================================
// Global State
// ============================================================================

/// Global settings state
static SETTINGS: tokio::sync::OnceCell<Arc<RwLock<AppSettings>>> =
    tokio::sync::OnceCell::const_new();

/// Get settings file path (alongside the executable for portability)
fn get_settings_path() -> PathBuf {
    // Get the directory containing the executable
    let exe_path = std::env::current_exe().expect("Failed to get executable path");
    let exe_dir = exe_path
        .parent()
        .expect("Failed to get executable directory");
    exe_dir.join("settings.json")
}

/// Initialize settings (call once at startup)
pub async fn init_settings() -> &'static Arc<RwLock<AppSettings>> {
    SETTINGS
        .get_or_init(|| async {
            let settings = load_settings_from_disk();
            Arc::new(RwLock::new(settings))
        })
        .await
}

/// Legacy app settings for migration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyAppSettings {
    pub connection: ConnectionSettings,
    pub bluetooth: BluetoothSettings,
    pub output: OutputSettings,
    pub channel_a: LegacyChannelSettings,
    pub channel_b: LegacyChannelSettings,
    pub shortcuts: KeyboardShortcuts,
    #[serde(default)]
    pub general: GeneralSettings,
}

impl LegacyAppSettings {
    fn migrate(self) -> AppSettings {
        AppSettings {
            connection: self.connection,
            bluetooth: self.bluetooth,
            output: self.output,
            channel_a: self.channel_a.migrate('A'),
            channel_b: self.channel_b.migrate('B'),
            shortcuts: self.shortcuts,
            general: self.general,
        }
    }
}

/// Load settings from disk with automatic migration from legacy format
fn load_settings_from_disk() -> AppSettings {
    let path = get_settings_path();
    println!("[Settings] Looking for settings at: {:?}", path);

    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(contents) => {
                // First try the new format
                match serde_json::from_str::<AppSettings>(&contents) {
                    Ok(settings) => {
                        println!("[Settings] Loaded from {:?}", path);
                        return settings;
                    }
                    Err(new_err) => {
                        println!("[Settings] New format parse failed: {}", new_err);

                        // Try legacy format
                        match serde_json::from_str::<LegacyAppSettings>(&contents) {
                            Ok(legacy) => {
                                println!("[Settings] Detected legacy format, migrating...");
                                let migrated = legacy.migrate();

                                // Save migrated settings
                                if let Err(e) = save_settings_to_disk(&migrated) {
                                    println!("[Settings] Failed to save migrated settings: {}", e);
                                } else {
                                    println!("[Settings] Migration complete, saved new format");
                                }

                                return migrated;
                            }
                            Err(legacy_err) => {
                                println!(
                                    "[Settings] Legacy format parse also failed: {}",
                                    legacy_err
                                );
                                println!(
                                    "[Settings] Will use defaults and overwrite corrupted file"
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("[Settings] Failed to read settings file: {}", e);
            }
        }
    } else {
        println!("[Settings] No settings file found, using defaults");
    }

    let defaults = AppSettings::default();
    // Save defaults to disk so the file exists
    if let Err(e) = save_settings_to_disk(&defaults) {
        println!("[Settings] Failed to save defaults: {}", e);
    }
    defaults
}

/// Save settings to disk
fn save_settings_to_disk(settings: &AppSettings) -> Result<(), String> {
    let path = get_settings_path();
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;
    fs::write(&path, json).map_err(|e| format!("Failed to write settings: {}", e))?;
    println!("[Settings] Saved to {:?}", path);
    Ok(())
}

// ============================================================================
// Public API
// ============================================================================

/// Get current settings
pub async fn get_settings() -> AppSettings {
    let state = init_settings().await;
    state.read().await.clone()
}

/// Update all settings
pub async fn update_settings(updates: AppSettings) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    *settings = updates;
    save_settings_to_disk(&settings)
}

/// Update just channel A settings
pub async fn update_channel_a(channel: ChannelSettings) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.channel_a = channel;
    save_settings_to_disk(&settings)
}

/// Update just channel B settings
pub async fn update_channel_b(channel: ChannelSettings) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.channel_b = channel;
    save_settings_to_disk(&settings)
}

/// Update just output settings
pub async fn update_output(output: OutputSettings) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.output = output;
    save_settings_to_disk(&settings)
}

/// Update just connection settings
pub async fn update_connection(connection: ConnectionSettings) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.connection = connection;
    save_settings_to_disk(&settings)
}

/// Update just bluetooth settings
pub async fn update_bluetooth(bluetooth: BluetoothSettings) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.bluetooth = bluetooth;
    save_settings_to_disk(&settings)
}

/// Update just shortcuts
pub async fn update_shortcuts(shortcuts: KeyboardShortcuts) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.shortcuts = shortcuts;
    save_settings_to_disk(&settings)
}

/// Update just general settings
pub async fn update_general(general: GeneralSettings) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.general = general;
    save_settings_to_disk(&settings)
}

// ============================================================================
// Output Pause State
// ============================================================================

/// Get current output pause state
pub async fn get_output_paused() -> bool {
    let state = init_settings().await;
    state.read().await.general.output_paused
}

/// Set output pause state and persist to disk
pub async fn set_output_paused(paused: bool) -> Result<(), String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.general.output_paused = paused;
    save_settings_to_disk(&settings)
}

/// Toggle output pause state and return the new state
pub async fn toggle_output_paused() -> Result<bool, String> {
    let state = init_settings().await;
    let mut settings = state.write().await;
    settings.general.output_paused = !settings.general.output_paused;
    let new_state = settings.general.output_paused;
    save_settings_to_disk(&settings)?;
    Ok(new_state)
}

// ============================================================================
// Presets Management
// ============================================================================

/// Global presets state
static PRESETS: tokio::sync::OnceCell<Arc<RwLock<Vec<ChannelPreset>>>> =
    tokio::sync::OnceCell::const_new();

/// Get presets file path
fn get_presets_path() -> PathBuf {
    let exe_path = std::env::current_exe().expect("Failed to get executable path");
    let exe_dir = exe_path
        .parent()
        .expect("Failed to get executable directory");
    exe_dir.join("presets.json")
}

/// Initialize presets (call once at startup)
pub async fn init_presets() -> &'static Arc<RwLock<Vec<ChannelPreset>>> {
    PRESETS
        .get_or_init(|| async {
            let presets = load_presets_from_disk();
            Arc::new(RwLock::new(presets))
        })
        .await
}

/// Load presets from disk
fn load_presets_from_disk() -> Vec<ChannelPreset> {
    let path = get_presets_path();
    println!("[Presets] Looking for presets at: {:?}", path);

    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Vec<ChannelPreset>>(&contents) {
                Ok(presets) => {
                    println!("[Presets] Loaded {} presets from {:?}", presets.len(), path);
                    return presets;
                }
                Err(e) => {
                    println!("[Presets] Failed to parse presets: {}", e);
                }
            },
            Err(e) => {
                println!("[Presets] Failed to read presets file: {}", e);
            }
        }
    } else {
        println!("[Presets] No presets file found");
    }

    Vec::new()
}

/// Save presets to disk
fn save_presets_to_disk(presets: &[ChannelPreset]) -> Result<(), String> {
    let path = get_presets_path();
    let json = serde_json::to_string_pretty(presets)
        .map_err(|e| format!("Failed to serialize presets: {}", e))?;
    fs::write(&path, json).map_err(|e| format!("Failed to write presets: {}", e))?;
    println!("[Presets] Saved {} presets to {:?}", presets.len(), path);
    Ok(())
}

/// Get all presets
pub async fn get_presets() -> Vec<ChannelPreset> {
    let state = init_presets().await;
    state.read().await.clone()
}

/// Save a new preset or update existing one with same name
pub async fn save_preset(preset: ChannelPreset) -> Result<(), String> {
    let state = init_presets().await;
    let mut presets = state.write().await;

    // Check if preset with same name exists
    if let Some(existing) = presets.iter_mut().find(|p| p.name == preset.name) {
        *existing = preset;
    } else {
        presets.push(preset);
    }

    save_presets_to_disk(&presets)
}

/// Delete a preset by name
pub async fn delete_preset(name: &str) -> Result<(), String> {
    let state = init_presets().await;
    let mut presets = state.write().await;

    let original_len = presets.len();
    presets.retain(|p| p.name != name);

    if presets.len() == original_len {
        return Err(format!("Preset '{}' not found", name));
    }

    save_presets_to_disk(&presets)
}

/// Rename a preset
pub async fn rename_preset(old_name: &str, new_name: &str) -> Result<(), String> {
    let state = init_presets().await;
    let mut presets = state.write().await;

    // Check if new name already exists
    if presets.iter().any(|p| p.name == new_name) {
        return Err(format!("Preset '{}' already exists", new_name));
    }

    // Find and rename
    if let Some(preset) = presets.iter_mut().find(|p| p.name == old_name) {
        preset.name = new_name.to_string();
        save_presets_to_disk(&presets)
    } else {
        Err(format!("Preset '{}' not found", old_name))
    }
}
