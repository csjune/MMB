use std::mem;
use std::ptr;

use windows_sys::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, SetMonitorBrightness,
};
use windows_sys::Win32::Foundation::{
    ERROR_GEN_FAILURE, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, GetLastError, LPARAM, RECT,
};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::core::BOOL;

use super::display_config::ActiveDisplayPaths;
use super::{MonitorError, MonitorId, last_win32_error, wide_to_string};

pub(super) struct DdcDiscovery {
    pub(super) monitors: Vec<DdcMonitor>,
    pub(super) warnings: Vec<String>,
}

pub(super) struct DdcMonitor {
    id: MonitorId,
    name: String,
    brightness: i32,
    physical_monitor: PhysicalMonitorHandle,
    min: u32,
    max: u32,
}

impl DdcMonitor {
    pub(super) fn id(&self) -> &MonitorId {
        &self.id
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn brightness(&self) -> i32 {
        self.brightness
    }

    pub(super) fn set_brightness(&mut self, percent: i32) -> Result<(), MonitorError> {
        let percent = percent.clamp(0, 100);
        let raw = percent_to_raw(percent, self.min, self.max);
        let ok = unsafe { SetMonitorBrightness(self.physical_monitor.handle(), raw) };

        if ok == 0 {
            return Err(last_win32_error("SetMonitorBrightness failed"));
        }

        self.brightness = percent;
        Ok(())
    }
}

struct PhysicalMonitorHandle {
    monitor: PHYSICAL_MONITOR,
}

impl PhysicalMonitorHandle {
    fn new(monitor: PHYSICAL_MONITOR) -> Self {
        Self { monitor }
    }

    fn handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.monitor.hPhysicalMonitor
    }

    fn description(&self) -> [u16; 128] {
        unsafe { ptr::addr_of!(self.monitor.szPhysicalMonitorDescription).read_unaligned() }
    }
}

impl Drop for PhysicalMonitorHandle {
    fn drop(&mut self) {
        unsafe {
            DestroyPhysicalMonitors(1, &self.monitor);
        }
    }
}

pub(super) fn discover(paths: &ActiveDisplayPaths) -> Result<DdcDiscovery, MonitorError> {
    let hmonitors = enumerate_display_monitors()?;
    let mut monitors = Vec::new();
    let mut warnings = Vec::new();

    for hmonitor in hmonitors {
        let discovery = discover_physical_monitors(hmonitor, paths);
        monitors.extend(discovery.monitors);
        warnings.extend(discovery.warnings);
    }

    Ok(DdcDiscovery { monitors, warnings })
}

fn enumerate_display_monitors() -> Result<Vec<HMONITOR>, MonitorError> {
    let mut monitors = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(enum_monitor),
            &mut monitors as *mut Vec<HMONITOR> as LPARAM,
        )
    };

    if ok == 0 {
        Err(last_win32_error("EnumDisplayMonitors failed"))
    } else {
        Ok(monitors)
    }
}

fn discover_physical_monitors(hmonitor: HMONITOR, paths: &ActiveDisplayPaths) -> DdcDiscovery {
    let display_name = display_name(hmonitor).unwrap_or_else(|| "UNKNOWN-DISPLAY".into());
    let pnp_id = paths.pnp_id_for_gdi_name(&display_name);
    let mut count = 0;
    let ok = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) };
    if ok == 0 {
        let error = last_win32_error("GetNumberOfPhysicalMonitorsFromHMONITOR failed");
        return DdcDiscovery {
            monitors: Vec::new(),
            warnings: vec![format!("failed to inspect {display_name}: {error}")],
        };
    }
    if count == 0 {
        return DdcDiscovery {
            monitors: Vec::new(),
            warnings: Vec::new(),
        };
    }

    let mut physical_monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
    let ok =
        unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, count, physical_monitors.as_mut_ptr()) };
    if ok == 0 {
        let error = last_win32_error("GetPhysicalMonitorsFromHMONITOR failed");
        return DdcDiscovery {
            monitors: Vec::new(),
            warnings: vec![format!("failed to inspect {display_name}: {error}")],
        };
    }

    let mut monitors = Vec::new();
    let mut warnings = Vec::new();
    for (index, physical_monitor) in physical_monitors.into_iter().enumerate() {
        match build_monitor(&display_name, pnp_id, index, physical_monitor) {
            Ok(Some(monitor)) => monitors.push(monitor),
            Ok(None) => {}
            Err(error) => warnings.push(format!("failed to inspect {display_name}: {error}")),
        }
    }

    DdcDiscovery { monitors, warnings }
}

