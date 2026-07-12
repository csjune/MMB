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
#[path = "windows_integration/theme.rs"]
mod theme;

#[cfg(windows)]
pub use desktop::work_area_near_cursor;
#[cfg(windows)]
pub use mouse_hook::{GlobalMouseEvent, GlobalMouseWatcher};
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
        LeftDown { x: i32, y: i32 },
        LeftUp { x: i32, y: i32 },
    }

    pub struct GlobalMouseWatcher;

    impl GlobalMouseWatcher {
        pub fn new() -> Self {
            Self
        }

        pub fn try_recv(&self) -> Result<GlobalMouseEvent, TryRecvError> {
            Err(TryRecvError::Empty)
        }

        pub fn drain(&self) {}
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
    GlobalMouseEvent, GlobalMouseWatcher, next_windows_dark_mode, set_windows_dark_mode,
    windows_main_dark_mode, work_area_near_cursor,
};
