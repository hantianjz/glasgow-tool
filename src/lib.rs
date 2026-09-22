//! Blocking UART access for Glasgow and FTDI C232HD devices.
//!
//! The root API is bus-first: UART functionality lives under [`uart`], leaving
//! sibling root modules available for future buses without a UART-specific
//! provider enum.
//!
//! # Glasgow
//!
//! ```no_run
//! use std::io::{Read, Write};
//! use std::time::Duration;
//! use glasgow_tool::uart;
//!
//! # fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//! let devices = uart::glasgow::list_devices()?;
//! let options = uart::glasgow::OpenOptions {
//!     serial: devices.first().and_then(|device| device.serial.clone()),
//!     baud: 115_200,
//! };
//! let mut port = uart::glasgow::open(&options)?;
//! port.write_all(b"hello")?;
//! port.flush()?;
//! port.drain(Duration::from_secs(5))?;
//! let mut reply = [0; 5];
//! port.read_exact(&mut reply)?;
//!
//! let session_port = uart::glasgow::open(&options)?;
//! let report = uart::run_session(
//!     session_port,
//!     std::io::Cursor::new(b"stream payload".to_vec()),
//!     std::io::sink(),
//!     uart::SessionOptions::stream(),
//! )?;
//! assert_eq!(report.reason, uart::StopReason::RxIdle);
//! # Ok(())
//! # }
//! ```
//!
//! # C232HD
//!
//! ```no_run
//! use std::io::{Read, Write};
//! use std::time::Duration;
//! use glasgow_tool::uart;
//!
//! # fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//! let devices = uart::c232::list_devices(uart::c232::Backend::Vcp)?;
//! let options = uart::c232::OpenOptions {
//!     serial: devices.first().map(|device| device.serial.clone()),
//!     ..uart::c232::OpenOptions::default()
//! };
//! let mut port = uart::c232::open(&options)?;
//! port.write_all(b"hello")?;
//! port.flush()?;
//! port.drain(Duration::from_secs(5))?;
//! let mut reply = [0; 5];
//! port.read_exact(&mut reply)?;
//!
//! let session_port = uart::c232::open(&options)?;
//! let report = uart::run_session(
//!     session_port,
//!     std::io::Cursor::new(vec![uart::CONSOLE_ESCAPE]),
//!     std::io::sink(),
//!     uart::SessionOptions::console(),
//! )?;
//! assert_eq!(report.reason, uart::StopReason::ConsoleEscape);
//! # Ok(())
//! # }
//! ```

mod error;
pub mod uart;

pub use error::{Error, Result};
