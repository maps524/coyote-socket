//! What the app remembers between runs.
//!
//! Only enough to make the second run one click: the last endpoint and a short
//! list of recent ones. The headset's IP moves on DHCP, so "pick from a list"
//! beats "retype it", and a list beats a single remembered value the first
//! time it changes and comes back.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How many endpoints to remember. Long enough to cover a headset, a desktop
/// player and the local fake; short enough to stay a menu rather than a
/// history.
const MAX_RECENTS: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Last endpoint connected to, `host:port`.
    pub endpoint: String,
    /// Most recent first, including `endpoint`.
    pub recents: Vec<String>,
    /// Port the bridge serves the phone app and the QR on.
    pub http_port: u16,
    /// Directory of static files to serve, when the PWA has been built.
    pub static_dir: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            recents: Vec::new(),
            http_port: 8787,
            static_dir: None,
        }
    }
}

impl Settings {
    /// Record a successful (or attempted) endpoint, moving it to the front.
    pub fn remember(&mut self, endpoint: &str) {
        self.endpoint = endpoint.to_string();
        self.recents.retain(|e| e != endpoint);
        self.recents.insert(0, endpoint.to_string());
        self.recents.truncate(MAX_RECENTS);
    }

    pub fn load(path: &PathBuf) -> Self {
        // A missing or corrupt file is not worth surfacing: defaults are
        // perfectly usable and the alternative is an error dialog on launch
        // about something the user never asked for.
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &PathBuf) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembering_moves_an_endpoint_to_the_front_without_duplicating() {
        let mut s = Settings::default();
        s.remember("a:1");
        s.remember("b:2");
        s.remember("a:1");
        assert_eq!(s.recents, vec!["a:1", "b:2"]);
        assert_eq!(s.endpoint, "a:1");
    }

    #[test]
    fn the_recents_list_stays_short() {
        let mut s = Settings::default();
        for i in 0..20 {
            s.remember(&format!("host{i}:23554"));
        }
        assert_eq!(s.recents.len(), MAX_RECENTS);
        assert_eq!(s.recents[0], "host19:23554");
    }

    #[test]
    fn an_unreadable_file_yields_defaults_rather_than_an_error() {
        let missing = std::env::temp_dir().join("coyote-bridge-does-not-exist.json");
        let s = Settings::load(&missing);
        assert_eq!(s.http_port, 8787);
        assert!(s.recents.is_empty());
    }

    #[test]
    fn settings_round_trip_through_disk() {
        let path = std::env::temp_dir().join("coyote-bridge-settings-test.json");
        let mut original = Settings::default();
        original.remember("192.168.1.50:23554");
        original.http_port = 9000;
        original.save(&path);

        let loaded = Settings::load(&path);
        assert_eq!(loaded.endpoint, "192.168.1.50:23554");
        assert_eq!(loaded.http_port, 9000);
        let _ = std::fs::remove_file(&path);
    }
}
