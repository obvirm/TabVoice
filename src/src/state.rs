//! Shared state + UI-local state.
//!
//! `AppState` di-share via `Arc` ke: UI thread, audio capture callback, hotkey
//! callback, dan transcriber worker. Karena semua field di belakang `Mutex`,
//! lock critical section diusahakan pendek (drop guard pattern).

use std::sync::{Arc, Mutex};

use oxiwhisper::WhisperModel;
use tokio::sync::mpsc::UnboundedSender;

use crate::audio::MicCapture;
use crate::settings::Settings;

/// State internal recorder — di-share antara hotkey callback (yang set
/// `is_recording`) dan audio capture callback (yang push samples).
#[derive(Default)]
pub struct RecorderState {
    /// `true` lagi merekam (hotkey di-hold).
    pub is_recording: bool,
    /// Buffer samples 16 kHz mono f32 sejak hotkey ditekan.
    pub samples: Vec<f32>,
    /// Handle ke microphone capture aktif. `Some` saat recording, `None` saat idle.
    /// Drop = stop stream (lihat [`MicCapture::Drop`]).
    pub mic: Mutex<Option<MicCapture>>,
}

/// State bersama seluruh aplikasi.
///
/// Di-share sebagai `Arc<AppState>` antara:
/// - UI thread (read `settings`, `recorder.is_recording` untuk indikator)
/// - Audio callback (push samples ke `recorder.samples`, emit `Amplitude` event)
/// - Hotkey callback (toggle `recorder.is_recording`, drain & kirim ke `release_tx`)
/// - Transcriber worker (read `settings.language`, replace `model` setelah reload)
pub struct AppState {
    /// Model Whisper yang sudah di-load; `None` sebelum init atau setelah unload.
    pub model: Mutex<Option<Arc<WhisperModel>>>,
    /// State recorder (shared antara hotkey + audio callback).
    pub recorder: Mutex<RecorderState>,
    /// Sender ke transcriber worker. Di-set sekali saat startup, di-drop saat shutdown.
    pub release_tx: Mutex<Option<UnboundedSender<Vec<f32>>>>,
    /// Settings persistent (di-load dari disk di startup, di-save saat user edit).
    pub settings: Mutex<Settings>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            model: Mutex::new(None),
            recorder: Mutex::new(RecorderState::default()),
            release_tx: Mutex::new(None),
            settings: Mutex::new(Settings::default()),
        }
    }
}