//! Global push-to-talk handler.
//!
//! Default chord: `Ctrl+Shift+Space` (sama dengan Whispr Flow & Windows Speech Recognition).
//! Background thread baca dari `GlobalHotKeyEvent::receiver()` (static crossbeam channel
//! internal crate) dan:
//! 1. Forward `AppEvent` ke iced UI thread (untuk indikator UI),
//! 2. Trigger `MicCapture` start/stop (yang push samples ke `recorder.samples`
//!    dan emit `AppEvent::Amplitude`),
//! 3. Saat release: kirim samples ke `release_tx` (transcriber worker).
//!
//! `HotkeyHandle` owns `GlobalHotKeyManager` — saat handle di-drop, manager drop →
//! OS unregister hotkey otomatis (Windows: `UnregisterHotKey` di destructor).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::audio;
use crate::events::{AppEvent, EventSender};
use crate::state::AppState;

/// Global guard: pastikan hanya SATU push-to-talk yang aktif per-proses.
/// `register_push_to_talk` akan return error kalau handle sebelumnya masih hidup.
static HOTKEY_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Handle yang owns `GlobalHotKeyManager` + id hotkey.
///
/// PENTING: jangan di-drop sebelum app selesai — kalau di-drop, hotkey ke-unregister
/// dari OS (Windows: `UnregisterHotKey`) dan listener thread kehilangan filter.
pub struct HotkeyHandle {
    /// Global hotkey manager (Windows: pegang thread-local Win32 state).
    pub manager: GlobalHotKeyManager,
    /// ID dari hotkey yang kita register (dipakai filter event di listener thread).
    pub hotkey_id: u32,
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        // Lepas idempotency guard agar bisa register ulang (mis. setelah reload settings).
        HOTKEY_REGISTERED.store(false, Ordering::SeqCst);
        log::debug!(
            "HotkeyHandle dropped, hotkey id={} unregistered",
            self.hotkey_id
        );
    }
}

/// Daftarkan push-to-talk default (`Ctrl+Shift+Space`) dan spawn listener thread.
///
/// # Args
/// - `event_tx`: sender ke `AppEvent` channel yang di-own iced UI thread.
/// - `state`: shared state lintas thread. Hotkey listener (di thread `tabvoice-hotkey`)
///   akan start `MicCapture` saat press dan stop + drain samples saat release.
///
/// # Returns
/// - `Ok(HotkeyHandle)` — pegang handle ini di main thread agar manager tetap hidup.
/// - `Err` — kalau hotkey sudah terdaftar, manager creation gagal, atau register
///   bentrok dengan chord yang dipakai app lain.
///
/// # Catatan
/// Pakai polling receiver (bukan `set_event_handler`) karena `set_event_handler`'s
/// closure butuh `Fn + Send + Sync`, sedangkan `std::sync::mpsc::Sender` hanya `Send`.
/// Polling menghindari overhead `Arc<Mutex<Sender>>`.
pub fn register_push_to_talk(
    hotkey_str: &str,
    event_tx: EventSender,
    state: Arc<AppState>,
) -> Result<HotkeyHandle> {
    // Idempotency guard: tolak kalau handle sebelumnya masih hidup.
    if HOTKEY_REGISTERED.swap(true, Ordering::SeqCst) {
        anyhow::bail!("Push-to-talk hotkey sudah terdaftar (HotkeyHandle masih hidup)");
    }

    // Inner closure: kalau ada step yang gagal, reset guard di outer.
    let result: Result<HotkeyHandle> = (|| {
        let manager = GlobalHotKeyManager::new()
            .context("Gagal membuat GlobalHotKeyManager")?;

        let hotkey = parse_hotkey_string(hotkey_str)
            .ok_or_else(|| anyhow::anyhow!("Format hotkey tidak valid: {}", hotkey_str))?;
        
        let hotkey_id = hotkey.id();

        manager
            .register(hotkey)
            .with_context(|| format!("Gagal mendaftarkan hotkey id={}", hotkey_id))?;

        log::info!(
            "Hotkey terdaftar: {} (id={})",
            hotkey_str,
            hotkey_id
        );

        // Spawn thread forwarder: polling global receiver → handle press/release.
        std::thread::Builder::new()
            .name("tabvoice-hotkey".to_string())
            .spawn(move || hotkey_listener_loop(hotkey_id, event_tx, state))
            .context("Gagal spawn hotkey listener thread")?;

        Ok(HotkeyHandle { manager, hotkey_id })
    })();

    if result.is_err() {
        // Reset guard supaya caller bisa retry.
        HOTKEY_REGISTERED.store(false, Ordering::SeqCst);
    }

    result
}

