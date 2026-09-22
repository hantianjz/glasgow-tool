//! C232HD UART discovery and opening.

mod usb;
mod vcp;

use std::path::PathBuf;

use crate::{Error, Result};

use super::Port;

/// C232HD transport backend.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Backend {
    /// Native operating-system serial driver.
    #[default]
    Vcp,
    /// Direct FTDI USB protocol.
    Usb,
}

/// Normalized descriptor-only C232HD device information.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    /// Discovery backend.
    pub backend: Backend,
    /// USB serial number.
    pub serial: String,
    /// USB vendor identifier.
    pub vendor_id: u16,
    /// USB product identifier.
    pub product_id: u16,
    /// USB product string.
    pub product: String,
    /// Backend attachment or device path.
    pub path: String,
    /// Stable operating-system path, when available.
    pub stable_path: Option<PathBuf>,
}

/// C232HD UART open parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenOptions {
    /// Transport backend.
    pub backend: Backend,
    /// Select by USB serial number.
    pub serial: Option<String>,
    /// Select a VCP device path.
    pub port: Option<PathBuf>,
    /// Requested line rate in bits per second.
    pub baud: u32,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            backend: Backend::Vcp,
            serial: None,
            port: None,
            baud: 115_200,
        }
    }
}

/// Enumerate C232HD descriptors without opening devices or detaching drivers.
///
/// # Errors
///
/// Returns [`Error::Access`] when backend discovery fails.
pub fn list_devices(backend: Backend) -> Result<Vec<DeviceInfo>> {
    match backend {
        Backend::Vcp => Ok(vcp::list_devices()?
            .into_iter()
            .map(|device| DeviceInfo {
                backend,
                serial: device.serial,
                vendor_id: vcp::FTDI_VID,
                product_id: vcp::FTDI_PID,
                product: device.product,
                path: device.port,
                stable_path: device.stable_path.map(PathBuf::from),
            })
            .collect()),
        Backend::Usb => Ok(usb::list_devices()?
            .into_iter()
            .map(|device| DeviceInfo {
                backend,
                serial: device.serial,
                vendor_id: vcp::FTDI_VID,
                product_id: vcp::FTDI_PID,
                product: vcp::PRODUCT.to_owned(),
                path: device.path,
                stable_path: None,
            })
            .collect()),
    }
}

/// Select, configure, and open one C232HD UART port.
///
/// # Errors
///
/// Returns selection, access, protocol, timeout, or baud-validation errors from
/// the selected backend.
pub fn open(options: &OpenOptions) -> Result<Port> {
    match options.backend {
        Backend::Vcp => {
            let devices = vcp::list_devices()?;
            let selected =
                vcp::select_device(&devices, options.serial.as_deref(), options.port.as_deref())?;
            Ok(Port::new(vcp::VcpTransport::open(
                &selected,
                options.baud,
                options.port.as_deref(),
            )?))
        }
        Backend::Usb => {
            if options.port.is_some() {
                return Err(Error::Selection(
                    "--port is valid only with --backend vcp".to_owned(),
                ));
            }
            Ok(Port::new(usb::UsbTransport::open(
                options.serial.as_deref(),
                options.baud,
            )?))
        }
    }
}
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::Error;

    use super::{Backend, OpenOptions, open};

    #[test]
    fn usb_open_rejects_a_vcp_path_before_discovery() {
        let result = open(&OpenOptions {
            backend: Backend::Usb,
            port: Some(PathBuf::from("/dev/ttyUSB0")),
            ..OpenOptions::default()
        });
        assert!(matches!(result, Err(Error::Selection(_))));
    }
}
