use std::collections::VecDeque;
use std::io::{self, Cursor, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use glasgow_tool::Tool;
use glasgow_tool::c232::usb::strip_ftdi_status;
use glasgow_tool::c232::vcp::ftdi_high_speed_baud;
use glasgow_tool::cli::AppError;
use glasgow_tool::events::{Backend, EventLog, HardwareCounters};
use glasgow_tool::glasgow::management::validate_resource_data;
use glasgow_tool::resources::{GLASGOW_RESOURCES, for_revision};
use glasgow_tool::session::{
    SessionMode, SessionOptions, StopReason, TransferFailureKind, TransferOutcome, Transport,
    TransportMetadata, run_session,
};
use parking_lot::Mutex;

#[test]
fn revision_resources_are_exact_and_unknown_revisions_are_rejected() {
    let revisions = GLASGOW_RESOURCES
        .iter()
        .map(|resource| resource.revision)
        .collect::<Vec<_>>();
    assert_eq!(revisions, ["C0", "C1", "C2", "C3"]);
    assert!(for_revision("C0").is_some());
    assert!(for_revision("C3").is_some());
    assert!(for_revision("B3").is_none());
    assert!(for_revision("C4").is_none());
}

#[test]
fn resource_validation_rejects_schema_and_digest_corruption() {
    let resource = for_revision("C3").unwrap();
    validate_resource_data("C3", resource.manifest, resource.bitstream).unwrap();

    let mut manifest: serde_json::Value = serde_json::from_slice(resource.manifest).unwrap();
    manifest["schema_version"] = 2.into();
    let invalid_schema = serde_json::to_vec(&manifest).unwrap();
    assert!(validate_resource_data("C3", &invalid_schema, resource.bitstream).is_err());

    let mut corrupted = resource.bitstream.to_vec();
    corrupted[0] ^= 1;
    assert!(validate_resource_data("C3", resource.manifest, &corrupted).is_err());
}

#[test]
fn ft232h_baud_boundaries_enforce_two_percent_error() {
    let low = ftdi_high_speed_baud(9_600).unwrap();
    assert!(low.actual.abs_diff(9_600) * 100 <= 9_600 * 2);
    let high = ftdi_high_speed_baud(12_000_000).unwrap();
    assert_eq!(high.actual, 12_000_000);
    assert_ne!(high.encoded_divisor & 0x2_0000, 0);
    assert!(ftdi_high_speed_baud(10_000_000).is_err());
    assert!(ftdi_high_speed_baud(9_599).is_err());
    assert!(ftdi_high_speed_baud(12_000_001).is_err());
}

#[test]
fn ftdi_status_is_stripped_from_full_and_short_packets() {
    let mut transfer = vec![0, 1 << 6];
    transfer.extend((0_u16..510).map(|value| value as u8));
    transfer.extend([0, (1 << 1) | (1 << 2), 0xaa, 0xbb, 0xcc]);
    let (payload, counters) = strip_ftdi_status(&transfer, 512).unwrap();
    assert_eq!(payload.len(), 513);
    assert_eq!(&payload[510..], &[0xaa, 0xbb, 0xcc]);
    assert_eq!(counters.overflow, 1);
    assert_eq!(counters.errors, 1);
    assert!(!counters.transmitter_empty);
    assert!(strip_ftdi_status(&[0], 512).is_err());
}

struct Loopback {
    metadata: TransportMetadata,
    bytes: Mutex<VecDeque<u8>>,
    writes: AtomicUsize,
    drains: AtomicUsize,
    cancelled: AtomicBool,
}

impl Transport for Loopback {
    fn metadata(&self) -> &TransportMetadata {
        &self.metadata
    }

    fn read(&self, buffer: &mut [u8]) -> TransferOutcome {
        let mut bytes = self.bytes.lock();
        let count = bytes.len().min(buffer.len());
        for target in &mut buffer[..count] {
            *target = bytes.pop_front().unwrap();
        }
        if count == 0 {
            TransferOutcome::failed(0, TransferFailureKind::TimedOut, "idle")
        } else {
            TransferOutcome::complete(count)
        }
    }

    fn write(&self, data: &[u8]) -> TransferOutcome {
        let count = data.len().min(3);
        self.bytes.lock().extend(&data[..count]);
        if self.writes.fetch_add(1, Ordering::Relaxed) == 0 {
            TransferOutcome::failed(count, TransferFailureKind::Interrupted, "partial")
        } else {
            TransferOutcome::complete(count)
        }
    }

    fn drain(&self, _timeout: Duration) -> Result<(), AppError> {
        self.drains.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn counters(&self) -> HardwareCounters {
        HardwareCounters::default()
    }
}

#[derive(Clone, Default)]
struct SharedOutput(Arc<Mutex<Vec<u8>>>);

impl Write for SharedOutput {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stream_accounts_partial_completion_then_drains_and_waits_for_idle() {
    let payload = b"partial USB completion survives retries".to_vec();
    let output = SharedOutput::default();
    let captured = Arc::clone(&output.0);
    let loopback = Arc::new(Loopback {
        metadata: TransportMetadata {
            backend: Backend::Usb,
            serial: "test".to_owned(),
            path: None,
            requested_baud: 115_200,
            actual_baud: 115_246,
            minimum_baud: 9_600,
            maximum_baud: 12_000_000,
        },
        bytes: Mutex::new(VecDeque::new()),
        writes: AtomicUsize::new(0),
        drains: AtomicUsize::new(0),
        cancelled: AtomicBool::new(false),
    });
    let transport: Arc<dyn Transport> = loopback.clone();
    let mut log = EventLog::open(None, Tool::C232Uart).unwrap();
    let report = run_session(
        transport,
        Cursor::new(payload.clone()),
        output,
        SessionOptions {
            mode: SessionMode::Stream,
            rx_idle_timeout: Duration::from_millis(20),
            drain_timeout: Duration::from_millis(200),
        },
        &mut log,
    )
    .unwrap();

    assert_eq!(*captured.lock(), payload);
    assert_eq!(report.statistics.tx_bytes_accepted, payload.len() as u64);
    assert_eq!(report.statistics.tx_bytes_completed, payload.len() as u64);
    assert_eq!(report.statistics.rx_bytes, payload.len() as u64);
    assert_eq!(report.reason, StopReason::RxIdle);
    assert_eq!(loopback.drains.load(Ordering::Relaxed), 1);
    assert!(loopback.cancelled.load(Ordering::Acquire));
}
