#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MonitorId(String);

impl MonitorId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn from_ui(value: &str) -> Self {
        Self(value.to_string())
    }

    pub(crate) fn to_ui(&self) -> &str {
        &self.0
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

#[derive(Debug)]
pub struct RefreshResult {
    pub snapshots: Vec<MonitorSnapshot>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct BrightnessUpdate {
    pub id: MonitorId,
    pub value: i32,
}

#[derive(Debug)]
pub struct ApplyOutcome {
    pub id: MonitorId,
    pub requested: i32,
    pub effective: Option<i32>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct ApplyReport {
    pub outcomes: Vec<ApplyOutcome>,
}

#[cfg(windows)]
#[path = "monitor_hardware/windows.rs"]
mod platform;

#[cfg(not(windows))]
mod platform {
    use std::fmt;

    use super::{ApplyOutcome, ApplyReport, BrightnessUpdate, RefreshResult};

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

        pub fn refresh(&mut self) -> Result<RefreshResult, MonitorError> {
            Ok(RefreshResult {
                snapshots: Vec::new(),
                warnings: Vec::new(),
            })
        }

        pub fn apply(&mut self, updates: Vec<BrightnessUpdate>) -> ApplyReport {
            ApplyReport {
                outcomes: updates
                    .into_iter()
                    .map(|update| ApplyOutcome {
                        id: update.id,
                        requested: update.value,
                        effective: None,
                        error: Some(MonitorError.to_string()),
                    })
                    .collect(),
            }
        }
    }
}

pub use platform::MonitorController;
