//! Native OS serial-driver backend for the C232HD-DDHSP-0.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use serialport::{
    ClearBuffer, DataBits, FlowControl, Parity, SerialPort, SerialPortType, StopBits,
};

use crate::Error;
use crate::uart::transport::{Counters, Metadata, TransferFailureKind, TransferOutcome, Transport};

pub const FTDI_VID: u16 = 0x0403;
pub const FTDI_PID: u16 = 0x6014;
pub const PRODUCT: &str = "C232HD-DDHSP-0";
const IO_TIMEOUT: Duration = Duration::from_millis(50);
const DRAIN_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VcpDevice {
    pub serial: String,
    pub vid: String,
    pub pid: String,
    pub product: String,
    pub port: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stable_path: Option<String>,
}

#[derive(Default)]
struct UsbMetadata {
    vid: Option<u16>,
    pid: Option<u16>,
    serial: Option<String>,
    product: Option<String>,
}

#[cfg(target_os = "linux")]
fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "linux")]
fn linux_usb_metadata(port: &str) -> UsbMetadata {
    let Some(name) = Path::new(port).file_name() else {
        return UsbMetadata::default();
    };
    let Ok(mut current) = fs::canonicalize(Path::new("/sys/class/tty").join(name).join("device"))
    else {
        return UsbMetadata::default();
    };
    loop {
        let vid = read_trimmed(&current.join("idVendor"))
            .and_then(|value| u16::from_str_radix(&value, 16).ok());
        let pid = read_trimmed(&current.join("idProduct"))
            .and_then(|value| u16::from_str_radix(&value, 16).ok());
        if vid.is_some() || pid.is_some() {
            return UsbMetadata {
                vid,
                pid,
                serial: read_trimmed(&current.join("serial")),
                product: read_trimmed(&current.join("product")),
            };
        }
        if !current.pop() {
            return UsbMetadata::default();
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn linux_usb_metadata(_port: &str) -> UsbMetadata {
    UsbMetadata::default()
}

#[cfg(target_os = "linux")]
fn stable_path(port: &str) -> Option<String> {
    let canonical_port = fs::canonicalize(port).ok()?;
    let entries = fs::read_dir("/dev/serial/by-id").ok()?;
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (fs::canonicalize(&path).ok()? == canonical_port).then_some(path)
        })
        .min()
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "linux"))]
fn stable_path(_port: &str) -> Option<String> {
    None
}

