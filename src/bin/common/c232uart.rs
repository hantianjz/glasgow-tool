#[cfg(test)]
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use glasgow_tool::uart::{self, CancellationToken, SessionOptions, run_session_observed};
use glasgow_tool::{Error, Result};
use serde::Serialize;

use super::common::{CliObserver, CommonOptions, RawTerminalGuard, StreamTiming, install_ctrl_c};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "lower")]
enum C232Backend {
    #[default]
    Vcp,
    Usb,
}

impl From<C232Backend> for uart::c232::Backend {
    fn from(backend: C232Backend) -> Self {
        match backend {
            C232Backend::Vcp => Self::Vcp,
            C232Backend::Usb => Self::Usb,
        }
    }
}

#[derive(Clone, Debug, Args)]
struct C232Common {
    /// Use the OS serial driver or explicit native USB access.
    #[arg(long, value_enum, default_value_t)]
    backend: C232Backend,
    /// VCP path. Valid only with `--backend vcp`.
    #[arg(long)]
    port: Option<PathBuf>,
    #[command(flatten)]
    common: CommonOptions,
}

impl C232Common {
    fn validate(&self) -> Result<()> {
        if self.backend == C232Backend::Usb && self.port.is_some() {
            return Err(Error::Selection(
                "--port is valid only with --backend vcp".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Args)]
struct C232Console {
    #[command(flatten)]
    device: C232Common,
}

#[derive(Clone, Debug, Args)]
struct C232Stream {
    #[command(flatten)]
    device: C232Common,
    #[command(flatten)]
    timing: StreamTiming,
}

#[derive(Clone, Debug, Subcommand)]
enum C232Command {
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
#[command(name = "c232uart", version, about = "Native C232HD UART tool")]
pub(crate) struct C232Cli {
    #[command(subcommand)]
    command: C232Command,
}

impl C232Cli {
    pub(crate) fn validate(&self) -> Result<()> {
        match &self.command {
            C232Command::List { .. } => Ok(()),
            C232Command::Console(options) => options.device.validate(),
            C232Command::Stream(options) => options.device.validate(),
        }
    }

    #[cfg(test)]
    fn try_parse_validated_from<I, T>(arguments: I) -> std::result::Result<Self, clap::Error>
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

#[derive(Serialize)]
struct ListEntry<'a> {
    serial: &'a str,
    vid: String,
    pid: String,
    product: &'a str,
    port: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stable_path: Option<String>,
}

fn print_devices(json: bool) -> Result<()> {
    let devices = uart::c232::list_devices(uart::c232::Backend::Vcp)?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if json {
        let entries = devices
            .iter()
            .map(|device| ListEntry {
                serial: &device.serial,
                vid: format!("{:04x}", device.vendor_id),
                pid: format!("{:04x}", device.product_id),
                product: &device.product,
                port: &device.path,
                stable_path: device
                    .stable_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            })
            .collect::<Vec<_>>();
        serde_json::to_writer(&mut output, &entries)
            .map_err(|error| Error::Protocol(format!("cannot encode device list: {error}")))?;
        output
            .write_all(b"\n")
            .map_err(|error| Error::Access(format!("cannot write device list: {error}")))?;
    } else {
        for device in devices {
            let display_path = device
                .stable_path
                .as_deref()
                .unwrap_or(device.path.as_ref());
            writeln!(
                output,
                "{} {:04x}:{:04x} {} {}",
                device.serial,
                device.vendor_id,
                device.product_id,
                display_path.display(),
                device.product
            )
            .map_err(|error| Error::Access(format!("cannot write device list: {error}")))?;
        }
    }
    Ok(())
}

fn open(options: &C232Common) -> Result<uart::Port> {
    uart::c232::open(&uart::c232::OpenOptions {
        backend: options.backend.into(),
        serial: options.common.serial.clone(),
        port: options.port.clone(),
        baud: options.common.baud,
    })
}

pub(crate) fn run(cli: C232Cli) -> Result<()> {
    match cli.command {
        C232Command::List { json } => print_devices(json),
        C232Command::Console(options) => {
            let token = CancellationToken::new();
            install_ctrl_c(&token)?;
            let mut observer =
                CliObserver::open(options.device.common.event_log.as_deref(), "c232uart")?;
            let port = open(&options.device)?;
            let _terminal = RawTerminalGuard::enter().map_err(|error| {
                Error::Access(format!("cannot enter raw terminal mode: {error}"))
            })?;
            let mut session_options = SessionOptions::console();
            session_options.rx_idle_timeout = Duration::from_secs(2);
            session_options.drain_timeout = Duration::from_secs(5);
            session_options.cancellation = token;
            let report = run_session_observed(
                port,
                io::stdin(),
                io::stdout(),
                session_options,
                &mut observer,
            )?;
            eprintln!(
                "TX accepted/completed: {}/{}, RX: {} bytes",
                report.statistics.tx_bytes_accepted,
                report.statistics.tx_bytes_completed,
                report.statistics.rx_bytes
            );
            Ok(())
        }
        C232Command::Stream(options) => {
            let token = CancellationToken::new();
            install_ctrl_c(&token)?;
            let mut observer =
                CliObserver::open(options.device.common.event_log.as_deref(), "c232uart")?;
            let port = open(&options.device)?;
            let mut session_options = SessionOptions::stream();
            session_options.rx_idle_timeout = options.timing.rx_idle_timeout;
            session_options.drain_timeout = options.timing.drain_timeout;
            session_options.cancellation = token;
            run_session_observed(
                port,
                io::stdin(),
                io::stdout(),
                session_options,
                &mut observer,
            )?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::C232Cli;

    #[test]
    fn usb_backend_rejects_vcp_port() {
        assert!(
            C232Cli::try_parse_validated_from([
                "c232uart",
                "stream",
                "--backend",
                "usb",
                "--port",
                "/dev/ttyUSB0",
            ])
            .is_err()
        );
    }
}
