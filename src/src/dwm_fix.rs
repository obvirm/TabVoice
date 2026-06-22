//! Disable DWM (Desktop Window Manager) rounded corner + drop shadow untuk
//! window iced, dan secara periodik memastikan window flags tetap benar.
//!
//! Latar belakang: di Windows 11, semua window borderless (`decorations: false`)
//! yang dibuat oleh winit akan di-render dengan rounded corner mask + drop shadow
//! oleh DWM. Efek ini muncul sebagai "kotak hitam" kecil di sekitar pill —
//! user melihat ini sebagai bug UI.
//!
//! Fix: cari HWND dari current process, lalu pakai `DwmSetWindowAttribute` untuk
//! set:
//! - `DWMWA_WINDOW_CORNER_PREFERENCE = 33` -> `DWMWCP_DONOTROUND = 1`
//! - `DWMWA_NCRENDERING_POLICY = 2` -> `DWMNCRP_DISABLED = 1` (non-client
//!   rendering OFF, yang juga drop drop shadow DWM)
//!
//! Selain itu, secara periodik re-apply:
//! - `WS_EX_TOOLWINDOW` agar tidak muncul di taskbar
//! - `WS_EX_TOPMOST` via `SetWindowPos(HWND_TOPMOST)` agar selalu di atas
//! - `WS_EX_NOACTIVATE` agar tidak mencuri focus
//!
//! Reference: https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmsetwindowattribute



#[cfg(windows)]

#[cfg(windows)]
use windows::Win32::Foundation::{BOOL, HWND};
#[cfg(windows)]
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
};
#[cfg(windows)]
use windows::Win32::System::Threading::GetCurrentProcessId;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible,
    SetWindowLongW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOPMOST,
};

#[cfg(windows)]
/// Find our main window HWND via EnumWindows + process PID matching.
/// Returns the first visible top-level window owned by this process.
unsafe fn find_our_hwnd() -> Option<HWND> {

    // Closure untuk callback. Karena EnumWindows butuh fn pointer (bukan
    // closure dengan captured state), pakai static atomic untuk simpan hasil.
    static mut FOUND_HWND: isize = 0;
    static mut FOUND_PID: u32 = 0;

    // SAFETY: Ini satu-shot, called from main thread sebelum runtime spawn.
    // `FOUND_HWND`/`FOUND_PID` hanya di-tulis di sini dan di-baca setelah
    // EnumWindows return, jadi tidak ada race.
    FOUND_HWND = 0;
    FOUND_PID = 0;

    unsafe extern "system" fn enum_proc(hwnd: HWND, _lparam: windows::Win32::Foundation::LPARAM) -> BOOL {
        let mut pid: u32 = 0;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let my_pid = GetCurrentProcessId();
        if pid == my_pid && IsWindowVisible(hwnd).as_bool() {
            FOUND_HWND = hwnd.0 as isize;
            FOUND_PID = pid;
            return BOOL(0); // FALSE = stop enum
        }
        BOOL(1) // TRUE = continue
    }

    let _ = EnumWindows(Some(enum_proc), windows::Win32::Foundation::LPARAM(0));

    if FOUND_HWND != 0 {
        Some(HWND(FOUND_HWND as *mut _))
    } else {
        None
    }
}

/// Disable DWM rounded corner + drop shadow untuk window current process,
/// dan spawn background thread yang secara periodik memastikan window flags
/// (always-on-top, no-taskbar, no-activate) tetap benar.
///
/// Harus dipanggil SETELAH window dibuat dan visible.
#[cfg(windows)]
pub fn disable_dwm_effects() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::thread::Builder::new()
            .name("dwm-fix".to_string())
            .spawn(|| {
                // Fase 1: Retry sampai window visible (max 5 detik).
                let mut hwnd_found = None;
                for _ in 0..50 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if let Some(hwnd) = unsafe { find_our_hwnd() } {
                        apply_dwm(hwnd);
                        apply_window_flags(hwnd);
                        hwnd_found = Some(hwnd);
                        break;
                    }
                }

                let Some(hwnd) = hwnd_found else {
                    log::warn!("dwm_fix: window HWND not found dalam 5 detik, skip");
                    return;
                };

                // Fase 2: Periodik re-apply setiap 3 detik.
                // Ini memastikan kalau Windows mereset style (misalnya karena
                // DPI change, monitor reconnect, Windows update, dll),
                // pill tetap always-on-top dan tidak muncul di taskbar.
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    apply_window_flags(hwnd);
                }
            })
            .expect("gagal spawn dwm-fix thread");
    });
}

#[cfg(not(windows))]
pub fn disable_dwm_effects() {
    // No-op for non-Windows
}

#[cfg(windows)]
fn apply_dwm(hwnd: HWND) {
    unsafe {
        // Disable rounded corner: set DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND.
        let corner_pref: i32 = DWMWCP_DONOTROUND.0;
        let res1 = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_pref as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );
        match res1 {
            Ok(()) => log::info!("DWM rounded corner disabled"),
            Err(e) => log::warn!("DWM rounded corner disable gagal: {e}"),
        }
    }
}

#[cfg(windows)]
fn apply_window_flags(hwnd: HWND) {
    unsafe {
        // Baca extended style saat ini
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;

        // Tambahkan WS_EX_TOOLWINDOW (hide from taskbar) dan WS_EX_NOACTIVATE
        let desired = ex_style
            | WS_EX_TOOLWINDOW.0
            | WS_EX_NOACTIVATE.0;

        if ex_style != desired {
            SetWindowLongW(hwnd, GWL_EXSTYLE, desired as i32);
            log::debug!("Re-applied WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE");
        }

        // Re-apply HWND_TOPMOST (always on top)
        // Ini idempotent -- SetWindowPos dengan HWND_TOPMOST tidak menyebabkan flicker
        if ex_style & WS_EX_TOPMOST.0 == 0 {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            log::debug!("Re-applied HWND_TOPMOST");
        }
    }
}