fn build_monitor(
    display_name: &str,
    pnp_id: Option<&str>,
    physical_index: usize,
    physical_monitor: PHYSICAL_MONITOR,
) -> Result<Option<DdcMonitor>, MonitorError> {
    let physical_monitor = PhysicalMonitorHandle::new(physical_monitor);
    let mut min = 0;
    let mut current = 0;
    let mut max = 0;
    let ok = unsafe {
        GetMonitorBrightness(physical_monitor.handle(), &mut min, &mut current, &mut max)
    };

    if ok == 0 {
        let code = unsafe { GetLastError() };
        if matches!(
            code,
            ERROR_INVALID_FUNCTION | ERROR_GEN_FAILURE | ERROR_NOT_SUPPORTED
        ) {
            return Ok(None);
        }
        return Err(MonitorError::Win32 {
            context: "GetMonitorBrightness failed",
            code,
        });
    }
    if max <= min {
        return Err(MonitorError::InvalidData {
            context: "invalid DDC brightness range",
            details: format!("minimum {min}, maximum {max}"),
        });
    }

    let description = physical_monitor.description();
    let name = wide_to_string(&description).unwrap_or_else(|| display_name.to_string());
    let id = MonitorId::new(match pnp_id {
        Some(pnp_id) => format!("ddc:{pnp_id}:{physical_index}"),
        None => format!(
            "ddc-gdi:{}:{physical_index}:{}",
            display_name.to_ascii_uppercase(),
            name.to_ascii_uppercase()
        ),
    });

    Ok(Some(DdcMonitor {
        id,
        name,
        brightness: raw_to_percent(current, min, max),
        physical_monitor,
        min,
        max,
    }))
}

unsafe extern "system" fn enum_monitor(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let monitors = unsafe { &mut *(data as *mut Vec<HMONITOR>) };
    monitors.push(hmonitor);
    1
}

fn display_name(hmonitor: HMONITOR) -> Option<String> {
    unsafe {
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: mem::size_of::<MONITORINFOEXW>() as u32,
                rcMonitor: RECT::default(),
                rcWork: RECT::default(),
                dwFlags: 0,
            },
            szDevice: [0; 32],
        };

        let ok = GetMonitorInfoW(
            hmonitor,
            &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
        );
        if ok == 0 {
            return None;
        }

        wide_to_string(&info.szDevice)
    }
}

fn raw_to_percent(value: u32, min: u32, max: u32) -> i32 {
    (((value.saturating_sub(min)) as f64 / (max - min) as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as i32
}

fn percent_to_raw(percent: i32, min: u32, max: u32) -> u32 {
    let range = max - min;
    min + ((percent.clamp(0, 100) as u32 * range + 50) / 100)
}

#[cfg(test)]
mod tests {
    use super::{percent_to_raw, raw_to_percent};

    #[test]
    fn brightness_conversion_respects_monitor_ranges() {
        assert_eq!(raw_to_percent(10, 10, 90), 0);
        assert_eq!(raw_to_percent(50, 10, 90), 50);
        assert_eq!(raw_to_percent(90, 10, 90), 100);
        assert_eq!(percent_to_raw(0, 10, 90), 10);
        assert_eq!(percent_to_raw(50, 10, 90), 50);
        assert_eq!(percent_to_raw(100, 10, 90), 90);
    }
}
