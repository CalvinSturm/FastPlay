use std::fs;
use std::path::PathBuf;

fn settings_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("FastPlay").join("settings.txt"))
}

pub fn load_volume() -> f32 {
    let Some(path) = settings_path() else {
        return 1.0;
    };
    let Ok(contents) = fs::read_to_string(&path) else {
        return 1.0;
    };
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("volume=") {
            if let Ok(v) = value.trim().parse::<f32>() {
                if (0.0..=1.5).contains(&v) {
                    return v;
                }
            }
        }
    }
    1.0
}

pub fn save_volume(volume: f32) {
    let Some(path) = settings_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&path, format!("volume={volume}\n"));
}
