#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MonitorId(usize);

impl MonitorId {
    pub(crate) fn from_index(index: usize) -> Self {
        Self(index)
    }

    pub(crate) fn from_ui(value: i32) -> Option<Self> {
        usize::try_from(value).ok().map(Self)
    }

    pub(crate) fn index(self) -> usize {
        self.0
    }

    pub(crate) fn to_ui(self) -> i32 {
        i32::try_from(self.0).expect("monitor index should fit in a Slint int")
    }
}

impl std::fmt::Display for MonitorId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug)]
pub struct MonitorSnapshot {
    pub id: MonitorId,
    pub name: String,
    pub brightness: i32,
}

#[cfg(windows)]
#[path = "monitor_hardware/windows.rs"]
mod platform;

#[cfg(not(windows))]
mod platform {
    use std::fmt;

    use super::{MonitorId, MonitorSnapshot};

    #[derive(Debug)]
    pub struct MonitorError;

    impl fmt::Display for MonitorError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "monitor brightness is only supported on Windows")
        }
    }

    impl std::error::Error for MonitorError {}

    pub struct MonitorController;

    impl MonitorController {
        pub fn new() -> Self {
            Self
        }

        pub fn refresh(&mut self) -> Result<(), MonitorError> {
            Ok(())
        }

        pub fn snapshots(&self) -> Vec<MonitorSnapshot> {
            Vec::new()
        }

        pub fn set_brightness(
            &mut self,
            _id: MonitorId,
            _percent: i32,
        ) -> Result<(), MonitorError> {
            Err(MonitorError)
        }
    }
}

pub use platform::MonitorController;
