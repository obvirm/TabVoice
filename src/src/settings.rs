//! Konfigurasi persistent di `%APPDATA%\TabVoice\settings.toml`.
//!
//! Format file: TOML, di-serialize via `serde` + `toml` crate.
//! Kalau file belum ada → return [`Settings::default()`].
//! Kalau file ada tapi corrupt → log warning, fall back ke default.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Nama folder config di bawah `%APPDATA%`.
const APP_DIR: &str = "TabVoice";
/// Nama file config.
const FILE_NAME: &str = "settings.toml";

/// Konfigurasi user yang di-persist ke disk.
///
/// Semua field di-`pub` agar UI / tray bisa langsung baca-tulis lewat struct
/// (serde akan serialize berdasarkan nama field).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Path ke model Whisper GGML (mis. `models/ggml-base.bin`).
    /// Path ini bisa absolut atau relatif terhadap working directory.
    pub model_path: PathBuf,

    /// BCP-47 language code (mis. `"en"`, `"id"`, `"ja"`).
    /// `None` = auto-detect dari audio.
    pub language: Option<String>,

    /// Hotkey string (mis. `"Ctrl+Shift+Space"`).
    /// Format mengikuti grammar `global-hotkey` crate.
    pub hotkey: String,

    /// Kalau `true`: paste otomatis ke window aktif saat hotkey dilepas.
    /// Kalau `false`: hanya salin ke clipboard.
    pub paste_on_release: bool,

    /// Nama microphone yang dipilih (opsional).
    /// `None` = pakai default system.
    pub device_name: Option<String>,

    /// Apakah menggunakan tema gelap.
    pub dark_mode: bool,

    /// Apakah menggunakan mode realtime transcription.
    pub realtime: bool,

    /// Apakah TabVoice dijalankan otomatis saat Windows menyala.
    #[serde(default)]
    pub auto_start: bool,

    /// Ambang batas RMS untuk Voice Activity Detection.
    /// Nilai default: 0.005. Jika suara lebih rendah dari ini, akan diabaikan (mencegah halusinasi).
    #[serde(default = "default_vad_threshold")]
    pub vad_threshold: f32,
}

fn default_vad_threshold() -> f32 {
    0.005
}

/// Mendapatkan absolute path ke folder `models/` di direktori yang sama dengan executable.
pub fn get_models_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join("models")
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model_path: get_models_dir().join("ggml-base.bin"),
            language: None,
            hotkey: "Ctrl+Shift+Space".to_string(),
            paste_on_release: true,
            device_name: None,
            dark_mode: true, // Default to dark mode since user asked for it
            realtime: false,
            auto_start: false,
            vad_threshold: default_vad_threshold(),
        }
    }
}

/// Resolve path ke `settings.toml` di bawah `%APPDATA%`.
///
/// Fallback ke `./tabvoice_settings.toml` kalau `APPDATA` env var tidak di-set
/// (mis. ketika running di Linux selama dev).
pub fn config_path() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(appdata) => PathBuf::from(appdata).join(APP_DIR).join(FILE_NAME),
        Err(_) => {
            log::warn!("APPDATA env var not set, using ./tabvoice_settings.toml");
            PathBuf::from(FILE_NAME)
        }
    }
}

/// Pastikan parent directory dari `path` ada; create kalau belum.
/// Idempotent: kalau directory sudah ada, no-op.
fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
    }
    Ok(())
}

/// Load settings dari disk. Return default kalau file tidak ada.
/// Kalau file corrupt, log error dan return default.
pub fn load_or_default() -> Settings {
    let path = config_path();
    if !path.exists() {
        log::info!("Settings file not found at {}, using defaults", path.display());
        return Settings::default();
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to read settings from {}: {e}", path.display());
            return Settings::default();
        }
    };

    let mut loaded = match toml::from_str::<Settings>(&raw) {
        Ok(s) => {
            log::info!("Loaded settings from {}", path.display());
            s
        }
        Err(e) => {
            log::error!(
                "Failed to parse settings at {}: {e}; using defaults",
                path.display()
            );
            Settings::default()
        }
    };
    
    // Pastikan `model_path` selalu menggunakan folder `models/` di dekat exe.
    // Jika user punya file lama dengan format "models/ggml-base.bin", kita 
    // ambil nama filenya saja lalu satukan dengan `get_models_dir()`.
    if let Some(file_name) = loaded.model_path.file_name() {
        loaded.model_path = get_models_dir().join(file_name);
    } else {
        loaded.model_path = get_models_dir().join("ggml-base.bin");
    }
    
    loaded
}

/// Save settings ke disk. Create parent directory kalau belum ada.
/// File di-write sebagai pretty-printed TOML.
pub fn save(s: &Settings) -> Result<()> {
    let path = config_path();
    ensure_parent(&path)?;
    let serialized = toml::to_string_pretty(s).context("serializing settings to TOML")?;
    std::fs::write(&path, serialized)
        .with_context(|| format!("writing settings to {}", path.display()))?;
    log::info!("Saved settings to {}", path.display());
    
    // Apply auto_start to Windows Registry
    apply_auto_start(s.auto_start);
    
    Ok(())
}

/// Menambah atau menghapus TabVoice dari Windows Startup (Registry)
pub fn apply_auto_start(enable: bool) {
    let Ok(exe_path) = std::env::current_exe() else { return };
    let exe_path_str = exe_path.display().to_string();
    
    if enable {
        let _ = std::process::Command::new("reg")
            .args([
                "add",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v", "TabVoice",
                "/t", "REG_SZ",
                "/d", &exe_path_str,
                "/f"
            ])
            .spawn();
        log::info!("Registered to Windows Startup");
    } else {
        let _ = std::process::Command::new("reg")
            .args([
                "delete",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v", "TabVoice",
                "/f"
            ])
            .spawn();
        log::info!("Removed from Windows Startup");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_round_trip() {
        let s = Settings::default();
        let serialized = toml::to_string(&s).unwrap();
        let parsed: Settings = toml::from_str(&serialized).unwrap();
        assert_eq!(s.model_path, parsed.model_path);
        assert_eq!(s.hotkey, parsed.hotkey);
        assert_eq!(s.paste_on_release, parsed.paste_on_release);
        assert_eq!(s.language, parsed.language);
        assert_eq!(s.device_name, parsed.device_name);
        assert_eq!(s.dark_mode, parsed.dark_mode);
        assert_eq!(s.realtime, parsed.realtime);
    }

    #[test]
    fn config_path_is_non_empty() {
        let p = config_path();
        assert!(!p.as_os_str().is_empty());
        assert!(p.ends_with(FILE_NAME));
    }
}