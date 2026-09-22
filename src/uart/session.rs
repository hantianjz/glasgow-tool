use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::{Error, Result};

use super::telemetry::{NoopObserver, SessionEvent, SessionObserver, SessionPhase};
use super::transport::{Counters, Port, TransferFailure, TransferFailureKind, Transport};

const APPLICATION_QUEUE_BYTES: usize = 64 * 1024;
const WOULD_BLOCK_BACKOFF: Duration = Duration::from_millis(1);
const COORDINATOR_TICK: Duration = Duration::from_millis(20);

/// Console escape byte (Ctrl-]).
pub const CONSOLE_ESCAPE: u8 = 0x1d;

/// Session framing and stop behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMode {
    /// Echo local input and consume Ctrl-] as the exit command.
    Console,
    /// Forward input byte-for-byte, then wait for RX idle after TX drain.
    Stream,
}

/// Process-neutral cancellation signal shared with a session coordinator.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Create an uncancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Report whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}

/// Full-duplex session configuration.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// Console or byte-transparent stream behavior.
    pub mode: SessionMode,
    /// Quiet interval after stream TX drain.
    pub rx_idle_timeout: Duration,
    /// Maximum bounded shutdown and TX-drain interval.
    pub drain_timeout: Duration,
    /// Caller-owned cancellation signal.
    pub cancellation: CancellationToken,
}

impl SessionOptions {
    /// Console defaults: 2-second RX idle and 5-second drain bounds.
    #[must_use]
    pub fn console() -> Self {
        Self {
            mode: SessionMode::Console,
            rx_idle_timeout: Duration::from_secs(2),
            drain_timeout: Duration::from_secs(5),
            cancellation: CancellationToken::new(),
        }
    }

    /// Stream defaults: 2-second RX idle and 5-second drain bounds.
    #[must_use]
    pub fn stream() -> Self {
        Self {
            mode: SessionMode::Stream,
            rx_idle_timeout: Duration::from_secs(2),
            drain_timeout: Duration::from_secs(5),
            cancellation: CancellationToken::new(),
        }
    }
}

#[derive(Default)]
struct AtomicStatistics {
    tx_accepted: AtomicU64,
    tx_completed: AtomicU64,
    rx: AtomicU64,
}

/// Byte counts accumulated by one session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionStatistics {
    /// Bytes accepted by the transport for transmission.
    pub tx_bytes_accepted: u64,
    /// Accepted bytes confirmed complete by a device-observable drain.
    pub tx_bytes_completed: u64,
    /// Bytes written to the session output.
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

/// Successful session stop condition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// Stream RX remained quiet after TX completed.
    RxIdle,
    /// Console input contained Ctrl-].
    ConsoleEscape,
    /// The caller requested cancellation.
    Cancelled,
}

