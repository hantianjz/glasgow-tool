//! Explicit FT232H direct-USB UART backend.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nusb::descriptors::TransferType;
use nusb::io::EndpointWrite;
use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, Direction, In, Out, Recipient};
use nusb::{Device, DeviceInfo, Endpoint, Interface, MaybeFuture};
use parking_lot::Mutex;

use crate::cli::AppError;
use crate::events::{Backend, HardwareCounters};
use crate::session::{TransferFailureKind, TransferOutcome, Transport, TransportMetadata};

use super::vcp::{FTDI_PID, FTDI_VID, PRODUCT, ftdi_high_speed_baud};

const SIO_RESET: u8 = 0;
const SIO_MODEM_CTRL: u8 = 1;
const SIO_SET_FLOW_CTRL: u8 = 2;
const SIO_SET_BAUD: u8 = 3;
const SIO_SET_DATA: u8 = 4;
const SIO_GET_MODEM_STATUS: u8 = 5;
const SIO_SET_LATENCY: u8 = 9;
const RESET_DEVICE: u16 = 0;
const PURGE_RX: u16 = 1;
const PURGE_TX: u16 = 2;
const MODEM_DTR_RTS_INACTIVE: u16 = 0x0300;
const INTERFACE_INDEX: u16 = 1;
const USB_TRANSFER_SIZE: usize = 32 * 1024;
const USB_TRANSFER_COUNT: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_millis(50);
const DRAIN_POLL: Duration = Duration::from_millis(10);
const LINE_OE: u8 = 1 << 1;
const LINE_PE: u8 = 1 << 2;
const LINE_FE: u8 = 1 << 3;
const LINE_BI: u8 = 1 << 4;
const LINE_TEMT: u8 = 1 << 6;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FtdiLineCounters {
    pub errors: u64,
    pub overflow: u64,
    pub transmitter_empty: bool,
}

/// Strip the two FTDI status bytes from every USB packet, including short packets.
pub fn strip_ftdi_status(
    transfer: &[u8],
    max_packet: usize,
) -> Result<(Vec<u8>, FtdiLineCounters), AppError> {
    let (payload, counters, _) = strip_ftdi_status_from(transfer, max_packet, Some(0))?;
    Ok((payload, counters))
}

fn strip_ftdi_status_from(
    transfer: &[u8],
    max_packet: usize,
    mut previous_line: Option<u8>,
) -> Result<(Vec<u8>, FtdiLineCounters, Option<u8>), AppError> {
    if max_packet < 2 {
        return Err(AppError::Protocol(
            "FTDI endpoint max-packet size is smaller than its status header".to_owned(),
        ));
    }
    let mut payload = Vec::with_capacity(transfer.len());
    let mut counters = FtdiLineCounters::default();
    for packet in transfer.chunks(max_packet) {
        if packet.len() < 2 {
            return Err(AppError::Protocol(
                "FTDI USB packet ended inside its status header".to_owned(),
            ));
        }
        let line = packet[1];
        if let Some(previous) = previous_line {
            let asserted = line & !previous;
            counters.overflow += u64::from(asserted & LINE_OE != 0);
            counters.errors += u64::from(asserted & (LINE_PE | LINE_FE | LINE_BI) != 0);
        }
        counters.transmitter_empty = line & LINE_TEMT != 0;
        previous_line = Some(line);
        payload.extend_from_slice(&packet[2..]);
    }
    Ok((payload, counters, previous_line))
}

#[derive(Clone)]
struct UsbDeviceInfo {
    serial: String,
    path: String,
    native: DeviceInfo,
}

