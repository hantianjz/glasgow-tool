use std::io;
use std::sync::Arc;
use std::time::Duration;

use crate::Result;

/// Provider identity and realized UART configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    /// Stable provider backend name.
    pub backend: &'static str,
    /// USB serial number, when one is available.
    pub serial: Option<String>,
    /// Attachment or device path, when one is available.
    pub path: Option<String>,
    /// Requested line rate in bits per second.
    pub requested_baud: u32,
    /// Realized line rate in bits per second.
    pub actual_baud: u32,
    /// Lowest supported line rate.
    pub minimum_baud: u32,
    /// Highest supported line rate.
    pub maximum_baud: u32,
}

/// Hardware receive counters accumulated during an open port's lifetime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct Counters {
    /// Receive framing, parity, or break errors.
    pub rx_errors: u64,
    /// Receive overflow errors.
    pub rx_overflow: u64,
}

/// Transport failure category used by partial transfer completions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferFailureKind {
    /// The operation was interrupted and may be retried.
    Interrupted,
    /// The operation would block and may be retried.
    WouldBlock,
    /// The operation timed out and may be retried by a session.
    TimedOut,
    /// Device or driver access failed.
    Access,
    /// The transport returned invalid protocol data.
    Protocol,
    /// The device disconnected.
    Disconnected,
}

/// Failure status accompanying a transfer completion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferFailure {
    /// Stable failure category.
    pub kind: TransferFailureKind,
    /// Provider-specific diagnostic.
    pub message: String,
}

impl std::fmt::Display for TransferFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// One transfer completion.
///
/// `completed` bytes are valid and must be accounted before `failure` is handled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferOutcome {
    /// Number of bytes completed before any failure.
    pub completed: usize,
    /// Optional status reported after the completed bytes.
    pub failure: Option<TransferFailure>,
}

impl TransferOutcome {
    /// Construct a successful completion.
    #[must_use]
    pub const fn complete(completed: usize) -> Self {
        Self {
            completed,
            failure: None,
        }
    }

    /// Construct a completion followed by a failure.
    #[must_use]
    pub fn failed(completed: usize, kind: TransferFailureKind, message: impl Into<String>) -> Self {
        Self {
            completed,
            failure: Some(TransferFailure {
                kind,
                message: message.into(),
            }),
        }
    }
}

/// Synchronous UART transport extension point.
///
/// Implementations must make [`Transport::cancel`] unblock in-flight I/O. When a
/// transfer both completes bytes and reports a failure, callers account the
/// completed bytes before handling the failure.
pub trait Transport: Send + Sync + 'static {
    /// Describe the selected device and realized configuration.
    fn metadata(&self) -> &Metadata;
    /// Receive bytes from the device.
    fn read(&self, buffer: &mut [u8]) -> TransferOutcome;
    /// Queue bytes for transmission.
    fn write(&self, data: &[u8]) -> TransferOutcome;
    /// Submit transport-buffered TX data without waiting for physical completion.
    fn submit_tx(&self) {}
    /// Wait until accepted TX data is physically observable as complete.
    ///
    /// # Errors
    ///
    /// Returns a provider error when completion cannot be observed within `timeout`.
    fn drain(&self, timeout: Duration) -> Result<()>;
    /// Cancel in-flight transport operations.
    fn cancel(&self);
    /// Return current hardware receive counters.
    fn counters(&self) -> Counters;
}

/// A reusable UART port backed by a provider or custom transport.
#[derive(Clone)]
pub struct Port {
    transport: Arc<dyn Transport>,
}

impl Port {
    /// Wrap a custom synchronous UART transport.
    pub fn new<T: Transport>(transport: T) -> Self {
        Self {
            transport: Arc::new(transport),
        }
    }

    pub(crate) fn into_transport(self) -> Arc<dyn Transport> {
        self.transport
    }

