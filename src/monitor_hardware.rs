#[derive(Clone, Debug)]
pub struct MonitorSnapshot {
    pub id: usize,
    pub name: String,
    pub brightness: i32,
}

#[cfg(windows)]
mod platform {
    use std::collections::{HashMap, HashSet};
    use std::fmt;
    use std::mem;
    use std::ptr;

    use serde::{Deserialize, Serialize};
    use windows_sys::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TARGET_DEVICE_NAME,
        DestroyPhysicalMonitors, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
        GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, QDC_ONLY_ACTIVE_PATHS,
        QueryDisplayConfig, SetMonitorBrightness,
    };
    use windows_sys::Win32::Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, GetLastError, LPARAM, RECT,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };
    use windows_sys::core::BOOL;
    use wmi::WMIConnection;

    use super::MonitorSnapshot;

    #[derive(Debug)]
    pub struct MonitorError {
        context: &'static str,
        code: u32,
    }

    impl fmt::Display for MonitorError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "{} (win32 error {})", self.context, self.code)
        }
    }

    impl std::error::Error for MonitorError {}

    struct Monitor {
        name: String,
        brightness: i32,
        backend: MonitorBackend,
    }

    enum MonitorBackend {
        Ddc {
            handle: windows_sys::Win32::Foundation::HANDLE,
            min: u32,
            max: u32,
        },
        Wmi {
            instance_path: String,
        },
    }

    impl Drop for MonitorBackend {
        fn drop(&mut self) {
            if let Self::Ddc { handle, .. } = self {
                let mut physical_monitor = PHYSICAL_MONITOR {
                    hPhysicalMonitor: *handle,
                    szPhysicalMonitorDescription: [0; 128],
                };

                unsafe {
                    DestroyPhysicalMonitors(1, &mut physical_monitor);
                }
            }
        }
    }

    enum BrightnessTarget {
        Ddc {
            handle: windows_sys::Win32::Foundation::HANDLE,
            min: u32,
            max: u32,
        },
        Wmi {
            instance_path: String,
        },
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
            self.monitors.clear();

            let mut hmonitors = Vec::new();
            let ok = unsafe {
                EnumDisplayMonitors(
                    ptr::null_mut(),
                    ptr::null(),
                    Some(enum_monitor),
                    &mut hmonitors as *mut Vec<HMONITOR> as LPARAM,
                )
            };

            if ok == 0 {
                return Err(last_error("EnumDisplayMonitors failed"));
            }

            for hmonitor in hmonitors {
                self.add_physical_monitors(hmonitor);
            }

            if let Err(error) = self.add_wmi_monitors() {
                eprintln!("failed to refresh WMI monitors: {error}");
            }

            Ok(())
        }

        pub fn snapshots(&self) -> Vec<MonitorSnapshot> {
            self.monitors
                .iter()
                .enumerate()
                .map(|(id, monitor)| MonitorSnapshot {
                    id,
                    name: monitor.name.clone(),
                    brightness: monitor.brightness,
                })
                .collect()
        }

        pub fn set_brightness(&mut self, id: usize, percent: i32) -> Result<(), MonitorError> {
            let percent = percent.clamp(0, 100);
            let target = self
                .monitors
                .get(id)
                .ok_or(MonitorError {
                    context: "unknown monitor id",
                    code: 0,
                })?
                .brightness_target();

            match target {
                BrightnessTarget::Ddc { handle, min, max } => {
                    let raw = percent_to_raw(percent, min, max);

                    let ok = unsafe { SetMonitorBrightness(handle, raw) };
                    if ok == 0 {
                        return Err(last_error("SetMonitorBrightness failed"));
                    }
                }
                BrightnessTarget::Wmi { instance_path } => {
                    set_wmi_brightness(&instance_path, percent)?;
                }
            }

            if let Some(monitor) = self.monitors.get_mut(id) {
                monitor.brightness = percent;
            }

            Ok(())
        }

        fn add_physical_monitors(&mut self, hmonitor: HMONITOR) {
            let mut count = 0;
            let ok = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) };
            if ok == 0 || count == 0 {
                return;
            }

            let mut physical_monitors =
                vec![PHYSICAL_MONITOR::default(); count.try_into().unwrap_or(0)];
            let ok = unsafe {
                GetPhysicalMonitorsFromHMONITOR(hmonitor, count, physical_monitors.as_mut_ptr())
            };
            if ok == 0 {
                return;
            }

            for physical_monitor in physical_monitors {
                let mut min = 0;
                let mut current = 0;
                let mut max = 0;
                let ok = unsafe {
                    GetMonitorBrightness(
                        physical_monitor.hPhysicalMonitor,
                        &mut min,
                        &mut current,
                        &mut max,
                    )
                };

                if ok == 0 || max <= min {
                    let mut monitor_to_destroy = physical_monitor;
                    unsafe {
                        DestroyPhysicalMonitors(1, &mut monitor_to_destroy);
                    }
                    continue;
                }

                let description = physical_monitor.szPhysicalMonitorDescription;
                let name = wide_to_string(&description)
                    .unwrap_or_else(|| display_name(hmonitor).unwrap_or_else(|| "Monitor".into()));

                self.monitors.push(Monitor {
                    name,
                    brightness: raw_to_percent(current, min, max),
                    backend: MonitorBackend::Ddc {
                        handle: physical_monitor.hPhysicalMonitor,
                        min,
                        max,
                    },
                });
            }
        }

        fn add_wmi_monitors(&mut self) -> Result<(), MonitorError> {
            let connection = wmi_connection("ROOT\\WMI")?;
            let brightness_monitors: Vec<WmiMonitorBrightness> = connection
                .raw_query(
                    "SELECT InstanceName, Active, CurrentBrightness FROM WmiMonitorBrightness WHERE Active = TRUE",
                )
                .map_err(|_| MonitorError {
                    context: "WMI brightness query failed",
                    code: 0,
                })?;
            let brightness_methods: Vec<WmiMonitorBrightnessMethodsInstance> = connection
                .raw_query(
                    "SELECT InstanceName, Active, __PATH FROM WmiMonitorBrightnessMethods WHERE Active = TRUE",
                )
                .map_err(|_| MonitorError {
                    context: "WMI brightness methods query failed",
                    code: 0,
                })?;
            let method_paths: HashMap<String, String> = brightness_methods
                .into_iter()
                .filter(|monitor| monitor.Active)
                .map(|monitor| (monitor.InstanceName, monitor.path))
                .collect();
            let active_pnp_ids = active_display_pnp_ids();
            let friendly_names = wmi_monitor_names();
            let pnp_names = pnp_monitor_names();

            for monitor in brightness_monitors
                .into_iter()
                .filter(|monitor| monitor.Active)
            {
                let Some(instance_path) = method_paths.get(&monitor.InstanceName).cloned() else {
                    continue;
                };
                let pnp_id = wmi_instance_to_pnp_id(&monitor.InstanceName);
                if !active_pnp_ids.is_empty()
                    && pnp_id
                        .as_ref()
                        .is_some_and(|pnp_id| !active_pnp_ids.contains(pnp_id))
                {
                    continue;
                }

                let name = friendly_names
                    .get(&monitor.InstanceName)
                    .cloned()
                    .or_else(|| pnp_id.as_ref().and_then(|id| pnp_names.get(id).cloned()))
                    .unwrap_or_else(|| "Integrated Monitor".into());

                self.monitors.push(Monitor {
                    name,
                    brightness: monitor.CurrentBrightness as i32,
                    backend: MonitorBackend::Wmi { instance_path },
                });
            }

            Ok(())
        }
    }

    impl Monitor {
        fn brightness_target(&self) -> BrightnessTarget {
            match &self.backend {
                MonitorBackend::Ddc { handle, min, max } => BrightnessTarget::Ddc {
                    handle: *handle,
                    min: *min,
                    max: *max,
                },
                MonitorBackend::Wmi { instance_path } => BrightnessTarget::Wmi {
                    instance_path: instance_path.clone(),
                },
            }
        }
    }

    #[derive(Deserialize)]
    #[allow(non_snake_case)]
    struct WmiMonitorBrightness {
        InstanceName: String,
        Active: bool,
        CurrentBrightness: u8,
    }

    #[derive(Deserialize)]
    #[allow(non_snake_case)]
    struct WmiMonitorBrightnessMethodsInstance {
        InstanceName: String,
        Active: bool,
        #[serde(rename = "__PATH")]
        path: String,
    }

    #[derive(Deserialize)]
    #[allow(non_snake_case)]
    struct WmiMonitorId {
        InstanceName: String,
        Active: bool,
        UserFriendlyName: Option<Vec<u16>>,
    }

    #[derive(Deserialize)]
    #[allow(non_snake_case)]
    struct Win32PnpEntity {
        Name: Option<String>,
        PNPDeviceID: Option<String>,
    }

    #[derive(Deserialize)]
    #[allow(non_camel_case_types)]
    struct WmiMonitorBrightnessMethods;

    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct WmiSetBrightnessInput {
        Timeout: u32,
        Brightness: u8,
    }

    #[derive(Deserialize)]
    #[allow(non_snake_case)]
    struct WmiSetBrightnessOutput {
        ReturnValue: u32,
    }

    fn set_wmi_brightness(instance_path: &str, percent: i32) -> Result<(), MonitorError> {
        let connection = wmi_connection("ROOT\\WMI")?;
        let input = WmiSetBrightnessInput {
            Timeout: 0,
            Brightness: percent.clamp(0, 100) as u8,
        };
        let output: WmiSetBrightnessOutput = connection
            .exec_instance_method::<WmiMonitorBrightnessMethods, _>(
                instance_path,
                "WmiSetBrightness",
                input,
            )
            .map_err(|_| MonitorError {
                context: "WmiSetBrightness failed",
                code: 0,
            })?;

        if output.ReturnValue == 0 {
            Ok(())
        } else {
            Err(MonitorError {
                context: "WmiSetBrightness failed",
                code: output.ReturnValue,
            })
        }
    }

    fn wmi_monitor_names() -> HashMap<String, String> {
        let Ok(connection) = wmi_connection("ROOT\\WMI") else {
            return HashMap::new();
        };
        let Ok(monitors) = connection.raw_query::<WmiMonitorId>(
            "SELECT InstanceName, Active, UserFriendlyName FROM WmiMonitorID WHERE Active = TRUE",
        ) else {
            return HashMap::new();
        };

        monitors
            .into_iter()
            .filter(|monitor| monitor.Active)
            .filter_map(|monitor| {
                friendly_name_from_wmi(&monitor.UserFriendlyName)
                    .map(|name| (monitor.InstanceName, name))
            })
            .collect()
    }

    fn pnp_monitor_names() -> HashMap<String, String> {
        let Ok(connection) = wmi_connection("ROOT\\CIMV2") else {
            return HashMap::new();
        };
        let Ok(monitors) = connection.raw_query::<Win32PnpEntity>(
            "SELECT Name, PNPDeviceID FROM Win32_PnPEntity WHERE PNPClass = 'Monitor'",
        ) else {
            return HashMap::new();
        };

        monitors
            .into_iter()
            .filter_map(|monitor| {
                Some((
                    normalize_pnp_id(&monitor.PNPDeviceID?),
                    monitor.Name?.trim().to_string(),
                ))
            })
            .filter(|(_, name)| !name.is_empty())
            .collect()
    }

    fn active_display_pnp_ids() -> HashSet<String> {
        let flags = QDC_ONLY_ACTIVE_PATHS;
        let mut path_count = 0;
        let mut mode_count = 0;

        for _ in 0..3 {
            let status =
                unsafe { GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) };
            if status != ERROR_SUCCESS {
                return HashSet::new();
            }

            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let status = unsafe {
                QueryDisplayConfig(
                    flags,
                    &mut path_count,
                    paths.as_mut_ptr(),
                    &mut mode_count,
                    modes.as_mut_ptr(),
                    ptr::null_mut(),
                )
            };

            if status == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }

            if status != ERROR_SUCCESS {
                return HashSet::new();
            }

            paths.truncate(path_count as usize);
            return paths
                .into_iter()
                .filter_map(active_display_pnp_id)
                .collect();
        }

        HashSet::new()
    }

    fn active_display_pnp_id(path: DISPLAYCONFIG_PATH_INFO) -> Option<String> {
        let mut target_name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size: mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            },
            ..Default::default()
        };

        let status = unsafe {
            DisplayConfigGetDeviceInfo(
                &mut target_name.header as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
            )
        };
        if status != ERROR_SUCCESS as i32 {
            return None;
        }

        wide_to_string(&target_name.monitorDevicePath)
            .and_then(|path| pnp_id_from_monitor_device_path(&path))
    }

    fn pnp_id_from_monitor_device_path(path: &str) -> Option<String> {
        let uppercase_path = path.to_ascii_uppercase();
        let marker = "DISPLAY#";
        let start = uppercase_path.find(marker)? + marker.len();
        let rest = &path[start..];
        let mut parts = rest.split('#');
        let hardware_id = parts.next()?.trim();
        let instance_id = parts.next()?.trim();

        if hardware_id.is_empty() || instance_id.is_empty() {
            return None;
        }

        Some(normalize_pnp_id(&format!(
            "DISPLAY\\{hardware_id}\\{instance_id}"
        )))
    }

    fn wmi_connection(namespace: &str) -> Result<WMIConnection, MonitorError> {
        WMIConnection::with_namespace_path(namespace).map_err(|_| MonitorError {
            context: "WMI connection failed",
            code: 0,
        })
    }

    fn friendly_name_from_wmi(name: &Option<Vec<u16>>) -> Option<String> {
        let name = name.as_ref()?;
        let end = name
            .iter()
            .position(|&character| character == 0)
            .unwrap_or(name.len());
        let value = String::from_utf16_lossy(&name[..end]).trim().to_string();

        if value.is_empty() { None } else { Some(value) }
    }

    fn wmi_instance_to_pnp_id(instance_name: &str) -> Option<String> {
        let base = instance_name
            .rsplit_once('_')
            .filter(|(_, suffix)| suffix.chars().all(|character| character.is_ascii_digit()))
            .map_or(instance_name, |(base, _)| base);

        if base.is_empty() {
            None
        } else {
            Some(normalize_pnp_id(base))
        }
    }

    fn normalize_pnp_id(value: &str) -> String {
        value
            .replace('#', "\\")
            .replace('/', "\\")
            .to_ascii_uppercase()
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
                    rcMonitor: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                    rcWork: RECT {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
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

    fn wide_to_string(buffer: &[u16]) -> Option<String> {
        let end = buffer.iter().position(|&character| character == 0)?;
        if end == 0 {
            return None;
        }

        Some(String::from_utf16_lossy(&buffer[..end]))
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

    fn last_error(context: &'static str) -> MonitorError {
        MonitorError {
            context,
            code: unsafe { GetLastError() },
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::fmt;

    use super::MonitorSnapshot;

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

        pub fn set_brightness(&mut self, _id: usize, _percent: i32) -> Result<(), MonitorError> {
            Err(MonitorError)
        }
    }
}

pub use platform::MonitorController;
