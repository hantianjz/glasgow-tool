use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use glasgow_tool::uart::{
    CancellationToken, NdjsonObserver, NoopObserver, SessionEvent, SessionObserver,
};
use glasgow_tool::{Error, Result};

#[derive(Clone, Debug, Args)]
pub(crate) struct CommonOptions {
    /// Select the device by USB serial number.
    #[arg(long)]
    pub(crate) serial: Option<String>,
    /// Requested line rate in bits per second.
    #[arg(long, default_value_t = 115_200, value_parser = parse_baud)]
    pub(crate) baud: u32,
    /// Write structured events as newline-delimited JSON.
    #[arg(long)]
    pub(crate) event_log: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct StreamTiming {
    /// RX quiet period required after TX drain.
    #[arg(long, default_value = "2s", value_parser = parse_duration)]
    pub(crate) rx_idle_timeout: Duration,
    /// Maximum wait for device-observable TX completion.
    #[arg(long, default_value = "5s", value_parser = parse_duration)]
    pub(crate) drain_timeout: Duration,
}

fn parse_baud(value: &str) -> std::result::Result<u32, String> {
    let baud = value
        .parse::<u32>()
        .map_err(|_| format!("invalid baud rate {value:?}"))?;
    if (9_600..=12_000_000).contains(&baud) {
        Ok(baud)
    } else {
        Err("baud rate must be between 9600 and 12000000".to_owned())
    }
}

fn parse_duration(value: &str) -> std::result::Result<Duration, String> {
    let (number, scale) = if let Some(number) = value.strip_suffix("ms") {
        (number, 0.001)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1.0)
    } else {
        return Err("duration must end in ms or s".to_owned());
    };
    let amount = number
        .parse::<f64>()
        .map_err(|_| format!("invalid duration {value:?}"))?;
    if !amount.is_finite() || amount <= 0.0 {
        return Err("duration must be finite and greater than zero".to_owned());
    }
    Ok(Duration::from_secs_f64(amount * scale))
}

pub(crate) fn exit_code(error: &Error) -> i32 {
    match error {
        Error::Selection(_) => 2,
        Error::Access(_) => 3,
        Error::Protocol(_) => 4,
        Error::Timeout(_) => 5,
        Error::Validation(_) => 6,
    }
}

pub(crate) fn install_ctrl_c(token: &CancellationToken) -> Result<()> {
    let token = token.clone();
    ctrlc::set_handler(move || token.cancel())
        .map_err(|error| Error::Protocol(format!("cannot install signal handler: {error}")))
}

pub(crate) struct RawTerminalGuard {
    active: bool,
}

impl RawTerminalGuard {
    pub(crate) fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self { active: true })
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            self.active = false;
        }
    }
}

pub(crate) enum CliObserver {
    Noop(NoopObserver),
    Ndjson(NdjsonObserver<BufWriter<File>>),
}

impl CliObserver {
    pub(crate) fn open(path: Option<&Path>, source: &'static str) -> Result<Self> {
        match path {
            None => Ok(Self::Noop(NoopObserver)),
            Some(path) => {
                let file = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(path)
                    .map_err(|error| Error::Access(format!("cannot open event log: {error}")))?;
                Ok(Self::Ndjson(NdjsonObserver::new(
                    BufWriter::new(file),
                    source,
                )))
            }
        }
    }
}

impl SessionObserver for CliObserver {
    fn observe(&mut self, event: &SessionEvent<'_>) -> Result<()> {
        match self {
            Self::Noop(observer) => observer.observe(event),
            Self::Ndjson(observer) => observer.observe(event),
        }
    }
}