    /// Describe the selected device and realized configuration.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        self.transport.metadata()
    }

    /// Return current hardware receive counters.
    #[must_use]
    pub fn counters(&self) -> Counters {
        self.transport.counters()
    }

    /// Wait for physical transmission completion.
    ///
    /// # Errors
    ///
    /// Returns a provider error when completion cannot be observed within `timeout`.
    pub fn drain(&self, timeout: Duration) -> Result<()> {
        self.transport.drain(timeout)
    }

    /// Cancel in-flight operations.
    pub fn cancel(&self) {
        self.transport.cancel();
    }
}

fn io_failure(failure: TransferFailure) -> io::Error {
    let kind = match failure.kind {
        TransferFailureKind::Interrupted => io::ErrorKind::Interrupted,
        TransferFailureKind::WouldBlock => io::ErrorKind::WouldBlock,
        TransferFailureKind::TimedOut => io::ErrorKind::TimedOut,
        TransferFailureKind::Access => io::ErrorKind::PermissionDenied,
        TransferFailureKind::Protocol => io::ErrorKind::InvalidData,
        TransferFailureKind::Disconnected => io::ErrorKind::NotConnected,
    };
    io::Error::new(kind, failure.message)
}

impl io::Read for Port {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let outcome = self.transport.read(buffer);
        if outcome.completed > buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "transport reported more RX bytes than buffer capacity",
            ));
        }
        if outcome.completed != 0 {
            return Ok(outcome.completed);
        }
        match outcome.failure {
            Some(failure) => Err(io_failure(failure)),
            None => Ok(0),
        }
    }
}

impl io::Write for Port {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let outcome = self.transport.write(data);
        if outcome.completed > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "transport reported more TX bytes than submitted",
            ));
        }
        if outcome.completed != 0 {
            return Ok(outcome.completed);
        }
        match outcome.failure {
            Some(failure) => Err(io_failure(failure)),
            None => Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "transport completed a zero-length TX without status",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.transport.submit_tx();
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};

    use parking_lot::Mutex;

    use super::*;

    struct FixedTransport {
        metadata: Metadata,
        writes: Mutex<VecDeque<TransferOutcome>>,
        submitted: Arc<AtomicBool>,
    }

    impl Transport for FixedTransport {
        fn metadata(&self) -> &Metadata {
            &self.metadata
        }

        fn read(&self, _buffer: &mut [u8]) -> TransferOutcome {
            TransferOutcome::complete(0)
        }

        fn write(&self, _data: &[u8]) -> TransferOutcome {
            self.writes.lock().pop_front().expect("configured write")
        }

        fn submit_tx(&self) {
            self.submitted.store(true, Ordering::Release);
        }

        fn drain(&self, _timeout: Duration) -> Result<()> {
            Ok(())
        }

        fn cancel(&self) {}

        fn counters(&self) -> Counters {
            Counters::default()
        }
    }

    fn port_with_writes(
        outcomes: impl IntoIterator<Item = TransferOutcome>,
    ) -> (Port, Arc<AtomicBool>) {
        let submitted = Arc::new(AtomicBool::new(false));
        let port = Port::new(FixedTransport {
            metadata: Metadata {
                backend: "test",
                serial: None,
                path: None,
                requested_baud: 115_200,
                actual_baud: 115_200,
                minimum_baud: 9_600,
                maximum_baud: 12_000_000,
            },
            writes: Mutex::new(outcomes.into_iter().collect()),
            submitted: Arc::clone(&submitted),
        });
        (port, submitted)
    }

    #[test]
    fn partial_completion_wins_over_failure_and_flush_submits() {
        let (mut port, submitted) = port_with_writes([TransferOutcome::failed(
            2,
            TransferFailureKind::Disconnected,
            "after bytes",
        )]);
        assert_eq!(port.write(b"abc").unwrap(), 2);
        port.flush().unwrap();
        assert!(submitted.load(Ordering::Acquire));
    }

    #[test]
    fn status_free_zero_write_is_write_zero() {
        let (mut port, _) = port_with_writes([TransferOutcome::complete(0)]);
        assert_eq!(
            port.write(b"x").unwrap_err().kind(),
            io::ErrorKind::WriteZero
        );
    }
}
