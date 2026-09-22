//! Bounded blocking full-duplex session coordinator.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use parking_lot::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::{AppError, ExitCode};
use crate::events::{Backend, EventLog, EventRecord, HardwareCounters, Phase};

/// Maximum application-owned buffer in either transfer direction.
pub const APPLICATION_QUEUE_BYTES: usize = 64 * 1024;
const WOULD_BLOCK_BACKOFF: Duration = Duration::from_millis(1);
const COORDINATOR_TICK: Duration = Duration::from_millis(20);
const CONSOLE_ESCAPE: u8 = 0x1d;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMode {
    Console,
    Stream,
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub mode: SessionMode,
    pub rx_idle_timeout: Duration,
    pub drain_timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct TransportMetadata {
    pub backend: Backend,
    pub serial: String,
    pub path: Option<String>,
    pub requested_baud: u32,
    pub actual_baud: u32,
    pub minimum_baud: u32,
    pub maximum_baud: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferFailureKind {
    Interrupted,
    WouldBlock,
    TimedOut,
    Access,
    Protocol,
    Disconnected,
}

#[derive(Clone, Debug)]
pub struct TransferFailure {
    pub kind: TransferFailureKind,
    pub message: String,
}

impl fmt::Display for TransferFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// One transfer completion. `completed` is accounted before `failure` is handled.
#[derive(Clone, Debug)]
pub struct TransferOutcome {
    pub completed: usize,
    pub failure: Option<TransferFailure>,
}

impl TransferOutcome {
    #[must_use]
    pub const fn complete(completed: usize) -> Self {
        Self {
            completed,
            failure: None,
        }
    }

    #[must_use]
    pub fn failed(completed: usize, kind: TransferFailureKind, message: impl Into<String>) -> Self {
        Self {
            completed,
            failure: Some(TransferFailure {
                kind,
                message: message.into(),
            }),
        }
    }
}

/// Synchronous device transport. Implementations must make `cancel` unblock I/O.
pub trait Transport: Send + Sync {
    fn metadata(&self) -> &TransportMetadata;
    fn read(&self, buffer: &mut [u8]) -> TransferOutcome;
    fn write(&self, data: &[u8]) -> TransferOutcome;
    /// Submit transport-buffered TX data without waiting for physical completion.
    fn submit_tx(&self) {}
    fn drain(&self, timeout: Duration) -> Result<(), AppError>;
    fn cancel(&self);
    fn counters(&self) -> HardwareCounters;
}

#[derive(Default)]
struct AtomicStatistics {
    tx_accepted: AtomicU64,
    tx_completed: AtomicU64,
    rx: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionStatistics {
    pub tx_bytes_accepted: u64,
    pub tx_bytes_completed: u64,
    pub rx_bytes: u64,
}

impl AtomicStatistics {
    fn snapshot(&self) -> SessionStatistics {
        SessionStatistics {
            tx_bytes_accepted: self.tx_accepted.load(Ordering::Relaxed),
            tx_bytes_completed: self.tx_completed.load(Ordering::Relaxed),
            rx_bytes: self.rx.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    RxIdle,
    ConsoleEscape,
    Cancelled,
}

impl StopReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RxIdle => "rx_idle",
            Self::ConsoleEscape => "console_escape",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionReport {
    pub statistics: SessionStatistics,
    pub hardware: HardwareCounters,
    pub reason: StopReason,
    pub exit_code: ExitCode,
}

enum WorkerEvent {
    TxDrained(StopReason),
    RxActivity,
    WorkerError(AppError),
    TxStopped,
    RxStopped,
}

fn map_transfer_failure(failure: TransferFailure) -> Option<AppError> {
    match failure.kind {
        TransferFailureKind::Interrupted
        | TransferFailureKind::WouldBlock
        | TransferFailureKind::TimedOut => None,
        TransferFailureKind::Access | TransferFailureKind::Disconnected => {
            Some(AppError::Access(failure.message))
        }
        TransferFailureKind::Protocol => Some(AppError::Protocol(failure.message)),
    }
}

fn write_transport(
    transport: &dyn Transport,
    mut data: &[u8],
    statistics: &AtomicStatistics,
    cancelled: &AtomicBool,
) -> Result<(), AppError> {
    while !data.is_empty() && !cancelled.load(Ordering::Acquire) {
        let outcome = transport.write(data);
        if outcome.completed > data.len() {
            return Err(AppError::Protocol(
                "transport reported more TX bytes than submitted".to_owned(),
            ));
        }
        if outcome.completed != 0 {
            statistics
                .tx_accepted
                .fetch_add(outcome.completed as u64, Ordering::Relaxed);
            data = &data[outcome.completed..];
        }
        if let Some(failure) = outcome.failure {
            if let Some(error) = map_transfer_failure(failure) {
                return Err(error);
            }
            if outcome.completed == 0 {
                thread::sleep(WOULD_BLOCK_BACKOFF);
            }
        } else if outcome.completed == 0 {
            return Err(AppError::Protocol(
                "transport completed a zero-length TX without status".to_owned(),
            ));
        }
    }
    Ok(())
}
struct SharedOutput<W>(Arc<Mutex<W>>);

impl<W> SharedOutput<W> {
    fn new(output: W) -> Self {
        Self(Arc::new(Mutex::new(output)))
    }
}

impl<W> Clone for SharedOutput<W> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<W: Write> SharedOutput<W> {
    fn write_all_and_flush(&self, data: &[u8]) -> io::Result<()> {
        let mut output = self.0.lock();
        output.write_all(data)?;
        output.flush()
    }
}

fn tx_worker<R: Read, W: Write>(
    transport: Arc<dyn Transport>,
    mut input: R,
    output: SharedOutput<W>,
    options: SessionOptions,
    statistics: Arc<AtomicStatistics>,
    cancelled: Arc<AtomicBool>,
    events: mpsc::Sender<WorkerEvent>,
) {
    let mut buffer = vec![0_u8; APPLICATION_QUEUE_BYTES];
    let result = (|| -> Result<StopReason, AppError> {
        let reason = loop {
            if cancelled.load(Ordering::Acquire) {
                return Ok(StopReason::Cancelled);
            }
            match input.read(&mut buffer) {
                Ok(0) => break StopReason::RxIdle,
                Ok(length) => {
                    let escape_index = (options.mode == SessionMode::Console)
                        .then(|| {
                            buffer[..length]
                                .iter()
                                .position(|byte| *byte == CONSOLE_ESCAPE)
                        })
                        .flatten();
                    let transmitted = escape_index.unwrap_or(length);
                    if options.mode == SessionMode::Console && transmitted != 0 {
                        output
                            .write_all_and_flush(&buffer[..transmitted])
                            .map_err(|error| {
                                AppError::Access(format!("local echo write failed: {error}"))
                            })?;
                    }
                    write_transport(
                        transport.as_ref(),
                        &buffer[..transmitted],
                        &statistics,
                        &cancelled,
                    )?;
                    if options.mode == SessionMode::Console {
                        transport.submit_tx();
                    }
                    if escape_index.is_some() {
                        break StopReason::ConsoleEscape;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(AppError::Access(format!("stdin read failed: {error}")));
                }
            }
        };

        if reason != StopReason::Cancelled {
            transport.drain(options.drain_timeout)?;
            let accepted = statistics.tx_accepted.load(Ordering::Relaxed);
            statistics.tx_completed.store(accepted, Ordering::Relaxed);
        }
        Ok(reason)
    })();

    match result {
        Ok(reason) => {
            let _ = events.send(WorkerEvent::TxDrained(reason));
        }
        Err(error) => {
            let _ = events.send(WorkerEvent::WorkerError(error));
        }
    }
    let _ = events.send(WorkerEvent::TxStopped);
}

fn rx_worker<W: Write>(
    transport: Arc<dyn Transport>,
    output: SharedOutput<W>,
    statistics: Arc<AtomicStatistics>,
    cancelled: Arc<AtomicBool>,
    events: mpsc::Sender<WorkerEvent>,
    last_rx: Arc<Mutex<Instant>>,
) {
    let mut buffer = vec![0_u8; APPLICATION_QUEUE_BYTES];
    let result = (|| -> Result<(), AppError> {
        while !cancelled.load(Ordering::Acquire) {
            let outcome = transport.read(&mut buffer);
            if outcome.completed > buffer.len() {
                return Err(AppError::Protocol(
                    "transport reported more RX bytes than buffer capacity".to_owned(),
                ));
            }
            if outcome.completed != 0 {
                output
                    .write_all_and_flush(&buffer[..outcome.completed])
                    .map_err(|error| AppError::Access(format!("stdout write failed: {error}")))?;
                statistics
                    .rx
                    .fetch_add(outcome.completed as u64, Ordering::Relaxed);
                *last_rx.lock() = Instant::now();
                let _ = events.send(WorkerEvent::RxActivity);
            }
            if let Some(failure) = outcome.failure {
                if let Some(error) = map_transfer_failure(failure) {
                    return Err(error);
                }
                if outcome.completed == 0 {
                    thread::sleep(WOULD_BLOCK_BACKOFF);
                }
            } else if outcome.completed == 0 {
                return Err(AppError::Protocol(
                    "transport completed a zero-length RX without status".to_owned(),
                ));
            }
        }
        Ok(())
    })();

    if let Err(error) = result {
        let _ = events.send(WorkerEvent::WorkerError(error));
    }
    let _ = events.send(WorkerEvent::RxStopped);
}

fn write_event(
    log: &mut EventLog,
    transport: &dyn Transport,
    phase: Phase,
    statistics: SessionStatistics,
    reason: Option<&str>,
    exit_code: Option<ExitCode>,
) -> Result<(), AppError> {
    let metadata = transport.metadata();
    log.write(EventRecord {
        schema_version: 1,
        monotonic_ns: log.timestamp(),
        tool: "",
        backend: metadata.backend,
        selected_serial: Some(&metadata.serial),
        selected_path: metadata.path.as_deref(),
        requested_baud: Some(metadata.requested_baud),
        actual_baud: Some(metadata.actual_baud),
        phase,
        tx_bytes_accepted: statistics.tx_bytes_accepted,
        tx_bytes_completed: statistics.tx_bytes_completed,
        rx_bytes: statistics.rx_bytes,
        hardware: transport.counters(),
        reason,
        exit_code: exit_code.map(ExitCode::as_i32),
    })
    .map_err(|error| AppError::Access(format!("event log write failed: {error}")))
}

/// Run the actual stdin/stdout session and return only after both workers are joined.
pub fn run_session<R, W>(
    transport: Arc<dyn Transport>,
    input: R,
    output: W,
    options: SessionOptions,
    event_log: &mut EventLog,
) -> Result<SessionReport, AppError>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal_cancelled = Arc::clone(&cancelled);
    ctrlc::set_handler(move || signal_cancelled.store(true, Ordering::Release))
        .map_err(|error| AppError::Protocol(format!("cannot install signal handler: {error}")))?;

    let statistics = Arc::new(AtomicStatistics::default());
    let last_rx = Arc::new(Mutex::new(Instant::now()));
    let (events_tx, events_rx) = mpsc::channel();
    let output = SharedOutput::new(output);

    write_event(
        event_log,
        transport.as_ref(),
        Phase::Running,
        statistics.snapshot(),
        None,
        None,
    )?;

    let tx_handle = {
        let transport = Arc::clone(&transport);
        let statistics = Arc::clone(&statistics);
        let cancelled = Arc::clone(&cancelled);
        let events = events_tx.clone();
        let worker_options = options.clone();
        let output = output.clone();
        thread::Builder::new()
            .name("uart-tx".to_owned())
            .spawn(move || {
                tx_worker(
                    transport,
                    input,
                    output,
                    worker_options,
                    statistics,
                    cancelled,
                    events,
                );
            })
            .map_err(|error| AppError::Protocol(format!("cannot start TX worker: {error}")))?
    };
    let rx_handle = {
        let transport = Arc::clone(&transport);
        let statistics = Arc::clone(&statistics);
        let cancelled = Arc::clone(&cancelled);
        let last_rx = Arc::clone(&last_rx);
        thread::Builder::new()
            .name("uart-rx".to_owned())
            .spawn(move || {
                rx_worker(transport, output, statistics, cancelled, events_tx, last_rx);
            })
            .map_err(|error| AppError::Protocol(format!("cannot start RX worker: {error}")))?
    };

    let mut tx_stopped = false;
    let mut rx_stopped = false;
    let mut drained_at = None;
    let mut stop_reason = None;
    let mut failure = None;
    let mut join_deadline = None;

    while !tx_stopped || !rx_stopped {
        if cancelled.load(Ordering::Acquire) && stop_reason.is_none() && failure.is_none() {
            stop_reason = Some(StopReason::Cancelled);
            failure = Some(AppError::Timeout("session cancelled by signal".to_owned()));
            transport.cancel();
            join_deadline = Some(Instant::now() + options.drain_timeout);
        }
        if let Some(started) = drained_at {
            let quiet_since = (*last_rx.lock()).max(started);
            if Instant::now().saturating_duration_since(quiet_since) >= options.rx_idle_timeout {
                stop_reason = Some(StopReason::RxIdle);
                cancelled.store(true, Ordering::Release);
                transport.cancel();
                join_deadline = Some(Instant::now() + options.drain_timeout);
            }
        }
        if join_deadline.is_some_and(|deadline| Instant::now() >= deadline) && failure.is_none() {
            failure = Some(AppError::Timeout(
                "workers did not stop within the configured drain timeout".to_owned(),
            ));
            transport.cancel();
        }

        match events_rx.recv_timeout(COORDINATOR_TICK) {
            Ok(WorkerEvent::TxDrained(reason)) => {
                write_event(
                    event_log,
                    transport.as_ref(),
                    Phase::Draining,
                    statistics.snapshot(),
                    Some(reason.as_str()),
                    None,
                )?;
                if options.mode == SessionMode::Console || reason == StopReason::Cancelled {
                    stop_reason = Some(reason);
                    cancelled.store(true, Ordering::Release);
                    transport.cancel();
                    join_deadline = Some(Instant::now() + options.drain_timeout);
                } else {
                    drained_at = Some(Instant::now());
                }
            }
            Ok(WorkerEvent::RxActivity) => {}
            Ok(WorkerEvent::WorkerError(error)) => {
                if failure.is_none() {
                    failure = Some(error);
                }
                cancelled.store(true, Ordering::Release);
                transport.cancel();
                join_deadline = Some(Instant::now() + options.drain_timeout);
            }
            Ok(WorkerEvent::TxStopped) => tx_stopped = true,
            Ok(WorkerEvent::RxStopped) => rx_stopped = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    tx_handle
        .join()
        .map_err(|_| AppError::Protocol("TX worker panicked".to_owned()))?;
    rx_handle
        .join()
        .map_err(|_| AppError::Protocol("RX worker panicked".to_owned()))?;

    let final_statistics = statistics.snapshot();
    if let Some(error) = failure {
        write_event(
            event_log,
            transport.as_ref(),
            Phase::Final,
            final_statistics,
            stop_reason.map(StopReason::as_str),
            Some(error.exit_code()),
        )?;
        return Err(error);
    }

    let reason = stop_reason.unwrap_or(StopReason::RxIdle);
    write_event(
        event_log,
        transport.as_ref(),
        Phase::Final,
        final_statistics,
        Some(reason.as_str()),
        Some(ExitCode::Success),
    )?;
    Ok(SessionReport {
        statistics: final_statistics,
        hardware: transport.counters(),
        reason,
        exit_code: ExitCode::Success,
    })
}
