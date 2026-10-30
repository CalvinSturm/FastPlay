use std::fs;
use std::path::PathBuf;

use crate::platform::menu::MenuBarMode;

const DEFAULT_VOLUME: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppSettings {
    pub volume: f32,
    pub frameless_windowed: bool,
    pub menu_bar_mode: MenuBarMode,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            volume: DEFAULT_VOLUME,
            frameless_windowed: false,
            menu_bar_mode: MenuBarMode::Visible,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("FastPlay").join("settings.txt"))
}

pub fn load() -> AppSettings {
    let Some(path) = settings_path() else {
        return AppSettings::default();
    };
    let Ok(contents) = fs::read_to_string(&path) else {
        return AppSettings::default();
    };
    parse(&contents)
}

fn parse(contents: &str) -> AppSettings {
    let mut settings = AppSettings::default();
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("volume=") {
            if let Ok(v) = value.trim().parse::<f32>() {
                if (0.0..=1.5).contains(&v) {
                    settings.volume = v;
                }
            }
        } else if let Some(value) = line.strip_prefix("frameless_windowed=") {
            if let Ok(enabled) = value.trim().parse::<bool>() {
                settings.frameless_windowed = enabled;
            }
        } else if let Some(value) = line.strip_prefix("menu_bar=") {
            if let Some(mode) = MenuBarMode::from_setting(value.trim()) {
                settings.menu_bar_mode = mode;
            }
        }
    }
    settings
}

pub fn save_volume(volume: f32) {
    let mut settings = load();
    settings.volume = volume;
    save(settings);
}

pub fn save_frameless_windowed(enabled: bool) {
    let mut settings = load();
    settings.frameless_windowed = enabled;
    save(settings);
}

pub fn save_menu_bar_mode(mode: MenuBarMode) {
    let mut settings = load();
    settings.menu_bar_mode = mode;
    save(settings);
}

fn save(settings: AppSettings) {
    let Some(path) = settings_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(
        &path,
        format!(
            "volume={}\nframeless_windowed={}\nmenu_bar={}\n",
            settings.volume,
            settings.frameless_windowed,
            settings.menu_bar_mode.as_setting()
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_values_use_backwards_compatible_defaults() {
        assert_eq!(parse(""), AppSettings::default());
        assert_eq!(
            parse("volume=0.75\n"),
            AppSettings {
                volume: 0.75,
                ..AppSettings::default()
            }
        );
    }

    #[test]
    fn parses_persisted_frameless_preference_without_losing_volume() {
        assert_eq!(
            parse("volume=0.8\nframeless_windowed=true\n"),
            AppSettings {
                volume: 0.8,
                frameless_windowed: true,
                ..AppSettings::default()
            }
        );
    }

    #[test]
    fn invalid_values_fall_back_independently() {
        assert_eq!(
            parse("volume=2.0\nframeless_windowed=maybe\nmenu_bar=sometimes\n"),
            AppSettings::default()
        );
    }

    #[test]
    fn migrates_removed_hover_menu_mode_to_hidden() {
        assert_eq!(
            parse("volume=1\nframeless_windowed=false\nmenu_bar=autohide\n").menu_bar_mode,
            MenuBarMode::Hidden
        );
    }
}
