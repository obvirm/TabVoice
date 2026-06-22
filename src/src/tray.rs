//! System tray icon dengan context menu.
//!
//! Pakai Win32 `Shell_NotifyIconW` langsung via `windows` crate 0.58.
//! Icon embedded di binary pakai `include_bytes!` — tidak ada runtime file lookup.
//! Fallback ke `IDI_APPLICATION` kalau extraction gagal (sangat jarang).

#![cfg(windows)]

use std::io::Write;
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
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DispatchMessageW, GDI_IMAGE_TYPE, GetCursorPos,
    GetMessageW, HMENU, HICON, LoadIconW, PostQuitMessage, RegisterClassExW,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, WNDCLASSEXW,
    IDI_APPLICATION, IMAGE_FLAGS, LR_LOADFROMFILE, LR_SHARED, MF_SEPARATOR,
    MF_STRING, MSG, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_COMMAND,
    WM_DESTROY, WM_RBUTTONUP, WM_USER, WINDOW_EX_STYLE, WINDOW_STYLE, HWND_MESSAGE,
};

use crate::events::{AppEvent, TrayAction};

// Embed tray icon binary langsung di compile-time. Selalu ada di mana pun
// binary dijalankan — tidak ada runtime file lookup.
const TRAY_ICON_BYTES: &[u8] = include_bytes!("../tray_icon.ico");

const WM_TRAYICON: u32 = WM_USER + 1;

const IDM_SETTINGS: usize = 1001;
const IDM_RELOAD_MODEL: usize = 1002;
const IDM_QUIT: usize = 1003;

const CLASS_NAME: windows::core::PCWSTR = w!("TabVoiceTrayClass");
const WINDOW_NAME: windows::core::PCWSTR = w!("TabVoice Tray Message Window");

const TRAY_TIP: &str = "TabVoice - Ctrl+Shift+Space to dictate";

static EVENT_TX: OnceLock<Sender<AppEvent>> = OnceLock::new();

/// Holds the HICON yang di-extract dari embedded bytes, supaya bisa
/// di-DestroyIcon saat cleanup.
struct IconHandle(HICON);
unsafe impl Send for IconHandle {}
unsafe impl Sync for IconHandle {}

/// Handle untuk system tray icon yang sedang aktif.
pub struct TrayHandle {
    hwnd: HWND,
    icon: Option<IconHandle>,
    cleaned: bool,
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        if !self.cleaned {
            unsafe { cleanup_inner(self.hwnd, self.icon.take()) };
        }
    }
}

/// Inisialisasi system tray icon dan menempelkannya ke aplikasi.
pub fn init(event_tx: Sender<AppEvent>) -> Result<TrayHandle> {
    if EVENT_TX.set(event_tx).is_err() {
        log::warn!("EVENT_TX sudah di-set, init tray dipanggil 2x?");
    }

    unsafe {
        let h_module = GetModuleHandleW(None).context("GetModuleHandleW gagal")?;
        let h_instance: HINSTANCE = h_module.into();

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

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            WINDOW_NAME,
            WINDOW_STYLE(0),
            0, 0, 0, 0,
            HWND_MESSAGE,
            HMENU(std::ptr::null_mut()),
            h_instance,
            None,
        )
        .context("CreateWindowExW gagal")?;

        let icon = load_embedded_icon().context("gagal extract tray icon")?;

        let mut sz_tip = [0u16; 128];
        let tip_wide: Vec<u16> = TRAY_TIP
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let copy_len = tip_wide.len().min(sz_tip.len());
        sz_tip[..copy_len].copy_from_slice(&tip_wide[..copy_len]);

        let nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: icon.0,
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
            let _ = DestroyIcon(icon.0);
            return Err(anyhow::anyhow!(
                "Shell_NotifyIconW(NIM_ADD) gagal (BOOL={})",
                ok.0
            ));
        }

        let hwnd_raw = hwnd.0 as isize;
        std::thread::Builder::new()
            .name("tabvoice-tray".to_string())
            .spawn(move || message_loop(HWND(hwnd_raw as *mut _)))
            .context("Gagal spawn tray message-loop thread")?;

        log::info!("System tray icon registered (tip={TRAY_TIP:?})");

        Ok(TrayHandle {
            hwnd,
            icon: Some(icon),
            cleaned: false,
        })
    }
}

