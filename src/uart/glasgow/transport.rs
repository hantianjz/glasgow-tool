//! Glasgow FPGA provisioning and queued UART bulk transport.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nusb::io::{EndpointRead, EndpointWrite};
use nusb::transfer::{Bulk, In, Out};
use nusb::{Device, Interface, MaybeFuture};
use parking_lot::Mutex;

use crate::Error;
use crate::uart::transport::{Counters, Metadata, TransferFailureKind, TransferOutcome, Transport};

use super::device::{API_LEVEL, DeviceInfo};
use super::management::{
    Management, Register, ValidatedResource, upload_fx2_firmware, validate_resource,
};

const USB_TRANSFER_SIZE: usize = 32 * 1024;
const USB_TRANSFER_COUNT: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_millis(50);
const COUNTER_POLL: Duration = Duration::from_millis(250);
const DRAIN_POLL: Duration = Duration::from_millis(10);
const SHUTDOWN_SETTLE: Duration = Duration::from_millis(100);
const TX_IDLE_BIT: u32 = 1 << 31;

fn baud_divisor(requested: u32) -> Result<(u32, u32), Error> {
    if !(9_600..=12_000_000).contains(&requested) {
        return Err(Error::Selection(
            "baud rate must be between 9600 and 12000000".to_owned(),
        ));
    }
    let divisor = ((48_000_000_u64 + u64::from(requested) / 2) / u64::from(requested))
        .clamp(1, (1 << 20) - 1) as u32;
    let actual = 48_000_000 / divisor;
    let error = u64::from(actual.abs_diff(requested)) * 10_000 / u64::from(requested);
    if error > 200 {
        return Err(Error::Validation(format!(
            "requested baud {requested} is represented as {actual}, exceeding 2% error"
        )));
    }
    Ok((divisor, actual))
}

fn map_io_error(error: &io::Error) -> TransferOutcome {
    let kind = match error.kind() {
        io::ErrorKind::Interrupted => TransferFailureKind::Interrupted,
        io::ErrorKind::WouldBlock => TransferFailureKind::WouldBlock,
        io::ErrorKind::TimedOut => TransferFailureKind::TimedOut,
        io::ErrorKind::PermissionDenied => TransferFailureKind::Access,
        io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected | io::ErrorKind::UnexpectedEof => {
            TransferFailureKind::Disconnected
        }
        _ => TransferFailureKind::Protocol,
    };
    TransferOutcome::failed(0, kind, error.to_string())
}

fn load_bitstream(
    device: &Device,
    management: &mut Management,
    resource: &ValidatedResource,
) -> Result<(), Error> {
    if management.fpga_status()? == Some(resource.bitstream_id) {
        return Ok(());
    }
    let interface = device.claim_interface(1).wait().map_err(|error| {
        Error::Access(format!(
            "cannot claim Glasgow configuration interface: {error}"
        ))
    })?;
    interface.set_alt_setting(3).wait().map_err(|error| {
        Error::Access(format!(
            "cannot select Glasgow FPGA configuration mode: {error}"
        ))
    })?;
    let endpoint = interface.endpoint::<Bulk, Out>(0x02).map_err(|error| {
        Error::Protocol(format!("Glasgow configuration endpoint mismatch: {error}"))
    })?;
    let mut writer = EndpointWrite::new(endpoint, USB_TRANSFER_SIZE)
        .with_num_transfers(USB_TRANSFER_COUNT)
        .with_write_timeout(Duration::from_secs(2));
    writer
        .write_all(resource.embedded.bitstream)
        .and_then(|()| writer.flush())
        .map_err(|error| Error::Access(format!("Glasgow FPGA upload failed: {error}")))?;
    drop(writer);
    management.finish_fpga_load(resource.embedded.bitstream.len(), resource.bitstream_id)?;
    interface.set_alt_setting(0).wait().map_err(|error| {
        Error::Access(format!(
            "cannot close Glasgow configuration interface: {error}"
        ))
    })?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if management.fpga_status()? == Some(resource.bitstream_id) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout(
                "Glasgow FPGA did not report configuration complete".to_owned(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct SetupCleanup<'a> {
    management: &'a mut Management,
    rx_interface: Option<Interface>,
    tx_interface: Option<Interface>,
    vio_enabled: bool,
    armed: bool,
}

impl<'a> SetupCleanup<'a> {
    fn new(management: &'a mut Management) -> Self {
        Self {
            management,
            rx_interface: None,
            tx_interface: None,
            vio_enabled: false,
            armed: true,
        }
    }

    fn finish(mut self) -> (Interface, Interface) {
        self.armed = false;
        (
            self.rx_interface.take().expect("RX interface configured"),
            self.tx_interface.take().expect("TX interface configured"),
        )
    }
}

impl Drop for SetupCleanup<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.vio_enabled {
            let _ = self.management.set_fixed_profile(false);
        }
        if let Some(interface) = self.rx_interface.as_ref() {
            let _ = interface.set_alt_setting(0).wait();
        }
        if let Some(interface) = self.tx_interface.as_ref() {
            let _ = interface.set_alt_setting(0).wait();
        }
    }
}
pub struct GlasgowTransport {
    metadata: Metadata,
    management: Mutex<Management>,
    read: Mutex<EndpointRead<Bulk>>,
    write: Mutex<EndpointWrite<Bulk>>,
    rx_interface: Interface,
    tx_interface: Interface,
    rx_errors_register: Register,
    rx_overflow_register: Register,
    tx_state_register: Register,
    rx_errors_baseline: u32,
    rx_overflow_baseline: u32,
    rx_errors: AtomicU64,
    rx_overflow: AtomicU64,
    last_counter_poll: Mutex<Instant>,
    cancelled: AtomicBool,
}

