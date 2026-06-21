//! System tray icon dengan context menu.
//!
//! Pakai Win32 `Shell_NotifyIconW` langsung via `windows` crate 0.58 —
//! tanpa dependensi `tray-icon`. Message-only window (parent `HWND_MESSAGE`)
//! menerima callback tray-icon, lalu show popup menu saat user right-click.
//!
//! Flow:
//! 1. `init()` — register window class, buat message-only HWND, attach icon
//!    ke system tray, spawn thread message loop.
//! 2. `WndProc` handle `WM_TRAYICON` (extract menu event dari lparam),
//!    `WM_COMMAND` (extract menu id), dan `WM_DESTROY` (cleanup).
//! 3. User pilih menu item → forward ke `AppEvent::TrayAction` via static
//!    `EVENT_TX` (`OnceLock` karena WndProc adalah `unsafe extern "system" fn`
//!    yang tidak bisa capture environment).
//! 4. `cleanup()` (optional) — hapus icon dari tray, destroy window.

#![cfg(windows)]

use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NOTIFYICONDATAW, NIM_ADD, NIM_DELETE,
    NIF_ICON, NIF_MESSAGE, NIF_TIP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, HMENU, HICON,
    LoadIconW, PostQuitMessage, RegisterClassExW, SetForegroundWindow,
    TrackPopupMenu, TranslateMessage, WNDCLASSEXW, IDI_APPLICATION,
    MF_SEPARATOR, MF_STRING, MSG, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_COMMAND, WM_DESTROY, WM_RBUTTONUP, WM_USER, WINDOW_EX_STYLE, WINDOW_STYLE,
    HWND_MESSAGE,
};

use crate::events::{AppEvent, TrayAction};

// WM_USER = 1024, +1 supaya tidak bentrok dengan message lain.
const WM_TRAYICON: u32 = WM_USER + 1;

// Menu item IDs.
const IDM_SETTINGS: usize = 1001;
const IDM_RELOAD_MODEL: usize = 1002;
const IDM_QUIT: usize = 1003;

const CLASS_NAME: windows::core::PCWSTR = w!("TabVoiceTrayClass");
const WINDOW_NAME: windows::core::PCWSTR = w!("TabVoice Tray Message Window");

const TRAY_TIP: &str = "TabVoice - Ctrl+Shift+Space to dictate";

/// Static sender ke `AppEvent` channel.
///
/// `WndProc` adalah `unsafe extern "system" fn` — tidak bisa capture
/// environment. Pakai `OnceLock` agar `EVENT_TX.set(...)` cuma dipanggil
/// sekali di `init()`, dan WndProc bisa baca via `EVENT_TX.get()`.
static EVENT_TX: OnceLock<Sender<AppEvent>> = OnceLock::new();

/// Handle yang owns `HWND` tray message window.
///
/// Drop tidak otomatis hapus icon dari system tray — panggil
/// [`cleanup`] secara eksplisit sebelum drop kalau mau.
pub struct TrayHandle {
    hwnd: HWND,
    /// Set true setelah [`cleanup`] dipanggil agar Drop tidak double-cleanup.
    cleaned: bool,
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        if !self.cleaned {
            // Best-effort cleanup kalau caller lupa panggil `cleanup()`.
            unsafe { cleanup_inner(self.hwnd) };
        }
    }
}

/// Init system tray: register class, buat message-only window, attach icon.
///
/// `event_tx` adalah clone dari sender `AppEvent` channel yang di-own UI.
/// Hanya boleh dipanggil SATU kali per proses (karena `EVENT_TX` static).
pub fn init(event_tx: Sender<AppEvent>) -> Result<TrayHandle> {
    // Set static sender — ignore error kalau sudah pernah di-set
    // (seharusnya tidak boleh, tapi defensive).
    if EVENT_TX.set(event_tx).is_err() {
        log::warn!("EVENT_TX sudah di-set, init tray dipanggil 2x?");
    }

    unsafe {
        // Module handle untuk WNDCLASSEXW.hInstance.
        let h_module = GetModuleHandleW(None).context("GetModuleHandleW gagal")?;
        let h_instance: HINSTANCE = h_module.into();

        // Register window class.
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: Default::default(),
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: h_instance,
            hIcon: HICON(std::ptr::null_mut()),
            hCursor: Default::default(),
            hbrBackground: Default::default(),
            lpszMenuName: w!(""),
            lpszClassName: CLASS_NAME,
            hIconSm: HICON(std::ptr::null_mut()),
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            return Err(anyhow::anyhow!("RegisterClassExW gagal (atom=0)"));
        }

        // Create message-only window (HWND_MESSAGE parent = -3, tidak visible).
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            WINDOW_NAME,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            HMENU(std::ptr::null_mut()),
            h_instance,
            None,
        )
        .context("CreateWindowExW gagal")?;

        // Load default app icon — `IDI_APPLICATION` adalah predefined icon.
        let h_icon = LoadIconW(None, IDI_APPLICATION).context("LoadIconW gagal")?;

        // Susun szTip (wide string, null-terminated, max 127 chars + NUL).
        let mut sz_tip = [0u16; 128];
        let tip_wide: Vec<u16> = TRAY_TIP
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let copy_len = tip_wide.len().min(sz_tip.len());
        sz_tip[..copy_len].copy_from_slice(&tip_wide[..copy_len]);

        // Bangun NOTIFYICONDATAW — ukuran struct harus di-set di cbSize
        // (OS baca ukuran ini untuk validasi versi).
        let nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: h_icon,
            szTip: sz_tip,
            dwState: Default::default(),
            dwStateMask: Default::default(),
            szInfo: [0u16; 256],
            Anonymous: Default::default(),
            szInfoTitle: [0u16; 64],
            dwInfoFlags: Default::default(),
            guidItem: Default::default(),
            hBalloonIcon: HICON(std::ptr::null_mut()),
        };

        let ok = Shell_NotifyIconW(NIM_ADD, &nid);
        if !ok.as_bool() {
            return Err(anyhow::anyhow!(
                "Shell_NotifyIconW(NIM_ADD) gagal (BOOL={})",
                ok.0
            ));
        }

        // Spawn thread message loop — GetMessageW blocking sampai WM_QUIT.
        // HWND (`*mut c_void`) tidak Send, convert ke isize untuk dikirim.
        let hwnd_raw = hwnd.0 as isize;
        std::thread::Builder::new()
            .name("tabvoice-tray".to_string())
            .spawn(move || message_loop(HWND(hwnd_raw as *mut _)))
            .context("Gagal spawn tray message-loop thread")?;

        log::info!("System tray icon registered (tip={TRAY_TIP:?})");

        Ok(TrayHandle {
            hwnd,
            cleaned: false,
        })
    }
}

