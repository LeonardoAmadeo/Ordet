use std::fs;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use ordet_format::write_bytes_atomically;
use serde::{Deserialize, Serialize};

use crate::i18n::UiLanguage;
use crate::theme::ThemeSelection;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub theme: ThemeSelection,
    #[serde(default)]
    pub word_target: Option<usize>,
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    #[serde(default)]
    pub ui_language: UiLanguage,
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,
    #[serde(default)]
    pub show_line_numbers: bool,
    #[serde(default = "default_true")]
    pub show_ruler: bool,
    #[serde(default = "default_true")]
    pub show_menu_bar: bool,
    #[serde(default = "default_autosave_enabled")]
    pub autosave_enabled: bool,
    #[serde(default = "default_indent_width")]
    pub indent_width: usize,
    #[serde(default = "default_true")]
    pub show_live_counts: bool,
    #[serde(default = "default_true")]
    pub backup_enabled: bool,
    #[serde(default = "default_true")]
    pub backup_on_close: bool,
    #[serde(default = "default_backup_keep")]
    pub backup_keep: usize,
    #[serde(default)]
    pub backup_dir: String,
}

pub fn default_autosave_enabled() -> bool {
    true
}

pub fn default_indent_width() -> usize {
    4
}

pub fn default_true() -> bool {
    true
}

pub fn default_backup_keep() -> usize {
    5
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeSelection::default(),
            word_target: None,
            recent_files: Vec::new(),
            ui_language: UiLanguage::default(),
            workspaces: Vec::new(),
            show_line_numbers: false,
            show_ruler: true,
            show_menu_bar: true,
            autosave_enabled: default_autosave_enabled(),
            indent_width: default_indent_width(),
            show_live_counts: true,
            backup_enabled: true,
            backup_on_close: true,
            backup_keep: default_backup_keep(),
            backup_dir: String::new(),
        }
    }
}

pub fn load_settings() -> Result<AppSettings, String> {
    let path = settings_path()?;
    if !path.exists() {
        return Ok(AppSettings::default());
    }

    let text = fs::read_to_string(&path)
        .map_err(|error| format!("Failed to read settings {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| {
        // A corrupt file heals itself: shelve it beside the original so no
        // edit is ever silently lost, then start from defaults.
        let backup = backup_path_for(&path);
        let backup_note = match fs::rename(&path, &backup) {
            Ok(()) => format!(" The broken file was kept at {}.", backup.display()),
            Err(rename_error) => format!(" Could not shelve it: {rename_error}."),
        };
        format!(
            "Failed to parse settings {}: {error}.{backup_note} Starting fresh.",
            path.display()
        )
    })
}

/// Sibling path for a shelved corrupt settings file.
fn backup_path_for(path: &std::path::Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    path.with_file_name(format!("{file_name}.corrupt-{stamp}"))
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path()?;
    let json = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("Failed to encode settings: {error}"))?;
    write_bytes_atomically(&path, json.as_bytes())
        .map_err(|error| format!("Failed to write settings {}: {error}", path.display()))
}

pub fn settings_path() -> Result<PathBuf, String> {
    if let Some(override_path) = std::env::var_os("ORDET_TUI_SETTINGS_PATH") {
        let override_path = PathBuf::from(override_path);
        if !override_path.as_os_str().is_empty() {
            return Ok(override_path);
        }
    }

    let config_dir = dirs::config_dir()
        .ok_or_else(|| "No platform config directory is available".to_string())?;
    // Keep the beta data directory while the command becomes `ordet`:
    // existing settings, history, and recovery stay where users left them.
    Ok(config_dir.join("ordet-tui").join("settings.json"))
}

#[cfg(test)]
pub(crate) fn settings_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::AppSettings;
    use crate::theme::{ThemePreset, ThemeSelection};

    #[test]
    fn settings_default_to_midnight() {
        assert_eq!(
            AppSettings::default().theme,
            ThemeSelection::Preset {
                preset: ThemePreset::Midnight
            }
        );
    }

    #[test]
    fn settings_ignore_unknown_keys() {
        // Settings files from older builds carry keys this version no longer
        // knows. Loading must ignore them; saving must drop them.
        let json = r#"{
            "word_target": 1500,
            "retired_feature_flag": true,
            "old_preferences": { "enabled": true },
            "legacy_extension_list": ["-- \u2014"]
        }"#;
        let settings: AppSettings = serde_json::from_str(json).expect("unknown keys are ignored");
        assert_eq!(settings.word_target, Some(1500));
        assert!(!settings.show_line_numbers);

        let saved = serde_json::to_string(&settings).expect("settings serialize");
        for unknown in [
            "retired_feature_flag",
            "old_preferences",
            "legacy_extension_list",
        ] {
            assert!(
                !saved.contains(unknown),
                "unknown key {unknown} survived a save: {saved}"
            );
        }
    }

    #[test]
    fn settings_path_honors_override() {
        let _guard = super::settings_env_lock()
            .lock()
            .expect("settings env lock");
        let expected = std::env::temp_dir().join("ordet-test-settings.json");
        let saved = std::env::var_os("ORDET_TUI_SETTINGS_PATH");
        unsafe {
            std::env::set_var("ORDET_TUI_SETTINGS_PATH", &expected);
        }
        let actual = super::settings_path().expect("override path");
        unsafe {
            // Restore rather than clear: the test harness deliberately keeps
            // a temp override in place so late writes stay out of the real
            // user config directory.
            match saved {
                Some(value) => std::env::set_var("ORDET_TUI_SETTINGS_PATH", value),
                None => std::env::remove_var("ORDET_TUI_SETTINGS_PATH"),
            }
        }
        assert_eq!(actual, expected);
    }
}
