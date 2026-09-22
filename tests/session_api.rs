use std::collections::VecDeque;
use std::io::{self, Cursor, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use glasgow_tool::Result;
use glasgow_tool::uart::{
    CancellationToken, Counters, Metadata, NdjsonObserver, Port, SessionEvent, SessionObserver,
    SessionOptions, SessionPhase, StopReason, TransferFailureKind, TransferOutcome, Transport,
    run_session_observed,
};
use parking_lot::Mutex;

struct Loopback {
    metadata: Metadata,
    wire: Arc<Mutex<Vec<u8>>>,
    receive: Mutex<VecDeque<u8>>,
    echo_rx: bool,
    cancelled: AtomicBool,
    counters: Counters,
}

impl Transport for Loopback {
    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn read(&self, buffer: &mut [u8]) -> TransferOutcome {
        if self.cancelled.load(Ordering::Acquire) {
            return TransferOutcome::failed(0, TransferFailureKind::TimedOut, "cancelled");
        }
        let mut receive = self.receive.lock();
        let completed = receive.len().min(buffer.len());
        for target in &mut buffer[..completed] {
            *target = receive.pop_front().expect("checked receive length");
        }
        if completed == 0 {
            TransferOutcome::failed(0, TransferFailureKind::TimedOut, "idle")
        } else {
            TransferOutcome::complete(completed)
        }
    }

    fn write(&self, data: &[u8]) -> TransferOutcome {
        self.wire.lock().extend_from_slice(data);
        if self.echo_rx {
            self.receive.lock().extend(data);
        }
        TransferOutcome::complete(data.len())
    }

    fn drain(&self, _timeout: Duration) -> Result<()> {
        Ok(())
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn counters(&self) -> Counters {
        self.counters
    }
}

fn loopback(
    backend: &'static str,
    echo_rx: bool,
    counters: Counters,
) -> (Port, Arc<Mutex<Vec<u8>>>) {
    let wire = Arc::new(Mutex::new(Vec::new()));
    let port = Port::new(Loopback {
        metadata: Metadata {
            backend,
            serial: Some("TEST123".to_owned()),
            path: Some("test-path".to_owned()),
            requested_baud: 115_200,
            actual_baud: 115_246,
            minimum_baud: 9_600,
            maximum_baud: 12_000_000,
        },
        wire: Arc::clone(&wire),
        receive: Mutex::new(VecDeque::new()),
        echo_rx,
        cancelled: AtomicBool::new(false),
        counters,
    });
    (port, wire)
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

#[derive(Default)]
struct PhaseObserver(Vec<SessionPhase>);

impl SessionObserver for PhaseObserver {
    fn observe(&mut self, event: &SessionEvent<'_>) -> Result<()> {
        self.0.push(event.phase);
        Ok(())
    }
}

#[test]
fn stream_then_console_use_independent_cancellation_tokens() {
    let stream_payload = b"stream payload".to_vec();
    let (stream_port, stream_wire) = loopback("usb", true, Counters::default());
    let stream_output = CapturedOutput::default();
    let stream_capture = Arc::clone(&stream_output.0);
    let stream_token = CancellationToken::new();
    let mut stream_options = SessionOptions::stream();
    stream_options.rx_idle_timeout = Duration::from_millis(20);
    stream_options.drain_timeout = Duration::from_millis(200);
    stream_options.cancellation = stream_token;
    let mut stream_observer = PhaseObserver::default();

    let stream_report = run_session_observed(
        stream_port,
        Cursor::new(stream_payload.clone()),
        stream_output,
        stream_options,
        &mut stream_observer,
    )
    .unwrap();

    assert_eq!(*stream_wire.lock(), stream_payload);
    assert_eq!(*stream_capture.lock(), stream_payload);
    assert_eq!(
        stream_report.statistics.tx_bytes_accepted,
        stream_payload.len() as u64
    );
    assert_eq!(
        stream_report.statistics.tx_bytes_completed,
        stream_payload.len() as u64
    );
    assert_eq!(
        stream_report.statistics.rx_bytes,
        stream_payload.len() as u64
    );
    assert_eq!(stream_report.reason, StopReason::RxIdle);
    assert_eq!(
        stream_observer.0,
        [
            SessionPhase::Running,
            SessionPhase::Draining,
            SessionPhase::Final
        ]
    );

    let console_payload = vec![b'K', 0x1c];
    let mut console_input = console_payload.clone();
    console_input.push(0x1d);
    let (console_port, console_wire) = loopback("glasgow", false, Counters::default());
    let console_output = CapturedOutput::default();
    let console_capture = Arc::clone(&console_output.0);
    let console_token = CancellationToken::new();
    let mut console_options = SessionOptions::console();
    console_options.drain_timeout = Duration::from_millis(200);
    console_options.cancellation = console_token;
    let mut console_observer = PhaseObserver::default();

    let console_report = run_session_observed(
        console_port,
        Cursor::new(console_input),
        console_output,
        console_options,
        &mut console_observer,
    )
    .unwrap();

    assert_eq!(*console_wire.lock(), console_payload);
    assert_eq!(*console_capture.lock(), console_payload);
    assert_eq!(console_report.statistics.tx_bytes_accepted, 2);
    assert_eq!(console_report.statistics.tx_bytes_completed, 2);
    assert_eq!(console_report.statistics.rx_bytes, 0);
    assert_eq!(console_report.reason, StopReason::ConsoleEscape);
    assert_eq!(
        console_observer.0,
        [
            SessionPhase::Running,
            SessionPhase::Draining,
            SessionPhase::Final
        ]
    );
}

#[test]
fn ndjson_observer_preserves_schema_and_final_status() {
    let payload = b"telemetry".to_vec();
    let counters = Counters {
        rx_errors: 2,
        rx_overflow: 3,
    };
    let (port, _) = loopback("usb", true, counters);
    let mut options = SessionOptions::stream();
    options.rx_idle_timeout = Duration::from_millis(20);
    options.drain_timeout = Duration::from_millis(200);
    let mut observer = NdjsonObserver::new(Vec::new(), "consumer-test");

    let report = run_session_observed(
        port,
        Cursor::new(payload.clone()),
        io::sink(),
        options,
        &mut observer,
    )
    .unwrap();
    let records = String::from_utf8(observer.into_inner())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["phase"], "running");
    assert_eq!(records[1]["phase"], "draining");
    assert_eq!(records[2]["phase"], "final");
    for record in &records {
        assert_eq!(record["schema_version"], 1);
        assert_eq!(record["tool"], "consumer-test");
        assert_eq!(record["backend"], "usb");
        assert_eq!(record["selected_serial"], "TEST123");
        assert_eq!(record["selected_path"], "test-path");
        assert_eq!(record["requested_baud"], 115_200);
        assert_eq!(record["actual_baud"], 115_246);
        assert_eq!(record["hardware"]["rx_errors"], 2);
        assert_eq!(record["hardware"]["rx_overflow"], 3);
    }
    assert_eq!(records[1]["reason"], "rx_idle");
    assert_eq!(records[2]["reason"], "rx_idle");
    assert_eq!(records[2]["exit_code"], 0);
    assert_eq!(records[2]["tx_bytes_accepted"], payload.len());
    assert_eq!(records[2]["tx_bytes_completed"], payload.len());
    assert_eq!(records[2]["rx_bytes"], payload.len());
    assert_eq!(report.hardware, counters);
}
