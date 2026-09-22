use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use glasgow_tool::Tool;
use glasgow_tool::cli::AppError;
use glasgow_tool::events::{Backend, EventLog, HardwareCounters};
use glasgow_tool::session::{
    SessionMode, SessionOptions, StopReason, TransferFailureKind, TransferOutcome, Transport,
    TransportMetadata, run_session,
};
use parking_lot::Mutex;

struct BufferedLoopback {
    metadata: TransportMetadata,
    pending: Mutex<VecDeque<u8>>,
    wire: Mutex<VecDeque<u8>>,
    received: AtomicUsize,
    cancelled: AtomicBool,
}

impl Transport for BufferedLoopback {
    fn metadata(&self) -> &TransportMetadata {
        &self.metadata
    }

    fn read(&self, buffer: &mut [u8]) -> TransferOutcome {
        if self.cancelled.load(Ordering::Acquire) {
            return TransferOutcome::failed(0, TransferFailureKind::TimedOut, "cancelled");
        }
        let mut wire = self.wire.lock();
        let count = wire.len().min(buffer.len());
        for target in &mut buffer[..count] {
            *target = wire.pop_front().unwrap();
        }
        if count != 0 {
            self.received.fetch_add(count, Ordering::Release);
        }
        TransferOutcome::failed(0, TransferFailureKind::TimedOut, "idle")
    }

    fn write(&self, data: &[u8]) -> TransferOutcome {
        self.pending.lock().extend(data);
        TransferOutcome::complete(data.len())
    }

    fn submit_tx(&self) {
        let mut pending = self.pending.lock();
        self.wire.lock().extend(pending.drain(..));
    }

    fn drain(&self, _timeout: Duration) -> Result<(), AppError> {
        self.submit_tx();
        Ok(())
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn counters(&self) -> HardwareCounters {
        HardwareCounters::default()
    }
}

struct KeypressInput {
    state: u8,
    transport: Arc<BufferedLoopback>,
    observed_before_exit: Arc<AtomicBool>,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Read for KeypressInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.state {
            0 => {
                self.state = 1;
                buffer[..2].copy_from_slice(&[b'x', 0x1c]);
                Ok(2)
            }
            1 => {
                let deadline = Instant::now() + Duration::from_millis(200);
                while (self.transport.received.load(Ordering::Acquire) < 2
                    || self.output.lock().as_slice() != [b'x', 0x1c])
                    && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                self.observed_before_exit.store(
                    self.transport.received.load(Ordering::Acquire) == 2
                        && self.output.lock().as_slice() == [b'x', 0x1c],
                    Ordering::Release,
                );
                self.state = 2;
                buffer[0] = 0x1d;
                Ok(1)
            }
            _ => Ok(0),
        }
    }
}

#[derive(Clone, Default)]
struct CapturedOutput(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedOutput {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn console_transmits_keypress_before_exit() {
    let transport = Arc::new(BufferedLoopback {
        metadata: TransportMetadata {
            backend: Backend::Glasgow,
            serial: "test".to_owned(),
            path: None,
            requested_baud: 115_200,
            actual_baud: 115_107,
            minimum_baud: 9_600,
            maximum_baud: 12_000_000,
        },
        pending: Mutex::new(VecDeque::new()),
        wire: Mutex::new(VecDeque::new()),
        received: AtomicUsize::new(0),
        cancelled: AtomicBool::new(false),
    });
    let observed_before_exit = Arc::new(AtomicBool::new(false));
    let output = CapturedOutput::default();
    let input = KeypressInput {
        state: 0,
        transport: Arc::clone(&transport),
        observed_before_exit: Arc::clone(&observed_before_exit),
        output: Arc::clone(&output.0),
    };
    let transport: Arc<dyn Transport> = transport;
    let mut log = EventLog::open(None, Tool::Guart).unwrap();

    let report = run_session(
        transport,
        input,
        output.clone(),
        SessionOptions {
            mode: SessionMode::Console,
            rx_idle_timeout: Duration::from_secs(2),
            drain_timeout: Duration::from_millis(200),
        },
        &mut log,
    )
    .unwrap();

    assert!(observed_before_exit.load(Ordering::Acquire));
    assert_eq!(output.0.lock().as_slice(), [b'x', 0x1c]);
    assert_eq!(report.statistics.tx_bytes_accepted, 2);
    assert_eq!(report.reason, StopReason::ConsoleEscape);
}
