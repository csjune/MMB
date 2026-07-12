mod ddc;
mod display_config;
mod wmi;

use std::fmt;

use windows_sys::Win32::Foundation::GetLastError;

use self::ddc::DdcMonitor;
use self::wmi::WmiMonitor;
use super::{
    ApplyOutcome, ApplyReport, BrightnessUpdate, MonitorId, MonitorSnapshot, RefreshResult,
};

#[derive(Debug)]
pub enum MonitorError {
    Win32 {
        context: &'static str,
        code: u32,
    },
    Wmi {
        context: &'static str,
        details: String,
    },
    InvalidData {
        context: &'static str,
        details: String,
    },
    StaleGeneration {
        requested: u64,
        current: u64,
    },
    UnknownMonitor(MonitorId),
}

impl MonitorError {
    pub(super) fn wmi(context: &'static str, error: impl fmt::Display) -> Self {
        Self::Wmi {
            context,
            details: error.to_string(),
        }
    }
}

impl fmt::Display for MonitorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Win32 { context, code } => {
                write!(formatter, "{context} (win32 error {code})")
            }
            Self::Wmi { context, details } => write!(formatter, "{context}: {details}"),
            Self::InvalidData { context, details } => write!(formatter, "{context}: {details}"),
            Self::StaleGeneration { requested, current } => write!(
                formatter,
                "stale monitor generation {requested}; current generation is {current}"
            ),
            Self::UnknownMonitor(id) => write!(formatter, "unknown monitor id {id}"),
        }
    }
}

impl std::error::Error for MonitorError {}

enum Monitor {
    Ddc(Box<DdcMonitor>),
    Wmi(WmiMonitor),
}

impl Monitor {
    fn id(&self) -> &MonitorId {
        match self {
            Self::Ddc(monitor) => monitor.id(),
            Self::Wmi(monitor) => monitor.id(),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Ddc(monitor) => monitor.name(),
            Self::Wmi(monitor) => monitor.name(),
        }
    }

    fn brightness(&self) -> i32 {
        match self {
            Self::Ddc(monitor) => monitor.brightness(),
            Self::Wmi(monitor) => monitor.brightness(),
        }
    }

    fn set_brightness(&mut self, percent: i32) -> Result<(), MonitorError> {
        match self {
            Self::Ddc(monitor) => monitor.set_brightness(percent),
            Self::Wmi(monitor) => monitor.set_brightness(percent),
        }
    }
}

pub struct MonitorController {
    monitors: Vec<Monitor>,
    generation: u64,
}

impl MonitorController {
    pub fn new() -> Self {
        Self {
            monitors: Vec::new(),
            generation: 0,
        }
    }

    pub fn refresh(&mut self) -> Result<RefreshResult, MonitorError> {
        let mut warnings = Vec::new();
        let (active_paths, active_paths_complete) = match display_config::active_display_paths() {
            Ok(paths) => (paths, true),
            Err(error) => {
                warnings.push(format!("failed to query active display paths: {error}"));
                (display_config::ActiveDisplayPaths::default(), false)
            }
        };
        warnings.extend(active_paths.warnings.iter().cloned());

        let ddc_discovery = ddc::discover(&active_paths)?;
        warnings.extend(ddc_discovery.warnings);
        let mut monitors = ddc_discovery
            .monitors
            .into_iter()
            .map(|monitor| Monitor::Ddc(Box::new(monitor)))
            .collect::<Vec<_>>();

        if active_paths_complete {
            match wmi::discover(&active_paths) {
                Ok(discovery) => {
                    warnings.extend(discovery.warnings);
                    monitors.extend(discovery.monitors.into_iter().map(Monitor::Wmi));
                }
                Err(error) => warnings.push(format!("failed to refresh WMI monitors: {error}")),
            }
        }

        self.monitors = monitors;
        self.generation = self.generation.wrapping_add(1).max(1);
        let snapshots = self
            .monitors
            .iter()
            .map(|monitor| MonitorSnapshot {
                id: monitor.id().clone(),
                name: monitor.name().to_string(),
                brightness: monitor.brightness(),
            })
            .collect();

        Ok(RefreshResult {
            generation: self.generation,
            snapshots,
            warnings,
        })
    }

    pub fn apply(&mut self, updates: Vec<BrightnessUpdate>) -> ApplyReport {
        let outcomes = updates
            .into_iter()
            .map(|update| {
                let requested = update.value.clamp(0, 100);
                if update.generation != self.generation {
                    return ApplyOutcome {
                        generation: update.generation,
                        id: update.id,
                        requested,
                        effective: None,
                        error: Some(
                            MonitorError::StaleGeneration {
                                requested: update.generation,
                                current: self.generation,
                            }
                            .to_string(),
                        ),
                    };
                }
                let Some(monitor) = self
                    .monitors
                    .iter_mut()
                    .find(|monitor| monitor.id() == &update.id)
                else {
                    let error = MonitorError::UnknownMonitor(update.id.clone()).to_string();
                    return ApplyOutcome {
                        generation: update.generation,
                        id: update.id,
                        requested,
                        effective: None,
                        error: Some(error),
                    };
                };

                let previous = monitor.brightness();
                let error = monitor
                    .set_brightness(requested)
                    .err()
                    .map(|error| error.to_string());

                ApplyOutcome {
                    generation: update.generation,
                    id: update.id,
                    requested,
                    effective: Some(if error.is_some() { previous } else { requested }),
                    error,
                }
            })
            .collect();

        ApplyReport { outcomes }
    }
}

pub(super) fn last_win32_error(context: &'static str) -> MonitorError {
    MonitorError::Win32 {
        context,
        code: unsafe { GetLastError() },
    }
}

pub(super) fn win32_status(context: &'static str, code: u32) -> MonitorError {
    MonitorError::Win32 { context, code }
}

pub(super) fn wide_to_string(buffer: &[u16]) -> Option<String> {
    let end = buffer.iter().position(|&character| character == 0)?;
    if end == 0 {
        return None;
    }

    Some(String::from_utf16_lossy(&buffer[..end]))
}
