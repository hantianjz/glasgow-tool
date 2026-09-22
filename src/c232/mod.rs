//! C232HD identity selection and transport backends.

pub mod usb;
pub mod vcp;

use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

use crate::Tool;
use crate::cli::{AppError, C232Backend, C232Cli, C232Command, C232Common};
use crate::events::EventLog;
use crate::session::{SessionMode, SessionOptions, Transport, run_session};
use crate::terminal::RawTerminalGuard;

fn print_devices(json: bool) -> Result<(), AppError> {
    let devices = vcp::list_devices()?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if json {
        serde_json::to_writer(&mut output, &devices)
            .map_err(|error| AppError::Protocol(format!("cannot encode device list: {error}")))?;
        output
            .write_all(b"\n")
            .map_err(|error| AppError::Access(format!("cannot write device list: {error}")))?;
    } else {
        for device in devices {
            writeln!(
                output,
                "{} {:04x}:{:04x} {} {}",
                device.serial,
                vcp::FTDI_VID,
                vcp::FTDI_PID,
                device.stable_path.as_deref().unwrap_or(&device.port),
                device.product
            )
            .map_err(|error| AppError::Access(format!("cannot write device list: {error}")))?;
        }
    }
    Ok(())
}

fn open_transport(options: &C232Common) -> Result<Arc<dyn Transport>, AppError> {
    match options.backend {
        C232Backend::Vcp => {
            let devices = vcp::list_devices()?;
            let selected = vcp::select_device(
                &devices,
                options.common.serial.as_deref(),
                options.port.as_deref(),
            )?;
            Ok(Arc::new(vcp::VcpTransport::open(
                &selected,
                options.common.baud,
                options.port.as_deref(),
            )?))
        }
        C232Backend::Usb => Ok(Arc::new(usb::UsbTransport::open(
            options.common.serial.as_deref(),
            options.common.baud,
        )?)),
    }
}

pub fn run(cli: C232Cli) -> Result<(), AppError> {
    match cli.command {
        C232Command::List { json } => print_devices(json),
        C232Command::Console(options) => {
            let transport = open_transport(&options.device)?;
            let mut event_log =
                EventLog::open(options.device.common.event_log.as_deref(), Tool::C232Uart)
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
                "TX accepted/completed: {}/{}, RX: {} bytes",
                report.statistics.tx_bytes_accepted,
                report.statistics.tx_bytes_completed,
                report.statistics.rx_bytes
            );
            Ok(())
        }
        C232Command::Stream(options) => {
            let transport = open_transport(&options.device)?;
            let mut event_log =
                EventLog::open(options.device.common.event_log.as_deref(), Tool::C232Uart)
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