impl StopReason {
    /// Stable telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RxIdle => "rx_idle",
            Self::ConsoleEscape => "console_escape",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Final successful session state.
#[derive(Clone, Debug)]
pub struct SessionReport {
    /// Final byte counters.
    pub statistics: SessionStatistics,
    /// Final hardware receive counters.
    pub hardware: Counters,
    /// Successful stop condition.
    pub reason: StopReason,
}

enum WorkerEvent {
    TxDrained(StopReason),
    RxActivity,
    WorkerError(Error),
    TxStopped,
    RxStopped,
}

fn map_transfer_failure(failure: TransferFailure) -> Option<Error> {
    match failure.kind {
        TransferFailureKind::Interrupted
        | TransferFailureKind::WouldBlock
        | TransferFailureKind::TimedOut => None,
        TransferFailureKind::Access | TransferFailureKind::Disconnected => {
            Some(Error::Access(failure.message))
        }
        TransferFailureKind::Protocol => Some(Error::Protocol(failure.message)),
    }
}

fn write_transport(
    transport: &dyn Transport,
    mut data: &[u8],
    statistics: &AtomicStatistics,
    cancelled: &AtomicBool,
) -> Result<()> {
    while !data.is_empty() && !cancelled.load(Ordering::Acquire) {
        let outcome = transport.write(data);
        if outcome.completed > data.len() {
            return Err(Error::Protocol(
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
            return Err(Error::Protocol(
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

struct WorkerContext<W> {
    transport: Arc<dyn Transport>,
    output: SharedOutput<W>,
    statistics: Arc<AtomicStatistics>,
    cancelled: Arc<AtomicBool>,
    events: mpsc::Sender<WorkerEvent>,
}

impl<W> Clone for WorkerContext<W> {
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
            output: self.output.clone(),
            statistics: Arc::clone(&self.statistics),
            cancelled: Arc::clone(&self.cancelled),
            events: self.events.clone(),
        }
    }
}

struct RxWorkerContext<W> {
    common: WorkerContext<W>,
    last_rx: Arc<Mutex<Instant>>,
}

fn tx_worker<R: Read, W: Write>(
    context: WorkerContext<W>,
    mut input: R,
    mode: SessionMode,
    drain_timeout: Duration,
) {
    let WorkerContext {
        transport,
        output,
        statistics,
        cancelled,
        events,
    } = context;
    let mut buffer = vec![0_u8; APPLICATION_QUEUE_BYTES];
    let result = (|| -> Result<StopReason> {
        let reason =
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Ok(StopReason::Cancelled);
                }
                match input.read(&mut buffer) {
                    Ok(0) => break StopReason::RxIdle,
                    Ok(length) => {
                        let escape_index = (mode == SessionMode::Console)
                            .then(|| {
                                buffer[..length]
                                    .iter()
                                    .position(|byte| *byte == CONSOLE_ESCAPE)
                            })
                            .flatten();
                        let transmitted = escape_index.unwrap_or(length);
                        if mode == SessionMode::Console && transmitted != 0 {
                            output.write_all_and_flush(&buffer[..transmitted]).map_err(
                                |error| Error::Access(format!("local echo write failed: {error}")),
                            )?;
                        }
                        write_transport(
                            transport.as_ref(),
                            &buffer[..transmitted],
                            &statistics,
                            &cancelled,
                        )?;
                        transport.submit_tx();
                        if escape_index.is_some() {
                            break StopReason::ConsoleEscape;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        return Err(Error::Access(format!("stdin read failed: {error}")));
                    }
                }
            };

        if reason != StopReason::Cancelled {
            transport.drain(drain_timeout)?;
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

fn rx_worker<W: Write>(context: RxWorkerContext<W>) {
    let RxWorkerContext { common, last_rx } = context;
    let WorkerContext {
        transport,
        output,
        statistics,
        cancelled,
        events,
    } = common;
    let mut buffer = vec![0_u8; APPLICATION_QUEUE_BYTES];
    let result = (|| -> Result<()> {
        while !cancelled.load(Ordering::Acquire) {
            let outcome = transport.read(&mut buffer);
            if outcome.completed > buffer.len() {
                return Err(Error::Protocol(
                    "transport reported more RX bytes than buffer capacity".to_owned(),
                ));
            }
            if outcome.completed != 0 {
                output
                    .write_all_and_flush(&buffer[..outcome.completed])
                    .map_err(|error| Error::Access(format!("stdout write failed: {error}")))?;
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
                return Err(Error::Protocol(
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

struct WorkerHandles {
    tx: thread::JoinHandle<()>,
    rx: thread::JoinHandle<()>,
}

impl WorkerHandles {
    fn join(self, failure: &mut Option<Error>) {
        if self.tx.join().is_err() && failure.is_none() {
            *failure = Some(Error::Protocol("TX worker panicked".to_owned()));
        }
        if self.rx.join().is_err() && failure.is_none() {
            *failure = Some(Error::Protocol("RX worker panicked".to_owned()));
        }
    }
}

fn spawn_workers<R, W>(
    input: R,
    tx_context: WorkerContext<W>,
    rx_context: RxWorkerContext<W>,
    mode: SessionMode,
    drain_timeout: Duration,
) -> Result<WorkerHandles>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let transport = Arc::clone(&tx_context.transport);
    let cancelled = Arc::clone(&tx_context.cancelled);
    let rx = thread::Builder::new()
        .name("uart-rx".to_owned())
        .spawn(move || rx_worker(rx_context))
        .map_err(|error| Error::Protocol(format!("cannot start RX worker: {error}")))?;
    let tx = match thread::Builder::new()
        .name("uart-tx".to_owned())
        .spawn(move || tx_worker(tx_context, input, mode, drain_timeout))
    {
        Ok(handle) => handle,
        Err(error) => {
            cancelled.store(true, Ordering::Release);
            transport.cancel();
            let _ = rx.join();
            return Err(Error::Protocol(format!("cannot start TX worker: {error}")));
        }
    };
    Ok(WorkerHandles { tx, rx })
}

fn observe<O: SessionObserver + ?Sized>(
    observer: &mut O,
    transport: &dyn Transport,
    phase: SessionPhase,
    statistics: SessionStatistics,
    stop_reason: Option<StopReason>,
    error: Option<&Error>,
) -> Result<()> {
    observer.observe(&SessionEvent {
        metadata: transport.metadata(),
        phase,
        statistics,
        counters: transport.counters(),
        stop_reason,
        error,
    })
}

struct Coordination {
    stop_reason: Option<StopReason>,
    failure: Option<Error>,
    observer_failed: bool,
}

struct Coordinator<'a, O: ?Sized> {
    observer: &'a mut O,
    transport: &'a dyn Transport,
    statistics: &'a AtomicStatistics,
    last_rx: &'a Mutex<Instant>,
    cancelled: &'a AtomicBool,
    mode: SessionMode,
    rx_idle_timeout: Duration,
    drain_timeout: Duration,
}

impl<O: SessionObserver + ?Sized> Coordinator<'_, O> {
    fn run(&mut self, events: &mpsc::Receiver<WorkerEvent>) -> Coordination {
        let mut tx_stopped = false;
        let mut rx_stopped = false;
        let mut drained_at = None;
        let mut stop_reason = None;
        let mut failure = None;
        let mut join_deadline = None;
        let mut observer_failed = false;

        while !tx_stopped || !rx_stopped {
            if self.cancelled.load(Ordering::Acquire) && stop_reason.is_none() && failure.is_none()
            {
                stop_reason = Some(StopReason::Cancelled);
                failure = Some(Error::Timeout("session cancelled by signal".to_owned()));
                self.transport.cancel();
                join_deadline = Some(Instant::now() + self.drain_timeout);
            }
            if let Some(started) = drained_at {
                let quiet_since = (*self.last_rx.lock()).max(started);
                if Instant::now().saturating_duration_since(quiet_since) >= self.rx_idle_timeout {
                    stop_reason = Some(StopReason::RxIdle);
                    self.cancelled.store(true, Ordering::Release);
                    self.transport.cancel();
                    join_deadline = Some(Instant::now() + self.drain_timeout);
                }
            }
            if join_deadline.is_some_and(|deadline| Instant::now() >= deadline) && failure.is_none()
            {
                failure = Some(Error::Timeout(
                    "workers did not stop within the configured drain timeout".to_owned(),
                ));
                self.transport.cancel();
            }

            match events.recv_timeout(COORDINATOR_TICK) {
                Ok(WorkerEvent::TxDrained(reason)) => {
                    let observation = (!observer_failed).then(|| {
                        observe(
                            self.observer,
                            self.transport,
                            SessionPhase::Draining,
                            self.statistics.snapshot(),
                            Some(reason),
                            None,
                        )
                    });
                    if let Some(Err(error)) = observation {
                        failure = Some(error);
                        observer_failed = true;
                        self.cancelled.store(true, Ordering::Release);
                        self.transport.cancel();
                        join_deadline = Some(Instant::now() + self.drain_timeout);
                    } else if self.mode == SessionMode::Console || reason == StopReason::Cancelled {
                        stop_reason = Some(reason);
                        self.cancelled.store(true, Ordering::Release);
                        self.transport.cancel();
                        join_deadline = Some(Instant::now() + self.drain_timeout);
                    } else {
                        drained_at = Some(Instant::now());
                    }
                }
                Ok(WorkerEvent::WorkerError(error)) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                    self.cancelled.store(true, Ordering::Release);
                    self.transport.cancel();
                    join_deadline = Some(Instant::now() + self.drain_timeout);
                }
                Ok(WorkerEvent::TxStopped) => tx_stopped = true,
                Ok(WorkerEvent::RxStopped) => rx_stopped = true,
                Ok(WorkerEvent::RxActivity) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Coordination {
            stop_reason,
            failure,
            observer_failed,
        }
    }
}

/// Run a full-duplex UART session without telemetry.
///
/// # Errors
///
/// Returns transport, stream, timeout, or worker errors after bounded shutdown.
pub fn run_session<R, W>(
    port: Port,
    input: R,
    output: W,
    options: SessionOptions,
) -> Result<SessionReport>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    run_session_observed(port, input, output, options, &mut NoopObserver)
}

/// Run a full-duplex UART session and synchronously observe its phases.
///
/// # Errors
///
/// Returns observer, transport, stream, timeout, or worker errors after bounded
/// shutdown. An initial observer error returns before workers start.
pub fn run_session_observed<R, W, O>(
    port: Port,
    input: R,
    output: W,
    options: SessionOptions,
    observer: &mut O,
) -> Result<SessionReport>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
    O: SessionObserver + ?Sized,
{
    let SessionOptions {
        mode,
        rx_idle_timeout,
        drain_timeout,
        cancellation,
    } = options;
    let transport = port.into_transport();
    let cancelled = cancellation.flag();
    let statistics = Arc::new(AtomicStatistics::default());
    let last_rx = Arc::new(Mutex::new(Instant::now()));
    let (events_tx, events_rx) = mpsc::channel();
    let common = WorkerContext {
        transport: Arc::clone(&transport),
        output: SharedOutput::new(output),
        statistics: Arc::clone(&statistics),
        cancelled: Arc::clone(&cancelled),
        events: events_tx,
    };

    observe(
        observer,
        transport.as_ref(),
        SessionPhase::Running,
        statistics.snapshot(),
        None,
        None,
    )?;
    let workers = spawn_workers(
        input,
        common.clone(),
        RxWorkerContext {
            common,
            last_rx: Arc::clone(&last_rx),
        },
        mode,
        drain_timeout,
    )?;
    let mut coordination = Coordinator {
        observer,
        transport: transport.as_ref(),
        statistics: &statistics,
        last_rx: &last_rx,
        cancelled: &cancelled,
        mode,
        rx_idle_timeout,
        drain_timeout,
    }
    .run(&events_rx);
    workers.join(&mut coordination.failure);

    let final_statistics = statistics.snapshot();
    if coordination.observer_failed {
        return Err(coordination.failure.unwrap_or_else(|| {
            Error::Protocol("observer failed without returning an error".to_owned())
        }));
    }
    if let Some(error) = coordination.failure {
        observe(
            observer,
            transport.as_ref(),
            SessionPhase::Final,
            final_statistics,
            coordination.stop_reason,
            Some(&error),
        )?;
        return Err(error);
    }

    let reason = coordination.stop_reason.unwrap_or(StopReason::RxIdle);
    observe(
        observer,
        transport.as_ref(),
        SessionPhase::Final,
        final_statistics,
        Some(reason),
        None,
    )?;
    Ok(SessionReport {
        statistics: final_statistics,
        hardware: transport.counters(),
        reason,
    })
}
