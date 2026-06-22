#![windows_subsystem = "windows"]

//! Entry point untuk binary `tabvoice`.
//!
//! Phase 6 orchestration:
//! 1. Setup logger
//! 2. Load settings
//! 3. Load WhisperModel (blocking)
//! 4. Setup event channel (AppEvent dari threads)
//! 5. Setup release channel (samples ke transcriber)
//! 6. Spawn transcriber worker di tokio
//! 7. Register global hotkey
//! 8. Init system tray
//! 9. Launch iced UI (window + subscription Tick)

use std::sync::{Arc, Mutex};

use eframe::egui;

use tabvoice_lib::app::{AppFlags, TabVoice};
use tabvoice_lib::{events, hotkey, settings, state as app_state, transcriber, tray};

fn main() -> eframe::Result<()> {
    // 1. Init logger (default level `info`, override via RUST_LOG).
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .try_init();

    // 2. Load settings dari %APPDATA%\TabVoice\settings.toml (atau default).
    let settings = settings::load_or_default();
    log::info!("Settings loaded: model={:?}", settings.model_path);

    // 3. Load WhisperModel jika MemoryMode::LoadOnStartup
    //    Jika mode lain, model akan di-load saat hotkey ditekan pertama kali.
    let model_opt = if settings.memory_mode == tabvoice_lib::settings::MemoryMode::LoadOnStartup {
        const DEFAULT_MODEL_BYTES: &[u8] = include_bytes!("../../models/ggml-base.bin");

        let m = match tabvoice_lib::ffi::WhisperModel::from_file(&settings.model_path) {
            Ok(m) => Arc::new(m),
            Err(e) => {
                log::warn!(
                    "Gagal load model dari file {:?}: {e}. Memuat model bawaan (embedded)...",
                    settings.model_path
                );
                match tabvoice_lib::ffi::WhisperModel::from_buffer(DEFAULT_MODEL_BYTES) {
                    Ok(m) => Arc::new(m),
                    Err(e2) => {
                        let msg = format!("Gagal memuat model bawaan (embedded): {e2}.");
                        log::error!("{msg}");
                        eprintln!("Error: {msg}");
                        std::process::exit(1);
                    }
                }
            }
        };

        log::info!(
            "WhisperModel loaded early: type={}, multilingual={}",
            m.model_type_readable(),
            m.is_multilingual()
        );
        Some(m)
    } else {
        log::info!("Memory mode is {:?}. Model will be lazy-loaded.", settings.memory_mode);
        None
    };

    // 4. Setup channels.
    //    - event_tx/rx: AppEvent antara background threads (audio, hotkey,
    //      tray, transcriber) ke iced UI thread.
    //    - release_tx/rx: samples audio (Vec<f32> 16kHz mono) dari hotkey
    //      callback ke transcriber worker.
    let (event_tx, event_rx) = events::channel();
    let (release_tx, release_rx) = tokio::sync::mpsc::unbounded_channel();

    // 5. Shared state lintas thread. `Mutex` di setiap field agar lock
    //    bisa dilepas sebelum await / blocking call.
    let app_state = Arc::new(app_state::AppState {
        model: Mutex::new(model_opt),
        recorder: Mutex::new(app_state::RecorderState::default()),
        release_tx: Mutex::new(Some(release_tx)),
        settings: Mutex::new(settings.clone()),
    });

    // 6. Spawn tokio runtime multi-thread (untuk transcriber worker).
    //    Box::leak supaya runtime tidak pernah di-drop selama app hidup -
    //    kalau runtime di-drop, semua spawned task langsung di-cancel.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");
    let rt_handle = rt.handle().clone();
    Box::leak(Box::new(rt));

    // 7. Spawn transcriber worker di tokio runtime.
    let transcriber = transcriber::Transcriber::new(settings.language.clone(), Arc::clone(&app_state));
    let event_tx_for_transcriber = event_tx.clone();
    rt_handle.spawn(async move {
        transcriber.run_loop(release_rx, event_tx_for_transcriber).await;
    });

    // 8. Register global hotkey (Ctrl+Shift+Space) dan spawn listener thread.
    //    Listener butuh Arc<AppState> untuk start/stop MicCapture saat press/release.
    let _hotkey_handle = hotkey::register_push_to_talk(&settings.hotkey, event_tx.clone(), app_state.clone())
        .expect("Failed to register hotkey Ctrl+Shift+Space");

    // 9. Init system tray icon + context menu. Tray spawn thread sendiri
    //    untuk message loop `GetMessageW`.
    let _tray_handle = tray::init(event_tx.clone()).expect("Failed to init system tray");

    // 10. Build AppFlags & launch egui UI.
    let flags = AppFlags {
        event_rx,
        event_tx: event_tx.clone(),
        state: app_state,
    };

    log::info!("TabVoice startup complete - launching egui UI");

    // Install global keyboard hook to block Space during recording
    tabvoice_lib::keyboard_hook::install_hook();

    // Start background thread to constantly track the last active window
    tabvoice_lib::focus::spawn_focus_tracker();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_inner_size([52.0, 52.0])
            .with_resizable(false)
            .with_taskbar(false),
        ..Default::default()
    };

    // Spawn a thread to apply WS_EX_NOACTIVATE and disable DWM shadows once the window appears
    tabvoice_lib::dwm_fix::disable_dwm_effects();

    eframe::run_native(
        "TabVoice",
        options,
        Box::new(|_cc| Box::new(TabVoice::new(flags))),
    )
}