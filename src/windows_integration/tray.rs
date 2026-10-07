use std::mem;
use std::ptr;

use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Shell::{NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GA_ROOT, GetAncestor, GetClassNameW, GetWindowThreadProcessId, HWND_MESSAGE,
    WindowFromPoint,
};

use super::wide_null;

// Slint's Windows tray backend registers its icon on a message-only window of
// this class, always with the same icon id.
const SLINT_TRAY_WINDOW_CLASS: &str = "SlintSystemTrayWindow";
const SLINT_TRAY_ICON_ID: u32 = 1;

// Top-level windows that can host notification area icons: the primary
// taskbar and the hidden-icons overflow flyout (Windows 10 and 11).
const NOTIFICATION_AREA_CLASSES: [&str; 3] = [
    "Shell_TrayWnd",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
];

/// Cheap check that is safe to run inside the low-level mouse hook. It only
/// tells whether the point is over a window that can show tray icons.
pub(super) fn point_is_in_notification_area(x: i32, y: i32) -> bool {
    let window = unsafe { WindowFromPoint(POINT { x, y }) };
    if window.is_null() {
        return false;
    }

    let root = unsafe { GetAncestor(window, GA_ROOT) };
    let mut class_name = [0u16; 64];
    let length = unsafe { GetClassNameW(root, class_name.as_mut_ptr(), class_name.len() as i32) };
    if length <= 0 {
        return false;
    }

    let class_name = &class_name[..length as usize];
    NOTIFICATION_AREA_CLASSES
        .iter()
        .any(|candidate| candidate.encode_utf16().eq(class_name.iter().copied()))
}

pub fn point_is_over_tray_icon(x: i32, y: i32) -> bool {
    let Some(window) = own_tray_window() else {
        return false;
    };

    let identifier = NOTIFYICONIDENTIFIER {
        cbSize: mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: window,
        uID: SLINT_TRAY_ICON_ID,
        ..unsafe { mem::zeroed() }
    };
    let mut rect = RECT::default();
    let result = unsafe { Shell_NotifyIconGetRect(&identifier, &mut rect) };

    result >= 0 && x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

fn own_tray_window() -> Option<HWND> {
    let class_name = wide_null(SLINT_TRAY_WINDOW_CLASS);
    let process_id = unsafe { GetCurrentProcessId() };
    let mut window = ptr::null_mut();

    loop {
        window = unsafe { FindWindowExW(HWND_MESSAGE, window, class_name.as_ptr(), ptr::null()) };
        if window.is_null() {
            return None;
        }

        let mut owner_process_id = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner_process_id) };
        if owner_process_id == process_id {
            return Some(window);
        }
    }
}