/// Extract embedded ICO bytes → tulis ke temp file → LoadImageW dari file.
/// `CreateIconFromResourceEx` tidak stabil di `windows` 0.58 untuk 32-bit BGRA,
/// jadi pakai temp file approach.
fn load_embedded_icon() -> Result<IconHandle> {
    let temp_dir = std::env::temp_dir();
    let temp_path = temp_dir.join(format!("tabvoice_tray_{}.ico", std::process::id()));
    {
        let mut f = std::fs::File::create(&temp_path)
            .with_context(|| format!("create temp {}", temp_path.display()))?;
        f.write_all(TRAY_ICON_BYTES)
            .with_context(|| format!("write temp {}", temp_path.display()))?;
    }

    unsafe {
        let wide: Vec<u16> = temp_path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let cpath = windows::core::PCWSTR(wide.as_ptr());
        let result = windows::Win32::UI::WindowsAndMessaging::LoadImageW(
            None,
            cpath,
            GDI_IMAGE_TYPE(1), // IMAGE_ICON
            32,
            32,
            IMAGE_FLAGS(LR_LOADFROMFILE.0 | LR_SHARED.0),
        );

        let _ = std::fs::remove_file(&temp_path);

        match result {
            Ok(handle) => {
                log::info!("Tray icon loaded from embedded bytes");
                Ok(IconHandle(HICON(handle.0)))
            }
            Err(e) => {
                log::warn!("LoadImageW gagal ({e}), fallback IDI_APPLICATION");
                LoadIconW(None, IDI_APPLICATION)
                    .map(IconHandle)
                    .context("LoadIconW IDI_APPLICATION gagal")
            }
        }
    }
}

/// Bersihkan tray icon saat aplikasi ditutup.
pub fn cleanup(mut handle: TrayHandle) {
    unsafe { cleanup_inner(handle.hwnd, handle.icon.take()) };
    handle.cleaned = true;
    log::info!("Tray icon removed");
}

unsafe fn cleanup_inner(hwnd: HWND, icon: Option<IconHandle>) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    let _ = DestroyWindow(hwnd);
    if let Some(IconHandle(h)) = icon {
        let _ = DestroyIcon(h);
    }
    let _ = PostQuitMessage(0);
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event = lparam.0 as u32;
            if event == WM_RBUTTONUP {
                show_context_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let menu_id = (wparam.0 & 0xFFFF) as usize;
            forward_menu_action(menu_id);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

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

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);

    let cmd = TrackPopupMenu(
        menu,
        TPM_NONOTIFY | TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x, pt.y,
        0,
        hwnd,
        None,
    );

    let _ = DestroyMenu(menu);

    if cmd.0 != 0 {
        forward_menu_action(cmd.0 as usize);
    }
}

fn forward_menu_action(menu_id: usize) {
    let action = match menu_id {
        IDM_SETTINGS => TrayAction::OpenSettings,
        IDM_RELOAD_MODEL => TrayAction::ReloadModel,
        IDM_QUIT => TrayAction::Quit,
        _ => return,
    };

    if let Some(tx) = EVENT_TX.get() {
        if tx.send(AppEvent::TrayAction(action)).is_err() {
            log::debug!("AppEvent channel tutup, tray forwarder drop message");
        }
    } else {
        log::warn!("EVENT_TX belum di-init, menu action terbuang");
    }
}

fn message_loop(hwnd: HWND) {
    unsafe {
        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, hwnd, 0, 0);
            if !result.as_bool() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    log::debug!("Tray message loop exited (hwnd={:?})", hwnd.0);
}