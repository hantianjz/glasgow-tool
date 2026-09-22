//! Blocking UART discovery, ports, sessions, and telemetry.

pub mod c232;
pub mod glasgow;
mod session;
mod telemetry;
mod transport;

pub use session::{
    CONSOLE_ESCAPE, CancellationToken, SessionMode, SessionOptions, SessionReport,
    SessionStatistics, StopReason, run_session, run_session_observed,
};
pub use telemetry::{NdjsonObserver, NoopObserver, SessionEvent, SessionObserver, SessionPhase};
pub use transport::{
    Counters, Metadata, Port, TransferFailure, TransferFailureKind, TransferOutcome, Transport,
};
