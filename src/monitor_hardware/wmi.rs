use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wmi::WMIConnection;

use super::MonitorError;
use super::display_config::{active_display_pnp_ids, normalize_pnp_id};

pub(super) struct WmiMonitor {
    name: String,
    brightness: i32,
    instance_path: String,
}

impl WmiMonitor {
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn brightness(&self) -> i32 {
        self.brightness
    }

    pub(super) fn set_brightness(&mut self, percent: i32) -> Result<(), MonitorError> {
        let percent = percent.clamp(0, 100);
        let connection = wmi_connection("ROOT\\WMI")?;
        let input = WmiSetBrightnessInput {
            Timeout: 0,
            Brightness: percent as u8,
        };
        let output: WmiSetBrightnessOutput = connection
            .exec_instance_method::<WmiMonitorBrightnessMethods, _>(
                &self.instance_path,
                "WmiSetBrightness",
                input,
            )
            .map_err(|error| MonitorError::wmi("WmiSetBrightness failed", error))?;

        if output.ReturnValue != 0 {
            return Err(MonitorError::Win32 {
                context: "WmiSetBrightness failed",
                code: output.ReturnValue,
            });
        }

        self.brightness = percent;
        Ok(())
    }
}

pub(super) fn discover() -> Result<Vec<WmiMonitor>, MonitorError> {
    let connection = wmi_connection("ROOT\\WMI")?;
    let brightness_monitors: Vec<WmiMonitorBrightness> = connection
        .raw_query(
            "SELECT InstanceName, Active, CurrentBrightness FROM WmiMonitorBrightness WHERE Active = TRUE",
        )
        .map_err(|error| MonitorError::wmi("WMI brightness query failed", error))?;
    let brightness_methods: Vec<WmiMonitorBrightnessMethodsInstance> = connection
        .raw_query(
            "SELECT InstanceName, Active, __PATH FROM WmiMonitorBrightnessMethods WHERE Active = TRUE",
        )
        .map_err(|error| MonitorError::wmi("WMI brightness methods query failed", error))?;
    let method_paths: HashMap<String, String> = brightness_methods
        .into_iter()
        .filter(|monitor| monitor.Active)
        .map(|monitor| (monitor.InstanceName, monitor.path))
        .collect();
    let active_pnp_ids = active_display_pnp_ids().unwrap_or_else(|error| {
        eprintln!("failed to query active display paths: {error}");
        Default::default()
    });
    let friendly_names = wmi_monitor_names(&connection).unwrap_or_else(|error| {
        eprintln!("failed to query WMI monitor names: {error}");
        HashMap::new()
    });
    let pnp_names = pnp_monitor_names().unwrap_or_else(|error| {
        eprintln!("failed to query PNP monitor names: {error}");
        HashMap::new()
    });

    Ok(brightness_monitors
        .into_iter()
        .filter(|monitor| monitor.Active)
        .filter_map(|monitor| {
            let instance_path = method_paths.get(&monitor.InstanceName)?.clone();
            let pnp_id = wmi_instance_to_pnp_id(&monitor.InstanceName);
            if !active_pnp_ids.is_empty()
                && pnp_id
                    .as_ref()
                    .is_some_and(|pnp_id| !active_pnp_ids.contains(pnp_id))
            {
                return None;
            }

            let name = friendly_names
                .get(&monitor.InstanceName)
                .cloned()
                .or_else(|| pnp_id.as_ref().and_then(|id| pnp_names.get(id).cloned()))
                .unwrap_or_else(|| "Integrated Monitor".into());

            Some(WmiMonitor {
                name,
                brightness: monitor.CurrentBrightness as i32,
                instance_path,
            })
        })
        .collect())
}

fn wmi_monitor_names(connection: &WMIConnection) -> Result<HashMap<String, String>, MonitorError> {
    let monitors = connection
        .raw_query::<WmiMonitorId>(
            "SELECT InstanceName, Active, UserFriendlyName FROM WmiMonitorID WHERE Active = TRUE",
        )
        .map_err(|error| MonitorError::wmi("WMI monitor name query failed", error))?;

    Ok(monitors
        .into_iter()
        .filter(|monitor| monitor.Active)
        .filter_map(|monitor| {
            friendly_name_from_wmi(&monitor.UserFriendlyName)
                .map(|name| (monitor.InstanceName, name))
        })
        .collect())
}

fn pnp_monitor_names() -> Result<HashMap<String, String>, MonitorError> {
    let connection = wmi_connection("ROOT\\CIMV2")?;
    let monitors = connection
        .raw_query::<Win32PnpEntity>(
            "SELECT Name, PNPDeviceID FROM Win32_PnPEntity WHERE PNPClass = 'Monitor'",
        )
        .map_err(|error| MonitorError::wmi("PNP monitor name query failed", error))?;

    Ok(monitors
        .into_iter()
        .filter_map(|monitor| {
            Some((
                normalize_pnp_id(&monitor.PNPDeviceID?),
                monitor.Name?.trim().to_string(),
            ))
        })
        .filter(|(_, name)| !name.is_empty())
        .collect())
}

fn wmi_connection(namespace: &str) -> Result<WMIConnection, MonitorError> {
    WMIConnection::with_namespace_path(namespace)
        .map_err(|error| MonitorError::wmi("WMI connection failed", error))
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

#[cfg(test)]
mod tests {
    use super::{friendly_name_from_wmi, wmi_instance_to_pnp_id};

    #[test]
    fn wmi_names_and_instance_ids_are_normalized() {
        assert_eq!(
            friendly_name_from_wmi(&Some(vec![68, 101, 108, 108, 0, 0])).as_deref(),
            Some("Dell")
        );
        assert_eq!(
            wmi_instance_to_pnp_id("DISPLAY\\BOE1234\\4&ABCD&0&UID265988_0").as_deref(),
            Some("DISPLAY\\BOE1234\\4&ABCD&0&UID265988")
        );
    }
}