/// Loop utama listener: polling global receiver, filter by id, dispatch ke handle_press/release.
fn hotkey_listener_loop(hotkey_id: u32, event_tx: EventSender, state: Arc<AppState>) {
    let receiver = GlobalHotKeyEvent::receiver();
    loop {
        let event = match receiver.recv() {
            Ok(ev) => ev,
            Err(_) => {
                // Crossbeam channel putus (crate shutdown / proses exit).
                log::debug!("GlobalHotKeyEvent receiver terputus, listener keluar");
                return;
            }
        };

        // Filter: hanya proses event dari hotkey kita (abaikan event dari hotkey lain
        // yang di-register app/process lain).
        if event.id != hotkey_id {
            continue;
        }

        match event.state {
            HotKeyState::Pressed => {
                if crate::app::IS_ASSIGNING_HOTKEY.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = event_tx.send(AppEvent::ActiveHotkeyCaptured);
                    continue;
                }
                crate::keyboard_hook::set_block_space(true);
                handle_press(&event_tx, &state);
                // Forward ke UI (untuk logging / indicator sekunder; UI utama
                // di-drive oleh Amplitude events).
                if event_tx.send(AppEvent::HotkeyPressed).is_err() {
                    log::debug!("Channel AppEvent tutup saat press, hotkey listener keluar");
                    return;
                }
            }
            HotKeyState::Released => {
                if crate::app::IS_ASSIGNING_HOTKEY.load(std::sync::atomic::Ordering::Relaxed) {
                    continue;
                }
                crate::keyboard_hook::set_block_space(false);
                handle_release(&event_tx, &state);
                if event_tx.send(AppEvent::HotkeyReleased).is_err() {
                    log::debug!("Channel AppEvent tutup saat release, hotkey listener keluar");
                    return;
                }
            }
        }
    }
}

/// Handle hotkey press: mulai microphone capture.
///
/// Steps:
/// 1. Lock `state.recorder`, set `is_recording = true`, clear `samples`.
/// 2. Bangun callback yang: append samples ke `recorder.samples` dan emit
///    `AppEvent::Amplitude` ke event_tx.
/// 3. Start `MicCapture` (pakai device sample rate & channels default),
///    simpan handle ke `recorder.mic`.
pub fn handle_press(event_tx: &EventSender, state: &Arc<AppState>) {
    // 1. Cek apakah sudah ada mic aktif (double-press case). Kalau ada, drop dulu
    //    supaya tidak ada stream leak.
    {
        let mut rec = state.recorder.lock().unwrap();
        if rec.mic.lock().unwrap().is_some() {
            log::warn!("Hotkey press: mic handle sudah ada, drop dulu");
            rec.mic.lock().unwrap().take();
        }
        rec.is_recording = true;
        rec.samples.clear();
        rec.last_partial_len = 0;
        rec.pasted_partial_text.clear();
    }

    // 2. Bangun callback yang capture event_tx & state (clone Arc).
    let event_tx_cb = event_tx.clone();
    let state_cb = Arc::clone(state);
    let on_samples: Arc<dyn Fn(Vec<f32>, f32) + Send + Sync> =
        Arc::new(move |samples: Vec<f32>, rms: f32| {
            // Append samples ke recorder.samples (lock brief).
            let mut emit_partial = None;
            {
                let Ok(mut rec) = state_cb.recorder.lock() else {
                    log::error!("recorder mutex poisoned in audio callback");
                    return;
                };
                rec.samples.extend(samples);

                let is_realtime = state_cb.settings.lock().map(|s| s.realtime).unwrap_or(false);
                if is_realtime {
                    let len = rec.samples.len();
                    // 1600 samples pada 16kHz = 100 ms
                    if len - rec.last_partial_len >= 1600 {
                        rec.last_partial_len = len;
                        emit_partial = Some(rec.samples.clone());
                    }
                }
            }
            if let Some(partial_samples) = emit_partial {
                if let Ok(guard) = state_cb.release_tx.lock() {
                    if let Some(tx) = guard.as_ref() {
                        let _ = tx.send(crate::transcriber::TranscriberInput::Partial(partial_samples));
                    }
                }
            }
            // Emit Amplitude event untuk UI waveform.
            let _ = event_tx_cb.send(AppEvent::Amplitude { value: rms });
        });

    // 3. Resolve device sample rate & channels dari default input device.
    //    Kalau device detection gagal, fallback ke 48000 Hz / 1 channel (mono mic)
    //    — MicCapture::start_capture akan tetap mencoba query device-nya sendiri,
    //    jadi hint ini cuma untuk logging.
    let (hint_rate, hint_channels) =
        detect_default_input_config().unwrap_or((48_000, 1));
        
    let device_name = state.settings.lock().unwrap().device_name.clone();

    // 4. Start MicCapture dengan callback wrapper.
    let capture = match audio::start_capture(hint_rate, hint_channels, device_name.as_deref(), move |s, r| {
        on_samples(s, r)
    }) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Hotkey press: gagal start MicCapture: {e}");
            // Reset state agar UI tidak stuck di Recording.
            let mut rec = state.recorder.lock().unwrap();
            rec.is_recording = false;
            rec.samples.clear();
            let _ = event_tx.send(AppEvent::Error {
                message: format!("Mic error: {e}"),
            });
            return;
        }
    };

    // 5. Simpan handle. Lock sekali — sebelumnya kita sudah lock untuk is_recording/samples,
    //    sekarang lock lagi untuk assign mic. Sebenarnya bisa digabung, tapi begini lebih
    //    jelas boundary-nya.
    {
        let rec = state.recorder.lock().unwrap();
        *rec.mic.lock().unwrap() = Some(capture);
    }

    log::info!("Hotkey pressed: MicCapture started");
}

