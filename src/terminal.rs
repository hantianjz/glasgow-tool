//! Raw-terminal guard used only by interactive console mode.

use std::io;

use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Restores the local terminal mode whenever console execution leaves scope.
#[derive(Debug)]
pub struct RawTerminalGuard {
    active: bool,
}

impl RawTerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self { active: true })
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if self.active {
            disable_raw_mode()?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            self.active = false;
        }
    }
}
