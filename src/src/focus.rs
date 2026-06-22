use std::sync::atomic::{AtomicIsize, Ordering};
use std::thread;
use std::time::Duration;
#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::System::Threading::GetCurrentProcessId;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow};

static LAST_HWND: AtomicIsize = AtomicIsize::new(0);

/// Spawn thread pelacak fokus jendela.
#[cfg(windows)]
pub fn spawn_focus_tracker() {
    thread::spawn(|| {
        let my_pid = unsafe { GetCurrentProcessId() };
        loop {
            thread::sleep(Duration::from_millis(50));
            let hwnd = unsafe { GetForegroundWindow() };
            if !hwnd.0.is_null() {
                let mut pid = 0;
                unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
                if pid != my_pid {
                    LAST_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
                }
            }
        }
    });
}

#[cfg(not(windows))]
pub fn spawn_focus_tracker() {
    log::warn!("Window focus tracking is not implemented on non-Windows platforms yet.");
}

/// Kembalikan fokus ke jendela terakhir yang aktif.
#[cfg(windows)]
pub fn restore_focus() {
    let hwnd_val = LAST_HWND.load(Ordering::Relaxed);
    if hwnd_val != 0 {
        let hwnd = HWND(hwnd_val as *mut _);
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(windows))]
pub fn restore_focus() {
    log::warn!("Window focus restoring is not implemented on non-Windows platforms yet.");
}
