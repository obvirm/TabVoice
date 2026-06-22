use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

#[cfg(windows)]
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::VK_SPACE;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, HHOOK, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_SYSKEYDOWN,
};

#[cfg(windows)]
struct HookWrapper(#[allow(dead_code)] HHOOK);
#[cfg(windows)]
unsafe impl Send for HookWrapper {}
#[cfg(windows)]
unsafe impl Sync for HookWrapper {}

static BLOCK_SPACE: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
static HOOK_HANDLE: OnceLock<HookWrapper> = OnceLock::new();

/// Menentukan apakah tombol spasi harus diblokir atau tidak.
pub fn set_block_space(block: bool) {
    BLOCK_SPACE.store(block, Ordering::SeqCst);
}

/// Mengecek apakah pemblokiran spasi sedang aktif.
pub fn is_blocking_space() -> bool {
    BLOCK_SPACE.load(Ordering::SeqCst)
}

#[cfg(windows)]
extern "system" fn hook_callback(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ncode >= 0 && is_blocking_space() {
        let event = wparam.0 as u32;
        if event == WM_KEYDOWN || event == WM_SYSKEYDOWN {
            let kbd = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if kbd.vkCode == VK_SPACE.0 as u32 {
                // Block the space key!
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, ncode, wparam, lparam) }
}

/// Memasang global keyboard hook (LowLevel) untuk mencegat penekanan tombol.
#[cfg(windows)]
pub fn install_hook() {
    if HOOK_HANDLE.get().is_none() {
        unsafe {
            let hook =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_callback), HINSTANCE::default(), 0);
            if let Ok(h) = hook {
                let _ = HOOK_HANDLE.set(HookWrapper(h));
                log::info!("Global keyboard hook installed to block space when recording");
            } else {
                log::error!("Failed to install global keyboard hook");
            }
        }
    }
}

#[cfg(not(windows))]
pub fn install_hook() {
    log::warn!("Global keyboard hook is not implemented on non-Windows platforms yet.");
}

