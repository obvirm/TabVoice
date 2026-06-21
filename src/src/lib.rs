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
//! - `transcriber` — wrapper oxiwhisper + tokio worker (Phase 3)
//! - `hotkey`  — global-hotkey handler (Phase 2)
//! - `paste`   — arboard + SendInput (Phase 4)
//! - `settings` — TOML config (Phase 3)
//! - `tray`    — Shell_NotifyIconW via windows crate (Phase 6)

#![warn(missing_docs)]

pub mod app;
pub mod audio;
pub mod events;
pub mod hotkey;
pub mod paste;
pub mod settings;
pub mod state;
pub mod transcriber;
pub mod tray;
pub mod view;
