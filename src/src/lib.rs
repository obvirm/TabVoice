//! Library root untuk `tabvoice_lib`.
//!
//! Modul-modul di sini dipakai bersama oleh binary `tabvoice` dan (nantinya)
//! integration tests. Struktur:
//!
//! - `app`     — struct utama yang mengimplementasikan `iced::Application`
//! - `state`   — `AppState` bersama + `UiState` untuk UI lokal
//! - `events`  — `AppEvent` enum + mpsc channel (Phase 2)
//! - `message` — `Message` enum untuk iced (Phase 2)
//! - `view`    — fungsi view() (Phase 5)
//! - `audio`   — cpal capture + rubato resample (Phase 2)
//! - `transcriber` — wrapper whisper.cpp FFI + tokio worker (Phase 3)
//! - `ffi`     — direct FFI wrapper untuk whisper.cpp (mirror oxiwhisper API)
//! - `hotkey`  — global-hotkey handler (Phase 2)
//! - `paste`   — arboard + SendInput (Phase 4)
//! - `settings` — TOML config (Phase 3)
//! - `tray`    — Shell_NotifyIconW via windows crate (Phase 6)


pub mod app;
pub mod audio;
pub mod dwm_fix;
pub mod events;
pub mod ffi;
pub mod hotkey;
pub mod paste;
pub mod settings;
pub mod state;
pub mod transcriber;
#[cfg(windows)]
#[path = "tray.rs"]
pub mod tray;

#[cfg(not(windows))]
pub mod tray {
    use std::sync::mpsc::Sender;
    use std::thread;
    use crate::events::{AppEvent, TrayAction};
    use anyhow::Result;
    use tray_icon::{
        menu::{Menu, MenuItem, PredefinedMenuItem},
        TrayIconBuilder, TrayIcon,
    };

    pub struct TrayHandle {
        _tray: TrayIcon,
    }

    pub fn init(event_tx: Sender<AppEvent>) -> Result<TrayHandle> {
        let tray_menu = Menu::new();
        let item_settings = MenuItem::new("Settings", true, None);
        let item_reload = MenuItem::new("Reload Model", true, None);
        let item_quit = MenuItem::new("Quit", true, None);
        
        let _ = tray_menu.append_items(&[
            &item_settings,
            &item_reload,
            &PredefinedMenuItem::separator(),
            &item_quit,
        ]);

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("TabVoice")
            .build().map_err(|e| anyhow::anyhow!("Tray error: {:?}", e))?;

        let menu_channel = tray_icon::menu::MenuEvent::receiver();
        let tray_channel = tray_icon::TrayIconEvent::receiver();

        thread::spawn(move || {
            loop {
                if let Ok(event) = menu_channel.try_recv() {
                    if event.id == item_settings.id() {
                        let _ = event_tx.send(AppEvent::TrayAction(TrayAction::OpenSettings));
                    } else if event.id == item_reload.id() {
                        let _ = event_tx.send(AppEvent::TrayAction(TrayAction::ReloadModel));
                    } else if event.id == item_quit.id() {
                        let _ = event_tx.send(AppEvent::TrayAction(TrayAction::Quit));
                    }
                }
                if let Ok(event) = tray_channel.try_recv() {
                    if let tray_icon::TrayIconEvent::Click { button: tray_icon::MouseButton::Left, .. } = event {
                        let _ = event_tx.send(AppEvent::TrayAction(TrayAction::OpenSettings));
                    }
                }
                thread::sleep(std::time::Duration::from_millis(50));
            }
        });

        Ok(TrayHandle { _tray: tray })
    }

    pub fn cleanup(_handle: TrayHandle) {}
}
pub mod keyboard_hook;
pub mod focus;