impl GlasgowTransport {
    pub fn open(mut selected: DeviceInfo, requested_baud: u32) -> Result<Self, Error> {
        let resource = validate_resource(&selected.revision)?;
        if selected.api_level != API_LEVEL {
            selected = upload_fx2_firmware(&selected, &resource)?;
            if selected.api_level != API_LEVEL || selected.revision != resource.manifest.revision {
                return Err(Error::Protocol(
                    "Glasgow firmware re-enumerated with incompatible API or revision".to_owned(),
                ));
            }
        }
        let device = selected.native.open().wait().map_err(|error| {
            Error::Access(format!(
                "cannot open Glasgow {} at {}: {error}",
                selected.serial.as_deref().unwrap_or("<no serial>"),
                selected.path
            ))
        })?;
        let mut management = Management::claim(&device)?;
        load_bitstream(&device, &mut management, &resource)?;

        let rx_meta = &resource.manifest.pipe.rx;
        let tx_meta = &resource.manifest.pipe.tx;
        let mut setup = SetupCleanup::new(&mut management);
        setup.rx_interface = Some(device.claim_interface(rx_meta.interface).wait().map_err(
            |error| Error::Access(format!("cannot claim Glasgow UART RX interface: {error}")),
        )?);
        setup.tx_interface = Some(device.claim_interface(tx_meta.interface).wait().map_err(
            |error| Error::Access(format!("cannot claim Glasgow UART TX interface: {error}")),
        )?);
        let rx_interface = setup.rx_interface.as_ref().expect("assigned RX interface");
        let tx_interface = setup.tx_interface.as_ref().expect("assigned TX interface");
        rx_interface
            .set_alt_setting(rx_meta.alternate_setting)
            .wait()
            .map_err(|error| Error::Access(format!("cannot enable Glasgow UART RX: {error}")))?;
        tx_interface
            .set_alt_setting(tx_meta.alternate_setting)
            .wait()
            .map_err(|error| Error::Access(format!("cannot enable Glasgow UART TX: {error}")))?;

        let read_endpoint = rx_interface
            .endpoint::<Bulk, In>(rx_meta.endpoint)
            .map_err(|error| {
                Error::Protocol(format!("Glasgow UART RX endpoint mismatch: {error}"))
            })?;
        let write_endpoint = tx_interface
            .endpoint::<Bulk, Out>(tx_meta.endpoint)
            .map_err(|error| {
                Error::Protocol(format!("Glasgow UART TX endpoint mismatch: {error}"))
            })?;
        if read_endpoint.max_packet_size() != usize::from(rx_meta.max_packet)
            || write_endpoint.max_packet_size() != usize::from(tx_meta.max_packet)
        {
            return Err(Error::Protocol(
                "Glasgow UART endpoint max-packet mismatch".to_owned(),
            ));
        }
        let read = EndpointRead::new(read_endpoint, USB_TRANSFER_SIZE)
            .with_num_transfers(USB_TRANSFER_COUNT)
            .with_read_timeout(IO_TIMEOUT);
        let write = EndpointWrite::new(write_endpoint, USB_TRANSFER_SIZE)
            .with_num_transfers(USB_TRANSFER_COUNT)
            .with_write_timeout(IO_TIMEOUT);

        let (divisor, actual_baud) = baud_divisor(requested_baud)?;
        setup
            .management
            .write_register(&resource.manifest.registers.baud_divisor, divisor)?;
        setup.management.set_fixed_profile(true)?;
        setup.vio_enabled = true;
        let rx_errors_baseline = setup
            .management
            .read_register(&resource.manifest.registers.rx_errors)?;
        let rx_overflow_baseline = setup
            .management
            .read_register(&resource.manifest.registers.rx_overflow)?;
        let (rx_interface, tx_interface) = setup.finish();

        Ok(Self {
            metadata: Metadata {
                backend: "glasgow",
                serial: selected.serial,
                path: Some(selected.path),
                requested_baud,
                actual_baud,
                minimum_baud: 9_600,
                maximum_baud: 12_000_000,
            },
            management: Mutex::new(management),
            read: Mutex::new(read),
            write: Mutex::new(write),
            rx_interface,
            tx_interface,
            rx_errors_register: resource.manifest.registers.rx_errors.clone(),
            rx_overflow_register: resource.manifest.registers.rx_overflow.clone(),
            tx_state_register: resource.manifest.registers.tx_state.clone(),
            rx_errors_baseline,
            rx_overflow_baseline,
            rx_errors: AtomicU64::new(0),
            rx_overflow: AtomicU64::new(0),
            last_counter_poll: Mutex::new(Instant::now()),
            cancelled: AtomicBool::new(false),
        })
    }

