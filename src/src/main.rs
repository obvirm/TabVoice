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

use iced::Task;
use iced::application;
use iced::window::{Level, Position, Settings as WindowSettings};
use iced::Size;

use tabvoice_lib::app::{self, AppFlags};
use tabvoice_lib::{events, hotkey, settings, state as app_state, transcriber, tray};

/// Mengunduh model Whisper GGML otomatis dari HuggingFace menggunakan `curl` bawaan Windows.
fn download_model(model_path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = model_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let model_name = model_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Nama file model tidak valid"))?;

    let url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        model_name
    );

    log::info!("Mengunduh model otomatis dari {} ke {}...", url, model_path.display());
    println!("Mengunduh model Whisper ({}) otomatis. Harap tunggu...", model_name);

    let status = std::process::Command::new("curl")
        .arg("-L")
        .arg("-o")
        .arg(model_path)
        .arg(&url)
        .status()?;

    if !status.success() {
        return Err(anyhow::anyhow!(
            "Gagal mengunduh model via curl (exit code: {:?})",
            status.code()
        ));
    }

    log::info!("Model {} berhasil diunduh!", model_name);
    Ok(())
}

fn main() -> iced::Result {
    // 1. Init logger (default level `info`, override via RUST_LOG).
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .try_init();

    // 2. Load settings dari %APPDATA%\TabVoice\settings.toml (atau default).
    let settings = settings::load_or_default();
    log::info!("Settings loaded: model={:?}", settings.model_path);

    // Cek keberadaan model, jika belum ada unduh secara otomatis.
    if !settings.model_path.exists() {
        if let Err(e) = download_model(&settings.model_path) {
            let msg = format!(
                "Gagal mengunduh model otomatis ke {:?}: {e}",
                settings.model_path
            );
            log::error!("{msg}");
            eprintln!("Error: {msg}");
            std::process::exit(1);
        }
    }

    // 3. Load WhisperModel (blocking, butuh ~detik untuk model besar).
    //    Kalau gagal, exit dengan error friendly - tanpa model, transcriber
    //    worker tidak bisa inference.
    let model = match oxiwhisper::WhisperModel::from_file(&settings.model_path) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            let msg = format!(
                "Gagal load model {:?}: {e}. Pastikan file model ada di lokasi tersebut.",
                settings.model_path
            );
            log::error!("{msg}");
            eprintln!("Error: {msg}");
            std::process::exit(1);
        }
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
        model: Mutex::new(Some(Arc::clone(&model))),
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
    let transcriber = transcriber::Transcriber::new(model, settings.language.clone());
    let event_tx_for_transcriber = event_tx.clone();
    rt_handle.spawn(async move {
        transcriber.run_loop(release_rx, event_tx_for_transcriber).await;
    });

    // 8. Register global hotkey (Ctrl+Shift+Space) dan spawn listener thread.
    //    Listener butuh Arc<AppState> untuk start/stop MicCapture saat press/release.
    let _hotkey_handle = hotkey::register_push_to_talk(event_tx.clone(), app_state.clone())
        .expect("Failed to register hotkey Ctrl+Shift+Space");

    // 9. Init system tray icon + context menu. Tray spawn thread sendiri
    //    untuk message loop `GetMessageW`.
    let _tray_handle = tray::init(event_tx.clone()).expect("Failed to init system tray");

    // 10. Build AppFlags & launch iced UI. UI loop akan drain event_rx
    //     di subscription Tick 60fps (lihat app::subscription).
    let flags = AppFlags {
        event_rx,
        event_tx: event_tx.clone(),
        state: app_state,
    };

    log::info!("TabVoice startup complete - launching iced UI");

    application("TabVoice", app::update, app::view)
        .subscription(app::subscription)
        .style(|_state, _theme| iced::application::Appearance {
            background_color: iced::Color::TRANSPARENT,
            text_color: iced::Color::WHITE,
        })
        .window(WindowSettings {
            size: Size::new(600.0, 80.0),
            position: Position::Centered,
            transparent: true,
            decorations: false,
            level: Level::AlwaysOnTop,
            resizable: false,
            ..Default::default()
        })
        .run_with(|| (app::TabVoice::new(flags), Task::none()))
}