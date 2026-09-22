use thiserror::Error as ThisError;

/// Error categories shared by UART providers and sessions.
#[derive(Debug, ThisError)]
pub enum Error {
    /// Device selection or caller option error.
    #[error("{0}")]
    Selection(String),
    /// Device, driver, filesystem, or stream access error.
    #[error("{0}")]
    Access(String),
    /// Device protocol or embedded-resource error.
    #[error("{0}")]
    Protocol(String),
    /// A bounded operation did not complete in time.
    #[error("{0}")]
    Timeout(String),
    /// Requested and realized configuration differ beyond tolerance.
    #[error("{0}")]
    Validation(String),
}

/// Result type used by the library API.
pub type Result<T, E = Error> = std::result::Result<T, E>;