/// Cleanup explicit — hapus icon dari tray, destroy window, post WM_QUIT.
///
/// Aman dipanggil lebih dari sekali (`TrayHandle::Drop` cek `cleaned` flag).
pub fn cleanup(mut handle: TrayHandle) {
    unsafe { cleanup_inner(handle.hwnd) };
    handle.cleaned = true;
    log::info!("Tray icon removed");
}

/// Inner cleanup — `unsafe` karena akses HWND langsung.
unsafe fn cleanup_inner(hwnd: HWND) {
    // Bangun minimal NOTIFYICONDATAW untuk NIM_DELETE (cukup cbSize + hWnd + uID).
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);

    let _ = DestroyWindow(hwnd);
    let _ = PostQuitMessage(0);
}

/// `WndProc` — dipanggil oleh Windows untuk message window.
///
/// TIDAK boleh capture environment (harus `extern "system"`). Pakai
/// `EVENT_TX.get()` untuk akses channel sender.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            // Low-word lparam = event (WM_RBUTTONUP, WM_LBUTTONDBLCLK, dll).
            let event = lparam.0 as u32;
            if event == WM_RBUTTONUP {
                show_context_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            // Low-word wparam = menu ID dari item yang dipilih user.
            let menu_id = (wparam.0 & 0xFFFF) as usize;
            forward_menu_action(menu_id);
            LRESULT(0)
        }
        WM_DESTROY => {
            // Window sedang dihancurkan — keluar dari message loop.
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Build + show context menu di posisi cursor, lalu forward pilihan user.
unsafe fn show_context_menu(hwnd: HWND) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(e) => {
            log::error!("CreatePopupMenu gagal: {e}");
            return;
        }
    };

    let _ = AppendMenuW(menu, MF_STRING, IDM_SETTINGS, w!("Settings..."));
    let _ = AppendMenuW(menu, MF_STRING, IDM_RELOAD_MODEL, w!("Reload Model"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, w!(""));
    let _ = AppendMenuW(menu, MF_STRING, IDM_QUIT, w!("Quit"));

    // Posisi cursor — pakai `POINT::default()` dulu kalau GetCursorPos gagal.
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    // SetForegroundWindow dibutuhkan supaya menu close properly saat user
    // klik di luar (workaround Win32 quirk).
    let _ = SetForegroundWindow(hwnd);

    // TrackPopupMenu dengan TPM_RETURNCMD — return value adalah menu ID
    // yang dipilih user, atau 0 kalau cancel (Esc / klik di luar).
    let cmd = TrackPopupMenu(
        menu,
        TPM_NONOTIFY | TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        None,
    );

    // Cleanup menu.
    let _ = DestroyMenu(menu);

    // Forward pilihan user (cmd = menu ID, 0 = cancel).
    if cmd.0 != 0 {
        forward_menu_action(cmd.0 as usize);
    }
}

/// Map menu ID ke `TrayAction` lalu kirim lewat `EVENT_TX`.
fn forward_menu_action(menu_id: usize) {
    let action = match menu_id {
        IDM_SETTINGS => TrayAction::OpenSettings,
        IDM_RELOAD_MODEL => TrayAction::ReloadModel,
        IDM_QUIT => TrayAction::Quit,
        _ => return, // Unknown ID — ignore.
    };

    if let Some(tx) = EVENT_TX.get() {
        if tx.send(AppEvent::TrayAction(action)).is_err() {
            // UI thread sudah drop receiver-nya (app shutdown).
            log::debug!("AppEvent channel tutup, tray forwarder drop message");
        }
    } else {
        log::warn!("EVENT_TX belum di-init, menu action terbuang");
    }
}

/// Message loop di thread terpisah.
///
/// Blocking pada `GetMessageW` sampai `WM_QUIT` di-post (lewat
/// `cleanup()` atau window destroy).
fn message_loop(hwnd: HWND) {
    unsafe {
        let mut msg = MSG::default();
        loop {
            // GetMessageW return: >0 = message, 0 = WM_QUIT, -1 = error.
            let result = GetMessageW(&mut msg, hwnd, 0, 0);
            if !result.as_bool() {
                // 0 = WM_QUIT, -1 = error — keluar loop.
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    log::debug!("Tray message loop exited (hwnd={:?})", hwnd.0);
}