fn list_devices() -> Result<Vec<UsbDeviceInfo>, AppError> {
    let mut devices = nusb::list_devices()
        .wait()
        .map_err(|error| AppError::Access(format!("cannot enumerate USB devices: {error}")))?
        .filter(|info| {
            info.vendor_id() == FTDI_VID
                && info.product_id() == FTDI_PID
                && info.product_string() == Some(PRODUCT)
        })
        .filter_map(|native| {
            Some(UsbDeviceInfo {
                serial: native.serial_number()?.to_owned(),
                path: native.bus_id().to_owned(),
                native,
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| (&left.serial, &left.path).cmp(&(&right.serial, &right.path)));
    Ok(devices)
}

fn select_device(serial: Option<&str>) -> Result<UsbDeviceInfo, AppError> {
    let devices = list_devices()?;
    let matches = devices
        .into_iter()
        .filter(|device| serial.is_none_or(|serial| device.serial == serial))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(AppError::Selection(
            "no matching C232HD-DDHSP-0 USB device was found".to_owned(),
        )),
        [device] => Ok(device.clone()),
        _ => Err(AppError::Selection(
            "multiple C232HD-DDHSP-0 devices match; specify --serial".to_owned(),
        )),
    }
}

fn discover_endpoints(device: &Device) -> Result<(u8, u8, usize), AppError> {
    for configuration in device.configurations() {
        for interface in configuration.interfaces() {
            if interface.interface_number() != 0 {
                continue;
            }
            for alternate in interface.alt_settings() {
                if alternate.alternate_setting() != 0 {
                    continue;
                }
                let mut rx = None;
                let mut tx = None;
                let mut packet = None;
                for endpoint in alternate.endpoints() {
                    if endpoint.transfer_type() != TransferType::Bulk {
                        continue;
                    }
                    match endpoint.direction() {
                        Direction::In => rx = Some(endpoint.address()),
                        Direction::Out => tx = Some(endpoint.address()),
                    }
                    packet = Some(endpoint.max_packet_size());
                }
                if let (Some(rx), Some(tx), Some(packet)) = (rx, tx, packet) {
                    return Ok((rx, tx, packet));
                }
            }
        }
    }
    Err(AppError::Protocol(
        "C232HD bulk endpoint descriptors are missing".to_owned(),
    ))
}

fn control_out(interface: &Interface, request: u8, value: u16, index: u16) -> Result<(), AppError> {
    interface
        .control_out(
            ControlOut {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request,
                value,
                index,
                data: &[],
            },
            IO_TIMEOUT,
        )
        .wait()
        .map_err(|error| {
            AppError::Access(format!("FTDI control request {request} failed: {error}"))
        })?;
    Ok(())
}

fn modem_status(interface: &Interface) -> Result<[u8; 2], AppError> {
    let response = interface
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: SIO_GET_MODEM_STATUS,
                value: 0,
                index: INTERFACE_INDEX,
                length: 2,
            },
            IO_TIMEOUT,
        )
        .wait()
        .map_err(|error| AppError::Access(format!("FTDI modem-status request failed: {error}")))?;
    response
        .as_slice()
        .try_into()
        .map_err(|_| AppError::Protocol("FTDI modem-status response is not two bytes".to_owned()))
}

fn configure_ftdi(interface: &Interface, requested_baud: u32) -> Result<u32, AppError> {
    control_out(interface, SIO_RESET, RESET_DEVICE, INTERFACE_INDEX)?;
    control_out(interface, SIO_RESET, PURGE_RX, INTERFACE_INDEX)?;
    control_out(interface, SIO_RESET, PURGE_TX, INTERFACE_INDEX)?;
    control_out(interface, SIO_SET_LATENCY, 16, INTERFACE_INDEX)?;
    let baud = ftdi_high_speed_baud(requested_baud)?;
    let value = baud.encoded_divisor as u16;
    let index = ((baud.encoded_divisor >> 8) as u16 & 0xff00) | INTERFACE_INDEX;
    control_out(interface, SIO_SET_BAUD, value, index)?;
    control_out(interface, SIO_SET_DATA, 8, INTERFACE_INDEX)?;
    control_out(interface, SIO_SET_FLOW_CTRL, 0, INTERFACE_INDEX)?;
    control_out(
        interface,
        SIO_MODEM_CTRL,
        MODEM_DTR_RTS_INACTIVE,
        INTERFACE_INDEX,
    )?;
    Ok(baud.actual)
}

struct UsbRx {
    endpoint: Endpoint<Bulk, In>,
    pending_payload: VecDeque<u8>,
    max_packet: usize,
    last_line_status: Option<u8>,
}

impl UsbRx {
    fn new(mut endpoint: Endpoint<Bulk, In>, max_packet: usize) -> Self {
        while endpoint.pending() < USB_TRANSFER_COUNT {
            let buffer = endpoint.allocate(USB_TRANSFER_SIZE);
            endpoint.submit(buffer);
        }
        Self {
            endpoint,
            pending_payload: VecDeque::new(),
            max_packet,
            last_line_status: None,
        }
    }

