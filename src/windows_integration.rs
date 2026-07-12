#[derive(Clone, Copy, Debug)]
pub struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub scale_factor: f32,
}

#[cfg(windows)]
#[path = "windows_integration/desktop.rs"]
mod desktop;
#[cfg(windows)]
#[path = "windows_integration/theme.rs"]
mod theme;

#[cfg(windows)]
pub use desktop::{cursor_is_in_rect, left_mouse_button_down, work_area_near_cursor};
#[cfg(windows)]
pub use theme::{next_windows_dark_mode, set_windows_dark_mode, windows_main_dark_mode};

#[cfg(not(windows))]
mod fallback {
    use std::fmt;

    use super::WorkArea;

    #[derive(Debug)]
    pub struct WindowsIntegrationError;

    impl fmt::Display for WindowsIntegrationError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "Windows integration is only supported on Windows"
            )
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

#[cfg(not(windows))]
pub use fallback::{
    cursor_is_in_rect, left_mouse_button_down, next_windows_dark_mode, set_windows_dark_mode,
    windows_main_dark_mode, work_area_near_cursor,
};