pub fn list_devices() -> Result<Vec<VcpDevice>, Error> {
    let ports = serialport::available_ports()
        .map_err(|error| Error::Access(format!("cannot enumerate serial ports: {error}")))?;
    let mut devices = Vec::new();
    for port in ports {
        let sysfs = linux_usb_metadata(&port.port_name);
        let usb = match port.port_type {
            SerialPortType::UsbPort(info) => Some(info),
            _ => None,
        };
        let vid = usb.as_ref().map(|info| info.vid).or(sysfs.vid);
        let pid = usb.as_ref().map(|info| info.pid).or(sysfs.pid);
        let serial = usb
            .as_ref()
            .and_then(|info| info.serial_number.clone())
            .or(sysfs.serial);
        let product = usb
            .as_ref()
            .and_then(|info| info.product.clone())
            .or(sysfs.product);
        if vid != Some(FTDI_VID) || pid != Some(FTDI_PID) || product.as_deref() != Some(PRODUCT) {
            continue;
        }
        let Some(serial) = serial else {
            continue;
        };
        devices.push(VcpDevice {
            serial,
            vid: format!("{FTDI_VID:04x}"),
            pid: format!("{FTDI_PID:04x}"),
            product: PRODUCT.to_owned(),
            stable_path: stable_path(&port.port_name),
            port: port.port_name,
        });
    }
    devices.sort_by(|left, right| (&left.serial, &left.port).cmp(&(&right.serial, &right.port)));
    devices.dedup_by(|left, right| left.serial == right.serial && left.port == right.port);
    Ok(devices)
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub fn select_device(
    devices: &[VcpDevice],
    serial: Option<&str>,
    port: Option<&Path>,
) -> Result<VcpDevice, Error> {
    let matches: Vec<_> = devices
        .iter()
        .filter(|device| serial.is_none_or(|serial| device.serial == serial))
        .filter(|device| {
            port.is_none_or(|port| {
                same_path(Path::new(&device.port), port)
                    || device
                        .stable_path
                        .as_deref()
                        .is_some_and(|stable| same_path(Path::new(stable), port))
            })
        })
        .cloned()
        .collect();
    match matches.as_slice() {
        [] if serial.is_some() && port.is_some() => Err(Error::Selection(
            "--serial and --port do not identify the same C232HD-DDHSP-0".to_owned(),
        )),
        [] => Err(Error::Selection(
            "no matching C232HD-DDHSP-0 was found".to_owned(),
        )),
        [device] => Ok(device.clone()),
        _ => Err(Error::Selection(
            "multiple C232HD-DDHSP-0 devices match; specify --serial".to_owned(),
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BaudResult {
    pub divisor_eighths: u32,
    pub encoded_divisor: u32,
    pub actual: u32,
}

pub fn ftdi_high_speed_baud(requested: u32) -> Result<BaudResult, Error> {
    if !(9_600..=12_000_000).contains(&requested) {
        return Err(Error::Selection(
            "baud rate must be between 9600 and 12000000".to_owned(),
        ));
    }
    let (divisor_eighths, mut encoded_divisor, actual) = if requested >= 12_000_000 {
        (8, 0, 12_000_000)
    } else if requested >= 8_000_000 {
        (12, 1, 8_000_000)
    } else if requested >= 6_000_000 {
        (16, 2, 6_000_000)
    } else {
        const FRACTION_CODE: [u32; 8] = [0, 3, 2, 4, 1, 5, 6, 7];
        let divisor = ((96_000_000_u64 + u64::from(requested) / 2) / u64::from(requested))
            .clamp(16, 0x1_ffff) as u32;
        let actual = u32::try_from((96_000_000_u64 + u64::from(divisor) / 2) / u64::from(divisor))
            .expect("FTDI divisor result fits in u32");
        let encoded = (divisor >> 3) | (FRACTION_CODE[(divisor & 7) as usize] << 14);
        (divisor, encoded, actual)
    };
    encoded_divisor |= 0x2_0000;
    let error = u64::from(actual.abs_diff(requested)) * 10_000 / u64::from(requested);
    if error > 200 {
        return Err(Error::Validation(format!(
            "requested baud {requested} is represented as {actual}, exceeding 2% error"
        )));
    }
    Ok(BaudResult {
        divisor_eighths,
        encoded_divisor,
        actual,
    })
}

pub struct VcpTransport {
    metadata: Metadata,
    read_port: Mutex<Box<dyn SerialPort>>,
    write_port: Mutex<Box<dyn SerialPort>>,
    cancelled: AtomicBool,
}

impl VcpTransport {
    pub fn open(
        device: &VcpDevice,
        requested_baud: u32,
        explicit_port: Option<&Path>,
    ) -> Result<Self, Error> {
        let baud = ftdi_high_speed_baud(requested_baud)?;
        let path = explicit_port
            .map(PathBuf::from)
            .or_else(|| device.stable_path.as_deref().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(&device.port));
        let mut builder = serialport::new(path.to_string_lossy(), requested_baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .timeout(IO_TIMEOUT)
            .dtr_on_open(false);
        #[cfg(unix)]
        {
            builder = builder.exclusive(true);
        }
        let mut read_port = builder.open().map_err(|error| {
            Error::Access(format!(
                "cannot open C232HD serial {} at {}: {error}",
                device.serial,
                path.display()
            ))
        })?;
        read_port
            .write_data_terminal_ready(false)
            .map_err(|error| {
                Error::Access(format!(
                    "cannot deassert DTR for C232HD serial {} at {}: {error}",
                    device.serial,
                    path.display()
                ))
            })?;
        read_port.write_request_to_send(false).map_err(|error| {
            Error::Access(format!(
                "cannot deassert RTS for C232HD serial {} at {}: {error}",
                device.serial,
                path.display()
            ))
        })?;
        read_port.clear(ClearBuffer::Input).map_err(|error| {
            Error::Access(format!(
                "cannot clear stale input for C232HD serial {} at {}: {error}",
                device.serial,
                path.display()
            ))
        })?;
        let write_port = read_port.try_clone().map_err(|error| {
            Error::Access(format!(
                "cannot clone C232HD serial {} at {}: {error}",
                device.serial,
                path.display()
            ))
        })?;
        Ok(Self {
            metadata: Metadata {
                backend: "vcp",
                serial: Some(device.serial.clone()),
                path: Some(path.to_string_lossy().into_owned()),
                requested_baud,
                actual_baud: baud.actual,
                minimum_baud: 9_600,
                maximum_baud: 12_000_000,
            },
            read_port: Mutex::new(read_port),
            write_port: Mutex::new(write_port),
            cancelled: AtomicBool::new(false),
        })
    }
}

fn io_outcome(result: io::Result<usize>) -> TransferOutcome {
    match result {
        Ok(completed) => TransferOutcome::complete(completed),
        Err(error) => {
            let kind = match error.kind() {
                io::ErrorKind::Interrupted => TransferFailureKind::Interrupted,
                io::ErrorKind::WouldBlock => TransferFailureKind::WouldBlock,
                io::ErrorKind::TimedOut => TransferFailureKind::TimedOut,
                io::ErrorKind::PermissionDenied => TransferFailureKind::Access,
                io::ErrorKind::BrokenPipe
                | io::ErrorKind::NotConnected
                | io::ErrorKind::UnexpectedEof => TransferFailureKind::Disconnected,
                _ => TransferFailureKind::Protocol,
            };
            TransferOutcome::failed(0, kind, error.to_string())
        }
    }
}

impl Transport for VcpTransport {
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn read(&self, buffer: &mut [u8]) -> TransferOutcome {
        if self.cancelled.load(Ordering::Acquire) {
            return TransferOutcome::failed(0, TransferFailureKind::TimedOut, "cancelled");
        }
        io_outcome(self.read_port.lock().read(buffer))
    }

    fn write(&self, data: &[u8]) -> TransferOutcome {
        if self.cancelled.load(Ordering::Acquire) {
            return TransferOutcome::failed(0, TransferFailureKind::TimedOut, "cancelled");
        }
        io_outcome(self.write_port.lock().write(data))
    }

    fn drain(&self, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        loop {
            let queued = self.write_port.lock().bytes_to_write().map_err(|error| {
                Error::Access(format!(
                    "cannot inspect C232HD output queue for serial {}: {error}",
                    self.metadata.serial.as_deref().unwrap_or("<no serial>")
                ))
            })?;
            if queued == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "C232HD serial {} did not drain within {} ms",
                    self.metadata.serial.as_deref().unwrap_or("<no serial>"),
                    timeout.as_millis()
                )));
            }
            std::thread::sleep(DRAIN_POLL);
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn counters(&self) -> Counters {
        Counters::default()
    }
}
#[cfg(test)]
mod tests {
    use super::ftdi_high_speed_baud;

    #[test]
    fn baud_boundaries_enforce_two_percent_error() {
        let low = ftdi_high_speed_baud(9_600).unwrap();
        assert!(low.actual.abs_diff(9_600) * 100 <= 9_600 * 2);
        let high = ftdi_high_speed_baud(12_000_000).unwrap();
        assert_eq!(high.actual, 12_000_000);
        assert_ne!(high.encoded_divisor & 0x2_0000, 0);
        assert!(ftdi_high_speed_baud(10_000_000).is_err());
        assert!(ftdi_high_speed_baud(9_599).is_err());
        assert!(ftdi_high_speed_baud(12_000_001).is_err());
    }
}