    fn read(&mut self, output: &mut [u8]) -> Result<(usize, FtdiLineCounters), TransferOutcome> {
        if !self.pending_payload.is_empty() {
            let count = output.len().min(self.pending_payload.len());
            for target in &mut output[..count] {
                *target = self
                    .pending_payload
                    .pop_front()
                    .expect("checked pending length");
            }
            return Ok((count, FtdiLineCounters::default()));
        }
        let Some(mut completion) = self.endpoint.wait_next_complete(IO_TIMEOUT) else {
            return Err(TransferOutcome::failed(
                0,
                TransferFailureKind::TimedOut,
                "FTDI USB read timed out",
            ));
        };
        let status_error = completion.status.as_ref().err().map(ToString::to_string);
        let stripped =
            strip_ftdi_status_from(&completion.buffer, self.max_packet, self.last_line_status);
        completion.buffer.clear();
        self.endpoint.submit(completion.buffer);
        let (payload, counters, last_line_status) = stripped.map_err(|error| {
            TransferOutcome::failed(0, TransferFailureKind::Protocol, error.to_string())
        })?;
        self.last_line_status = last_line_status;
        let count = output.len().min(payload.len());
        output[..count].copy_from_slice(&payload[..count]);
        self.pending_payload.extend(&payload[count..]);
        if let Some(error) = status_error {
            return Err(TransferOutcome::failed(
                count,
                TransferFailureKind::Access,
                format!("FTDI USB read failed: {error}"),
            ));
        }
        Ok((count, counters))
    }
}

pub struct UsbTransport {
    metadata: TransportMetadata,
    device: Device,
    interface: Option<Interface>,
    read: Mutex<Option<UsbRx>>,
    write: Mutex<Option<EndpointWrite<Bulk>>>,
    rx_errors: AtomicU64,
    rx_overflow: AtomicU64,
    cancelled: AtomicBool,
    driver_detached: bool,
}

impl UsbTransport {
    pub fn open(serial: Option<&str>, requested_baud: u32) -> Result<Self, AppError> {
        let selected = select_device(serial)?;
        let device = selected.native.open().wait().map_err(|error| {
            AppError::Access(format!(
                "cannot open C232HD USB serial {} at {}: {error}",
                selected.serial, selected.path
            ))
        })?;
        let (rx_address, tx_address, max_packet) = discover_endpoints(&device)?;
        let driver_detached = match device.detach_kernel_driver(0) {
            Ok(()) => true,
            Err(error)
                if error.kind() == nusb::ErrorKind::NotFound
                    || (cfg!(target_os = "linux") && error.os_error() == Some(61)) =>
            {
                device.attach_kernel_driver(0).map_err(|attach_error| {
                    AppError::Access(format!(
                        "cannot restore the VCP driver for C232HD serial {} at {}: {attach_error}",
                        selected.serial, selected.path
                    ))
                })?;
                device.detach_kernel_driver(0).map_err(|detach_error| {
                    AppError::Access(format!(
                        "cannot detach the restored VCP driver for C232HD serial {} at {}: {detach_error}",
                        selected.serial, selected.path
                    ))
                })?;
                true
            }
            Err(error) => {
                return Err(AppError::Access(format!(
                    "cannot detach the VCP driver for C232HD serial {} at {}: {error}",
                    selected.serial, selected.path
                )));
            }
        };
        let setup = (|| -> Result<_, AppError> {
            let interface = device.claim_interface(0).wait().map_err(|error| {
                AppError::Access(format!("cannot claim C232HD USB interface: {error}"))
            })?;
            let actual_baud = configure_ftdi(&interface, requested_baud)?;
            let read_endpoint = interface
                .endpoint::<Bulk, In>(rx_address)
                .map_err(|error| {
                    AppError::Protocol(format!("FTDI RX endpoint mismatch: {error}"))
                })?;
            let write_endpoint = interface
                .endpoint::<Bulk, Out>(tx_address)
                .map_err(|error| {
                    AppError::Protocol(format!("FTDI TX endpoint mismatch: {error}"))
                })?;
            let read = UsbRx::new(read_endpoint, max_packet);
            let write = EndpointWrite::new(write_endpoint, USB_TRANSFER_SIZE)
                .with_num_transfers(USB_TRANSFER_COUNT)
                .with_write_timeout(IO_TIMEOUT);
            Ok((interface, read, write, actual_baud))
        })();
        let (interface, read, write, actual_baud) = match setup {
            Ok(value) => value,
            Err(error) => {
                let _ = device.attach_kernel_driver(0);
                return Err(error);
            }
        };
        Ok(Self {
            metadata: TransportMetadata {
                backend: Backend::Usb,
                serial: selected.serial,
                path: Some(selected.path),
                requested_baud,
                actual_baud,
                minimum_baud: 9_600,
                maximum_baud: 12_000_000,
            },
            device,
            interface: Some(interface),
            read: Mutex::new(Some(read)),
            write: Mutex::new(Some(write)),
            rx_errors: AtomicU64::new(0),
            rx_overflow: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
            driver_detached,
        })
    }
}

