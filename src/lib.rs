//! Shared implementation for the native Glasgow and C232HD UART tools.

pub mod c232;
pub mod cli;
pub mod events;
pub mod glasgow;
pub mod resources;
pub mod session;
pub mod terminal;

/// Executable identity used in diagnostics and structured events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tool {
    /// Glasgow UART tool.
    Guart,
    /// FTDI C232HD UART tool.
    C232Uart,
}

impl Tool {
    /// Stable executable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Guart => "guart",
            Self::C232Uart => "c232uart",
        }
    }
}
