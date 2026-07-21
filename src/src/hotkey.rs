//! Global push-to-talk handler.
//!
//! Default chord: `Ctrl+Shift+Space` (sama dengan Whispr Flow & Windows Speech Recognition).
//! Background thread baca dari `GlobalHotKeyEvent::receiver()` (static crossbeam channel
//! internal crate) dan:
//! 1. Forward `AppEvent` ke iced UI thread (untuk indikator UI),
//! 2. Trigger `MicCapture` start/stop (yang push samples ke `recorder.samples`
//!    dan emit `AppEvent::Amplitude`),
//! 3. Saat release: kirim samples ke `release_tx` (transcriber worker).
//! `HotkeyHandle` cuma marker — manager disimpan di `AppState`
//! (hidup terus selama proses); ganti shortcut lewat [`reregister_hotkey`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::audio;
use crate::events::{AppEvent, EventSender};
use crate::state::AppState;

/// Wrapper supaya `GlobalHotKeyManager` bisa di-share lintas thread.
/// SAFE karena manager cuma diakses dari main thread (UI thread) dan
/// listener thread cuma baca `CURRENT_HOTKEY` (bukan manager).
struct SendSyncManager(Mutex<Option<GlobalHotKeyManager>>);
unsafe impl Send for SendSyncManager {}
unsafe impl Sync for SendSyncManager {}

