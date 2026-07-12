use std::fmt;
use std::mem;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, HWND_BROADCAST, PostMessageW, SMTO_ABORTIFHUNG, SMTO_NOTIMEOUTIFNOTHUNG,
    SendMessageTimeoutW, SendNotifyMessageW, WM_DWMCOLORIZATIONCOLORCHANGED, WM_SETTINGCHANGE,
    WM_SYSCOLORCHANGE, WM_THEMECHANGED,
};
use windows_sys::core::BOOL;

const THEME_REGISTRY_SUBKEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
const THEME_SETTINGS: [&str; 5] = [
    "ImmersiveColorSet",
    "WindowsThemeElement",
    "SystemUsesLightTheme",
    "AppsUseLightTheme",
    "Policy",
];
const THEME_MESSAGES: [u32; 3] = [
    WM_THEMECHANGED,
    WM_SYSCOLORCHANGE,
    WM_DWMCOLORIZATIONCOLORCHANGED,
];

#[derive(Debug)]
pub struct WindowsIntegrationError {
    context: &'static str,
    code: u32,
}

impl fmt::Display for WindowsIntegrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} (win32 error {})", self.context, self.code)
    }
}

impl std::error::Error for WindowsIntegrationError {}

#[derive(Clone, Copy)]
struct WindowsThemeState {
    apps_dark_mode: bool,
    system_dark_mode: bool,
}

pub fn windows_main_dark_mode() -> bool {
    windows_theme_state().system_dark_mode
}

pub fn next_windows_dark_mode() -> bool {
    let theme_state = windows_theme_state();

    if theme_state.apps_dark_mode == theme_state.system_dark_mode {
        !theme_state.system_dark_mode
    } else {
        theme_state.system_dark_mode
    }
}

pub fn set_windows_dark_mode(dark_mode: bool) -> Result<(), WindowsIntegrationError> {
    let light_theme_value = if dark_mode { 0u32 } else { 1u32 };

    set_theme_registry_value("SystemUsesLightTheme", light_theme_value)?;
    set_theme_registry_value("AppsUseLightTheme", light_theme_value)?;
    broadcast_theme_change();
    Ok(())
}

fn windows_theme_state() -> WindowsThemeState {
    WindowsThemeState {
        apps_dark_mode: theme_registry_dark_mode("AppsUseLightTheme"),
        system_dark_mode: theme_registry_dark_mode("SystemUsesLightTheme"),
    }
}

fn theme_registry_dark_mode(name: &'static str) -> bool {
    read_theme_registry_value(name) == Some(0)
}

fn read_theme_registry_value(name: &'static str) -> Option<u32> {
    let subkey = wide_null(THEME_REGISTRY_SUBKEY);
    let value_name = wide_null(name);
    let mut value = 1u32;
    let mut value_size = mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            &mut value as *mut u32 as *mut _,
            &mut value_size,
        )
    };

    (status == ERROR_SUCCESS).then_some(value)
}

fn set_theme_registry_value(name: &'static str, value: u32) -> Result<(), WindowsIntegrationError> {
    let subkey = wide_null(THEME_REGISTRY_SUBKEY);
    let value_name = wide_null(name);
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            REG_DWORD,
            &value as *const u32 as *const _,
            mem::size_of::<u32>() as u32,
        )
    };

    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(WindowsIntegrationError {
            context: "RegSetKeyValueW failed",
            code: status,
        })
    }
}

fn broadcast_theme_change() {
    update_per_user_system_parameters();

    for setting in THEME_SETTINGS {
        broadcast_setting_change(Some(setting));
    }
    broadcast_setting_change(None);
    broadcast_theme_messages(HWND_BROADCAST);
    notify_taskbar_windows();
    update_per_user_system_parameters();
}

fn broadcast_setting_change(setting: Option<&str>) {
    let setting = setting.map(wide_null);
    let setting_ptr = setting
        .as_ref()
        .map_or(0, |setting| setting.as_ptr() as LPARAM);

    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            setting_ptr,
            SMTO_ABORTIFHUNG | SMTO_NOTIMEOUTIFNOTHUNG,
            500,
            ptr::null_mut(),
        );
    }
}

fn notify_taskbar_windows() {
    for class_name in ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"] {
        let class_name = wide_null(class_name);
        let mut previous: HWND = ptr::null_mut();

        loop {
            let window = unsafe {
                FindWindowExW(ptr::null_mut(), previous, class_name.as_ptr(), ptr::null())
            };
            if window.is_null() {
                break;
            }

            notify_taskbar_window(window);
            previous = window;
        }
    }
}

fn notify_taskbar_window(window: HWND) {
    for setting in THEME_SETTINGS {
        let setting = wide_null(setting);
        unsafe {
            SendMessageTimeoutW(
                window,
                WM_SETTINGCHANGE,
                0,
                setting.as_ptr() as LPARAM,
                SMTO_ABORTIFHUNG | SMTO_NOTIMEOUTIFNOTHUNG,
                500,
                ptr::null_mut(),
            );
        }
    }

    broadcast_theme_messages(window);
}

fn broadcast_theme_messages(window: HWND) {
    for message in THEME_MESSAGES {
        unsafe {
            SendMessageTimeoutW(
                window,
                message,
                0,
                0,
                SMTO_ABORTIFHUNG | SMTO_NOTIMEOUTIFNOTHUNG,
                500,
                ptr::null_mut(),
            );
            SendNotifyMessageW(window, message, 0, 0);
            PostMessageW(window, message, 0, 0);
        }
    }
}

fn update_per_user_system_parameters() {
    type UpdatePerUserSystemParameters = unsafe extern "system" fn(u32, BOOL) -> BOOL;

    let library_name = wide_null("user32.dll");
    let module = unsafe { GetModuleHandleW(library_name.as_ptr()) };
    if module.is_null() {
        return;
    }

    let procedure =
        unsafe { GetProcAddress(module, c"UpdatePerUserSystemParameters".as_ptr().cast()) };
    let Some(procedure) = procedure else {
        return;
    };

    let update: UpdatePerUserSystemParameters = unsafe { mem::transmute(procedure) };
    unsafe {
        update(1, 1);
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
