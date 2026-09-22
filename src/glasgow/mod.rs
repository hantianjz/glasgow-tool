//! Native Glasgow discovery, provisioning, and UART transport.

pub mod device;
pub mod management;
pub mod uart;

use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

use crate::Tool;
use crate::cli::{AppError, GuartCli, GuartCommand};
use crate::events::EventLog;
use crate::session::{SessionMode, SessionOptions, Transport, run_session};
use crate::terminal::RawTerminalGuard;

fn print_devices(json: bool) -> Result<(), AppError> {
    let devices = device::list_devices()?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if json {
        serde_json::to_writer(&mut output, &devices)
            .map_err(|error| AppError::Protocol(format!("cannot encode Glasgow list: {error}")))?;
        output
            .write_all(b"\n")
            .map_err(|error| AppError::Access(format!("cannot write Glasgow list: {error}")))?;
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
            .map_err(|error| AppError::Access(format!("cannot write Glasgow list: {error}")))?;
        }
    }
    Ok(())
}

pub fn run(cli: GuartCli) -> Result<(), AppError> {
    match cli.command {
        GuartCommand::List { json } => print_devices(json),
        GuartCommand::Console(options) => {
            let devices = device::list_devices()?;
            let selected = device::select_device(&devices, options.common.serial.as_deref())?;
            let transport: Arc<dyn Transport> =
                Arc::new(uart::GlasgowTransport::open(selected, options.common.baud)?);
            let mut event_log = EventLog::open(options.common.event_log.as_deref(), Tool::Guart)
                .map_err(|error| AppError::Access(format!("cannot open event log: {error}")))?;
            let _terminal = RawTerminalGuard::enter().map_err(|error| {
                AppError::Access(format!("cannot enter raw terminal mode: {error}"))
            })?;
            let report = run_session(
                transport,
                io::stdin(),
                io::stdout(),
                SessionOptions {
                    mode: SessionMode::Console,
                    rx_idle_timeout: Duration::from_secs(2),
                    drain_timeout: Duration::from_secs(5),
                },
                &mut event_log,
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
            let devices = device::list_devices()?;
            let selected = device::select_device(&devices, options.common.serial.as_deref())?;
            let transport: Arc<dyn Transport> =
                Arc::new(uart::GlasgowTransport::open(selected, options.common.baud)?);
            let mut event_log = EventLog::open(options.common.event_log.as_deref(), Tool::Guart)
                .map_err(|error| AppError::Access(format!("cannot open event log: {error}")))?;
            run_session(
                transport,
                io::stdin(),
                io::stdout(),
                SessionOptions {
                    mode: SessionMode::Stream,
                    rx_idle_timeout: options.timing.rx_idle_timeout,
                    drain_timeout: options.timing.drain_timeout,
                },
                &mut event_log,
            )?;
            Ok(())
        }
    }
}
