//! Stable NDJSON telemetry that never shares stdout with payload bytes.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use serde::Serialize;

use crate::Tool;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Glasgow,
    Vcp,
    Usb,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Selected,
    Opened,
    Running,
    Draining,
    Closing,
    Final,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct HardwareCounters {
    pub rx_errors: u64,
    pub rx_overflow: u64,
}

/// Complete snapshot written as one stable NDJSON record.
#[derive(Clone, Debug, Serialize)]
pub struct EventRecord<'a> {
    pub schema_version: u8,
    pub monotonic_ns: u64,
    pub tool: &'a str,
    pub backend: Backend,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_serial: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_baud: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_baud: Option<u32>,
    pub phase: Phase,
    pub tx_bytes_accepted: u64,
    pub tx_bytes_completed: u64,
    pub rx_bytes: u64,
    pub hardware: HardwareCounters,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// Process-relative monotonic timestamp source.
#[derive(Debug)]
pub struct EventClock(Instant);

impl Default for EventClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl EventClock {
    #[must_use]
    pub fn elapsed_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// Optional line-buffered event sink.
pub struct EventLog {
    writer: Option<BufWriter<File>>,
    clock: EventClock,
    tool: Tool,
}

impl EventLog {
    pub fn open(path: Option<&Path>, tool: Tool) -> io::Result<Self> {
        let writer = path
            .map(|path| {
                OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(path)
                    .map(BufWriter::new)
            })
            .transpose()?;
        Ok(Self {
            writer,
            clock: EventClock::default(),
            tool,
        })
    }

    #[must_use]
    pub fn timestamp(&self) -> u64 {
        self.clock.elapsed_ns()
    }

    pub fn write(&mut self, mut record: EventRecord<'_>) -> io::Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        record.schema_version = 1;
        record.monotonic_ns = self.clock.elapsed_ns();
        record.tool = self.tool.name();
        serde_json::to_writer(&mut *writer, &record)?;
        writer.write_all(b"\n")?;
        writer.flush()
    }
}