/// Handle hotkey release: stop microphone capture, kirim samples ke transcriber.
///
/// Steps:
/// 1. Drop MicCapture (stop stream).
/// 2. Clone `recorder.samples` dan kirim ke `release_tx` (kalau ada).
/// 3. Set `is_recording = false`.
/// 4. Emit `AppEvent::Amplitude { 0.0 }` untuk reset waveform UI.
pub fn handle_release(event_tx: &EventSender, state: &Arc<AppState>) {
    // 1+2. Stop MicCapture + drain samples dalam satu critical section.
    let samples = {
        let mut rec = state.recorder.lock().unwrap();
        // Drop MicCapture (stop stream).
        rec.mic.lock().unwrap().take();
        // Drain samples.
        std::mem::take(&mut rec.samples)
        // `rec` di-drop di sini — lock dilepas sebelum blocking call (send ke channel).
    };

    let sample_count = samples.len();
    log::info!(
        "Hotkey released: MicCapture stopped, {} samples ({:.2}s @16kHz)",
        sample_count,
        sample_count as f32 / 16_000.0
    );

    // 3. Kirim ke transcriber worker kalau channel masih hidup.
    if let Some(tx) = state.release_tx.lock().unwrap().as_ref() {
        if let Err(e) = tx.send(crate::transcriber::TranscriberInput::Final(samples)) {
            log::warn!("Gagal kirim samples ke release_tx (transcriber sudah drop?): {e}");
        }
    } else {
        log::warn!("release_tx belum di-set; samples di-drop");
    }

    // 4. Set is_recording = false.
    state.recorder.lock().unwrap().is_recording = false;

    // 5. Emit Amplitude 0.0 untuk reset waveform UI.
    let _ = event_tx.send(AppEvent::Amplitude { value: 0.0 });
}

/// Detect default input device's sample rate & channels.
/// Return `None` kalau tidak ada device / device tidak expose config.
fn detect_default_input_config() -> Option<(u32, u16)> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let device = host.default_input_device()?;
    let config = device.default_input_config().ok()?;
    Some((config.sample_rate().0, config.channels()))
}

/// Parse string "Ctrl+Shift+Space" jadi `HotKey` (untuk Phase 6 settings.toml).
///
/// Format: token dipisah `+`, case-insensitive.
/// Modifier: `ctrl`/`control`, `shift`, `alt`, `super`/`meta`/`win`/`cmd`.
/// Key: `space`, `enter`/`return`, `tab`, `escape`/`esc`, `backspace`, `f1`..`f12`.
///
/// Return `None` kalau format tidak dikenali atau tidak ada key.
pub fn parse_hotkey_string(s: &str) -> Option<HotKey> {
    use Code::*;

    let mut modifiers = Modifiers::empty();
    let mut key_code: Option<Code> = None;

    for token in s.split('+') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" => modifiers |= Modifiers::ALT,
            "super" | "meta" | "win" | "cmd" => modifiers |= Modifiers::META,
            "space" => key_code = Some(Space),
            "enter" | "return" => key_code = Some(Enter),
            "tab" => key_code = Some(Tab),
            "escape" | "esc" => key_code = Some(Escape),
            "backspace" => key_code = Some(Backspace),
            _ => {
                // F1..F12
                if let Some(n) = lower.strip_prefix('f') {
                    if let Ok(num) = n.parse::<u8>() {
                        key_code = match num {
                            1 => Some(F1),
                            2 => Some(F2),
                            3 => Some(F3),
                            4 => Some(F4),
                            5 => Some(F5),
                            6 => Some(F6),
                            7 => Some(F7),
                            8 => Some(F8),
                            9 => Some(F9),
                            10 => Some(F10),
                            11 => Some(F11),
                            12 => Some(F12),
                            _ => return None,
                        };
                        continue;
                    }
                }
                // Token tidak dikenali.
                return None;
            }
        }
    }

    let code = key_code?;
    let mods = if modifiers == Modifiers::empty() {
        None
    } else {
        Some(modifiers)
    };
    Some(HotKey::new(mods, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_chord() {
        let hk = parse_hotkey_string("Ctrl+Shift+Space").expect("should parse");
        assert_eq!(hk.id(), HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space).id());
    }

    #[test]
    fn parse_lowercase() {
        let hk = parse_hotkey_string("shift+space").expect("should parse");
        assert_eq!(hk.id(), HotKey::new(Some(Modifiers::SHIFT), Code::Space).id());
    }

    #[test]
    fn parse_unknown_key_returns_none() {
        assert!(parse_hotkey_string("Ctrl+Banana").is_none());
    }

    #[test]
    fn parse_only_modifier_returns_none() {
        assert!(parse_hotkey_string("Ctrl+Shift").is_none());
    }
}
