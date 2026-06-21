//! Auto-paste hasil transkrip ke window aktif.
//!
//! Flow: `copy_to_clipboard` -> sleep 30ms -> `send_paste` (Ctrl+V via Win32 `SendInput`).
//!
//! Catatan: `SendInput` di-block oleh UIPI kalau app elevated. Mitigasi:
//! manifest `requestedExecutionLevel="asInvoker"`.

use std::thread;
use std::time::Duration;

use anyhow::Context;

/// Tulis `text` ke Windows clipboard via `arboard`.
///
/// Bisa gagal kalau clipboard sedang di-hold exclusive oleh app lain (jarang).
/// Clipboard di-set tanpa clear eksplisit — text menetap di clipboard sampai
/// ditimpa copy berikutnya (acceptable untuk use case push-to-talk).
pub fn copy_to_clipboard(text: &str) -> anyhow::Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("Gagal membuka Windows clipboard")?;
    clipboard
        .set_text(text.to_owned())
        .context("Gagal menulis ke clipboard")?;
    log::debug!("Clipboard set: {} chars", text.len());
    Ok(())
}

/// Kirim chord Ctrl+V via Win32 `SendInput`. Windows only.
///
/// Susun 4 event: Ctrl down, V down, V up, Ctrl up. Return error kalau
/// jumlah event yang berhasil dikirim < jumlah yang diminta (indikasi
/// `SendInput` di-reject oleh UIPI atau thread belum punya focus).
#[cfg(windows)]
pub fn send_paste() -> anyhow::Result<()> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_V,
    };

    // 4 INPUT events: Ctrl down, V down, V up, Ctrl up.
    let ctrl_down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_CONTROL.0),
                wScan: 0,
                dwFlags: KEYBD_EVENT_FLAGS(0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let v_down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_V.0),
                wScan: 0,
                dwFlags: KEYBD_EVENT_FLAGS(0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let v_up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_V.0),
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let ctrl_up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_CONTROL.0),
                wScan: 0,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    let inputs = [ctrl_down, v_down, v_up, ctrl_up];
    let sent = unsafe {
        SendInput(
            &inputs,
            std::mem::size_of::<INPUT>() as i32,
        )
    };

    if sent as usize != inputs.len() {
        anyhow::bail!(
            "SendInput hanya mengirim {sent} dari {} events (UIPI block? foreground window tidak fokus?)",
            inputs.len()
        );
    }

    log::debug!("SendInput Ctrl+V: {} events terkirim", sent);
    Ok(())
}

/// Stub non-Windows: TabVoice target Windows, tapi sediakan stub agar
/// `cargo check` di host non-Windows tidak fail.
#[cfg(not(windows))]
pub fn send_paste() -> anyhow::Result<()> {
    anyhow::bail!("send_paste hanya diimplementasi untuk Windows")
}

/// Convenience: copy ke clipboard + sleep 30ms + kirim Ctrl+V.
///
/// Sleep 30ms memberi waktu OS untuk flush clipboard ke reader (Notepad,
/// browser, dsb) sebelum `SendInput` trigger paste. Tanpa delay, sebagian
/// app tidak sempat baca clipboard content.
pub fn paste_text(text: &str) -> anyhow::Result<()> {
    if text.is_empty() {
        log::debug!("paste_text: text kosong, skip");
        return Ok(());
    }

    copy_to_clipboard(text).context("copy_to_clipboard gagal")?;
    thread::sleep(Duration::from_millis(30));
    send_paste().context("send_paste gagal")?;

    log::info!("Pasted {} chars", text.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `paste_text` dengan empty string harus no-op tanpa error.
    #[test]
    fn paste_text_empty_noop() {
        paste_text("").expect("empty paste should succeed");
    }

    /// `copy_to_clipboard` smoke test — tulis lalu baca balik via `arboard`.
    /// Skip di non-Windows (arboard mungkin bermasalah di headless CI).
    #[cfg(windows)]
    #[test]
    fn copy_to_clipboard_roundtrip() {
        let marker = "tabvoice-test-marker-7c1f";
        copy_to_clipboard(marker).expect("copy should succeed");
        let mut clipboard = arboard::Clipboard::new().expect("open clipboard");
        let read = clipboard.get_text().expect("read clipboard");
        assert_eq!(read, marker);
    }
}