impl SendSyncManager {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<GlobalHotKeyManager>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Manager global hotkey — dibuat SEKALI (di `register_push_to_talk`, thread
/// utama) dan hidup terus selama proses. Disimpan di `OnceLock` supaya
/// shortcut bisa di-re-register (ganti tanpa restart) tanpa membuat manager baru.
static HOTKEY_MANAGER: OnceLock<SendSyncManager> = OnceLock::new();

/// Hotkey yang sedang aktif: dipakai untuk filter event di listener thread
/// DAN untuk re-registration. Di-update tiap kali register / re-register.
static CURRENT_HOTKEY: Mutex<Option<HotKey>> = Mutex::new(None);

/// Pastikan listener thread hanya di-spawn sekali.
static LISTENER_SPAWNED: AtomicBool = AtomicBool::new(false);

/// Handle penanda bahwa global hotkey sedang terdaftar.
///
/// Manager disimpan di static [`HOTKEY_MANAGER`] (bukan di-hold handle ini),
/// sehingga handle ini cuma marker — `Drop`-nya tidak unregister hotkey
/// (kita ganti shortcut lewat [`reregister_hotkey`] tanpa drop manager).
pub struct HotkeyHandle {
    /// ID hotkey yang sedang aktif (referensi/log saja).
    pub hotkey_id: u32,
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        log::debug!("HotkeyHandle dropped, hotkey id={}", self.hotkey_id);
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
    let manager = HOTKEY_MANAGER.get_or_init(|| {
        let mgr = GlobalHotKeyManager::new()
            .expect("Gagal membuat GlobalHotKeyManager");
        SendSyncManager(Mutex::new(Some(mgr)))
    });
    let mgr_guard = manager.lock();
    let hotkey = parse_hotkey_string(hotkey_str)
        .ok_or_else(|| anyhow::anyhow!("Format hotkey tidak valid: {}", hotkey_str))?;
    let hotkey_id = hotkey.id();
    mgr_guard.as_ref().unwrap().register(hotkey)
        .with_context(|| format!("Gagal mendaftarkan hotkey id={}", hotkey_id))?;
    drop(mgr_guard);

    *CURRENT_HOTKEY
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(hotkey);

    log::info!("Hotkey terdaftar: {} (id={})", hotkey_str, hotkey_id);

    // Spawn listener thread SEKALI (di-guard oleh LISTENER_SPAWNED).
    if !LISTENER_SPAWNED.swap(true, Ordering::SeqCst) {
        std::thread::Builder::new()
            .name("tabvoice-hotkey".to_string())
            .spawn(move || hotkey_listener_loop(event_tx, state))
            .context("Gagal spawn hotkey listener thread")?;
    }

    Ok(HotkeyHandle { hotkey_id })
}

/// Loop utama listener: polling global receiver, filter by id, dispatch ke handle_press/release.
fn hotkey_listener_loop(event_tx: EventSender, state: Arc<AppState>) {
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

        // Filter: hanya proses event dari hotkey yang sedang aktif. ID dibaca
        // tiap iterasi supaya listener otomatis mengikuti shortcut yang
        // di-re-register lewat settings (tanpa spawn ulang thread).
        let current_id = CURRENT_HOTKEY
            .lock()
            .map(|g| g.as_ref().map(|h| h.id()))
            .unwrap_or(None);
        let Some(id) = current_id else {
            continue;
        };
        if event.id != id {
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
    // 1. Cek apakah sudah ada mic aktif (double-press case). Ambil handle lama
    //    KELUAR dari Mutex, lalu drop di LUAR lock.
    //
    //    PENTING: Drop `MicCapture` = drop cpal `Stream` = join audio thread.
    //    Kalau kita drop mic sambil masih memegang `recorder.lock()`, dan pada
    //    saat yang sama audio callback (`on_samples`) sedang berjalan dan juga
    //    mengambil `recorder.lock()`, maka terjadi deadlock: thread hotkey
    //    menunggu audio thread selesai (join), tapi audio thread menunggu
    //    `recorder.lock()` yang kita pegang → aplikasi HANG ("ngeheng") dan
    //    shortcut mati. Makanya mic di-drop setelah lock dilepas.
    let old_mic = {
        let mut rec = state.recorder.lock().unwrap_or_else(|e| e.into_inner());
        let old = rec.mic.lock().unwrap_or_else(|e| e.into_inner()).take();
        rec.is_recording = true;
        rec.samples.clear();
        rec.last_partial_len = 0;
        rec.pasted_partial_text.clear();
        old
    };
    drop(old_mic);

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
        
    let device_name = state.settings.lock().unwrap_or_else(|e| e.into_inner()).device_name.clone();

    // 4. Start MicCapture dengan callback wrapper.
    let capture = match audio::start_capture(hint_rate, hint_channels, device_name.as_deref(), move |s, r| {
        on_samples(s, r)
    }) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Hotkey press: gagal start MicCapture: {e}");
            // Reset state agar UI tidak stuck di Recording.
            let mut rec = state.recorder.lock().unwrap_or_else(|e| e.into_inner());
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
        let rec = state.recorder.lock().unwrap_or_else(|e| e.into_inner());
        *rec.mic.lock().unwrap_or_else(|e| e.into_inner()) = Some(capture);
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
    // 1+2. Ambil MicCapture (stop stream) + drain samples. Mic di-drop di LUAR
    //    lock — lihat penjelasan deadlock di `handle_press`.
    let (old_mic, samples) = {
        let mut rec = state.recorder.lock().unwrap_or_else(|e| e.into_inner());
        let old = rec.mic.lock().unwrap_or_else(|e| e.into_inner()).take();
        // Drain samples.
        let samples = std::mem::take(&mut rec.samples);
        (old, samples)
        // `rec` di-drop di sini — lock dilepas sebelum blocking call / drop mic.
    };
    // Drop MicCapture di LUAR lock (join audio thread) — cegah deadlock.
    drop(old_mic);

    let sample_count = samples.len();
    log::info!(
        "Hotkey released: MicCapture stopped, {} samples ({:.2}s @16kHz)",
        sample_count,
        sample_count as f32 / 16_000.0
    );

    // 3. Kirim ke transcriber worker kalau channel masih hidup.
    if let Some(tx) = state.release_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if let Err(e) = tx.send(crate::transcriber::TranscriberInput::Final(samples)) {
            log::warn!("Gagal kirim samples ke release_tx (transcriber sudah drop?): {e}");
        }
    } else {
        log::warn!("release_tx belum di-set; samples di-drop");
    }

    // 4. Set is_recording = false.
    state.recorder.lock().unwrap_or_else(|e| e.into_inner()).is_recording = false;

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
            "up" => key_code = Some(ArrowUp),
            "down" => key_code = Some(ArrowDown),
            "left" => key_code = Some(ArrowLeft),
            "right" => key_code = Some(ArrowRight),
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
                // Single letter A-Z
                let chars: Vec<char> = lower.chars().collect();
                if chars.len() == 1 {
                    let c = chars[0];
                    if c.is_ascii_alphabetic() {
                        key_code = letter_to_code(c);
                        continue;
                    }
                    // Single digit 0-9
                    if c.is_ascii_digit() {
                        key_code = digit_to_code(c);
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

/// Petakan huruf ASCII ke `Code` keyboard (KeyA..KeyZ).
fn letter_to_code(c: char) -> Option<Code> {
    Some(match c {
        'a' => Code::KeyA,
        'b' => Code::KeyB,
        'c' => Code::KeyC,
        'd' => Code::KeyD,
        'e' => Code::KeyE,
        'f' => Code::KeyF,
        'g' => Code::KeyG,
        'h' => Code::KeyH,
        'i' => Code::KeyI,
        'j' => Code::KeyJ,
        'k' => Code::KeyK,
        'l' => Code::KeyL,
        'm' => Code::KeyM,
        'n' => Code::KeyN,
        'o' => Code::KeyO,
        'p' => Code::KeyP,
        'q' => Code::KeyQ,
        'r' => Code::KeyR,
        's' => Code::KeyS,
        't' => Code::KeyT,
        'u' => Code::KeyU,
        'v' => Code::KeyV,
        'w' => Code::KeyW,
        'x' => Code::KeyX,
        'y' => Code::KeyY,
        'z' => Code::KeyZ,
        _ => return None,
    })
}

/// Petakan digit ASCII ke `Code` keyboard (Digit0..Digit9).
fn digit_to_code(c: char) -> Option<Code> {
    Some(match c {
        '0' => Code::Digit0,
        '1' => Code::Digit1,
        '2' => Code::Digit2,
        '3' => Code::Digit3,
        '4' => Code::Digit4,
        '5' => Code::Digit5,
        '6' => Code::Digit6,
        '7' => Code::Digit7,
        '8' => Code::Digit8,
        '9' => Code::Digit9,
        _ => return None,
    })
}

/// String representasi hotkey yang sedang aktif (untuk revert saat re-register gagal).
pub fn current_hotkey_string() -> Option<String> {
    CURRENT_HOTKEY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|h| (*h).into_string())
}

/// Ganti shortcut global yang aktif tanpa restart aplikasi.
///
/// Dipanggil dari UI Settings (thread utama) saat user menyimpan hotkey baru.
/// Membatalkan pendaftaran hotkey lama lalu mendaftarkan yang baru di manager
/// yang sama.
pub fn reregister_hotkey(hotkey_str: &str) -> Result<()> {
    let mgr = HOTKEY_MANAGER
        .get()
        .context("GlobalHotKeyManager belum di-init (register_push_to_talk belum dipanggil)")?;

    let new_hotkey = parse_hotkey_string(hotkey_str)
        .ok_or_else(|| anyhow::anyhow!("Format hotkey tidak valid: {}", hotkey_str))?;

    // Batalkan pendaftaran hotkey lama (best-effort).
    let old = CURRENT_HOTKEY.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(old) = old {
        if let Some(m) = mgr.lock().as_mut() {
            let _ = m.unregister(old);
        }
    }

    // Daftarkan yang baru.
    if let Some(m) = mgr.lock().as_ref() {
        m.register(new_hotkey)
            .with_context(|| format!("Gagal mendaftarkan hotkey baru: {}", hotkey_str))?;
    } else {
        anyhow::bail!("GlobalHotKeyManager not available in manager");
    }

    *CURRENT_HOTKEY.lock().unwrap_or_else(|e| e.into_inner()) = Some(new_hotkey);

    log::info!(
        "Hotkey di-re-register ke: {} (id={})",
        hotkey_str,
        new_hotkey.id()
    );
    Ok(())
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
