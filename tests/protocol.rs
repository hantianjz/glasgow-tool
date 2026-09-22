use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use glasgow_tool::Result;
use glasgow_tool::uart::{
    Counters, Metadata, Port, SessionOptions, StopReason, TransferFailureKind, TransferOutcome,
    Transport, run_session,
};
use parking_lot::Mutex;

struct LoopbackState {
    bytes: Mutex<VecDeque<u8>>,
    pending: Mutex<VecDeque<u8>>,
    writes: AtomicUsize,
    submits: AtomicUsize,
    drains: AtomicUsize,
    cancelled: AtomicBool,
}
struct Loopback {
    metadata: Metadata,
    state: Arc<LoopbackState>,
}

impl Transport for Loopback {
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn read(&self, buffer: &mut [u8]) -> TransferOutcome {
        let mut bytes = self.state.bytes.lock();
        let count = bytes.len().min(buffer.len());
        for target in &mut buffer[..count] {
            *target = bytes.pop_front().expect("checked buffered length");
        }
        if count == 0 {
            TransferOutcome::failed(0, TransferFailureKind::TimedOut, "idle")
        } else {
            TransferOutcome::complete(count)
        }
    }

    fn write(&self, data: &[u8]) -> TransferOutcome {
        let count = data.len().min(3);
        self.state.pending.lock().extend(&data[..count]);
        if self.state.writes.fetch_add(1, Ordering::Relaxed) == 0 {
            TransferOutcome::failed(count, TransferFailureKind::Interrupted, "partial")
        } else {
            TransferOutcome::complete(count)
        }
    }
    fn submit_tx(&self) {
        let mut pending = self.state.pending.lock();
        self.state.bytes.lock().extend(pending.drain(..));
        self.state.submits.fetch_add(1, Ordering::Release);
    }

    fn drain(&self, _timeout: Duration) -> Result<()> {
        self.submit_tx();
        self.state.drains.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
    }

    fn counters(&self) -> Counters {
        Counters::default()
    }
}

struct StreamingInput {
    payload: Option<Vec<u8>>,
    state: Arc<LoopbackState>,
    submitted_before_eof: Arc<AtomicBool>,
}

impl Read for StreamingInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(payload) = self.payload.take() {
            assert!(payload.len() <= buffer.len());
            buffer[..payload.len()].copy_from_slice(&payload);
            return Ok(payload.len());
        }
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while self.state.submits.load(Ordering::Acquire) == 0
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.submitted_before_eof.store(
            self.state.submits.load(Ordering::Acquire) != 0,
            Ordering::Release,
        );
        Ok(0)
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
    let submitted_before_eof = Arc::new(AtomicBool::new(false));
    let state = Arc::new(LoopbackState {
        bytes: Mutex::new(VecDeque::new()),
        pending: Mutex::new(VecDeque::new()),
        writes: AtomicUsize::new(0),
        submits: AtomicUsize::new(0),
        drains: AtomicUsize::new(0),
        cancelled: AtomicBool::new(false),
    });
    let port = Port::new(Loopback {
        metadata: Metadata {
            backend: "usb",
            serial: Some("test".to_owned()),
            path: None,
            requested_baud: 115_200,
            actual_baud: 115_246,
            minimum_baud: 9_600,
            maximum_baud: 12_000_000,
        },
        state: Arc::clone(&state),
    });
    let mut options = SessionOptions::stream();
    options.rx_idle_timeout = Duration::from_millis(20);
    options.drain_timeout = Duration::from_millis(200);

    let input = StreamingInput {
        payload: Some(payload.clone()),
        state: Arc::clone(&state),
        submitted_before_eof: Arc::clone(&submitted_before_eof),
    };
    let report = run_session(port, input, output, options).unwrap();

    assert_eq!(*captured.lock(), payload);
    assert_eq!(report.statistics.tx_bytes_accepted, payload.len() as u64);
    assert_eq!(report.statistics.tx_bytes_completed, payload.len() as u64);
    assert_eq!(report.statistics.rx_bytes, payload.len() as u64);
    assert_eq!(report.reason, StopReason::RxIdle);
    assert_eq!(state.drains.load(Ordering::Relaxed), 1);
    assert!(state.cancelled.load(Ordering::Acquire));
    assert!(submitted_before_eof.load(Ordering::Acquire));
}
