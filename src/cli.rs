//! Stable command-line contract shared by both executables.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use thiserror::Error;

/// Process exit codes promised to callers and the lab harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    CliOrSelection = 2,
    AccessOrBusy = 3,
    ProtocolOrResource = 4,
    TimeoutOrCancellation = 5,
    ValidationMismatch = 6,
}

impl ExitCode {
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Error categories mapped to the stable process exit contract.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Selection(String),
    #[error("{0}")]
    Access(String),
    #[error("{0}")]
    Protocol(String),
    #[error("{0}")]
    Timeout(String),
    #[error("{0}")]
    Validation(String),
}

impl AppError {
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Selection(_) => ExitCode::CliOrSelection,
            Self::Access(_) => ExitCode::AccessOrBusy,
            Self::Protocol(_) => ExitCode::ProtocolOrResource,
            Self::Timeout(_) => ExitCode::TimeoutOrCancellation,
            Self::Validation(_) => ExitCode::ValidationMismatch,
        }
    }
}

/// C232HD transport chosen explicitly by the user.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum C232Backend {
    #[default]
    Vcp,
    Usb,
}

/// Options common to console and stream modes.
#[derive(Clone, Debug, Args)]
pub struct CommonOptions {
    /// Select the device by USB serial number.
    #[arg(long)]
    pub serial: Option<String>,
    /// Requested line rate in bits per second.
    #[arg(long, default_value_t = 115_200, value_parser = parse_baud)]
    pub baud: u32,
    /// Write structured events as newline-delimited JSON.
    #[arg(long)]
    pub event_log: Option<PathBuf>,
}

/// Timing options used by byte-transparent stream mode.
#[derive(Clone, Debug, Args)]
pub struct StreamTiming {
    /// RX quiet period required after TX drain.
    #[arg(long, default_value = "2s", value_parser = parse_duration)]
    pub rx_idle_timeout: Duration,
    /// Maximum wait for device-observable TX completion.
    #[arg(long, default_value = "5s", value_parser = parse_duration)]
    pub drain_timeout: Duration,
}

#[derive(Clone, Debug, Args)]
pub struct GuartConsole {
    #[command(flatten)]
    pub common: CommonOptions,
}

#[derive(Clone, Debug, Args)]
pub struct GuartStream {
    #[command(flatten)]
    pub common: CommonOptions,
    #[command(flatten)]
    pub timing: StreamTiming,
}

#[derive(Clone, Debug, Args)]
pub struct C232Common {
    /// Use the OS serial driver or explicit native USB access.
    #[arg(long, value_enum, default_value_t)]
    pub backend: C232Backend,
    /// VCP path. Valid only with `--backend vcp`.
    #[arg(long)]
    pub port: Option<PathBuf>,
    #[command(flatten)]
    pub common: CommonOptions,
}

impl C232Common {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.backend == C232Backend::Usb && self.port.is_some() {
            return Err(AppError::Selection(
                "--port is valid only with --backend vcp".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Args)]
pub struct C232Console {
    #[command(flatten)]
    pub device: C232Common,
}

#[derive(Clone, Debug, Args)]
pub struct C232Stream {
    #[command(flatten)]
    pub device: C232Common,
    #[command(flatten)]
    pub timing: StreamTiming,
}

#[derive(Clone, Debug, Subcommand)]
pub enum GuartCommand {
    /// List normal Glasgow devices without changing their state.
    List {
        /// Emit one JSON array on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Run an interactive raw-terminal session.
    Console(GuartConsole),
    /// Copy stdin to UART and UART to stdout byte-for-byte.
    Stream(GuartStream),
}

#[derive(Clone, Debug, Subcommand)]
pub enum C232Command {
    /// List attached C232HD-DDHSP-0 devices.
    List {
        /// Emit one JSON array on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Run an interactive raw-terminal session.
    Console(C232Console),
    /// Copy stdin to UART and UART to stdout byte-for-byte.
    Stream(C232Stream),
}

#[derive(Clone, Debug, Parser)]
#[command(name = "guart", version, about = "Native Glasgow UART tool")]
pub struct GuartCli {
    #[command(subcommand)]
    pub command: GuartCommand,
}

#[derive(Clone, Debug, Parser)]
#[command(name = "c232uart", version, about = "Native C232HD UART tool")]
pub struct C232Cli {
    #[command(subcommand)]
    pub command: C232Command,
}

impl C232Cli {
    pub fn validate(&self) -> Result<(), AppError> {
        match &self.command {
            C232Command::List { .. } => Ok(()),
            C232Command::Console(options) => options.device.validate(),
            C232Command::Stream(options) => options.device.validate(),
        }
    }

    pub fn try_parse_validated_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let cli = Self::try_parse_from(arguments)?;
        cli.validate().map_err(|error| {
            clap::Error::raw(clap::error::ErrorKind::ArgumentConflict, error.to_string())
        })?;
        Ok(cli)
    }
}

fn parse_baud(value: &str) -> Result<u32, String> {
    let baud = value
        .parse::<u32>()
        .map_err(|_| format!("invalid baud rate {value:?}"))?;
    if (9_600..=12_000_000).contains(&baud) {
        Ok(baud)
    } else {
        Err("baud rate must be between 9600 and 12000000".to_owned())
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
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
