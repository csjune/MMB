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
#[path = "windows_integration/mouse_hook.rs"]
mod mouse_hook;
#[cfg(windows)]
#[path = "windows_integration/single_instance.rs"]
mod single_instance;
#[cfg(windows)]
#[path = "windows_integration/theme.rs"]
mod theme;

#[cfg(windows)]
pub use desktop::{show_error_message, work_area_near_cursor};
#[cfg(windows)]
pub use mouse_hook::{GlobalMouseEvent, GlobalMouseWatcher};
#[cfg(windows)]
pub use single_instance::acquire_single_instance;
#[cfg(windows)]
pub use theme::{next_windows_dark_mode, set_windows_dark_mode, windows_main_dark_mode};

#[cfg(not(windows))]
mod fallback {
    use std::fmt;
    use std::sync::mpsc::TryRecvError;

    use super::WorkArea;

    #[derive(Debug)]
    pub struct WindowsIntegrationError;

    #[derive(Clone, Copy)]
    pub enum GlobalMouseEvent {
        ButtonDown { click_id: u64, x: i32, y: i32 },
    }

    pub struct GlobalMouseWatcher;

    pub struct SingleInstanceGuard;

    impl GlobalMouseWatcher {
        pub fn new() -> Result<Self, WindowsIntegrationError> {
            Err(WindowsIntegrationError)
        }

        pub fn polling() -> Self {
            Self
        }

        pub fn try_recv(&self) -> Result<GlobalMouseEvent, TryRecvError> {
            Err(TryRecvError::Empty)
        }

        pub fn drain(&self) {}

        pub fn latest_click_id(&self) -> u64 {
            0
        }
    }

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

    pub fn acquire_single_instance() -> std::io::Result<Option<SingleInstanceGuard>> {
        Ok(Some(SingleInstanceGuard))
    }

    pub fn windows_main_dark_mode() -> Result<bool, WindowsIntegrationError> {
        Err(WindowsIntegrationError)
    }

    pub fn next_windows_dark_mode() -> Result<bool, WindowsIntegrationError> {
        Err(WindowsIntegrationError)
    }

    pub fn set_windows_dark_mode(_dark_mode: bool) -> Result<(), WindowsIntegrationError> {
        Err(WindowsIntegrationError)
    }

    pub fn show_error_message(title: &str, message: &str) {
        eprintln!("{title}: {message}");
    }
}

#[cfg(not(windows))]
pub use fallback::{
    GlobalMouseEvent, GlobalMouseWatcher, acquire_single_instance, next_windows_dark_mode,
    set_windows_dark_mode, show_error_message, windows_main_dark_mode, work_area_near_cursor,
};
