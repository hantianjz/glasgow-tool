use std::io::{self, Write};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use glasgow_tool::uart::{self, CancellationToken, SessionOptions, run_session_observed};
use glasgow_tool::{Error, Result};
use serde::Serialize;

use super::common::{CliObserver, CommonOptions, RawTerminalGuard, StreamTiming, install_ctrl_c};

#[derive(Clone, Debug, Args)]
struct GuartConsole {
    #[command(flatten)]
    common: CommonOptions,
}

#[derive(Clone, Debug, Args)]
struct GuartStream {
    #[command(flatten)]
    common: CommonOptions,
    #[command(flatten)]
    timing: StreamTiming,
}

#[derive(Clone, Debug, Subcommand)]
enum GuartCommand {
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

#[derive(Clone, Debug, Parser)]
#[command(name = "guart", version, about = "Native Glasgow UART tool")]
pub(crate) struct GuartCli {
    #[command(subcommand)]
    command: GuartCommand,
}

#[derive(Serialize)]
struct ListEntry<'a> {
    serial: Option<&'a str>,
    vid: String,
    pid: String,
    revision: &'a str,
    api_level: u8,
    api_compatible: bool,
    path: &'a str,
}

fn print_devices(json: bool) -> Result<()> {
    let devices = uart::glasgow::list_devices()?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if json {
        let entries = devices
            .iter()
            .map(|device| ListEntry {
                serial: device.serial.as_deref(),
                vid: format!("{:04x}", device.vendor_id),
                pid: format!("{:04x}", device.product_id),
                revision: &device.revision,
                api_level: device.api_level,
                api_compatible: device.api_compatible,
                path: &device.path,
            })
            .collect::<Vec<_>>();
        serde_json::to_writer(&mut output, &entries)
            .map_err(|error| Error::Protocol(format!("cannot encode Glasgow list: {error}")))?;
        output
            .write_all(b"\n")
            .map_err(|error| Error::Access(format!("cannot write Glasgow list: {error}")))?;
    } else {
        for device in devices {
            writeln!(
                output,
                "{} {} rev{} API {} ({})",
                device.serial.as_deref().unwrap_or("<no serial>"),
                device.path,
                device.revision,
                device.api_level,
                if device.api_compatible {
                    "compatible"
                } else {
                    "firmware update required"
                }
            )
            .map_err(|error| Error::Access(format!("cannot write Glasgow list: {error}")))?;
        }
    }
    Ok(())
}

fn open(common: &CommonOptions) -> Result<uart::Port> {
    uart::glasgow::open(&uart::glasgow::OpenOptions {
        serial: common.serial.clone(),
        baud: common.baud,
    })
}

pub(crate) fn run(cli: GuartCli) -> Result<()> {
    match cli.command {
        GuartCommand::List { json } => print_devices(json),
        GuartCommand::Console(options) => {
            let token = CancellationToken::new();
            install_ctrl_c(&token)?;
            let mut observer = CliObserver::open(options.common.event_log.as_deref(), "guart")?;
            let port = open(&options.common)?;
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
                "TX accepted/completed: {}/{}, RX: {} bytes, errors: {}, overflow: {}",
                report.statistics.tx_bytes_accepted,
                report.statistics.tx_bytes_completed,
                report.statistics.rx_bytes,
                report.hardware.rx_errors,
                report.hardware.rx_overflow
            );
            Ok(())
        }
        GuartCommand::Stream(options) => {
            let token = CancellationToken::new();
            install_ctrl_c(&token)?;
            let mut observer = CliObserver::open(options.common.event_log.as_deref(), "guart")?;
            let port = open(&options.common)?;
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
