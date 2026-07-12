use std::collections::HashSet;
use std::mem;
use std::ptr;

use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
    QueryDisplayConfig,
};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};

use super::{MonitorError, wide_to_string, win32_status};

pub(super) fn active_display_pnp_ids() -> Result<HashSet<String>, MonitorError> {
    let flags = QDC_ONLY_ACTIVE_PATHS;
    let mut path_count = 0;
    let mut mode_count = 0;

    for _ in 0..3 {
        let status =
            unsafe { GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) };
        if status != ERROR_SUCCESS {
            return Err(win32_status("GetDisplayConfigBufferSizes failed", status));
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
            return Err(win32_status("QueryDisplayConfig failed", status));
        }

        paths.truncate(path_count as usize);
        return Ok(paths
            .into_iter()
            .filter_map(active_display_pnp_id)
            .collect());
    }

    Err(win32_status(
        "QueryDisplayConfig remained unstable",
        ERROR_INSUFFICIENT_BUFFER,
    ))
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
        DisplayConfigGetDeviceInfo(&mut target_name.header as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER)
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

pub(super) fn normalize_pnp_id(value: &str) -> String {
    value.replace(['#', '/'], "\\").to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::{normalize_pnp_id, pnp_id_from_monitor_device_path};

    #[test]
    fn monitor_device_paths_become_normalized_pnp_ids() {
        assert_eq!(
            pnp_id_from_monitor_device_path(r"\\?\DISPLAY#DEL40A9#5&12345&0&UID4352#{guid}")
                .as_deref(),
            Some(r"DISPLAY\DEL40A9\5&12345&0&UID4352")
        );
        assert_eq!(
            normalize_pnp_id("display#abc/instance"),
            r"DISPLAY\ABC\INSTANCE"
        );
    }
}
