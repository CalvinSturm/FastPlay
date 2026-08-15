use std::fs;
use std::path::PathBuf;

const DEFAULT_VOLUME: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppSettings {
    pub volume: f32,
    pub frameless_windowed: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            volume: DEFAULT_VOLUME,
            frameless_windowed: false,
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
            "volume={}\nframeless_windowed={}\n",
            settings.volume, settings.frameless_windowed
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
                frameless_windowed: false,
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
            }
        );
    }

    #[test]
    fn invalid_values_fall_back_independently() {
        assert_eq!(
            parse("volume=2.0\nframeless_windowed=maybe\n"),
            AppSettings::default()
        );
    }
}
