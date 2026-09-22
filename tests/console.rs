use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use glasgow_tool::Result;
use glasgow_tool::uart::{
    Counters, Metadata, Port, SessionOptions, StopReason, TransferFailureKind, TransferOutcome,
    Transport, run_session,
};
use parking_lot::Mutex;

struct LoopbackState {
    pending: Mutex<VecDeque<u8>>,
    transmitted: Mutex<Vec<u8>>,
    received: AtomicUsize,
    cancelled: AtomicBool,
}

struct BufferedLoopback {
    metadata: Metadata,
    state: Arc<LoopbackState>,
}

impl Transport for BufferedLoopback {
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn read(&self, _buffer: &mut [u8]) -> TransferOutcome {
        TransferOutcome::failed(0, TransferFailureKind::TimedOut, "idle")
    }

    fn write(&self, data: &[u8]) -> TransferOutcome {
        self.state.pending.lock().extend(data);
        TransferOutcome::complete(data.len())
    }

    fn submit_tx(&self) {
        let mut pending = self.state.pending.lock();
        let count = pending.len();
        self.state.transmitted.lock().extend(pending.drain(..));
        self.state.received.fetch_add(count, Ordering::Release);
    }

    fn drain(&self, _timeout: Duration) -> Result<()> {
        self.submit_tx();
        Ok(())
    }

    fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
    }

    fn counters(&self) -> Counters {
        Counters::default()
    }
}

struct KeypressInput {
    state: u8,
    transport: Arc<LoopbackState>,
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
fn console_echoes_and_transmits_ctrl_backslash_before_ctrl_bracket_exit() {
    let state = Arc::new(LoopbackState {
        pending: Mutex::new(VecDeque::new()),
        transmitted: Mutex::new(Vec::new()),
        received: AtomicUsize::new(0),
        cancelled: AtomicBool::new(false),
    });
    let port = Port::new(BufferedLoopback {
        metadata: Metadata {
            backend: "glasgow",
            serial: Some("test".to_owned()),
            path: None,
            requested_baud: 115_200,
            actual_baud: 115_107,
            minimum_baud: 9_600,
            maximum_baud: 12_000_000,
        },
        state: Arc::clone(&state),
    });
    let observed_before_exit = Arc::new(AtomicBool::new(false));
    let output = CapturedOutput::default();
    let input = KeypressInput {
        state: 0,
        transport: Arc::clone(&state),
        observed_before_exit: Arc::clone(&observed_before_exit),
        output: Arc::clone(&output.0),
    };
    let mut options = SessionOptions::console();
    options.drain_timeout = Duration::from_millis(200);

    let report = run_session(port, input, output.clone(), options).unwrap();

    assert!(observed_before_exit.load(Ordering::Acquire));
    assert_eq!(output.0.lock().as_slice(), [b'x', 0x1c]);
    assert_eq!(state.transmitted.lock().as_slice(), [b'x', 0x1c]);
    assert_eq!(report.statistics.tx_bytes_accepted, 2);
    assert_eq!(report.statistics.tx_bytes_completed, 2);
    assert_eq!(report.reason, StopReason::ConsoleEscape);
}
