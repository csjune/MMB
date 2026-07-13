use std::fmt;
use std::mem;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, FreeLibrary, HWND, LPARAM,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, REG_DWORD, REG_NOTIFY_CHANGE_LAST_SET, RRF_RT_REG_DWORD,
    RegCloseKey, RegDeleteKeyValueW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
    RegSetKeyValueW,
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
const SET_PREFERRED_APP_MODE_ORDINAL: usize = 135;
const FLUSH_MENU_THEMES_ORDINAL: usize = 136;
const PREFERRED_APP_MODE_FORCE_DARK: i32 = 2;
const PREFERRED_APP_MODE_FORCE_LIGHT: i32 = 3;

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

pub struct WindowsThemeWatcher {
    key: HKEY,
}

impl WindowsThemeWatcher {
    pub fn new() -> Result<Self, WindowsIntegrationError> {
        let subkey = wide_null(THEME_REGISTRY_SUBKEY);
        let mut key = ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_NOTIFY, &mut key) };
        if status == ERROR_SUCCESS {
            Ok(Self { key })
        } else {
            Err(WindowsIntegrationError {
                context: "RegOpenKeyExW theme watcher failed",
                code: status,
            })
        }
    }

    pub fn wait_for_change(&self) -> Result<(), WindowsIntegrationError> {
        let status = unsafe {
            RegNotifyChangeKeyValue(self.key, 0, REG_NOTIFY_CHANGE_LAST_SET, ptr::null_mut(), 0)
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(WindowsIntegrationError {
                context: "RegNotifyChangeKeyValue failed",
                code: status,
            })
        }
    }
}

impl Drop for WindowsThemeWatcher {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.key);
        }
    }
}

type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
type FlushMenuThemes = unsafe extern "system" fn();

struct MenuThemeApi {
    set_preferred_app_mode: SetPreferredAppMode,
    flush_menu_themes: FlushMenuThemes,
}

pub fn set_process_menu_dark_mode(dark_mode: bool) {
    let Some(api) = menu_theme_api() else {
        return;
    };
    let mode = if dark_mode {
        PREFERRED_APP_MODE_FORCE_DARK
    } else {
        PREFERRED_APP_MODE_FORCE_LIGHT
    };
    unsafe {
        (api.set_preferred_app_mode)(mode);
        (api.flush_menu_themes)();
    }
}

fn menu_theme_api() -> Option<&'static MenuThemeApi> {
    static API: OnceLock<Option<MenuThemeApi>> = OnceLock::new();
    API.get_or_init(load_menu_theme_api).as_ref()
}

fn load_menu_theme_api() -> Option<MenuThemeApi> {
    let library_name = wide_null("uxtheme.dll");
    let module = unsafe { LoadLibraryW(library_name.as_ptr()) };
    if module.is_null() {
        return None;
    }

    let set_mode = unsafe { GetProcAddress(module, SET_PREFERRED_APP_MODE_ORDINAL as *const u8) };
    let flush = unsafe { GetProcAddress(module, FLUSH_MENU_THEMES_ORDINAL as *const u8) };
    let (Some(set_mode), Some(flush)) = (set_mode, flush) else {
        unsafe {
            FreeLibrary(module);
        }
        return None;
    };

    Some(MenuThemeApi {
        set_preferred_app_mode: unsafe {
            mem::transmute::<unsafe extern "system" fn() -> isize, SetPreferredAppMode>(set_mode)
        },
        flush_menu_themes: unsafe {
            mem::transmute::<unsafe extern "system" fn() -> isize, FlushMenuThemes>(flush)
        },
    })
}

pub fn windows_main_dark_mode() -> Result<bool, WindowsIntegrationError> {
    Ok(windows_theme_state()?.system_dark_mode)
}

pub fn next_windows_dark_mode() -> Result<bool, WindowsIntegrationError> {
    let theme_state = windows_theme_state()?;

    Ok(
        if theme_state.apps_dark_mode == theme_state.system_dark_mode {
            !theme_state.system_dark_mode
        } else {
            theme_state.system_dark_mode
        },
    )
}

pub fn set_windows_dark_mode(dark_mode: bool) -> Result<(), WindowsIntegrationError> {
    let light_theme_value = if dark_mode { 0u32 } else { 1u32 };
    let mut registry = WindowsThemeRegistry;
    update_theme_registry(&mut registry, light_theme_value)
        .map_err(ThemeUpdateError::into_error)?;
    broadcast_theme_change();
    Ok(())
}

fn windows_theme_state() -> Result<WindowsThemeState, WindowsIntegrationError> {
    Ok(WindowsThemeState {
        apps_dark_mode: theme_registry_dark_mode("AppsUseLightTheme")?,
        system_dark_mode: theme_registry_dark_mode("SystemUsesLightTheme")?,
    })
}

fn theme_registry_dark_mode(name: &'static str) -> Result<bool, WindowsIntegrationError> {
    Ok(theme_value_is_dark(read_theme_registry_value_result(name)?))
}

fn theme_value_is_dark(value: Option<u32>) -> bool {
    value.unwrap_or(1) == 0
}