impl Transport for UsbTransport {
    fn metadata(&self) -> &TransportMetadata {
        &self.metadata
    }

    fn read(&self, buffer: &mut [u8]) -> TransferOutcome {
        if self.cancelled.load(Ordering::Acquire) {
            return TransferOutcome::failed(0, TransferFailureKind::TimedOut, "cancelled");
        }
        let mut read = self.read.lock();
        let Some(read) = read.as_mut() else {
            return TransferOutcome::failed(0, TransferFailureKind::Disconnected, "closed");
        };
        match read.read(buffer) {
            Ok((completed, counters)) => {
                self.rx_errors.fetch_add(counters.errors, Ordering::Relaxed);
                self.rx_overflow
                    .fetch_add(counters.overflow, Ordering::Relaxed);
                if completed == 0 {
                    TransferOutcome::failed(
                        0,
                        TransferFailureKind::WouldBlock,
                        "FTDI USB status-only packet",
                    )
                } else {
                    TransferOutcome::complete(completed)
                }
            }
            Err(outcome) => outcome,
        }
    }

    fn write(&self, data: &[u8]) -> TransferOutcome {
        if self.cancelled.load(Ordering::Acquire) {
            return TransferOutcome::failed(0, TransferFailureKind::TimedOut, "cancelled");
        }
        let mut write = self.write.lock();
        let Some(write) = write.as_mut() else {
            return TransferOutcome::failed(0, TransferFailureKind::Disconnected, "closed");
        };
        match write.write(data) {
            Ok(completed) => TransferOutcome::complete(completed),
            Err(error) => TransferOutcome::failed(
                0,
                match error.kind() {
                    io::ErrorKind::Interrupted => TransferFailureKind::Interrupted,
                    io::ErrorKind::WouldBlock => TransferFailureKind::WouldBlock,
                    io::ErrorKind::TimedOut => TransferFailureKind::TimedOut,
                    _ => TransferFailureKind::Access,
                },
                error.to_string(),
            ),
        }
    }

    fn drain(&self, timeout: Duration) -> Result<(), AppError> {
        let deadline = Instant::now() + timeout;
        if let Some(write) = self.write.lock().as_mut() {
            write.set_write_timeout(timeout);
            let flush_result = write.flush();
            write.set_write_timeout(IO_TIMEOUT);
            flush_result.map_err(|error| {
                if error.kind() == io::ErrorKind::TimedOut {
                    AppError::Timeout(format!(
                        "FTDI USB drain exceeded {} ms",
                        timeout.as_millis()
                    ))
                } else {
                    AppError::Access(format!("FTDI USB drain failed: {error}"))
                }
            })?;
        }
        loop {
            let interface = self
                .interface
                .as_ref()
                .ok_or_else(|| AppError::Access("FTDI USB interface is closed".to_owned()))?;
            if modem_status(interface)?[1] & LINE_TEMT != 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(AppError::Timeout(format!(
                    "C232HD USB serial {} did not drain within {} ms",
                    self.metadata.serial,
                    timeout.as_millis()
                )));
            }
            std::thread::sleep(DRAIN_POLL);
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(read) = self.read.lock().as_mut() {
            read.endpoint.cancel_all();
        }
    }

    fn counters(&self) -> HardwareCounters {
        HardwareCounters {
            rx_errors: self.rx_errors.load(Ordering::Relaxed),
            rx_overflow: self.rx_overflow.load(Ordering::Relaxed),
        }
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        if let Some(mut read) = self.read.get_mut().take() {
            read.endpoint.cancel_all();
            while read.endpoint.pending() != 0 {
                if read.endpoint.wait_next_complete(IO_TIMEOUT).is_none() {
                    break;
                }
            }
        }
        if let Some(write) = self.write.get_mut().take() {
            let mut endpoint = write.into_inner();
            endpoint.cancel_all();
            while endpoint.pending() != 0 {
                if endpoint.wait_next_complete(IO_TIMEOUT).is_none() {
                    break;
                }
            }
        }
        self.interface.take();
        if self.driver_detached {
            for _ in 0..10 {
                if self.device.attach_kernel_driver(0).is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
