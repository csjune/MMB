#[derive(Clone, Copy, Debug)]
pub struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub scale_factor: f32,
}

#[cfg(windows)]
mod platform {
    use std::fmt;
    use std::mem;
    use std::ptr;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW,
        MonitorFromPoint,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
    };
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, HWND_BROADCAST, PostMessageW,
        SMTO_ABORTIFHUNG, SMTO_NOTIMEOUTIFNOTHUNG, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, SendMessageTimeoutW, SendNotifyMessageW, SetWindowLongPtrW,
        SetWindowPos, WM_DWMCOLORIZATIONCOLORCHANGED, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE,
        WM_THEMECHANGED, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };
    use windows_sys::core::BOOL;

    use super::WorkArea;

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

    pub fn work_area_near_cursor() -> Option<WorkArea> {
        let mut point = POINT { x: 0, y: 0 };

        unsafe {
            if GetCursorPos(&mut point) == 0 {
                return None;
            }

            let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
            if monitor.is_null() {
                return None;
            }

            let mut info = MONITORINFOEXW {
                monitorInfo: MONITORINFO {
                    cbSize: mem::size_of::<MONITORINFOEXW>() as u32,
                    rcMonitor: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                    rcWork: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                    dwFlags: 0,
                },
                szDevice: [0; 32],
            };

            let ok = GetMonitorInfoW(
                monitor,
                &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
            );
            if ok == 0 {
                return None;
            }

            Some(WorkArea {
                left: info.monitorInfo.rcWork.left,
                top: info.monitorInfo.rcWork.top,
                right: info.monitorInfo.rcWork.right,
                bottom: info.monitorInfo.rcWork.bottom,
                scale_factor: scale_factor_for_monitor(monitor),
            })
        }
    }

    pub fn left_mouse_button_down() -> bool {
        unsafe { (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0 }
    }

    pub fn cursor_is_in_rect(left: i32, top: i32, right: i32, bottom: i32) -> bool {
        let mut point = POINT { x: 0, y: 0 };

        unsafe {
            GetCursorPos(&mut point) != 0
                && point.x >= left
                && point.x < right
                && point.y >= top
                && point.y < bottom
        }
    }

    pub fn hide_window_from_taskbar(window: &slint::Window) {
        let slint_window_handle = window.window_handle();
        let Ok(window_handle) = slint_window_handle.window_handle() else {
            return;
        };

        let RawWindowHandle::Win32(handle) = window_handle.as_raw() else {
            return;
        };

        let hwnd = handle.hwnd.get() as HWND;
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let next_style = (style | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;

        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next_style as isize);
            SetWindowPos(
                hwnd,
                ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

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
        let subkey = wide_null("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
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

        if status == ERROR_SUCCESS {
            Some(value)
        } else {
            None
        }
    }

    fn wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn set_theme_registry_value(
        name: &'static str,
        value: u32,
    ) -> Result<(), WindowsIntegrationError> {
        let subkey = wide_null("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
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

        for setting in [
            "ImmersiveColorSet",
            "WindowsThemeElement",
            "SystemUsesLightTheme",
            "AppsUseLightTheme",
            "Policy",
        ] {
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
        for setting in [
            "ImmersiveColorSet",
            "WindowsThemeElement",
            "SystemUsesLightTheme",
            "AppsUseLightTheme",
            "Policy",
        ] {
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
        for message in [
            WM_THEMECHANGED,
            WM_SYSCOLORCHANGE,
            WM_DWMCOLORIZATIONCOLORCHANGED,
        ] {
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
            unsafe { GetProcAddress(module, b"UpdatePerUserSystemParameters\0".as_ptr()) };
        let Some(procedure) = procedure else {
            return;
        };

        let update: UpdatePerUserSystemParameters = unsafe { mem::transmute(procedure) };
        unsafe {
            update(1, 1);
        }
    }

    fn scale_factor_for_monitor(monitor: HMONITOR) -> f32 {
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;

        let result =
            unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };

        if result >= 0 && dpi_x > 0 {
            dpi_x as f32 / 96.0
        } else {
            1.0
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::fmt;

    use super::WorkArea;

    #[derive(Debug)]
    pub struct WindowsIntegrationError;

    impl fmt::Display for WindowsIntegrationError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "Windows integration is only supported on Windows")
        }
    }

    impl std::error::Error for WindowsIntegrationError {}

    pub fn work_area_near_cursor() -> Option<WorkArea> {
        None
    }

    pub fn left_mouse_button_down() -> bool {
        false
    }

    pub fn cursor_is_in_rect(_left: i32, _top: i32, _right: i32, _bottom: i32) -> bool {
        true
    }

    pub fn hide_window_from_taskbar(_window: &slint::Window) {}

    pub fn windows_main_dark_mode() -> bool {
        false
    }

    pub fn next_windows_dark_mode() -> bool {
        true
    }

    pub fn set_windows_dark_mode(_dark_mode: bool) -> Result<(), WindowsIntegrationError> {
        Err(WindowsIntegrationError)
    }
}

pub use platform::{
    cursor_is_in_rect, hide_window_from_taskbar, left_mouse_button_down, next_windows_dark_mode,
    set_windows_dark_mode, windows_main_dark_mode, work_area_near_cursor,
};