fn read_theme_registry_value_result(
    name: &'static str,
) -> Result<Option<u32>, WindowsIntegrationError> {
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

    match status {
        ERROR_SUCCESS => Ok(Some(value)),
        ERROR_FILE_NOT_FOUND => Ok(None),
        code => Err(WindowsIntegrationError {
            context: "RegGetValueW failed",
            code,
        }),
    }
}

fn delete_theme_registry_value(name: &'static str) -> Result<(), WindowsIntegrationError> {
    let subkey = wide_null(THEME_REGISTRY_SUBKEY);
    let value_name = wide_null(name);
    let status =
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, subkey.as_ptr(), value_name.as_ptr()) };

    if matches!(status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(WindowsIntegrationError {
            context: "RegDeleteKeyValueW failed",
            code: status,
        })
    }
}

trait ThemeRegistry {
    type Error;

    fn read(&mut self, name: &'static str) -> Result<Option<u32>, Self::Error>;
    fn write(&mut self, name: &'static str, value: u32) -> Result<(), Self::Error>;
    fn delete(&mut self, name: &'static str) -> Result<(), Self::Error>;
}

struct WindowsThemeRegistry;

impl ThemeRegistry for WindowsThemeRegistry {
    type Error = WindowsIntegrationError;

    fn read(&mut self, name: &'static str) -> Result<Option<u32>, Self::Error> {
        read_theme_registry_value_result(name)
    }

    fn write(&mut self, name: &'static str, value: u32) -> Result<(), Self::Error> {
        set_theme_registry_value(name, value)
    }

    fn delete(&mut self, name: &'static str) -> Result<(), Self::Error> {
        delete_theme_registry_value(name)
    }
}

enum ThemeUpdateError<E> {
    Update(E),
    Rollback(E),
}

impl<E> ThemeUpdateError<E> {
    fn into_error(self) -> E {
        match self {
            Self::Update(error) | Self::Rollback(error) => error,
        }
    }
}

fn update_theme_registry<R: ThemeRegistry>(
    registry: &mut R,
    value: u32,
) -> Result<(), ThemeUpdateError<R::Error>> {
    let previous_system = registry
        .read("SystemUsesLightTheme")
        .map_err(ThemeUpdateError::Update)?;
    registry
        .read("AppsUseLightTheme")
        .map_err(ThemeUpdateError::Update)?;
    registry
        .write("SystemUsesLightTheme", value)
        .map_err(ThemeUpdateError::Update)?;

    if let Err(error) = registry.write("AppsUseLightTheme", value) {
        restore_theme_registry_value(registry, "SystemUsesLightTheme", previous_system)
            .map_err(ThemeUpdateError::Rollback)?;
        return Err(ThemeUpdateError::Update(error));
    }

    Ok(())
}

fn restore_theme_registry_value<R: ThemeRegistry>(
    registry: &mut R,
    name: &'static str,
    value: Option<u32>,
) -> Result<(), R::Error> {
    match value {
        Some(value) => registry.write(name, value),
        None => registry.delete(name),
    }
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{ThemeRegistry, theme_value_is_dark, update_theme_registry};

    #[derive(Default)]
    struct FakeRegistry {
        values: HashMap<&'static str, u32>,
        fail_write: Option<&'static str>,
    }

    impl ThemeRegistry for FakeRegistry {
        type Error = &'static str;

        fn read(&mut self, name: &'static str) -> Result<Option<u32>, Self::Error> {
            Ok(self.values.get(name).copied())
        }

        fn write(&mut self, name: &'static str, value: u32) -> Result<(), Self::Error> {
            if self.fail_write == Some(name) {
                self.fail_write = None;
                return Err("write failed");
            }
            self.values.insert(name, value);
            Ok(())
        }

        fn delete(&mut self, name: &'static str) -> Result<(), Self::Error> {
            self.values.remove(name);
            Ok(())
        }
    }

    #[test]
    fn second_theme_write_failure_restores_the_first_value() {
        let mut registry = FakeRegistry {
            values: HashMap::from([("SystemUsesLightTheme", 1), ("AppsUseLightTheme", 1)]),
            fail_write: Some("AppsUseLightTheme"),
        };

        assert!(update_theme_registry(&mut registry, 0).is_err());
        assert_eq!(registry.values["SystemUsesLightTheme"], 1);
        assert_eq!(registry.values["AppsUseLightTheme"], 1);
    }

    #[test]
    fn rollback_removes_a_value_that_was_originally_missing() {
        let mut registry = FakeRegistry {
            values: HashMap::from([("AppsUseLightTheme", 1)]),
            fail_write: Some("AppsUseLightTheme"),
        };

        assert!(update_theme_registry(&mut registry, 0).is_err());
        assert!(!registry.values.contains_key("SystemUsesLightTheme"));
    }

    #[test]
    fn a_missing_theme_value_uses_the_windows_light_default() {
        assert!(!theme_value_is_dark(None));
        assert!(theme_value_is_dark(Some(0)));
        assert!(!theme_value_is_dark(Some(1)));
    }
}
