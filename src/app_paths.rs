use std::path::PathBuf;

pub fn config_base() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return Some(home().map_or_else(std::env::temp_dir, |path| {
            path.join("Library/Application Support")
        }));
    }
    std::env::var_os("APPDATA").map(PathBuf::from)
}

pub fn config_base_or_current() -> PathBuf {
    config_base().unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_base() -> PathBuf {
    if cfg!(target_os = "macos") {
        return config_base().unwrap_or_else(std::env::temp_dir);
    }
    std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

pub fn home() -> Option<PathBuf> {
    let key = if cfg!(target_os = "macos") {
        "HOME"
    } else {
        "USERPROFILE"
    };
    std::env::var_os(key).map(PathBuf::from)
}
