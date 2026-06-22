//! Disable DWM (Desktop Window Manager) rounded corner + drop shadow untuk
//! window iced.
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
//! Catatan: HWND iced window tidak di-expose secara publik. Cara ini pakai
//! `EnumWindows` + `GetWindowThreadProcessId` matching ke PID kita untuk
//! menemukan HWND. Aman karena hanya window dengan PID kita yang match.
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
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
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

/// Disable DWM rounded corner + drop shadow untuk window current process.
/// Idempotent — kedua kali call akan no-op.
///
/// Harus dipanggil SETELAH window dibuat dan visible. Recommended: spawn thread
/// terpisah yang retry setiap 100ms sampai window found, max 5 detik.
#[cfg(windows)]
pub fn disable_dwm_effects() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::thread::Builder::new()
            .name("dwm-fix".to_string())
            .spawn(|| {
                // Retry sampai window visible (max 5 detik).
                for _ in 0..50 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if let Some(hwnd) = unsafe { find_our_hwnd() } {
                        apply(hwnd);
                        return;
                    }
                }
                log::warn!("dwm_fix: window HWND not found dalam 5 detik, skip");
            })
            .expect("gagal spawn dwm-fix thread");
    });
}

#[cfg(not(windows))]
pub fn disable_dwm_effects() {
    // No-op for non-Windows
}

#[cfg(windows)]
fn apply(hwnd: HWND) {
    unsafe {
        // 1. Disable rounded corner: set DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND.
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