    fn refresh_counters(&self) -> Result<(), Error> {
        let mut last_poll = self.last_counter_poll.lock();
        if last_poll.elapsed() < COUNTER_POLL {
            return Ok(());
        }
        let mut management = self.management.lock();
        let errors = management.read_register(&self.rx_errors_register)?;
        let overflow = management.read_register(&self.rx_overflow_register)?;
        self.rx_errors.store(
            u64::from(errors.saturating_sub(self.rx_errors_baseline)),
            Ordering::Relaxed,
        );
        self.rx_overflow.store(
            u64::from(overflow.saturating_sub(self.rx_overflow_baseline)),
            Ordering::Relaxed,
        );
        *last_poll = Instant::now();
        Ok(())
    }
}

impl Transport for GlasgowTransport {
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn read(&self, buffer: &mut [u8]) -> TransferOutcome {
        if self.cancelled.load(Ordering::Acquire) {
            return TransferOutcome::failed(0, TransferFailureKind::TimedOut, "cancelled");
        }
        let outcome = match self.read.lock().read(buffer) {
            Ok(completed) => TransferOutcome::complete(completed),
            Err(error) => map_io_error(&error),
        };
        if let Err(error) = self.refresh_counters() {
            return TransferOutcome::failed(
                outcome.completed,
                TransferFailureKind::Protocol,
                error.to_string(),
            );
        }
        outcome
    }

    fn write(&self, data: &[u8]) -> TransferOutcome {
        if self.cancelled.load(Ordering::Acquire) {
            return TransferOutcome::failed(0, TransferFailureKind::TimedOut, "cancelled");
        }
        match self.write.lock().write(data) {
            Ok(completed) => TransferOutcome::complete(completed),
            Err(error) => map_io_error(&error),
        }
    }
    fn submit_tx(&self) {
        self.write.lock().submit();
    }

    fn drain(&self, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        let flush_result = {
            let mut write = self.write.lock();
            write.set_write_timeout(timeout);
            let result = write.flush();
            write.set_write_timeout(IO_TIMEOUT);
            result
        };
        flush_result.map_err(|error| {
            if error.kind() == io::ErrorKind::TimedOut {
                Error::Timeout(format!(
                    "Glasgow UART USB drain exceeded {} ms",
                    timeout.as_millis()
                ))
            } else {
                Error::Access(format!("Glasgow UART USB drain failed: {error}"))
            }
        })?;
        loop {
            let state = self
                .management
                .lock()
                .read_register(&self.tx_state_register)?;
            if state & TX_IDLE_BIT != 0 && state & !TX_IDLE_BIT == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!(
                    "Glasgow UART did not drain within {} ms",
                    timeout.as_millis()
                )));
            }
            std::thread::sleep(DRAIN_POLL);
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.read.lock().cancel_all();
    }

    fn counters(&self) -> Counters {
        let _ = self.refresh_counters();
        Counters {
            rx_errors: self.rx_errors.load(Ordering::Relaxed),
            rx_overflow: self.rx_overflow.load(Ordering::Relaxed),
        }
    }
}

impl Drop for GlasgowTransport {
    fn drop(&mut self) {
        // Keep TX at idle briefly so a peer finishing the same session does not
        // decode the subsequent I/O-voltage transition as received data.
        std::thread::sleep(SHUTDOWN_SETTLE);
        let _ = self.management.get_mut().set_fixed_profile(false);
        let _ = self.rx_interface.set_alt_setting(0).wait();
        let _ = self.tx_interface.set_alt_setting(0).wait();
    }
}
