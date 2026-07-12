mod ddc;
mod display_config;
mod wmi;

use std::fmt;

use windows_sys::Win32::Foundation::GetLastError;

use self::ddc::DdcMonitor;
use self::wmi::WmiMonitor;
use super::{MonitorId, MonitorSnapshot};

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
            Self::UnknownMonitor(id) => {
                write!(formatter, "unknown monitor id {}", id.index())
            }
        }
    }
}

impl std::error::Error for MonitorError {}

enum Monitor {
    Ddc(Box<DdcMonitor>),
    Wmi(WmiMonitor),
}

impl Monitor {
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
}

impl MonitorController {
    pub fn new() -> Self {
        Self {
            monitors: Vec::new(),
        }
    }

    pub fn refresh(&mut self) -> Result<(), MonitorError> {
        let mut monitors = ddc::discover()?
            .into_iter()
            .map(|monitor| Monitor::Ddc(Box::new(monitor)))
            .collect::<Vec<_>>();

        match wmi::discover() {
            Ok(wmi_monitors) => monitors.extend(wmi_monitors.into_iter().map(Monitor::Wmi)),
            Err(error) => eprintln!("failed to refresh WMI monitors: {error}"),
        }

        self.monitors = monitors;
        Ok(())
    }

    pub fn snapshots(&self) -> Vec<MonitorSnapshot> {
        self.monitors
            .iter()
            .enumerate()
            .map(|(index, monitor)| MonitorSnapshot {
                id: MonitorId::from_index(index),
                name: monitor.name().to_string(),
                brightness: monitor.brightness(),
            })
            .collect()
    }

    pub fn set_brightness(&mut self, id: MonitorId, percent: i32) -> Result<(), MonitorError> {
        self.monitors
            .get_mut(id.index())
            .ok_or(MonitorError::UnknownMonitor(id))?
            .set_brightness(percent.clamp(0, 100))
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
