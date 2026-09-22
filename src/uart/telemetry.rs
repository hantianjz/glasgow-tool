use std::io::Write;
use std::time::Instant;

use serde::Serialize;

use crate::{Error, Result};

use super::session::{SessionStatistics, StopReason};
use super::transport::{Counters, Metadata};

/// Observable stage of a UART session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// Workers are about to start.
    Running,
    /// Accepted TX data has completed at the device.
    Draining,
    /// Both workers have joined.
    Final,
}

/// Borrowed snapshot delivered to a session observer.
#[derive(Debug)]
pub struct SessionEvent<'a> {
    /// Selected port metadata.
    pub metadata: &'a Metadata,
    /// Current session phase.
    pub phase: SessionPhase,
    /// Current byte counters.
    pub statistics: SessionStatistics,
    /// Current hardware receive counters.
    pub counters: Counters,
    /// Session stop reason, once known.
    pub stop_reason: Option<StopReason>,
    /// Final coordinator error, when the session failed.
    pub error: Option<&'a Error>,
}

/// Receives synchronous session snapshots.
pub trait SessionObserver {
    /// Observe one session event.
    ///
    /// # Errors
    ///
    /// Returns an observer-specific error to stop the session coordinator.
    fn observe(&mut self, event: &SessionEvent<'_>) -> Result<()>;
}

/// Observer that discards all events.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopObserver;

impl SessionObserver for NoopObserver {
    fn observe(&mut self, _event: &SessionEvent<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Serialize)]
struct EventRecord<'a> {
    schema_version: u8,
    monotonic_ns: u64,
    tool: &'a str,
    backend: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_serial: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_path: Option<&'a str>,
    requested_baud: Option<u32>,
    actual_baud: Option<u32>,
    phase: SessionPhase,
    tx_bytes_accepted: u64,
    tx_bytes_completed: u64,
    rx_bytes: u64,
    hardware: Counters,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

fn error_exit_code(error: &Error) -> i32 {
    match error {
        Error::Selection(_) => 2,
        Error::Access(_) => 3,
        Error::Protocol(_) => 4,
        Error::Timeout(_) => 5,
        Error::Validation(_) => 6,
    }
}

/// NDJSON session observer preserving telemetry schema version 1.
pub struct NdjsonObserver<W: Write> {
    writer: W,
    source: String,
    started: Instant,
}

impl<W: Write> NdjsonObserver<W> {
    /// Create an observer writing one flushed JSON object per event.
    pub fn new(writer: W, source: impl Into<String>) -> Self {
        Self {
            writer,
            source: source.into(),
            started: Instant::now(),
        }
    }

    /// Recover the underlying writer.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<W: Write> SessionObserver for NdjsonObserver<W> {
    fn observe(&mut self, event: &SessionEvent<'_>) -> Result<()> {
        let final_exit_code =
            (event.phase == SessionPhase::Final).then(|| event.error.map_or(0, error_exit_code));
        let monotonic_ns = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let record = EventRecord {
            schema_version: 1,
            monotonic_ns,
            tool: &self.source,
            backend: event.metadata.backend,
            selected_serial: event.metadata.serial.as_deref(),
            selected_path: event.metadata.path.as_deref(),
            requested_baud: Some(event.metadata.requested_baud),
            actual_baud: Some(event.metadata.actual_baud),
            phase: event.phase,
            tx_bytes_accepted: event.statistics.tx_bytes_accepted,
            tx_bytes_completed: event.statistics.tx_bytes_completed,
            rx_bytes: event.statistics.rx_bytes,
            hardware: event.counters,
            reason: event.stop_reason.map(StopReason::as_str),
            exit_code: final_exit_code,
        };
        serde_json::to_writer(&mut self.writer, &record)
            .map_err(|error| Error::Access(format!("event log write failed: {error}")))?;
        self.writer
            .write_all(b"\n")
            .and_then(|()| self.writer.flush())
            .map_err(|error| Error::Access(format!("event log write failed: {error}")))
    }
}
