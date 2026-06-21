//! Event enum yang dikirim dari background threads ke iced UI thread.
//!
//! Pattern: background worker (audio capture, hotkey listener, tray, transcriber)
//! mengirim `AppEvent` lewat `std::sync::mpsc::channel`; UI thread drain receiver-nya
//! di subscription iced.

use std::sync::mpsc::{Receiver, Sender};

/// Event yang dikirim dari background threads (audio, hotkey, tray, transcriber)
/// ke iced UI thread lewat `std::sync::mpsc::channel`.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// Level amplitude RMS (0.0 - 1.0) untuk waveform UI (Phase 2+).
    Amplitude {
        /// Nilai RMS ter-normalisasi (0.0 = silent, 1.0 = clip).
        value: f32,
    },
    /// Hotkey ditekan → mulai recording.
    HotkeyPressed,
    /// Hotkey dilepas → stop recording.
    HotkeyReleased,
    /// Aksi dari tray icon menu (Phase 6).
    TrayAction(TrayAction),
    /// Transkrip selesai (Phase 3+).
    Done {
        /// Teks hasil transkrip yang akan di-paste.
        text: String,
    },
    /// Error recoverable — tampilkan toast / log.
    Error {
        /// Pesan error human-readable.
        message: String,
    },
}

/// Aksi yang dikirim dari system tray menu (Phase 6).
#[derive(Debug, Clone)]
pub enum TrayAction {
    /// Reload model Whisper (mis. setelah download model baru).
    ReloadModel,
    /// Buka window settings.
    OpenSettings,
    /// Keluar dari aplikasi.
    Quit,
}

/// Type alias untuk channel sender.
pub type EventSender = Sender<AppEvent>;
/// Type alias untuk channel receiver (di-own UI thread).
pub type EventReceiver = Receiver<AppEvent>;

/// Buat channel baru. Convenience wrapper.
pub fn channel() -> (EventSender, EventReceiver) {
    std::sync::mpsc::channel()
}
