use nusb::{DeviceInfo as NativeDeviceInfo, MaybeFuture};

use crate::{Error, Result};

pub(super) const GLASGOW_VID: u16 = 0x20b7;
pub(super) const GLASGOW_PID: u16 = 0x9db1;
pub(super) const CYPRESS_VID: u16 = 0x04b4;
pub(super) const CYPRESS_PID: u16 = 0x8613;
pub(super) const API_LEVEL: u8 = 9;

/// Descriptor-only information about a normal Glasgow device.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    /// USB serial number, when supplied by the device.
    pub serial: Option<String>,
    /// USB vendor identifier.
    pub vendor_id: u16,
    /// USB product identifier.
    pub product_id: u16,
    /// Hardware revision string.
    pub revision: String,
    /// Firmware API level.
    pub api_level: u8,
    /// Whether the firmware API already matches this library.
    pub api_compatible: bool,
    /// Stable attachment path where available.
    pub path: String,
    pub(super) native: NativeDeviceInfo,
}

/// Descriptor-only information about a Cypress recovery-mode device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryCandidate {
    /// USB vendor identifier.
    pub vendor_id: u16,
    /// USB product identifier.
    pub product_id: u16,
    /// Attachment path.
    pub path: String,
}

fn attachment_path(info: &NativeDeviceInfo) -> String {
    info.bus_id().to_owned()
}

#[must_use]
pub(super) fn decode_revision(value: u8) -> String {
    let major = value >> 4;
    let minor = value & 0x0f;
    if (1..=26).contains(&major) && minor <= 9 {
        let letter = char::from(b'A' + major - 1);
        format!("{letter}{minor}")
    } else {
        format!("unknown-{value:02x}")
    }
}

/// Enumerate normal Glasgow descriptors without opening or modifying devices.
///
/// # Errors
///
/// Returns [`Error::Access`] when USB descriptor enumeration fails.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mut devices = nusb::list_devices()
        .wait()
        .map_err(|error| Error::Access(format!("cannot enumerate USB devices: {error}")))?
        .filter(|info| info.vendor_id() == GLASGOW_VID && info.product_id() == GLASGOW_PID)
        .map(|native| {
            let version = native.device_version();
            let api_level = (version >> 8) as u8;
            DeviceInfo {
                serial: native.serial_number().map(str::to_owned),
                vendor_id: GLASGOW_VID,
                product_id: GLASGOW_PID,
                revision: decode_revision(version.to_le_bytes()[0]),
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

pub(super) fn select_device(devices: &[DeviceInfo], serial: Option<&str>) -> Result<DeviceInfo> {
    let matches = devices
        .iter()
        .filter(|device| serial.is_none_or(|serial| device.serial.as_deref() == Some(serial)))
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(Error::Selection("no matching Glasgow was found".to_owned())),
        [device] => Ok(device.clone()),
        _ => Err(Error::Selection(
            "multiple Glasgow devices match; specify --serial".to_owned(),
        )),
    }
}

/// Enumerate recovery-mode descriptors without opening or modifying devices.
///
/// # Errors
///
/// Returns [`Error::Access`] when USB descriptor enumeration fails.
pub fn list_recovery_candidates() -> Result<Vec<RecoveryCandidate>> {
    let mut candidates = nusb::list_devices()
        .wait()
        .map_err(|error| Error::Access(format!("cannot enumerate USB devices: {error}")))?
        .filter(|info| info.vendor_id() == CYPRESS_VID && info.product_id() == CYPRESS_PID)
        .map(|info| RecoveryCandidate {
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            path: attachment_path(&info),
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(candidates)
}
