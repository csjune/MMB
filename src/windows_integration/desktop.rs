use std::mem;

use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW,
    MonitorFromPoint,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

use super::WorkArea;

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
                rcMonitor: RECT::default(),
                rcWork: RECT::default(),
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

fn scale_factor_for_monitor(monitor: HMONITOR) -> f32 {
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    let result = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };

    if result >= 0 && dpi_x > 0 {
        dpi_x as f32 / 96.0
    } else {
        1.0
    }
}
