//! Descriptor-only Glasgow discovery and deterministic selection.

use nusb::{DeviceInfo, MaybeFuture};
use serde::Serialize;

use crate::cli::AppError;

pub const GLASGOW_VID: u16 = 0x20b7;
pub const GLASGOW_PID: u16 = 0x9db1;
pub const CYPRESS_VID: u16 = 0x04b4;
pub const CYPRESS_PID: u16 = 0x8613;
pub const API_LEVEL: u8 = 9;

#[derive(Clone, Debug, Serialize)]
pub struct GlasgowDeviceInfo {
    pub serial: Option<String>,
    pub vid: String,
    pub pid: String,
    pub revision: String,
    pub api_level: u8,
    pub api_compatible: bool,
    pub path: String,
    #[serde(skip)]
    pub native: DeviceInfo,
}

#[cfg(target_os = "linux")]
fn attachment_path(info: &DeviceInfo) -> String {
    info.bus_id().to_owned()
}

#[cfg(not(target_os = "linux"))]
fn attachment_path(info: &DeviceInfo) -> String {
    info.bus_id().to_owned()
}

#[must_use]
pub fn decode_revision(value: u8) -> String {
    let major = value >> 4;
    let minor = value & 0x0f;
    if (1..=26).contains(&major) && minor <= 9 {
        let letter = char::from(b'A' + major - 1);
        format!("{letter}{minor}")
    } else {
        format!("unknown-{value:02x}")
    }
}

/// Enumerate descriptors only. This function never opens a device.
pub fn list_devices() -> Result<Vec<GlasgowDeviceInfo>, AppError> {
    let mut devices = nusb::list_devices()
        .wait()
        .map_err(|error| AppError::Access(format!("cannot enumerate USB devices: {error}")))?
        .filter(|info| info.vendor_id() == GLASGOW_VID && info.product_id() == GLASGOW_PID)
        .map(|native| {
            let version = native.device_version();
            let api_level = (version >> 8) as u8;
            GlasgowDeviceInfo {
                serial: native.serial_number().map(str::to_owned),
                vid: format!("{GLASGOW_VID:04x}"),
                pid: format!("{GLASGOW_PID:04x}"),
                revision: decode_revision(version as u8),
                api_level,
                api_compatible: api_level == API_LEVEL,
                path: attachment_path(&native),
                native,
            }
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| (&left.serial, &left.path).cmp(&(&right.serial, &right.path)));
    Ok(devices)
}

pub fn select_device(
    devices: &[GlasgowDeviceInfo],
    serial: Option<&str>,
) -> Result<GlasgowDeviceInfo, AppError> {
    let matches = devices
        .iter()
        .filter(|device| serial.is_none_or(|serial| device.serial.as_deref() == Some(serial)))
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(AppError::Selection(
            "no matching Glasgow was found".to_owned(),
        )),
        [device] => Ok(device.clone()),
        _ => Err(AppError::Selection(
            "multiple Glasgow devices match; specify --serial".to_owned(),
        )),
    }
}

pub fn list_recovery_candidates() -> Result<Vec<DeviceInfo>, AppError> {
    Ok(nusb::list_devices()
        .wait()
        .map_err(|error| AppError::Access(format!("cannot enumerate USB devices: {error}")))?
        .filter(|info| info.vendor_id() == CYPRESS_VID && info.product_id() == CYPRESS_PID)
        .collect())
}
