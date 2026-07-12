use std::mem;
use std::ptr;

use windows_sys::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, SetMonitorBrightness,
};
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::core::BOOL;

use super::{MonitorError, last_win32_error, wide_to_string};

pub(super) struct DdcMonitor {
    name: String,
    brightness: i32,
    physical_monitor: PhysicalMonitorHandle,
    min: u32,
    max: u32,
}

impl DdcMonitor {
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

pub(super) fn discover() -> Result<Vec<DdcMonitor>, MonitorError> {
    let hmonitors = enumerate_display_monitors()?;
    let mut monitors = Vec::new();

    for hmonitor in hmonitors {
        monitors.extend(discover_physical_monitors(hmonitor));
    }

    Ok(monitors)
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

fn discover_physical_monitors(hmonitor: HMONITOR) -> Vec<DdcMonitor> {
    let mut count = 0;
    let ok = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) };
    if ok == 0 || count == 0 {
        return Vec::new();
    }

    let mut physical_monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
    let ok =
        unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, count, physical_monitors.as_mut_ptr()) };
    if ok == 0 {
        return Vec::new();
    }

    physical_monitors
        .into_iter()
        .filter_map(|physical_monitor| build_monitor(hmonitor, physical_monitor))
        .collect()
}

fn build_monitor(hmonitor: HMONITOR, physical_monitor: PHYSICAL_MONITOR) -> Option<DdcMonitor> {
    let physical_monitor = PhysicalMonitorHandle::new(physical_monitor);
    let mut min = 0;
    let mut current = 0;
    let mut max = 0;
    let ok = unsafe {
        GetMonitorBrightness(physical_monitor.handle(), &mut min, &mut current, &mut max)
    };

    if ok == 0 || max <= min {
        return None;
    }

    let description = physical_monitor.description();
    let name = wide_to_string(&description)
        .or_else(|| display_name(hmonitor))
        .unwrap_or_else(|| "Monitor".into());

    Some(DdcMonitor {
        name,
        brightness: raw_to_percent(current, min, max),
        physical_monitor,
        min,
        max,
    })
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
