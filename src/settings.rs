use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// `settings.json` in the app's config directory. Read on every check, so
/// edits take effect within seconds.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Kept aloft when Aloft last ran.
    pub on: bool,
    /// A computer that can't sleep stays hot in a bag; turn off below this charge on battery.
    pub min_battery: u8,
    /// Where to find Flokque, if it runs on this computer.
    pub flokque_url: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            on: false,
            min_battery: 20,
            flokque_url: "http://127.0.0.1:4820".into(),
        }
    }
}

const FILE: &str = "settings.json";

impl Settings {
    pub fn load(dir: &Path) -> Self {
        fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_on(dir: &Path, on: bool) {
        let mut settings = Self::load(dir);
        settings.on = on;
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(
            dir.join(FILE),
            serde_json::to_string_pretty(&settings).unwrap_or_default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_defaults_for_missing_keys() {
        let settings: Settings = serde_json::from_str(r#"{"minBattery": 10}"#).unwrap();
        assert_eq!(settings.min_battery, 10);
        assert_eq!(settings.flokque_url, "http://127.0.0.1:4820");
        assert!(!settings.on);
    }
}
