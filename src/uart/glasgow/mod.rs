//! Glasgow UART discovery and opening.

mod device;
mod management;
mod resources;
mod transport;

pub use device::{DeviceInfo, RecoveryCandidate, list_devices, list_recovery_candidates};

use crate::Result;

use super::Port;
use transport::GlasgowTransport;

/// Glasgow UART open parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenOptions {
    /// Select a Glasgow by USB serial number.
    pub serial: Option<String>,
    /// Requested line rate in bits per second.
    pub baud: u32,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            serial: None,
            baud: 115_200,
        }
    }
}

/// Select, provision, configure, and open one Glasgow UART port.
///
/// # Errors
///
/// Returns selection, access, protocol, timeout, or baud-validation errors from
/// discovery, resource validation, firmware update, or device setup.
pub fn open(options: &OpenOptions) -> Result<Port> {
    let devices = device::list_devices()?;
    let selected = device::select_device(&devices, options.serial.as_deref())?;
    Ok(Port::new(GlasgowTransport::open(selected, options.baud)?))
}
